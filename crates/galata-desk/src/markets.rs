//! The Markets section: the watched markets as a grid of heatmap tiles,
//! under a panel for the selected market: its detail, its chart, and the
//! account's position in it. Its only actions are the order ticket and the
//! position panel's close, take-profit / stop-loss and cancel, each opening
//! the Portfolio section's own confirmation.

use std::collections::HashMap;
use std::time::SystemTime;

use chrono::{DateTime, Local};
use galata_desk_core::format as f;
use galata_desk_core::market::candles::Candle;
use galata_desk_core::market::candles::Placement;
use galata_desk_core::market::feed::{Connection, Listed, Listing};
use galata_desk_core::market::fills::{CandleFills, FillSide, SideFills};
use galata_desk_core::market::sort::SortBy;
use galata_desk_core::market::watchlist::{LIMIT, Watchlist};
use galata_desk_core::market::{Interval, MarketId, MarketPeriod};
use galata_desk_core::model::OrderRow;
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::rust_decimal::prelude::ToPrimitive;
use galata_desk_core::trade::sign::Tpsl;
use galata_desk_core::venue::Side;
use gpui_kit::component::button::Button;
use gpui_kit::component::chart::CandlestickChart;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::plot::scale::{Scale, ScaleBand};
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, SharedString, Subscription, TestSupportExt, Window, deferred, div,
    px,
};

use crate::market_tile::{self, ColourBy};
use crate::portfolio::Portfolio;
use crate::theme::{MONO, Palette};
use crate::ui;

/// A tile is at least this wide; the grid fits as many columns as that allows.
const TILE_MIN: f32 = 230.;
/// Tall enough for the price, the range bar and the account's strip.
const TILE_HEIGHT: f32 = 156.;
/// The lighter grey of a tile's secondary figures. Readable on the strongest
/// tile colour (see `market_tile`'s contrast test).
const SOFT: u32 = 0xd8d3c8;
const GAP: f32 = 8.;
/// The market panel: a fixed height above the tiles, its detail column on
/// the left, the position column on the right and the chart taking the rest.
// Tall enough for the detail's name line, figures and the Buy and Sell
// buttons under them.
const PANEL_HEIGHT: f32 = 406.;
const DETAIL_WIDTH: f32 = 280.;
/// The chart's plot, and the column of price labels beside it.
const PLOT_HEIGHT: f32 = 266.;
const PRICE_AXIS_WIDTH: f32 = 74.;
/// The room one time label takes on the time axis, gap included.
const TIME_LABEL_WIDTH: f32 = 100.;
/// A chart narrower than this has no room for the price axis beside a plot
/// with two time labels under it: it draws the compact form, without the
/// price axis.
const COMPACT_CHART_WIDTH: f32 = PRICE_AXIS_WIDTH + 8. + 2. * TIME_LABEL_WIDTH;
/// The component kit draws a chart's highest value this far below its top.
const PLOT_TOP_INSET: f32 = 10.;

mod blotter;
mod chart;
mod detail;
pub mod dock;
mod model;
mod position;
mod ticket;
mod tiles;
mod workspace;

#[cfg(test)]
pub use blotter::{BlotterPanel, BlotterTab};
pub use chart::ChartPanel;
pub use detail::DetailPanel;
use model::Account;
pub use model::{MarketSource, MarketsModel};
pub use position::PositionPanel;
#[cfg(test)]
pub use ticket::TicketPanel;
pub use tiles::{Outline, TilesEvent, TilesHandler, TilesPanel};
#[cfg(test)]
use {
    crate::market_position, galata_desk_core::market::feed::MarketSnapshot, model::SETTLE,
    std::time::Instant,
};

pub struct Markets {
    /// Everything the panels share: the feed, the watchlist, the period,
    /// colour and sort, what each market is, and the watcher connection.
    model: Entity<MarketsModel>,
    /// The watched markets' tiles, under the market panel.
    tiles: Entity<TilesPanel>,
    /// The market panel: the selected market's detail on the left, its chart
    /// in the middle and the account's position in it on the right.
    detail: Entity<DetailPanel>,
    chart: Entity<ChartPanel>,
    position: Entity<PositionPanel>,
    picker_open: bool,
    search: Entity<InputState>,
    /// The fullscreen workspace, made the first time it is needed and kept.
    workspace: Option<workspace::Workspace>,
    /// Whether Markets was last drawn fullscreen, to notice the change.
    was_fullscreen: bool,
    /// Tests and screenshot runs say whether the window is fullscreen.
    #[cfg(any(test, feature = "shot"))]
    fullscreen: Option<bool>,
    _subscriptions: Vec<Subscription>,
}

impl Markets {
    pub fn new(portfolio: Entity<Portfolio>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = cx.new(|cx| MarketsModel::new(portfolio.clone(), cx));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search markets"));
        // A tile picked shows its market in the panel above.
        let markets = cx.entity().downgrade();
        let on_tiles: TilesHandler = std::rc::Rc::new(move |event, _, cx| {
            let _ = markets.update(cx, |this, cx| match event {
                TilesEvent::Selected(market) => this.select(market, cx),
                TilesEvent::AddMarket => {
                    if !this.picker_open {
                        this.toggle_picker(cx);
                    }
                }
            });
        });
        let tiles = cx.new(|cx| TilesPanel::new(model.clone(), Outline::Watchlist, on_tiles, cx));
        let detail = cx.new(|cx| DetailPanel::new(model.clone(), MarketSource::Watchlist, cx));
        let chart = cx.new(|cx| ChartPanel::new(model.clone(), MarketSource::Watchlist, cx));
        let position =
            cx.new(|cx| PositionPanel::new(model.clone(), MarketSource::Watchlist, window, cx));
        let subscriptions = vec![
            // Typing in the search field redraws the list.
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            // Anything the panels share redraws them.
            cx.observe(&model, |_, _, cx| cx.notify()),
            // Going fullscreen, or leaving it, changes the layout.
            cx.observe_window_bounds(window, |_, _, cx| cx.notify()),
        ];

        #[allow(unused_mut)]
        let mut this = Self {
            model,
            tiles,
            detail,
            chart,
            position,
            picker_open: false,
            search,
            workspace: None,
            was_fullscreen: false,
            #[cfg(any(test, feature = "shot"))]
            fullscreen: None,
            _subscriptions: subscriptions,
        };
        #[cfg(feature = "shot")]
        this.preselect(cx);
        this.model.update(cx, |m, _| m.start());
        this
    }

    /// Whether the window is fullscreen: then Markets is the workspace.
    fn is_fullscreen(&self, window: &Window) -> bool {
        #[cfg(any(test, feature = "shot"))]
        if let Some(fullscreen) = self.fullscreen {
            return fullscreen;
        }
        window.is_fullscreen()
    }

