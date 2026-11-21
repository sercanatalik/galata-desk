//! The Funding section: what each open position pays or earns in funding at
//! current rates and has since it opened, the current rate of each market
//! the account watches without holding, and the chosen market's funding rate
//! over time.
//!
//! Positions and rates are the Portfolio view's; the watchlist is the one
//! the Markets section keeps. What is this section's own is which row is
//! chosen, the chart's resolution, and the funding history it loads.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Local};
use galata_desk_core::feed::Section;
use galata_desk_core::format as f;
use galata_desk_core::funding::feed::{ChartSnapshot, HistoryFeed};
use galata_desk_core::funding::history::{self, HOUR_MS, Held, Resolution};
use galata_desk_core::funding::{self, FundingRates, WatchedRow};
use galata_desk_core::market::MarketId;
use galata_desk_core::market::feed::system_clock;
use galata_desk_core::market::history::Budget;
use galata_desk_core::market::watchlist::Watchlist;
use galata_desk_core::model::PositionRow;
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::rust_decimal::prelude::ToPrimitive;
use galata_desk_core::venue::{Http, Info};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, FocusHandle, Hsla, KeyDownEvent, PathBuilder, Pixels,
    ScrollHandle, Subscription, Task, TestSupportExt, Window, canvas, div, point, px,
};

use crate::Core;
use crate::grid::{Cell, Col, Grid, Tone};
use crate::portfolio::Portfolio;
use crate::theme::{MONO, Palette};
use crate::ui;

/// The table's columns: name and relative width.
const COLUMNS: [(&str, f32); 8] = [
    ("Market", 1.3),
    ("Size", 1.3),
    ("Position value", 1.1),
    ("Rate (1h)", 0.9),
    ("Yearly", 0.8),
    ("Per hour", 1.0),
    ("Per day", 1.0),
    ("Since open", 1.1),
];

const ROW_HEIGHT: f32 = 34.;
/// The table never shrinks below its header and this many rows; under that
/// it scrolls, so the chart beneath stays on screen.
const TABLE_MIN_ROWS: f32 = 3.;
/// The chart's plot, and the columns of labels either side of it.
const PLOT_HEIGHT: f32 = 176.;
const LEFT_AXIS: f32 = 62.;
const RIGHT_AXIS: f32 = 56.;
/// The chart panel under its heading: the figures, the legend, the plot and
/// its times. The states without a chart take the same height, so the table
/// above does not move when a chart arrives.
const CHART_BODY: f32 = 257.;

/// What the section last drew, for tests.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Drawn {
    /// The position rows' markets, then the watched rows', as coin names.
    pub positions: Vec<String>,
    pub watched: Vec<String>,
    /// Every row's figures after its name, in column order, joined by `|`:
    /// position rows, then watched rows. A dash is a figure there is none
    /// of; `...` is one still loading.
    pub figures_by_row: Vec<String>,
    /// The Positions card: how many earn and pay. Empty until it is known.
    pub earn_pay: String,
    /// What the chart panel says in place of a chart, when it has none.
    pub chart_says: String,
    /// The line beside the heading.
    pub as_of: String,
    /// The chart panel's state: `none` with no panel, else `loading`,
    /// `failed`, `empty`, `chart`, `part` or `young`.
    pub chart: &'static str,
    /// The line beside the chart's market.
    pub chart_note: String,
    /// The figures above the chart, as `name: value`.
    pub figures: Vec<String>,
    pub legend: String,
    /// How many points the rate line has.
    pub points: usize,
}

pub struct Funding {
    portfolio: Entity<Portfolio>,
    feed: HistoryFeed,
    /// The chosen market's history, as the feed last reported it.
    chart: ChartSnapshot,
    /// The row whose market the chart shows. A market, not a place in the
    /// table: rows move as rates do.
    chosen: Option<MarketId>,
    resolution: Resolution,
    /// The watchlist the Markets section saved; `None` when it never has.
    stored: Option<Watchlist>,
    /// The dexes whose rates the Portfolio view was last asked to keep.
    watched_dexes: BTreeSet<String>,
    /// One focus handle per row, so the keyboard can reach each.
    focus: HashMap<MarketId, FocusHandle>,
    /// Where the table is scrolled to. Its scrollbar reads this.
    scroll: ScrollHandle,
    drawn: RefCell<Drawn>,
    /// Stand-in positions, rates and watchlist for a screenshot.
    #[cfg(feature = "shot")]
    staged: Option<sample::Staged>,
    _watch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Funding {
    pub fn new(portfolio: Entity<Portfolio>, cx: &mut Context<Self>) -> Self {
        let core = Core::get(cx);
        let stored = core.store.watchlist();
        // The feed's task runs on the I/O runtime. It asks the venue nothing
        // until a market is chosen.
        let feed = {
            let _io = core.runtime.enter();
            HistoryFeed::start(
                Info::new(Http::at(core.info_url())),
                Budget::for_funding(),
                system_clock(),
            )
        };
        #[allow(unused_mut)]
        let mut snapshots = feed.snapshots();
        // As in the other sections: the headless tests give the view its
        // snapshots directly and do not listen to the feed.
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
                    #[cfg(feature = "shot")]
                    if this.staged.is_some() {
                        return;
                    }
                    this.chart = snapshot;
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        });
        // Positions and rates are the portfolio's: redraw when they change.
        let subscriptions = vec![cx.observe(&portfolio, |_, _, cx| cx.notify())];

