//! Loading the histories the venue pages forward by time: funding payments
//! (500 a page) and the ledger (2,000 a page).
//!
//! A walk asks from a start time, takes each page's last time as the next
//! start (dropping the rows that repeat at that millisecond), and stops at a
//! short page. It is paced by a [`Budget`], waits out a rate limit and asks
//! again, and stops after a number of pages, saying where it stopped.

use std::future::Future;
use std::time::Duration;

use crate::funding::feed::request_weight;
use crate::market::history::Budget;
use crate::venue::{LedgerRow, UserFundingRow, VenueError};

/// How long to stop after the venue says to slow down; doubles while it keeps
/// saying so.
const SLOW_DOWN_FIRST: Duration = Duration::from_secs(15);
const SLOW_DOWN_MAX: Duration = Duration::from_secs(120);

/// A row with a time, and a way to tell it from another at the same time.
pub trait Timed: Clone {
    fn time_ms(&self) -> u64;
    fn same(&self, other: &Self) -> bool;
}

impl Timed for UserFundingRow {
    fn time_ms(&self) -> u64 {
        self.time
    }
    fn same(&self, other: &Self) -> bool {
        self.time == other.time && self.delta.coin == other.delta.coin
    }
}

impl Timed for LedgerRow {
    fn time_ms(&self) -> u64 {
        self.time
    }
    fn same(&self, other: &Self) -> bool {
        self.time == other.time && self.hash == other.hash && self.delta == other.delta
    }
}

/// How one history is paged.
#[derive(Debug, Clone, Copy)]
pub struct PageSpec {
    /// The most rows the venue returns in one answer.
    pub rows: usize,
    /// The most pages one load asks for.
    pub max_pages: usize,
}

impl PageSpec {
    pub const FUNDING: PageSpec = PageSpec {
        rows: 500,
        max_pages: 50,
    };
    pub const LEDGER: PageSpec = PageSpec {
        rows: 2000,
        max_pages: 50,
    };
}

impl Budget {
    /// The History section's allowance: two ledger pages or five funding
    /// pages at once, then about five funding pages a minute.
    pub fn for_account_history() -> Self {
        Self::new(240.0, 240.0)
    }
}

/// The rows of one history held so far, oldest first.
#[derive(Debug, Clone, PartialEq)]
pub struct Paged<T> {
    pub rows: Vec<T>,
    /// The earliest time loaded from; `None` before the first load.
    pub from_ms: Option<u64>,
    /// When a load stopped at its page limit: rows after this time were not
    /// loaded.
    pub stopped_at: Option<u64>,
}

impl<T> Default for Paged<T> {
    fn default() -> Self {
        Paged {
            rows: Vec::new(),
            from_ms: None,
            stopped_at: None,
        }
    }
}

impl<T: Timed> Paged<T> {
    /// Add rows not already held, keeping time order.
    pub fn merge(&mut self, rows: Vec<T>) {
        for row in rows {
            if !self.rows.iter().any(|held| held.same(&row)) {
                self.rows.push(row);
            }
        }
        self.rows.sort_by_key(|r| r.time_ms());
    }

    pub fn newest_ms(&self) -> Option<u64> {
        self.rows.last().map(|r| r.time_ms())
    }

    /// What must be asked for the rows to reach back to `since`: a start,
    /// and an end when only an older stretch is missing. `None` when they
    /// already do.
    pub fn gap(&self, since: u64) -> Option<(u64, Option<u64>)> {
        match self.from_ms {
            None => Some((since, None)),
            Some(from) if since < from => Some((since, Some(from))),
            Some(_) => None,
        }
    }
}

/// Walk one history forward from `start` (to `end`, or to now), page by
/// page. Returns the rows and, when the page limit was reached, the time of
/// the last row loaded.
pub async fn walk<T, F, Fut>(
    ask: F,
    start: u64,
    end: Option<u64>,
    spec: PageSpec,
    budget: &mut Budget,
) -> Result<(Vec<T>, Option<u64>), VenueError>
where
    T: Timed,
    F: FnMut(u64, Option<u64>) -> Fut,
    Fut: Future<Output = Result<Vec<T>, VenueError>>,
{
    walk_with(ask, start, end, spec, budget, |_| {}).await
}

