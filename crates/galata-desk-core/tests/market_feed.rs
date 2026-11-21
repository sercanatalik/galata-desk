//! The market feed against a scripted stream and a venue made of generated
//! candles: what it subscribes to, what it loads, and what it publishes.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use galata_desk_core::market::candles::Candle;
use galata_desk_core::market::feed::*;
use galata_desk_core::market::history::Budget;
use galata_desk_core::market::ws::{Event, Sub, Subscriptions};
use galata_desk_core::market::{Interval, MarketId, MarketPeriod};
use galata_desk_core::venue::{AssetCtx, Transport, VenueError};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

/// A round time to count candles from: 2026-11-17 00:00 UTC.
const T0: u64 = 1_794_873_600_000;

/// Records every change of subscription.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<(bool, Sub)>>>);

impl Recorder {
    fn take(&self) -> Vec<(bool, Sub)> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
    fn wanted(&self) -> Vec<Sub> {
        let mut wanted: Vec<Sub> = Vec::new();
        for (on, sub) in self.0.lock().unwrap().iter() {
            if *on {
                wanted.push(sub.clone());
            } else {
                wanted.retain(|s| s != sub);
            }
        }
        wanted
    }
}

impl Subscriptions for Recorder {
    fn subscribe(&self, sub: Sub) {
        self.0.lock().unwrap().push((true, sub));
    }
    fn unsubscribe(&self, sub: Sub) {
        self.0.lock().unwrap().push((false, sub));
    }
}

/// A venue that answers history with one candle per interval, each closing
/// one higher than the last, and the list of markets from its fixture.
#[derive(Clone)]
struct Venue {
    /// While true, history requests wait.
    hold: Arc<AtomicBool>,
    /// How many of the next history requests are answered "slow down".
    slow_downs: Arc<AtomicU64>,
    listing_fails: Arc<AtomicBool>,
    /// A coin whose history requests fail while it is named here.
    broken: Arc<Mutex<Option<String>>>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Venue {
    fn new() -> Self {
        Self {
            hold: Arc::default(),
            slow_downs: Arc::default(),
            listing_fails: Arc::default(),
            broken: Arc::default(),
            asked: Arc::default(),
        }
    }
    fn count(&self, what: &str) -> usize {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.starts_with(what))
            .count()
    }
}

impl Transport for Venue {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        match body["type"].as_str().unwrap() {
            "allPerpMetas" => {
                self.asked.lock().unwrap().push("allPerpMetas".into());
                if self.listing_fails.load(Ordering::SeqCst) {
                    return Err(VenueError::Unreachable("no connection".into()));
                }
                // The main dex lists four majors, forty numbered markets and
                // one delisted one; the `xyz` dex lists two.
                let market = |name: String, lev: u32| json!({ "name": name, "szDecimals": 2, "maxLeverage": lev });
                let mut main = vec![
                    market("BTC".into(), 40),
                    market("ETH".into(), 25),
                    market("SOL".into(), 20),
                    market("HYPE".into(), 10),
                ];
                main.extend((0..40).map(|i| market(format!("M{i}"), 5)));
                main.push(json!({ "name": "MATIC", "szDecimals": 1, "maxLeverage": 20, "isDelisted": true }));
                let xyz = vec![
                    market("xyz:NVDA".into(), 10),
                    market("xyz:SILVER".into(), 20),
                ];
                Ok(
                    serde_json::to_vec(&json!([{ "universe": main }, { "universe": xyz }]))
                        .unwrap(),
                )
            }
            "candleSnapshot" => {
                let req = &body["req"];
                let coin = req["coin"].as_str().unwrap();
                let interval = req["interval"].as_str().unwrap();
                self.asked
                    .lock()
                    .unwrap()
                    .push(format!("candleSnapshot {coin} {interval}"));
                while self.hold.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                if self.broken.lock().unwrap().as_deref() == Some(coin) {
                    return Err(VenueError::Unreachable("no connection".into()));
                }
                if self.slow_downs.load(Ordering::SeqCst) > 0 {
                    self.slow_downs.fetch_sub(1, Ordering::SeqCst);
                    return Err(VenueError::RateLimited);
                }
                let step = Interval::from_venue(interval).unwrap().millis();
                let (from, to) = (
                    req["startTime"].as_u64().unwrap(),
                    req["endTime"].as_u64().unwrap(),
                );
                let mut candles = Vec::new();
                let mut t = from.div_ceil(step) * step;
                let mut px = 100u64;
                while t <= to {
                    candles.push(json!({
                        "t": t, "s": coin, "i": interval,
                        "o": px.to_string(), "h": (px + 1).to_string(),
                        "l": px.to_string(), "c": (px + 1).to_string(),
                    }));
                    px += 1;
                    t += step;
                }
                Ok(serde_json::to_vec(&candles).unwrap())
            }
            other => panic!("unexpected query {other}"),
        }
    }
}

fn id(coin: &str) -> MarketId {
    MarketId::new(coin)
}

fn ctx(mark: &str, mid: Option<&str>) -> AssetCtx {
    AssetCtx {
        mark_px: mark.parse().unwrap(),
        mid_px: mid.map(|m| m.parse().unwrap()),
        oracle_px: "100".parse().unwrap(),
        premium: Some("0.001".parse().unwrap()),
        funding: "0.00001".parse().unwrap(),
        open_interest: "10".parse().unwrap(),
        day_ntl_vlm: "1000000".parse().unwrap(),
    }
}

