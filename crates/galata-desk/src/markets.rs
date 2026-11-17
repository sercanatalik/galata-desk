//! The Markets section: the watched markets as a grid of heatmap tiles.
//! Read-only: it subscribes to public market data and nothing else.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Local};
use galata_desk_core::format as f;
use galata_desk_core::market::candles::Candle;
use galata_desk_core::market::feed::{
    Connection, Listed, Listing, Live, MarketFeed, MarketSnapshot, Tile,
};
use galata_desk_core::market::sort::{self, SortBy};
use galata_desk_core::market::watchlist::{AddError, LIMIT, Watchlist};
use galata_desk_core::market::{Interval, MarketId, MarketPeriod};
use galata_desk_core::model::PositionRow;
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::rust_decimal::prelude::ToPrimitive;
use galata_desk_core::venue::Http;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::chart::CandlestickChart;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, FocusHandle, KeyDownEvent, SharedString, Subscription, Task,
    TestSupportExt, Window, deferred, div, px,
};

use crate::Core;
use crate::market_tile::{self, ColourBy};
use crate::portfolio::Portfolio;
use crate::theme::{MONO, Palette};
use crate::ui;

/// A tile is at least this wide; the grid fits as many columns as that allows.
const TILE_MIN: f32 = 196.;
const TILE_HEIGHT: f32 = 96.;
const GAP: f32 = 8.;
/// The market panel: a fixed height above the tiles, its detail column on
/// the left and the chart taking the rest.
const PANEL_HEIGHT: f32 = 344.;
const DETAIL_WIDTH: f32 = 280.;
/// The chart's plot, and the column of price labels beside it.
const PLOT_HEIGHT: f32 = 266.;
const PRICE_AXIS_WIDTH: f32 = 74.;
/// The component kit draws a chart's highest value this far below its top.
const PLOT_TOP_INSET: f32 = 10.;
/// How long a sorted order is held before it is worked out again.
const SETTLE: Duration = Duration::from_secs(5);

/// How long to wait for a first connection before saying it has not come.
const PATIENCE: Duration = Duration::from_secs(5);

