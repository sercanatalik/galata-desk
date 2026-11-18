//! The funding history feed against a venue that makes up hourly rows: what
//! it asks for, in what order, and what it shows when the venue fails.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use galata_desk_core::funding::feed::*;
use galata_desk_core::funding::history::{HOUR_MS, Resolution};
use galata_desk_core::market::MarketId;
use galata_desk_core::market::feed::Clock;
use galata_desk_core::market::history::Budget;
use galata_desk_core::venue::*;
use serde_json::Value;

/// A top of the hour. The tests start ten minutes past it.
const T0: u64 = 1_794_988_800_000;
const START: u64 = T0 + 600_000;

#[derive(Clone, Copy, PartialEq)]
enum Mood {
    Fine,
    Down,
    RateLimiting,
}

/// One question the venue was asked.
#[derive(Debug, Clone, PartialEq)]
struct Asked {
    coin: String,
    from: u64,
    to: u64,
}

#[derive(Clone)]
struct Fake {
    clock: Clock,
    mood: Arc<Mutex<Mood>>,
    asked: Arc<Mutex<Vec<Asked>>>,
    /// Markets listed only since this time.
    listed_since: Arc<Mutex<Vec<(String, u64)>>>,
    /// The venue publishes an hour's row this long after the hour.
    publishes_after: Arc<Mutex<u64>>,
}

impl Fake {
    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().unwrap().clone()
    }
    fn set(&self, mood: Mood) {
        *self.mood.lock().unwrap() = mood;
    }
}

impl Transport for Fake {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        assert_eq!(body["type"], "fundingHistory");
        let coin = body["coin"].as_str().unwrap().to_string();
        let from = body["startTime"].as_u64().unwrap();
        let to = body["endTime"].as_u64().unwrap();
        self.asked.lock().unwrap().push(Asked {
            coin: coin.clone(),
            from,
            to,
        });
        match *self.mood.lock().unwrap() {
            Mood::Down => return Err(VenueError::Unreachable("no connection".into())),
            Mood::RateLimiting => return Err(VenueError::RateLimited),
            Mood::Fine => {}
        }
        let listed = self
            .listed_since
            .lock()
            .unwrap()
            .iter()
            .find(|(c, _)| *c == coin)
            .map(|(_, since)| *since)
            .unwrap_or(0);
        let published_until = (self.clock)() - *self.publishes_after.lock().unwrap();
        // Hourly rows stamped 47 ms past the hour, oldest first, 500 at most.
        let first_hour = from.max(listed).div_ceil(HOUR_MS);
        let rows: Vec<String> = (first_hour..)
            .map(|h| h * HOUR_MS + 47)
            .take_while(|t| *t <= to && *t <= published_until)
            .take(500)
            .map(|t| {
                format!(r#"{{"coin":"{coin}","fundingRate":"0.00001","premium":"0.0","time":{t}}}"#)
            })
            .collect();
        Ok(format!("[{}]", rows.join(",")).into_bytes())
    }
}

struct Venue {
    fake: Fake,
    feed: HistoryFeed,
}

fn venue() -> Venue {
    // The wall clock follows the test's paused clock.
    let began = tokio::time::Instant::now();
    let clock: Clock = Arc::new(move || START + began.elapsed().as_millis() as u64);
    let fake = Fake {
        clock: clock.clone(),
        mood: Arc::new(Mutex::new(Mood::Fine)),
        asked: Arc::default(),
        listed_since: Arc::default(),
        publishes_after: Arc::new(Mutex::new(1_000)),
    };
    let feed = HistoryFeed::start(Info::new(fake.clone()), Budget::for_funding(), clock);
    Venue { fake, feed }
}

fn market(coin: &str) -> Option<MarketId> {
    Some(MarketId::new(coin))
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test(start_paused = true)]
async fn nothing_is_asked_until_a_market_is_chosen() {
    let v = venue();
    tokio::time::sleep(Duration::from_secs(600)).await;
    assert!(v.fake.asked().is_empty());
    assert_eq!(*v.feed.snapshots().borrow(), ChartSnapshot::default());
}

