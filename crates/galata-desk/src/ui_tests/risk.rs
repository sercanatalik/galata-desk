//! The Risk section. The view is given positions through the portfolio and
//! candles directly, as the feed would hand them over, on a fixed clock.

use std::collections::HashMap;
use std::sync::Arc;

use galata_desk_core::market::candles::Candle;
use galata_desk_core::market::{Interval, MarketId};
use galata_desk_core::model::PositionRow;
use galata_desk_core::risk::{Confidence, RiskPeriod, SeriesState};
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::rust_decimal::prelude::FromPrimitive;
use galata_desk_core::store::Store;
use galata_desk_core::venue::MarginMode;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{AppContext, Entity, TestAppContext, WindowHandle, base::Root};

use super::{open, open_sized};
use crate::Core;
use crate::desk::{Desk, Section};
use crate::risk::Risk;

const DAY: u64 = 86_400_000;
const NOW: u64 = 1_794_873_600_000;

/// (coin, notional, mark, daily vol, loading on the common driver)
const BOOK: [(&str, f64, f64, f64, f64); 6] = [
    ("BTC", 40_000.0, 98_000.0, 0.028, 0.85),
    ("ETH", 50_000.0, 3_400.0, 0.036, 0.85),
    ("SOL", 20_000.0, 190.0, 0.045, 0.80),
    ("DOGE", -15_000.0, 0.32, 0.052, 0.70),
    ("xyz:SILVER", 30_000.0, 31.0, 0.018, 0.05),
    ("xyz:NVDA", -10_000.0, 180.0, 0.026, 0.25),
];

fn row(coin: &str, notional: f64, mark: f64) -> PositionRow {
    let (dex, market) = coin.split_once(':').unwrap_or(("", coin));
    let size = notional / mark;
    PositionRow {
        asset: None,
        market: market.into(),
        dex: dex.into(),
        leverage: 5,
        mode: MarginMode::Cross,
        size: Decimal::from_f64(size).unwrap(),
        value: Decimal::from_f64(notional.abs()).unwrap(),
        entry: Decimal::from_f64(mark).unwrap(),
        mark: Some(Decimal::from_f64(mark).unwrap()),
        pnl: Decimal::ZERO,
        roe: Decimal::ZERO,
        liquidation: None,
        margin: Decimal::ZERO,
        funding: Decimal::ZERO,
    }
}

fn book() -> Vec<PositionRow> {
    BOOK.iter().map(|(c, n, m, _, _)| row(c, *n, *m)).collect()
}

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

fn candles() -> HashMap<(MarketId, Interval), SeriesState> {
    let mut all = HashMap::new();
    for interval in [Interval::H8, Interval::D1, Interval::D3] {
        let step = interval.millis();
        let per_day = DAY as f64 / step as f64;
        let n = (400 * DAY / step) as usize;
        let start = NOW - n as u64 * step;
        let mut d = Draws(11);
        let mut closes: Vec<Vec<f64>> = BOOK.iter().map(|b| vec![b.2]).collect();
        for _ in 1..n {
            let common = d.next();
            for (i, (_, _, _, vol, load)) in BOOK.iter().enumerate() {
                let r =
                    vol / per_day.sqrt() * (load * common + (1.0 - load * load).sqrt() * d.next());
                let last = *closes[i].last().unwrap();
                closes[i].push(last * r.exp());
            }
        }
        for (i, (coin, ..)) in BOOK.iter().enumerate() {
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
            all.insert((MarketId::new(*coin), interval), SeriesState::Ready(series));
        }
    }
    all
}

/// Open the app with the book held and go to Risk; give it candles when
/// `loaded`.
fn go_to_risk(
    cx: &mut TestAppContext,
    positions: Vec<PositionRow>,
    loaded: Option<HashMap<(MarketId, Interval), SeriesState>>,
) -> (
    WindowHandle<Root>,
    Entity<Desk>,
    Entity<Risk>,
    tempfile::TempDir,
) {
    let (window, desk, dir) = open_sized(
        cx,
        true,
        gpui_kit::size(gpui_kit::px(1480.), gpui_kit::px(2600.)),
    );
    let risk = cx
        .update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            let portfolio = desk.read(cx).portfolio_view().unwrap();
            portfolio.update(cx, |view, cx| view.set_positions(positions, cx));
            let risk = desk.read(cx).risk_view().unwrap();
            risk.update(cx, |view, cx| {
                view.set_clock(Arc::new(|| NOW));
                if let Some(c) = loaded {
                    view.set_candles(c, cx);
                }
            });
            window.within("sections").click(4usize, cx);
            risk
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    (window, desk, risk, dir)
}

