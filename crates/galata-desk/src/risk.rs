//! The Risk section: how much the open positions could lose in a day, each
//! one's share of that, their exposure to a benchmark, and which of them
//! move together.
//!
//! Positions are the Portfolio view's. What is this section's own is the
//! candle history it loads and the user's choices: the periods compared and
//! focused, the covariance estimator, the confidence, the benchmark and the
//! cluster threshold. All the arithmetic is
//! [`galata_desk_core::risk::RiskReport`]; this file only draws it.

mod beta;
mod matrix;
mod var;

use std::cell::RefCell;

use galata_desk_core::market::feed::{Clock, system_clock};
use galata_desk_core::market::{Interval, MarketId};
use galata_desk_core::model::PositionRow;
use galata_desk_core::risk::feed::{RiskCandles, RiskFeed};
use galata_desk_core::risk::report::{Exclusion, FocusState};
use galata_desk_core::risk::{
    Confidence, Estimator, RiskPeriod, RiskPosition, RiskReport, RiskSettings,
};
use galata_desk_core::rust_decimal::prelude::ToPrimitive;
use galata_desk_core::venue::Http;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, Subscription, Task, TestSupportExt, Window, div, px};

use crate::Core;
use crate::portfolio::Portfolio;
use crate::theme::Palette;
use crate::ui;

/// Which of the section's two views is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    VarCovariance,
    BetaClusters,
}

/// What the matrix shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatrixView {
    #[default]
    Correlation,
    Covariance,
}

/// What the section last drew, for tests.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Drawn {
    /// `empty`, `loading`, `too-few` or `ready`.
    pub state: &'static str,
    /// The cards along the top, as `name: value`.
    pub cards: Vec<String>,
    /// The Periods table, one line per row.
    pub periods: Vec<String>,
    /// The matrix's diagonal cells, as drawn.
    pub diagonal: Vec<String>,
    /// The line beside the matrix's title.
    pub matrix_note: String,
    /// The breakdown, as `market component`.
    pub components: Vec<(String, String)>,
    /// Parametric VaR as the breakdown states it.
    pub var: String,
    /// The beta table, one line per row: market, beta, R², equivalent.
    pub betas: Vec<String>,
    /// Markets marked as poorly explained by the benchmark.
    pub low_r2: Vec<String>,
    /// Markets marked as oracle-priced.
    pub oracle: Vec<String>,
    /// The clusters, as their member coins joined by `+`.
    pub clusters: Vec<String>,
    /// The notice naming excluded markets, if any.
    pub excluded: String,
}

