//! A candle chart of one market: the account's fills, its open orders and
//! entry, and the watcher's armed levels drawn on it. The windowed layout's
//! chart follows the watchlist's selection; a workspace chart keeps its own
//! market.

use galata_desk_core::format as f;
use galata_desk_core::market::candles::Candle;
use galata_desk_core::market::feed::{Chart, ChartKey, ChartView, Tile};
use galata_desk_core::market::fills::{self, CandleFills};
use galata_desk_core::market::{MarketId, MarketPeriod};
use galata_desk_core::rust_decimal::Decimal;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, SharedString, Subscription, TestSupportExt, Window, div, px,
};

use super::dock::{DockMeta, PanelKind, PanelRecord, dock_panel, market_name};
use super::links::LinkEvent;
use super::model::{MarketSource, MarketsModel};
use super::{
    CallMenu, Calls, Entry, OrderLevel, PlotPointer, candle_chart, ghost_candles, interval_words,
    order_levels, time_label,
};
use crate::theme::Palette;
use crate::ui;

pub struct ChartPanel {
    model: Entity<MarketsModel>,
    source: MarketSource,
    /// What the chart area last drew, for tests: its state, how many buy and
    /// sell marks, and the "shown since" time.
    drawn_chart: std::cell::Cell<(&'static str, usize, usize, Option<u64>)>,
    /// Where the pointer is over the chart, for "Alert at <price>".
    plot: PlotPointer,
    /// Why the last call level added from the chart was not saved.
    call_problem: Option<SharedString>,
    /// The time cursor this chart last told its links about.
    sent_cursor: Option<u64>,
    /// The chart's own period; `None` follows the toolbar's.
    period: Option<MarketPeriod>,
    /// The chart this panel holds in the feed, while it keeps its own
    /// market. The windowed chart follows the selection, which the feed
    /// charts anyway, and holds nothing.
    held: Option<ChartKey>,
    /// Its key and links in the workspace.
    dock: DockMeta,
    _subscriptions: Vec<Subscription>,
}

dock_panel!(ChartPanel, PanelKind::Chart, ChartPanel::period_menu);

impl ChartPanel {
    pub fn new(model: Entity<MarketsModel>, source: MarketSource, cx: &mut Context<Self>) -> Self {
        let dock = DockMeta::fresh(cx);
        Self::build(model, source, None, dock, cx)
    }

    /// A workspace chart of its own market and period.
    pub fn docked(
        model: Entity<MarketsModel>,
        market: Option<MarketId>,
        period: Option<MarketPeriod>,
        dock: DockMeta,
        cx: &mut Context<Self>,
    ) -> Self {
        // The chart owns its period: a new chart, or one saved without a
        // period, starts at the toolbar's and keeps it.
        let period = period.or(Some(model.read(cx).period));
        Self::build(model, MarketSource::Own(market), period, dock, cx)
    }

    /// The header's period menu: every period, with the candles it gives.
    fn period_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use gpui_kit::component::Sizable as _;
        use gpui_kit::component::button::{Button, ButtonVariants as _};
        use gpui_kit::component::menu::DropdownMenu as _;
        let current = self.period(cx);
        let panel = cx.entity().downgrade();
        Some(
            Button::new("chart-period")
                .ghost()
                .xsmall()
                .label(current.label())
                .accessibility_label(format!("Chart period: {}", current.label()))
                .dropdown_menu(move |mut menu, _, _| {
                    for period in MarketPeriod::ALL {
                        let panel = panel.clone();
                        menu = menu.item(
                            PopupMenuItem::new(period_item(period))
                                .checked(period == current)
                                .on_click(move |_, _, cx| {
                                    let _ =
                                        panel.update(cx, |p, cx| p.set_period(Some(period), cx));
                                }),
                        );
                    }
                    menu
                })
                .into_any_element(),
        )
    }

