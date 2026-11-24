//! Turning each market's candles into one table of returns.
//!
//! Every market's series must line up: row `t` of the table is the same
//! stretch of time for every column. So only the open times every market has
//! are kept, and the returns are the log changes between consecutive kept
//! closes. A market listed partway through the window shortens every column;
//! the count of returns says so.

use std::collections::{BTreeSet, HashMap};

use rust_decimal::prelude::ToPrimitive;

use super::Matrix;
use crate::market::candles::Candle;

/// The aligned log returns: one row per step, one column per market, in the
/// order the series were given.
#[derive(Debug, Clone, PartialEq)]
pub struct Returns {
    pub table: Matrix,
}

impl Returns {
    pub fn count(&self) -> usize {
        self.table.rows()
    }
}

/// Align `series` on the open times they all share at or after `from_ms`, and
/// take log returns between consecutive closes. A candle that has not closed by
/// `now_ms` is still forming and is left out. A close that is
/// not a positive number drops that open time for every series.
pub fn align(series: &[&[Candle]], from_ms: u64, now_ms: u64, interval_ms: u64) -> Returns {
    let cutoff = now_ms.saturating_sub(interval_ms);
    let closes: Vec<HashMap<u64, f64>> = series
        .iter()
        .map(|candles| {
            candles
                .iter()
                .filter(|c| c.open_ms >= from_ms && c.open_ms <= cutoff)
                .filter_map(|c| {
                    c.close
                        .to_f64()
                        .filter(|v| *v > 0.0)
                        .map(|v| (c.open_ms, v))
                })
                .collect()
        })
        .collect();
    let shared: Vec<u64> = match closes.split_first() {
        None => Vec::new(),
        Some((first, rest)) => {
            let times: BTreeSet<u64> = first
                .keys()
                .copied()
                .filter(|t| rest.iter().all(|m| m.contains_key(t)))
                .collect();
            times.into_iter().collect()
        }
    };
    let rows: Vec<Vec<f64>> = shared
        .windows(2)
        .map(|w| closes.iter().map(|m| (m[&w[1]] / m[&w[0]]).ln()).collect())
        .collect();
    let table = if rows.is_empty() {
        Matrix::zeros(0, series.len())
    } else {
        Matrix::from_rows(&rows)
    };
    Returns { table }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    const H: u64 = 3_600_000;

    fn candles(closes: &[(u64, &str)]) -> Vec<Candle> {
        closes
            .iter()
            .map(|(t, c)| {
                let c: Decimal = c.parse().unwrap();
                Candle {
                    open_ms: t * H,
                    open: c,
                    high: c,
                    low: c,
                    close: c,
                }
            })
            .collect()
    }

    #[test]
    fn log_returns_between_consecutive_closes() {
        let a = candles(&[(0, "100"), (1, "110"), (2, "99")]);
        let r = align(&[&a], 0, 10 * H, H);
        assert_eq!(r.count(), 2);
        assert!((r.table.get(0, 0) - (1.1f64).ln()).abs() < 1e-12);
        assert!((r.table.get(1, 0) - (0.9f64).ln()).abs() < 1e-12);
    }

    #[test]
    fn a_missing_candle_drops_that_time_for_every_series() {
        let a = candles(&[(0, "100"), (1, "101"), (2, "102"), (3, "103")]);
        let b = candles(&[(0, "10"), (2, "11"), (3, "12")]);
        let r = align(&[&a, &b], 0, 10 * H, H);
        // Shared times 0, 2, 3: two returns each.
        assert_eq!(r.count(), 2);
        assert!((r.table.get(0, 0) - (102.0f64 / 100.0).ln()).abs() < 1e-12);
        assert!((r.table.get(0, 1) - (11.0f64 / 10.0).ln()).abs() < 1e-12);
    }

    #[test]
    fn a_short_history_shortens_every_column() {
        let a = candles(&[(0, "1"), (1, "1"), (2, "1"), (3, "1"), (4, "1")]);
        let young = candles(&[(3, "5"), (4, "6")]);
        let r = align(&[&a, &young], 0, 10 * H, H);
        assert_eq!(r.count(), 1);
    }

    #[test]
    fn the_forming_candle_and_anything_before_the_window_are_left_out() {
        let a = candles(&[(0, "1"), (1, "2"), (2, "4"), (3, "8"), (4, "16")]);
        // Window from hour 1; now is 4.5h so the hour-4 candle is forming.
        let r = align(&[&a], H, 4 * H + H / 2, H);
        assert_eq!(r.count(), 2);
    }
}