        #[allow(unused_mut)]
        let mut this = Self {
            portfolio,
            feed,
            chart: ChartSnapshot::default(),
            chosen: None,
            resolution: Resolution::default(),
            stored,
            watched_dexes: BTreeSet::new(),
            focus: HashMap::new(),
            scroll: ScrollHandle::new(),
            drawn: RefCell::new(Drawn::default()),
            #[cfg(feature = "shot")]
            staged: None,
            _watch: watch,
            _subscriptions: subscriptions,
        };
        #[cfg(feature = "shot")]
        this.stage_for_shot();
        this
    }

    /// Read the watchlist again: the Markets section may have changed it
    /// since this section was last shown.
    pub fn refresh_watchlist(&mut self, cx: &mut Context<Self>) {
        self.stored = Core::get(cx).store.watchlist();
        cx.notify();
    }

    fn positions(&self, cx: &Context<Self>) -> Option<Vec<PositionRow>> {
        #[cfg(feature = "shot")]
        if let Some(staged) = &self.staged {
            return Some(staged.positions.clone());
        }
        self.portfolio.read(cx).positions().map(<[_]>::to_vec)
    }

    fn rates(&self, cx: &Context<Self>) -> Section<FundingRates> {
        #[cfg(feature = "shot")]
        if let Some(staged) = &self.staged {
            return staged.rates.clone();
        }
        self.portfolio.read(cx).rates().clone()
    }

    /// The watched markets: the stored list, or, when Markets has never
    /// saved one, the list it would start with. Nothing until the positions
    /// are known, since that default depends on them.
    fn watchlist(&self, positions: Option<&[PositionRow]>) -> Vec<MarketId> {
        #[cfg(feature = "shot")]
        if let Some(staged) = &self.staged {
            return staged.watchlist.clone();
        }
        match (&self.stored, positions) {
            (_, None) => Vec::new(),
            (Some(list), Some(_)) => list.markets().to_vec(),
            (None, Some(positions)) => Watchlist::default_for(positions).markets().to_vec(),
        }
    }

    fn choose(&mut self, market: &MarketId, cx: &mut Context<Self>) {
        if self.chosen.as_ref() != Some(market) {
            self.chosen = Some(market.clone());
            self.feed.choose(self.chosen.clone());
        }
        cx.notify();
    }

    fn set_resolution(&mut self, resolution: Resolution, cx: &mut Context<Self>) {
        self.resolution = resolution;
        self.feed.set_resolution(resolution);
        cx.notify();
    }

    /// Keep one row chosen while there are rows: the first, unless the
    /// chosen market is still among them.
    fn keep_a_row_chosen(&mut self, markets: &[MarketId]) {
        let still_there = self.chosen.as_ref().is_some_and(|m| markets.contains(m));
        if !still_there {
            let first = markets.first().cloned();
            if self.chosen != first {
                self.chosen = first;
                self.feed.choose(self.chosen.clone());
            }
        }
    }

    /// Ask the Portfolio view to keep rates for the watched markets' dexes.
    fn keep_watched_rates(&mut self, watched: &[WatchedRow], cx: &mut Context<Self>) {
        let dexes: BTreeSet<String> = watched
            .iter()
            .map(|row| row.market.dex().to_string())
            .collect();
        if dexes != self.watched_dexes {
            self.watched_dexes = dexes.clone();
            self.portfolio.read(cx).watch_dexes(dexes);
        }
    }

    /// One row of the table: focusable, and chosen by a click, Enter or Space.
    fn row(
        &mut self,
        ix: usize,
        market: &MarketId,
        cells: Vec<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let handle = self
            .focus
            .entry(market.clone())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        let focused = handle.is_focused(window);
        let chosen = self.chosen.as_ref() == Some(market);
        let click_market = market.clone();
        let key_market = market.clone();
        h_flex()
            .id(("funding-row", ix))
            .px_4()
            .h(px(ROW_HEIGHT))
            .flex_shrink_0()
            .gap_3()
            .border_b_1()
            .text_size(px(12.5))
            .cursor_pointer()
            .track_focus(&handle)
            .border_color(if chosen || focused {
                Palette::amber_line()
            } else {
                Palette::rule()
            })
            .when(chosen, |d| d.bg(Palette::amber_ground()))
            .when(!chosen, |d| d.hover(|d| d.bg(Palette::raise())))
            .on_click(cx.listener(move |this, _, _, cx| this.choose(&click_market, cx)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.choose(&key_market, cx);
                }
            }))
            .children(
                cells
                    .into_iter()
                    .enumerate()
                    .map(|(ix, content)| cell(ix, content)),
            )
            .test_support()
            .into_any_element()
    }

    /// The chosen market's funding rate over time.
    fn chart_panel(
        &self,
        market: &MarketId,
        position: Option<&PositionRow>,
        drawn: &mut Drawn,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let resolution = self.resolution;
        // Just after a row is chosen the feed may still hold the last one's.
        let current = self.chart.market.as_ref() == Some(market);
        let history = if current {
            self.chart.history.clone()
        } else {
            Arc::default()
        };
        let failed = current && self.chart.failed;
        let loading = !current || self.chart.loading;
        let shown = history.window(resolution);
        let held = position.map(|p| if p.is_long() { Held::Long } else { Held::Short });

        let side_words = match held {
            Some(Held::Long) => "you are long",
            Some(Held::Short) => "you are short",
            None => "no position",
        };
        // A market younger than the window: say when its history begins.
        let starts = history
            .starts_ms()
            .filter(|_| shown.buckets.len() < resolution.buckets());
        let state = if shown.buckets.is_empty() {
            if failed {
                "failed"
            } else if loading {
                "loading"
            } else {
                "empty"
            }
        } else if loading {
            "part"
        } else if starts.is_some() {
            "young"
        } else {
            "chart"
        };
        let mut note = format!("{} \u{b7} {side_words}", resolution.window_words());
        match state {
            "part" => note.push_str(" \u{b7} earlier history is loading"),
            "young" => {
                if let Some(at) = starts {
                    note.push_str(&format!(" \u{b7} history starts {}", date_label(at, false)));
                }
            }
            _ => {}
        }

        let heading = h_flex()
            .gap_3()
            .items_center()
            .child(
                h_flex()
                    .gap_2()
                    .text_size(px(14.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(div().font_family(MONO).child(market.ticker().to_string()))
                    .when(!market.dex().is_empty(), |d| {
                        d.child(
                            div()
                                .font_family(MONO)
                                .font_weight(gpui_kit::FontWeight::NORMAL)
                                .text_color(Palette::muted())
                                .child(market.dex().to_string()),
                        )
                    })
                    .child("funding rate"),
            )
            .child(ui::label(note.clone()))
            .child(div().flex_1())
            .child(
                TabBar::new("funding-resolution")
                    .segmented()
                    .xsmall()
                    .selected_index(
                        Resolution::ALL
                            .iter()
                            .position(|r| *r == resolution)
                            .unwrap_or(0),
                    )
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.set_resolution(Resolution::ALL[*ix], cx);
                    }))
                    .children(Resolution::ALL.map(Resolution::label)),
            );

        let name = market.ticker().to_string();
        let says = match state {
            "loading" => format!(
                "Loading the {} of funding for {name}",
                resolution.window_words().to_lowercase()
            ),
            "failed" => format!(
                "The funding history for {name} could not be loaded. The rates and flows above are not affected; retrying."
            ),
            "empty" => format!("Hyperliquid has no funding history for {name}."),
            _ => String::new(),
        };
        drawn.chart_says = says.clone();
        let body: AnyElement = match state {
            "loading" => v_flex()
                .id("funding-chart-loading")
                .h(px(CHART_BODY))
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .bg(Palette::raise())
                .text_size(px(12.5))
                .text_color(Palette::muted())
                .child(says)
                .test_support()
                .into_any_element(),
            "failed" => v_flex()
                .id("funding-chart-failed")
                .h(px(CHART_BODY))
                .gap_2()
                .child(ui::notice(says))
                // Where the chart will be.
                .child(
                    div()
                        .flex_1()
                        .rounded(px(6.))
                        .border_1()
                        .border_dashed()
                        .border_color(Palette::line()),
                )
                .test_support()
                .into_any_element(),
            "empty" => v_flex()
                .id("funding-chart-empty")
                .h(px(CHART_BODY))
                .items_center()
                .justify_center()
                .text_size(px(12.5))
                .text_color(Palette::muted())
                .child(says)
                .test_support()
                .into_any_element(),
            _ => {
                let summary = shown.summary().expect("there is a bucket to sum up");
                let mut figures: Vec<(String, String, Hsla)> = vec![
                    (
                        "Average".into(),
                        format!(
                            "{} {}",
                            f::pct_signed(summary.average, 4),
                            resolution.bucket_words()
                        ),
                        Palette::text(),
                    ),
                    (
                        "Yearly at that average".into(),
                        f::pct_signed(summary.yearly, 1),
                        Palette::text(),
                    ),
                    (
                        "Positive".into(),
                        format!(
                            "{} of {} {}",
                            summary.positive,
                            summary.buckets,
                            resolution.bucket_noun()
                        ),
                        Palette::text(),
                    ),
                    (
                        "Cumulative".into(),
                        f::pct_signed(summary.cumulative, 2),
                        Palette::text(),
                    ),
                ];
                if let (Some(side), Some(position)) = (held, position) {
                    let amount = history::what_if(side, position.value, summary.cumulative);
                    let side_word = match side {
                        Held::Long => "long",
                        Held::Short => "short",
                    };
                    let verb = if amount < Decimal::ZERO {
                        "paid"
                    } else {
                        "received"
                    };
                    figures.push((
                        format!("A {side_word} of today's size would have {verb}"),
                        f::usd_signed(amount),
                        tone(amount),
                    ));
                }
                drawn.figures = figures
                    .iter()
                    .map(|(name, value, _)| format!("{name}: {value}"))
                    .collect();
                drawn.points = shown.buckets.len();

                let legend = match held {
                    Some(side) => format!(
                        "Rate {} \u{b7} blue where your {} received funding, orange where it paid",
                        resolution.bucket_words(),
                        match side {
                            Held::Long => "long",
                            Held::Short => "short",
                        }
                    ),
                    None => format!(
                        "Rate {} \u{b7} above zero longs pay shorts, below zero shorts pay longs",
                        resolution.bucket_words()
                    ),
                };
                drawn.legend = legend.clone();
                let swatch = |colour: Hsla, width: f32| div().w(px(width)).h(px(2.)).bg(colour);
                let legend_row = h_flex()
                    .justify_between()
                    .gap_4()
                    .text_size(px(12.))
                    .text_color(Palette::muted())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(match held {
                                Some(_) => h_flex()
                                    .child(swatch(Palette::gain(), 9.))
                                    .child(swatch(Palette::loss(), 9.)),
                                None => h_flex().child(swatch(Palette::amber(), 18.)),
                            })
                            .child(legend),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(swatch(Palette::text(), 18.))
                            .child("Cumulative rate (right axis)"),
                    );

                v_flex()
                    .id("funding-chart-plot")
                    .gap(px(8.))
                    .when(failed, |d| {
                        d.child(ui::notice(format!(
                            "The funding history for {name} could not be brought up to date; retrying."
                        )))
                    })
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_x(px(26.))
                            .gap_y(px(4.))
                            .text_size(px(12.5))
                            .children(figures.into_iter().map(|(name, value, colour)| {
                                h_flex()
                                    .gap_2()
                                    .child(div().text_color(Palette::muted()).child(name))
                                    .child(div().font_family(MONO).text_color(colour).child(value))
                            })),
                    )
                    .child(legend_row)
                    .child(plot(&shown, held))
                    .test_support()
                    .into_any_element()
            }
        };
        drawn.chart = state;
        drawn.chart_note = note;

        ui::panel()
            .id("funding-chart")
            .flex_shrink_0()
            .px_4()
            .pt_3()
            .pb(px(14.))
            .gap(px(10.))
            .child(heading)
            .child(body)
            .test_support()
            .into_any_element()
    }
}