pub struct Markets {
    portfolio: Entity<Portfolio>,
    feed: MarketFeed,
    snapshot: MarketSnapshot,
    /// `None` until the default list can be decided, which needs the
    /// account's positions.
    watchlist: Option<Watchlist>,
    period: MarketPeriod,
    colour: ColourBy,
    /// The tile the pointer rests on. It shows that tile's remove button and
    /// nothing else: only a click selects.
    hovered: Option<MarketId>,
    /// What the tiles are ordered by. Only the drawing order: the watchlist
    /// keeps its own.
    sort: SortBy,
    /// The order the tiles were last drawn in, and when it was worked out.
    /// It is held for a few seconds so tiles do not jump on every tick.
    order: Vec<MarketId>,
    ordered_at: Option<Instant>,
    /// Work the order out again at the next draw, whatever its age.
    reorder: bool,
    /// One focus handle per tile, so the keyboard can reach each.
    focus: HashMap<MarketId, FocusHandle>,
    picker_open: bool,
    search: Entity<InputState>,
    /// Something the user should know that is not about the venue.
    warning: Option<SharedString>,
    opened_at: Instant,
    _watch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Markets {
    pub fn new(portfolio: Entity<Portfolio>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let core = Core::get(cx);
        let stored = core.store.watchlist();
        let watched: Vec<MarketId> = stored
            .as_ref()
            .map(|w| w.markets().to_vec())
            .unwrap_or_default();
        // The feed's tasks run on the I/O runtime.
        let feed = {
            let _io = core.runtime.enter();
            MarketFeed::live(
                Http::at(core.info_url()),
                core.ws_url(),
                MarketPeriod::default(),
                watched,
            )
        };
        #[allow(unused_mut)]
        let mut snapshots = feed.snapshots();
        // In the headless tests the view is given its snapshots directly. The
        // feed still runs, on the I/O runtime's threads, and a wake-up from
        // one of them is activity the deterministic test scheduler refuses:
        // so there the view does not listen to it.
        #[cfg(test)]
        let watch = {
            drop(snapshots);
            Task::ready(())
        };
        #[cfg(not(test))]
        let watch = cx.spawn(async move |this, cx| {
            while snapshots.changed().await.is_ok() {
                let snapshot = snapshots.borrow_and_update().clone();
                let applied = this.update(cx, |this, cx| {
                    this.snapshot = snapshot;
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        });

        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search markets"));
        let mut subscriptions = vec![
            // Typing in the search field redraws the list.
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        // The portfolio's positions decide the default list, and the held marks.
        subscriptions.push(cx.observe(&portfolio, |this, _, cx| {
            this.decide_default(cx);
            cx.notify();
        }));

        // Redraw once patience runs out, so "cannot reach the venue" can show.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(PATIENCE + Duration::from_millis(100))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();

        #[allow(unused_mut)]
        let mut this = Self {
            portfolio,
            feed,
            snapshot: MarketSnapshot::default(),
            watchlist: stored,
            period: MarketPeriod::default(),
            colour: ColourBy::default(),
            hovered: None,
            sort: SortBy::default(),
            order: Vec::new(),
            ordered_at: None,
            reorder: false,
            focus: HashMap::new(),
            picker_open: false,
            search,
            warning: None,
            opened_at: Instant::now(),
            _watch: watch,
            _subscriptions: subscriptions,
        };
        this.decide_default(cx);
        #[cfg(feature = "shot")]
        this.preselect();
        this.feed
            .select(this.watchlist.as_ref().and_then(|w| w.selected().cloned()));
        this
    }

    /// `GALATA_DESK_SHOT_STATE=section=markets,mperiod=week,colour=funding,select=xyz:NVDA,picker=1`
    #[cfg(feature = "shot")]
    fn preselect(&mut self) {
        let state = std::env::var("GALATA_DESK_SHOT_STATE").unwrap_or_default();
        for (key, value) in state.split(',').filter_map(|kv| kv.split_once('=')) {
            match key {
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
                    self.period = period;
                    self.feed.set_period(period);
                }
                "sort" => {
                    self.sort = match value {
                        "gainers" => SortBy::Gainers,
                        "losers" => SortBy::Losers,
                        "volatility" => SortBy::Volatility,
                        "funding" => SortBy::Funding,
                        "name" => SortBy::Name,
                        _ => SortBy::Watchlist,
                    };
                }
                "colour" => {
                    self.colour = match value {
                        "volatility" => ColourBy::Volatility,
                        "funding" => ColourBy::Funding,
                        _ => ColourBy::Change,
                    }
                }
                "select" => {
                    if let Some(list) = &mut self.watchlist {
                        list.select(&MarketId::new(value));
                    }
                }
                "picker" => self.picker_open = true,
                _ => {}
            }
        }
    }

    /// With no remembered watchlist, start from what the account holds, once
    /// its positions are known.
    fn decide_default(&mut self, cx: &mut Context<Self>) {
        if self.watchlist.is_some() {
            return;
        }
        let Some(list) = self
            .portfolio
            .read(cx)
            .positions()
            .map(Watchlist::default_for)
        else {
            return;
        };
        for market in list.markets() {
            self.feed.watch(market.clone());
        }
        self.feed.select(list.selected().cloned());
        self.watchlist = Some(list);
        self.save(cx);
    }

    /// The market the panel shows.
    fn selected(&self) -> Option<&MarketId> {
        self.watchlist.as_ref()?.selected()
    }

    /// Show a market in the panel. Only a click, or Enter or Space on a
    /// focused tile, comes here; pointing at a tile does not.
    fn select(&mut self, market: &MarketId, cx: &mut Context<Self>) {
        let Some(list) = &mut self.watchlist else {
            return;
        };
        if list.selected() != Some(market) && list.select(market) {
            self.feed.select(Some(market.clone()));
            self.save(cx);
        }
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(list) = &self.watchlist
            && let Err(error) = Core::get(cx).store.save_watchlist(list)
        {
            self.warning = Some(format!("The watchlist could not be saved: {error}").into());
        }
    }

    fn add(&mut self, market: MarketId, cx: &mut Context<Self>) {
        let Some(list) = &mut self.watchlist else {
            return;
        };
        match list.add(market.clone()) {
            Ok(()) => {
                self.feed.watch(market);
                // A market added to an empty watchlist becomes the selection.
                self.feed.select(list.selected().cloned());
                self.save(cx);
            }
            // The list shows both states; choosing such a row adds nothing.
            Err(AddError::AlreadyWatched | AddError::Full) => {}
        }
        cx.notify();
    }

    fn remove(&mut self, market: &MarketId, cx: &mut Context<Self>) {
        let Some(list) = &mut self.watchlist else {
            return;
        };
        if list.remove(market) {
            // Removing the selected market selects its neighbour.
            self.feed.select(list.selected().cloned());
            self.feed.unwatch(market.clone());
            self.focus.remove(market);
            if self.hovered.as_ref() == Some(market) {
                self.hovered = None;
            }
            self.save(cx);
        }
        cx.notify();
    }

    fn set_period(&mut self, period: MarketPeriod, cx: &mut Context<Self>) {
        self.period = period;
        self.feed.set_period(period);
        // Change and volatility are now another period's: order by them at once.
        self.reorder = true;
        cx.notify();
    }

    fn set_sort(&mut self, sort: SortBy, cx: &mut Context<Self>) {
        self.sort = sort;
        self.reorder = true;
        cx.notify();
    }

    /// The order to draw `tiles` in, as positions into them; they are in
    /// watchlist order.
    ///
    /// A sort by a moving figure is settled: worked out again only every few
    /// seconds, or at once when the sort, the period or the set of markets
    /// changes.
    fn settled_order(&mut self, tiles: &[Tile]) -> Vec<usize> {
        let same_markets =
            self.order.len() == tiles.len() && tiles.iter().all(|t| self.order.contains(&t.market));
        let due = self.ordered_at.is_none_or(|at| at.elapsed() >= SETTLE);
        if self.reorder || !same_markets || !self.sort.moves() || due {
            self.order = sort::order(tiles, self.sort)
                .into_iter()
                .map(|i| tiles[i].market.clone())
                .collect();
            self.ordered_at = Some(Instant::now());
            self.reorder = false;
        }
        self.order
            .iter()
            .filter_map(|market| tiles.iter().position(|t| &t.market == market))
            .collect()
    }

    fn toggle_picker(&mut self, cx: &mut Context<Self>) {
        self.picker_open = !self.picker_open;
        if self.picker_open {
            // The list may have failed to load, or grown old.
            self.feed.refresh_listing();
        }
        cx.notify();
    }

    /// The account's position in a market, if it holds one.
    fn position(&self, market: &MarketId, cx: &Context<Self>) -> Option<PositionRow> {
        self.portfolio
            .read(cx)
            .positions()?
            .iter()
            .find(|p| p.dex == market.dex() && p.market == market.ticker())
            .cloned()
    }

    /// Whether the feed has failed to connect for long enough to say so.
    fn unreachable(&self) -> bool {
        self.snapshot.connection == Connection::Connecting && self.opened_at.elapsed() >= PATIENCE
    }

    /// The Markets section's part of the title bar: whether it is live.
    pub fn status(&mut self, _: &mut Context<Self>) -> AnyElement {
        let (text, colour) = match self.snapshot.connection {
            Connection::Live => ("Live".to_string(), Palette::gain()),
            Connection::Down { .. } => ("Not live".to_string(), Palette::amber()),
            Connection::Connecting if self.unreachable() => {
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
        let count = self.watchlist.as_ref().map(|w| w.markets().len());
        // One period for the whole section: the tiles' figures and the chart.
        let chosen = self.period;
        let view = cx.entity().downgrade();
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
                                let _ = view.update(cx, |this, cx| this.set_period(choice, cx));
                            }),
                    );
                }
                menu
            });
        let sorted = self.sort;
        let view = cx.entity().downgrade();
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
                                let _ = view.update(cx, |this, cx| this.set_sort(choice, cx));
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
                    .position(|c| *c == self.colour)
                    .unwrap_or(0),
            )
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.colour = ColourBy::ALL[*ix];
                cx.notify();
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