fn redraw(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

/// Parse `+$1,234` or `-$56` as dollars.
fn dollars(text: &str) -> f64 {
    let negative = text.starts_with('-');
    let v: f64 = text
        .trim_start_matches(['+', '-', '$'])
        .replace(['$', ','], "")
        .parse()
        .unwrap();
    if negative { -v } else { v }
}

#[gpui_kit::test]
fn risk_follows_history_and_keeps_its_choices_across_sections(cx: &mut TestAppContext) {
    let (window, desk, risk, _dir) = go_to_risk(cx, book(), Some(candles()));
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(desk.read(cx).section(), Some(Section::Risk));
        assert!(window.try_find("risk").is_some());
        assert!(window.try_find("account-footer").is_some());
        assert_eq!(risk.read(cx).drawn().state, "ready");
        // 180D focused, 95%.
        window.within("risk-focus").click(2usize, cx);
        window.within("risk-confidence").click(0usize, cx);
        // To Markets and every other section, and back.
        for ix in [1usize, 2, 3, 0] {
            window.within("sections").click(ix, cx);
        }
        window.within("sections").click(4usize, cx);
    })
    .unwrap();
    redraw(cx, window);
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(desk.read(cx).section(), Some(Section::Risk));
        let s = risk.read(cx).settings().clone();
        assert_eq!(s.focus, RiskPeriod::HalfYear);
        assert_eq!(s.confidence, Confidence::P95);
        assert!(window.try_find("risk").is_some());
        // The feed was asked for every held market, the focused interval first.
        let (markets, intervals) = risk.read(cx).wanted().clone();
        assert_eq!(markets.len(), 6);
        assert_eq!(intervals[0], Interval::D1);
        assert_eq!(intervals.len(), 3);
    })
    .unwrap();
}