impl Render for Funding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let positions = self.positions(cx);
        let rates = self.rates(cx);
        let empty = FundingRates::new();
        let known = rates.data.as_ref();
        let rows = positions
            .as_deref()
            .map(|p| funding::rows(p, known.unwrap_or(&empty)));
        let watched = funding::watched(
            &self.watchlist(positions.as_deref()),
            positions.as_deref().unwrap_or_default(),
            known.unwrap_or(&empty),
        );
        let summary = rows.as_deref().map(funding::summary);
        // Flows need both the positions and the rates.
        let flows_ready = rows.is_some() && known.is_some();

        // Every row's market, in the order drawn: positions, then watched.
        let markets: Vec<MarketId> = rows
            .iter()
            .flatten()
            .map(|r| MarketId::from_parts(&r.dex, &r.market))
            .chain(watched.iter().map(|w| w.market.clone()))
            .collect();
        self.keep_a_row_chosen(&markets);
        self.keep_watched_rates(&watched, cx);
        self.focus.retain(|market, _| markets.contains(market));

        let mut drawn = Drawn {
            positions: rows
                .iter()
                .flatten()
                .map(|r| funding::coin(&r.market, &r.dex))
                .collect(),
            watched: watched
                .iter()
                .map(|w| w.market.coin().to_string())
                .collect(),
            chart: "none",
            ..Drawn::default()
        };

        let count =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut as_of = Vec::new();
        if let Some(p) = &positions {
            as_of.push(count(p.len(), "position", "positions"));
            if !watched.is_empty() {
                as_of.push(format!("{} watched without a position", watched.len()));
            }
            if let Some(at) = rates.fetched_at {
                as_of.push(format!(
                    "rates as of {}",
                    DateTime::<Local>::from(at).format("%H:%M:%S")
                ));
            }
        }
        let as_of = as_of.join(" \u{b7} ");
        drawn.as_of = as_of.clone();

        let pending = |w: f32, h: f32| ui::placeholder(px(w), px(h)).into_any_element();
        let card = |name: &'static str, value: AnyElement, note: String| {
            ui::panel()
                .flex_1()
                .min_w(px(180.))
                .px_4()
                .py_3()
                .gap(px(6.))
                .child(ui::label(name))
                .child(value)
                .child(ui::label(note).text_size(px(12.)))
        };
        let money = |value: Decimal| {
            ui::figure(f::usd_signed(value), px(21.))
                .text_color(tone(value))
                .into_any_element()
        };

        let left_out = summary
            .as_ref()
            .filter(|s| flows_ready && s.without_rate > 0)
            .map(|s| {
                format!(
                    "{} position{} without a rate left out",
                    s.without_rate,
                    if s.without_rate == 1 { "" } else { "s" }
                )
            });
        let cards = h_flex()
            .flex_wrap()
            .flex_shrink_0()
            .gap(px(10.))
            .child(card(
                "Net per hour",
                match &summary {
                    Some(s) if flows_ready => money(s.per_hour),
                    _ => pending(120., 24.),
                },
                left_out.clone().unwrap_or("At current rates".into()),
            ))
            .child(card(
                "Net per day",
                match &summary {
                    Some(s) if flows_ready => money(s.per_day()),
                    _ => pending(120., 24.),
                },
                "If rates and sizes hold".into(),
            ))
            .child(card(
                "Since positions opened",
                match &summary {
                    Some(s) => money(s.since_open),
                    None => pending(120., 24.),
                },
                "Open positions only".into(),
            ))
            .child(card(
                "Positions",
                match &summary {
                    Some(s) if flows_ready => {
                        let text = format!("{} earn \u{b7} {} pay", s.earning, s.paying);
                        drawn.earn_pay = text.clone();
                        ui::figure(text, px(21.)).into_any_element()
                    }
                    _ => pending(140., 24.),
                },
                "By the current rate".into(),
            ));

        let header = h_flex()
            .px_4()
            .h(px(ROW_HEIGHT))
            .flex_shrink_0()
            .gap_3()
            .border_b_1()
            .border_color(Palette::line())
            .text_size(px(12.5))
            .text_color(Palette::muted())
            .children(
                COLUMNS
                    .iter()
                    .enumerate()
                    .map(|(ix, (name, _))| cell(ix, div().child(*name).into_any_element())),
            );

        // A figure that depends on the rate: a placeholder while rates load,
        // or a dash when the venue's rates do not include the market.
        let rates_loading = known.is_none();
        let draw = |figures: &[Option<(String, Hsla)>]| -> (Vec<AnyElement>, String) {
            let cells = figures
                .iter()
                .map(|value| match value {
                    Some((text, colour)) => figure(text.clone(), *colour),
                    None if rates_loading => ui::placeholder(px(56.), px(12.)).into_any_element(),
                    None => dash(),
                })
                .collect();
            let text = figures
                .iter()
                .map(|value| match value {
                    Some((text, _)) => text.as_str(),
                    None if rates_loading => "...",
                    None => DASH,
                })
                .collect::<Vec<_>>()
                .join("|");
            (cells, text)
        };

        let mut ix = 0;
        let mut position_rows = Vec::new();
        for row in rows.iter().flatten() {
            let market = MarketId::from_parts(&row.dex, &row.market);
            let name = h_flex()
                .gap_2()
                .font_family(MONO)
                .child(row.market.clone())
                .when(!row.dex.is_empty(), |d| {
                    d.child(div().text_color(Palette::muted()).child(row.dex.clone()))
                })
                .child(
                    div()
                        .text_color(Palette::muted())
                        .child(format!("{}x", row.leverage)),
                )
                .into_any_element();
            let (mut cells, text) = draw(&coloured(held_figures(row)));
            cells.insert(0, name);
            drawn.figures_by_row.push(text);
            position_rows.push(self.row(ix, &market, cells, window, cx));
            ix += 1;
        }
        let mut watched_rows = Vec::new();
        for row in &watched {
            let name = h_flex()
                .gap_2()
                .font_family(MONO)
                .child(row.market.ticker().to_string())
                .when(!row.market.dex().is_empty(), |d| {
                    d.child(
                        div()
                            .text_color(Palette::muted())
                            .child(row.market.dex().to_string()),
                    )
                })
                .into_any_element();
            let (mut cells, text) = draw(&coloured(watched_figures(row)));
            cells.insert(0, name);
            drawn.figures_by_row.push(text);
            watched_rows.push(self.row(ix, &row.market, cells, window, cx));
            ix += 1;
        }

        let nothing_watched = watched_rows.is_empty();
        let table_body: AnyElement = match &rows {
            None => v_flex()
                .p_4()
                .gap_3()
                .children((0..6).map(|_| ui::placeholder(px(9999.), px(14.))))
                .into_any_element(),
            Some(rows) => {
                let held: AnyElement = if rows.is_empty() {
                    h_flex()
                        .id("funding-no-positions")
                        .h(px(if nothing_watched { 120. } else { 56. }))
                        .flex_shrink_0()
                        .justify_center()
                        .border_b_1()
                        .border_color(if nothing_watched {
                            gpui_kit::transparent_black()
                        } else {
                            Palette::rule()
                        })
                        .text_color(Palette::muted())
                        .child("No open positions, so no funding to pay or earn.")
                        .test_support()
                        .into_any_element()
                } else {
                    v_flex()
                        .id("funding-rows")
                        .flex_shrink_0()
                        .children(position_rows)
                        .test_support()
                        .into_any_element()
                };
                let scrolled = v_flex()
                    .id("funding-table")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(held)
                    .when(!nothing_watched, |d| {
                        d.child(
                            h_flex()
                                .px_4()
                                .h(px(28.))
                                .flex_shrink_0()
                                .border_b_1()
                                .border_color(Palette::rule())
                                .text_size(px(12.))
                                .text_color(Palette::muted())
                                .child("Watched in Markets, no position"),
                        )
                        .child(
                            v_flex()
                                .id("funding-watched")
                                .flex_shrink_0()
                                .children(watched_rows)
                                .test_support(),
                        )
                    })
                    .test_support();
                // The rows often do not fit above the chart. The bar is
                // always drawn, so that is seen without scrolling to find
                // out; it draws nothing when everything fits.
                div()
                    .relative()
                    .size_full()
                    .child(scrolled)
                    .child(
                        div().absolute().inset_0().child(
                            Scrollbar::vertical(&self.scroll)
                                .id("funding-table-scrollbar")
                                .mode(ScrollbarMode::Always)
                                .viewport_from_layout(),
                        ),
                    )
                    .into_any_element()
            }
        };

        // The chosen row's market, and the account's position in it if any.
        let chart = self.chosen.clone().map(|market| {
            let position = positions
                .iter()
                .flatten()
                .find(|p| p.market == market.ticker() && p.dex == market.dex())
                .cloned();
            self.chart_panel(&market, position.as_ref(), &mut drawn, cx)
        });
        *self.drawn.borrow_mut() = drawn;

        v_flex()
            .id("funding")
            .size_full()
            .p_3()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_baseline()
                    .flex_shrink_0()
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Funding"),
                    )
                    .child(ui::label(as_of).text_size(px(13.))),
            )
            .when(rates.is_stale(), |d| {
                d.child(ui::notice(
                    "The funding rates could not be refreshed. The rates and flows below are the last received; retrying.",
                ))
            })
            .when(rates.data.is_none() && rates.error.is_some(), |d| {
                d.child(ui::notice(
                    "The funding rates could not be loaded. Position sizes and funding since open are shown; retrying.",
                ))
            })
            .child(cards)
            .child(
                ui::panel()
                    .flex_1()
                    .min_h(px(ROW_HEIGHT * (TABLE_MIN_ROWS + 1.)))
                    .child(header)
                    .child(div().flex_1().min_h_0().child(table_body)),
            )
            .children(chart)
            .child(
                ui::label(
                    "A plus is funding you receive, a minus funding you pay. Per hour and per day assume the current rate and position size hold. Positions come first, largest flows first; then the markets you watch in Markets without a position, in the order you added them. Choose a row to chart its funding rate.",
                )
                .flex_shrink_0(),
            )
            .test_support()
    }
}

