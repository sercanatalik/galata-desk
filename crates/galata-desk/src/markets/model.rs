//! What every Markets panel shares: the feed and what it last reported, the
//! watchlist, the period, colour and sort the tiles use, what each market is,
//! and the window's watcher connection. Each panel keeps only its own state
//! (its market, its pointer, what it has open) and reads the rest from here.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use galata_desk_core::market::feed::{Connection, MarketFeed, MarketSnapshot, Tile};
use galata_desk_core::market::pnl::{self, MarketPnl};
use galata_desk_core::market::sort::{self, SortBy};
use galata_desk_core::market::watchlist::{AddError, Watchlist};
use galata_desk_core::market::{MarketId, MarketPeriod};
use galata_desk_core::model::PositionRow;
#[cfg(feature = "shot")]
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::venue::Http;
use gpui_kit::{App, Context, Entity, SharedString, Subscription, Task};

use super::CallLevel;
use crate::Core;
use crate::market_about::AboutBook;
use crate::market_tile::ColourBy;
use crate::portfolio::Portfolio;

/// How long a sorted order is held before it is worked out again.
pub(super) const SETTLE: Duration = Duration::from_secs(5);

/// How long to wait for a first connection before saying it has not come.
pub(super) const PATIENCE: Duration = Duration::from_secs(5);

/// Which market a panel shows.
pub enum MarketSource {
    /// The watchlist's selection: the windowed layout's panels.
    Watchlist,
    /// The panel's own: a workspace panel.
    #[allow(dead_code, reason = "the workspace's panels use it")]
    Own(Option<MarketId>),
}

impl MarketSource {
    /// The market a panel with this source shows.
    pub fn market(&self, model: &Entity<MarketsModel>, cx: &App) -> Option<MarketId> {
        match self {
            Self::Watchlist => model.read(cx).selected().cloned(),
            Self::Own(market) => market.clone(),
        }
    }

    /// Show `market`, for a panel that keeps its own. Whether it changed.
    #[allow(dead_code, reason = "the workspace's panels use it")]
    pub fn set(&mut self, market: Option<MarketId>) -> bool {
        match self {
            Self::Own(own) if *own != market => {
                *own = market;
                true
            }
            _ => false,
        }
    }
}

pub struct MarketsModel {
    pub(super) portfolio: Entity<Portfolio>,
    pub(super) feed: MarketFeed,
    pub(super) snapshot: MarketSnapshot,
    /// `None` until the default list can be decided, which needs the
    /// account's positions.
    pub(super) watchlist: Option<Watchlist>,
    pub(super) period: MarketPeriod,
    pub(super) colour: ColourBy,
    /// What the tiles are ordered by. Only the drawing order: the watchlist
    /// keeps its own.
    pub(super) sort: SortBy,
    /// The order the tiles were last drawn in, and when it was worked out.
    /// It is held for a few seconds so tiles do not jump on every tick.
    pub(super) order: Vec<MarketId>,
    pub(super) ordered_at: Option<Instant>,
    /// Work the order out again at the next draw, whatever its age.
    pub(super) reorder: bool,
    /// Something the user should know that is not about the venue.
    pub(super) warning: Option<SharedString>,
    /// What each watched market is: company, kind, deployer, oracle.
    pub(super) about: AboutBook,
    pub(super) opened_at: Instant,
    /// The window's connection to galata-watcher, while it has one: its
    /// armed call levels are drawn on the chart, and new ones added from the
    /// chart and the tiles.
    pub(super) watcher: Option<Entity<crate::watcher_link::WatcherLink>>,
    _watcher_changes: Option<Subscription>,
    _watch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// What the account holds and has made, as the tiles show it.
pub(super) struct Account {
    /// `None` until the positions have first arrived.
    pub(super) positions: Option<Vec<PositionRow>>,
    /// Each market's PNL. `None` until the walk through the account's fills
    /// has finished.
    pub(super) pnl: Option<HashMap<MarketId, MarketPnl>>,
    /// When older fills may exist, the earliest one counted.
    pub(super) reaches_from: Option<u64>,
}

impl Account {
    pub(super) fn position(&self, market: &MarketId) -> Option<Option<&PositionRow>> {
        self.positions.as_ref().map(|all| {
            all.iter()
                .find(|p| p.dex == market.dex() && p.market == market.ticker() && !p.size.is_zero())
        })
    }

    pub(super) fn pnl(&self, market: &MarketId) -> Option<MarketPnl> {
        self.pnl
            .as_ref()
            .map(|all| all.get(market).copied().unwrap_or_default())
    }
}

impl MarketsModel {
    pub fn new(portfolio: Entity<Portfolio>, cx: &mut Context<Self>) -> Self {
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
        // so there the model does not listen to it.
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

        // The portfolio's positions decide the default list, and the held marks.
        let subscriptions = vec![cx.observe(&portfolio, |this, _, cx| {
            this.decide_default(cx);
            cx.notify();
        })];

        // Redraw once patience runs out, so "cannot reach the venue" can show.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(PATIENCE + Duration::from_millis(100))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();

        let mut this = Self {
            portfolio,
            feed,
            snapshot: MarketSnapshot::default(),
            watchlist: stored,
            period: MarketPeriod::default(),
            colour: ColourBy::default(),
            sort: SortBy::default(),
            order: Vec::new(),
            ordered_at: None,
            reorder: false,
            warning: None,
            about: AboutBook::default(),
            opened_at: Instant::now(),
            watcher: None,
            _watcher_changes: None,
            _watch: watch,
            _subscriptions: subscriptions,
        };
        this.decide_default(cx);
        this
    }

