//! The History section's loader against a fake venue: what it asks, when,
//! and what it keeps when an answer fails.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use galata_desk_core::history::feed::{HistoryFeed, REFRESH, Tab};
use galata_desk_core::history::orders::{Status, collapse};
use galata_desk_core::venue::*;
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Venue {
    asked: Arc<Mutex<Vec<Value>>>,
    /// Answers to `historicalOrders`, in turn; the last repeats.
    orders: Arc<Mutex<Vec<Result<Value, ()>>>>,
}

impl Venue {
    fn asked(&self, kind: &str) -> Vec<Value> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b["type"] == kind)
            .cloned()
            .collect()
    }
}

fn order(oid: u64, status: &str, at: u64, sz: &str) -> Value {
    json!({
        "order": {"coin": "xyz:ANT", "side": "A", "limitPx": "105.0", "sz": sz, "oid": oid,
                  "timestamp": 1_795_000_000_000u64 + oid, "triggerCondition": "N/A",
                  "isTrigger": false, "triggerPx": "0.0", "children": [], "isPositionTpsl": false,
                  "reduceOnly": false, "orderType": "Limit", "origSz": "28.57", "tif": "Gtc", "cloid": null},
        "status": status, "statusTimestamp": at,
    })
}

impl Transport for Venue {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        self.asked.lock().unwrap().push(body.clone());
        let answer = match body["type"].as_str().unwrap() {
            "historicalOrders" => {
                let mut answers = self.orders.lock().unwrap();
                let next = if answers.len() > 1 {
                    answers.remove(0)
                } else {
                    answers[0].clone()
                };
                match next {
                    Ok(v) => v,
                    Err(()) => return Err(VenueError::Status(500)),
                }
            }
            "userFunding" => {
                // Three payments, one at each hour from the start asked.
                let start = body["startTime"].as_u64().unwrap();
                let rows: Vec<Value> = (0..3)
                    .map(|i| {
                        json!({"time": start + i * 3_600_000, "hash": "0x0",
                        "delta": {"type": "funding", "coin": "BTC", "usdc": "-0.01", "szi": "0.3",
                                  "fundingRate": "0.00001", "nSamples": 1}})
                    })
                    .collect();
                json!(rows)
            }
            "userNonFundingLedgerUpdates" => json!([]),
            other => panic!("unexpected {other}"),
        };
        Ok(serde_json::to_vec(&answer).unwrap())
    }
}

/// Let the loader's task run, with the clock moved on by `by`.
async fn settle(by: Duration) {
    tokio::time::advance(by).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_tab_never_opened_asks_the_venue_nothing() {
    let venue = Venue::default();
    let _feed = HistoryFeed::start(venue.clone(), "0xabc");
    settle(Duration::from_secs(600)).await;
    assert!(venue.asked.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_shown_tab_refreshes_each_minute_and_a_hidden_one_stops() {
    let venue = Venue::default();
    *venue.orders.lock().unwrap() = vec![Ok(json!([order(1, "open", 10, "28.57")]))];
    let feed = HistoryFeed::start(venue.clone(), "0xabc");
    feed.show(Some(Tab::Orders), 0);
    settle(Duration::ZERO).await;
    assert_eq!(venue.asked("historicalOrders").len(), 1);
    settle(REFRESH).await;
    assert_eq!(venue.asked("historicalOrders").len(), 2);
    // Showing it again does not ask again: it is already held.
    feed.show(Some(Tab::Orders), 0);
    settle(Duration::ZERO).await;
    assert_eq!(venue.asked("historicalOrders").len(), 2);
    // Hidden: no more refreshes.
    feed.show(None, 0);
    settle(REFRESH * 5).await;
    assert_eq!(venue.asked("historicalOrders").len(), 2);
    // Trades asks nothing of its own.
    feed.show(Some(Tab::Trades), 0);
    settle(REFRESH * 2).await;
    assert_eq!(venue.asked.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_refresh_moves_an_order_on_to_its_new_status_once() {
    let venue = Venue::default();
    *venue.orders.lock().unwrap() = vec![
        Ok(json!([order(1, "open", 10, "28.57")])),
        Ok(json!([
            order(1, "filled", 20, "0.0"),
            order(1, "open", 10, "28.57")
        ])),
    ];
    let feed = HistoryFeed::start(venue.clone(), "0xabc");
    let snapshots = feed.snapshots();
    feed.show(Some(Tab::Orders), 0);
    settle(Duration::ZERO).await;
    settle(REFRESH).await;
    let held = snapshots.borrow().orders.data.clone().unwrap();
    assert_eq!(held.rows.len(), 2);
    let orders = collapse(&held.rows);
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].status, Status::Filled);
    assert_eq!(held.shown_from, None);
}

#[tokio::test(start_paused = true)]
async fn a_failed_refresh_keeps_the_rows_and_says_why() {
    let venue = Venue::default();
    *venue.orders.lock().unwrap() = vec![Ok(json!([order(1, "open", 10, "28.57")])), Err(())];
    let feed = HistoryFeed::start(venue.clone(), "0xabc");
    let snapshots = feed.snapshots();
    feed.show(Some(Tab::Orders), 0);
    settle(Duration::ZERO).await;
    settle(REFRESH).await;
    let orders = snapshots.borrow().orders.clone();
    assert_eq!(orders.data.unwrap().rows.len(), 1);
    assert!(orders.error.is_some());
    assert!(orders.fetched_at.is_some());
}

#[tokio::test(start_paused = true)]
async fn widening_the_period_asks_only_for_the_older_stretch() {
    let venue = Venue::default();
    let feed = HistoryFeed::start(venue.clone(), "0xabc");
    let snapshots = feed.snapshots();
    let month_ago = 1_792_000_000_000u64;
    feed.show(Some(Tab::Funding), month_ago);
    settle(Duration::ZERO).await;
    let first = venue.asked("userFunding");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["startTime"], month_ago);
    assert!(first[0].get("endTime").is_none());
    // Narrowing asks nothing.
    feed.show(Some(Tab::Funding), month_ago + 86_400_000);
    settle(Duration::ZERO).await;
    assert_eq!(venue.asked("userFunding").len(), 1);
    // Widening asks only for what lies before the rows held.
    let earlier = month_ago - 60 * 86_400_000;
    feed.show(Some(Tab::Funding), earlier);
    settle(Duration::ZERO).await;
    let asked = venue.asked("userFunding");
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[1]["startTime"], earlier);
    assert_eq!(asked[1]["endTime"], month_ago);
    let held = snapshots.borrow().funding.data.clone().unwrap();
    assert_eq!(held.rows.len(), 6);
    assert_eq!(held.from_ms, Some(earlier));
    // A refresh tops up from the newest row.
    settle(REFRESH).await;
    let asked = venue.asked("userFunding");
    assert_eq!(asked[2]["startTime"], month_ago + 2 * 3_600_000);
}
