//! The Risk section's report, end to end over eight markets of synthetic
//! candles.

use std::collections::HashMap;

use galata_desk_core::market::candles::Candle;
use galata_desk_core::market::{Interval, MarketId};
use galata_desk_core::risk::report::{Exclusion, FocusState, PeriodState};
use galata_desk_core::risk::{
    Confidence, Estimator, RiskPeriod, RiskPosition, RiskReport, RiskSettings, SeriesState,
};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

const DAY: u64 = 24 * 3_600_000;
const NOW: u64 = 1_000 * DAY;

/// (market, notional, mark, daily vol, loading on the common driver)
const BOOK: [(&str, f64, f64, f64, f64); 8] = [
    ("BTC", 40_000.0, 98_000.0, 0.028, 0.85),
    ("ETH", 50_000.0, 3_400.0, 0.036, 0.85),
    ("SOL", 20_000.0, 190.0, 0.045, 0.80),
    ("AVAX", 8_000.0, 32.0, 0.050, 0.78),
    ("DOGE", -15_000.0, 0.32, 0.052, 0.70),
    ("HYPE", 12_000.0, 28.0, 0.060, 0.60),
    ("xyz:SILVER", 30_000.0, 31.0, 0.018, 0.05),
    ("xyz:NVDA", -10_000.0, 180.0, 0.026, 0.25),
];

fn positions() -> Vec<RiskPosition> {
    BOOK.iter()
        .map(|(m, n, px, _, _)| RiskPosition {
            market: MarketId::new(*m),
            notional: *n,
            mark: *px,
        })
        .collect()
}

/// Deterministic normal-ish draws.
struct Draws(u64);
impl Draws {
    fn next(&mut self) -> f64 {
        let mut s = 0.0;
        for _ in 0..12 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            s += (self.0 >> 11) as f64 / (1u64 << 53) as f64;
        }
        s - 6.0
    }
}

/// A year and a bit of candles at `interval` for every market.
fn candles(interval: Interval) -> HashMap<(MarketId, Interval), SeriesState> {
    let step = interval.millis();
    let per_day = DAY as f64 / step as f64;
    let n = (400 * DAY / step) as usize;
    let start = NOW - n as u64 * step;
    let mut d = Draws(7);
    let mut closes: Vec<Vec<f64>> = BOOK.iter().map(|b| vec![b.2]).collect();
    for _ in 1..n {
        let common = d.next();
        for (i, (_, _, _, vol, load)) in BOOK.iter().enumerate() {
            let sd = vol / per_day.sqrt();
            let r = sd * (load * common + (1.0 - load * load).sqrt() * d.next());
            let last = *closes[i].last().unwrap();
            closes[i].push(last * r.exp());
        }
    }
    BOOK.iter()
        .enumerate()
        .map(|(i, (m, ..))| {
            let series = closes[i]
                .iter()
                .enumerate()
                .map(|(k, c)| {
                    let c = Decimal::from_f64(*c).unwrap();
                    Candle {
                        open_ms: start + k as u64 * step,
                        open: c,
                        high: c,
                        low: c,
                        close: c,
                    }
                })
                .collect();
            ((MarketId::new(*m), interval), SeriesState::Ready(series))
        })
        .collect()
}

fn all_candles() -> HashMap<(MarketId, Interval), SeriesState> {
    let mut all = candles(Interval::H8);
    all.extend(candles(Interval::D1));
    all.extend(candles(Interval::D3));
    all
}

fn ready(report: &RiskReport) -> &galata_desk_core::risk::report::Focus {
    match &report.focus {
        FocusState::Ready(f) => f,
        other => panic!("focus not ready: {other:?}"),
    }
}

