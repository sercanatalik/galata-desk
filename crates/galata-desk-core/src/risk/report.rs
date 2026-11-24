//! Everything the Risk section shows, from positions, candles and the user's
//! choices, in one pure call.

use std::collections::{BTreeSet, HashMap};

use super::beta::{self, BetaRow, Exposure};
use super::cluster::{self, Cluster};
use super::cov::{self, Estimator};
use super::returns;
use super::var::{self, Confidence, Contribution, Tail};
use super::{Matrix, RiskPeriod};
use crate::market::candles::Candle;
use crate::market::{Interval, MarketId};

/// Fewer returns than this and a period's figures are not computed.
pub const MIN_RETURNS: usize = 3;

/// One open position, as the risk figures need it.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskPosition {
    pub market: MarketId,
    /// Signed USD value at the mark: negative is short.
    pub notional: f64,
    /// USD per unit.
    pub mark: f64,
}

impl RiskPosition {
    /// A HIP-3 market, priced by an oracle that stalls when its underlying
    /// market is closed.
    pub fn oracle_priced(&self) -> bool {
        !self.market.dex().is_empty()
    }
}

/// One market's candles at one interval, as far as the loader has got.
#[derive(Debug, Clone, PartialEq)]
pub enum SeriesState {
    Loading,
    Failed,
    Ready(Vec<Candle>),
}

/// What the user has chosen.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskSettings {
    /// The periods compared side by side.
    pub periods: BTreeSet<RiskPeriod>,
    /// The period the matrix, breakdown and beta views use.
    pub focus: RiskPeriod,
    pub estimator: Estimator,
    pub confidence: Confidence,
    /// The stored benchmark choice; see [`beta::default_benchmark`].
    pub benchmark: Option<MarketId>,
    pub threshold: f64,
}

impl Default for RiskSettings {
    fn default() -> Self {
        Self {
            periods: RiskPeriod::ALL.into_iter().collect(),
            focus: RiskPeriod::default(),
            estimator: Estimator::default(),
            confidence: Confidence::default(),
            benchmark: None,
            threshold: cluster::DEFAULT_THRESHOLD,
        }
    }
}

impl RiskSettings {
    /// Every candle interval these settings need, shortest first.
    pub fn intervals(&self) -> Vec<Interval> {
        let mut out: Vec<Interval> = Vec::new();
        for p in self.periods.iter().chain(std::iter::once(&self.focus)) {
            if !out.contains(&p.interval()) {
                out.push(p.interval());
            }
        }
        out.sort_by_key(|i| i.millis());
        out
    }
}

/// Why a held market is left out of every figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    /// Its candles could not be loaded.
    Failed,
    /// It has too few candles in a period to give a return.
    TooShort,
}

/// One row of the Periods table.
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodRow {
    pub period: RiskPeriod,
    pub state: PeriodState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PeriodState {
    Loading,
    TooFew { count: usize },
    Ready(PeriodFigures),
}

/// One period's headline figures. USD; VaR at one day.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeriodFigures {
    pub count: usize,
    /// The portfolio's standard deviation over one candle interval.
    pub sigma_candle: f64,
    pub var95: f64,
    pub var99: f64,
    pub cornish_fisher99: f64,
}

