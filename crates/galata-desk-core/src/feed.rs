//! Keeping one account's portfolio current: what to ask, how often, and what
//! to show when the venue does not answer.
//!
//! Three tiers, by how fast the thing moves and what it costs to ask:
//!
//! | Tier     | Every | Asks                                                   |
//! |----------|-------|--------------------------------------------------------|
//! | holdings | 5 s   | perps state of each dex the account is on, spot state  |
//! |          |       | (and, with each sweep, the account's collateral mode)  |
//! | orders   | 10 s  | resting orders of each such dex                        |
//! | overview | 60 s  | history, fees, vaults, staking; a sweep of every dex   |
//!
//! Only dexes where the account has something are asked every few seconds.
//! The sweep finds them: every dex's state once a minute, every dex's orders
//! once every five.
//!
//! A tier that fails keeps the data it last had and records why. HTTP 429
//! doubles every interval; each later success halves it back.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, SystemTime};

use futures_util::future::{try_join_all, try_join4};
use rust_decimal::Decimal;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::funding::FundingRates;
use crate::market::fill_history::{self, FillHistory};
use crate::market::fills::Fill;
use crate::market::history::Budget;
use crate::model::{self, Collateral, DexState, Holdings, OrderRow};
use crate::venue::{
    ClearinghouseState, Info, PerpMeta, PortfolioHistory, SpotMeta, Transport, UserFees, VenueError,
};

/// One part of the screen: what was last fetched, when, and whether the most
/// recent attempt failed.
#[derive(Debug, Clone, PartialEq)]
pub struct Section<T> {
    pub data: Option<T>,
    /// When `data` was fetched.
    pub fetched_at: Option<SystemTime>,
    /// Why the most recent attempt failed; `None` after a success.
    pub error: Option<VenueError>,
}

impl<T> Default for Section<T> {
    fn default() -> Self {
        Self {
            data: None,
            fetched_at: None,
            error: None,
        }
    }
}

impl<T> Section<T> {
    /// Nothing has arrived and nothing has failed: show placeholders.
    pub fn is_loading(&self) -> bool {
        self.data.is_none() && self.error.is_none()
    }

    /// Data is shown, but the latest attempt to renew it failed.
    pub fn is_stale(&self) -> bool {
        self.data.is_some() && self.error.is_some()
    }

    fn succeed(&mut self, data: T) {
        self.data = Some(data);
        self.fetched_at = Some(SystemTime::now());
        self.error = None;
    }

    fn fail(&mut self, error: VenueError) {
        self.error = Some(error);
    }
}

/// The slow-moving part: performance history, fees, vaults, staking.
#[derive(Debug, Clone, PartialEq)]
pub struct Overview {
    pub history: PortfolioHistory,
    pub fees: UserFees,
    pub vault_equity: Decimal,
    pub staking_hype: Decimal,
}

/// Everything the portfolio screen shows, as last known.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub holdings: Section<Holdings>,
    pub orders: Section<Vec<OrderRow>>,
    pub overview: Section<Overview>,
    /// The current funding rate of every market on the dexes the account has
    /// positions on, and on the dexes of the markets it watches.
    pub rates: Section<FundingRates>,
    /// The account's fills across all markets: the walk back through its
    /// history once it has finished, and the latest fills on top.
    pub fills: Section<crate::market::fills::Fills>,
    /// The venue has asked the app to slow down and it has not yet recovered.
    pub slowed: bool,
}

impl Snapshot {
    /// The most recent successful fetch of any section.
    pub fn last_refreshed(&self) -> Option<SystemTime> {
        [
            self.holdings.fetched_at,
            self.orders.fetched_at,
            self.overview.fetched_at,
        ]
        .into_iter()
        .flatten()
        .max()
    }

    /// The failure to tell the user about, if any section's latest attempt failed.
    pub fn failure(&self) -> Option<&VenueError> {
        self.holdings
            .error
            .as_ref()
            .or(self.orders.error.as_ref())
            .or(self.overview.error.as_ref())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Holdings,
    Orders,
    Overview,
}

/// How long to wait between refreshes of each tier, and how that stretches
/// when the venue pushes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cadence {
    factor: u32,
}

