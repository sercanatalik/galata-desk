//! The watched markets, kept live: figures from the stream, history from
//! REST, joined into one snapshot per market.
//!
//! Commands go in (watch, unwatch, change the period); snapshots come out on
//! a watch channel, at most twice a second. Dropping the feed stops all of it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rust_decimal::Decimal;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::candles::{Series, Stats, stats};
use super::history::{Budget, Loaded, Loader, Request};
use super::ws::{Event, Sub, Subscriptions, WsClient};
use super::{Interval, MarketId, MarketPeriod};
use crate::venue::{AssetCtx, Info, PerpMeta, Transport, VenueError};

/// A market's figures as of now.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    /// The mid price, or the mark where the venue reports no mid.
    pub last: Decimal,
    pub mark: Decimal,
    pub oracle: Decimal,
    /// Mark over oracle, as a fraction.
    pub premium: Option<Decimal>,
    /// The hourly funding rate, as a fraction.
    pub funding: Decimal,
    /// 24-hour volume in USD.
    pub volume_usd: Decimal,
    /// Open interest in USD: the venue's figure in the asset, at the mark.
    pub open_interest_usd: Decimal,
}

impl From<&AssetCtx> for Live {
    fn from(ctx: &AssetCtx) -> Self {
        Live {
            last: ctx.mid_px.unwrap_or(ctx.mark_px),
            mark: ctx.mark_px,
            oracle: ctx.oracle_px,
            premium: ctx.premium,
            funding: ctx.funding,
            volume_usd: ctx.day_ntl_vlm,
            open_interest_usd: ctx.open_interest * ctx.mark_px,
        }
    }
}

/// One watched market, as much of it as has arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub market: MarketId,
    /// `None` until the stream's first message for the market.
    pub live: Option<Live>,
    /// `None` until the market's history has loaded.
    pub stats: Option<Stats>,
    pub max_leverage: Option<u32>,
    /// The venue's list of markets is known and this one is not on it.
    pub unavailable: bool,
}

/// Whether the stream is up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Connection {
    /// Not connected yet since the feed started.
    Connecting,
    Live,
    /// Was live; lost at `since`. Reconnecting.
    Down {
        since: SystemTime,
    },
}

/// A market the venue lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub market: MarketId,
    pub max_leverage: u32,
}

/// The venue's list of perpetual markets, for the add action.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Listing {
    #[default]
    Loading,
    /// Main dex first, then builder dexes, each in the venue's order.
    Ready(Arc<Vec<Listed>>),
    Failed,
}

/// Everything the Markets section shows, as last known.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketSnapshot {
    pub period: MarketPeriod,
    /// In watchlist order.
    pub tiles: Vec<Tile>,
    pub connection: Connection,
    /// When the stream last said anything.
    pub last_message: Option<SystemTime>,
    pub listing: Listing,
}

impl Default for MarketSnapshot {
    fn default() -> Self {
        Self {
            period: MarketPeriod::default(),
            tiles: Vec::new(),
            connection: Connection::Connecting,
            last_message: None,
            listing: Listing::Loading,
        }
    }
}

enum Command {
    Watch(MarketId),
    Unwatch(MarketId),
    SetPeriod(MarketPeriod),
    RefreshListing,
}

/// The time in milliseconds since the epoch. Tests supply their own.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default()
    })
}

/// A running feed. Dropping it stops the stream and the loading.
pub struct MarketFeed {
    snapshots: watch::Receiver<MarketSnapshot>,
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

/// How often snapshots are published at most.
const PUBLISH_EVERY: Duration = Duration::from_millis(500);
/// How often the rolling window is re-read even when nothing arrived.
const ROLL_EVERY: Duration = Duration::from_secs(5);
/// How often a market still without history is asked for again.
const RETRY_HISTORY_EVERY: Duration = Duration::from_secs(30);
/// How old the list of markets may be before the add action reloads it.
const LISTING_FRESH_FOR: Duration = Duration::from_secs(600);
/// How often a list of markets that failed to load is tried again.
const RETRY_LISTING_EVERY: Duration = Duration::from_secs(15);

impl MarketFeed {
    /// Start against the live venue on the current Tokio runtime.
    pub fn live<T: Transport + Clone>(
        transport: T,
        ws_url: impl Into<String>,
        period: MarketPeriod,
        watched: Vec<MarketId>,
    ) -> Self {
        let (client, events) = WsClient::start(ws_url);
        Self::start(
            transport,
            client,
            events,
            system_clock(),
            Budget::for_history(),
            period,
            watched,
        )
    }