/// A held position's figures after its market: size, value, the rate
/// hourly and yearly, and the flows per hour, per day and since it opened.
/// `None` where the rate is not known.
fn held_figures(row: &funding::FundingRow) -> [Option<(String, Tone)>; 7] {
    let signed = |v: Decimal| (f::usd_signed(v), Tone::signed(sign_of(v)));
    [
        Some((
            f::size(row.size, &row.market),
            if row.size >= Decimal::ZERO {
                Tone::Gain
            } else {
                Tone::Loss
            },
        )),
        Some((f::usd(row.value), Tone::Text)),
        row.rate.map(|r| (f::funding_hourly(r), Tone::Text)),
        row.rate.map(|r| (f::funding_yearly(r), Tone::Text)),
        row.per_hour.map(signed),
        row.per_day().map(signed),
        Some(signed(row.since_open)),
    ]
}

/// A watched market's figures: nothing is held, so only the rate columns
/// have one.
fn watched_figures(row: &WatchedRow) -> [Option<(String, Tone)>; 7] {
    let none = || Some((DASH.to_string(), Tone::Muted));
    [
        none(),
        none(),
        row.rate.map(|r| (f::funding_hourly(r), Tone::Text)),
        row.rate.map(|r| (f::funding_yearly(r), Tone::Text)),
        none(),
        none(),
        none(),
    ]
}