impl Default for Cadence {
    fn default() -> Self {
        Self { factor: 1 }
    }
}

impl Cadence {
    const MAX_FACTOR: u32 = 16;

    pub fn base(tier: Tier) -> Duration {
        match tier {
            Tier::Holdings => Duration::from_secs(5),
            Tier::Orders => Duration::from_secs(10),
            Tier::Overview => Duration::from_secs(60),
        }
    }

    /// The current wait for a tier. The fast tiers stretch to a minute at
    /// most, the overview to ten.
    pub fn interval(&self, tier: Tier) -> Duration {
        let ceiling = match tier {
            Tier::Holdings | Tier::Orders => Duration::from_secs(60),
            Tier::Overview => Duration::from_secs(600),
        };
        (Self::base(tier) * self.factor).min(ceiling)
    }

    /// Record how an attempt went.
    pub fn record(&mut self, result: Result<(), &VenueError>) {
        match result {
            Err(VenueError::RateLimited) => {
                self.factor = (self.factor * 2).min(Self::MAX_FACTOR);
            }
            Ok(()) => self.factor = (self.factor / 2).max(1),
            // Unreachable or misunderstood: retry at the same pace.
            Err(_) => {}
        }
    }

    pub fn is_slowed(&self) -> bool {
        self.factor > 1
    }
}

/// One account's refresher: knows what it has learned and how to renew it.
pub struct Refresher<T> {
    info: Info<T>,
    user: String,
    /// Every perpetuals dex; the main one is the empty name.
    dexes: Vec<String>,
    meta: Option<SpotMeta>,
    /// How the account's collateral is held, once the venue has said.
    collateral: Option<Collateral>,
    /// The last state seen on each dex.
    states: HashMap<String, ClearinghouseState>,
    /// The markets of each dex a position or an order was seen on. Asked
    /// once: a market's place in its dex's list does not change.
    perp_metas: HashMap<String, PerpMeta>,
    /// The last orders seen on each dex.
    orders: HashMap<String, Vec<OrderRow>>,
    /// Dexes worth asking every few seconds.
    active: BTreeSet<String>,
    /// Dexes that had resting orders at the last full sweep.
    with_orders: BTreeSet<String>,
    /// Dexes whose funding rates are wanted though the account may hold
    /// nothing there: those of the markets it watches.
    watched_dexes: BTreeSet<String>,
    snapshot: Snapshot,
    cadence: Cadence,
    /// Every fill seen: the walk's and the top-ups'.
    history: FillHistory,
}

/// How often the orders tier asks every dex instead of only the active ones.
const ORDER_SWEEP_EVERY: Duration = Duration::from_secs(300);

/// A sweep asks one question per dex. The main network has about a dozen
/// dexes; the test network has hundreds, and asking them all at once is
/// refused. So a sweep goes out in batches with a pause between them.
const SWEEP_BATCH: usize = 32;
const SWEEP_PAUSE: Duration = Duration::from_secs(1);
/// With more dexes than this, the costly sweeps are rationed: positions are
/// looked for everywhere only every ten minutes, and open orders only on the
/// dexes where the account is already known to be.
const MANY_DEXES: usize = 32;
const WIDE_SWEEP_EVERY: Duration = Duration::from_secs(600);