    fn legend(&self) -> AnyElement {
        let (low, steps, high) = market_tile::legend(self.colour, self.period);
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
            .child("Click a tile to chart it")
            .into_any_element()
    }

    /// The add-market list, opened from the add button.
    fn picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let full = self.watchlist.as_ref().is_some_and(Watchlist::is_full);
        let query = self.search.read(cx).value().trim().to_lowercase();

        let body: AnyElement = match &self.snapshot.listing {
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
                                this.feed.refresh_listing();
                                cx.notify();
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
                        let watched = self.watchlist.as_ref().is_some_and(|w| w.contains(&market));
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

    /// One market's tile. Clicking it, or pressing Enter or Space while it
    /// has focus, selects its market.
    fn tile(
        &mut self,
        ix: usize,
        tile: &Tile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let market = tile.market.clone();
        let handle = self
            .focus
            .entry(market.clone())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        let focused = handle.is_focused(window);
        let selected = self.selected() == Some(&market);
        // Pointing at a tile, or focusing it, shows its remove button.
        let reachable = focused || self.hovered.as_ref() == Some(&market);
        let held = self.position(&market, cx).is_some();
        let by_funding = self.sort == SortBy::Funding;

        let header = h_flex()
            .gap(px(6.))
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(market.ticker().to_string()),
            )
            .when(!market.dex().is_empty(), |d| d.child(dex_tag(market.dex())))
            .when(held, |d| {
                d.child(div().size(px(7.)).rounded_full().bg(Palette::amber()))
            })
            .child(div().flex_1());

        let soft = gpui_kit::rgb(0xd8d3c8);
        let body: AnyElement = if tile.unavailable {
            v_flex()
                .flex_1()
                .justify_between()
                .child(header)
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(Palette::muted())
                        .child("Unavailable"),
                )
                .child(h_flex().child(remove_labelled(ix, &market, cx)))
                .into_any_element()
        } else {
            let pending = |w: f32, h: f32| ui::placeholder(px(w), px(h)).into_any_element();
            let stats = tile.stats.as_ref();
            v_flex()
                .flex_1()
                .justify_between()
                .child(
                    header
                        .child(match stats {
                            Some(s) => div()
                                .font_family(MONO)
                                .text_size(px(12.5))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(f::pct_signed_f(s.change, 2))
                                .into_any_element(),
                            None => pending(52., 14.),
                        })
                        .when(reachable, |d| d.child(remove_button(ix, &market, cx))),
                )
                .child(match &tile.live {
                    Some(live) => ui::figure(f::price(live.last), px(19.)).into_any_element(),
                    None => pending(110., 22.),
                })
                .child(
                    h_flex()
                        .justify_between()
                        .text_size(px(11.5))
                        .text_color(soft)
                        // Sorted by funding, the tile shows what it is sorted by.
                        .child(if by_funding {
                            "Funding (1h)"
                        } else {
                            "Volatility"
                        })
                        .child(match (by_funding, &tile.live, stats) {
                            (true, Some(live), _) => div()
                                .font_family(MONO)
                                .child(f::funding_hourly(live.funding))
                                .into_any_element(),
                            (false, _, Some(s)) => div()
                                .font_family(MONO)
                                .child(f::volatility(s.volatility))
                                .into_any_element(),
                            _ => pending(34., 12.),
                        }),
                )
                .into_any_element()
        };