    /// Start with the stream, clock and budget given.
    pub fn start<T: Transport + Clone, S: Subscriptions>(
        transport: T,
        subscriptions: S,
        events: mpsc::UnboundedReceiver<Event>,
        clock: Clock,
        budget: Budget,
        period: MarketPeriod,
        watched: Vec<MarketId>,
    ) -> Self {
        let (tx, snapshots) = watch::channel(MarketSnapshot {
            period,
            ..MarketSnapshot::default()
        });
        let (commands, rx) = mpsc::unbounded_channel();
        let info = Info::new(transport);
        let (loader, loaded) = Loader::start(info.clone(), budget);
        let state = State {
            info,
            subscriptions,
            loader,
            clock,
            period,
            watched: Vec::new(),
            active: HashSet::new(),
            ctx: HashMap::new(),
            series: HashMap::new(),
            requested: HashSet::new(),
            connection: Connection::Connecting,
            heard_at_ms: None,
            last_message: None,
            listing: Listing::Loading,
            listing_at: None,
            leverage: HashMap::new(),
        };
        let task = tokio::spawn(run(state, watched, tx, rx, events, loaded));
        Self {
            snapshots,
            commands,
            task,
        }
    }

    /// A receiver that always holds the latest snapshot.
    pub fn snapshots(&self) -> watch::Receiver<MarketSnapshot> {
        self.snapshots.clone()
    }

    pub fn watch(&self, market: MarketId) {
        let _ = self.commands.send(Command::Watch(market));
    }

    pub fn unwatch(&self, market: MarketId) {
        let _ = self.commands.send(Command::Unwatch(market));
    }

    pub fn set_period(&self, period: MarketPeriod) {
        let _ = self.commands.send(Command::SetPeriod(period));
    }

    /// Reload the list of markets if it failed or has grown old.
    pub fn refresh_listing(&self) {
        let _ = self.commands.send(Command::RefreshListing);
    }
}

impl Drop for MarketFeed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct State<T, S> {
    info: Info<T>,
    subscriptions: S,
    loader: Loader,
    clock: Clock,
    period: MarketPeriod,
    watched: Vec<MarketId>,
    /// Watched markets the venue is known to list, and so subscribed to. The
    /// venue closes the whole connection on a subscription to a market it does
    /// not list, so nothing is subscribed before the list says it may be.
    active: HashSet<MarketId>,
    ctx: HashMap<MarketId, AssetCtx>,
    /// Candles by market and interval, kept for the session.
    series: HashMap<(MarketId, Interval), Series>,
    /// History asked for and not yet answered.
    requested: HashSet<(MarketId, Interval)>,
    connection: Connection,
    /// When the stream last said anything, by the feed's clock.
    heard_at_ms: Option<u64>,
    last_message: Option<SystemTime>,
    listing: Listing,
    listing_at: Option<Instant>,
    leverage: HashMap<MarketId, u32>,
}

impl<T: Transport + Clone, S: Subscriptions> State<T, S> {
    fn interval(&self) -> Interval {
        self.period.interval()
    }

    /// Ask for a market's history at the current interval, unless it is
    /// already here and current, or already asked for. `force` asks even when
    /// a series is loaded: after an outage it has a hole in it.
    fn want_history(&mut self, market: &MarketId, force: bool) {
        let interval = self.interval();
        let key = (market.clone(), interval);
        if self.requested.contains(&key) {
            return;
        }
        let now = (self.clock)();
        if !force && let Some(series) = self.series.get(&key) {
            // A series is current while its newest candle is recent. One that
            // was left while another period was shown has gone stale.
            let newest = series.window(now, 2 * interval.millis()).last().is_some();
            if series.is_loaded() && newest {
                return;
            }
        }
        self.requested.insert(key);
        self.loader.request(Request {
            market: market.clone(),
            interval,
            // One candle more than the window, so its oldest candle is whole.
            from_ms: now.saturating_sub(self.period.millis() + interval.millis()),
            to_ms: now,
        });
    }

    fn watch(&mut self, market: MarketId) {
        if self.watched.contains(&market) {
            return;
        }
        self.watched.push(market.clone());
        self.activate(&market);
    }