impl<T: Transport> Refresher<T> {
    pub fn new(transport: T, user: impl Into<String>) -> Self {
        Self {
            info: Info::new(transport),
            user: user.into(),
            dexes: Vec::new(),
            meta: None,
            collateral: None,
            states: HashMap::new(),
            perp_metas: HashMap::new(),
            orders: HashMap::new(),
            // The main dex is always asked.
            active: BTreeSet::from([String::new()]),
            with_orders: BTreeSet::new(),
            watched_dexes: BTreeSet::new(),
            snapshot: Snapshot::default(),
            cadence: Cadence::default(),
            history: FillHistory::default(),
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The network has too many dexes to ask them all often.
    pub fn many_dexes(&self) -> bool {
        self.dexes.len() > MANY_DEXES
    }

    /// How long until positions are looked for on every dex again.
    pub fn sweep_interval(&self) -> Duration {
        if self.many_dexes() {
            WIDE_SWEEP_EVERY
        } else {
            self.cadence.interval(Tier::Overview)
        }
    }

    pub fn cadence(&self) -> &Cadence {
        &self.cadence
    }

    /// What never changes while the app runs: the list of dexes and of spot
    /// tokens. Asked until answered.
    async fn reference(&mut self) -> Result<(), VenueError> {
        if self.dexes.is_empty() {
            self.dexes = self.info.perp_dexs().await?;
        }
        if self.meta.is_none() {
            self.meta = Some(self.info.spot_meta().await?);
        }
        Ok(())
    }

    fn settle<V>(&mut self, result: &Result<V, VenueError>) {
        self.cadence.record(result.as_ref().map(|_| ()));
        self.snapshot.slowed = self.cadence.is_slowed();
    }

    /// Positions and balances. `sweep` asks every dex instead of the active ones.
    pub async fn refresh_holdings(&mut self, sweep: bool) {
        let result = self.fetch_holdings(sweep).await;
        self.settle(&result);
        match result {
            Ok(holdings) => self.snapshot.holdings.succeed(holdings),
            Err(e) => self.snapshot.holdings.fail(e),
        }
    }

    async fn fetch_holdings(&mut self, sweep: bool) -> Result<Holdings, VenueError> {
        self.reference().await?;
        let asked: Vec<String> = if sweep {
            self.dexes.clone()
        } else {
            self.active.iter().cloned().collect()
        };
        // One request per dex, a batch at a time: a sweep of a dozen dexes
        // goes out at once, and one of hundreds is spread out so the venue
        // does not refuse it.
        let mut states = Vec::with_capacity(asked.len());
        for (n, batch) in asked.chunks(SWEEP_BATCH).enumerate() {
            if n > 0 {
                tokio::time::sleep(SWEEP_PAUSE).await;
            }
            states.extend(
                try_join_all(
                    batch
                        .iter()
                        .map(|dex| self.info.clearinghouse_state(&self.user, dex)),
                )
                .await?,
            );
        }
        for (dex, state) in asked.into_iter().zip(states) {
            let has_something =
                !state.asset_positions.is_empty() || !state.margin_summary.account_value.is_zero();
            if dex.is_empty() || has_something {
                self.active.insert(dex.clone());
            } else {
                self.active.remove(&dex);
            }
            self.states.insert(dex, state);
        }

        // The account's mode decides how its parts add up. It is asked on the
        // first pass and again with each sweep, so a change of account type
        // shows within a minute.
        if sweep || self.collateral.is_none() {
            let mode = self.info.account_mode(&self.user).await?;
            self.collateral = Some(Collateral::from_venue(&mode));
        }
        let collateral = self.collateral.unwrap_or_default();

        let spot = self.info.spot_state(&self.user).await?;
        // Prices are needed only to value a spot token other than USDC.
        let needs_prices = spot
            .balances
            .iter()
            .any(|b| b.token != 0 && !b.total.is_zero());
        let mids = if needs_prices {
            self.info.all_mids().await?
        } else {
            HashMap::new()
        };

        // Closing a position needs its market's number, as acting on an
        // order does: the dex's list of markets is asked once, the first
        // time the account holds something there. Until it is had, the
        // positions show and cannot be acted on.
        self.learn_markets(
            self.states
                .iter()
                .filter(|(_, state)| !state.asset_positions.is_empty())
                .map(|(dex, _)| dex.clone())
                .collect(),
        )
        .await;

        let meta = self.meta.as_ref().expect("reference() set it");
        let dexes: Vec<DexState<'_>> = self
            .dexes
            .iter()
            .filter_map(|dex| {
                self.states.get(dex).map(|state| DexState {
                    dex,
                    state,
                    dexes: &self.dexes,
                    perp: self.perp_metas.get(dex),
                })
            })
            .collect();
        Ok(model::holdings(&dexes, &spot, meta, &mids, collateral))
    }

    /// Ask for the markets of each of `dexes` not yet known. A list that
    /// cannot be had now is asked for again next time.
    async fn learn_markets(&mut self, dexes: Vec<String>) {
        for dex in dexes {
            if !self.perp_metas.contains_key(&dex)
                && let Ok(perp) = self.info.perp_meta(&dex).await
            {
                self.perp_metas.insert(dex, perp);
            }
        }
    }

    /// Resting orders. `sweep` asks every dex instead of the active ones.
    pub async fn refresh_orders(&mut self, sweep: bool) {
        let result = self.fetch_orders(sweep).await;
        self.settle(&result);
        match result {
            Ok(rows) => self.snapshot.orders.succeed(rows),
            Err(e) => self.snapshot.orders.fail(e),
        }
    }

    async fn fetch_orders(&mut self, sweep: bool) -> Result<Vec<OrderRow>, VenueError> {
        self.reference().await?;
        // An order query costs ten times a position query. Where there are
        // hundreds of dexes, only those the account is known to be on are
        // asked: an order resting on a dex with no position or balance there
        // is not found.
        let asked: Vec<String> = if sweep && !self.many_dexes() {
            self.dexes.clone()
        } else {
            self.active.union(&self.with_orders).cloned().collect()
        };
        let answers = try_join_all(
            asked
                .iter()
                .map(|dex| self.info.open_orders(&self.user, dex)),
        )
        .await?;
        // Acting on an order needs its market's number, which is its place
        // in the dex's list. Until the list is had the orders show and
        // cannot be acted on.
        self.learn_markets(
            asked
                .iter()
                .zip(&answers)
                .filter(|(_, raw)| !raw.is_empty())
                .map(|(dex, _)| dex.clone())
                .collect(),
        )
        .await;
        let meta = self.meta.as_ref().expect("reference() set it");
        for (dex, raw) in asked.into_iter().zip(answers) {
            let rows = model::orders(&dex, &raw, meta, &self.dexes, self.perp_metas.get(&dex));
            if rows.is_empty() {
                self.with_orders.remove(&dex);
            } else {
                self.with_orders.insert(dex.clone());
            }
            self.orders.insert(dex, rows);
        }
        let mut all: Vec<OrderRow> = self.orders.values().flatten().cloned().collect();
        model::sort_orders(&mut all);
        Ok(all)
    }

    /// History, fees, vaults and staking.
    pub async fn refresh_overview(&mut self) {
        let result = self.fetch_overview().await;
        self.settle(&result);
        match result {
            Ok(overview) => self.snapshot.overview.succeed(overview),
            Err(e) => self.snapshot.overview.fail(e),
        }
    }

    /// The account's latest fills, folded into what is already held.
    pub async fn refresh_fills(&mut self) {
        let result = self.info.user_fills(&self.user).await;
        self.settle(&result);
        match result {
            Ok(raw) => {
                self.history
                    .top_up(raw.into_iter().map(Fill::from).collect());
                self.snapshot.fills.succeed(self.history.to_fills());
            }
            Err(e) => self.snapshot.fills.fail(e),
        }
    }

    /// Fold in the walk back through the account's fills. Returns whether it
    /// succeeded: a failed walk is tried again later, and the latest fills
    /// stay shown meanwhile.
    pub fn finish_fill_walk(&mut self, walked: Result<Vec<Fill>, VenueError>) -> bool {
        match walked {
            Ok(fills) => {
                self.history.finish_walk(fills);
                self.snapshot.fills.succeed(self.history.to_fills());
                true
            }
            Err(_) => false,
        }
    }

    /// Walk the account's fills on a request client of its own, so the walk's
    /// pacing does not hold up the tiers.
    pub fn fill_walk(&self) -> impl Future<Output = Result<Vec<Fill>, VenueError>> + Send + 'static
    where
        T: Clone,
    {
        let info = self.info.clone();
        let user = self.user.clone();
        async move {
            let mut budget = Budget::for_fill_history();
            fill_history::walk(&info, &user, &mut budget).await
        }
    }

    /// Also keep the funding rates of these dexes current: the ones the
    /// watched markets are on. Returns whether that changed what is asked.
    pub fn watch_dexes(&mut self, dexes: BTreeSet<String>) -> bool {
        let changed = self.watched_dexes != dexes;
        self.watched_dexes = dexes;
        changed
    }

    /// The current funding rates on every dex the account has a position on
    /// or watches a market on, each asked once. Until the positions are
    /// known nothing is asked: the first answer should cover them.
    pub async fn refresh_rates(&mut self) {
        let Some(holdings) = &self.snapshot.holdings.data else {
            return;
        };
        let mut dexes: BTreeSet<String> =
            holdings.positions.iter().map(|p| p.dex.clone()).collect();
        dexes.extend(self.watched_dexes.iter().cloned());
        let result = try_join_all(dexes.iter().map(|dex| self.info.funding_rates(dex))).await;
        self.settle(&result);
        match result {
            Ok(per_dex) => self
                .snapshot
                .rates
                .succeed(per_dex.into_iter().flatten().collect()),
            Err(e) => self.snapshot.rates.fail(e),
        }
    }

    async fn fetch_overview(&mut self) -> Result<Overview, VenueError> {
        let (history, fees, vaults, staking) = try_join4(
            self.info.portfolio(&self.user),
            self.info.user_fees(&self.user),
            self.info.vault_equities(&self.user),
            self.info.delegator_summary(&self.user),
        )
        .await?;
        let (vault_equity, staking_hype) = model::slow_equity(&vaults, &staking);
        Ok(Overview {
            history,
            fees,
            vault_equity,
            staking_hype,
        })
    }
}

enum Command {
    RefreshNow,
    AfterAction,
    WatchDexes(BTreeSet<String>),
}

/// A running refresher. Dropping it stops the refreshing.
pub struct Feed {
    snapshots: watch::Receiver<Snapshot>,
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

impl Feed {
    /// Start refreshing `user`'s portfolio on the current Tokio runtime.
    pub fn start<T: Transport + Clone>(transport: T, user: impl Into<String>) -> Self {
        let (tx, snapshots) = watch::channel(Snapshot::default());
        let (commands, rx) = mpsc::unbounded_channel();
        let refresher = Refresher::new(transport, user);
        let task = tokio::spawn(run(refresher, tx, rx));
        Self {
            snapshots,
            commands,
            task,
        }
    }

    /// A receiver that always holds the latest snapshot.
    pub fn snapshots(&self) -> watch::Receiver<Snapshot> {
        self.snapshots.clone()
    }

    /// Fetch every section again at once.
    pub fn refresh_now(&self) {
        let _ = self.commands.send(Command::RefreshNow);
    }

    /// Fetch the positions and the open orders again at once, without
    /// waiting for their turn: an action on either may have changed both
    /// (a close removes a position's take-profit and stop-loss; a limit
    /// close adds an order).
    pub fn refresh_after_action(&self) {
        let _ = self.commands.send(Command::AfterAction);
    }

    /// Keep the funding rates of these dexes current too: the ones the
    /// watched markets are on. A change is fetched at once.
    pub fn watch_dexes(&self, dexes: BTreeSet<String>) {
        let _ = self.commands.send(Command::WatchDexes(dexes));
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// How long to wait before walking the fills again after a walk failed.
const WALK_AGAIN: Duration = Duration::from_secs(60);

/// A spawned task that stops when this is dropped, with the feed.
struct Owned(JoinHandle<()>);

impl Drop for Owned {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn run<T: Transport + Clone>(
    mut refresher: Refresher<T>,
    tx: watch::Sender<Snapshot>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let publish = |r: &Refresher<T>| {
        // Subscribers are woken only when something they can see changed.
        tx.send_if_modified(|current| {
            let changed = current != r.snapshot();
            if changed {
                *current = r.snapshot().clone();
            }
            changed
        });
    };

    let start = Instant::now();
    let mut holdings_due = start;
    let mut orders_due = start;
    let mut overview_due = start;
    // The first pass of each tier sweeps every dex.
    let mut holdings_sweep_due = start;
    let mut orders_sweep_due = start;
    // The watched dexes changed: their rates are wanted now.
    let mut rates_wanted = false;

    // The walk back through the account's fills runs beside the tiers, at
    // its own pace, and reports here when it is done.
    let (walked_tx, mut walked) = mpsc::unbounded_channel();
    let spawn_walk = |refresher: &Refresher<T>, after: Duration| {
        let walk = refresher.fill_walk();
        let tx = walked_tx.clone();
        Owned(tokio::spawn(async move {
            tokio::time::sleep(after).await;
            let _ = tx.send(walk.await);
        }))
    };
    let mut _walk = spawn_walk(&refresher, Duration::ZERO);

    loop {
        let next = holdings_due.min(orders_due).min(overview_due);
        tokio::select! {
            _ = tokio::time::sleep_until(next) => {}
            Some(result) = walked.recv() => {
                if !refresher.finish_fill_walk(result) {
                    _walk = spawn_walk(&refresher, WALK_AGAIN);
                }
                publish(&refresher);
            }
            command = commands.recv() => match command {
                // After an action: the positions and the orders, on the
                // dexes already known, without a sweep.
                Some(Command::AfterAction) => {
                    let now = Instant::now();
                    holdings_due = now;
                    orders_due = now;
                }
                Some(Command::WatchDexes(dexes)) => {
                    rates_wanted |= refresher.watch_dexes(dexes);
                }
                Some(Command::RefreshNow) => {
                    let now = Instant::now();
                    holdings_due = now;
                    orders_due = now;
                    overview_due = now;
                    holdings_sweep_due = now;
                    orders_sweep_due = now;
                }
                // The feed was dropped.
                None => return,
            },
        }

        let now = Instant::now();
        if now >= holdings_due {
            let sweep = now >= holdings_sweep_due;
            refresher.refresh_holdings(sweep).await;
            if sweep && refresher.snapshot().holdings.error.is_none() {
                holdings_sweep_due = Instant::now() + refresher.sweep_interval();
            }
            holdings_due = Instant::now() + refresher.cadence().interval(Tier::Holdings);
            publish(&refresher);
            // The first positions say which dexes to ask for rates: do not
            // leave the Funding section empty until the slow tier comes round.
            if refresher.snapshot().rates.is_loading() {
                refresher.refresh_rates().await;
                publish(&refresher);
            }
        }
        // Rates for newly watched dexes, once there are positions to go with
        // them; before that the first holdings pass asks for both.
        if rates_wanted && refresher.snapshot().holdings.data.is_some() {
            rates_wanted = false;
            if now < overview_due {
                refresher.refresh_rates().await;
                publish(&refresher);
            }
        }
        if now >= overview_due {
            refresher.refresh_overview().await;
            refresher.refresh_rates().await;
            refresher.refresh_fills().await;
            overview_due = Instant::now() + refresher.cadence().interval(Tier::Overview);
            publish(&refresher);
        }
        if now >= orders_due {
            let sweep = now >= orders_sweep_due;
            refresher.refresh_orders(sweep).await;
            if sweep && refresher.snapshot().orders.error.is_none() {
                orders_sweep_due = Instant::now() + ORDER_SWEEP_EVERY;
            }
            orders_due = Instant::now() + refresher.cadence().interval(Tier::Orders);
            publish(&refresher);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiting_doubles_the_wait_and_success_halves_it_back() {
        let mut c = Cadence::default();
        assert_eq!(c.interval(Tier::Holdings), Duration::from_secs(5));
        c.record(Err(&VenueError::RateLimited));
        assert_eq!(c.interval(Tier::Holdings), Duration::from_secs(10));
        assert_eq!(c.interval(Tier::Overview), Duration::from_secs(120));
        assert!(c.is_slowed());
        c.record(Err(&VenueError::RateLimited));
        assert_eq!(c.interval(Tier::Holdings), Duration::from_secs(20));
        c.record(Ok(()));
        assert_eq!(c.interval(Tier::Holdings), Duration::from_secs(10));
        c.record(Ok(()));
        assert_eq!(c.interval(Tier::Holdings), Duration::from_secs(5));
        assert!(!c.is_slowed());
    }

    #[test]
    fn the_wait_has_a_ceiling() {
        let mut c = Cadence::default();
        for _ in 0..20 {
            c.record(Err(&VenueError::RateLimited));
        }
        assert_eq!(c.interval(Tier::Holdings), Duration::from_secs(60));
        assert_eq!(c.interval(Tier::Orders), Duration::from_secs(60));
        assert_eq!(c.interval(Tier::Overview), Duration::from_secs(600));
    }

    #[test]
    fn other_failures_do_not_change_the_pace() {
        let mut c = Cadence::default();
        c.record(Err(&VenueError::Unreachable("no connection".into())));
        c.record(Err(&VenueError::Status(502)));
        assert_eq!(c, Cadence::default());
    }
}