fn coloured(figures: [Option<(String, Tone)>; 7]) -> [Option<(String, Hsla)>; 7] {
    figures.map(|f| f.map(|(text, tone)| (text, tone.color())))
}

fn sign_of(value: Decimal) -> i32 {
    if value > Decimal::ZERO {
        1
    } else if value < Decimal::ZERO {
        -1
    } else {
        0
    }
}

impl Funding {
    /// The table's rows as a list, for a blotter: the held positions, then
    /// the watched markets, with the same figures this section shows.
    pub fn blotter_grid(&self, cx: &Context<Self>) -> Grid {
        let mut grid = Grid::new(
            COLUMNS
                .iter()
                .enumerate()
                .map(|(ix, (name, width))| {
                    if ix == 0 {
                        Col::text(name, 120. * width)
                    } else {
                        Col::number(name, 120. * width)
                    }
                })
                .collect(),
            "No open positions or watched markets",
        );
        let positions = self.positions(cx);
        let rates = self.rates(cx);
        let empty = FundingRates::new();
        let known = rates.data.as_ref();
        let Some(positions) = positions else {
            grid.set_empty("Loading\u{2026}");
            return grid;
        };
        let rows = funding::rows(&positions, known.unwrap_or(&empty));
        let watched = funding::watched(
            &self.watchlist(Some(&positions)),
            &positions,
            known.unwrap_or(&empty),
        );
        let line = |name: String, dex: &str, figures: [Option<(String, Tone)>; 7]| {
            let mut name = Cell::text(name);
            if !dex.is_empty() {
                name = name.and(format!(" {dex}"), Tone::Tag);
            }
            std::iter::once(name)
                .chain(figures.into_iter().map(|figure| match figure {
                    Some((text, tone)) => Cell::new(text, tone),
                    // As the section draws it: still loading, or none.
                    None if known.is_none() => Cell::muted("..."),
                    None => Cell::muted(DASH),
                }))
                .collect::<Vec<_>>()
        };
        let all = rows
            .iter()
            .map(|r| {
                line(
                    format!("{} {}x", r.market, r.leverage),
                    &r.dex,
                    held_figures(r),
                )
            })
            .chain(watched.iter().map(|w| {
                line(
                    w.market.ticker().to_string(),
                    w.market.dex(),
                    watched_figures(w),
                )
            }))
            .collect();
        grid.set_rows(all);
        grid.set_row_markets(
            rows.iter()
                .map(|r| Some(MarketId::from_parts(&r.dex, &r.market)))
                .chain(watched.iter().map(|w| Some(w.market.clone())))
                .collect(),
        );
        grid
    }
}