    /// The window went fullscreen: show the workspace, made now if this is
    /// the first time. A ticket panel in it takes the ticket again.
    fn enter_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.is_none() {
            self.workspace = Some(workspace::Workspace::new(&self.model, window, cx));
        }
        let docked = self.workspace.as_ref().is_some_and(|w| w.has_ticket(cx));
        let portfolio = self.model.read(cx).portfolio.clone();
        portfolio.update(cx, |p, cx| p.set_ticket_docked(docked, cx));
    }

    /// The window left fullscreen: the fixed layout shows channel A's market,
    /// and the ticket opens over the screen again.
    fn leave_workspace(&mut self, cx: &mut Context<Self>) {
        let channel_a = self.workspace.as_ref().and_then(|w| w.channels[0].clone());
        if let Some(market) = channel_a {
            self.select(&market, cx);
        }
        let portfolio = self.model.read(cx).portfolio.clone();
        portfolio.update(cx, |p, cx| p.set_ticket_docked(false, cx));
    }

    /// The workspace with every panel closed: what can fill it again.
    fn empty_workspace(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id("workspace-empty")
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_color(Palette::muted())
                    .child("The workspace is empty. Add a panel, or choose a layout."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(self.add_panel_menu("workspace-empty-add", cx))
                    .child(
                        Button::new(("preset", 0usize))
                            .small()
                            .outline()
                            .label("Trader")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.apply_trader(window, cx)),
                            ),
                    ),
            )
            .test_support()
            .into_any_element()
    }

    /// Whether the workspace is what Markets shows now.
    fn workspace_shown(&self) -> bool {
        self.was_fullscreen && self.workspace.is_some()
    }

    /// Add panel: every kind of panel, the order ticket only while there is
    /// none.
    fn add_panel_menu(&self, id: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let ticket_docked = self.model.read(cx).portfolio.read(cx).ticket_is_docked();
        let markets = cx.entity().downgrade();
        Button::new(id)
            .small()
            .outline()
            .label("Add panel \u{25be}")
            .dropdown_menu(move |mut menu, _, _| {
                for (kind, name) in workspace::CATALOGUE {
                    let markets = markets.clone();
                    let added = kind == dock::PanelKind::Ticket && ticket_docked;
                    let name = if added {
                        format!("{name} (added)")
                    } else {
                        name.to_string()
                    };
                    menu = menu.item(PopupMenuItem::new(name).disabled(added).on_click(
                        move |_, window, cx| {
                            let _ = markets.update(cx, |m, cx| m.add_panel(kind, window, cx));
                        },
                    ));
                }
                menu
            })
            .into_any_element()
    }

    /// Add a panel of `kind` to the workspace. Never a second order ticket.
    pub fn add_panel(
        &mut self,
        kind: dock::PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let portfolio = self.model.read(cx).portfolio.clone();
        if kind == dock::PanelKind::Ticket && portfolio.read(cx).ticket_is_docked() {
            return;
        }
        workspace.add(&self.model, kind, window, cx);
        cx.notify();
    }

    /// Lay the workspace out as Trader.
    fn apply_trader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let second = {
            let m = self.model.read(cx);
            m.watchlist
                .as_ref()
                .and_then(|w| {
                    w.markets()
                        .iter()
                        .find(|x| Some(*x) != workspace.channels[0].as_ref())
                })
                .cloned()
        };
        workspace.apply_trader(&self.model, second, window, cx);
        cx.notify();
    }

    /// The History and Funding views, for the workspace's blotter.
    pub fn set_sources(
        &mut self,
        history: Entity<crate::history::History>,
        funding: Entity<crate::funding::Funding>,
        cx: &mut Context<Self>,
    ) {
        self.model.update(cx, |m, _| {
            m.history = Some(history);
            m.funding = Some(funding);
        });
    }

    /// Use this connection to the watcher, or none. The window calls this
    /// whenever its connection changes.
    pub fn set_watcher(
        &mut self,
        link: Option<Entity<crate::watcher_link::WatcherLink>>,
        cx: &mut Context<Self>,
    ) {
        self.model.update(cx, |m, cx| m.set_watcher(link, cx));
    }

    /// `GALATA_DESK_SHOT_STATE=section=markets,mperiod=week,colour=funding,select=xyz:NVDA,picker=1,fullscreen=1`
    #[cfg(feature = "shot")]
    fn preselect(&mut self, cx: &mut Context<Self>) {
        let state = std::env::var("GALATA_DESK_SHOT_STATE").unwrap_or_default();
        for (key, value) in state.split(',').filter_map(|kv| kv.split_once('=')) {
            if key == "picker" {
                self.picker_open = true;
                continue;
            }
            if key == "fullscreen" {
                self.fullscreen = Some(value == "1");
                continue;
            }
            self.model.update(cx, |m, _| match key {
                "about" => m.about.open_at_start = value == "1",
                "mperiod" => {
                    let period = match value {
                        "3m" => MarketPeriod::Min3,
                        "5m" => MarketPeriod::Min5,
                        "15m" => MarketPeriod::Min15,
                        "30m" => MarketPeriod::Min30,
                        "hour" => MarketPeriod::Hour,
                        "week" => MarketPeriod::Week,
                        _ => MarketPeriod::Day,
                    };
                    m.period = period;
                    m.feed.set_period(period);
                }
                "sort" => {
                    m.sort = match value {
                        "gainers" => SortBy::Gainers,
                        "losers" => SortBy::Losers,
                        "volatility" => SortBy::Volatility,
                        "funding" => SortBy::Funding,
                        "name" => SortBy::Name,
                        _ => SortBy::Watchlist,
                    };
                }
                "colour" => {
                    m.colour = match value {
                        "volatility" => ColourBy::Volatility,
                        "funding" => ColourBy::Funding,
                        _ => ColourBy::Change,
                    }
                }
                "select" => {
                    if let Some(list) = &mut m.watchlist {
                        list.select(&MarketId::new(value));
                    }
                }
                _ => {}
            });
        }
    }

    /// The market the panel shows.
    fn selected(&self, cx: &gpui_kit::App) -> Option<MarketId> {
        self.model.read(cx).selected().cloned()
    }

    /// Show a market in the panel. Only a click, or Enter or Space on a
    /// focused tile, comes here; pointing at a tile does not.
    fn select(&mut self, market: &MarketId, cx: &mut Context<Self>) {
        if self.model.update(cx, |m, cx| m.select(market, cx)) {
            // The alerts menu is the last market's.
            self.position.update(cx, |p, cx| p.market_changed(cx));
        }
        cx.notify();
    }

    fn add(&mut self, market: MarketId, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| m.add(market, cx));
    }

    fn remove(&mut self, market: &MarketId, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| m.remove(market, cx));
    }

    fn toggle_picker(&mut self, cx: &mut Context<Self>) {
        self.picker_open = !self.picker_open;
        if self.picker_open {
            // The list may have failed to load, or grown old.
            self.model.update(cx, |m, cx| m.refresh_listing(cx));
        }
        cx.notify();
    }

    /// The account's positions and per-market PNL, for the tiles.
    fn account(&self, cx: &gpui_kit::App) -> Account {
        self.model.read(cx).account(cx)
    }

    /// The Markets section's part of the title bar: whether it is live.
    pub fn status(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.read(cx);
        let (text, colour) = match model.snapshot.connection {
            Connection::Live => ("Live".to_string(), Palette::gain()),
            Connection::Down { .. } => ("Not live".to_string(), Palette::amber()),
            Connection::Connecting if model.unreachable() => {
                ("Not connected".to_string(), Palette::amber())
            }
            Connection::Connecting => ("Connecting\u{2026}".to_string(), Palette::faint()),
        };
        h_flex()
            .gap_2()
            .child(div().size(px(7.)).rounded_full().bg(colour))
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .text_color(Palette::muted())
                    .child(text),
            )
            .into_any_element()
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let (count, chosen, sorted, colour_now) = {
            let m = self.model.read(cx);
            (
                m.watchlist.as_ref().map(|w| w.markets().len()),
                // One period for the whole section: the tiles' figures and the chart.
                m.period,
                m.sort,
                m.colour,
            )
        };
        let view = self.model.downgrade();
        let period = Button::new("mperiod")
            .small()
            .outline()
            .label(format!("{} \u{25be}", chosen.label()))
            .dropdown_menu(move |mut menu, _, _| {
                for choice in MarketPeriod::ALL {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(period_words(choice))
                            .checked(choice == chosen)
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |m, cx| m.set_period(choice, cx));
                            }),
                    );
                }
                menu
            });
        let view = self.model.downgrade();
        let sort_menu = Button::new("msort")
            .small()
            .outline()
            .label(format!("{} \u{25be}", sorted.label()))
            .dropdown_menu(move |mut menu, _, _| {
                for choice in SortBy::ALL {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(choice.words())
                            .checked(choice == sorted)
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |m, cx| m.set_sort(choice, cx));
                            }),
                    );
                }
                menu
            });
        let colour = TabBar::new("colour")
            .segmented()
            .xsmall()
            .selected_index(
                ColourBy::ALL
                    .iter()
                    .position(|c| *c == colour_now)
                    .unwrap_or(0),
            )
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.model
                    .update(cx, |m, cx| m.set_colour(ColourBy::ALL[*ix], cx));
            }))
            .children(ColourBy::ALL.map(ColourBy::label));

        h_flex()
            .flex_wrap()
            .gap_x_6()
            .gap_y_2()
            .items_center()
            .child(
                h_flex()
                    .gap_3()
                    .items_baseline()
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Markets"),
                    )
                    .children(count.map(|n| ui::label(format!("{n} watched")).text_size(px(13.)))),
            )
            .child(div().flex_1())
            .children(
                self.workspace_shown()
                    .then(|| self.add_panel_menu("workspace-add", cx)),
            )
            .child(h_flex().gap_2().child(ui::label("Period")).child(period))
            .child(h_flex().gap_2().child(ui::label("Sort")).child(sort_menu))
            .child(h_flex().gap_2().child(ui::label("Colour by")).child(colour))
            .child(
                div()
                    .relative()
                    .child(
                        Button::new("add-market")
                            .small()
                            .label("+ Add market")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(cx))),
                    )
                    .when(self.picker_open, |d| {
                        d.child(deferred(self.picker(cx)).with_priority(2))
                    }),
            )
            .into_any_element()
    }

    fn legend(&self, account: &Account, cx: &gpui_kit::App) -> AnyElement {
        let (colour, period) = {
            let m = self.model.read(cx);
            (m.colour, m.period)
        };
        let (low, steps, high) = market_tile::legend(colour, period);
        h_flex()
            .flex_wrap()
            .gap_x_5()
            .gap_y_1()
            .text_size(px(12.))
            .text_color(Palette::muted())
            .child(
                h_flex()
                    .gap_2()
                    .child(low)
                    .child(
                        h_flex().gap(px(2.)).children(
                            steps
                                .into_iter()
                                .map(|c| div().w(px(26.)).h(px(10.)).rounded(px(2.)).bg(c)),
                        ),
                    )
                    .child(high),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().size(px(7.)).rounded_full().bg(Palette::amber()))
                    .child("You hold a position"),
            )
            .children(account.reaches_from.map(|from| {
                legend_line(
                    "pnl-since",
                    format!("* PNL counts trades since {}", day(from)),
                    Palette::amber_text(),
                )
            }))
            .into_any_element()
    }

    /// The add-market list, opened from the add button.
    fn picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let (full, listing, watched_now) = {
            let m = self.model.read(cx);
            (
                m.watchlist.as_ref().is_some_and(Watchlist::is_full),
                m.snapshot.listing.clone(),
                m.watchlist.clone(),
            )
        };
        let query = self.search.read(cx).value().trim().to_lowercase();

        let body: AnyElement = match &listing {
            Listing::Loading => div()
                .p_4()
                .text_color(Palette::muted())
                .child("Loading the list of markets\u{2026}")
                .into_any_element(),
            Listing::Failed => v_flex()
                .p_4()
                .gap_3()
                .child(
                    div()
                        .text_color(Palette::amber_text())
                        .child("The list of markets could not be loaded."),
                )
                .child(
                    h_flex().child(
                        Button::new("retry-listing")
                            .small()
                            .label("Retry")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.update(cx, |m, cx| m.refresh_listing(cx));
                            })),
                    ),
                )
                .into_any_element(),
            Listing::Ready(listed) => {
                let matches: Vec<(usize, &Listed)> = listed
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.market.coin().to_lowercase().contains(&query))
                    .collect();
                if matches.is_empty() {
                    div()
                        .p_4()
                        .text_color(Palette::muted())
                        .child("No market matches.")
                        .into_any_element()
                } else {
                    let mut rows: Vec<AnyElement> = Vec::with_capacity(matches.len() + 12);
                    let mut dex: Option<&str> = None;
                    for (ix, listed) in matches {
                        // A heading where each dex begins.
                        if dex != Some(listed.market.dex()) {
                            dex = Some(listed.market.dex());
                            let name = if listed.market.dex().is_empty() {
                                "Main dex".to_string()
                            } else {
                                listed.market.dex().to_string()
                            };
                            rows.push(
                                div()
                                    .px_3()
                                    .pt_2()
                                    .pb_1()
                                    .text_size(px(11.))
                                    .text_color(Palette::faint())
                                    .child(name)
                                    .into_any_element(),
                            );
                        }
                        let market = listed.market.clone();
                        let watched = watched_now.as_ref().is_some_and(|w| w.contains(&market));
                        let blocked = watched || full;
                        rows.push(
                            h_flex()
                                .id(("pick", ix))
                                .px_3()
                                .h(px(30.))
                                .gap_2()
                                .text_size(px(13.))
                                .when(!blocked, |d| {
                                    d.cursor_pointer()
                                        .hover(|s| s.bg(Palette::raise()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.add(market.clone(), cx);
                                        }))
                                })
                                .when(blocked, |d| d.text_color(Palette::faint()))
                                .child(
                                    div()
                                        .font_family(MONO)
                                        .child(listed.market.ticker().to_string()),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(Palette::faint())
                                        .child(if watched {
                                            "Watched".to_string()
                                        } else {
                                            format!("up to {}x", listed.max_leverage)
                                        }),
                                )
                                .test_support()
                                .into_any_element(),
                        );
                    }
                    v_flex()
                        .id("pick-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .pb_2()
                        .children(rows)
                        .into_any_element()
                }
            }
        };

        ui::panel()
            .id("picker")
            .occlude()
            .absolute()
            .top(px(36.))
            .right_0()
            .w(px(320.))
            .h(px(440.))
            .border_color(Palette::amber_line())
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.picker_open = false;
                cx.notify();
            }))
            .child(
                div()
                    .p_2()
                    .child(Input::new(&self.search).small().cleanable(true)),
            )
            .when(full, |d| {
                d.child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_size(px(12.5))
                        .text_color(Palette::amber_text())
                        .child(format!(
                            "The watchlist is full ({LIMIT} markets). Remove one to add another."
                        )),
                )
            })
            .child(body)
            .into_any_element()
    }

    /// The market panel: the selected market's detail on the left, its chart
    /// on the right. `None` when nothing is watched.
    fn panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let market = self.selected(cx)?;
        let tile = self.model.read(cx).tile_of(&market);
        let tile = &tile;
        let content: AnyElement = if tile.unavailable {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(market.to_string()),
                )
                .child(
                    div()
                        .text_color(Palette::muted())
                        .child("Unavailable: the venue no longer lists this market."),
                )
                .child(
                    Button::new("panel-remove")
                        .small()
                        .label("Remove")
                        .on_click(cx.listener(move |this, _, _, cx| this.remove(&market, cx))),
                )
                .into_any_element()
        } else {
            h_flex()
                .size_full()
                .child(self.detail.clone())
                .child(self.chart.clone())
                .child(self.position.clone())
                .into_any_element()
        };

        Some(
            ui::panel()
                .id("market-panel")
                .h(px(PANEL_HEIGHT))
                .flex_shrink_0()
                .child(content)
                .test_support()
                .into_any_element(),
        )
    }

    fn notices(&self, cx: &gpui_kit::App) -> Vec<AnyElement> {
        let model = self.model.read(cx);
        let mut out = Vec::new();
        if let Some(warning) = &model.warning {
            out.push(ui::notice(warning.clone()).into_any_element());
        }
        match model.snapshot.connection {
            Connection::Down { since } => out.push(
                ui::notice(format!(
                    "Not live since {}. The figures below are the last received; reconnecting.",
                    clock(since)
                ))
                .into_any_element(),
            ),
            Connection::Connecting if model.unreachable() => out.push(
                ui::notice("Hyperliquid's market feed cannot be reached. The app keeps trying.")
                    .into_any_element(),
            ),
            _ => {}
        }
        out
    }
}