    fn build(
        model: Entity<MarketsModel>,
        source: MarketSource,
        period: Option<MarketPeriod>,
        dock: DockMeta,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            // Fills, orders and the entry are the portfolio's.
            cx.observe(&model.read(cx).portfolio.clone(), |_, _, cx| cx.notify()),
            // A closed chart lets its candles go.
            cx.on_release(|this: &mut Self, cx| {
                if let Some(key) = this.held.take() {
                    this.model.update(cx, |m, _| m.release(&key));
                }
            }),
        ];
        let mut this = Self {
            model,
            source,
            drawn_chart: std::cell::Cell::new(("none", 0, 0, None)),
            plot: PlotPointer::default(),
            call_problem: None,
            sent_cursor: None,
            period,
            held: None,
            dock,
            _subscriptions: subscriptions,
        };
        this.hold(cx);
        this
    }

    /// The period the chart covers: its own, or the toolbar's.
    pub fn period(&self, cx: &gpui_kit::App) -> MarketPeriod {
        self.period.unwrap_or(self.model.read(cx).period)
    }

    /// What the chart saves in a layout.
    pub fn record(&self, cx: &gpui_kit::App) -> PanelRecord {
        self.dock
            .record(PanelKind::Chart, self.market(cx).as_ref(), self.period, cx)
    }

    /// The panel's title: the market and the period it covers.
    pub fn title_text(&self, cx: &gpui_kit::App) -> String {
        match self.market(cx) {
            Some(market) => format!(
                "{} \u{b7} {}",
                market_name(&market),
                self.period(cx).label()
            ),
            None => "Chart".to_string(),
        }
    }

    /// The pointer moved over the chart, or left it: show the time cursor
    /// at the candle under it, and tell the linked charts.
    fn pointer_moved(
        &mut self,
        at: Option<gpui_kit::Point<gpui_kit::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let time = at.and_then(|at| self.plot.time_at(at));
        if time == self.sent_cursor {
            return;
        }
        self.sent_cursor = time;
        self.plot.cursor.set(time);
        cx.notify();
        self.dock.publish(LinkEvent::TimeCursor(time), cx);
    }

    /// A click on the candles: tell the linked order ticket the price under
    /// the pointer, for its limit price. Nothing is sent.
    fn price_clicked(&mut self, at: gpui_kit::Point<gpui_kit::Pixels>, cx: &mut Context<Self>) {
        if !self.plot.bounds.get().is_some_and(|b| b.contains(&at)) {
            return;
        }
        self.plot.track(at);
        let (Some(market), Some(price)) = (self.market(cx), self.plot.price()) else {
            return;
        };
        self.dock.publish(LinkEvent::LimitPrice(market, price), cx);
    }

    /// Apply a linked panel's event. This never publishes.
    pub fn apply(&mut self, event: &LinkEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event {
            LinkEvent::TimeCursor(time) => self.apply_time_cursor(*time, cx),
            LinkEvent::SetMarket(market) => self.set_market(Some(market.clone()), cx),
            LinkEvent::LimitPrice(..) | LinkEvent::FilterBlotter(_) => {}
        }
    }

    fn apply_time_cursor(&mut self, time: Option<u64>, cx: &mut Context<Self>) {
        if self.plot.cursor.replace(time) != time {
            cx.notify();
        }
    }

    /// The price under the pointer, for tests.
    #[cfg(test)]
    pub fn pointer_price(&self) -> Option<galata_desk_core::rust_decimal::Decimal> {
        self.plot.price()
    }

    /// Where the time cursor is drawn, for tests.
    #[cfg(test)]
    pub fn time_cursor(&self) -> Option<u64> {
        self.plot.cursor.get()
    }

    /// Cover `period`, or follow the toolbar's with `None`.
    pub fn set_period(&mut self, period: Option<MarketPeriod>, cx: &mut Context<Self>) {
        if self.period != period {
            self.period = period;
            self.hold(cx);
            cx.notify();
        }
    }

    /// Hold the chart this panel shows in the feed, and let go of the one
    /// it showed before.
    fn hold(&mut self, cx: &mut Context<Self>) {
        let want = match &self.source {
            MarketSource::Own(Some(market)) => Some(ChartKey {
                market: market.clone(),
                period: self.period(cx),
            }),
            _ => None,
        };
        if want == self.held {
            return;
        }
        let old = std::mem::replace(&mut self.held, want.clone());
        self.model.update(cx, |m, _| {
            if let Some(key) = want {
                m.acquire(key);
            }
            if let Some(key) = old {
                m.release(&key);
            }
        });
    }

    /// What the chart has to draw: its period, its candles once loaded,
    /// and whether loading them failed.
    fn view(&self, market: &MarketId, cx: &gpui_kit::App) -> (MarketPeriod, Option<Chart>, bool) {
        let period = self.period(cx);
        let m = self.model.read(cx);
        match &self.held {
            Some(key) => match m.snapshot.charts.get(key) {
                Some(ChartView::Ready(chart)) => (period, Some(chart.clone()), false),
                Some(ChartView::Failed) => (period, None, true),
                _ => (period, None, false),
            },
            // The windowed chart is the selected market's. It belongs to the
            // panel only while it is this market's: just after a new
            // selection the feed may still hold the last one.
            None => {
                let chart =
                    m.snapshot.chart.clone().filter(|chart| {
                        &chart.market == market && chart.interval == period.interval()
                    });
                (period, chart, m.snapshot.chart_failed)
            }
        }
    }

    /// The market the chart shows.
    pub fn market(&self, cx: &gpui_kit::App) -> Option<MarketId> {
        self.source.market(&self.model, cx)
    }

    /// Show `market`, for a chart that keeps its own.
    #[allow(dead_code, reason = "the workspace's chart panels use it")]
    pub fn set_market(&mut self, market: Option<MarketId>, cx: &mut Context<Self>) {
        if self.source.set(market) {
            self.hold(cx);
            cx.notify();
        }
    }

    /// What the chart area last drew: `loading`, `failed`, `short`, `chart`
    /// or `none`; how many `B` and `S` marks; and the "shown since" time.
    #[cfg(test)]
    pub fn drawn_chart(&self) -> (&'static str, usize, usize, Option<u64>) {
        self.drawn_chart.get()
    }

    /// The price range the chart was last drawn over: low and high.
    #[cfg(test)]
    pub fn chart_range(&self) -> Option<(f64, f64)> {
        self.plot.range.get().map(|(low, high, _)| (low, high))
    }

    /// Add an armed level in `market` at `price`, its direction from `last`.
    pub fn add_call(
        &mut self,
        market: String,
        price: Decimal,
        last: Option<Decimal>,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = self.model.read(cx).watcher.clone() else {
            return;
        };
        let adding = link.update(cx, |link, cx| {
            link.add_level(market, price, None, None, last, cx)
        });
        self.call_problem = None;
        cx.spawn(async move |this, cx| {
            if let Err(why) = adding.await {
                let _ = this.update(cx, |this, cx| {
                    this.call_problem = Some(why.into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The panel's right side: the market's candles over the period.
    #[allow(clippy::too_many_arguments)]
    fn chart(
        &self,
        period: MarketPeriod,
        chart_failed: bool,
        tile: &Tile,
        candles: Option<&[Candle]>,
        grouped: &[CandleFills],
        levels: &[OrderLevel],
        calls: Calls,
        entry: Option<Entry>,
        since: Option<u64>,
        // The system asks for reduced motion: placeholders hold still.
        still: bool,
    ) -> AnyElement {
        let interval = period.interval();
        // A period under an hour is too few candles to read, so the chart
        // shows the hour it ends; the heading says so.
        let shown = if period.chart_millis() > period.millis() {
            format!("{}, last hour", interval_words(interval))
        } else {
            interval_words(interval).to_string()
        };
        // The venue returns only the latest fills: say so when they do not
        // reach back to the chart's first candle.
        let span_ms = candles
            .and_then(|c| Some(c.last()?.open_ms.saturating_sub(c.first()?.open_ms)))
            .unwrap_or_default();
        let since_note = since.map(|at| {
            format!(
                "Your trades shown since {}",
                time_label(at, span_ms.max(24 * 3_600_000))
            )
        });
        // In the workspace the panel's header already names the market and
        // period. The line gives way to a narrow chart rather than widen it.
        let docked = matches!(self.source, MarketSource::Own(_));
        let squeeze = |d: gpui_kit::Div| {
            d.min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
        };
        let heading = h_flex()
            .min_w_0()
            .overflow_hidden()
            .gap_4()
            .text_size(px(12.5))
            .text_color(Palette::muted())
            .when(!docked, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_color(Palette::text())
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(format!(
                            "{} \u{b7} {}",
                            tile.market.ticker(),
                            period.label()
                        )),
                )
            })
            .child(squeeze(div()).child(shown))
            .child(div().flex_1())
            .children(
                since_note.map(|note| squeeze(div()).id("trades-since").child(note).test_support()),
            );

        let message = |text: &'static str| {
            h_flex()
                .size_full()
                .justify_center()
                .text_color(Palette::faint())
                .child(text)
                .into_any_element()
        };
        let marks = |side: fn(&CandleFills) -> bool| grouped.iter().filter(|c| side(c)).count();
        let (state, body): (&'static str, AnyElement) = match candles {
            // The last attempt to load the candles failed. It is tried again.
            None if chart_failed => (
                "failed",
                v_flex()
                    .id("chart-failed")
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_color(Palette::amber_text())
                            .child("The chart's candles could not be loaded"),
                    )
                    .child(ui::label(
                        "Trying again in a few seconds. The figures on the left are live.",
                    ))
                    .test_support()
                    .into_any_element(),
            ),
            // Still loading: placeholders in the shape of candles, and what
            // is being waited for.
            None => (
                "loading",
                v_flex()
                    .id(if still {
                        "chart-loading-still"
                    } else {
                        "chart-loading"
                    })
                    .size_full()
                    .gap_2()
                    .child(ghost_candles(still))
                    .child(ui::label(format!(
                        "Loading {} {} candles\u{2026}",
                        tile.market.ticker(),
                        period.label()
                    )))
                    .test_support()
                    .into_any_element(),
            ),
            Some(candles) if candles.len() < 2 => {
                ("short", message("Not enough history for this period yet"))
            }
            Some(candles) => (
                "chart",
                candle_chart(
                    candles,
                    tile.live.as_ref().map(|l| l.last),
                    grouped,
                    levels,
                    calls,
                    entry,
                    &self.plot,
                ),
            ),
        };
        let drawn = state == "chart";
        self.drawn_chart.set((
            state,
            if drawn {
                marks(|c| c.bought.is_some())
            } else {
                0
            },
            if drawn {
                marks(|c| c.sold.is_some())
            } else {
                0
            },
            since,
        ));

        v_flex()
            .flex_1()
            .min_w_0()
            .w_full()
            .h_full()
            .px_4()
            .py_3()
            .gap_2()
            .child(heading)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(body),
            )
            .into_any_element()
    }
}

impl Render for ChartPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(market) = self.market(cx) else {
            self.drawn_chart.set(("none", 0, 0, None));
            return div().flex_1().into_any_element();
        };
        let (tile, portfolio) = {
            let m = self.model.read(cx);
            (m.tile_of(&market), m.portfolio.clone())
        };
        let tile = &tile;
        let (period, chart, chart_failed) = self.view(&market, cx);
        let candles = chart.as_ref().map(|chart| chart.candles.as_slice());
        // The account's fills in this market, grouped into these candles.
        let (grouped, since) = match (portfolio.read(cx).fills(), candles) {
            (Some(all), Some(candles)) if !candles.is_empty() => (
                fills::group(&all.rows, &market, candles, period.interval().millis()),
                fills::shown_since(all, candles[0].open_ms),
            ),
            _ => (Vec::new(), None),
        };
        // The watcher's armed levels in this market, and the menu that
        // adds one at the pointer.
        let calls = {
            let (levels, can_add) = self.model.read(cx).calls_in(&market, cx);
            let menu: Option<CallMenu> = can_add.then(|| {
                let plot = self.plot.clone();
                let markets = cx.entity().downgrade();
                let coin = market.coin().to_string();
                let last = tile.live.as_ref().map(|l| l.last);
                std::rc::Rc::new(
                    move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                        let Some(price) = plot.price() else {
                            return menu;
                        };
                        let (markets, coin) = (markets.clone(), coin.clone());
                        menu.item(
                            PopupMenuItem::new(format!("Alert at {}", f::price(price))).on_click(
                                move |_, _, cx| {
                                    let _ = markets.update(cx, |m, cx| {
                                        m.add_call(coin.clone(), price, last, cx)
                                    });
                                },
                            ),
                        )
                    },
                ) as CallMenu
            });
            Calls { levels, menu }
        };
        // The account's open orders in this market, as levels.
        let levels = portfolio
            .read(cx)
            .orders()
            .map(|all| order_levels(all, &market))
            .unwrap_or_default();
        // The account's entry, while it holds the market.
        let entry = portfolio.read(cx).positions().and_then(|all| {
            all.iter()
                .find(|p| p.dex == market.dex() && p.market == market.ticker() && !p.size.is_zero())
                .map(|p| {
                    let size = f::amount(p.size);
                    Entry {
                        price: p.entry,
                        size: if p.size > Decimal::ZERO {
                            format!("+{size}")
                        } else {
                            size
                        },
                    }
                })
        });
        let chart = self.chart(
            period,
            chart_failed,
            tile,
            candles,
            &grouped,
            &levels,
            calls,
            entry,
            since,
            cx.reduce_motion(),
        );
        // The pointer over the candles moves the time cursor here and on the
        // linked charts.
        div()
            .id("chart-panel")
            .size_full()
            .on_mouse_move(cx.listener(|this, e: &gpui_kit::MouseMoveEvent, _, cx| {
                this.pointer_moved(Some(e.position), cx)
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.pointer_moved(None, cx)
                }
            }))
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, e: &gpui_kit::MouseDownEvent, _, cx| {
                    this.price_clicked(e.position, cx)
                }),
            )
            .child(chart)
            .into_any_element()
    }
}

/// A period in a chart's period menu: its label and the candles it gives.
pub fn period_item(period: MarketPeriod) -> String {
    format!(
        "{} \u{b7} {}",
        period.label(),
        interval_words(period.interval())
    )
}
