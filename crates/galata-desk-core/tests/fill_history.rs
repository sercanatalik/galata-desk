//! The walk back through the account's fills, against a venue that pages
//! them as the real one does: at most 2,000 from a start time, oldest first,
//! out of the 10,000 most recent.

use std::sync::{Arc, Mutex};

use galata_desk_core::market::fill_history::{VENUE_KEEPS, walk};
use galata_desk_core::market::history::Budget;
use galata_desk_core::venue::*;
use serde_json::{Value, json};

#[derive(Clone)]
struct Pager {
    /// How many fills the account has made, one a second.
    total: u64,
    /// Answers to refuse with a rate limit before serving.
    refuse: Arc<Mutex<u32>>,
    asked: Arc<Mutex<Vec<u64>>>,
}

impl Pager {
    fn new(total: u64) -> Self {
        Self {
            total,
            refuse: Arc::default(),
            asked: Arc::default(),
        }
    }
    fn asked(&self) -> Vec<u64> {
        self.asked.lock().unwrap().clone()
    }
}

const T0: u64 = 1_790_000_000_000;

impl Transport for Pager {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        assert_eq!(body["type"], "userFillsByTime");
        let start = body["startTime"].as_u64().unwrap();
        self.asked.lock().unwrap().push(start);
        {
            let mut refuse = self.refuse.lock().unwrap();
            if *refuse > 0 {
                *refuse -= 1;
                return Err(VenueError::RateLimited);
            }
        }
        // Only the most recent 10,000 are kept.
        let kept_from = self.total.saturating_sub(VENUE_KEEPS as u64);
        let rows: Vec<Value> = (kept_from..self.total)
            .map(|i| (i, T0 + i * 1_000))
            .filter(|(_, t)| *t >= start)
            .take(2_000)
            .map(|(i, t)| {
                json!({
                    "coin": "BTC", "px": "100", "sz": "1", "side": "B", "time": t,
                    "closedPnl": "0", "fee": "0.01", "tid": i + 1,
                })
            })
            .collect();
        Ok(serde_json::to_vec(&rows).unwrap())
    }
}

#[tokio::test(start_paused = true)]
async fn ten_thousand_fills_take_six_pages() {
    let venue = Pager::new(25_000);
    let info = Info::new(venue.clone());
    let fills = walk(&info, "0xabc", &mut Budget::for_fill_history())
        .await
        .unwrap();
    assert_eq!(fills.len(), VENUE_KEEPS);
    // Five full pages, less the one fill each repeats at its start, and a
    // short sixth page for the last few.
    assert_eq!(venue.asked().len(), 6);
    assert_eq!(venue.asked()[0], 0, "the walk starts from the beginning");
    // Each page starts at the last fill of the one before.
    assert_eq!(venue.asked()[1], T0 + (15_000 + 1_999) * 1_000);
}

#[tokio::test(start_paused = true)]
async fn three_hundred_fills_take_one_page() {
    let venue = Pager::new(300);
    let info = Info::new(venue.clone());
    let fills = walk(&info, "0xabc", &mut Budget::for_fill_history())
        .await
        .unwrap();
    assert_eq!(fills.len(), 300);
    assert_eq!(venue.asked().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_rate_limited_page_is_asked_again_not_reported_as_failed() {
    let venue = Pager::new(300);
    *venue.refuse.lock().unwrap() = 2;
    let info = Info::new(venue.clone());
    let fills = walk(&info, "0xabc", &mut Budget::for_fill_history())
        .await
        .unwrap();
    assert_eq!(fills.len(), 300);
    assert_eq!(venue.asked(), vec![0, 0, 0]);
}

#[tokio::test(start_paused = true)]
async fn pages_are_paced_by_the_budget() {
    let venue = Pager::new(6_000);
    let info = Info::new(venue.clone());
    let started = tokio::time::Instant::now();
    walk(&info, "0xabc", &mut Budget::for_fill_history())
        .await
        .unwrap();
    assert_eq!(venue.asked().len(), 4);
    // One page at once, then about one a minute.
    assert!(started.elapsed() >= std::time::Duration::from_secs(170));
}
