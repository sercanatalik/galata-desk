//! The risk feed against a venue made of generated candles.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use galata_desk_core::market::history::Budget;
use galata_desk_core::market::{Interval, MarketId};
use galata_desk_core::risk::SeriesState;
use galata_desk_core::risk::feed::{RiskCandles, RiskFeed};
use galata_desk_core::venue::{Transport, VenueError};
use serde_json::{Value, json};
use tokio::sync::watch;

/// 2026-11-17 00:00 UTC.
const T0: u64 = 1_794_873_600_000;
const DAY: u64 = 86_400_000;

#[derive(Clone, Default)]
struct Venue {
    broken: Arc<Mutex<Option<String>>>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Venue {
    fn count(&self, what: &str) -> usize {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|a| *a == what)
            .count()
    }
}

impl Transport for Venue {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        assert_eq!(body["type"], "candleSnapshot", "only public candle queries");
        let req = &body["req"];
        let coin = req["coin"].as_str().unwrap();
        let interval = req["interval"].as_str().unwrap();
        self.asked
            .lock()
            .unwrap()
            .push(format!("{coin} {interval}"));
        if self.broken.lock().unwrap().as_deref() == Some(coin) {
            return Err(VenueError::Unreachable("no connection".into()));
        }
        let step = Interval::from_venue(interval).unwrap().millis();
        let (from, to) = (
            req["startTime"].as_u64().unwrap(),
            req["endTime"].as_u64().unwrap(),
        );
        let mut out = Vec::new();
        let mut t = from.div_ceil(step) * step;
        let mut px = 100u64;
        while t <= to {
            out.push(json!({ "t": t, "s": coin, "i": interval, "o": px.to_string(), "h": px.to_string(), "l": px.to_string(), "c": px.to_string() }));
            px += 1 + (t / step) % 3;
            t += step;
        }
        Ok(serde_json::to_vec(&out).unwrap())
    }
}

struct Rig {
    feed: RiskFeed,
    snapshots: watch::Receiver<RiskCandles>,
    venue: Venue,
    clock: Arc<AtomicU64>,
}

fn rig(venue: Venue) -> Rig {
    let clock = Arc::new(AtomicU64::new(T0));
    let now = clock.clone();
    let feed = RiskFeed::start(
        venue.clone(),
        Arc::new(move || now.load(Ordering::SeqCst)),
        // Plenty, so tests measure the feed rather than the budget.
        Budget::new(10_000.0, 10_000.0),
    );
    let snapshots = feed.snapshots();
    Rig {
        feed,
        snapshots,
        venue,
        clock,
    }
}

impl Rig {
    async fn settle(&mut self) -> RiskCandles {
        tokio::time::sleep(Duration::from_secs(6)).await;
        self.snapshots.borrow_and_update().clone()
    }
}

fn id(c: &str) -> MarketId {
    MarketId::new(c)
}

#[tokio::test(start_paused = true)]
async fn every_market_loads_at_every_interval() {
    let mut r = rig(Venue::default());
    r.feed.want(
        vec![id("BTC"), id("xyz:SILVER")],
        vec![Interval::D1, Interval::H8],
    );
    let s = r.settle().await;
    assert_eq!(s.series.len(), 4);
    let Some(SeriesState::Ready(c)) = s.series.get(&(id("BTC"), Interval::D1)) else {
        panic!("BTC 1d not ready")
    };
    assert!(c.len() >= 180, "{}", c.len());
    // The focused interval was asked for first.
    let asked = r.venue.asked.lock().unwrap().clone();
    assert_eq!(&asked[..2], ["BTC 1d", "xyz:SILVER 1d"]);
}

#[tokio::test(start_paused = true)]
async fn a_failing_market_is_failed_and_retried() {
    let venue = Venue::default();
    *venue.broken.lock().unwrap() = Some("xyz:NVDA".into());
    let mut r = rig(venue);
    r.feed
        .want(vec![id("BTC"), id("xyz:NVDA")], vec![Interval::D1]);
    let s = r.settle().await;
    assert_eq!(
        s.series.get(&(id("xyz:NVDA"), Interval::D1)),
        Some(&SeriesState::Failed)
    );
    assert!(matches!(
        s.series.get(&(id("BTC"), Interval::D1)),
        Some(SeriesState::Ready(_))
    ));

    *r.venue.broken.lock().unwrap() = None;
    r.clock.fetch_add(121_000, Ordering::SeqCst);
    let s = r.settle().await;
    assert!(matches!(
        s.series.get(&(id("xyz:NVDA"), Interval::D1)),
        Some(SeriesState::Ready(_))
    ));
    assert_eq!(r.venue.count("xyz:NVDA 1d"), 2);
}

#[tokio::test(start_paused = true)]
async fn a_series_is_refreshed_only_after_a_newer_candle_closes() {
    let mut r = rig(Venue::default());
    r.feed.want(vec![id("BTC")], vec![Interval::D1]);
    r.settle().await;
    // Ten minutes on, the day has not closed: no new request.
    r.clock.fetch_add(600_000, Ordering::SeqCst);
    r.settle().await;
    assert_eq!(r.venue.count("BTC 1d"), 1);
    // A day on, it has.
    r.clock.fetch_add(DAY, Ordering::SeqCst);
    r.settle().await;
    assert_eq!(r.venue.count("BTC 1d"), 2);
}

#[tokio::test(start_paused = true)]
async fn a_market_left_out_is_forgotten() {
    let mut r = rig(Venue::default());
    r.feed.want(vec![id("BTC"), id("DOGE")], vec![Interval::D1]);
    r.settle().await;
    r.feed.want(vec![id("BTC")], vec![Interval::D1]);
    let s = r.settle().await;
    assert_eq!(s.series.len(), 1);
    assert!(s.series.contains_key(&(id("BTC"), Interval::D1)));
}