    /// Start the feed on the selected market. Called once the shell has
    /// applied any screenshot state.
    pub(super) fn start(&mut self) {
        self.feed
            .select(self.watchlist.as_ref().and_then(|w| w.selected().cloned()));
    }

    /// Use this connection to the watcher, or none.
    pub(super) fn set_watcher(
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
    pub(super) fn calls_in(&self, market: &MarketId, cx: &App) -> (Vec<CallLevel>, bool) {
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

    /// Ask the venue what each watched market is, once per session. A main
    /// dex market has nothing to ask: it is crypto, its oracle the
    /// validators'. A failed answer is asked again a minute later.
    pub(super) fn want_about(&mut self, cx: &mut Context<Self>) {
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

    /// The watchlist's selected market: the one the windowed panel shows.
    pub(super) fn selected(&self) -> Option<&MarketId> {
        self.watchlist.as_ref()?.selected()
    }

    /// Make `market` the watchlist's selection. Whether it changed.
    pub(super) fn select(&mut self, market: &MarketId, cx: &mut Context<Self>) -> bool {
        let Some(list) = &mut self.watchlist else {
            return false;
        };
        let changed = list.selected() != Some(market) && list.select(market);
        if changed {
            self.feed.select(Some(market.clone()));
            self.save(cx);
        }
        cx.notify();
        changed
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(list) = &self.watchlist
            && let Err(error) = Core::get(cx).store.save_watchlist(list)
        {
            self.warning = Some(format!("The watchlist could not be saved: {error}").into());
        }
    }

    pub(super) fn add(&mut self, market: MarketId, cx: &mut Context<Self>) {
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

    /// Stop watching `market`. Whether it was watched.
    pub(super) fn remove(&mut self, market: &MarketId, cx: &mut Context<Self>) -> bool {
        let Some(list) = &mut self.watchlist else {
            return false;
        };
        let removed = list.remove(market);
        if removed {
            // Removing the selected market selects its neighbour.
            self.feed.select(list.selected().cloned());
            self.feed.unwatch(market.clone());
            self.save(cx);
        }
        cx.notify();
        removed
    }

    pub(super) fn set_period(&mut self, period: MarketPeriod, cx: &mut Context<Self>) {
        self.period = period;
        self.feed.set_period(period);
        // Change and volatility are now another period's: order by them at once.
        self.reorder = true;
        cx.notify();
    }

    pub(super) fn set_sort(&mut self, sort: SortBy, cx: &mut Context<Self>) {
        self.sort = sort;
        self.reorder = true;
        cx.notify();
    }

    pub(super) fn set_colour(&mut self, colour: ColourBy, cx: &mut Context<Self>) {
        self.colour = colour;
        cx.notify();
    }

    pub(super) fn refresh_listing(&mut self, cx: &mut Context<Self>) {
        self.feed.refresh_listing();
        cx.notify();
    }

    /// The order to draw `tiles` in, as positions into them; they are in
    /// watchlist order.
    ///
    /// A sort by a moving figure is settled: worked out again only every few
    /// seconds, or at once when the sort, the period or the set of markets
    /// changes.
    pub(super) fn settled_order(&mut self, tiles: &[Tile]) -> Vec<usize> {
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

    /// Every watched market's tile, in watchlist order: one the feed has not
    /// reported yet is all placeholders. `None` until the list is decided.
    pub(super) fn watched_tiles(&self) -> Option<Vec<Tile>> {
        let list = self.watchlist.as_ref()?;
        Some(
            list.markets()
                .iter()
                .map(|market| self.tile_of(market))
                .collect(),
        )
    }

    /// What the feed last reported for `market`, or a blank tile.
    pub(super) fn tile_of(&self, market: &MarketId) -> Tile {
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
    }

    /// The last price of `market`, while the feed has one.
    #[cfg(feature = "shot")]
    pub(super) fn last_price(&self, market: &MarketId) -> Option<Decimal> {
        self.snapshot
            .tiles
            .iter()
            .find(|t| &t.market == market)
            .and_then(|t| t.live.as_ref())
            .map(|l| l.last)
    }

    /// The account's positions and per-market PNL, for the tiles.
    pub(super) fn account(&self, cx: &App) -> Account {
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
    pub(super) fn unreachable(&self) -> bool {
        self.snapshot.connection == Connection::Connecting && self.opened_at.elapsed() >= PATIENCE
    }
}