#[tokio::test(start_paused = true)]
async fn pages_are_asked_newest_first_until_the_window_is_covered() {
    let v = venue();
    v.feed.choose(market("xyz:NVDA"));
    settle().await;

    // Three pages fit the allowance at once; the chart can already draw.
    let asked = v.fake.asked();
    assert_eq!(asked.len(), 3);
    assert!(asked.iter().all(|a| a.coin == "xyz:NVDA"));
    assert_eq!(asked[0].from, T0 - 499 * HOUR_MS);
    assert_eq!(asked[0].to, T0 + HOUR_MS - 1);
    assert_eq!(asked[1].to, asked[0].from - 1);
    assert_eq!(asked[2].to, asked[1].from - 1);
    let part = v.feed.snapshots().borrow().clone();
    assert_eq!(part.history.len(), 1500);
    assert!(part.loading && !part.failed);
    assert_eq!(part.market, market("xyz:NVDA"));

    // The rest follows as the allowance refills, and then nothing more.
    tokio::time::sleep(Duration::from_secs(120)).await;
    let asked = v.fake.asked();
    assert_eq!(asked.len(), 5);
    assert_eq!(asked[4].from, T0 - 2159 * HOUR_MS);
    let all = v.feed.snapshots().borrow().clone();
    assert_eq!(all.history.len(), 2160);
    assert!(!all.loading);
    assert_eq!(all.history.window(Resolution::Day).buckets.len(), 90);
}