pub struct Risk {
    portfolio: Entity<Portfolio>,
    feed: RiskFeed,
    candles: RiskCandles,
    clock: Clock,
    settings: RiskSettings,
    view: View,
    matrix: MatrixView,
    /// What the feed was last asked for, so it is asked again only on change.
    wanted: (Vec<MarketId>, Vec<Interval>),
    drawn: RefCell<Drawn>,
    _watch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Risk {
    pub fn new(portfolio: Entity<Portfolio>, cx: &mut Context<Self>) -> Self {
        let core = Core::get(cx);
        let settings = RiskSettings {
            benchmark: core.risk_benchmark.clone(),
            ..RiskSettings::default()
        };
        // The feed's task runs on the I/O runtime. It asks the venue nothing
        // until the section is first drawn.
        let feed = {
            let _io = core.runtime.enter();
            RiskFeed::live(Http::at(core.info_url()))
        };
        #[allow(unused_mut)]
        let mut snapshots = feed.snapshots();
        // As in the other sections: the headless tests give the view its
        // candles directly and do not listen to the feed.
        #[cfg(test)]
        let watch = {
            drop(snapshots);
            Task::ready(())
        };
        #[cfg(not(test))]
        let watch = cx.spawn(async move |this, cx| {
            while snapshots.changed().await.is_ok() {
                let snapshot = snapshots.borrow_and_update().clone();
                let applied = this.update(cx, |this: &mut Self, cx| {
                    this.candles = snapshot;
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        });
        let subscriptions = vec![cx.observe(&portfolio, |_, _, cx| cx.notify())];
        Self {
            portfolio,
            feed,
            candles: RiskCandles::default(),
            clock: system_clock(),
            settings,
            view: View::default(),
            matrix: MatrixView::default(),
            wanted: (Vec::new(), Vec::new()),
            drawn: RefCell::new(Drawn::default()),
            _watch: watch,
            _subscriptions: subscriptions,
        }
    }

    fn positions(&self, cx: &Context<Self>) -> Option<Vec<RiskPosition>> {
        self.portfolio
            .read(cx)
            .positions()
            .map(|rows| rows.iter().filter_map(risk_position).collect())
    }

    /// Ask the feed for every held market at every interval the choices
    /// need, the focused period's first.
    fn keep_loading(&mut self, positions: &[RiskPosition]) {
        let markets: Vec<MarketId> = positions.iter().map(|p| p.market.clone()).collect();
        let mut intervals = vec![self.settings.focus.interval()];
        for i in self.settings.intervals() {
            if !intervals.contains(&i) {
                intervals.push(i);
            }
        }
        let wanted = (markets, intervals);
        if wanted != self.wanted {
            self.feed.want(wanted.0.clone(), wanted.1.clone());
            self.wanted = wanted;
        }
    }

    fn set_benchmark(&mut self, market: MarketId, cx: &mut Context<Self>) {
        self.settings.benchmark = Some(market.clone());
        cx.global_mut::<Core>().risk_benchmark = Some(market);
        // A choice that could not be saved still applies until the app quits.
        let core = Core::get(cx);
        let _ = core.store.save_settings(&core.settings());
        cx.notify();
    }

    fn controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.settings;
        let group = |name: &'static str, body: AnyElement| {
            v_flex()
                .gap(px(6.))
                .child(ui::label(name).text_size(px(11.5)))
                .child(body)
        };
        let periods = h_flex()
            .gap_3()
            .children(RiskPeriod::ALL.into_iter().map(|p| {
                Checkbox::new(("risk-compare", p as usize))
                    .label(p.label())
                    .checked(s.periods.contains(&p))
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        if *checked {
                            this.settings.periods.insert(p);
                        } else {
                            this.settings.periods.remove(&p);
                        }
                        cx.notify();
                    }))
            }));
        let focus = TabBar::new("risk-focus")
            .segmented()
            .xsmall()
            .selected_index(
                RiskPeriod::ALL
                    .iter()
                    .position(|p| *p == s.focus)
                    .unwrap_or(0),
            )
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.settings.focus = RiskPeriod::ALL[*ix];
                cx.notify();
            }))
            .children(RiskPeriod::ALL.map(|p| p.label()));
        let estimator = TabBar::new("risk-estimator")
            .segmented()
            .xsmall()
            .selected_index(
                Estimator::ALL
                    .iter()
                    .position(|e| *e == s.estimator)
                    .unwrap_or(0),
            )
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.settings.estimator = Estimator::ALL[*ix];
                cx.notify();
            }))
            .children(Estimator::ALL.map(|e| e.label()));
        let confidence = TabBar::new("risk-confidence")
            .segmented()
            .xsmall()
            .selected_index(
                Confidence::ALL
                    .iter()
                    .position(|c| *c == s.confidence)
                    .unwrap_or(0),
            )
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.settings.confidence = Confidence::ALL[*ix];
                cx.notify();
            }))
            .children(Confidence::ALL.map(|c| c.label()));
        h_flex()
            .flex_wrap()
            .items_end()
            .gap(px(24.))
            .child(group("Focused period", focus.into_any_element()))
            .child(group("Periods compared", periods.into_any_element()))
            .child(group("Estimator", estimator.into_any_element()))
            .child(group("Confidence", confidence.into_any_element()))
            .child(group(
                "Horizon",
                ui::figure("1 day", px(13.)).h(px(24.)).into_any_element(),
            ))
            .into_any_element()
    }
}