fn tone(value: Decimal) -> Hsla {
    ui::tone_of(if value > Decimal::ZERO {
        1
    } else if value < Decimal::ZERO {
        -1
    } else {
        0
    })
}

/// One cell of the table, as wide as its column.
fn cell(ix: usize, content: AnyElement) -> gpui_kit::Div {
    let (_, width) = COLUMNS[ix];
    div()
        .flex_basis(px(0.))
        // Relative widths, as flex factors.
        .flex_grow(width)
        .flex_shrink(1.)
        .when(ix > 0, |d| d.flex().justify_end())
        .child(content)
}

fn figure(text: String, colour: Hsla) -> AnyElement {
    div()
        .font_family(MONO)
        .text_color(colour)
        .child(text)
        .into_any_element()
}

/// Where there is no figure to show.
const DASH: &str = "\u{2014}";

fn dash() -> AnyElement {
    figure(DASH.into(), Palette::muted())
}

/// A day, and the hour too where the chart is cut by the hour.
fn date_label(ms: u64, with_hour: bool) -> String {
    let at = DateTime::from_timestamp_millis(ms as i64)
        .unwrap_or_default()
        .with_timezone(&Local);
    if with_hour {
        at.format("%-d %b %H:%M").to_string()
    } else {
        at.format("%-d %b").to_string()
    }
}

