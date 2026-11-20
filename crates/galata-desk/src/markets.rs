//! The Markets section: the watched markets as a grid of heatmap tiles.
//! Read-only: it subscribes to public market data and nothing else.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Local};
use galata_desk_core::format as f;
use galata_desk_core::market::candles::Candle;
use galata_desk_core::market::candles::Placement;
use galata_desk_core::market::feed::{
    Connection, Listed, Listing, Live, MarketFeed, MarketSnapshot, Tile,
};
use galata_desk_core::market::fills::{self, CandleFills, FillSide, SideFills};
use galata_desk_core::market::pnl::{self, MarketPnl, WindowPnl};
use galata_desk_core::market::sort::{self, SortBy};
use galata_desk_core::market::watchlist::{AddError, LIMIT, Watchlist};
use galata_desk_core::market::{Interval, MarketId, MarketPeriod};
use galata_desk_core::model::{OrderRow, PositionRow};
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::rust_decimal::prelude::ToPrimitive;
use galata_desk_core::trade::sign::Tpsl;
use galata_desk_core::venue::Http;
use galata_desk_core::venue::Side;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
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
    AnyElement, Context, Entity, FocusHandle, KeyDownEvent, SharedString, Subscription, Task,
    TestSupportExt, Window, deferred, div, px,
};