        let hover_market = market.clone();
        let click_market = market.clone();
        let key_market = market.clone();
        div()
            .id(("tile", ix))
            .relative()
            .h(px(TILE_HEIGHT))
            .cursor_pointer()
            .track_focus(&handle)
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered = Some(hover_market.clone());
                } else if this.hovered.as_ref() == Some(&hover_market) {
                    this.hovered = None;
                }
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.select(&click_market, cx)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "enter" | "space" => this.select(&key_market, cx),
                    // A focused tile is removed with Delete or Backspace.
                    "backspace" | "delete" => this.remove(&key_market, cx),
                    _ => {}
                }
            }))
            .child(
                v_flex()
                    .size_full()
                    .px_3()
                    .pt(px(10.))
                    .pb(px(9.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if selected {
                        Palette::amber()
                    } else if focused {
                        Palette::amber_line()
                    } else {
                        Palette::text().opacity(0.08)
                    })
                    .bg(market_tile::background(self.colour, self.period, tile))
                    .child(body),
            )
            .test_support()
            .into_any_element()
    }

    /// The market panel: the selected market's detail on the left, its chart
    /// on the right. `None` when nothing is watched.
    fn panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let market = self.selected()?.clone();
        let blank = Tile {
            market: market.clone(),
            live: None,
            stats: None,
            max_leverage: None,
            unavailable: false,
        };
        let tile = self
            .snapshot
            .tiles
            .iter()
            .find(|t| t.market == market)
            .unwrap_or(&blank);
        let position = self.position(&market, cx);

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
            // The chart belongs to the panel only while it is this market's:
            // just after a new selection the feed may still hold the last one.
            let candles = self
                .snapshot
                .chart
                .as_ref()
                .filter(|chart| chart.market == market && chart.interval == self.period.interval())
                .map(|chart| chart.candles.as_slice());
            h_flex()
                .size_full()
                .child(self.detail(tile, position.as_ref()))
                .child(self.chart(tile, candles))
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

    /// The panel's left side: everything the venue says about the market now.
    fn detail(&self, tile: &Tile, position: Option<&PositionRow>) -> AnyElement {
        let market = &tile.market;
        let pending = |w: f32, h: f32| ui::placeholder(px(w), px(h)).into_any_element();
        let row = |name: &str, value: AnyElement| {
            h_flex()
                .justify_between()
                .text_size(px(12.5))
                .child(div().text_color(Palette::muted()).child(name.to_string()))
                .child(value)
        };
        let figure = |text: String, tone: gpui_kit::Hsla| {
            div()
                .font_family(MONO)
                .text_color(tone)
                .child(text)
                .into_any_element()
        };
        let plain = Palette::text();
        let live: Option<&Live> = tile.live.as_ref();
        let stats = tile.stats.as_ref();
        // A live figure, or a placeholder until the stream has spoken.
        let from_live = |show: &dyn Fn(&Live) -> (String, gpui_kit::Hsla)| match live {
            Some(l) => {
                let (text, tone) = show(l);
                figure(text, tone)
            }
            None => pending(70., 13.),
        };
        // A figure that needs history, or a placeholder until it has loaded.
        let from_stats =
            |show: &dyn Fn(&galata_desk_core::market::candles::Stats) -> String| match stats {
                Some(s) => figure(show(s), plain),
                None => pending(70., 13.),
            };
        let funding_tone = |l: &Live| ui::tone_of(sign(l.funding));

        v_flex()
            .w(px(DETAIL_WIDTH))
            .h_full()
            .flex_shrink_0()
            .px_4()
            .py_3()
            .gap(px(3.))
            .border_r_1()
            .border_color(Palette::rule())
            .child(
                h_flex()
                    .gap(px(7.))
                    .child(
                        div()
                            .text_size(px(17.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(market.ticker().to_string()),
                    )
                    .when(!market.dex().is_empty(), |d| d.child(dex_tag(market.dex())))
                    .when(position.is_some(), |d| {
                        d.child(div().size(px(7.)).rounded_full().bg(Palette::amber()))
                    })
                    .child(div().flex_1())
                    .children(tile.max_leverage.map(|l| {
                        div()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(Palette::muted())
                            .child(format!("up to {l}x"))
                    })),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_baseline()
                    .pb_1()
                    .child(match live {
                        Some(l) => ui::figure(f::price(l.last), px(26.)).into_any_element(),
                        None => pending(130., 28.),
                    })
                    .child(match stats {
                        Some(s) => div()
                            .font_family(MONO)
                            .text_size(px(13.5))
                            .text_color(ui::tone_of(if s.change > 0.0 {
                                1
                            } else if s.change < 0.0 {
                                -1
                            } else {
                                0
                            }))
                            .child(f::pct_signed_f(s.change, 2))
                            .into_any_element(),
                        None => pending(56., 14.),
                    }),
            )
            .child(row("Mark", from_live(&|l| (f::price(l.mark), plain))))
            .child(row("Oracle", from_live(&|l| (f::price(l.oracle), plain))))
            .child(row(
                "Premium",
                from_live(&|l| (f::premium(l.premium), plain)),
            ))
            .child(row(
                "Funding (1h)",
                from_live(&|l| (f::funding_hourly(l.funding), funding_tone(l))),
            ))
            .child(row(
                "Funding (yearly)",
                from_live(&|l| (f::funding_yearly(l.funding), funding_tone(l))),
            ))
            .child(row(
                "24h volume",
                from_live(&|l| (f::usd_abbrev(l.volume_usd), plain)),
            ))
            .child(row(
                "Open interest",
                from_live(&|l| (f::usd_abbrev(l.open_interest_usd), plain)),
            ))
            .child(row(
                &format!("{} low / high", self.period.label()),
                from_stats(&|s| format!("{} / {}", f::price(s.low), f::price(s.high))),
            ))
            .child(row(
                "Volatility",
                from_stats(&|s| f::volatility(s.volatility)),
            ))
            .children(position.map(|p| {
                h_flex()
                    .justify_between()
                    .mt(px(2.))
                    .pt(px(6.))
                    .border_t_1()
                    .border_color(Palette::line())
                    .text_size(px(12.5))
                    .child(
                        div()
                            .text_color(Palette::amber_text())
                            .child("Your position"),
                    )
                    .child(div().font_family(MONO).child(signed_size(p)))
            }))
            .into_any_element()
    }

    /// The panel's right side: the market's candles over the period.
    fn chart(&self, tile: &Tile, candles: Option<&[Candle]>) -> AnyElement {
        let interval = self.period.interval();
        // A period under an hour is too few candles to read, so the chart
        // shows the hour it ends; the heading says so.
        let shown = if self.period.chart_millis() > self.period.millis() {
            format!("{}, last hour", interval_words(interval))
        } else {
            interval_words(interval).to_string()
        };
        let heading = h_flex()
            .gap_4()
            .text_size(px(12.5))
            .text_color(Palette::muted())
            .child(
                div()
                    .text_color(Palette::text())
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(format!(
                        "{} \u{b7} {}",
                        tile.market.ticker(),
                        self.period.label()
                    )),
            )
            .child(shown);

        let message = |text: &'static str| {
            h_flex()
                .size_full()
                .justify_center()
                .text_color(Palette::faint())
                .child(text)
                .into_any_element()
        };
        let body: AnyElement = match candles {
            // History has not loaded: a placeholder in the chart's shape.
            None => div()
                .size_full()
                .child(ui::placeholder(px(9999.), px(9999.)))
                .into_any_element(),
            Some(candles) if candles.len() < 2 => message("Not enough history for this period yet"),
            Some(candles) => candle_chart(candles, tile.live.as_ref().map(|l| l.last)),
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .px_4()
            .py_3()
            .gap_2()
            .child(heading)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn notices(&self) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if let Some(warning) = &self.warning {
            out.push(ui::notice(warning.clone()).into_any_element());
        }
        match self.snapshot.connection {
            Connection::Down { since } => out.push(
                ui::notice(format!(
                    "Not live since {}. The figures below are the last received; reconnecting.",
                    clock(since)
                ))
                .into_any_element(),
            ),
            Connection::Connecting if self.unreachable() => out.push(
                ui::notice("Hyperliquid's market feed cannot be reached. The app keeps trying.")
                    .into_any_element(),
            ),
            _ => {}
        }
        out
    }
}

/// The small tag naming a builder dex.
fn dex_tag(dex: &str) -> gpui_kit::Div {
    div()
        .px(px(5.))
        .rounded(px(4.))
        .bg(Palette::ground().opacity(0.45))
        .font_family(MONO)
        .text_size(px(10.5))
        .child(dex.to_string())
}

/// The remove action on an unavailable tile: always shown, and labelled.
fn remove_labelled(ix: usize, market: &MarketId, cx: &mut Context<Markets>) -> Button {
    let market = market.clone();
    Button::new(("remove", ix))
        .xsmall()
        .label("Remove")
        .on_click(cx.listener(move |this, _, _, cx| this.remove(&market, cx)))
}

/// The remove action on a tile.
fn remove_button(ix: usize, market: &MarketId, cx: &mut Context<Markets>) -> Button {
    let market = market.clone();
    Button::new(("remove", ix))
        .ghost()
        .xsmall()
        .label("\u{d7}")
        .accessibility_label(format!("Remove {}", market.ticker()))
        .tooltip("Remove from the watchlist")
        .on_click(cx.listener(move |this, _, _, cx| this.remove(&market, cx)))
}

impl Render for Markets {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let padding = 12.;
        let width = f32::from(window.viewport_size().width) - 2. * padding;
        let columns = (((width + GAP) / (TILE_MIN + GAP)).floor() as u16).max(1);

        let body: AnyElement = match &self.watchlist {
            // The default list needs the account's positions.
            None => h_flex()
                .size_full()
                .justify_center()
                .text_color(Palette::muted())
                .child("Waiting for the account's positions to choose the markets to watch\u{2026}")
                .into_any_element(),
            Some(list) if list.is_empty() => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_color(Palette::muted())
                        .child("Nothing is watched."),
                )
                .child(
                    Button::new("add-first")
                        .primary()
                        .label("Add a market")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.picker_open {
                                this.toggle_picker(cx);
                            }
                        })),
                )
                .into_any_element(),
            Some(list) => {
                // A tile for every watched market, at once: one the feed has
                // not reported yet is all placeholders.
                let tiles: Vec<Tile> = list
                    .markets()
                    .iter()
                    .map(|market| {
                        self.snapshot
                            .tiles
                            .iter()
                            .find(|t| &t.market == market)
                            .cloned()
                            .unwrap_or(Tile {
                                market: market.clone(),
                                live: None,
                                stats: None,
                                max_leverage: None,
                                unavailable: false,
                            })
                    })
                    .collect();
                let mut grid = div().grid().grid_cols(columns).gap(px(GAP));
                // Drawn in the sorted order; a tile's id stays its place in the
                // watchlist, so focus and Remove follow the market.
                for ix in self.settled_order(&tiles) {
                    grid = grid.child(self.tile(ix, &tiles[ix], window, cx));
                }
                div()
                    .id("tiles")
                    .size_full()
                    .overflow_y_scroll()
                    .child(grid)
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .p(px(padding))
            .gap_3()
            .child(self.toolbar(cx))
            .child(self.legend())
            .children(self.notices())
            // The selected market, above the tiles and always in view.
            .children(self.panel(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
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

/// A position's size with its sign always shown: `+123.467 NVDA`.
fn signed_size(position: &PositionRow) -> String {
    let size = f::size(position.size, &position.market);
    if position.size > Decimal::ZERO {
        format!("+{size}")
    } else {
        size
    }
}

/// A time of day on this machine's clock: `04:21:07`.
fn clock(at: SystemTime) -> String {
    DateTime::<Local>::from(at).format("%H:%M:%S").to_string()
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

/// Where a price sits down the plot, in pixels from its top.
///
/// The component kit's candlestick chart, with its own time axis off, puts
/// the lowest value at the bottom edge and the highest [`PLOT_TOP_INSET`]
/// below the top. The price labels and the last-price line are placed by the
/// same rule so they sit on the candles.
fn plot_y(value: f64, low: f64, high: f64) -> f32 {
    if high <= low {
        return PLOT_HEIGHT / 2.;
    }
    let ratio = ((value - low) / (high - low)).clamp(0., 1.) as f32;
    PLOT_HEIGHT - ratio * (PLOT_HEIGHT - PLOT_TOP_INSET)
}

/// The candlestick chart with what the kit does not draw: a price axis, a
/// time axis, and a line at the last price.
fn candle_chart(candles: &[Candle], last: Option<Decimal>) -> AnyElement {
    let span_ms = match (candles.first(), candles.last()) {
        (Some(first), Some(last)) => last.open_ms.saturating_sub(first.open_ms),
        _ => 0,
    };
    let bars: Vec<Bar> = candles
        .iter()
        .map(|c| Bar {
            key: SharedString::from(c.open_ms.to_string()),
            time: SharedString::from(time_label(c.open_ms, 24 * 3_600_000)),
            open: c.open,
            high: c.high,
            low: c.low,
            close: c.close,
        })
        .collect();
    let f64_of = |d: Decimal| d.to_f64().unwrap_or_default();
    let low = candles.iter().map(|c| c.low).min().unwrap_or_default();
    let high = candles.iter().map(|c| c.high).max().unwrap_or_default();
    let (low_f, high_f) = (f64_of(low), f64_of(high));
    // Prices are labelled at the precision the venue quotes this market in:
    // a closed candle's, since the live last price can be a finer mid.
    let like = candles.first().map(|c| c.open).unwrap_or(low);

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
        });

    // Five price labels, evenly spaced from the high down to the low.
    let labels = (0..5).map(|i| {
        let value = high_f - (high_f - low_f) * i as f64 / 4.;
        let text = f::price_like(Decimal::try_from(value).unwrap_or_default(), like);
        div()
            .absolute()
            .right_0()
            .top(px(plot_y(value, low_f, high_f) - 7.))
            .font_family(MONO)
            .text_size(px(11.))
            .text_color(Palette::muted())
            .child(text)
    });

    // Five times along the bottom, from the oldest candle to the newest.
    let times = (0..5).map(|i| {
        let at = (candles.len() - 1) * i / 4;
        div()
            .font_family(MONO)
            .text_size(px(11.))
            .text_color(Palette::muted())
            .child(time_label(candles[at].open_ms, span_ms))
    });

    v_flex()
        .size_full()
        .gap_1()
        .child(
            h_flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h(px(PLOT_HEIGHT))
                        .child(chart)
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
                )
                .child(
                    div()
                        .relative()
                        .w(px(PRICE_AXIS_WIDTH))
                        .h(px(PLOT_HEIGHT))
                        .flex_shrink_0()
                        .children(labels),
                ),
        )
        .child(
            h_flex()
                .justify_between()
                .pr(px(PRICE_AXIS_WIDTH + 8.))
                .children(times),
        )
        .into_any_element()
}

#[cfg(test)]
impl Markets {
    /// Replace what the feed has reported, for tests that have no venue.
    pub fn set_snapshot(&mut self, snapshot: MarketSnapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        cx.notify();
    }

    pub fn watched(&self) -> Vec<String> {
        self.watchlist
            .as_ref()
            .map(|w| w.markets().iter().map(|m| m.to_string()).collect())
            .unwrap_or_default()
    }

    pub fn selections(&self) -> String {
        format!(
            "period={} colour={}",
            self.period.label(),
            self.colour.label()
        )
    }

    /// The tiles in the order they were last drawn.
    pub fn drawn(&self) -> Vec<String> {
        self.order.iter().map(|m| m.to_string()).collect()
    }

    /// Pretend the settled order is old enough to be worked out again.
    pub fn age_order(&mut self) {
        self.ordered_at = Some(Instant::now() - SETTLE);
    }

    /// The market the panel shows.
    pub fn selected_market(&self) -> Option<String> {
        self.selected().map(|m| m.to_string())
    }

    /// The tile the pointer rests on, which shows its remove button.
    pub fn hovered_market(&self) -> Option<String> {
        self.hovered.as_ref().map(|m| m.to_string())
    }

    pub fn picker_is_open(&self) -> bool {
        self.picker_open
    }

    /// Put the keyboard focus on a market's tile.
    pub fn focus_market(&self, market: &str, window: &mut Window, cx: &mut Context<Self>) {
        let handle = &self.focus[&MarketId::new(market)];
        window.focus(handle, cx);
        cx.notify();
    }

    /// Type into the add-market search field.
    pub fn set_search(&self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
        cx.notify();
    }
}
