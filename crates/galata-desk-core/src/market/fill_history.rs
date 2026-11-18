//! Every fill of the account's the app can get: one walk back through the
//! venue's history when the account connects, topped up with the latest
//! fills from then on.
//!
//! The venue keeps only an account's 10,000 most recent fills and gives at
//! most 2,000 in one answer, paged forward by time. A walk from the start of
//! time therefore costs at most six requests (each page repeats the fills
//! at the millisecond the last one ended on), and an account with 10,000 or
//! more fills may have older ones nobody can see. The store says so, so that
//! sums over a period can be marked as partial.

use std::collections::HashSet;
use std::time::Duration;

use super::fills::{Fill, Fills, VENUE_CAP};
use super::history::Budget;
use crate::funding::feed::request_weight;
use crate::venue::{Info, Transport, VenueError};

/// The most fills the venue keeps for one account.
pub const VENUE_KEEPS: usize = 10_000;

/// How long to stop after the venue says to slow down; doubles while it keeps
/// saying so.
const SLOW_DOWN_FIRST: Duration = Duration::from_secs(15);
const SLOW_DOWN_MAX: Duration = Duration::from_secs(120);

impl Budget {
    /// The fill history's allowance: one full page at once, then about one
    /// a minute.
    ///
    /// A page of 2,000 fills costs 120 by the venue's documented weight, so
    /// the longest walk, six pages, spreads over five minutes and does not
    /// land in the first minute's burst of candles, funding and Portfolio.
    pub fn for_fill_history() -> Self {
        Self::new(120.0, 120.0)
    }
}

/// The account's fills, oldest first, each once.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FillHistory {
    rows: Vec<Fill>,
    ids: HashSet<u64>,
    /// The walk back through the venue's history has finished.
    walked: bool,
    /// Older fills may exist that the venue did not give.
    held_back: bool,
}

impl FillHistory {
    /// Add fills, dropping any already held. Returns whether anything was new.
    pub fn merge(&mut self, fills: impl IntoIterator<Item = Fill>) -> bool {
        let before = self.rows.len();
        for fill in fills {
            if self.ids.insert(fill.id) {
                self.rows.push(fill);
            }
        }
        let added = self.rows.len() > before;
        if added {
            self.rows.sort_by_key(|f| (f.time_ms, f.id));
        }
        added
    }

    /// Fold in the venue's latest fills. Until the walk has finished, a full
    /// answer means older fills exist that are not held yet.
    pub fn top_up(&mut self, latest: Vec<Fill>) {
        if !self.walked && latest.len() >= VENUE_CAP {
            self.held_back = true;
        }
        self.merge(latest);
    }

    /// Fold in the walk's fills. When it reached as many as the venue keeps,
    /// older ones may exist.
    pub fn finish_walk(&mut self, walked: Vec<Fill>) {
        self.held_back = walked.len() >= VENUE_KEEPS;
        self.merge(walked);
        self.walked = true;
    }

    pub fn rows(&self) -> &[Fill] {
        &self.rows
    }

    /// Whether the walk back through the venue's history has finished: until
    /// it has, sums over a period are not shown.
    pub fn is_walked(&self) -> bool {
        self.walked
    }

    /// When older fills may exist, the time of the earliest one held: sums
    /// over a period that starts before it are partial. `None` when every
    /// fill is held.
    pub fn reaches_from(&self) -> Option<u64> {
        if self.held_back {
            self.rows.first().map(|f| f.time_ms)
        } else {
            None
        }
    }

    /// The fills, as the chart reads them.
    pub fn to_fills(&self) -> Fills {
        Fills {
            rows: self.rows.clone(),
            capped: self.held_back,
            walked: self.walked,
        }
    }
}