/// A portfolio row as the risk figures need it: signed USD value and the
/// mark, from the row's own mark or its value over its size.
fn risk_position(row: &PositionRow) -> Option<RiskPosition> {
    let notional = row.signed_value().to_f64()?;
    let mark = row
        .mark
        .or_else(|| (!row.size.is_zero()).then(|| row.value.abs() / row.size.abs()))
        .and_then(|m| m.to_f64())
        .unwrap_or(0.0);
    Some(RiskPosition {
        market: MarketId::from_parts(&row.dex, &row.market),
        notional,
        mark,
    })
}

impl Render for Risk {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let positions = self.positions(cx);
        if let Some(p) = &positions {
            self.keep_loading(p);
        }
        let now = (self.clock)();
        let report = positions
            .as_ref()
            .map(|p| RiskReport::compute(p, &self.candles.series, now, &self.settings));

        let mut drawn = Drawn::default();
        let view_tabs = TabBar::new("risk-view")
            .underline()
            .small()
            .selected_index(match self.view {
                View::VarCovariance => 0,
                View::BetaClusters => 1,
            })
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.view = if *ix == 0 {
                    View::VarCovariance
                } else {
                    View::BetaClusters
                };
                cx.notify();
            }))
            .children(["VaR & covariance", "Beta & clusters"]);

        let mut page = v_flex()
            .w_full()
            .max_w(px(1440.))
            .gap(px(16.))
            .p_5()
            .child(view_tabs)
            .child(self.controls(cx));

        if let Some(r) = &report
            && !r.excluded.is_empty()
        {
            let names: Vec<String> = r
                .excluded
                .iter()
                .map(|(m, why)| {
                    format!(
                        "{} ({})",
                        m.coin(),
                        match why {
                            Exclusion::Failed => "history could not be loaded",
                            Exclusion::TooShort => "too little history",
                        }
                    )
                })
                .collect();
            let text = format!("Figures exclude {}.", names.join(", "));
            drawn.excluded = text.clone();
            page = page.child(ui::notice(text).id("risk-excluded").test_support());
        }

        let body: AnyElement = match (&positions, &report) {
            (None, _) | (_, None) => {
                drawn.state = "loading";
                ui::placeholder(px(420.), px(24.)).into_any_element()
            }
            (Some(_), Some(r)) => match &r.focus {
                FocusState::Empty => {
                    drawn.state = "empty";
                    ui::panel()
                        .id("risk-empty")
                        .test_support()
                        .p_5()
                        .child(ui::label("No open positions: there is nothing to measure."))
                        .into_any_element()
                }
                FocusState::Loading => {
                    drawn.state = "loading";
                    v_flex()
                        .gap(px(16.))
                        .child(
                            h_flex()
                                .id("risk-loading")
                                .test_support()
                                .gap_3()
                                .child(ui::placeholder(px(220.), px(64.)))
                                .child(ui::placeholder(px(220.), px(64.)))
                                .child(ui::placeholder(px(220.), px(64.)))
                                .child(ui::label("Loading candle history…")),
                        )
                        .when(self.view == View::VarCovariance, |d| {
                            d.child(var::periods(self, r, &mut drawn))
                        })
                        .into_any_element()
                }
                FocusState::TooFew { count } => {
                    drawn.state = "too-few";
                    v_flex()
                        .gap(px(16.))
                        .child(ui::notice(format!(
                            "{} has only {count} return{} shared by every position: too few to estimate risk.",
                            self.settings.focus.label(),
                            if *count == 1 { "" } else { "s" }
                        )))
                        .when(self.view == View::VarCovariance, |d| {
                            d.child(var::periods(self, r, &mut drawn))
                        })
                        .into_any_element()
                }
                FocusState::Ready(f) => {
                    drawn.state = "ready";
                    match self.view {
                        View::VarCovariance => var::render(self, r, f, &mut drawn, cx),
                        View::BetaClusters => beta::render(self, r, f, &mut drawn, cx),
                    }
                }
            },
        };
        if let Some(r) = &report {
            drawn.oracle = r
                .included
                .iter()
                .filter(|p| p.oracle_priced())
                .map(|p| p.market.coin().to_string())
                .collect();
        }
        *self.drawn.borrow_mut() = drawn;

        div()
            .id("risk")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .child(h_flex().w_full().justify_center().child(page.child(body)))
    }
}