/// The plot: the rate per bucket against the left scale and its running
/// total against the right, on one zero line, with the scales' labels either
/// side and five times beneath.
///
/// The rate line is cut where it crosses zero. With a position each stretch
/// takes the colour of what that side did there, received or paid; without
/// one the whole line is amber.
fn plot(shown: &history::Window, held: Option<Held>) -> AnyElement {
    let slots = shown.resolution.buckets();
    let count = shown.buckets.len();
    // The buckets are the window's newest: a short history sits at the right.
    let x = move |i: usize| (slots - count + i) as f64 / (slots - 1) as f64;
    let percent = |d: Decimal| d.to_f64().unwrap_or_default() * 100.;
    let rates: Vec<f64> = shown.buckets.iter().map(|b| percent(b.rate)).collect();
    let totals: Vec<f64> = shown.cumulative().into_iter().map(percent).collect();
    let axes = history::axes(&rates, &totals);

    let colour_of = |positive: bool| match held {
        None => Palette::amber(),
        Some(side) => {
            let rate = if positive {
                Decimal::ONE
            } else {
                -Decimal::ONE
            };
            if side.sign_of(rate) > 0 {
                Palette::gain()
            } else {
                Palette::loss()
            }
        }
    };
    let rate_points: Vec<(f64, f64)> = rates.iter().enumerate().map(|(i, r)| (x(i), *r)).collect();
    let stretches: Vec<(Hsla, Vec<(f64, f64)>)> = history::split_at_zero(&rate_points)
        .into_iter()
        .map(|s| {
            (
                colour_of(s.positive),
                s.points
                    .into_iter()
                    .map(|(x, v)| (x, axes.left_y(v)))
                    .collect(),
            )
        })
        .collect();
    let total_line: Vec<(f64, f64)> = totals
        .iter()
        .enumerate()
        .map(|(i, t)| (x(i), axes.right_y(*t)))
        .collect();
    let steps = axes.steps();
    let zero_y = axes.zero_y();

    let lines = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let at = |x: f64, y: f64| {
                point(
                    bounds.origin.x + bounds.size.width * x as f32,
                    bounds.origin.y + bounds.size.height * y as f32,
                )
            };
            let mut stroke = |points: &[(f64, f64)], width: f32, colour: Hsla, dash: &[Pixels]| {
                let mut path = PathBuilder::stroke(px(width));
                if !dash.is_empty() {
                    path = path.dash_array(dash);
                }
                let mut points = points.iter();
                let Some(&(x, y)) = points.next() else {
                    return;
                };
                path.move_to(at(x, y));
                for &(x, y) in points {
                    path.line_to(at(x, y));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, colour);
                }
            };
            // The grid: one line per step, and a dotted one at zero.
            for k in 0..=steps {
                let y = k as f64 / steps as f64;
                if (y - zero_y).abs() > 1e-9 {
                    stroke(&[(0., y), (1., y)], 1., Palette::rule(), &[]);
                }
            }
            stroke(
                &[(0., zero_y), (1., zero_y)],
                1.,
                Palette::faint(),
                &[px(2.), px(4.)],
            );
            stroke(&total_line, 1.5, Palette::text(), &[]);
            for (colour, points) in &stretches {
                stroke(points, 1.5, *colour, &[]);
            }
        },
    )
    .size_full();

    let ticks = axes.ticks();
    let label = |text: String, at: usize| {
        div()
            .absolute()
            .top(px(PLOT_HEIGHT * at as f32 / steps as f32 - 7.))
            .font_family(MONO)
            .text_size(px(11.))
            .text_color(Palette::muted())
            .child(text)
    };
    let left_labels = ticks
        .iter()
        .enumerate()
        .map(|(i, (left, _))| label(history::axis_label(*left, axes.left_step), i).right_0());
    let right_labels = ticks
        .iter()
        .enumerate()
        .map(|(i, (_, right))| label(history::axis_label(*right, axes.right_step), i).left_0());

    // Five times along the bottom, from the window's first bucket to its last.
    let span = shown.resolution.bucket_hours() * HOUR_MS;
    let end = shown.buckets.last().map(|b| b.end_ms).unwrap_or_default();
    let start = end.saturating_sub((slots as u64 - 1) * span);
    let by_hour = shown.resolution == Resolution::Hour;
    let times = (0..5u64).map(|i| {
        div()
            .font_family(MONO)
            .text_size(px(11.))
            .text_color(Palette::muted())
            .child(date_label(start + (end - start) * i / 4, by_hour))
    });

    v_flex()
        .gap_1()
        .pt(px(6.))
        .child(
            h_flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .relative()
                        .w(px(LEFT_AXIS))
                        .h(px(PLOT_HEIGHT))
                        .flex_shrink_0()
                        .children(left_labels),
                )
                .child(div().flex_1().min_w_0().h(px(PLOT_HEIGHT)).child(lines))
                .child(
                    div()
                        .relative()
                        .w(px(RIGHT_AXIS))
                        .h(px(PLOT_HEIGHT))
                        .flex_shrink_0()
                        .children(right_labels),
                ),
        )
        .child(
            h_flex()
                .justify_between()
                .pl(px(LEFT_AXIS + 8.))
                .pr(px(RIGHT_AXIS + 8.))
                .children(times),
        )
        .into_any_element()
}

/// Made-up funding histories, for tests and screenshots.
#[cfg(any(test, feature = "shot"))]
pub mod sample {
    use super::*;
    use galata_desk_core::funding::history::{History, hour_of};

    /// `hours` of hourly rates ending at `end_ms`: mostly a small positive
    /// rate, with days that spike either way. With `since_listing`, the
    /// venue has nothing older.
    pub fn history(end_ms: u64, hours: u64, since_listing: bool) -> History {
        let end = hour_of(end_ms);
        let first = end - (hours - 1) * HOUR_MS;
        let mut seed: u64 = 20_261_118;
        let mut next = move || {
            seed = (seed * 1_664_525 + 1_013_904_223) % 4_294_967_296;
            seed as f64 / 4_294_967_296.
        };
        let spikes = [
            (21, -12.),
            (28, 22.),
            (40, 16.),
            (57, -8.),
            (58, 15.),
            (71, -9.),
            (76, 26.),
            (79, -34.),
            (83, 42.),
            (85, -16.),
            (86, 50.),
            (87, 20.),
        ];
        let days = hours.div_ceil(24);
        let mut rows = Vec::new();
        for day in 0..days {
            // Counted from the oldest day of a 90-day window, so a shorter
            // history shows that window's newest days.
            let of_ninety = day + 90u64.saturating_sub(days);
            let level = spikes
                .iter()
                .find(|(d, _)| *d == of_ninety)
                .map(|(_, level)| *level)
                .unwrap_or_else(|| 6. + next() * 7.);
            let swing = (level.abs() * 0.5_f64).max(5.);
            for hour in 0..24 {
                let at = first + (day * 24 + hour) * HOUR_MS;
                if at > end {
                    break;
                }
                // In millionths, so the rate is a round decimal: 9 is 0.0009%.
                let rate = (level + (next() - 0.5) * swing).round() as i64;
                rows.push((at + 47, Decimal::new(rate, 6)));
            }
        }
        let mut history = History::default();
        // Asked from an hour earlier, a first row later than that says the
        // venue has nothing older.
        let from = if since_listing {
            first - HOUR_MS
        } else {
            first
        };
        history.merge_page(from, rows);
        history
    }

