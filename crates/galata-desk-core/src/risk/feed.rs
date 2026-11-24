//! Candle history for the Risk section: every held market at every interval
//! the chosen periods use.
//!
//! History comes through the same rationed loader Markets uses, spending the
//! same [`shared_history_budget`] so the two sections together keep to it. A
//! loaded series is asked for again once a newer candle has closed and at
//! least [`REFRESH_AT_MOST_EVERY`] has passed; a failed one, every
//! [`RETRY_FAILED_EVERY`]. No live candles are streamed: risk is estimated
//! from closed candles only.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::{RiskPeriod, SeriesState};
use crate::market::feed::{Clock, system_clock};
use crate::market::history::{Loaded, Loader, Request, SharedBudget, shared_history_budget};
use crate::market::{Interval, MarketId};
use crate::venue::{Info, Transport};

pub const REFRESH_AT_MOST_EVERY: Duration = Duration::from_secs(300);
pub const RETRY_FAILED_EVERY: Duration = Duration::from_secs(120);
const TICK: Duration = Duration::from_secs(5);

/// What the feed has, published whole. `version` grows with every change.
#[derive(Debug, Clone, Default)]
pub struct RiskCandles {
    pub version: u64,
    pub series: Arc<HashMap<(MarketId, Interval), SeriesState>>,
}

enum Command {
    Want(Vec<MarketId>, Vec<Interval>),
}

/// A running feed. Dropping it stops the loading.
pub struct RiskFeed {
    snapshots: watch::Receiver<RiskCandles>,
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

impl RiskFeed {
    /// Start against the live venue on the current Tokio runtime.
    pub fn live<T: Transport + Clone>(transport: T) -> Self {
        Self::start(transport, system_clock(), shared_history_budget())
    }

    pub fn start<T: Transport + Clone>(
        transport: T,
        clock: Clock,
        budget: impl Into<SharedBudget>,
    ) -> Self {
        let (tx, snapshots) = watch::channel(RiskCandles::default());
        let (commands, rx) = mpsc::unbounded_channel();
        let (loader, loaded) = Loader::start(Info::new(transport), budget);
        let task = tokio::spawn(run(loader, clock, tx, rx, loaded));
        Self {
            snapshots,
            commands,
            task,
        }
    }

    pub fn snapshots(&self) -> watch::Receiver<RiskCandles> {
        self.snapshots.clone()
    }

    /// Load `markets` at `intervals`, in that order of priority: the first
    /// interval's series are asked for first. Anything left out is forgotten.
    pub fn want(&self, markets: Vec<MarketId>, intervals: Vec<Interval>) {
        let _ = self.commands.send(Command::Want(markets, intervals));
    }
}

impl Drop for RiskFeed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// How far back a series at `interval` is loaded: the longest period that
/// uses it, and two candles more.
fn window(interval: Interval) -> u64 {
    let longest = RiskPeriod::ALL
        .iter()
        .filter(|p| p.interval() == interval)
        .map(|p| p.millis())
        .max()
        .unwrap_or(0);
    longest + 2 * interval.millis()
}

struct Entry {
    state: SeriesState,
    /// When it was last asked for, by the clock.
    asked_ms: u64,
    in_flight: bool,
}

struct State {
    loader: Loader,
    clock: Clock,
    wanted: Vec<(MarketId, Interval)>,
    entries: HashMap<(MarketId, Interval), Entry>,
    version: u64,
}

impl State {
    fn ask(&mut self, key: &(MarketId, Interval)) {
        let now = (self.clock)();
        self.loader.request(Request {
            market: key.0.clone(),
            interval: key.1,
            from_ms: now.saturating_sub(window(key.1)),
            to_ms: now,
        });
        let entry = self.entries.entry(key.clone()).or_insert(Entry {
            state: SeriesState::Loading,
            asked_ms: now,
            in_flight: true,
        });
        entry.asked_ms = now;
        entry.in_flight = true;
    }

    fn want(&mut self, markets: Vec<MarketId>, intervals: Vec<Interval>) {
        let wanted: Vec<(MarketId, Interval)> = intervals
            .iter()
            .flat_map(|i| markets.iter().map(move |m| (m.clone(), *i)))
            .collect();
        let keep: HashSet<(MarketId, Interval)> = wanted.iter().cloned().collect();
        self.loader.keep_only(keep.clone());
        let before = self.entries.len();
        self.entries.retain(|k, _| keep.contains(k));
        let mut changed = self.entries.len() != before;
        for key in &wanted {
            if !self.entries.contains_key(key) {
                self.ask(key);
                changed = true;
            }
        }
        self.wanted = wanted;
        if changed {
            self.version += 1;
        }
    }

    fn on_loaded(&mut self, loaded: Loaded) {
        let key = (loaded.market, loaded.interval);
        let Some(entry) = self.entries.get_mut(&key) else {
            return;
        };
        entry.in_flight = false;
        entry.state = match (loaded.candles, &entry.state) {
            (Ok(candles), _) => SeriesState::Ready(candles),
            // A refresh that fails keeps what was loaded before.
            (Err(_), SeriesState::Ready(old)) => SeriesState::Ready(old.clone()),
            (Err(_), _) => SeriesState::Failed,
        };
        self.version += 1;
    }

    fn refresh_due(&mut self) {
        let now = (self.clock)();
        let due: Vec<(MarketId, Interval)> = self
            .wanted
            .iter()
            .filter(|key| match self.entries.get(*key) {
                Some(e) if !e.in_flight => {
                    let since = now.saturating_sub(e.asked_ms);
                    match e.state {
                        SeriesState::Failed => since >= RETRY_FAILED_EVERY.as_millis() as u64,
                        SeriesState::Ready(_) => {
                            let step = key.1.millis();
                            let next_close = (e.asked_ms / step + 1) * step;
                            now >= next_close && since >= REFRESH_AT_MOST_EVERY.as_millis() as u64
                        }
                        SeriesState::Loading => false,
                    }
                }
                _ => false,
            })
            .cloned()
            .collect();
        for key in due {
            self.ask(&key);
        }
    }

    fn snapshot(&self) -> RiskCandles {
        RiskCandles {
            version: self.version,
            series: Arc::new(
                self.entries
                    .iter()
                    .map(|(k, e)| (k.clone(), e.state.clone()))
                    .collect(),
            ),
        }
    }
}

async fn run(
    loader: Loader,
    clock: Clock,
    tx: watch::Sender<RiskCandles>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut loaded: mpsc::UnboundedReceiver<Loaded>,
) {
    let mut state = State {
        loader,
        clock,
        wanted: Vec::new(),
        entries: HashMap::new(),
        version: 0,
    };
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut published = 0;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Want(markets, intervals)) => state.want(markets, intervals),
                None => return,
            },
            Some(answer) = loaded.recv() => state.on_loaded(answer),
            _ = tick.tick() => state.refresh_due(),
        }
        if state.version != published {
            published = state.version;
            let _ = tx.send(state.snapshot());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_interval_loads_its_longest_period_and_a_little_more() {
        assert_eq!(window(Interval::H8), 30 * 86_400_000 + 2 * 8 * 3_600_000);
        assert_eq!(window(Interval::D1), 182 * 86_400_000);
        assert_eq!(window(Interval::D3), 371 * 86_400_000);
    }
}