struct Rig {
    feed: MarketFeed,
    snapshots: watch::Receiver<MarketSnapshot>,
    events: mpsc::UnboundedSender<Event>,
    subs: Recorder,
    venue: Venue,
    clock: Arc<AtomicU64>,
}

fn rig(watched: &[&str]) -> Rig {
    rig_with(Venue::new(), watched)
}

fn rig_with(venue: Venue, watched: &[&str]) -> Rig {
    let subs = Recorder::default();
    let (events, rx) = mpsc::unbounded_channel();
    let clock = Arc::new(AtomicU64::new(T0));
    let now = clock.clone();
    let feed = MarketFeed::start(
        venue.clone(),
        subs.clone(),
        rx,
        Arc::new(move || now.load(Ordering::SeqCst)),
        Budget::for_history(),
        MarketPeriod::Day,
        watched.iter().map(|c| id(c)).collect(),
    );
    let snapshots = feed.snapshots();
    Rig {
        feed,
        snapshots,
        events,
        subs,
        venue,
        clock,
    }
}

impl Rig {
    /// Let the feed work and return what it last published.
    async fn settle(&mut self) -> MarketSnapshot {
        tokio::time::sleep(Duration::from_millis(700)).await;
        self.snapshots.borrow_and_update().clone()
    }
}

#[tokio::test(start_paused = true)]
async fn each_watched_market_gets_two_subscriptions_and_its_history() {
    let mut r = rig(&["BTC", "xyz:NVDA"]);
    let snapshot = r.settle().await;
    assert_eq!(
        r.subs.wanted(),
        [
            Sub::Ctx(id("BTC")),
            Sub::Candles(id("BTC"), Interval::M15),
            Sub::Ctx(id("xyz:NVDA")),
            Sub::Candles(id("xyz:NVDA"), Interval::M15),
        ]
    );
    assert_eq!(r.venue.count("candleSnapshot BTC 15m"), 1);
    assert_eq!(r.venue.count("candleSnapshot xyz:NVDA 15m"), 1);
    // Tiles come in watchlist order, with history and no live price yet.
    let markets: Vec<_> = snapshot.tiles.iter().map(|t| t.market.clone()).collect();
    assert_eq!(markets, [id("BTC"), id("xyz:NVDA")]);
    assert!(
        snapshot
            .tiles
            .iter()
            .all(|t| t.live.is_none() && t.stats.is_some())
    );
}

#[tokio::test(start_paused = true)]
async fn a_live_price_shows_before_history_arrives() {
    let venue = Venue::new();
    venue.hold.store(true, Ordering::SeqCst);
    let mut r = rig_with(venue, &["BTC"]);
    r.events
        .send(Event::Ctx(id("BTC"), ctx("101", Some("100.5"))))
        .unwrap();
    let snapshot = r.settle().await;
    let tile = &snapshot.tiles[0];
    let live = tile.live.as_ref().expect("the live price arrived");
    assert_eq!(live.last.to_string(), "100.5", "the mid is the last price");
    assert_eq!(live.open_interest_usd, Decimal::from(1010));
    assert!(tile.stats.is_none(), "history has not arrived");

    r.venue.hold.store(false, Ordering::SeqCst);
    let snapshot = r.settle().await;
    let stats = snapshot.tiles[0].stats.as_ref().expect("history arrived");
    // 97 candles opening at 100, 101, ...; the window keeps 96 and the live
    // price is 100.5.
    assert_eq!(stats.start.to_string(), "101");
    assert!(stats.change < 0.0);
    assert!(stats.volatility.is_some());
}

#[tokio::test(start_paused = true)]
async fn with_no_mid_the_mark_is_the_last_price() {
    let mut r = rig(&["BTC"]);
    r.events
        .send(Event::Ctx(id("BTC"), ctx("101", None)))
        .unwrap();
    let snapshot = r.settle().await;
    assert_eq!(
        snapshot.tiles[0].live.as_ref().unwrap().last.to_string(),
        "101"
    );
}

#[tokio::test(start_paused = true)]
async fn a_live_candle_extends_the_range() {
    let mut r = rig(&["BTC"]);
    r.settle().await;
    r.events
        .send(Event::Candle(
            id("BTC"),
            Interval::M15,
            Candle {
                open_ms: T0,
                open: "196".parse().unwrap(),
                high: "250".parse().unwrap(),
                low: "196".parse().unwrap(),
                close: "240".parse().unwrap(),
            },
        ))
        .unwrap();
    let snapshot = r.settle().await;
    assert_eq!(
        snapshot.tiles[0].stats.as_ref().unwrap().high.to_string(),
        "250"
    );
}