/// The small tag naming a builder dex.
/// A position-panel section's name: small, spaced capitals.
fn section_title(title: &'static str) -> gpui_kit::Div {
    div()
        .text_size(px(11.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(Palette::muted())
        .child(title.to_uppercase())
}

fn dex_tag(dex: &str) -> gpui_kit::Div {
    div()
        .px(px(5.))
        .rounded(px(4.))
        .bg(Palette::ground().opacity(0.45))
        .font_family(MONO)
        .text_size(px(10.5))
        .child(dex.to_string())
}

impl Render for Markets {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Only a screenshot run needs the window here.
        #[cfg(not(feature = "shot"))]
        let _ = window;
        self.model.update(cx, |m, cx| m.want_about(cx));
        // A screenshot run's order ticket opens once the selected market has
        // a price.
        #[cfg(feature = "shot")]
        if let Some(market) = self.selected(cx)
            && let Some(mid) = self.model.read(cx).last_price(&market)
        {
            self.model
                .read(cx)
                .portfolio
                .clone()
                .update(cx, |p, cx| p.stage_ticket_shot(market, mid, window, cx));
        }
        let fullscreen = self.is_fullscreen(window);
        if fullscreen != self.was_fullscreen {
            self.was_fullscreen = fullscreen;
            if fullscreen {
                self.enter_workspace(window, cx);
            } else {
                self.leave_workspace(cx);
            }
        }
        let padding = 12.;
        let page = v_flex()
            .size_full()
            .p(px(padding))
            .gap_3()
            .child(self.toolbar(cx))
            .child(self.legend(&self.account(cx), cx))
            .children(self.notices(cx));
        match self.workspace.as_ref().filter(|_| fullscreen) {
            Some(workspace) if workspace.is_empty(cx) => page
                .id("markets-workspace")
                .child(self.empty_workspace(cx))
                .test_support(),
            Some(workspace) => page
                .id("markets-workspace")
                .child(div().flex_1().min_h_0().child(workspace.area.clone()))
                .test_support(),
            None => page
                .id("markets-fixed")
                // The selected market, above the tiles and always in view.
                .children(self.panel(cx))
                .child(div().flex_1().min_h_0().child(self.tiles.clone()))
                .test_support(),
        }
    }
}

const DASH: &str = "\u{2014}";

/// The tile's range bar: the period's low to high, the usual band, a tick at
/// the last price, and how far that is from the average.
fn range_bar(ix: usize, at: &Placement) -> AnyElement {
    let stretched = at.stretched();
    let soft = gpui_kit::rgb(SOFT);
    let tick_height = if stretched { 12. } else { 10. };
    h_flex()
        .items_center()
        .gap_2()
        .h(px(12.))
        .child(
            div()
                .id(("tile-bar", ix))
                .relative()
                .flex_1()
                .h(px(4.))
                .rounded(px(2.))
                .bg(Palette::ground().opacity(0.5))
                .children(at.band.map(|(from, to)| {
                    div()
                        .absolute()
                        .top_0()
                        .left(gpui_kit::relative(from as f32))
                        .w(gpui_kit::relative((to - from) as f32))
                        .h(px(4.))
                        .rounded(px(2.))
                        .bg(Palette::text().opacity(0.24))
                }))
                .child(
                    div()
                        .absolute()
                        .top(px((4. - tick_height) / 2.))
                        .left(gpui_kit::relative(at.tick as f32))
                        .ml(px(-1.))
                        .w(px(2.))
                        .h(px(tick_height))
                        .rounded(px(1.))
                        .bg(Palette::text()),
                )
                .test_support(),
        )
        .children(at.z.map(|z| {
            div()
                .id(("tile-sigma", ix))
                .aria_label(f::sigma(z))
                .min_w(px(40.))
                .text_right()
                .font_family(MONO)
                .text_size(px(10.5))
                .when(stretched, |d| {
                    d.font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(Palette::text())
                })
                .when(!stretched, |d| d.text_color(soft))
                .child(f::sigma(z))
                .test_support()
        }))
        .into_any_element()
}

/// One line of the legend, named for tests and screen readers.
fn legend_line(
    id: &'static str,
    text: impl Into<SharedString>,
    colour: gpui_kit::Hsla,
) -> AnyElement {
    let text = text.into();
    div()
        .id(id)
        .aria_label(text.clone())
        .text_color(colour)
        .child(text)
        .test_support()
        .into_any_element()
}

/// A day on this machine's calendar: `2 Feb 2026`.
fn day(ms: u64) -> String {
    let at = DateTime::from_timestamp_millis(ms as i64)
        .map(|t| t.with_timezone(&Local))
        .unwrap_or_default();
    at.format("%-d %b %Y").to_string()
}

fn sign(value: Decimal) -> i32 {
    if value > Decimal::ZERO {
        1
    } else if value < Decimal::ZERO {
        -1
    } else {
        0
    }
}

/// A time of day on this machine's clock: `04:21:07`.
fn clock(at: SystemTime) -> String {
    DateTime::<Local>::from(at).format("%H:%M:%S").to_string()
}

/// How strong an order's line is where it meets the price axis.
const ORDER_LINE_STRENGTH: f32 = 0.4;
/// An order's price tag on the axis, in pixels.
const ORDER_TAG_HEIGHT: f32 = 18.;

/// An open order's price on the chart.
#[derive(Clone, Debug, PartialEq)]
struct OrderLevel {
    price: Decimal,
    buy: bool,
    /// Named on its line: a take-profit or stop-loss.
    mark: Option<&'static str>,
    /// Its tag's tooltip: the order in full.
    detail: String,
}

impl OrderLevel {
    fn colour(&self) -> gpui_kit::Hsla {
        if self.buy {
            Palette::gain()
        } else {
            Palette::loss()
        }
    }
}

/// An order's tag on the price axis, or the tag for those beyond an edge.
struct OrderTag {
    id: gpui_kit::ElementId,
    text: String,
    /// What a screen reader says for it.
    spoken: String,
    /// Its tooltip.
    detail: String,
    colour: gpui_kit::Hsla,
    /// Where it belongs: its price's height on the plot.
    centre: f32,
}

/// Move tags, given by their tops in order down the plot, apart so none
/// overlaps the next, keeping them within `0..=max`.
fn spread(tops: &mut [f32], pitch: f32, max: f32) {
    for t in tops.iter_mut() {
        *t = t.clamp(0., max);
    }
    for i in 1..tops.len() {
        tops[i] = tops[i].max(tops[i - 1] + pitch);
    }
    if let Some(last) = tops.last_mut() {
        *last = last.min(max);
    }
    for i in (0..tops.len().saturating_sub(1)).rev() {
        tops[i] = tops[i].min(tops[i + 1] - pitch);
    }
    for t in tops.iter_mut() {
        *t = t.max(0.);
    }
}

/// The account's open orders in `market`, as levels on its chart. A trigger
/// order sits at its trigger price.
fn order_levels(orders: &[OrderRow], market: &MarketId) -> Vec<OrderLevel> {
    orders
        .iter()
        .filter(|o| o.market == market.ticker() && o.dex == market.dex())
        .map(|o| {
            let buy = matches!(o.side, Side::Buy);
            let side = if buy { "Buy" } else { "Sell" };
            let price = o.trigger_px.unwrap_or(o.price);
            let sized = !o.remaining.is_zero();
            let mark = o.tpsl.map(|t| match t {
                Tpsl::Tp => "TP",
                Tpsl::Sl => "SL",
            });
            let size = if sized {
                f::size(o.remaining, &o.market)
            } else {
                "the position".to_string()
            };
            OrderLevel {
                price,
                buy,
                mark,
                detail: format!("{} \u{b7} {side} {size} at {}", o.kind, f::price(price)),
            }
        })
        .collect()
}

/// One candle as the chart draws and reads it.
#[derive(Clone)]
struct Bar {
    /// The chart places bars by label, so each needs its own: the open time.
    key: SharedString,
    time: SharedString,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    /// What the account bought and sold in this candle.
    fills: CandleFills,
}

/// The component kit's candlestick chart lays its candles out with these
/// band settings. The fill marks use the same, so they sit on their candles.
const BAND_PADDING_INNER: f32 = 0.4;
const BAND_PADDING_OUTER: f32 = 0.2;
/// A fill mark's side, in pixels.
const MARK_SIZE: f32 = 16.;

/// A `B` or `S` on the chart: a small bordered chip, centred on its candle.
fn fill_mark(
    id: (&'static str, usize),
    letter: &'static str,
    colour: gpui_kit::Hsla,
    x: f32,
    top: f32,
) -> AnyElement {
    div()
        .id(id)
        .absolute()
        .left(gpui_kit::relative(x))
        .top(px(top))
        .ml(px(-MARK_SIZE / 2.))
        .size(px(MARK_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .border_color(colour.opacity(0.7))
        .bg(Palette::panel())
        .text_color(colour)
        .text_size(px(10.5))
        .font_weight(gpui_kit::FontWeight::BOLD)
        .child(letter)
        .test_support()
        .into_any_element()
}

/// One side of a candle's fills, for the readout: `Sold 8 at 103 (2 fills)`
/// and what it realised.
fn fills_line(side: FillSide, fills: &SideFills, like: Decimal) -> gpui_kit::Div {
    let (word, colour) = match side {
        FillSide::Bought => ("Bought", Palette::gain()),
        FillSide::Sold => ("Sold", Palette::loss()),
    };
    let count = if fills.count == 1 {
        "1 fill".to_string()
    } else {
        format!("{} fills", fills.count)
    };
    v_flex()
        .mt(px(3.))
        .child(div().text_color(colour).child(format!(
            "{word} {} at {} ({count})",
            f::amount(fills.size),
            f::price_like(fills.average, like),
        )))
        .when(!fills.realised.is_zero(), |d| {
            d.child(
                div()
                    .font_family(MONO)
                    .text_color(ui::tone_of(sign(fills.realised)))
                    .child(f::usd_signed(fills.realised)),
            )
        })
}

/// Placeholders in the shape of candles, while the real ones load. The
/// heights are a fixed pattern, so they do not jump from frame to frame.
fn ghost_candles(still: bool) -> AnyElement {
    const PATTERN: [(f32, f32); 12] = [
        (0.30, 0.34),
        (0.38, 0.22),
        (0.26, 0.30),
        (0.44, 0.26),
        (0.52, 0.18),
        (0.40, 0.32),
        (0.34, 0.24),
        (0.48, 0.28),
        (0.56, 0.20),
        (0.46, 0.30),
        (0.36, 0.26),
        (0.42, 0.22),
    ];
    h_flex()
        .flex_1()
        .min_h_0()
        .h(px(PLOT_HEIGHT))
        .items_start()
        .justify_between()
        .px_2()
        .children((0..36).map(|i| {
            let (top, height) = PATTERN[i % PATTERN.len()];
            v_flex()
                .h_full()
                .child(div().h(gpui_kit::relative(top)))
                // The kit's placeholder pulses whatever the system's motion
                // setting; when reduced motion is asked for, a plain block of
                // the same shape stands in for it.
                .child(if still {
                    div()
                        .w(px(8.))
                        .h(gpui_kit::relative(height))
                        .rounded(px(2.))
                        .bg(Palette::text().opacity(0.08))
                        .into_any_element()
                } else {
                    Skeleton::new()
                        .w(px(8.))
                        .h(gpui_kit::relative(height))
                        .rounded(px(2.))
                        .into_any_element()
                })
        }))
        .into_any_element()
}

/// A period as the menu names it.
fn period_words(period: MarketPeriod) -> &'static str {
    match period {
        MarketPeriod::Min3 => "3 minutes",
        MarketPeriod::Min5 => "5 minutes",
        MarketPeriod::Min15 => "15 minutes",
        MarketPeriod::Min30 => "30 minutes",
        MarketPeriod::Hour => "1 hour",
        MarketPeriod::Day => "24 hours",
        MarketPeriod::Week => "7 days",
    }
}

/// "15-minute candles".
fn interval_words(interval: Interval) -> &'static str {
    match interval {
        Interval::M1 => "1-minute candles",
        Interval::M15 => "15-minute candles",
        Interval::H2 => "2-hour candles",
    }
}

/// The time axis's label for a candle. `span_ms` is how much time the whole
/// chart covers, which decides whether the date or the time matters.
fn time_label(open_ms: u64, span_ms: u64) -> String {
    const HOUR: u64 = 3_600_000;
    let at = DateTime::from_timestamp_millis(open_ms as i64)
        .unwrap_or_default()
        .with_timezone(&Local);
    if span_ms <= 12 * HOUR {
        at.format("%H:%M").to_string()
    } else if span_ms <= 72 * HOUR {
        at.format("%b %-d %H:%M").to_string()
    } else {
        at.format("%b %-d").to_string()
    }
}

/// A time axis label for a narrow chart: the time, or over more than three
/// days the date.
fn short_time_label(open_ms: u64, span_ms: u64) -> String {
    let at = DateTime::from_timestamp_millis(open_ms as i64)
        .unwrap_or_default()
        .with_timezone(&Local);
    if span_ms <= 72 * 3_600_000 {
        at.format("%H:%M").to_string()
    } else {
        at.format("%b %-d").to_string()
    }
}

/// Where a price sits down the plot, in pixels from its top.
///
/// The component kit's candlestick chart, with its own time axis off, puts
/// the lowest value at the bottom edge and the highest [`PLOT_TOP_INSET`]
/// below the top. The price labels and the last-price line are placed by the
/// same rule so they sit on the candles.
/// The menu that adds a call level at the pointer.
type CallMenu = std::rc::Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// A watcher call level, as the chart draws it.
#[derive(Clone, Debug, PartialEq)]
struct CallLevel {
    price: Decimal,
    note: Option<String>,
}

/// What the chart shows of the watcher.
struct Calls {
    levels: Vec<CallLevel>,
    /// Offered only while a level can be added.
    menu: Option<CallMenu>,
}

/// Where the pointer is over the plot, and the plot's price range, kept as
/// the chart draws and the pointer moves, for "Alert at <price>".
type TileBounds =
    std::rc::Rc<std::cell::RefCell<HashMap<MarketId, gpui_kit::Bounds<gpui_kit::Pixels>>>>;

#[derive(Clone, Default)]
struct PlotPointer {
    /// From the plot's top.
    y: std::rc::Rc<std::cell::Cell<Option<f32>>>,
    bounds: std::rc::Rc<std::cell::Cell<Option<gpui_kit::Bounds<gpui_kit::Pixels>>>>,
    /// Low, high, and a price at the venue's precision.
    range: std::rc::Rc<std::cell::Cell<Option<(f64, f64, Decimal)>>>,
    /// How wide the whole chart was last drawn, which decides its compact
    /// form.
    width: std::rc::Rc<std::cell::Cell<Option<gpui_kit::Pixels>>>,
}

impl PlotPointer {
    fn price(&self) -> Option<Decimal> {
        let (low, high, like) = self.range.get()?;
        Some(round_price(price_at(self.y.get()?, low, high), like))
    }

    fn track(&self, position: gpui_kit::Point<gpui_kit::Pixels>) {
        if let Some(b) = self.bounds.get() {
            self.y.set(Some((position.y - b.origin.y) / px(1.)));
        }
    }
}

/// The plot, recording where the pointer is, with the call level menu when
/// there is one.
fn plot_area(
    plot: gpui_kit::Stateful<gpui_kit::Div>,
    pointer: &PlotPointer,
    menu: Option<CallMenu>,
) -> AnyElement {
    let (on_move, on_down, bounds) = (pointer.clone(), pointer.clone(), pointer.bounds.clone());
    let plot = plot
        .child(
            gpui_kit::canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
        .on_mouse_move(move |e: &gpui_kit::MouseMoveEvent, _, _| on_move.track(e.position))
        .on_mouse_down(
            gpui_kit::MouseButton::Right,
            move |e: &gpui_kit::MouseDownEvent, _, _| on_down.track(e.position),
        )
        .test_support();
    match menu {
        Some(menu) => plot
            .context_menu(move |m, window, cx| menu(m, window, cx))
            .into_any_element(),
        None => plot.into_any_element(),
    }
}

/// The price at `y` from the plot's top: the inverse of [`plot_y`].
fn price_at(y: f32, low: f64, high: f64) -> f64 {
    let ratio = ((PLOT_HEIGHT - y) / (PLOT_HEIGHT - PLOT_TOP_INSET)).clamp(0., 1.) as f64;
    low + ratio * (high - low)
}

/// `value` at the venue's price precision: five significant figures, and no
/// more decimals than `like`, a price the venue quoted for the market.
fn round_price(value: f64, like: Decimal) -> Decimal {
    let d = Decimal::try_from(value).unwrap_or_default();
    d.round_sf(5)
        .unwrap_or(d)
        .round_dp(like.scale())
        .normalize()
}

/// The watcher's armed levels, as dashed amber lines tagged "Alert", and a
/// tag at the edge for those above or below the candles.
fn call_lines(calls: &[CallLevel], low: f64, high: f64, like: Decimal) -> Vec<AnyElement> {
    let f64_of = |d: Decimal| d.to_f64().unwrap_or_default();
    let tag = |text: String| {
        div()
            .absolute()
            .left(px(6.))
            .px(px(5.))
            .rounded(px(4.))
            .bg(Palette::amber().opacity(0.14))
            .border_1()
            .border_color(Palette::amber().opacity(0.4))
            .text_size(px(10.5))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(Palette::amber())
            .whitespace_nowrap()
            .child(text)
    };
    let label = |c: &CallLevel| match &c.note {
        Some(note) => format!("Alert {} \u{b7} {note}", f::price_like(c.price, like)),
        None => format!("Alert {}", f::price_like(c.price, like)),
    };
    let mut out = Vec::new();
    let (mut above, mut below) = (Vec::new(), Vec::new());
    for (i, c) in calls.iter().enumerate() {
        let v = f64_of(c.price);
        if v > high {
            above.push(c);
        } else if v < low {
            below.push(c);
        } else {
            let y = plot_y(v, low, high);
            out.push(
                div()
                    .id(("call-line", i))
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(y))
                    .h(px(1.))
                    .overflow_hidden()
                    .flex()
                    .gap(px(4.))
                    .children((0..240).map(|_| {
                        div()
                            .flex_shrink_0()
                            .w(px(6.))
                            .h(px(1.))
                            .bg(Palette::amber().opacity(0.85))
                    }))
                    .child(tag(label(c)).bottom(px(2.)))
                    .test_support()
                    .into_any_element(),
            );
        }
    }
    let edge = |side: &[&CallLevel], up: bool| -> Option<AnyElement> {
        let nearest = if up {
            side.iter().min_by_key(|c| c.price)
        } else {
            side.iter().max_by_key(|c| c.price)
        }?;
        let text = if side.len() == 1 {
            format!(
                "{} {}",
                if up { "\u{25b2}" } else { "\u{25bc}" },
                label(nearest)
            )
        } else {
            format!(
                "{} {} alerts",
                if up { "\u{25b2}" } else { "\u{25bc}" },
                side.len()
            )
        };
        let t = tag(text).id(if up { "call-above" } else { "call-below" });
        let t = if up { t.top(px(2.)) } else { t.bottom(px(2.)) };
        Some(t.test_support().into_any_element())
    };
    out.extend(edge(&above, true));
    out.extend(edge(&below, false));
    out
}

/// The account's position in the charted market: where it was entered, and
/// its signed size.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    price: Decimal,
    size: String,
}

/// A dashed line at the position's entry price, tagged on the right. It is
/// drawn only within the candles' range: it never widens the price axis.
fn entry_line(entry: Option<&Entry>, low: f64, high: f64, like: Decimal) -> Option<AnyElement> {
    let entry = entry?;
    let v = entry.price.to_f64().unwrap_or_default();
    if v < low || v > high {
        return None;
    }
    let tone = Palette::text().opacity(0.62);
    let text = format!(
        "Entry {} \u{b7} {}",
        f::price_like(entry.price, like),
        entry.size
    );
    Some(
        div()
            .id("entry-line")
            .absolute()
            .left_0()
            .right_0()
            .top(px(plot_y(v, low, high)))
            .h(px(1.))
            .flex()
            .gap(px(3.))
            .overflow_hidden()
            .aria_label(text.clone())
            .children((0..320).map(move |_| div().flex_shrink_0().w(px(3.)).h(px(1.)).bg(tone)))
            .child(
                div()
                    .absolute()
                    .right(px(6.))
                    .bottom(px(2.))
                    .px(px(5.))
                    .rounded(px(4.))
                    .bg(Palette::panel().opacity(0.9))
                    .border_1()
                    .border_color(tone)
                    .text_size(px(10.5))
                    .font_family(MONO)
                    .text_color(Palette::text())
                    .whitespace_nowrap()
                    .child(text),
            )
            .test_support()
            .into_any_element(),
    )
}

fn plot_y(value: f64, low: f64, high: f64) -> f32 {
    if high <= low {
        return PLOT_HEIGHT / 2.;
    }
    let ratio = ((value - low) / (high - low)).clamp(0., 1.) as f32;
    PLOT_HEIGHT - ratio * (PLOT_HEIGHT - PLOT_TOP_INSET)
}

/// The candlestick chart with what the kit does not draw: a price axis, a
/// time axis, and a line at the last price.
fn candle_chart(
    candles: &[Candle],
    last: Option<Decimal>,
    grouped: &[CandleFills],
    levels: &[OrderLevel],
    calls: Calls,
    entry: Option<Entry>,
    plot: &PlotPointer,
) -> AnyElement {
    let measured = &plot.width;
    // Too narrow for the price axis: the candles alone. And never more time
    // labels than fit, so none is cut off. The width is what the chart was
    // last drawn at; a change draws it again.
    let width = measured.get().map(f32::from);
    let compact = width.is_some_and(|w| w < COMPACT_CHART_WIDTH);
    let plot_width = width.map(|w| {
        if compact {
            w
        } else {
            w - PRICE_AXIS_WIDTH - 8.
        }
    });
    let time_labels = plot_width
        .map(|w| ((w / TIME_LABEL_WIDTH).floor() as usize).clamp(2, 5))
        .unwrap_or(5);
    // Too narrow even for two whole labels: the time alone, or the date
    // alone over more than three days.
    let short_times = plot_width.is_some_and(|w| w < 2. * TIME_LABEL_WIDTH);
    let span_ms = match (candles.first(), candles.last()) {
        (Some(first), Some(last)) => last.open_ms.saturating_sub(first.open_ms),
        _ => 0,
    };
    let bars: Vec<Bar> = candles
        .iter()
        .enumerate()
        .map(|(i, c)| Bar {
            key: SharedString::from(c.open_ms.to_string()),
            time: SharedString::from(time_label(c.open_ms, 24 * 3_600_000)),
            open: c.open,
            high: c.high,
            low: c.low,
            close: c.close,
            fills: grouped.get(i).cloned().unwrap_or_default(),
        })
        .collect();
    // Where each candle sits across the plot, as a share of its width: the
    // same band scale, with the same settings, the chart places them by.
    let keys: Vec<SharedString> = bars.iter().map(|b| b.key.clone()).collect();
    let bands = ScaleBand::new(keys.iter().cloned(), [0., 1.])
        .padding_inner(BAND_PADDING_INNER)
        .padding_outer(BAND_PADDING_OUTER);
    let half_band = bands.band_width() / 2.;
    let centre = |key: &SharedString| bands.tick(key).map(|start| start + half_band);
    let f64_of = |d: Decimal| d.to_f64().unwrap_or_default();
    let low = candles.iter().map(|c| c.low).min().unwrap_or_default();
    let high = candles.iter().map(|c| c.high).max().unwrap_or_default();
    let (low_f, high_f) = (f64_of(low), f64_of(high));
    // Prices are labelled at the precision the venue quotes this market in:
    // a closed candle's, since the live last price can be a finer mid.
    let like = candles.first().map(|c| c.open).unwrap_or(low);
    plot.range.set(Some((low_f, high_f, like)));

    let chart = CandlestickChart::new(bars)
        .id("candles")
        // Draw in when the market or period changes, not on every tick.
        .appear(false)
        .x(|b: &Bar| b.key.clone())
        .open(move |b: &Bar| f64_of(b.open))
        .high(move |b: &Bar| f64_of(b.high))
        .low(move |b: &Bar| f64_of(b.low))
        .close(move |b: &Bar| f64_of(b.close))
        .x_axis(false)
        .bullish(Palette::gain())
        .bearish(Palette::loss())
        .tooltip_content(|b: &Bar, _, _| {
            let line = |name: &'static str, value: Decimal| {
                h_flex()
                    .gap_4()
                    .justify_between()
                    .child(div().text_color(Palette::muted()).child(name))
                    .child(div().font_family(MONO).child(f::price(value)))
            };
            v_flex()
                .gap(px(2.))
                .text_size(px(12.))
                .child(div().text_color(Palette::muted()).child(b.time.clone()))
                .child(line("Open", b.open))
                .child(line("High", b.high))
                .child(line("Low", b.low))
                .child(line("Close", b.close))
                .children(
                    b.fills
                        .bought
                        .as_ref()
                        .map(|side| fills_line(FillSide::Bought, side, b.open)),
                )
                .children(
                    b.fills
                        .sold
                        .as_ref()
                        .map(|side| fills_line(FillSide::Sold, side, b.open)),
                )
        });

    let marks: Vec<AnyElement> = candles
        .iter()
        .zip(&keys)
        .zip(grouped)
        .enumerate()
        .flat_map(|(i, ((candle, key), fills))| {
            let Some(x) = centre(key) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            if fills.bought.is_some() {
                // Beneath the candle's low, kept inside the plot.
                let top =
                    (plot_y(f64_of(candle.low), low_f, high_f) + 3.).min(PLOT_HEIGHT - MARK_SIZE);
                out.push(fill_mark(("fill-b", i), "B", Palette::gain(), x, top));
            }
            if fills.sold.is_some() {
                // Above the candle's high.
                let top = (plot_y(f64_of(candle.high), low_f, high_f) - 3. - MARK_SIZE).max(0.);
                out.push(fill_mark(("fill-s", i), "S", Palette::loss(), x, top));
            }
            out
        })
        .collect();

    // The open orders, by where each sits against the candles' range. One
    // outside it has no line; those above, and those below, share a tag at
    // that edge.
    let mut inside: Vec<(usize, &OrderLevel, f32)> = Vec::new();
    let (mut above, mut below): (Vec<&OrderLevel>, Vec<&OrderLevel>) = (Vec::new(), Vec::new());
    for (i, level) in levels.iter().enumerate() {
        let value = f64_of(level.price);
        if value > high_f {
            above.push(level);
        } else if value < low_f {
            below.push(level);
        } else {
            inside.push((i, level, plot_y(value, low_f, high_f)));
        }
    }
    // A line fades in from the left and is strongest at the axis, so the
    // candles stay clear and the level reads where its price is. Only a
    // take-profit or stop-loss is named on it: the colour says buy or sell.
    let order_lines: Vec<AnyElement> = inside
        .iter()
        .map(|(i, level, y)| {
            let colour = level.colour();
            div()
                .id(("order-line", *i))
                .absolute()
                .left_0()
                .right_0()
                .top(px(*y))
                .h(px(1.))
                .bg(gpui_kit::linear_gradient(
                    90.,
                    gpui_kit::linear_color_stop(colour.opacity(0.), 0.),
                    gpui_kit::linear_color_stop(colour.opacity(ORDER_LINE_STRENGTH), 1.),
                ))
                .children(level.mark.map(|mark| {
                    div()
                        .absolute()
                        .right(px(6.))
                        .bottom(px(2.))
                        .text_size(px(10.5))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(colour.opacity(0.8))
                        .child(mark)
                }))
                .test_support()
                .into_any_element()
        })
        .collect();

    // Each order's price on the axis, in its side's colour; pointing at it
    // says what the order is. Tags that would overlap are moved apart.
    let edge = |levels: &[&OrderLevel], up: bool| -> Option<OrderTag> {
        let nearest = if up {
            levels.iter().min_by_key(|l| l.price)
        } else {
            levels.iter().max_by_key(|l| l.price)
        }?;
        let (arrow, side) = if up {
            ("\u{25b2}", "above")
        } else {
            ("\u{25bc}", "below")
        };
        let one = levels.len() == 1;
        let colour = if levels.iter().all(|l| l.buy == nearest.buy) {
            nearest.colour()
        } else {
            Palette::muted()
        };
        let detail = if one {
            nearest.detail.clone()
        } else {
            format!(
                "{} orders {side} the chart. The nearest: {}",
                levels.len(),
                nearest.detail
            )
        };
        Some(OrderTag {
            id: if up { "order-above" } else { "order-below" }.into(),
            text: if one {
                format!("{arrow} {}", f::price_like(nearest.price, like))
            } else {
                format!("{arrow} {}", levels.len())
            },
            spoken: if one {
                format!("{detail}, {side} the chart")
            } else {
                detail.clone()
            },
            detail,
            colour,
            centre: if up { 0. } else { PLOT_HEIGHT },
        })
    };
    let mut tags: Vec<OrderTag> = inside
        .iter()
        .map(|(i, level, y)| OrderTag {
            id: ("order-level", *i).into(),
            text: f::price_like(level.price, like),
            spoken: level.detail.clone(),
            detail: level.detail.clone(),
            colour: level.colour(),
            centre: *y,
        })
        .chain(edge(&above, true))
        .chain(edge(&below, false))
        .collect();
    tags.sort_by(|a, b| a.centre.total_cmp(&b.centre));
    let mut tops: Vec<f32> = tags
        .iter()
        .map(|t| t.centre - ORDER_TAG_HEIGHT / 2.)
        .collect();
    spread(
        &mut tops,
        ORDER_TAG_HEIGHT + 1.,
        PLOT_HEIGHT - ORDER_TAG_HEIGHT,
    );
    let tag_ys: Vec<f32> = tops.iter().map(|t| t + ORDER_TAG_HEIGHT / 2.).collect();
    let order_tags: Vec<AnyElement> = tags
        .into_iter()
        .zip(tops)
        .map(|(tag, top)| {
            let detail = tag.detail;
            div()
                .id(tag.id)
                .absolute()
                .left(px(4.))
                .top(px(top))
                .h(px(ORDER_TAG_HEIGHT))
                .px(px(5.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .border_1()
                .border_color(tag.colour.opacity(0.3))
                .bg(tag.colour.opacity(0.14))
                .font_family(MONO)
                .text_size(px(11.))
                .text_color(tag.colour)
                .whitespace_nowrap()
                .child(tag.text)
                .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
                .aria_label(tag.spoken)
                .test_support()
                .into_any_element()
        })
        .collect();

    // Five price labels, evenly spaced from the high down to the low, less
    // any an order's tag would cover.
    let labels = (0..5).filter_map(|i| {
        let value = high_f - (high_f - low_f) * i as f64 / 4.;
        let y = plot_y(value, low_f, high_f);
        if tag_ys.iter().any(|t| (t - y).abs() < ORDER_TAG_HEIGHT - 3.) {
            return None;
        }
        let text = f::price_like(Decimal::try_from(value).unwrap_or_default(), like);
        Some(
            div()
                .absolute()
                .right_0()
                .top(px(y - 7.))
                .font_family(MONO)
                .text_size(px(11.))
                .text_color(Palette::muted())
                .child(text),
        )
    });

    // Five times along the bottom, from the oldest candle to the newest.
    let times = (0..time_labels).map(|i| {
        let at = (candles.len() - 1) * i / (time_labels - 1);
        div()
            .id(("chart-time", i))
            .whitespace_nowrap()
            .font_family(MONO)
            .text_size(px(11.))
            .text_color(Palette::muted())
            .child(if short_times {
                short_time_label(candles[at].open_ms, span_ms)
            } else {
                time_label(candles[at].open_ms, span_ms)
            })
    });

    let measure = {
        let measured = measured.clone();
        gpui_kit::canvas(
            move |bounds, window, _| {
                if measured.replace(Some(bounds.size.width)) != Some(bounds.size.width) {
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    };
    v_flex()
        .relative()
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .gap_1()
        .child(measure)
        .child(
            h_flex()
                .min_w_0()
                .items_start()
                .gap_2()
                .child(plot_area(
                    div()
                        .id("chart-plot")
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h(px(PLOT_HEIGHT))
                        .child(chart)
                        // The watcher's call levels, under everything the
                        // account did.
                        .children(call_lines(&calls.levels, low_f, high_f, like))
                        // The account's open orders, beneath the fills and
                        // the last price.
                        .children(order_lines)
                        // Where the account's position was entered.
                        .children(entry_line(entry.as_ref(), low_f, high_f, like))
                        // The account's fills: a B under the candle where
                        // it bought, an S above the candle where it sold.
                        .children(marks)
                        // The last price, across the plot.
                        .children(last.map(|last| {
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top(px(plot_y(f64_of(last), low_f, high_f)))
                                .h(px(1.))
                                .bg(Palette::amber().opacity(0.75))
                        })),
                    plot,
                    calls.menu,
                ))
                .when(!compact, |row| {
                    row.child(
                        div()
                            .id("chart-price-axis")
                            .relative()
                            .w(px(PRICE_AXIS_WIDTH))
                            .h(px(PLOT_HEIGHT))
                            .flex_shrink_0()
                            .children(labels)
                            .children(order_tags)
                            .test_support(),
                    )
                }),
        )
        .child(
            h_flex()
                .id("chart-time-axis")
                .min_w_0()
                .overflow_hidden()
                .justify_between()
                .when(!compact, |axis| axis.pr(px(PRICE_AXIS_WIDTH + 8.)))
                .children(times.map(|t| t.test_support()))
                .test_support(),
        )
        .into_any_element()
}

#[cfg(test)]
impl Markets {
    /// What the position panel last drew.
    pub fn position_panel(&self, cx: &gpui_kit::App) -> Option<market_position::Model> {
        self.position.read(cx).position_panel()
    }

    /// The price range the chart was last drawn over: low and high.
    pub fn chart_range(&self, cx: &gpui_kit::App) -> Option<(f64, f64)> {
        self.chart.read(cx).chart_range()
    }

    /// Type a price in the alerts menu.
    pub fn set_alert_price(&self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.position
            .update(cx, |p, cx| p.set_alert_price(text, window, cx));
    }

    /// Type a distance in the alerts menu.
    pub fn set_alert_near_liq(&self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.position
            .update(cx, |p, cx| p.set_alert_near_liq(text, window, cx));
    }

    /// Put the keyboard on the PNL breakdown's header.
    pub fn focus_pnl_header(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.position
            .update(cx, |p, cx| p.focus_pnl_header(window, cx));
    }

    /// Replace what the feed has reported, for tests that have no venue.
    /// What every panel shares, for building panels beside the windowed ones.
    pub fn model(&self) -> Entity<MarketsModel> {
        self.model.clone()
    }

    /// Draw Markets as if the window were fullscreen, or not.
    pub fn set_fullscreen(&mut self, fullscreen: bool, cx: &mut Context<Self>) {
        self.fullscreen = Some(fullscreen);
        cx.notify();
    }

    /// The workspace's dock area, once it has been made.
    pub fn workspace_area(&self) -> Option<Entity<gpui_kit::component::dock::DockArea>> {
        self.workspace.as_ref().map(|w| w.area.clone())
    }

    /// Show `market` on the workspace's channel A.
    pub fn set_channel_a(&mut self, market: &str) {
        if let Some(w) = &mut self.workspace {
            w.channels[0] = Some(MarketId::new(market));
        }
    }

    pub fn set_snapshot(&mut self, snapshot: MarketSnapshot, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            m.snapshot = snapshot;
            cx.notify();
        });
    }

    pub fn watched(&self, cx: &gpui_kit::App) -> Vec<String> {
        self.model
            .read(cx)
            .watchlist
            .as_ref()
            .map(|w| w.markets().iter().map(|m| m.to_string()).collect())
            .unwrap_or_default()
    }

    pub fn selections(&self, cx: &gpui_kit::App) -> String {
        let m = self.model.read(cx);
        format!("period={} colour={}", m.period.label(), m.colour.label())
    }

    /// What the chart area last drew: `loading`, `failed`, `short`, `chart`
    /// or `none`; how many `B` and `S` marks; and the "shown since" time.
    pub fn drawn_chart(&self, cx: &gpui_kit::App) -> (&'static str, usize, usize, Option<u64>) {
        self.chart.read(cx).drawn_chart()
    }

    /// The tiles in the order they were last drawn.
    pub fn drawn(&self, cx: &gpui_kit::App) -> Vec<String> {
        self.model
            .read(cx)
            .order
            .iter()
            .map(|m| m.to_string())
            .collect()
    }

    /// Pretend the settled order is old enough to be worked out again.
    pub fn age_order(&mut self, cx: &mut Context<Self>) {
        self.model
            .update(cx, |m, _| m.ordered_at = Some(Instant::now() - SETTLE));
    }

    /// The market the panel shows.
    pub fn selected_market(&self, cx: &gpui_kit::App) -> Option<String> {
        self.selected(cx).map(|m| m.to_string())
    }

    /// The tile the pointer rests on, which shows its remove button.
    pub fn hovered_market(&self, cx: &gpui_kit::App) -> Option<String> {
        self.tiles.read(cx).hovered_market()
    }

    pub fn picker_is_open(&self) -> bool {
        self.picker_open
    }

    /// Put the keyboard focus on a market's tile.
    pub fn focus_market(&self, market: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.tiles
            .update(cx, |tiles, cx| tiles.focus_market(market, window, cx));
    }

    /// Type into the add-market search field.
    pub fn set_search(&self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
        cx.notify();
    }
}

#[cfg(test)]
impl Markets {
    /// The "Alert for <market>…" form, while it is open.
    pub fn tile_form(&self, cx: &gpui_kit::App) -> Option<Entity<crate::level_form::LevelForm>> {
        self.tiles.read(cx).tile_form()
    }
}

#[cfg(test)]
mod call_level_tests {
    use super::*;

    #[test]
    fn the_pointer_height_inverts_the_price_scale() {
        let (low, high) = (99., 141.);
        for price in [99., 120., 130., 141.] {
            let y = plot_y(price, low, high);
            assert!((price_at(y, low, high) - price).abs() < 1e-3, "{price}");
        }
        // Above or below the plot clamps to its range.
        assert_eq!(price_at(-50., low, high), high);
        assert_eq!(price_at(PLOT_HEIGHT + 50., low, high), low);
    }

    #[test]
    fn a_pointer_price_is_rounded_to_the_venues_precision() {
        // Whole-number quotes: no decimals.
        assert_eq!(round_price(130.008, Decimal::from(100)), Decimal::from(130));
        // Five significant figures.
        assert_eq!(
            round_price(62_512.34, Decimal::new(625_001, 1)),
            Decimal::from(62_512)
        );
        assert_eq!(
            round_price(1.234_567, Decimal::new(12_345, 4)),
            Decimal::new(12_346, 4)
        );
        // No more decimals than the market quotes.
        assert_eq!(
            round_price(0.123_456, Decimal::new(123, 3)),
            Decimal::new(123, 3)
        );
    }
}