    /// A chart snapshot for `market` holding `history`.
    pub fn snapshot(
        market: &str,
        resolution: Resolution,
        history: History,
        loading: bool,
        failed: bool,
    ) -> ChartSnapshot {
        ChartSnapshot {
            market: Some(MarketId::new(market)),
            resolution,
            history: Arc::new(history),
            loading,
            failed,
        }
    }

    /// Stand-ins for what the Portfolio view and the store would supply.
    #[cfg(feature = "shot")]
    pub struct Staged {
        pub positions: Vec<PositionRow>,
        pub rates: Section<FundingRates>,
        pub watchlist: Vec<MarketId>,
    }
}

#[cfg(feature = "shot")]
impl Funding {
    /// `funding=positions|watched|none|loading|part|failed|young|empty` in
    /// `GALATA_DESK_SHOT_STATE`, with `fres=1h|8h|D`: show that state of the
    /// section with made-up positions, rates and history, whatever the
    /// account holds. `watched` charts a market without a position; `none`
    /// has no open position.
    fn stage_for_shot(&mut self) {
        use galata_desk_core::venue::MarginMode;
        use std::time::SystemTime;

        let setting = std::env::var("GALATA_DESK_SHOT_STATE").unwrap_or_default();
        let get = |key: &str| {
            setting
                .split(',')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        };
        let Some(state) = get("funding") else {
            return;
        };
        self.resolution = match get("fres").as_deref() {
            Some("1h") => Resolution::Hour,
            Some("8h") => Resolution::EightHours,
            _ => Resolution::Day,
        };
        let d = |s: &str| s.parse::<Decimal>().expect("a number");
        let position =
            |market: &str, dex: &str, leverage, size: &str, value: &str, paid: &str| PositionRow {
                asset: None,
                market: market.into(),
                dex: dex.into(),
                leverage,
                mode: MarginMode::Cross,
                size: d(size),
                value: d(value),
                entry: Decimal::ONE,
                mark: None,
                pnl: Decimal::ZERO,
                roe: Decimal::ZERO,
                liquidation: None,
                margin: Decimal::ZERO,
                funding: d(paid),
            };
        let positions = if state == "none" {
            Vec::new()
        } else {
            vec![
                position("BTC", "", 20, "1.25", "118750", "-1337.59"),
                position("ETH", "", 10, "-24", "74400", "904.17"),
                position("NVDA", "xyz", 10, "-301.6", "48210", "412.36"),
                position("SOL", "", 5, "210", "31500", "-88.20"),
                position("HYPE", "", 5, "620", "18600", "61.05"),
                position("GOLD", "xyz", 3, "2", "8240", "-3.11"),
            ]
        };
        let rates: FundingRates = [
            ("BTC", "0.000013"),
            ("ETH", "0.000011"),
            ("xyz:NVDA", "0.000006"),
            ("SOL", "0.000008"),
            ("HYPE", "-0.000009"),
            ("xyz:GOLD", "0.000002"),
            ("DOGE", "-0.000005"),
            ("XRP", "0.000019"),
            ("xyz:SILVER", "0.000001"),
            ("xyz:TSLA", "0.000012"),
        ]
        .into_iter()
        .map(|(coin, rate)| (coin.to_string(), d(rate)))
        .collect();
        let watchlist: Vec<MarketId> = ["BTC", "DOGE", "xyz:NVDA", "XRP", "xyz:SILVER", "xyz:TSLA"]
            .map(MarketId::new)
            .to_vec();
        let chosen = match state.as_str() {
            "watched" => "XRP",
            "none" => "DOGE",
            "loading" | "failed" | "part" => "SOL",
            "young" | "empty" => "xyz:GOLD",
            _ => "xyz:NVDA",
        };
        let now = system_clock()();
        let window = self.resolution.window_hours();
        let (history, loading, failed) = match state.as_str() {
            "loading" => (Default::default(), true, false),
            "failed" => (Default::default(), true, true),
            "part" => (sample::history(now, 500.min(window), false), true, false),
            "young" => (
                sample::history(now, (28 * 24).min(window), true),
                false,
                false,
            ),
            "empty" => {
                let mut none = galata_desk_core::funding::history::History::default();
                none.merge_page(now, []);
                (none, false, false)
            }
            _ => (sample::history(now, window, false), false, false),
        };
        self.chosen = Some(MarketId::new(chosen));
        self.chart = sample::snapshot(chosen, self.resolution, history, loading, failed);
        self.staged = Some(sample::Staged {
            positions,
            rates: Section {
                data: Some(rates),
                fetched_at: Some(SystemTime::now()),
                ..Section::default()
            },
            watchlist,
        });
    }
}

#[cfg(test)]
impl Funding {
    /// Replace what the feed has reported, for tests that have no venue.
    pub fn set_chart(&mut self, snapshot: ChartSnapshot, cx: &mut Context<Self>) {
        self.chart = snapshot;
        cx.notify();
    }

    /// What was last drawn.
    pub fn drawn(&self) -> Drawn {
        self.drawn.borrow().clone()
    }

    pub fn chosen_market(&self) -> Option<String> {
        self.chosen.as_ref().map(|m| m.coin().to_string())
    }

    pub fn resolution(&self) -> Resolution {
        self.resolution
    }

    /// Put the keyboard focus on a market's row.
    pub fn focus_market(&self, market: &str, window: &mut Window, cx: &mut Context<Self>) {
        let handle = &self.focus[&MarketId::new(market)];
        window.focus(handle, cx);
        cx.notify();
    }
}
