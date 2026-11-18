//! Funding history for the market someone is looking at, loaded over REST on
//! a budget.
//!
//! Only the chosen market is loaded: the newest 500 hours first, then
//! backwards until the window the chart shows is covered. What has loaded is
//! kept for as long as the feed lives, so a market chosen again is there at
//! once, and each new hour's rate is added while a market stays chosen.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::history::{HOUR_MS, History, Resolution, hour_of};
use crate::market::MarketId;
use crate::market::feed::Clock;
use crate::market::history::Budget;
use crate::venue::{Info, Transport, VenueError};

/// The venue's documented weight for a history request: 20, and 1 more for
/// every 20 rows it returns.
pub fn request_weight(rows: u64) -> f64 {
    20.0 + (rows / 20) as f64
}

impl Budget {
    /// The funding history's allowance: three full pages at once, then about
    /// a page every twenty seconds.
    ///
    /// A page of 500 rows costs 45 by the venue's documentation, so the
    /// daily window's five requests cost about 210 and a cold start can
    /// spend 255 in its first minute. With the Portfolio section and the
    /// candle loader (see `for_history`) that keeps the app's first minute
    /// near 1,125 of the venue's 1,200.
    pub fn for_funding() -> Self {
        Self::new(135.0, 120.0)
    }
}

/// How long after the hour turns its rate is first asked for: the venue
/// stamps a row within a tenth of a second, and publishes it soon after.
const TOP_UP_AFTER: Duration = Duration::from_secs(20);
/// How long to wait before asking again for an hour the venue did not have.
const TOP_UP_AGAIN: Duration = Duration::from_secs(60);

/// How long to wait after a failure; doubles while it keeps failing.
const RETRY_FIRST: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(120);
/// How long to stop after the venue says to slow down; doubles likewise.
const SLOW_DOWN_FIRST: Duration = Duration::from_secs(15);
const SLOW_DOWN_MAX: Duration = Duration::from_secs(120);

/// The chosen market's funding history, as last known.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChartSnapshot {
    /// `None` with nothing chosen.
    pub market: Option<MarketId>,
    pub resolution: Resolution,
    /// What has loaded of the chosen market's history.
    pub history: Arc<History>,
    /// More of the window is still to come.
    pub loading: bool,
    /// The last attempt to load failed. It is tried again; a success clears
    /// this.
    pub failed: bool,
}

enum Command {
    Choose(Option<MarketId>),
    SetResolution(Resolution),
}

/// A running feed. Dropping it stops the loading.
pub struct HistoryFeed {
    snapshots: watch::Receiver<ChartSnapshot>,
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

impl HistoryFeed {
    /// Start on the current Tokio runtime, with nothing chosen. `clock`
    /// gives the time in milliseconds since the epoch.
    pub fn start<T: Transport>(info: Info<T>, budget: Budget, clock: Clock) -> Self {
        let (tx, snapshots) = watch::channel(ChartSnapshot::default());
        let (commands, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(info, budget, clock, rx, tx));
        Self {
            snapshots,
            commands,
            task,
        }
    }

    /// A receiver that always holds the latest snapshot.
    pub fn snapshots(&self) -> watch::Receiver<ChartSnapshot> {
        self.snapshots.clone()
    }

    /// Load and keep current this market's history, and no other's.
    pub fn choose(&self, market: Option<MarketId>) {
        let _ = self.commands.send(Command::Choose(market));
    }

    /// Cover this resolution's window.
    pub fn set_resolution(&self, resolution: Resolution) {
        let _ = self.commands.send(Command::SetResolution(resolution));
    }
}

impl Drop for HistoryFeed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What to ask the venue for next.
enum Ask {
    /// Part of the window that has not been loaded.
    Page(u64, u64),
    /// The hours since the newest row.
    TopUp(u64, u64),
}

struct State {
    histories: HashMap<MarketId, Arc<History>>,
    chosen: Option<MarketId>,
    resolution: Resolution,
    failed: bool,
    /// Nothing is asked before this: the last attempt failed.
    retry_at: Option<tokio::time::Instant>,
    retry_after: Duration,
    /// No top-up is asked before this: the venue did not have the hour yet.
    top_up_at: Option<tokio::time::Instant>,
}

impl State {
    fn apply(&mut self, command: Command) {
        match command {
            Command::Choose(market) => {
                if self.chosen != market {
                    self.chosen = market;
                    // A failure belonged to the market that was chosen.
                    self.failed = false;
                    self.retry_at = None;
                    self.retry_after = RETRY_FIRST;
                    self.top_up_at = None;
                }
            }
            Command::SetResolution(resolution) => self.resolution = resolution,
        }
    }

    fn history(&self) -> Arc<History> {
        self.chosen
            .as_ref()
            .and_then(|market| self.histories.get(market).cloned())
            .unwrap_or_default()
    }