/// Walk the account's fills from the earliest the venue keeps to now, at
/// most 2,000 at a time and paced by `budget`. Stops at a short page or once
/// as many fills as the venue keeps are in hand. A rate-limit answer waits
/// and asks again; any other failure ends the walk.
pub async fn walk<T: Transport>(
    info: &Info<T>,
    user: &str,
    budget: &mut Budget,
) -> Result<Vec<Fill>, VenueError> {
    let mut seen: HashSet<u64> = HashSet::new();
    let mut fills: Vec<Fill> = Vec::new();
    let mut start_ms = 0u64;
    let mut slow_down = SLOW_DOWN_FIRST;
    loop {
        while let Err(wait) = budget.spend(request_weight(VENUE_CAP as u64)) {
            tokio::time::sleep(wait).await;
        }
        let page = match info.user_fills_by_time(user, start_ms).await {
            Ok(page) => page,
            Err(VenueError::RateLimited) => {
                tokio::time::sleep(slow_down).await;
                slow_down = (slow_down * 2).min(SLOW_DOWN_MAX);
                continue;
            }
            Err(e) => return Err(e),
        };
        slow_down = SLOW_DOWN_FIRST;
        let full = page.len() >= VENUE_CAP;
        let latest = page.iter().map(|f| f.time).max();
        for f in page {
            if seen.insert(f.tid) {
                fills.push(Fill::from(f));
            }
        }
        if !full || fills.len() >= VENUE_KEEPS {
            return Ok(fills);
        }
        // The next page starts where this one ended, so fills sharing that
        // millisecond are not skipped; repeats are dropped above. A page
        // whose fills all share one millisecond moves on past it.
        let latest = latest.unwrap_or(start_ms);
        start_ms = if latest > start_ms {
            latest
        } else {
            start_ms + 1
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::fills::FillSide;
    use rust_decimal::Decimal;

    fn fill(id: u64, time_ms: u64) -> Fill {
        Fill {
            coin: "BTC".into(),
            side: FillSide::Bought,
            price: Decimal::ONE,
            size: Decimal::ONE,
            time_ms,
            realised: Decimal::ZERO,
            fee: Decimal::ZERO,
            id,
        }
    }

    #[test]
    fn pages_that_overlap_at_one_millisecond_keep_each_fill_once() {
        let mut h = FillHistory::default();
        // Two fills at 200: the first page ends on one, the next starts on both.
        assert!(h.merge(vec![fill(1, 100), fill(2, 200)]));
        assert!(h.merge(vec![fill(2, 200), fill(3, 200), fill(4, 300)]));
        let ids: Vec<u64> = h.rows().iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
        assert!(!h.merge(vec![fill(4, 300)]), "nothing new");
    }

    #[test]
    fn a_top_up_that_overlaps_the_history_adds_only_new_fills_in_time_order() {
        let mut h = FillHistory::default();
        h.finish_walk(vec![fill(1, 100), fill(2, 200), fill(3, 300)]);
        h.top_up(vec![fill(5, 500), fill(3, 300), fill(4, 400)]);
        let times: Vec<u64> = h.rows().iter().map(|f| f.time_ms).collect();
        assert_eq!(times, vec![100, 200, 300, 400, 500]);
        assert!(h.is_walked());
        assert_eq!(h.reaches_from(), None);
    }

    #[test]
    fn coverage_is_complete_below_what_the_venue_keeps_and_partial_at_it() {
        let mut short = FillHistory::default();
        short.finish_walk((0..9_999).map(|i| fill(i, 1_000 + i)).collect());
        assert_eq!(short.reaches_from(), None);
        assert!(!short.to_fills().capped);

        let mut full = FillHistory::default();
        full.finish_walk((0..10_000).map(|i| fill(i, 1_000 + i)).collect());
        assert_eq!(full.reaches_from(), Some(1_000));
        assert!(full.to_fills().capped);
    }

    #[test]
    fn before_the_walk_a_full_top_up_means_older_fills_exist() {
        let mut h = FillHistory::default();
        h.top_up((0..VENUE_CAP as u64).map(|i| fill(i, 5_000 + i)).collect());
        assert!(!h.is_walked());
        assert_eq!(h.reaches_from(), Some(5_000));
        // The walk then finds everything: the history is complete.
        h.finish_walk((0..3_000).map(|i| fill(10_000 + i, i)).collect());
        assert_eq!(h.reaches_from(), None);
    }
}
