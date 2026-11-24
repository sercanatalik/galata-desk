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

use super::candles::{Candle, Series, Stats, chart, stats};
use super::history::{Loaded, Loader, Request, SharedBudget, shared_history_budget};
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

/// The selected market's candles over the period, for the market panel.
#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub market: MarketId,
    pub interval: Interval,
    /// Oldest first. The newest is the candle still forming, ending at the
    /// live last price: the same window the market's tile is computed from.
    pub candles: Vec<Candle>,
}

/// A chart a panel shows of its own: a market and a period. The period sets
/// the candle interval and how far back the chart reaches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChartKey {
    pub market: MarketId,
    pub period: MarketPeriod,
}

/// What a panel's own chart can show so far.
#[derive(Debug, Clone, PartialEq)]
pub enum ChartView {
    /// Its history has not loaded yet.
    Loading,
    /// The last attempt to load its history failed. It is tried again.
    Failed,
    Ready(Chart),
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
    /// The selected market's chart, once its history has loaded. `None` with
    /// nothing selected.
    pub chart: Option<Chart>,
    /// There is no chart because the last attempt to load the selected
    /// market's history failed. It is tried again; a success clears this.
    pub chart_failed: bool,
    /// Each chart named with `set_charts`, by its market and period.
    pub charts: HashMap<ChartKey, ChartView>,
}

impl Default for MarketSnapshot {
    fn default() -> Self {
        Self {
            period: MarketPeriod::default(),
            tiles: Vec::new(),
            connection: Connection::Connecting,
            last_message: None,
            listing: Listing::Loading,
            chart: None,
            chart_failed: false,
            charts: HashMap::new(),
        }
    }
}

enum Command {
    Watch(MarketId),
    Unwatch(MarketId),
    SetPeriod(MarketPeriod),
    Select(Option<MarketId>),
    SetCharts(HashSet<ChartKey>),
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
            shared_history_budget(),
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
        budget: impl Into<SharedBudget>,
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
            selected: None,
            charts: HashSet::new(),
            chart_subs: HashSet::new(),
            active: HashSet::new(),
            ctx: HashMap::new(),
            series: HashMap::new(),
            requested: HashSet::new(),
            failed: HashSet::new(),
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

    /// Name the market the panel shows; its candles are published with each
    /// snapshot. `None` publishes no chart.
    pub fn select(&self, market: Option<MarketId>) {
        let _ = self.commands.send(Command::Select(market));
    }

    /// Name every chart the panels show of their own, beside the selected
    /// market's. Each is published with each snapshot, in `charts`. A market
    /// charted here is streamed and loaded whether or not it is watched; two
    /// charts at one interval of one market share the candles. A chart left
    /// out of the next call is stopped.
    pub fn set_charts(&self, charts: impl IntoIterator<Item = ChartKey>) {
        let _ = self
            .commands
            .send(Command::SetCharts(charts.into_iter().collect()));
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
    /// The market the panel shows.
    selected: Option<MarketId>,
    /// The charts panels show of their own.
    charts: HashSet<ChartKey>,
    /// What is subscribed to for those charts alone: the watched markets'
    /// subscriptions do not count. A subscription is made once, so when a
    /// chart and the watchlist want the same one, the watchlist holds it.
    chart_subs: HashSet<Sub>,
    /// Watched markets the venue is known to list, and so subscribed to. The
    /// venue closes the whole connection on a subscription to a market it does
    /// not list, so nothing is subscribed before the list says it may be.
    active: HashSet<MarketId>,
    ctx: HashMap<MarketId, AssetCtx>,
    /// Candles by market and interval, kept for the session.
    series: HashMap<(MarketId, Interval), Series>,
    /// History asked for and not yet answered.
    requested: HashSet<(MarketId, Interval)>,
    /// History whose last load failed, until one succeeds.
    failed: HashSet<(MarketId, Interval)>,
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
        self.load(market, self.period, force);
    }