#[test]
fn every_field_is_filled_and_consistent() {
    let candles = all_candles();
    let settings = RiskSettings::default();
    let report = RiskReport::compute(&positions(), &candles, NOW, &settings);

    assert_eq!(report.included.len(), 8);
    assert!(report.excluded.is_empty());
    assert_eq!(
        report.included[0].market,
        MarketId::new("ETH"),
        "largest first"
    );
    assert_eq!(
        report.included.iter().filter(|p| p.oracle_priced()).count(),
        2
    );

    assert_eq!(report.periods.len(), 4);
    for row in &report.periods {
        let PeriodState::Ready(f) = &row.state else {
            panic!("{:?} not ready", row.period)
        };
        assert!(f.count >= 89, "{:?}: {}", row.period, f.count);
        assert!(f.var99 > f.var95);
        // The candle sigma scales to one day by √(candles a day).
        let day_sigma = f.var95 / Confidence::P95.z();
        assert!(
            (day_sigma - f.sigma_candle * row.period.per_day().sqrt()).abs() < 1e-6 * day_sigma
        );
    }

    let f = ready(&report);
    assert_eq!(f.period, RiskPeriod::Quarter);
    assert_eq!(f.covariance.rows(), 8);
    let sum: f64 = f.contributions.iter().map(|c| c.component).sum();
    assert!(((sum - f.parametric) / f.parametric).abs() < 1e-9);
    assert!(f.historical.shortfall >= f.historical.var);
    assert_eq!(report.included[f.benchmark].market, MarketId::new("BTC"));
    assert!((f.betas[f.benchmark].beta - 1.0).abs() < 1e-9);
    assert!(f.exposure.beta_gross >= f.exposure.beta_net.abs());
    assert_eq!(f.exposure.gross, 185_000.0);
    // The crypto markets cluster; silver does not.
    let silver = report
        .included
        .iter()
        .position(|p| p.market.coin() == "xyz:SILVER")
        .unwrap();
    assert!(f.clusters.iter().all(|c| !c.members.contains(&silver)));
    assert!(f.uncorrelated.as_ref().unwrap().members.contains(&silver));
    assert!(f.betas[silver].low_r2());
}

#[test]
fn every_estimator_gives_a_full_report() {
    let candles = all_candles();
    for estimator in Estimator::ALL {
        let settings = RiskSettings {
            estimator,
            ..RiskSettings::default()
        };
        let report = RiskReport::compute(&positions(), &candles, NOW, &settings);
        let f = ready(&report);
        assert!(f.parametric > 0.0, "{estimator:?}");
        assert_eq!(report.included[f.benchmark].market, MarketId::new("BTC"));
        let sum: f64 = f.contributions.iter().map(|c| c.component).sum();
        assert!(
            ((sum - f.parametric) / f.parametric).abs() < 1e-9,
            "{estimator:?}"
        );
    }
}

#[test]
fn a_failed_market_is_named_and_left_out() {
    let mut candles = all_candles();
    candles.insert(
        (MarketId::new("xyz:NVDA"), Interval::D1),
        SeriesState::Failed,
    );
    let report = RiskReport::compute(&positions(), &candles, NOW, &RiskSettings::default());
    assert_eq!(
        report.excluded,
        vec![(MarketId::new("xyz:NVDA"), Exclusion::Failed)]
    );
    assert_eq!(report.included.len(), 7);
    assert!(matches!(report.focus, FocusState::Ready(_)));
}

#[test]
fn loading_is_never_zero() {
    let report = RiskReport::compute(&positions(), &HashMap::new(), NOW, &RiskSettings::default());
    assert_eq!(report.focus, FocusState::Loading);
    assert!(
        report
            .periods
            .iter()
            .all(|r| r.state == PeriodState::Loading)
    );
}

#[test]
fn no_positions_is_empty() {
    let report = RiskReport::compute(&[], &all_candles(), NOW, &RiskSettings::default());
    assert_eq!(report.focus, FocusState::Empty);
}

#[test]
fn switching_the_benchmark_restates_every_beta() {
    let candles = all_candles();
    let settings = RiskSettings {
        benchmark: Some(MarketId::new("ETH")),
        ..RiskSettings::default()
    };
    let report = RiskReport::compute(&positions(), &candles, NOW, &settings);
    let f = ready(&report);
    assert_eq!(report.included[f.benchmark].market, MarketId::new("ETH"));
    assert!((f.betas[f.benchmark].beta - 1.0).abs() < 1e-9);
    assert!((f.exposure.beta_net_units - f.exposure.beta_net / 3_400.0).abs() < 1e-9);
}