use crate::Core;
use crate::market_about::AboutBook;
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
/// the left and the chart taking the rest.
// Tall enough for the detail's name line, figures and the Buy and Sell
// buttons under them.
const PANEL_HEIGHT: f32 = 406.;
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
    /// What the chart area last drew, for tests: its state, how many buy and
    /// sell marks, and the "shown since" time.
    drawn_chart: std::cell::Cell<(&'static str, usize, usize, Option<u64>)>,
    /// One focus handle per tile, so the keyboard can reach each.
    focus: HashMap<MarketId, FocusHandle>,
    picker_open: bool,
    search: Entity<InputState>,
    /// Something the user should know that is not about the venue.
    warning: Option<SharedString>,
    /// What each watched market is: company, kind, deployer, oracle.
    about: AboutBook,
    opened_at: Instant,
    /// The window's connection to galata-watcher, while it has one: its
    /// armed call levels are drawn on the chart, and new ones added from the
    /// chart and the tiles.
    watcher: Option<Entity<crate::watcher_link::WatcherLink>>,
    _watcher_changes: Option<Subscription>,
    /// Where the pointer is over the chart, for "Alert at <price>".
    plot: PlotPointer,
    /// Why the last call level added from Markets was not saved.
    call_problem: Option<SharedString>,
    /// The "Alert for <market>…" form, open under that market's tile.
    tile_form: Option<(MarketId, Entity<crate::level_form::LevelForm>, Subscription)>,
    _watch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// What the account holds and has made, as the tiles show it.
struct Account {
    /// `None` until the positions have first arrived.
    positions: Option<Vec<PositionRow>>,
    /// Each market's PNL. `None` until the walk through the account's fills
    /// has finished.
    pnl: Option<HashMap<MarketId, MarketPnl>>,
    /// When older fills may exist, the earliest one counted.
    reaches_from: Option<u64>,
}

impl Account {
    fn position(&self, market: &MarketId) -> Option<Option<&PositionRow>> {
        self.positions.as_ref().map(|all| {
            all.iter()
                .find(|p| p.dex == market.dex() && p.market == market.ticker() && !p.size.is_zero())
        })
    }

    fn pnl(&self, market: &MarketId) -> Option<MarketPnl> {
        self.pnl
            .as_ref()
            .map(|all| all.get(market).copied().unwrap_or_default())
    }
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
            drawn_chart: std::cell::Cell::new(("none", 0, 0, None)),
            focus: HashMap::new(),
            picker_open: false,
            search,
            warning: None,
            about: AboutBook::default(),
            opened_at: Instant::now(),
            watcher: None,
            _watcher_changes: None,
            plot: PlotPointer::default(),
            call_problem: None,
            tile_form: None,
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

    /// Use this connection to the watcher, or none. The window calls this
    /// whenever its connection changes.
    pub fn set_watcher(
        &mut self,
        link: Option<Entity<crate::watcher_link::WatcherLink>>,
        cx: &mut Context<Self>,
    ) {
        self._watcher_changes = link.as_ref().map(|l| cx.observe(l, |_, _, cx| cx.notify()));
        self.watcher = link;
        cx.notify();
    }

    /// The watcher's armed levels in `market`, and whether levels can be
    /// added: only while it answers, is configured, and watches the network
    /// shown here.
    fn calls_in(&self, market: &MarketId, cx: &gpui_kit::App) -> (Vec<CallLevel>, bool) {
        let Some(link) = &self.watcher else {
            return (Vec::new(), false);
        };
        let snapshot = link.read(cx).snapshot();
        let Some(config) = snapshot.config.config() else {
            return (Vec::new(), false);
        };
        if config.network != Core::get(cx).network {
            return (Vec::new(), false);
        }
        let levels = config
            .armed_in(market.coin())
            .map(|l| CallLevel {
                price: l.price,
                note: l.note.clone(),
            })
            .collect();
        (levels, snapshot.editable().is_some())
    }

    /// Add an armed level in `market` at `price`, its direction from `last`.
    pub fn add_call(
        &mut self,
        market: String,
        price: Decimal,
        last: Option<Decimal>,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = self.watcher.clone() else {
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

    /// Open the "Alert for <market>…" form under `market`'s tile, its price
    /// set to `last`. It closes when the level is saved or the user cancels.
    pub fn open_tile_form(
        &mut self,
        market: MarketId,
        last: Option<Decimal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = self.watcher.clone() else {
            return;
        };
        let coin = market.coin().to_string();
        let form = cx.new(|cx| {
            crate::level_form::LevelForm::for_market("tile-alert", link, coin, last, window, cx)
        });
        let closing = cx.subscribe(
            &form,
            |this, _, _: &crate::level_form::LevelFormEvent, cx| {
                this.tile_form = None;
                cx.notify();
            },
        );
        self.tile_form = Some((market, form, closing));
        cx.notify();
    }

    /// Ask the venue what each watched market is, once per session. A main
    /// dex market has nothing to ask: it is crypto, its oracle the
    /// validators'. A failed answer is asked again a minute later.
    fn want_about(&mut self, cx: &mut Context<Self>) {
        let markets: Vec<MarketId> = self
            .snapshot
            .tiles
            .iter()
            .map(|t| t.market.clone())
            .collect();
        for market in markets {
            if market.dex().is_empty() && self.about.get(&market).is_none() {
                self.about
                    .put(market, galata_desk_core::market::about::About::main_dex());
            }
        }
        // The headless tests do not reach the venue.
        if cfg!(test) {
            return;
        }
        let core = Core::get(cx);
        let runtime = core.runtime.clone();
        let info = galata_desk_core::venue::Info::new(Http::at(core.info_url()));
        if !self.about.dexes_asked {
            self.about.dexes_asked = true;
            let (runtime, info) = (runtime.clone(), info.clone());
            cx.spawn(async move |this, cx| {
                let answer = runtime.spawn(async move { info.dex_infos().await }).await;
                let _ = this.update(cx, |this, cx| match answer {
                    Ok(Ok(dexes)) => {
                        this.about.put_dexes(dexes);
                        cx.notify();
                    }
                    _ => this.retry_about(None, cx),
                });
            })
            .detach();
        }
        let wanted: Vec<MarketId> = self
            .snapshot
            .tiles
            .iter()
            .filter(|t| !t.unavailable && !t.market.dex().is_empty())
            .map(|t| t.market.clone())
            .filter(|m| self.about.get(m).is_none() && !self.about.asked.contains(m))
            .collect();
        for market in wanted {
            self.about.asked.insert(market.clone());
            let (runtime, info) = (runtime.clone(), info.clone());
            cx.spawn(async move |this, cx| {
                let coin = market.coin().to_string();
                let answer = runtime
                    .spawn(async move { info.perp_annotation(&coin).await })
                    .await;
                let _ = this.update(cx, |this, cx| match answer {
                    Ok(Ok(annotation)) => {
                        let about = galata_desk_core::market::about::About::of(
                            &annotation.unwrap_or_default(),
                            market.ticker(),
                        );
                        this.about.put(market, about);
                        cx.notify();
                    }
                    _ => this.retry_about(Some(market), cx),
                });
            })
            .detach();
        }
    }

    /// Forget that `market` (or, for `None`, the dexes) was asked for, a
    /// minute from now, so the next draw asks again.
    fn retry_about(&mut self, market: Option<MarketId>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(60))
                .await;
            let _ = this.update(cx, |this, cx| {
                match market {
                    Some(m) => {
                        this.about.asked.remove(&m);
                    }
                    None => this.about.dexes_asked = false,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `GALATA_DESK_SHOT_STATE=section=markets,mperiod=week,colour=funding,select=xyz:NVDA,picker=1`
    #[cfg(feature = "shot")]
    fn preselect(&mut self) {
        let state = std::env::var("GALATA_DESK_SHOT_STATE").unwrap_or_default();
        for (key, value) in state.split(',').filter_map(|kv| kv.split_once('=')) {
            match key {
                "about" => self.about.open_at_start = value == "1",
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

    /// The account's positions and per-market PNL, for the tiles.
    fn account(&self, cx: &Context<Self>) -> Account {
        let portfolio = self.portfolio.read(cx);
        let positions = portfolio.positions().map(<[PositionRow]>::to_vec);
        let walked = portfolio.fills().filter(|f| f.walked);
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Account {
            pnl: match (walked, &positions) {
                (Some(fills), Some(positions)) => Some(pnl::market_pnl(fills, positions, now_ms)),
                _ => None,
            },
            reaches_from: walked.and_then(|f| f.reaches_from()),
            positions,
        }
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

    fn legend(&self, account: &Account) -> AnyElement {
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
        account: &Account,
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
        let position = account.position(&market);
        let held = position.flatten().is_some();
        let by_funding = self.sort == SortBy::Funding;
        let headline = self
            .about
            .get(&market)
            .and_then(|a| a.headline())
            .map(str::to_string);

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
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(11.))
                    .text_color(gpui_kit::rgb(SOFT))
                    .children(headline.clone()),
            );

        let soft = gpui_kit::rgb(SOFT);
        let mut name = market.ticker().to_string();
        if let Some(h) = &headline {
            name.push_str(&format!(", {h}"));
        }
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
            let last = tile.live.as_ref().map(|l| l.last);
            let placement = stats.zip(last).map(|(s, last)| s.placement(last));
            let pnl = account.pnl(&market);
            let position_text = position.map(|p| match p {
                Some(p) => f::usd_k_signed(p.signed_value()),
                None => DASH.to_string(),
            });
            let window_text = |w: fn(&MarketPnl) -> Option<WindowPnl>| {
                pnl.as_ref().map(|p| match w(p) {
                    Some(w) if w.partial => format!("{}*", f::pnl_rounded(w.usd)),
                    Some(w) => f::pnl_rounded(w.usd),
                    None => DASH.to_string(),
                })
            };
            let d30 = window_text(|p| p.d30);
            let ytd = window_text(|p| p.ytd);
            let all = window_text(|p| p.all);

            // What a screen reader says for the tile.
            if let Some(live) = &tile.live {
                name.push_str(&format!(", last {}", f::price(live.last)));
            }
            if let Some(s) = stats {
                name.push_str(&format!(", {}", f::pct_signed_f(s.change, 2)));
            }
            if let Some(z) = placement.and_then(|p| p.z) {
                name.push_str(&format!(
                    ", {} standard deviations from the period average",
                    f::sigma(z).trim_end_matches('\u{3c3}')
                ));
            }
            let spoken = |t: &Option<String>| match t.as_deref() {
                Some(DASH) => "none".to_string(),
                Some(t) => t.trim_end_matches('*').to_string(),
                None => "loading".to_string(),
            };
            name.push_str(&format!(
                ", position {}, 30-day PNL {}, year-to-date PNL {}, all-time PNL {}",
                spoken(&position_text),
                spoken(&d30),
                spoken(&ytd),
                spoken(&all),
            ));

            let cell = |id: &'static str, label: &'static str, value: Option<String>| {
                h_flex()
                    .justify_between()
                    .gap_1()
                    .child(div().text_size(px(10.)).text_color(soft).child(label))
                    .child(match value {
                        Some(text) => div()
                            .id((id, ix))
                            .aria_label(text.clone())
                            .font_family(MONO)
                            .text_size(px(11.))
                            .when(text == DASH, |d| d.text_color(soft))
                            .child(text)
                            .test_support()
                            .into_any_element(),
                        None => pending(40., 11.),
                    })
            };

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
                .child(
                    h_flex()
                        .items_baseline()
                        .gap(px(6.))
                        .child(match &tile.live {
                            Some(live) => {
                                ui::figure(f::price(live.last), px(19.)).into_any_element()
                            }
                            None => pending(110., 22.),
                        })
                        .child(div().flex_1())
                        // Sorted by funding, the tile shows what it is sorted by.
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(soft)
                                .child(if by_funding { "Funding (1h)" } else { "Vol" }),
                        )
                        .child(match (by_funding, &tile.live, stats) {
                            (true, Some(live), _) => div()
                                .id(("tile-vol", ix))
                                .aria_label(f::funding_hourly(live.funding))
                                .font_family(MONO)
                                .text_size(px(10.5))
                                .text_color(soft)
                                .child(f::funding_hourly(live.funding))
                                .test_support()
                                .into_any_element(),
                            (false, _, Some(s)) => div()
                                .id(("tile-vol", ix))
                                .aria_label(f::volatility(s.volatility))
                                .font_family(MONO)
                                .text_size(px(10.5))
                                .text_color(soft)
                                .child(f::volatility(s.volatility))
                                .test_support()
                                .into_any_element(),
                            _ => pending(30., 11.),
                        }),
                )
                .child(match placement {
                    Some(at) => range_bar(ix, &at),
                    None => pending(TILE_MIN - 40., 10.),
                })
                .child(
                    div()
                        .grid()
                        .grid_cols(2)
                        .gap_x_3()
                        .gap_y(px(2.))
                        .pt(px(5.))
                        .border_t_1()
                        .border_color(Palette::text().opacity(0.13))
                        .child(cell("tile-pos", "Position", position_text.clone()))
                        .child(cell("tile-30d", "30D PNL", d30.clone()))
                        .child(cell("tile-ytd", "YTD PNL", ytd.clone()))
                        .child(cell("tile-all", "All-time", all.clone())),
                )
                .into_any_element()
        };

        let hover_market = market.clone();
        let click_market = market.clone();
        let key_market = market.clone();
        let tile_element = div()
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
            .aria_label(name)
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
            // The "Alert for <market>…" form, over what lies below the tile.
            .children(
                self.tile_form
                    .as_ref()
                    .filter(|(m, _, _)| *m == market)
                    .map(|(_, form, _)| {
                        deferred(
                            div()
                                .id("tile-alert-form")
                                .absolute()
                                .top(px(TILE_HEIGHT + 6.))
                                .left_0()
                                .w(px(640.))
                                .p_3()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(Palette::amber())
                                .bg(Palette::panel())
                                .shadow_lg()
                                .child(form.clone())
                                .test_support(),
                        )
                        .with_priority(2)
                    }),
            )
            .test_support();
        // Right-click: "Alert for <market>…", while the watcher can take one.
        if !self.calls_in(&market, cx).1 {
            return tile_element.into_any_element();
        }
        let markets = cx.entity().downgrade();
        let last = tile.live.as_ref().map(|l| l.last);
        let label = format!("Alert for {}\u{2026}", market.ticker());
        tile_element
            .context_menu(move |menu, _, _| {
                let (markets, market, label) = (markets.clone(), market.clone(), label.clone());
                menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let _ = markets.update(cx, |m, cx| {
                        m.open_tile_form(market.clone(), last, window, cx)
                    });
                }))
            })
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
            // The account's fills in this market, grouped into these candles.
            let (grouped, since) = match (self.portfolio.read(cx).fills(), candles) {
                (Some(all), Some(candles)) if !candles.is_empty() => (
                    fills::group(&all.rows, &market, candles, self.period.interval().millis()),
                    fills::shown_since(all, candles[0].open_ms),
                ),
                _ => (Vec::new(), None),
            };
            // The watcher's armed levels in this market, and the menu that
            // adds one at the pointer.
            let calls = {
                let (levels, can_add) = self.calls_in(&market, cx);
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
                                PopupMenuItem::new(format!("Alert at {}", f::price(price)))
                                    .on_click(move |_, _, cx| {
                                        let _ = markets.update(cx, |m, cx| {
                                            m.add_call(coin.clone(), price, last, cx)
                                        });
                                    }),
                            )
                        },
                    ) as CallMenu
                });
                Calls { levels, menu }
            };
            // The account's open orders in this market, as levels.
            let levels = self
                .portfolio
                .read(cx)
                .orders()
                .map(|all| order_levels(all, &market))
                .unwrap_or_default();
            h_flex()
                .size_full()
                .child(self.detail(
                    tile,
                    position.as_ref(),
                    (!tile.unavailable).then(|| self.trade_buttons(&market, cx)),
                ))
                .child(self.chart(
                    tile,
                    candles,
                    &grouped,
                    &levels,
                    calls,
                    since,
                    cx.reduce_motion(),
                ))
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

    /// Buy / Long and Sell / Short for the selected market: each opens the
    /// order ticket on that side. Trading's guards are explained in the
    /// ticket, so these are not disabled by them.
    fn trade_buttons(&self, market: &MarketId, cx: &mut Context<Self>) -> AnyElement {
        let side = |id: &'static str, label: &'static str, buy: bool, cx: &mut Context<Self>| {
            // The button's colour paints as a tint, with its words in the
            // full colour: blue for buying, orange for selling.
            let ground = if buy {
                Palette::gain()
            } else {
                Palette::loss()
            };
            let hover = ground.opacity(0.3);
            let market = market.clone();
            let name = format!("{} {}", if buy { "Buy" } else { "Sell" }, market.ticker());
            Button::new(id)
                .flex_1()
                .label(label)
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(ground)
                        .foreground(ground)
                        .hover(hover)
                        .active(hover),
                )
                .accessibility_label(name)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let market = market.clone();
                    this.portfolio.update(cx, |portfolio, cx| {
                        portfolio.open_ticket(market, buy, window, cx)
                    });
                }))
        };
        h_flex()
            .mt_auto()
            .pt_2()
            .gap_2()
            .child(side("trade-buy", "Buy / Long", true, cx))
            .child(side("trade-sell", "Sell / Short", false, cx))
            .into_any_element()
    }

    /// The panel's left side: everything the venue says about the market now,
    /// with the Buy and Sell buttons under it.
    fn detail(
        &self,
        tile: &Tile,
        position: Option<&PositionRow>,
        trade: Option<AnyElement>,
    ) -> AnyElement {
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
                    .child(self.about.button(market))
                    .child(div().flex_1())
                    .children(tile.max_leverage.map(|l| {
                        div()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(Palette::muted())
                            .child(format!("up to {l}x"))
                    })),
            )
            .children(self.about.get(market).and_then(|a| {
                let line = [a.name.clone(), a.category.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" \u{b7} ");
                (!line.is_empty()).then(|| {
                    div()
                        .id("market-about-line")
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(Palette::muted())
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .aria_label(line.clone())
                        .child(line)
                        .test_support()
                })
            }))
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
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .font_family(MONO)
                            .child(div().text_color(gpui_kit::rgb(SOFT)).child(signed_size(p)))
                            .child(f::pnl_rounded(p.signed_value())),
                    )
            }))
            .children(trade)
            .into_any_element()
    }

    /// The panel's right side: the market's candles over the period.
    #[allow(clippy::too_many_arguments)]
    fn chart(
        &self,
        tile: &Tile,
        candles: Option<&[Candle]>,
        grouped: &[CandleFills],
        levels: &[OrderLevel],
        calls: Calls,
        since: Option<u64>,
        // The system asks for reduced motion: placeholders hold still.
        still: bool,
    ) -> AnyElement {
        let interval = self.period.interval();
        // A period under an hour is too few candles to read, so the chart
        // shows the hour it ends; the heading says so.
        let shown = if self.period.chart_millis() > self.period.millis() {
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
            .child(shown)
            .child(div().flex_1())
            .children(since_note.map(|note| div().id("trades-since").child(note).test_support()));

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
            None if self.snapshot.chart_failed => (
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
                        self.period.label()
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
        self.want_about(cx);
        // A screenshot run's order ticket opens once the selected market has
        // a price.
        #[cfg(feature = "shot")]
        if let Some(market) = self.selected().cloned()
            && let Some(mid) = self
                .snapshot
                .tiles
                .iter()
                .find(|t| t.market == market)
                .and_then(|t| t.live.as_ref())
                .map(|l| l.last)
        {
            self.portfolio
                .update(cx, |p, cx| p.stage_ticket_shot(market, mid, window, cx));
        }
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
                let account = self.account(cx);
                let mut grid = div().grid().grid_cols(columns).gap(px(GAP));
                // Drawn in the sorted order; a tile's id stays its place in the
                // watchlist, so focus and Remove follow the market.
                for ix in self.settled_order(&tiles) {
                    grid = grid.child(self.tile(ix, &tiles[ix], &account, window, cx));
                }
                div()
                    .id("tiles")
                    .size_full()
                    .overflow_y_scroll()
                    .child(grid)
                    .test_support()
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .p(px(padding))
            .gap_3()
            .child(self.toolbar(cx))
            .child(self.legend(&self.account(cx)))
            .children(self.notices())
            // The selected market, above the tiles and always in view.
            .children(self.panel(cx))
            .child(div().flex_1().min_h_0().child(body))
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
#[derive(Clone, Default)]
struct PlotPointer {
    /// From the plot's top.
    y: std::rc::Rc<std::cell::Cell<Option<f32>>>,
    bounds: std::rc::Rc<std::cell::Cell<Option<gpui_kit::Bounds<gpui_kit::Pixels>>>>,
    /// Low, high, and a price at the venue's precision.
    range: std::rc::Rc<std::cell::Cell<Option<(f64, f64, Decimal)>>>,
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
    plot: &PlotPointer,
) -> AnyElement {
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
                .child(
                    div()
                        .relative()
                        .w(px(PRICE_AXIS_WIDTH))
                        .h(px(PLOT_HEIGHT))
                        .flex_shrink_0()
                        .children(labels)
                        .children(order_tags),
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

    /// What the chart area last drew: `loading`, `failed`, `short`, `chart`
    /// or `none`; how many `B` and `S` marks; and the "shown since" time.
    pub fn drawn_chart(&self) -> (&'static str, usize, usize, Option<u64>) {
        self.drawn_chart.get()
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

#[cfg(test)]
impl Markets {
    /// The "Alert for <market>…" form, while it is open.
    pub fn tile_form(&self) -> Option<Entity<crate::level_form::LevelForm>> {
        self.tile_form.as_ref().map(|(_, f, _)| f.clone())
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