/// The focused period in full.
#[derive(Debug, Clone, PartialEq)]
pub struct Focus {
    pub period: RiskPeriod,
    pub count: usize,
    /// One-day covariance, in the order of [`RiskReport::included`].
    pub covariance: Matrix,
    pub correlation: Matrix,
    pub sigma: f64,
    pub parametric: f64,
    pub cornish_fisher: f64,
    /// Cornish-Fisher fell back to the normal quantile.
    pub cornish_fisher_fell_back: bool,
    pub skew: f64,
    pub excess_kurtosis: f64,
    /// Historical VaR and shortfall at one day.
    pub historical: Tail,
    pub contributions: Vec<Contribution>,
    /// Index into [`RiskReport::included`].
    pub benchmark: usize,
    pub betas: Vec<BetaRow>,
    pub exposure: Exposure,
    pub clusters: Vec<Cluster>,
    pub uncorrelated: Option<Cluster>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FocusState {
    /// No open positions to measure.
    Empty,
    Loading,
    TooFew {
        count: usize,
    },
    Ready(Box<Focus>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RiskReport {
    /// The positions the figures cover, largest |notional| first.
    pub included: Vec<RiskPosition>,
    pub excluded: Vec<(MarketId, Exclusion)>,
    pub periods: Vec<PeriodRow>,
    pub focus: FocusState,
}

type Candles = HashMap<(MarketId, Interval), SeriesState>;

impl RiskReport {
    /// The report for `positions` at `now_ms`. A series missing from
    /// `candles` counts as loading.
    pub fn compute(
        positions: &[RiskPosition],
        candles: &Candles,
        now_ms: u64,
        settings: &RiskSettings,
    ) -> Self {
        let loading = SeriesState::Loading;
        let state = |m: &MarketId, i: Interval| candles.get(&(m.clone(), i)).unwrap_or(&loading);
        let mut needed: Vec<RiskPeriod> = settings.periods.iter().copied().collect();
        if !needed.contains(&settings.focus) {
            needed.push(settings.focus);
        }

        let mut held: Vec<RiskPosition> = positions
            .iter()
            .filter(|p| p.notional != 0.0)
            .cloned()
            .collect();
        held.sort_by(|a, b| {
            b.notional
                .abs()
                .total_cmp(&a.notional.abs())
                .then_with(|| a.market.cmp(&b.market))
        });

        let mut included = Vec::new();
        let mut excluded = Vec::new();
        for p in held {
            let mut why = None;
            for period in &needed {
                match state(&p.market, period.interval()) {
                    SeriesState::Failed => why = Some(Exclusion::Failed),
                    SeriesState::Ready(c) if why.is_none() => {
                        let from = now_ms.saturating_sub(period.millis());
                        let r = returns::align(
                            &[c.as_slice()],
                            from,
                            now_ms,
                            period.interval().millis(),
                        );
                        if r.count() < 1 {
                            why = Some(Exclusion::TooShort);
                        }
                    }
                    _ => {}
                }
            }
            match why {
                Some(w) => excluded.push((p.market.clone(), w)),
                None => included.push(p),
            }
        }
        let w: Vec<f64> = included.iter().map(|p| p.notional).collect();

        let table = |period: RiskPeriod| -> Option<returns::Returns> {
            let mut series = Vec::new();
            for p in &included {
                match state(&p.market, period.interval()) {
                    SeriesState::Ready(c) => series.push(c.as_slice()),
                    _ => return None,
                }
            }
            let from = now_ms.saturating_sub(period.millis());
            Some(returns::align(
                &series,
                from,
                now_ms,
                period.interval().millis(),
            ))
        };

        let periods = RiskPeriod::ALL
            .into_iter()
            .filter(|p| settings.periods.contains(p))
            .map(|period| {
                let state = if included.is_empty() {
                    PeriodState::TooFew { count: 0 }
                } else {
                    match table(period) {
                        None => PeriodState::Loading,
                        Some(r) if r.count() < MIN_RETURNS => {
                            PeriodState::TooFew { count: r.count() }
                        }
                        Some(r) => match cov::estimate(&r.table, settings.estimator) {
                            None => PeriodState::TooFew { count: r.count() },
                            Some(native) => {
                                let day = native.scaled(period.per_day());
                                let (s, k) = var::moments(&var::pnl(&r.table, &w));
                                let sigma = var::sigma(&day, &w);
                                PeriodState::Ready(PeriodFigures {
                                    count: r.count(),
                                    sigma_candle: var::sigma(&native, &w),
                                    var95: Confidence::P95.z() * sigma,
                                    var99: Confidence::P99.z() * sigma,
                                    cornish_fisher99: var::cornish_fisher(Confidence::P99, s, k).0
                                        * sigma,
                                })
                            }
                        },
                    }
                };
                PeriodRow { period, state }
            })
            .collect();

        let focus = if included.is_empty() {
            if excluded.is_empty() {
                FocusState::Empty
            } else {
                FocusState::TooFew { count: 0 }
            }
        } else {
            match table(settings.focus) {
                None => FocusState::Loading,
                Some(r) if r.count() < MIN_RETURNS => FocusState::TooFew { count: r.count() },
                Some(r) => match cov::estimate(&r.table, settings.estimator) {
                    None => FocusState::TooFew { count: r.count() },
                    Some(native) => FocusState::Ready(Box::new(focus(
                        &included, &w, &r.table, native, settings,
                    ))),
                },
            }
        };

        Self {
            included,
            excluded,
            periods,
            focus,
        }
    }
}

fn focus(
    included: &[RiskPosition],
    w: &[f64],
    r: &Matrix,
    native: Matrix,
    settings: &RiskSettings,
) -> Focus {
    let period = settings.focus;
    let day = native.scaled(period.per_day());
    let correlation = cov::correlation(&day);
    let sigma = var::sigma(&day, w);
    let conf = settings.confidence;
    let pnl = var::pnl(r, w);
    let (skew, excess_kurtosis) = var::moments(&pnl);
    let (z_cf, cornish_fisher_fell_back) = var::cornish_fisher(conf, skew, excess_kurtosis);
    let root = period.per_day().sqrt();
    let tail = var::historical(&pnl, conf).expect("at least MIN_RETURNS returns");
    let historical = Tail {
        var: tail.var * root,
        shortfall: tail.shortfall * root,
        count: tail.count,
    };
    let held: Vec<(MarketId, f64)> = included
        .iter()
        .map(|p| (p.market.clone(), p.notional))
        .collect();
    let benchmark =
        beta::default_benchmark(&held, settings.benchmark.as_ref()).expect("positions are held");
    let bench_mark = included[benchmark].mark;
    let betas = beta::rows(&day, w, benchmark, bench_mark);
    let exposure = beta::exposure(w, &betas, bench_mark);
    let beta_values: Vec<f64> = betas.iter().map(|b| b.beta).collect();
    let (clusters, uncorrelated) =
        cluster::clusters(&correlation, settings.threshold, w, &beta_values);
    Focus {
        period,
        count: r.rows(),
        parametric: conf.z() * sigma,
        cornish_fisher: z_cf * sigma,
        cornish_fisher_fell_back,
        skew,
        excess_kurtosis,
        historical,
        contributions: var::contributions(&day, w, conf),
        benchmark,
        betas,
        exposure,
        clusters,
        uncorrelated,
        sigma,
        covariance: day,
        correlation,
    }
}