// ---- Formatting -------------------------------------------------------------

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Whole dollars: `$4,213`, `-$950`.
pub(crate) fn usd(v: f64) -> String {
    let r = v.round();
    let sign = if r < 0.0 { "-" } else { "" };
    format!("{sign}${}", group(r.abs() as u64))
}

/// Whole dollars with a sign: `+$2,400`, `-$21,000`, `$0`.
pub(crate) fn usd_signed(v: f64) -> String {
    if v.round() > 0.0 {
        format!("+{}", usd(v))
    } else {
        usd(v)
    }
}

/// A quantity of a market's units to four significant figures, signed:
/// `+0.02449 BTC`, `-65,625 DOGE`.
pub(crate) fn units(v: f64, market: &MarketId) -> String {
    let a = v.abs();
    let body = if a >= 1000.0 {
        group(a.round() as u64)
    } else if a == 0.0 {
        "0".into()
    } else {
        let places = (3 - a.log10().floor() as i32).max(0) as usize;
        format!("{a:.places$}")
    };
    let sign = if v > 0.0 && body != "0" {
        "+"
    } else if v < 0.0 && body != "0" {
        "-"
    } else {
        ""
    };
    format!("{sign}{body} {}", market.ticker())
}

/// A card along the top of a view.
pub(crate) fn card(
    name: String,
    value: String,
    note: String,
    tone: gpui_kit::Hsla,
    drawn: &mut Drawn,
) -> AnyElement {
    drawn.cards.push(format!("{name}: {value}"));
    ui::panel()
        .flex_1()
        .min_w(px(200.))
        .px_4()
        .py_3()
        .gap(px(6.))
        .child(ui::label(name))
        .child(ui::figure(value, px(22.)).text_color(tone))
        .child(ui::label(note).text_size(px(12.)))
        .into_any_element()
}

/// A small amber tag beside a market's name.
pub(crate) fn tag(text: &'static str) -> AnyElement {
    div()
        .px(px(6.))
        .rounded(px(4.))
        .bg(Palette::amber_ground())
        .border_1()
        .border_color(Palette::amber_line())
        .text_color(Palette::amber_text())
        .text_size(px(10.5))
        .child(text)
        .into_any_element()
}

#[cfg(test)]
impl Risk {
    pub fn drawn(&self) -> Drawn {
        self.drawn.borrow().clone()
    }

    /// Hand the view candles as the feed would.
    pub fn set_candles(
        &mut self,
        series: std::collections::HashMap<
            (MarketId, Interval),
            galata_desk_core::risk::SeriesState,
        >,
        cx: &mut Context<Self>,
    ) {
        self.candles = RiskCandles {
            version: self.candles.version + 1,
            series: std::sync::Arc::new(series),
        };
        cx.notify();
    }

    /// Read the clock from here instead of the system's.
    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    pub fn settings(&self) -> &RiskSettings {
        &self.settings
    }

    /// What the feed was last asked for.
    pub fn wanted(&self) -> &(Vec<MarketId>, Vec<Interval>) {
        &self.wanted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(usd(4213.4), "$4,213");
        assert_eq!(usd(-950.0), "-$950");
        assert_eq!(usd_signed(2400.0), "+$2,400");
        assert_eq!(usd_signed(-21000.0), "-$21,000");
        assert_eq!(usd_signed(0.2), "$0");
        let btc = MarketId::new("BTC");
        assert_eq!(units(0.024_489_8, &btc), "+0.02449 BTC");
        assert_eq!(units(-0.81, &btc), "-0.8100 BTC");
        assert_eq!(units(-65_625.0, &MarketId::new("DOGE")), "-65,625 DOGE");
        assert_eq!(units(12.5, &MarketId::new("xyz:SILVER")), "+12.50 SILVER");
    }
}