#[tokio::test(start_paused = true)]
async fn unwatching_stops_a_markets_subscriptions_and_removes_its_tile() {
    let mut r = rig(&["BTC", "ETH"]);
    r.settle().await;
    r.feed.unwatch(id("BTC"));
    let snapshot = r.settle().await;
    assert_eq!(snapshot.tiles.len(), 1);
    assert_eq!(snapshot.tiles[0].market, id("ETH"));
    assert_eq!(
        r.subs.wanted(),
        [Sub::Ctx(id("ETH")), Sub::Candles(id("ETH"), Interval::M15)]
    );
    // Figures that still arrive for it are not kept.
    r.events
        .send(Event::Ctx(id("BTC"), ctx("1", None)))
        .unwrap();
    assert_eq!(r.settle().await.tiles.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn watching_adds_a_tile_at_the_end() {
    let mut r = rig(&["BTC"]);
    r.settle().await;
    r.feed.watch(id("SOL"));
    r.feed.watch(id("SOL"));
    let snapshot = r.settle().await;
    let markets: Vec<_> = snapshot.tiles.iter().map(|t| t.market.clone()).collect();
    assert_eq!(markets, [id("BTC"), id("SOL")]);
    assert_eq!(r.venue.count("candleSnapshot SOL"), 1);
}

#[tokio::test(start_paused = true)]
async fn changing_the_period_moves_the_candle_stream_and_loads_history_once() {
    let mut r = rig(&["BTC"]);
    r.settle().await;
    r.subs.take();

    r.feed.set_period(MarketPeriod::Week);
    let snapshot = r.settle().await;
    assert_eq!(snapshot.period, MarketPeriod::Week);
    assert_eq!(
        r.subs.take(),
        [
            (false, Sub::Candles(id("BTC"), Interval::M15)),
            (true, Sub::Candles(id("BTC"), Interval::H2)),
        ]
    );
    assert_eq!(r.venue.count("candleSnapshot BTC 2h"), 1);
    assert!(snapshot.tiles[0].stats.is_some());

    // Back to a period already loaded, straight away: nothing is asked again.
    r.feed.set_period(MarketPeriod::Day);
    let snapshot = r.settle().await;
    assert_eq!(r.venue.count("candleSnapshot BTC 15m"), 1);
    assert!(snapshot.tiles[0].stats.is_some());
}

#[tokio::test(start_paused = true)]
async fn a_period_left_for_a_long_time_is_loaded_again() {
    let mut r = rig(&["BTC"]);
    r.settle().await;
    r.feed.set_period(MarketPeriod::Week);
    r.settle().await;
    // An hour passes on another period; the 15-minute series has gone stale.
    r.clock.fetch_add(3_600_000, Ordering::SeqCst);
    r.feed.set_period(MarketPeriod::Day);
    r.settle().await;
    assert_eq!(r.venue.count("candleSnapshot BTC 15m"), 2);
}

#[tokio::test(start_paused = true)]
async fn a_burst_of_messages_publishes_at_most_twice_a_second() {
    let mut r = rig(&["BTC"]);
    r.settle().await;
    let mut published = 0;
    // A hundred price changes over two seconds.
    for i in 0..100u32 {
        r.events
            .send(Event::Ctx(id("BTC"), ctx(&(100 + i).to_string(), None)))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        if r.snapshots.has_changed().unwrap() {
            r.snapshots.borrow_and_update();
            published += 1;
        }
    }
    assert!(
        (3..=5).contains(&published),
        "published {published} times in two seconds"
    );
}

#[tokio::test(start_paused = true)]
async fn the_connection_state_follows_the_stream() {
    let mut r = rig(&["BTC"]);
    assert_eq!(r.settle().await.connection, Connection::Connecting);

    r.events.send(Event::Connected).unwrap();
    assert_eq!(r.settle().await.connection, Connection::Live);

    r.events.send(Event::Disconnected).unwrap();
    let down = r.settle().await;
    assert!(matches!(down.connection, Connection::Down { .. }));
    // The last figures stay.
    assert!(down.tiles[0].stats.is_some());

    r.events.send(Event::Connected).unwrap();
    assert_eq!(r.settle().await.connection, Connection::Live);
}

#[tokio::test(start_paused = true)]
async fn a_short_silence_reloads_nothing_and_a_long_one_reloads_history() {
    let mut r = rig(&["BTC"]);
    r.events.send(Event::Connected).unwrap();
    r.events
        .send(Event::Ctx(id("BTC"), ctx("100", None)))
        .unwrap();
    r.settle().await;
    assert_eq!(r.venue.count("candleSnapshot"), 1);

    // Five minutes down, with 15-minute candles: the stream fills the gap.
    r.events.send(Event::Disconnected).unwrap();
    r.settle().await;
    r.clock.fetch_add(5 * 60_000, Ordering::SeqCst);
    r.events.send(Event::Connected).unwrap();
    r.events
        .send(Event::Ctx(id("BTC"), ctx("101", None)))
        .unwrap();
    r.settle().await;
    assert_eq!(r.venue.count("candleSnapshot"), 1);

    // Forty minutes down: candles were missed.
    r.events.send(Event::Disconnected).unwrap();
    r.settle().await;
    r.clock.fetch_add(40 * 60_000, Ordering::SeqCst);
    r.events.send(Event::Connected).unwrap();
    r.events
        .send(Event::Ctx(id("BTC"), ctx("102", None)))
        .unwrap();
    r.settle().await;
    assert_eq!(r.venue.count("candleSnapshot"), 2);
}

/// A machine that sleeps notices the dead connection only after it wakes, a
/// moment before it reconnects. The hole in history is as long as the sleep.
#[tokio::test(start_paused = true)]
async fn a_sleep_is_a_long_silence_however_late_the_drop_is_noticed() {
    let mut r = rig(&["BTC", "ETH"]);
    r.events.send(Event::Connected).unwrap();
    r.events
        .send(Event::Ctx(id("BTC"), ctx("100", None)))
        .unwrap();
    r.settle().await;
    assert_eq!(r.venue.count("candleSnapshot"), 2);

    // Asleep for an hour. On waking the drop is noticed and mended at once.
    r.clock.fetch_add(60 * 60_000, Ordering::SeqCst);
    r.events.send(Event::Disconnected).unwrap();
    r.events.send(Event::Connected).unwrap();
    r.events
        .send(Event::Ctx(id("BTC"), ctx("103", None)))
        .unwrap();
    r.settle().await;
    assert_eq!(r.venue.count("candleSnapshot"), 4, "both markets reload");
}

/// Even if the connection survives the silence.
#[tokio::test(start_paused = true)]
async fn a_long_silence_on_a_connection_that_held_also_reloads_history() {
    let mut r = rig(&["BTC"]);
    r.events.send(Event::Connected).unwrap();
    r.events
        .send(Event::Ctx(id("BTC"), ctx("100", None)))
        .unwrap();
    r.settle().await;
    r.clock.fetch_add(20 * 60_000, Ordering::SeqCst);
    r.events
        .send(Event::Ctx(id("BTC"), ctx("101", None)))
        .unwrap();
    r.settle().await;
    assert_eq!(r.venue.count("candleSnapshot"), 2);
}

#[tokio::test(start_paused = true)]
async fn the_list_of_markets_gives_leverage_and_marks_a_delisted_market() {
    // MATIC is in the fixture as delisted; DOGE is not in it at all.
    let mut r = rig(&["BTC", "MATIC", "DOGE", "xyz:NVDA"]);
    let snapshot = r.settle().await;
    let Listing::Ready(listed) = &snapshot.listing else {
        panic!("the list should have loaded");
    };
    assert!(listed.iter().any(|l| l.market == id("xyz:NVDA")));
    assert!(
        listed.iter().all(|l| l.market != id("MATIC")),
        "delisted markets are not offered"
    );
    let tile = |coin: &str| {
        snapshot
            .tiles
            .iter()
            .find(|t| t.market == id(coin))
            .unwrap()
    };
    assert_eq!(tile("BTC").max_leverage, Some(40));
    assert!(!tile("BTC").unavailable);
    assert!(tile("MATIC").unavailable);
    assert!(tile("DOGE").unavailable);
    assert!(!tile("xyz:NVDA").unavailable);
}

#[tokio::test(start_paused = true)]
async fn a_list_that_fails_to_load_marks_nothing_unavailable_and_can_be_retried() {
    let venue = Venue::new();
    venue.listing_fails.store(true, Ordering::SeqCst);
    let mut r = rig_with(venue, &["BTC"]);
    let snapshot = r.settle().await;
    assert_eq!(snapshot.listing, Listing::Failed);
    assert!(!snapshot.tiles[0].unavailable);
    // Without the list nothing is subscribed to: a market the venue does not
    // list would close the whole connection.
    assert!(r.subs.wanted().is_empty());
    assert_eq!(r.venue.count("candleSnapshot"), 0);

    r.venue.listing_fails.store(false, Ordering::SeqCst);
    r.feed.refresh_listing();
    let snapshot = r.settle().await;
    assert!(matches!(snapshot.listing, Listing::Ready(_)));
    assert_eq!(r.venue.count("allPerpMetas"), 2);
    // With the list in hand the market is subscribed to and loaded.
    assert_eq!(r.subs.wanted().len(), 2);
    assert!(snapshot.tiles[0].stats.is_some());

    // A list in hand and fresh is not asked for again.
    r.feed.refresh_listing();
    r.settle().await;
    assert_eq!(r.venue.count("allPerpMetas"), 2);
}

#[tokio::test(start_paused = true)]
async fn history_waits_out_a_slow_down_and_then_loads() {
    let venue = Venue::new();
    venue.slow_downs.store(2, Ordering::SeqCst);
    let mut r = rig_with(venue, &["BTC"]);
    let snapshot = r.settle().await;
    assert!(
        snapshot.tiles[0].stats.is_none(),
        "still waiting, and no error to show"
    );

    // 15 seconds, then 30: within a minute it has loaded.
    tokio::time::sleep(Duration::from_secs(60)).await;
    let snapshot = r.settle().await;
    assert!(snapshot.tiles[0].stats.is_some());
    assert_eq!(r.venue.count("candleSnapshot"), 3);
}

#[tokio::test(start_paused = true)]
async fn many_markets_load_in_a_burst_and_then_at_a_pace() {
    let coins: Vec<String> = (0..40).map(|i| format!("M{i}")).collect();
    let refs: Vec<&str> = coins.iter().map(String::as_str).collect();
    let mut r = rig(&refs);
    // At once: what the burst allows.
    let snapshot = r.settle().await;
    let loaded = snapshot.tiles.iter().filter(|t| t.stats.is_some()).count();
    assert_eq!(loaded, 5, "five requests fit the burst");
    // A minute on: about eleven more.
    tokio::time::sleep(Duration::from_secs(60)).await;
    let loaded = r
        .settle()
        .await
        .tiles
        .iter()
        .filter(|t| t.stats.is_some())
        .count();
    assert!(
        (15..=17).contains(&loaded),
        "{loaded} loaded after a minute"
    );
    // Within three and a half minutes, all of them.
    tokio::time::sleep(Duration::from_secs(150)).await;
    let loaded = r
        .settle()
        .await
        .tiles
        .iter()
        .filter(|t| t.stats.is_some())
        .count();
    assert_eq!(loaded, 40);
    assert_eq!(r.venue.count("candleSnapshot"), 40);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_feed_stops_everything() {
    let mut r = rig(&["BTC"]);
    r.settle().await;
    let asked = r.venue.count("");
    drop(r.feed);
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert!(
        r.events.is_closed(),
        "the feed no longer listens to the stream"
    );
    assert_eq!(r.venue.count(""), asked);
}

#[tokio::test(start_paused = true)]
async fn a_market_the_venue_does_not_list_is_never_subscribed_to() {
    let mut r = rig(&["BTC", "MATIC", "NOSUCH"]);
    r.settle().await;
    assert_eq!(
        r.subs.wanted(),
        [Sub::Ctx(id("BTC")), Sub::Candles(id("BTC"), Interval::M15)]
    );
    assert_eq!(r.venue.count("candleSnapshot"), 1);
    // Nor when the period changes, nor when it is removed.
    r.feed.set_period(MarketPeriod::Hour);
    r.feed.unwatch(id("NOSUCH"));
    r.settle().await;
    assert!(r.subs.0.lock().unwrap().iter().all(|(_, sub)| match sub {
        Sub::Ctx(m) | Sub::Candles(m, _) => m == &id("BTC"),
    }));
}

#[tokio::test(start_paused = true)]
async fn a_list_that_failed_is_tried_again_without_being_asked() {
    let venue = Venue::new();
    venue.listing_fails.store(true, Ordering::SeqCst);
    let mut r = rig_with(venue, &["BTC"]);
    r.settle().await;
    r.venue.listing_fails.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(20)).await;
    let snapshot = r.settle().await;
    assert!(matches!(snapshot.listing, Listing::Ready(_)));
    assert!(snapshot.tiles[0].stats.is_some());
}

#[tokio::test(start_paused = true)]
async fn changing_the_period_drops_history_still_queued_for_the_old_one() {
    let coins: Vec<String> = (0..40).map(|i| format!("M{i}")).collect();
    let refs: Vec<&str> = coins.iter().map(String::as_str).collect();
    let mut r = rig(&refs);
    r.settle().await;
    let before = r.venue.count("candleSnapshot");
    assert!(before <= 6, "only the burst has gone out: {before}");

    // Thirty-odd requests at 15 minutes are still queued. Change the period.
    r.feed.set_period(MarketPeriod::Week);
    tokio::time::sleep(Duration::from_secs(600)).await;
    r.settle().await;
    // None of the queued 15-minute requests was sent after the change (one
    // may have been in flight), and every market loaded at the new interval.
    let fifteen = r
        .venue
        .asked
        .lock()
        .unwrap()
        .iter()
        .filter(|a| a.ends_with(" 15m"))
        .count();
    assert!(
        fifteen <= before + 1,
        "{fifteen} requests at 15m, {before} before the change"
    );
    let two_hour = r
        .venue
        .asked
        .lock()
        .unwrap()
        .iter()
        .filter(|a| a.ends_with(" 2h"))
        .count();
    assert_eq!(two_hour, 40);
}

// ---- The selected market's chart ------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_selected_markets_candles_are_published_and_no_others() {
    let mut r = rig(&["BTC", "ETH"]);
    // Nothing selected: no chart.
    assert_eq!(r.settle().await.chart, None);

    r.feed.select(Some(id("ETH")));
    let snapshot = r.settle().await;
    let chart = snapshot
        .chart
        .expect("a market is selected and its history is loaded");
    assert_eq!(chart.market, id("ETH"));
    assert_eq!(chart.interval, Interval::M15);
    // A day of 15-minute candles, plus the one forming.
    assert!(
        (96..=97).contains(&chart.candles.len()),
        "{} candles",
        chart.candles.len()
    );
    assert!(
        chart
            .candles
            .windows(2)
            .all(|w| w[0].open_ms < w[1].open_ms)
    );
}

#[tokio::test(start_paused = true)]
async fn the_chart_follows_a_change_of_selection_and_goes_when_nothing_is_selected() {
    let mut r = rig(&["BTC", "ETH"]);
    r.feed.select(Some(id("BTC")));
    assert_eq!(r.settle().await.chart.unwrap().market, id("BTC"));
    r.feed.select(Some(id("ETH")));
    assert_eq!(r.settle().await.chart.unwrap().market, id("ETH"));
    r.feed.select(None);
    assert_eq!(r.settle().await.chart, None);
    // Selecting cost no request: the candles were already loaded for the tiles.
    assert_eq!(r.venue.count("candleSnapshot"), 2);
}

#[tokio::test(start_paused = true)]
async fn the_chart_agrees_with_the_tiles_figures() {
    let mut r = rig(&["BTC"]);
    r.feed.select(Some(id("BTC")));
    r.events
        .send(Event::Ctx(id("BTC"), ctx("150", Some("150.5"))))
        .unwrap();
    let snapshot = r.settle().await;
    let chart = snapshot.chart.unwrap();
    let stats = snapshot.tiles[0].stats.as_ref().unwrap();
    assert_eq!(chart.candles.first().unwrap().open, stats.start);
    assert_eq!(
        chart.candles.last().unwrap().close.to_string(),
        "150.5",
        "ends at the last price"
    );
    assert_eq!(chart.candles.iter().map(|c| c.low).min(), Some(stats.low));
    assert_eq!(chart.candles.iter().map(|c| c.high).max(), Some(stats.high));
}

#[tokio::test(start_paused = true)]
async fn a_live_candle_updates_and_extends_the_chart() {
    let mut r = rig(&["BTC"]);
    r.feed.select(Some(id("BTC")));
    let before = r.settle().await.chart.unwrap();
    let newest = before.candles.last().unwrap().open_ms;

    // The candle still forming gets a new high.
    r.events
        .send(Event::Candle(
            id("BTC"),
            Interval::M15,
            Candle {
                open_ms: newest,
                open: "196".parse().unwrap(),
                high: "260".parse().unwrap(),
                low: "196".parse().unwrap(),
                close: "250".parse().unwrap(),
            },
        ))
        .unwrap();
    let updated = r.settle().await.chart.unwrap();
    assert_eq!(updated.candles.len(), before.candles.len());
    assert_eq!(updated.candles.last().unwrap().high.to_string(), "260");

    // Its interval ends and the next candle begins.
    r.events
        .send(Event::Candle(
            id("BTC"),
            Interval::M15,
            Candle {
                open_ms: newest + Interval::M15.millis(),
                open: "250".parse().unwrap(),
                high: "251".parse().unwrap(),
                low: "249".parse().unwrap(),
                close: "250".parse().unwrap(),
            },
        ))
        .unwrap();
    let extended = r.settle().await.chart.unwrap();
    assert_eq!(extended.candles.len(), before.candles.len() + 1);
}

#[tokio::test(start_paused = true)]
async fn the_chart_window_rolls_forward() {
    let mut r = rig(&["BTC"]);
    r.feed.select(Some(id("BTC")));
    let before = r.settle().await.chart.unwrap();
    // An hour on, with nothing new: the four oldest candles have left.
    r.clock.fetch_add(3_600_000, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(6)).await;
    let after = r.settle().await.chart.unwrap();
    assert_eq!(after.candles.len(), before.candles.len() - 4);
    assert!(after.candles[0].open_ms > before.candles[0].open_ms);
}

#[tokio::test(start_paused = true)]
async fn a_period_change_redraws_the_chart_at_the_new_interval() {
    let mut r = rig(&["BTC"]);
    r.feed.select(Some(id("BTC")));
    r.settle().await;
    r.feed.set_period(MarketPeriod::Week);
    let chart = r.settle().await.chart.unwrap();
    assert_eq!(chart.interval, Interval::H2);
    assert!(
        (84..=85).contains(&chart.candles.len()),
        "{} candles",
        chart.candles.len()
    );
}

#[tokio::test(start_paused = true)]
async fn a_market_that_is_unwatched_takes_its_chart_with_it() {
    let mut r = rig(&["BTC", "ETH"]);
    r.feed.select(Some(id("BTC")));
    assert!(r.settle().await.chart.is_some());
    r.feed.unwatch(id("BTC"));
    assert_eq!(r.settle().await.chart, None);
}

#[tokio::test(start_paused = true)]
async fn there_is_no_chart_until_the_selected_markets_history_has_loaded() {
    let venue = Venue::new();
    venue.hold.store(true, Ordering::SeqCst);
    let mut r = rig_with(venue, &["BTC"]);
    r.feed.select(Some(id("BTC")));
    r.events
        .send(Event::Ctx(id("BTC"), ctx("101", None)))
        .unwrap();
    let snapshot = r.settle().await;
    assert!(snapshot.tiles[0].live.is_some());
    assert_eq!(snapshot.chart, None);
    r.venue.hold.store(false, Ordering::SeqCst);
    assert!(r.settle().await.chart.is_some());
}

#[tokio::test(start_paused = true)]
async fn the_selected_markets_history_is_loaded_next() {
    let coins: Vec<String> = (0..40).map(|i| format!("M{i}")).collect();
    let refs: Vec<&str> = coins.iter().map(String::as_str).collect();
    let mut r = rig(&refs);
    // The burst has loaded the first five; M39 is last in a queue of 35.
    let snapshot = r.settle().await;
    assert!(snapshot.tiles[39].stats.is_none());
    r.feed.select(Some(id("M39")));
    // Enough budget for one more request arrives within ten seconds.
    tokio::time::sleep(Duration::from_secs(10)).await;
    let snapshot = r.settle().await;
    assert!(
        snapshot.chart.is_some(),
        "the selected market's chart has loaded"
    );
    assert!(snapshot.tiles[39].stats.is_some());
    // And it jumped the queue: its neighbours are still waiting.
    assert!(snapshot.tiles[38].stats.is_none());
}

// ---- Periods shorter than an hour -----------------------------------------

#[tokio::test(start_paused = true)]
async fn a_short_period_measures_minutes_and_charts_the_hour() {
    let mut r = rig(&["BTC"]);
    r.feed.select(Some(id("BTC")));
    r.feed.set_period(MarketPeriod::Hour);
    let hour = r.settle().await;
    let hour_stats = hour.tiles[0].stats.clone().unwrap();

    r.feed.set_period(MarketPeriod::Min5);
    let snapshot = r.settle().await;
    assert_eq!(snapshot.period, MarketPeriod::Min5);
    let stats = snapshot.tiles[0].stats.clone().unwrap();
    // The change is over five minutes, not the hour: a later, nearer start.
    assert!(
        stats.start > hour_stats.start,
        "{} vs {}",
        stats.start,
        hour_stats.start
    );
    assert!(stats.change.abs() < hour_stats.change.abs());
    // The chart still shows the hour, in 1-minute candles.
    let chart = snapshot.chart.unwrap();
    assert_eq!(chart.interval, Interval::M1);
    assert!(
        (60..=61).contains(&chart.candles.len()),
        "{} candles",
        chart.candles.len()
    );
    assert_eq!(chart.candles, hour.chart.unwrap().candles);
}

#[tokio::test(start_paused = true)]
async fn periods_of_an_hour_and_less_share_one_load_and_one_stream() {
    let mut r = rig(&["BTC"]);
    r.feed.set_period(MarketPeriod::Hour);
    r.settle().await;
    let asked = r.venue.count("candleSnapshot");
    let changes = r.subs.0.lock().unwrap().len();
    for period in [
        MarketPeriod::Min3,
        MarketPeriod::Min15,
        MarketPeriod::Min30,
        MarketPeriod::Min5,
        MarketPeriod::Hour,
    ] {
        r.feed.set_period(period);
        let snapshot = r.settle().await;
        assert_eq!(snapshot.period, period);
        assert!(
            snapshot.tiles[0].stats.is_some(),
            "{period:?} has figures at once"
        );
    }
    assert_eq!(
        r.venue.count("candleSnapshot"),
        asked,
        "no history was asked for again"
    );
    assert_eq!(
        r.subs.0.lock().unwrap().len(),
        changes,
        "no subscription changed"
    );
}

#[tokio::test(start_paused = true)]
async fn a_short_period_chosen_first_still_loads_an_hour() {
    let mut r = rig(&["BTC"]);
    r.feed.select(Some(id("BTC")));
    r.feed.set_period(MarketPeriod::Min3);
    let snapshot = r.settle().await;
    assert!((60..=61).contains(&snapshot.chart.unwrap().candles.len()));
}

// ---- A chart whose history could not be loaded ----------------------------

#[tokio::test(start_paused = true)]
async fn a_failed_history_load_is_reported_and_a_later_success_clears_it() {
    let venue = Venue::new();
    *venue.broken.lock().unwrap() = Some("BTC".into());
    let mut r = rig_with(venue, &["BTC", "ETH"]);
    r.feed.select(Some(id("BTC")));
    let snapshot = r.settle().await;
    assert_eq!(snapshot.chart, None);
    assert!(
        snapshot.chart_failed,
        "the selected market's history failed"
    );
    // The other market loaded: its tile has figures.
    assert!(snapshot.tiles[1].stats.is_some());

    // A failure belongs to its market: selecting the one that loaded shows
    // its chart and no failure.
    r.feed.select(Some(id("ETH")));
    let snapshot = r.settle().await;
    assert!(snapshot.chart.is_some());
    assert!(!snapshot.chart_failed);

    // The venue recovers; the feed asks again by itself.
    r.feed.select(Some(id("BTC")));
    *r.venue.broken.lock().unwrap() = None;
    tokio::time::sleep(Duration::from_secs(40)).await;
    let snapshot = r.settle().await;
    assert!(
        snapshot.chart.is_some(),
        "the chart loaded on a later attempt"
    );
    assert!(!snapshot.chart_failed);
}

#[tokio::test(start_paused = true)]
async fn a_chart_that_is_only_slow_is_not_reported_as_failed() {
    let venue = Venue::new();
    venue.hold.store(true, Ordering::SeqCst);
    let mut r = rig_with(venue, &["BTC"]);
    r.feed.select(Some(id("BTC")));
    let snapshot = r.settle().await;
    assert_eq!(snapshot.chart, None);
    assert!(!snapshot.chart_failed);
}

// ---- Panels' own charts ----------------------------------------------------

fn key(coin: &str, period: MarketPeriod) -> ChartKey {
    ChartKey {
        market: id(coin),
        period,
    }
}

#[tokio::test(start_paused = true)]
async fn a_chart_of_an_unwatched_market_is_streamed_loaded_once_and_stopped() {
    let mut r = rig(&["BTC"]);
    r.settle().await;
    r.subs.take();
    let eth = key("ETH", MarketPeriod::Week);
    // Two panels on one market and period name one chart.
    r.feed.set_charts([eth.clone(), eth.clone()]);
    let snapshot = r.settle().await;
    assert_eq!(
        r.subs.take(),
        [
            (true, Sub::Ctx(id("ETH"))),
            (true, Sub::Candles(id("ETH"), Interval::H2)),
        ]
    );
    assert_eq!(r.venue.count("candleSnapshot ETH 2h"), 1);
    let ChartView::Ready(chart) = &snapshot.charts[&eth] else {
        panic!("{:?}", snapshot.charts[&eth]);
    };
    assert_eq!(chart.market, id("ETH"));
    assert_eq!(chart.interval, Interval::H2);
    // A week of two-hour candles, plus the one forming.
    assert!(
        (84..=85).contains(&chart.candles.len()),
        "{}",
        chart.candles.len()
    );
    // No tile for it: the watchlist is unchanged.
    assert_eq!(snapshot.tiles.len(), 1);

    // Naming it again costs nothing.
    r.feed.set_charts([eth.clone()]);
    r.settle().await;
    assert_eq!(r.venue.count("candleSnapshot ETH 2h"), 1);
    assert!(r.subs.take().is_empty());

    // Closing the last chart stops its stream.
    r.feed.set_charts([]);
    let snapshot = r.settle().await;
    assert!(snapshot.charts.is_empty());
    let changes = r.subs.take();
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(changes.contains(&(false, Sub::Ctx(id("ETH")))));
    assert!(changes.contains(&(false, Sub::Candles(id("ETH"), Interval::H2))));
}

#[tokio::test(start_paused = true)]
async fn a_chart_of_a_watched_market_at_the_watchlists_period_costs_nothing_more() {
    let mut r = rig(&["BTC"]);
    r.settle().await;
    r.subs.take();
    let btc = key("BTC", MarketPeriod::Day);
    r.feed.set_charts([btc.clone()]);
    let snapshot = r.settle().await;
    assert!(r.subs.take().is_empty());
    assert_eq!(r.venue.count("candleSnapshot"), 1);
    assert!(matches!(snapshot.charts[&btc], ChartView::Ready(_)));
    // Closing it leaves the watchlist's subscriptions alone.
    r.feed.set_charts([]);
    r.settle().await;
    assert!(r.subs.take().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_chart_of_a_watched_market_at_another_period_adds_only_its_candles() {
    let mut r = rig(&["BTC"]);
    r.settle().await;
    r.subs.take();
    r.feed.set_charts([key("BTC", MarketPeriod::Hour)]);
    r.settle().await;
    assert_eq!(
        r.subs.take(),
        [(true, Sub::Candles(id("BTC"), Interval::M1))]
    );
    assert_eq!(r.venue.count("candleSnapshot BTC 1m"), 1);
}

#[tokio::test(start_paused = true)]
async fn the_watchlist_and_a_chart_hand_a_subscription_over_without_a_gap() {
    let mut r = rig(&["BTC"]);
    r.settle().await;
    let week = key("BTC", MarketPeriod::Week);
    r.feed.set_charts([week.clone()]);
    r.settle().await;
    r.subs.take();
    // The watchlist moves to the chart's interval: the chart's stream is
    // already there, so nothing is subscribed twice.
    r.feed.set_period(MarketPeriod::Week);
    r.settle().await;
    assert_eq!(
        r.subs.take(),
        [(false, Sub::Candles(id("BTC"), Interval::M15))]
    );
    // And back: the chart keeps its stream when the watchlist lets it go.
    r.feed.set_period(MarketPeriod::Day);
    r.settle().await;
    assert_eq!(
        r.subs.take(),
        [(true, Sub::Candles(id("BTC"), Interval::M15))]
    );
    // Unwatching the market keeps its chart, its figures and their
    // streams: only the watchlist's candles go.
    r.feed.unwatch(id("BTC"));
    let snapshot = r.settle().await;
    assert!(snapshot.tiles.is_empty());
    assert!(matches!(snapshot.charts[&week], ChartView::Ready(_)));
    assert_eq!(
        r.subs.take(),
        [(false, Sub::Candles(id("BTC"), Interval::M15))]
    );
}

#[tokio::test(start_paused = true)]
async fn a_chart_of_a_market_the_venue_does_not_list_is_never_subscribed_to() {
    let mut r = rig(&[]);
    let gone = key("MATIC", MarketPeriod::Day);
    r.feed.set_charts([gone.clone()]);
    let snapshot = r.settle().await;
    assert!(r.subs.wanted().is_empty());
    assert_eq!(r.venue.count("candleSnapshot"), 0);
    assert_eq!(snapshot.charts[&gone], ChartView::Loading);
}

#[tokio::test(start_paused = true)]
async fn four_charts_load_first_and_within_the_venues_limits() {
    let coins: Vec<String> = (0..40).map(|i| format!("M{i}")).collect();
    let refs: Vec<&str> = coins.iter().map(String::as_str).collect();
    let mut r = rig(&refs);
    let charts = ["BTC", "ETH", "SOL", "HYPE"].map(|c| key(c, MarketPeriod::Day));
    r.feed.set_charts(charts.clone());
    // At once: the burst, the four charts ahead of the queue of tiles.
    let snapshot = r.settle().await;
    assert_eq!(
        r.venue.count("candleSnapshot"),
        5,
        "five requests fit the burst"
    );
    for chart in &charts {
        assert!(
            matches!(snapshot.charts[chart], ChartView::Ready(_)),
            "{chart:?}"
        );
    }
    // A minute on: the same steady pace as without them.
    tokio::time::sleep(Duration::from_secs(60)).await;
    r.settle().await;
    let asked = r.venue.count("candleSnapshot");
    assert!((15..=17).contains(&asked), "{asked} asked after a minute");
}

#[tokio::test(start_paused = true)]
async fn closing_a_chart_drops_its_history_still_queued() {
    let coins: Vec<String> = (0..40).map(|i| format!("M{i}")).collect();
    let refs: Vec<&str> = coins.iter().map(String::as_str).collect();
    let mut r = rig(&refs);
    r.settle().await;
    // The burst is spent; a chart's request waits in the queue.
    r.feed.set_charts([key("BTC", MarketPeriod::Week)]);
    r.settle().await;
    r.feed.set_charts([]);
    tokio::time::sleep(Duration::from_secs(60)).await;
    r.settle().await;
    assert_eq!(r.venue.count("candleSnapshot BTC"), 0);
}