    /// Ask for a market's history for `period`, as `want_history` does.
    fn load(&mut self, market: &MarketId, period: MarketPeriod, force: bool) {
        let interval = period.interval();
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
            from_ms: now.saturating_sub(period.chart_millis() + interval.millis()),
            to_ms: now,
        });
        self.hurry_selected();
    }

    /// The histories someone is waiting to see, the panels' own charts and
    /// the selected market's: if they are queued, they go to the front, the
    /// selected market's first.
    fn hurry_selected(&self) {
        for key in &self.charts {
            let interval = key.period.interval();
            if self.requested.contains(&(key.market.clone(), interval)) {
                self.loader.prioritise(key.market.clone(), interval);
            }
        }
        if let Some(market) = &self.selected
            && self.requested.contains(&(market.clone(), self.interval()))
        {
            self.loader.prioritise(market.clone(), self.interval());
        }
    }

    fn charted(&self, market: &MarketId) -> bool {
        self.charts.iter().any(|key| &key.market == market)
    }

    /// Whether anything shows `market`: the watchlist or a chart.
    fn wanted(&self, market: &MarketId) -> bool {
        self.watched.contains(market) || self.charted(market)
    }

    /// Whether a chart needs `sub` and the venue lists its market.
    fn chart_needs(&self, sub: &Sub) -> bool {
        match sub {
            Sub::Ctx(market) => self.leverage.contains_key(market) && self.charted(market),
            Sub::Candles(market, interval) => {
                self.leverage.contains_key(market)
                    && self
                        .charts
                        .iter()
                        .any(|k| &k.market == market && k.period.interval() == *interval)
            }
        }
    }

    /// Subscribe for the watchlist. A chart may already hold it: then the
    /// watchlist takes it over.
    fn base_subscribe(&mut self, sub: Sub) {
        if !self.chart_subs.remove(&sub) {
            self.subscriptions.subscribe(sub);
        }
    }

    /// Unsubscribe for the watchlist. A chart may still need it: then the
    /// chart takes it over.
    fn base_unsubscribe(&mut self, sub: Sub) {
        if self.chart_needs(&sub) {
            self.chart_subs.insert(sub);
        } else {
            self.subscriptions.unsubscribe(sub);
        }
    }

    /// Subscribe to what the charts need beyond the watchlist, and drop what
    /// they no longer do. Markets the venue does not list are left out: a
    /// subscription to one closes the whole connection.
    fn sync_chart_subs(&mut self) {
        let mut keys: Vec<&ChartKey> = self.charts.iter().collect();
        keys.sort_by(|a, b| a.market.cmp(&b.market));
        let mut wanted: Vec<Sub> = Vec::new();
        for key in keys {
            let market = &key.market;
            if !self.leverage.contains_key(market) {
                continue;
            }
            let active = self.active.contains(market);
            let interval = key.period.interval();
            let mut needs = Vec::new();
            if !active {
                needs.push(Sub::Ctx(market.clone()));
            }
            if !(active && interval == self.interval()) {
                needs.push(Sub::Candles(market.clone(), interval));
            }
            for sub in needs {
                if !wanted.contains(&sub) {
                    wanted.push(sub);
                }
            }
        }
        let mut gone: Vec<Sub> = self
            .chart_subs
            .iter()
            .filter(|sub| !wanted.contains(sub))
            .cloned()
            .collect();
        gone.sort_by_key(|sub| format!("{sub:?}"));
        for sub in gone {
            self.chart_subs.remove(&sub);
            self.subscriptions.unsubscribe(sub);
        }
        for sub in wanted {
            if self.chart_subs.insert(sub.clone()) {
                self.subscriptions.subscribe(sub);
            }
        }
    }

    /// Load each chart's history the venue lists, unless it is here.
    fn load_charts(&mut self, force: bool) {
        let keys: Vec<ChartKey> = self.charts.iter().cloned().collect();
        for key in keys {
            if self.leverage.contains_key(&key.market) {
                self.load(&key.market, key.period, force);
            }
        }
    }

    fn set_charts(&mut self, charts: HashSet<ChartKey>) {
        if charts == self.charts {
            return;
        }
        let old = std::mem::replace(&mut self.charts, charts);
        // History queued for a chart that closed is no longer wanted.
        let before = self.requested.len();
        let keep: HashSet<(MarketId, Interval)> = self
            .requested
            .iter()
            .filter(|(market, interval)| {
                (self.active.contains(market) && *interval == self.interval())
                    || self.chart_needs(&Sub::Candles(market.clone(), *interval))
            })
            .cloned()
            .collect();
        if keep.len() != before {
            self.requested = keep.clone();
            self.loader.keep_only(keep);
        }
        // A market nothing shows any more loses what was kept for it.
        for key in &old {
            if !self.wanted(&key.market) {
                self.forget(&key.market.clone());
            }
        }
        self.sync_chart_subs();
        self.load_charts(false);
        self.hurry_selected();
    }

    /// Drop the figures and candles kept for `market`.
    fn forget(&mut self, market: &MarketId) {
        self.ctx.remove(market);
        self.failed.retain(|(m, _)| m != market);
        self.series.retain(|(m, _), _| m != market);
        self.requested.retain(|(m, _)| m != market);
    }

    fn select(&mut self, market: Option<MarketId>) {
        self.selected = market;
        self.hurry_selected();
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
        self.base_subscribe(Sub::Ctx(market.clone()));
        self.base_subscribe(Sub::Candles(market.clone(), self.interval()));
        self.want_history(market, false);
    }

    fn unwatch(&mut self, market: &MarketId) {
        if !self.watched.contains(market) {
            return;
        }
        self.watched.retain(|m| m != market);
        if self.active.remove(market) {
            self.base_unsubscribe(Sub::Ctx(market.clone()));
            self.base_unsubscribe(Sub::Candles(market.clone(), self.interval()));
        }
        // A chart of the market keeps its candles.
        if !self.charted(market) {
            self.forget(market);
        }
    }

    fn set_period(&mut self, period: MarketPeriod) {
        if period == self.period {
            return;
        }
        let old = self.interval();
        self.period = period;
        let new = self.interval();
        // Periods of an hour and less share their candles: nothing to reload.
        if old == new {
            return;
        }
        // History still queued for the old period is no longer wanted,
        // unless a chart is waiting for it.
        let keep: HashSet<(MarketId, Interval)> = self
            .requested
            .iter()
            .filter(|(market, interval)| {
                *interval == new || self.chart_needs(&Sub::Candles(market.clone(), *interval))
            })
            .cloned()
            .collect();
        self.requested = keep.clone();
        self.loader.keep_only(keep);
        for market in self.active_in_order() {
            self.base_unsubscribe(Sub::Candles(market.clone(), old));
            self.base_subscribe(Sub::Candles(market.clone(), new));
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
                if self.wanted(&market) {
                    self.ctx.insert(market, ctx);
                }
            }
            Event::Candle(market, interval, candle) => {
                self.heard();
                if self.wanted(&market) {
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
            self.load_charts(true);
        }
        self.last_message = Some(SystemTime::now());
    }

    fn on_loaded(&mut self, loaded: Loaded) {
        let key = (loaded.market, loaded.interval);
        self.requested.remove(&key);
        if !self.wanted(&key.0) {
            return;
        }
        // A failed load leaves the tile on placeholders and is asked again
        // later. It is remembered, so the chart can say it failed and is not
        // merely slow.
        match loaded.candles {
            Ok(candles) => {
                self.failed.remove(&key);
                self.series.entry(key).or_default().set_history(candles);
            }
            Err(_) => {
                self.failed.insert(key);
            }
        }
    }

    /// Ask again for any watched market still without history.
    fn retry_missing_history(&mut self) {
        for market in self.active_in_order() {
            self.want_history(&market, false);
        }
        self.load_charts(false);
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
                self.sync_chart_subs();
                self.load_charts(false);
                self.hurry_selected();
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
        // Nothing older than two of the longest windows at a series'
        // interval is ever asked for again, and never less than two weeks.
        for ((_, interval), series) in self.series.iter_mut() {
            let keep = (2 * interval.longest_window()).max(2 * MarketPeriod::Week.millis());
            series.trim(now, keep);
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
        // The selected market's chart: the candles its tile was computed
        // from, reaching back at least an hour so that a period of a few
        // minutes still has a chart to read. A market that was unwatched has
        // no chart.
        let chart = self
            .selected
            .as_ref()
            .filter(|market| self.watched.contains(market))
            .and_then(|market| {
                let series = self.series.get(&(market.clone(), interval))?;
                if !series.is_loaded() {
                    return None;
                }
                let last = self.ctx.get(market).map(|ctx| Live::from(ctx).last);
                Some(Chart {
                    market: market.clone(),
                    interval,
                    candles: chart(series.window(now, period.chart_millis()), last),
                })
            });
        let chart_failed = chart.is_none()
            && self
                .selected
                .as_ref()
                .is_some_and(|market| self.failed.contains(&(market.clone(), interval)));
        // Each panel's own chart, from the candles at its period's interval.
        let charts = self
            .charts
            .iter()
            .map(|key| {
                let interval = key.period.interval();
                let at = (key.market.clone(), interval);
                let view = match self.series.get(&at).filter(|s| s.is_loaded()) {
                    Some(series) => {
                        let last = self.ctx.get(&key.market).map(|ctx| Live::from(ctx).last);
                        ChartView::Ready(Chart {
                            market: key.market.clone(),
                            interval,
                            candles: super::candles::chart(
                                series.window(now, key.period.chart_millis()),
                                last,
                            ),
                        })
                    }
                    None if self.failed.contains(&at) => ChartView::Failed,
                    None => ChartView::Loading,
                };
                (key.clone(), view)
            })
            .collect();
        MarketSnapshot {
            period,
            tiles,
            connection: self.connection,
            last_message: self.last_message,
            listing: self.listing.clone(),
            chart,
            chart_failed,
            charts,
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
                    Some(Command::Select(market)) => state.select(market),
                    Some(Command::SetCharts(charts)) => state.set_charts(charts),
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