    /// The next thing to ask for, and how long to wait when there is
    /// nothing to ask now.
    fn next(&self, now_ms: u64) -> Result<Ask, Duration> {
        // Until the next hour's rate is due.
        let until_next_hour =
            Duration::from_millis(hour_of(now_ms) + HOUR_MS - now_ms) + TOP_UP_AFTER;
        if self.chosen.is_none() {
            return Err(until_next_hour);
        }
        let history = self.history();
        if let Some((from, to)) = history.next_page(now_ms, self.resolution) {
            return Ok(Ask::Page(from, to));
        }
        let Some((from, to)) = history.top_up(now_ms) else {
            return Err(until_next_hour);
        };
        let now = tokio::time::Instant::now();
        // Not before the hour's rate can have been published.
        let since_hour = Duration::from_millis(now_ms - hour_of(now_ms));
        if since_hour < TOP_UP_AFTER {
            return Err(TOP_UP_AFTER - since_hour);
        }
        match self.top_up_at {
            Some(at) if at > now => Err(at - now),
            _ => Ok(Ask::TopUp(from, to)),
        }
    }

    fn snapshot(&self, now_ms: u64) -> ChartSnapshot {
        let history = self.history();
        ChartSnapshot {
            loading: self.chosen.is_some() && history.next_page(now_ms, self.resolution).is_some(),
            market: self.chosen.clone(),
            resolution: self.resolution,
            history,
            failed: self.failed,
        }
    }
}

async fn run<T: Transport>(
    info: Info<T>,
    mut budget: Budget,
    clock: Clock,
    mut commands: mpsc::UnboundedReceiver<Command>,
    tx: watch::Sender<ChartSnapshot>,
) {
    let mut state = State {
        histories: HashMap::new(),
        chosen: None,
        resolution: Resolution::default(),
        failed: false,
        retry_at: None,
        retry_after: RETRY_FIRST,
        top_up_at: None,
    };
    let mut slow_down = SLOW_DOWN_FIRST;

    loop {
        // Take everything waiting, so the work reflects what is wanted now.
        while let Ok(command) = commands.try_recv() {
            state.apply(command);
        }
        let snapshot = state.snapshot(clock());
        tx.send_if_modified(|current| {
            let changed = *current != snapshot;
            if changed {
                *current = snapshot;
            }
            changed
        });

        // Wait for `wait`, or for a command, whichever comes first.
        macro_rules! pause {
            ($wait:expr) => {{
                tokio::select! {
                    _ = tokio::time::sleep($wait) => {}
                    command = commands.recv() => match command {
                        Some(command) => state.apply(command),
                        // The feed was dropped.
                        None => return,
                    },
                }
                continue;
            }};
        }

        let now = tokio::time::Instant::now();
        if let Some(at) = state.retry_at
            && at > now
        {
            pause!(at - now);
        }
        let ask = match state.next(clock()) {
            Ok(ask) => ask,
            Err(wait) => pause!(wait),
        };
        let (from, to) = match ask {
            Ask::Page(from, to) | Ask::TopUp(from, to) => (from, to),
        };
        let hours = (hour_of(to) - hour_of(from + HOUR_MS - 1)) / HOUR_MS + 1;
        if let Err(wait) = budget.spend(request_weight(hours)) {
            pause!(wait);
        }
        let market = state.chosen.clone().expect("something is chosen");
        let answer = info.funding_history(market.coin(), from, to).await;
        match answer {
            Err(VenueError::RateLimited) => {
                // Not this request's fault: ask again after a wait.
                let wait = slow_down;
                slow_down = (slow_down * 2).min(SLOW_DOWN_MAX);
                pause!(wait);
            }
            Err(_) => {
                slow_down = SLOW_DOWN_FIRST;
                // Whoever is chosen now sees the failure only if it is theirs.
                if state.chosen.as_ref() == Some(&market) {
                    state.failed = true;
                    state.retry_at = Some(tokio::time::Instant::now() + state.retry_after);
                    state.retry_after = (state.retry_after * 2).min(RETRY_MAX);
                }
            }
            Ok(rows) => {
                slow_down = SLOW_DOWN_FIRST;
                let rows: Vec<(u64, Decimal)> =
                    rows.into_iter().map(|r| (r.time, r.funding_rate)).collect();
                let none = rows.is_empty();
                let history = Arc::make_mut(state.histories.entry(market.clone()).or_default());
                match ask {
                    Ask::Page(..) => history.merge_page(from, rows),
                    Ask::TopUp(..) => {
                        history.extend(rows);
                        if none && state.chosen.as_ref() == Some(&market) {
                            state.top_up_at = Some(tokio::time::Instant::now() + TOP_UP_AGAIN);
                        }
                    }
                }
                if state.chosen.as_ref() == Some(&market) {
                    state.failed = false;
                    state.retry_at = None;
                    state.retry_after = RETRY_FIRST;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_page_costs_45_and_a_single_row_20() {
        assert_eq!(request_weight(500), 45.0);
        assert_eq!(request_weight(160), 28.0);
        assert_eq!(request_weight(1), 20.0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_allowance_is_three_pages_at_once() {
        let mut budget = Budget::for_funding();
        for _ in 0..3 {
            assert!(budget.spend(request_weight(500)).is_ok());
        }
        assert!(budget.spend(request_weight(500)).is_err());
    }
}