    /// Subscribe to a watched market and load its history, once the venue's
    /// list shows it is there to subscribe to.
    fn activate(&mut self, market: &MarketId) {
        if self.active.contains(market) || !self.leverage.contains_key(market) {
            return;
        }
        self.active.insert(market.clone());
        self.subscriptions.subscribe(Sub::Ctx(market.clone()));
        self.subscriptions
            .subscribe(Sub::Candles(market.clone(), self.interval()));
        self.want_history(market, false);
    }

    fn unwatch(&mut self, market: &MarketId) {
        if !self.watched.contains(market) {
            return;
        }
        self.watched.retain(|m| m != market);
        if self.active.remove(market) {
            self.subscriptions.unsubscribe(Sub::Ctx(market.clone()));
            self.subscriptions
                .unsubscribe(Sub::Candles(market.clone(), self.interval()));
        }
        self.ctx.remove(market);
        self.series.retain(|(m, _), _| m != market);
        self.requested.retain(|(m, _)| m != market);
    }

    fn set_period(&mut self, period: MarketPeriod) {
        if period == self.period {
            return;
        }
        let old = self.interval();
        self.period = period;
        let new = self.interval();
        // History still queued for the old period is no longer wanted.
        self.loader.keep_only(new);
        self.requested.retain(|(_, interval)| *interval == new);
        for market in self.active_in_order() {
            self.subscriptions
                .unsubscribe(Sub::Candles(market.clone(), old));
            self.subscriptions
                .subscribe(Sub::Candles(market.clone(), new));
            self.want_history(&market, false);
        }
    }

    /// The subscribed markets, in watchlist order.
    fn active_in_order(&self) -> Vec<MarketId> {
        self.watched
            .iter()
            .filter(|m| self.active.contains(*m))
            .cloned()
            .collect()
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Connected => {
                self.connection = Connection::Live;
            }
            Event::Disconnected => {
                if self.connection == Connection::Live {
                    self.connection = Connection::Down {
                        since: SystemTime::now(),
                    };
                }
            }
            Event::Ctx(market, ctx) => {
                self.heard();
                if self.watched.contains(&market) {
                    self.ctx.insert(market, ctx);
                }
            }
            Event::Candle(market, interval, candle) => {
                self.heard();
                if self.watched.contains(&market) {
                    self.series
                        .entry((market, interval))
                        .or_default()
                        .push_live(candle);
                }
            }
        }
    }

    /// Note that the stream said something, and mend history if it had been
    /// silent for too long.
    ///
    /// A silence shorter than one candle loses nothing: the stream sends the
    /// whole current candle. A longer one leaves a hole in every series. The
    /// silence is measured from the last message, not from when a dropped
    /// connection was noticed: a machine that slept notices only on waking,
    /// and by then the hole is already there.
    fn heard(&mut self) {
        let now = (self.clock)();
        if let Some(last) = self.heard_at_ms.replace(now)
            && now.saturating_sub(last) > self.interval().millis()
        {
            for market in self.active_in_order() {
                self.want_history(&market, true);
            }
        }
        self.last_message = Some(SystemTime::now());
    }

    fn on_loaded(&mut self, loaded: Loaded) {
        let key = (loaded.market, loaded.interval);
        self.requested.remove(&key);
        if !self.watched.contains(&key.0) {
            return;
        }
        // A failed load leaves the tile on placeholders; it is asked again later.
        if let Ok(candles) = loaded.candles {
            self.series.entry(key).or_default().set_history(candles);
        }
    }

    /// Ask again for any watched market still without history.
    fn retry_missing_history(&mut self) {
        for market in self.active_in_order() {
            self.want_history(&market, false);
        }
    }

    fn on_listing(&mut self, answer: Result<Vec<PerpMeta>, VenueError>) {
        match answer {
            Ok(metas) => {
                let listed: Vec<Listed> = metas
                    .into_iter()
                    .flat_map(|meta| meta.universe)
                    .filter(|market| !market.is_delisted)
                    .map(|market| Listed {
                        market: MarketId::new(market.name),
                        max_leverage: market.max_leverage,
                    })
                    .collect();
                self.leverage = listed
                    .iter()
                    .map(|l| (l.market.clone(), l.max_leverage))
                    .collect();
                self.listing = Listing::Ready(Arc::new(listed));
                self.listing_at = Some(Instant::now());
                // Now it is known which watched markets can be subscribed to.
                for market in self.watched.clone() {
                    self.activate(&market);
                }
            }
            // A list already in hand is better than none.
            Err(_) if matches!(self.listing, Listing::Ready(_)) => {}
            Err(_) => self.listing = Listing::Failed,
        }
    }