/// [`walk`], telling `on_page` the rows so far after each full page, so
/// they can be shown while the rest load.
pub async fn walk_with<T, F, Fut>(
    mut ask: F,
    start: u64,
    end: Option<u64>,
    spec: PageSpec,
    budget: &mut Budget,
    mut on_page: impl FnMut(&[T]),
) -> Result<(Vec<T>, Option<u64>), VenueError>
where
    T: Timed,
    F: FnMut(u64, Option<u64>) -> Fut,
    Fut: Future<Output = Result<Vec<T>, VenueError>>,
{
    let mut out: Paged<T> = Paged::default();
    let mut from = start;
    let mut pages = 0;
    let mut slow_down = SLOW_DOWN_FIRST;
    loop {
        while let Err(wait) = budget.spend(request_weight(spec.rows as u64)) {
            tokio::time::sleep(wait).await;
        }
        let page = match ask(from, end).await {
            Ok(page) => page,
            Err(VenueError::RateLimited) => {
                tokio::time::sleep(slow_down).await;
                slow_down = (slow_down * 2).min(SLOW_DOWN_MAX);
                continue;
            }
            Err(e) => return Err(e),
        };
        slow_down = SLOW_DOWN_FIRST;
        pages += 1;
        let full = page.len() >= spec.rows;
        let latest = page.iter().map(Timed::time_ms).max();
        out.merge(page);
        if !full {
            return Ok((out.rows, None));
        }
        if pages >= spec.max_pages {
            return Ok((out.rows, latest));
        }
        on_page(&out.rows);
        // The next page starts where this one ended; rows sharing that
        // millisecond repeat and are dropped. A page all at one millisecond
        // moves on past it.
        let latest = latest.unwrap_or(from);
        from = if latest > from { latest } else { from + 1 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::FundingDelta;
    use rust_decimal::Decimal;
    use std::cell::RefCell;

    fn payment(time: u64, coin: &str) -> UserFundingRow {
        UserFundingRow {
            time,
            delta: FundingDelta {
                coin: coin.into(),
                usdc: Decimal::ONE,
                szi: Decimal::ONE,
                funding_rate: Decimal::ZERO,
                n_samples: 1,
            },
        }
    }

    /// A venue holding `total` payments, one an hour from time 0, that
    /// answers 500 at a time from a start time and refuses `refuse` times.
    struct Venue {
        total: u64,
        refuse: RefCell<u32>,
        asked: RefCell<Vec<(u64, Option<u64>)>>,
    }

    impl Venue {
        fn new(total: u64) -> Self {
            Venue {
                total,
                refuse: RefCell::new(0),
                asked: RefCell::new(Vec::new()),
            }
        }
        async fn answer(
            &self,
            start: u64,
            end: Option<u64>,
        ) -> Result<Vec<UserFundingRow>, VenueError> {
            self.asked.borrow_mut().push((start, end));
            if *self.refuse.borrow() > 0 {
                *self.refuse.borrow_mut() -= 1;
                return Err(VenueError::RateLimited);
            }
            Ok((0..self.total)
                .map(|i| i * 3_600_000)
                .filter(|t| *t >= start && end.is_none_or(|e| *t <= e))
                .take(500)
                .map(|t| payment(t, "BTC"))
                .collect())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_history_takes_pages_each_starting_where_the_last_ended() {
        let venue = Venue::new(1_200);
        let (rows, stopped) = walk(
            |s, e| venue.answer(s, e),
            0,
            None,
            PageSpec::FUNDING,
            &mut Budget::for_account_history(),
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1_200);
        assert_eq!(stopped, None);
        let asked = venue.asked.borrow();
        assert_eq!(asked.len(), 3);
        assert_eq!(asked[1].0, 499 * 3_600_000);
        assert!(rows.windows(2).all(|w| w[0].time < w[1].time));
    }

    #[tokio::test(start_paused = true)]
    async fn a_rate_limited_page_is_asked_again_not_failed() {
        let venue = Venue::new(10);
        *venue.refuse.borrow_mut() = 2;
        let (rows, _) = walk(
            |s, e| venue.answer(s, e),
            0,
            None,
            PageSpec::FUNDING,
            &mut Budget::for_account_history(),
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 10);
        assert_eq!(venue.asked.borrow().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_load_stops_at_its_page_limit_and_says_where() {
        let venue = Venue::new(5_000);
        let spec = PageSpec {
            rows: 500,
            max_pages: 2,
        };
        let (rows, stopped) = walk(
            |s, e| venue.answer(s, e),
            0,
            None,
            spec,
            &mut Budget::for_account_history(),
        )
        .await
        .unwrap();
        assert_eq!(venue.asked.borrow().len(), 2);
        assert_eq!(rows.len(), 999);
        assert_eq!(stopped, Some(998 * 3_600_000));
    }

    #[tokio::test(start_paused = true)]
    async fn each_full_page_is_reported_as_it_arrives() {
        let venue = Venue::new(1_200);
        let mut seen: Vec<usize> = Vec::new();
        walk_with(
            |s, e| venue.answer(s, e),
            0,
            None,
            PageSpec::FUNDING,
            &mut Budget::for_account_history(),
            |rows| seen.push(rows.len()),
        )
        .await
        .unwrap();
        // After the first and second pages; the short last page ends it.
        assert_eq!(seen, [500, 999]);
    }

    #[test]
    fn rows_held_say_what_is_missing_to_reach_back_further() {
        let mut held: Paged<UserFundingRow> = Paged::default();
        assert_eq!(held.gap(100), Some((100, None)));
        held.from_ms = Some(100);
        assert_eq!(held.gap(150), None);
        // Widening asks only for the stretch before what is held.
        assert_eq!(held.gap(40), Some((40, Some(100))));
    }

    #[test]
    fn a_top_up_adds_only_rows_not_held() {
        let mut held: Paged<UserFundingRow> = Paged::default();
        held.merge(vec![payment(1, "BTC"), payment(2, "BTC")]);
        held.merge(vec![
            payment(2, "BTC"),
            payment(2, "ETH"),
            payment(3, "BTC"),
        ]);
        assert_eq!(held.rows.len(), 4);
        assert_eq!(held.newest_ms(), Some(3));
    }
}