#[gpui_kit::test]
fn no_control_on_the_risk_screen_trades(cx: &mut TestAppContext) {
    let (window, _desk, _risk, _dir) = go_to_risk(cx, book(), Some(candles()));
    for view in [0usize, 1] {
        cx.update_window(window.into(), |_, window, cx| {
            window.within("risk-view").click(view, cx)
        })
        .unwrap();
        redraw(cx, window);
        cx.update_window(window.into(), |_, window, _| {
            for forbidden in [
                "Trade", "Buy", "Sell", "Close", "Cancel", "Deposit", "Withdraw", "Transfer",
            ] {
                assert!(
                    window.find_all_by_label(forbidden).is_empty(),
                    "a `{forbidden}` control is in a read-only section"
                );
            }
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn with_no_positions_risk_says_there_is_nothing_to_measure(cx: &mut TestAppContext) {
    let (window, _desk, risk, _dir) = go_to_risk(cx, Vec::new(), Some(candles()));
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(risk.read(cx).drawn().state, "empty");
        assert!(window.try_find("risk-empty").is_some());
        assert!(risk.read(cx).drawn().cards.is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
fn while_history_loads_no_figure_reads_zero(cx: &mut TestAppContext) {
    let (window, _desk, risk, _dir) = go_to_risk(cx, book(), None);
    cx.update_window(window.into(), |_, window, cx| {
        let drawn = risk.read(cx).drawn();
        assert_eq!(drawn.state, "loading");
        assert!(window.try_find("risk-loading").is_some());
        assert!(drawn.cards.is_empty());
        assert!(!drawn.periods.is_empty());
        assert!(
            drawn.periods.iter().all(|p| !p.contains('$')),
            "{:?}",
            drawn.periods
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_market_whose_history_fails_is_named_and_left_out(cx: &mut TestAppContext) {
    let mut c = candles();
    c.insert(
        (MarketId::new("xyz:NVDA"), Interval::D1),
        SeriesState::Failed,
    );
    let (window, _desk, risk, _dir) = go_to_risk(cx, book(), Some(c));
    cx.update_window(window.into(), |_, window, cx| {
        let drawn = risk.read(cx).drawn();
        assert_eq!(drawn.state, "ready");
        assert!(drawn.excluded.contains("xyz:NVDA"), "{}", drawn.excluded);
        assert!(window.try_find("risk-excluded").is_some());
        assert!(drawn.components.iter().all(|(m, _)| m != "xyz:NVDA"));
        assert_eq!(drawn.components.len(), 5);
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_matrix_switches_between_correlation_and_covariance(cx: &mut TestAppContext) {
    let (window, _desk, risk, _dir) = go_to_risk(cx, book(), Some(candles()));
    cx.update_window(window.into(), |_, window, cx| {
        let drawn = risk.read(cx).drawn();
        assert_eq!(drawn.diagonal, vec!["1.00"; 6]);
        assert!(!drawn.matrix_note.contains("10⁻⁴"));
        window.within("risk-matrix").click(1usize, cx);
    })
    .unwrap();
    redraw(cx, window);
    cx.update_window(window.into(), |_, _, cx| {
        let drawn = risk.read(cx).drawn();
        assert!(drawn.matrix_note.contains("×10⁻⁴"), "{}", drawn.matrix_note);
        assert!(drawn.diagonal.iter().all(|d| d != "1.00"));
        // HIP-3 markets are marked.
        assert_eq!(drawn.oracle, ["xyz:SILVER", "xyz:NVDA"]);
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_breakdown_adds_up_to_the_var_it_states(cx: &mut TestAppContext) {
    let (window, _desk, risk, _dir) = go_to_risk(cx, book(), Some(candles()));
    cx.update_window(window.into(), |_, _, cx| {
        let drawn = risk.read(cx).drawn();
        let sum: f64 = drawn.components.iter().map(|(_, c)| dollars(c)).sum();
        let var = dollars(&drawn.var);
        // Each component is rounded to the dollar.
        assert!(
            (sum - var).abs() <= drawn.components.len() as f64,
            "{sum} vs {var}"
        );
        assert!(
            drawn.cards[0].starts_with("Parametric VaR 99%: $"),
            "{:?}",
            drawn.cards
        );
        // A focused period's row is in the table, with one-day figures.
        assert_eq!(drawn.periods.len(), 4);
        assert!(drawn.periods[0].starts_with("30D|8h|"));
        assert!(drawn.periods[0].contains("×√3"));
        assert!(drawn.periods[1].contains("|native|"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn unticking_a_period_drops_its_row(cx: &mut TestAppContext) {
    let (window, _desk, risk, _dir) = go_to_risk(cx, book(), Some(candles()));
    cx.update_window(window.into(), |_, window, cx| {
        window.click(("risk-compare", RiskPeriod::Month as usize), cx);
    })
    .unwrap();
    redraw(cx, window);
    cx.update_window(window.into(), |_, _, cx| {
        let drawn = risk.read(cx).drawn();
        assert_eq!(drawn.periods.len(), 3);
        assert!(drawn.periods.iter().all(|p| !p.starts_with("30D")));
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_benchmark_and_threshold_restate_the_beta_view(cx: &mut TestAppContext) {
    let (window, _desk, risk, dir) = go_to_risk(cx, book(), Some(candles()));
    cx.update_window(window.into(), |_, window, cx| {
        window.within("risk-view").click(1usize, cx)
    })
    .unwrap();
    redraw(cx, window);
    let before = cx
        .update_window(window.into(), |_, window, cx| {
            let drawn = risk.read(cx).drawn();
            // BTC by default: its own row reads beta 1.
            assert!(
                drawn.betas.iter().any(|b| b.starts_with("BTC|1.00|1.00|")),
                "{:?}",
                drawn.betas
            );
            assert!(drawn.low_r2.contains(&"xyz:SILVER".to_string()));
            assert!(
                drawn
                    .cards
                    .iter()
                    .any(|c| c.starts_with("Hedge to β-neutral: -"))
            );
            // Included order is by size: ETH, BTC, SILVER, SOL, DOGE, NVDA.
            window.within("risk-benchmark").click(0usize, cx);
            drawn.clusters
        })
        .unwrap();
    redraw(cx, window);
    cx.update_window(window.into(), |_, window, cx| {
        let drawn = risk.read(cx).drawn();
        assert!(
            drawn.betas.iter().any(|b| b.starts_with("ETH|1.00|1.00|")),
            "{:?}",
            drawn.betas
        );
        assert_eq!(Core::get(cx).risk_benchmark, Some(MarketId::new("ETH")));
        window.within("risk-threshold").click(2usize, cx);
    })
    .unwrap();
    redraw(cx, window);
    cx.update_window(window.into(), |_, _, cx| {
        let after = risk.read(cx).drawn().clusters;
        // A higher threshold never merges: at least as many groups.
        assert!(after.len() >= before.len(), "{before:?} -> {after:?}");
    })
    .unwrap();
    assert_eq!(
        Store::at(dir.path()).settings().risk_benchmark,
        Some(MarketId::new("ETH"))
    );
}

#[gpui_kit::test]
fn risk_is_offered_only_while_connected(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open(cx, false);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(desk.read(cx).section(), None);
        assert!(window.find_all_by_label("Risk").is_empty());
    })
    .unwrap();
}