    fn listing_is_stale(&self) -> bool {
        match (&self.listing, self.listing_at) {
            (Listing::Ready(_), Some(at)) => at.elapsed() > LISTING_FRESH_FOR,
            (Listing::Loading, _) => false,
            _ => true,
        }
    }

    fn snapshot(&mut self) -> MarketSnapshot {
        let now = (self.clock)();
        let interval = self.interval();
        let period = self.period;
        let listed = matches!(self.listing, Listing::Ready(_));
        // Nothing older than two windows is ever asked for again.
        for series in self.series.values_mut() {
            series.trim(now, 2 * MarketPeriod::Week.millis());
        }
        let tiles = self
            .watched
            .iter()
            .map(|market| {
                let live = self.ctx.get(market).map(Live::from);
                let stats = self
                    .series
                    .get(&(market.clone(), interval))
                    .filter(|series| series.is_loaded())
                    .and_then(|series| {
                        stats(
                            series.window(now, period.millis()),
                            live.as_ref().map(|l| l.last),
                        )
                    });
                Tile {
                    market: market.clone(),
                    live,
                    stats,
                    max_leverage: self.leverage.get(market).copied(),
                    unavailable: listed && !self.leverage.contains_key(market),
                }
            })
            .collect();
        MarketSnapshot {
            period,
            tiles,
            connection: self.connection,
            last_message: self.last_message,
            listing: self.listing.clone(),
        }
    }
}

async fn run<T: Transport + Clone, S: Subscriptions>(
    mut state: State<T, S>,
    watched: Vec<MarketId>,
    tx: watch::Sender<MarketSnapshot>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut events: mpsc::UnboundedReceiver<Event>,
    mut loaded: mpsc::UnboundedReceiver<Loaded>,
) {
    for market in watched {
        state.watch(market);
    }

    // The list of markets is fetched beside everything else.
    let (listing_tx, mut listing_rx) = mpsc::unbounded_channel();
    let fetch_listing = |info: &Info<T>| {
        let info = info.clone();
        let tx = listing_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(info.all_perp_metas().await);
        });
    };
    fetch_listing(&state.info);

    let mut publish = tokio::time::interval(PUBLISH_EVERY);
    publish.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut dirty = true;
    let mut rolled = Instant::now();
    let mut retried = Instant::now();
    let mut listing_tried = Instant::now();

    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(Command::Watch(market)) => state.watch(market),
                    Some(Command::Unwatch(market)) => state.unwatch(&market),
                    Some(Command::SetPeriod(period)) => state.set_period(period),
                    Some(Command::RefreshListing) => {
                        if state.listing_is_stale() {
                            if !matches!(state.listing, Listing::Ready(_)) {
                                state.listing = Listing::Loading;
                            }
                            listing_tried = Instant::now();
                            fetch_listing(&state.info);
                        }
                    }
                    // The feed was dropped.
                    None => return,
                }
                dirty = true;
            }
            event = events.recv() => match event {
                Some(event) => {
                    state.on_event(event);
                    dirty = true;
                }
                None => return,
            },
            Some(answer) = loaded.recv() => {
                state.on_loaded(answer);
                dirty = true;
            }
            Some(answer) = listing_rx.recv() => {
                state.on_listing(answer);
                dirty = true;
            }
            _ = publish.tick() => {
                if retried.elapsed() >= RETRY_HISTORY_EVERY {
                    retried = Instant::now();
                    state.retry_missing_history();
                }
                // Without the list nothing can be subscribed to, so a failed
                // load is tried again without being asked.
                if state.listing == Listing::Failed && listing_tried.elapsed() >= RETRY_LISTING_EVERY {
                    listing_tried = Instant::now();
                    fetch_listing(&state.info);
                }
                // The window rolls even when the market is silent.
                if rolled.elapsed() >= ROLL_EVERY {
                    dirty = true;
                }
                if dirty {
                    dirty = false;
                    rolled = Instant::now();
                    let snapshot = state.snapshot();
                    tx.send_if_modified(|current| {
                        let changed = *current != snapshot;
                        if changed {
                            *current = snapshot;
                        }
                        changed
                    });
                }
            }
        }
    }
}