#[tokio::test(start_paused = true)]
async fn the_hourly_window_is_one_request_and_a_wider_one_asks_for_the_rest() {
    let v = venue();
    v.feed.set_resolution(Resolution::Hour);
    v.feed.choose(market("BTC"));
    settle().await;
    assert_eq!(v.fake.asked().len(), 1);
    assert_eq!(v.fake.asked()[0].from, T0 - 167 * HOUR_MS);
    assert!(!v.feed.snapshots().borrow().loading);

    v.feed.set_resolution(Resolution::EightHours);
    settle().await;
    let asked = v.fake.asked();
    // 720 hours: the 552 before the 168 already held, in two requests.
    assert_eq!(asked.len(), 3);
    assert_eq!(asked[1].to, T0 - 167 * HOUR_MS - 1);
    assert_eq!(asked[2].from, T0 - 719 * HOUR_MS);
    assert_eq!(v.feed.snapshots().borrow().history.len(), 720);

    // Going back needs nothing.
    v.feed.set_resolution(Resolution::Hour);
    settle().await;
    assert_eq!(v.fake.asked().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn choosing_another_market_abandons_the_first_ones_pages_and_keeps_its_rows() {
    let v = venue();
    v.feed.choose(market("BTC"));
    settle().await;
    assert_eq!(v.fake.asked().len(), 3);

    v.feed.set_resolution(Resolution::Hour);
    v.feed.choose(market("ETH"));
    tokio::time::sleep(Duration::from_secs(300)).await;
    let asked = v.fake.asked();
    // BTC's two remaining pages are never asked: only ETH's one.
    assert_eq!(asked.len(), 4);
    assert_eq!(asked[3].coin, "ETH");
    assert_eq!(v.feed.snapshots().borrow().history.len(), 168);

    // BTC's 1,500 hours are still there, at once and without a request.
    v.feed.choose(market("BTC"));
    settle().await;
    let back = v.feed.snapshots().borrow().clone();
    assert_eq!(back.market, market("BTC"));
    assert_eq!(back.history.len(), 1500);
    assert!(!back.loading, "the hourly window is covered");
    assert_eq!(v.fake.asked().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn a_young_market_stops_at_its_first_hour() {
    let v = venue();
    // Listed 28 days ago.
    let since = T0 - 28 * 24 * HOUR_MS;
    v.fake
        .listed_since
        .lock()
        .unwrap()
        .push(("xyz:NEW".into(), since));
    v.feed.choose(market("xyz:NEW"));
    tokio::time::sleep(Duration::from_secs(300)).await;
    // The newest page is full; the second comes back short, and that is all.
    assert_eq!(v.fake.asked().len(), 2);
    let snapshot = v.feed.snapshots().borrow().clone();
    assert!(!snapshot.loading);
    assert_eq!(snapshot.history.starts_ms(), Some(since));
    assert_eq!(snapshot.history.window(Resolution::Day).buckets.len(), 28);
}

#[tokio::test(start_paused = true)]
async fn a_failure_is_shown_and_retried_and_a_success_clears_it() {
    let v = venue();
    v.fake.set(Mood::Down);
    v.feed.set_resolution(Resolution::Hour);
    v.feed.choose(market("BTC"));
    settle().await;
    let failed = v.feed.snapshots().borrow().clone();
    assert!(failed.failed && failed.loading);
    assert!(failed.history.is_empty());
    assert_eq!(v.fake.asked().len(), 1);

    // It is tried again, less and less often.
    tokio::time::sleep(Duration::from_secs(40)).await;
    let tries = v.fake.asked().len();
    assert!((3..=4).contains(&tries), "{tries}");

    v.fake.set(Mood::Fine);
    tokio::time::sleep(Duration::from_secs(120)).await;
    let recovered = v.feed.snapshots().borrow().clone();
    assert!(!recovered.failed && !recovered.loading);
    assert_eq!(recovered.history.len(), 168);
}

#[tokio::test(start_paused = true)]
async fn a_failure_does_not_follow_to_the_next_market_chosen() {
    let v = venue();
    v.fake.set(Mood::Down);
    v.feed.set_resolution(Resolution::Hour);
    v.feed.choose(market("BTC"));
    settle().await;
    assert!(v.feed.snapshots().borrow().failed);

    v.fake.set(Mood::Fine);
    v.feed.choose(market("ETH"));
    settle().await;
    let eth = v.feed.snapshots().borrow().clone();
    assert!(!eth.failed);
    assert_eq!(eth.history.len(), 168);
}

#[tokio::test(start_paused = true)]
async fn being_told_to_slow_down_is_waited_out_and_not_reported_as_a_failure() {
    let v = venue();
    v.fake.set(Mood::RateLimiting);
    v.feed.set_resolution(Resolution::Hour);
    v.feed.choose(market("BTC"));
    settle().await;
    assert_eq!(v.fake.asked().len(), 1);
    let waiting = v.feed.snapshots().borrow().clone();
    assert!(!waiting.failed && waiting.loading);

    // Nothing more for fifteen seconds, then the same request again.
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(v.fake.asked().len(), 1);
    v.fake.set(Mood::Fine);
    tokio::time::sleep(Duration::from_secs(10)).await;
    let asked = v.fake.asked();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0], asked[1]);
    assert_eq!(v.feed.snapshots().borrow().history.len(), 168);
}

#[tokio::test(start_paused = true)]
async fn the_next_hours_rate_is_asked_once_after_the_hour_turns_and_not_before() {
    let v = venue();
    v.feed.set_resolution(Resolution::Hour);
    v.feed.choose(market("BTC"));
    settle().await;
    assert_eq!(v.fake.asked().len(), 1);
    assert_eq!(v.feed.snapshots().borrow().history.last_ms(), Some(T0));

    // Ten minutes past the hour now; nothing is asked for the 49 that remain.
    tokio::time::sleep(Duration::from_secs(49 * 60)).await;
    assert_eq!(v.fake.asked().len(), 1);

    // The hour turns: one request, for that hour, a little after it.
    tokio::time::sleep(Duration::from_secs(2 * 60)).await;
    let asked = v.fake.asked();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[1].from, T0 + HOUR_MS);
    assert_eq!(asked[1].to, T0 + 2 * HOUR_MS - 1);
    let snapshot = v.feed.snapshots().borrow().clone();
    assert_eq!(snapshot.history.last_ms(), Some(T0 + HOUR_MS));
    assert_eq!(snapshot.history.len(), 169);

    // And nothing more until the next one.
    tokio::time::sleep(Duration::from_secs(50 * 60)).await;
    assert_eq!(v.fake.asked().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn an_hour_the_venue_has_not_published_is_asked_for_again_a_minute_later() {
    let v = venue();
    // The venue is three minutes late with each hour's row.
    *v.fake.publishes_after.lock().unwrap() = 180_000;
    v.feed.set_resolution(Resolution::Hour);
    v.feed.choose(market("BTC"));
    settle().await;
    assert_eq!(v.fake.asked().len(), 1);

    // To four minutes past the next hour: asked at 20 s, 80 s, 140 s, 200 s.
    tokio::time::sleep(Duration::from_secs(54 * 60)).await;
    let asked = v.fake.asked();
    assert_eq!(asked.len(), 5, "{asked:?}");
    assert_eq!(
        v.feed.snapshots().borrow().history.last_ms(),
        Some(T0 + HOUR_MS)
    );
    // Within five minutes of the hour, and nothing after it arrived.
    tokio::time::sleep(Duration::from_secs(20 * 60)).await;
    assert_eq!(v.fake.asked().len(), 5);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_feed_stops_the_asking() {
    let v = venue();
    v.feed.choose(market("BTC"));
    settle().await;
    let before = v.fake.asked().len();
    drop(v.feed);
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(v.fake.asked().len(), before);
}
