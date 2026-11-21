//! Candle history, loaded over REST on a budget.
//!
//! The stream sends only the current candle, so the candles before it are
//! asked for once per market and interval. That costs REST weight, which the
//! Portfolio section and galata-datawatch also draw on, so it is rationed: a
//! burst to fill a small watchlist at once, then a steady pace.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::candles::Candle;
use super::{Interval, MarketId};
use crate::venue::{Info, Transport, VenueError};

/// What one history request costs: 20, plus a little for the candles returned.
pub const REQUEST_WEIGHT: f64 = 22.0;

/// A token bucket of REST weight.
#[derive(Debug, Clone)]
pub struct Budget {
    capacity: f64,
    per_minute: f64,
    tokens: f64,
    at: Instant,
}

impl Budget {
    /// `capacity` may be spent at once; it refills at `per_minute`.
    pub fn new(capacity: f64, per_minute: f64) -> Self {
        Self {
            capacity,
            per_minute,
            tokens: capacity,
            at: Instant::now(),
        }
    }

    /// The history loader's allowance: five requests at once, then about
    /// eleven a minute.
    ///
    /// Measured on 2026-11-17 with a larger allowance (200 at once, 400 a
    /// minute): the app's first minute cost 1,060 of the venue's 1,200, half
    /// of it the Portfolio section's own start. This keeps the first minute
    /// near 870 and leaves room for other programs on the same address.
    pub fn for_history() -> Self {
        Self::new(110.0, 240.0)
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let earned = now.duration_since(self.at).as_secs_f64() / 60.0 * self.per_minute;
        self.tokens = (self.tokens + earned).min(self.capacity);
        self.at = now;
    }

    /// Spend `weight` if it is there. Otherwise, how long until it will be.
    pub fn spend(&mut self, weight: f64) -> Result<(), Duration> {
        self.refill();
        if self.tokens >= weight {
            self.tokens -= weight;
            Ok(())
        } else {
            let short = weight - self.tokens;
            Err(Duration::from_secs_f64(short / self.per_minute * 60.0))
        }
    }
}

/// A request for one market's history at one interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub market: MarketId,
    pub interval: Interval,
    pub from_ms: u64,
    pub to_ms: u64,
}

/// What came back. An error means the loader gave up on this request; it does
/// not give up on rate limiting, only waits.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub market: MarketId,
    pub interval: Interval,
    pub candles: Result<Vec<Candle>, VenueError>,
}

/// How long to stop after the venue says to slow down; doubles while it keeps
/// saying so.
const SLOW_DOWN_FIRST: Duration = Duration::from_secs(15);
const SLOW_DOWN_MAX: Duration = Duration::from_secs(120);

enum Ask {
    Load(Request),
    /// Forget queued requests for anything else: the period changed, or a
    /// chart closed, and nobody is waiting for them any more.
    KeepOnly(HashSet<(MarketId, Interval)>),
    /// Serve this market's queued request next: someone is looking at it.
    First(MarketId, Interval),
}

/// A running loader. Requests are served in the order they arrive.
pub struct Loader {
    requests: mpsc::UnboundedSender<Ask>,
    task: JoinHandle<()>,
}

impl Loader {
    pub fn start<T: Transport>(
        info: Info<T>,
        budget: Budget,
    ) -> (Self, mpsc::UnboundedReceiver<Loaded>) {
        let (requests, rx) = mpsc::unbounded_channel();
        let (tx, loaded) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(info, budget, rx, tx));
        (Self { requests, task }, loaded)
    }

    pub fn request(&self, request: Request) {
        let _ = self.requests.send(Ask::Load(request));
    }

    /// Move a market's queued request to the front. Nothing happens if it is
    /// not queued.
    pub fn prioritise(&self, market: MarketId, interval: Interval) {
        let _ = self.requests.send(Ask::First(market, interval));
    }

    /// Drop queued requests for any market and interval not in `wanted`.
    pub fn keep_only(&self, wanted: HashSet<(MarketId, Interval)>) {
        let _ = self.requests.send(Ask::KeepOnly(wanted));
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run<T: Transport>(
    info: Info<T>,
    mut budget: Budget,
    mut requests: mpsc::UnboundedReceiver<Ask>,
    loaded: mpsc::UnboundedSender<Loaded>,
) {
    let mut queue: VecDeque<Request> = VecDeque::new();
    let mut slow_down = SLOW_DOWN_FIRST;
    loop {
        // Take everything waiting, so the queue reflects what is wanted now.
        while let Ok(ask) = requests.try_recv() {
            take(&mut queue, ask);
        }
        let Some(request) = queue.front().cloned() else {
            match requests.recv().await {
                Some(ask) => take(&mut queue, ask),
                None => return,
            }
            continue;
        };
        if let Err(wait) = budget.spend(REQUEST_WEIGHT) {
            tokio::time::sleep(wait).await;
            continue;
        }
        let answer = info
            .candle_snapshot(
                request.market.coin(),
                request.interval.as_str(),
                request.from_ms,
                request.to_ms,
            )
            .await;
        match answer {
            Err(VenueError::RateLimited) => {
                // Not this request's fault: keep it at the front and wait.
                tokio::time::sleep(slow_down).await;
                slow_down = (slow_down * 2).min(SLOW_DOWN_MAX);
            }
            answer => {
                slow_down = SLOW_DOWN_FIRST;
                queue.pop_front();
                let _ = loaded.send(Loaded {
                    market: request.market,
                    interval: request.interval,
                    candles: answer.map(|wire| wire.into_iter().map(Candle::from).collect()),
                });
            }
        }
    }
}

fn take(queue: &mut VecDeque<Request>, ask: Ask) {
    match ask {
        Ask::Load(request) => queue.push_back(request),
        Ask::KeepOnly(wanted) => queue.retain(|r| wanted.contains(&(r.market.clone(), r.interval))),
        Ask::First(market, interval) => {
            if let Some(at) = queue
                .iter()
                .position(|r| r.market == market && r.interval == interval)
                && let Some(request) = queue.remove(at)
            {
                queue.push_front(request);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_burst_is_allowed_then_a_steady_pace() {
        let mut budget = Budget::new(200.0, 400.0);
        // Nine requests of 22 fit in 200.
        for _ in 0..9 {
            assert!(budget.spend(REQUEST_WEIGHT).is_ok());
        }
        let wait = budget.spend(REQUEST_WEIGHT).unwrap_err();
        // 2 of weight left, 20 short, at 400 a minute: three seconds.
        assert!((wait.as_secs_f64() - 3.0).abs() < 0.01, "{wait:?}");

        tokio::time::advance(wait).await;
        assert!(budget.spend(REQUEST_WEIGHT).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn the_budget_never_grows_past_its_capacity() {
        let mut budget = Budget::new(200.0, 400.0);
        tokio::time::advance(Duration::from_secs(3600)).await;
        for _ in 0..9 {
            assert!(budget.spend(REQUEST_WEIGHT).is_ok());
        }
        assert!(budget.spend(REQUEST_WEIGHT).is_err());
    }
}
