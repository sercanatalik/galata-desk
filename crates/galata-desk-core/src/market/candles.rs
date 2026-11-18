//! Candles, and what a period of them says: change, range, volatility.
//!
//! Prices stay as the venue's decimals, so a low or a high is shown exactly
//! as the venue quoted it. The statistics are computed in `f64`: they are
//! statistics, not money.

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

/// One candle.
#[derive(Debug, Clone, PartialEq)]
pub struct Candle {
    /// When the candle opened, in milliseconds since the epoch.
    pub open_ms: u64,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
}

/// One market's candles at one interval: history from a query, kept current
/// by the live stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Series {
    /// Oldest first, one per open time.
    candles: Vec<Candle>,
    /// Whether history has arrived. Until it has, live candles are only held.
    loaded: bool,
}

impl Series {
    /// Whether history has arrived.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Install history. Live candles that arrived first are merged on top, so
    /// nothing is lost between the query and the stream.
    pub fn set_history(&mut self, mut history: Vec<Candle>) {
        history.sort_by_key(|c| c.open_ms);
        history.dedup_by_key(|c| c.open_ms);
        let held = std::mem::replace(&mut self.candles, history);
        self.loaded = true;
        for candle in held {
            self.merge(candle);
        }
    }

    /// Take a candle from the live stream: the current candle as it forms.
    pub fn push_live(&mut self, candle: Candle) {
        self.merge(candle);
    }

    /// A candle replaces the one with the same open time, is appended when
    /// newer than all, and otherwise is put in its place.
    fn merge(&mut self, candle: Candle) {
        match self
            .candles
            .binary_search_by_key(&candle.open_ms, |c| c.open_ms)
        {
            Ok(at) => self.candles[at] = candle,
            Err(at) => self.candles.insert(at, candle),
        }
    }

    /// The candles whose open time falls within `period_ms` before `now_ms`.
    pub fn window(&self, now_ms: u64, period_ms: u64) -> &[Candle] {
        let from = now_ms.saturating_sub(period_ms);
        let start = self.candles.partition_point(|c| c.open_ms < from);
        &self.candles[start..]
    }

    /// Drop candles older than `keep_ms` before `now_ms`, so a long session
    /// does not grow the series without bound.
    pub fn trim(&mut self, now_ms: u64, keep_ms: u64) {
        let from = now_ms.saturating_sub(keep_ms);
        let start = self.candles.partition_point(|c| c.open_ms < from);
        self.candles.drain(..start);
    }

    pub fn len(&self) -> usize {
        self.candles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candles.is_empty()
    }
}

/// What a period says about a market.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    /// The price at the start of the period: the oldest candle's open.
    pub start: Decimal,
    /// The last price relative to `start`, as a fraction: `0.0271` is +2.71%.
    pub change: f64,
    pub low: Decimal,
    pub high: Decimal,
    /// Where the last price sits from `low` (0) to `high` (1).
    pub marker: f64,
    /// Realised volatility over the period, as a fraction. `None` with fewer
    /// than two returns.
    pub volatility: Option<f64>,
    /// The average close over the period and the closes' sample standard
    /// deviation, in price. `None` with fewer than [`MIN_CLOSES`] closes, or
    /// when every close is the same.
    pub mean: Option<f64>,
    pub sd: Option<f64>,
}

/// The fewest closes a period needs before its average and spread mean
/// anything.
pub const MIN_CLOSES: usize = 10;

/// How far from its usual level a price must be, in standard deviations,
/// before it is called stretched.
pub const STRETCHED: f64 = 2.0;

/// Where the last price sits on a tile's bar, each place from the low (0) to
/// the high (1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// The last price.
    pub tick: f64,
    /// The average ±1 standard deviation, clipped to the bar. `None` when the
    /// period has too few closes or no spread.
    pub band: Option<(f64, f64)>,
    /// The last price's distance from the average, in standard deviations.
    pub z: Option<f64>,
}

impl Placement {
    /// The price is far enough from its usual level to stand out.
    pub fn stretched(&self) -> bool {
        self.z.is_some_and(|z| z.abs() >= STRETCHED)
    }
}

impl Stats {
    /// Where `last` sits on the period's bar.
    pub fn placement(&self, last: Decimal) -> Placement {
        let low = self.low.to_f64().unwrap_or(0.0);
        let high = self.high.to_f64().unwrap_or(0.0);
        let last = last.to_f64().unwrap_or(low);
        let span = high - low;
        let place = |price: f64| {
            if span > 0.0 {
                ((price - low) / span).clamp(0.0, 1.0)
            } else {
                0.5
            }
        };
        let (band, z) = match (self.mean, self.sd) {
            (Some(mean), Some(sd)) if sd > 0.0 => (
                Some((place(mean - sd), place(mean + sd))),
                Some((last - mean) / sd),
            ),
            _ => (None, None),
        };
        Placement {
            tick: place(last),
            band,
            z,
        }
    }
}

/// The statistics of one window of candles. `last` is the live last price;
/// without one the newest close stands in. `None` for an empty window.
pub fn stats(window: &[Candle], last: Option<Decimal>) -> Option<Stats> {
    let first = window.first()?;
    let newest = window.last()?;
    let last = last.unwrap_or(newest.close);

    // The range includes the present: a live price beyond the candles'
    // extremes widens it.
    let low = window.iter().map(|c| c.low).fold(last, Decimal::min);
    let high = window.iter().map(|c| c.high).fold(last, Decimal::max);

    let start = first.open;
    let change = match (last.to_f64(), start.to_f64()) {
        (Some(last), Some(start)) if start != 0.0 => last / start - 1.0,
        _ => 0.0,
    };
    let marker = if high > low {
        ((last - low) / (high - low))
            .to_f64()
            .unwrap_or(0.5)
            .clamp(0.0, 1.0)
    } else {
        // A flat market has no ends to be near.
        0.5
    };

    let closes: Vec<f64> = window.iter().filter_map(|c| c.close.to_f64()).collect();
    // For the average and spread the newest candle closes at the live price,
    // as the chart draws it.
    let mut settled = closes.clone();
    if let (Some(newest), Some(last)) = (settled.last_mut(), last.to_f64()) {
        *newest = last;
    }
    let (mean, sd) = match spread(&settled) {
        Some((mean, sd)) => (Some(mean), Some(sd)),
        None => (None, None),
    };

    Some(Stats {
        start,
        change,
        low,
        high,
        marker,
        volatility: volatility(&closes),
        mean,
        sd,
    })
}

/// The average of the closes and their sample standard deviation. `None`
/// with fewer than [`MIN_CLOSES`], or when they are all the same.
pub fn spread(closes: &[f64]) -> Option<(f64, f64)> {
    let n = closes.len();
    if n < MIN_CLOSES {
        return None;
    }
    let mean = closes.iter().sum::<f64>() / n as f64;
    let variance = closes.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    let sd = variance.sqrt();
    (sd > 0.0).then_some((mean, sd))
}

/// A window of candles as a chart shows it: the newest candle ends at the
/// live last price and reaches as far as it, so the chart's last close, low
/// and high are the same figures [`stats`] reports for the window.
pub fn chart(window: &[Candle], last: Option<Decimal>) -> Vec<Candle> {
    let mut candles = window.to_vec();
    if let (Some(last), Some(newest)) = (last, candles.last_mut()) {
        newest.close = last;
        newest.high = newest.high.max(last);
        newest.low = newest.low.min(last);
    }
    candles
}

/// Realised volatility: the sample standard deviation of the log returns
/// between consecutive closes, scaled by the square root of the number of
/// returns, so it reads as how far the market typically moved over the whole
/// period. Not annualised. `None` with fewer than two returns.
pub fn volatility(closes: &[f64]) -> Option<f64> {
    let returns: Vec<f64> = closes
        .windows(2)
        .filter(|w| w[0] > 0.0 && w[1] > 0.0)
        .map(|w| (w[1] / w[0]).ln())
        .collect();
    let n = returns.len();
    if n < 2 {
        return None;
    }
    let mean = returns.iter().sum::<f64>() / n as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    Some(variance.sqrt() * (n as f64).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    /// A candle that opens at `t` minutes with the given open, high, low, close.
    fn candle(t: u64, o: &str, h: &str, l: &str, c: &str) -> Candle {
        Candle {
            open_ms: t * 60_000,
            open: d(o),
            high: d(h),
            low: d(l),
            close: d(c),
        }
    }

    fn flat(t: u64, px: &str) -> Candle {
        candle(t, px, px, px, px)
    }

    #[test]
    fn a_live_candle_replaces_the_one_with_its_open_time() {
        let mut s = Series::default();
        s.set_history(vec![flat(0, "100"), flat(1, "101")]);
        s.push_live(candle(1, "101", "103", "101", "102"));
        assert_eq!(s.len(), 2);
        assert_eq!(s.window(120_000, u64::MAX)[1].close, d("102"));
    }

    #[test]
    fn a_newer_live_candle_is_appended() {
        let mut s = Series::default();
        s.set_history(vec![flat(0, "100"), flat(1, "101")]);
        s.push_live(flat(2, "102"));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn live_candles_that_arrive_before_history_are_kept() {
        let mut s = Series::default();
        assert!(!s.is_loaded());
        // The stream sends minute 2 as it forms, twice, then minute 3.
        s.push_live(candle(2, "102", "102", "102", "102"));
        s.push_live(candle(2, "102", "105", "102", "104"));
        s.push_live(flat(3, "104"));
        // History lands late, and still holds an older view of minute 2.
        s.set_history(vec![flat(0, "100"), flat(1, "101"), flat(2, "102")]);
        assert!(s.is_loaded());
        let all = s.window(u64::MAX, u64::MAX);
        assert_eq!(all.len(), 4);
        assert_eq!(all[2].high, d("105"), "the live view of minute 2 wins");
        assert_eq!(all[3].open_ms, 180_000);
    }

    #[test]
    fn history_is_sorted_and_deduplicated() {
        let mut s = Series::default();
        s.set_history(vec![
            flat(2, "102"),
            flat(0, "100"),
            flat(2, "102"),
            flat(1, "101"),
        ]);
        let times: Vec<u64> = s
            .window(u64::MAX, u64::MAX)
            .iter()
            .map(|c| c.open_ms)
            .collect();
        assert_eq!(times, [0, 60_000, 120_000]);
    }

    #[test]
    fn the_window_rolls_forward() {
        let mut s = Series::default();
        s.set_history((0..10).map(|t| flat(t, "100")).collect());
        // Five minutes before minute 9.5: candles opening at 5 through 9.
        assert_eq!(s.window(9 * 60_000 + 30_000, 5 * 60_000).len(), 5);
        // A minute later the oldest of those has dropped out.
        assert_eq!(s.window(10 * 60_000 + 30_000, 5 * 60_000).len(), 4);
    }

    #[test]
    fn a_high_that_ages_out_no_longer_counts() {
        let mut s = Series::default();
        s.set_history(vec![
            candle(0, "100", "120", "100", "100"),
            flat(1, "100"),
            flat(2, "100"),
        ]);
        let with = stats(s.window(2 * 60_000, 3 * 60_000), None).unwrap();
        assert_eq!(with.high, d("120"));
        let without = stats(s.window(3 * 60_000 + 1, 3 * 60_000), None).unwrap();
        assert_eq!(without.high, d("100"));
    }

    #[test]
    fn trimming_drops_what_no_window_will_ask_for() {
        let mut s = Series::default();
        s.set_history((0..10).map(|t| flat(t, "100")).collect());
        s.trim(10 * 60_000, 4 * 60_000);
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn change_is_measured_from_the_start_of_the_period() {
        let window = [flat(0, "100"), flat(1, "101")];
        let gain = stats(&window, Some(d("102.71"))).unwrap();
        assert_eq!(format!("{:+.2}", gain.change * 100.0), "+2.71");
        let loss = stats(&window, Some(d("97.36"))).unwrap();
        assert_eq!(format!("{:+.2}", loss.change * 100.0), "-2.64");
        assert_eq!(gain.start, d("100"));
    }

    #[test]
    fn the_marker_shows_where_the_price_sits() {
        let window = [candle(0, "105", "110", "100", "105")];
        let near_high = stats(&window, Some(d("108"))).unwrap();
        assert!((near_high.marker - 0.8).abs() < 1e-9);
        assert_eq!(near_high.low, d("100"));
        assert_eq!(near_high.high, d("110"));
    }

    #[test]
    fn a_price_above_the_high_becomes_the_high() {
        let window = [candle(0, "105", "110", "100", "105")];
        let s = stats(&window, Some(d("112.5"))).unwrap();
        assert_eq!(s.high, d("112.5"));
        assert_eq!(s.marker, 1.0);
        let s = stats(&window, Some(d("99"))).unwrap();
        assert_eq!(s.low, d("99"));
        assert_eq!(s.marker, 0.0);
    }

    #[test]
    fn a_flat_market_puts_the_marker_in_the_middle() {
        let s = stats(&[flat(0, "100"), flat(1, "100")], Some(d("100"))).unwrap();
        assert_eq!(s.marker, 0.5);
    }

    #[test]
    fn without_a_live_price_the_newest_close_stands_in() {
        let window = [flat(0, "100"), candle(1, "100", "104", "100", "103")];
        let s = stats(&window, None).unwrap();
        assert_eq!(format!("{:+.2}", s.change * 100.0), "+3.00");
    }

    #[test]
    fn an_empty_window_has_no_statistics() {
        assert_eq!(stats(&[], Some(d("100"))), None);
    }

    #[test]
    fn the_chart_ends_at_the_last_price_and_agrees_with_the_statistics() {
        let window = [
            candle(0, "100", "104", "99", "101"),
            candle(1, "101", "103", "100", "102"),
        ];
        let last = Some(d("105"));
        let shown = chart(&window, last);
        let s = stats(&window, last).unwrap();
        // The first candle opens where the change is measured from.
        assert_eq!(shown[0].open, s.start);
        // The newest candle ends at the last price and reaches up to it.
        assert_eq!(shown[1].close, d("105"));
        assert_eq!(shown[1].high, d("105"));
        assert_eq!(shown[1].open, d("101"), "its open is untouched");
        // The chart's extremes are the statistics' low and high.
        assert_eq!(shown.iter().map(|c| c.low).min(), Some(s.low));
        assert_eq!(shown.iter().map(|c| c.high).max(), Some(s.high));
    }

    #[test]
    fn a_chart_with_no_live_price_is_the_candles_as_they_are() {
        let window = [flat(0, "100"), flat(1, "101")];
        assert_eq!(chart(&window, None), window);
        assert!(chart(&[], Some(d("1"))).is_empty());
    }

    #[test]
    fn a_steady_market_has_no_volatility() {
        assert_eq!(volatility(&[100.0, 100.0, 100.0, 100.0]), Some(0.0));
    }

    #[test]
    fn the_specs_series_is_two_point_three_percent() {
        let v = volatility(&[100.0, 101.0, 100.0, 101.0, 100.0]).unwrap();
        assert_eq!(format!("{:.1}", v * 100.0), "2.3");
    }

    #[test]
    fn fewer_than_two_returns_is_no_figure() {
        assert_eq!(volatility(&[]), None);
        assert_eq!(volatility(&[100.0]), None);
        assert_eq!(volatility(&[100.0, 101.0]), None);
        assert!(volatility(&[100.0, 101.0, 102.0]).is_some());
    }

    /// Ten one-minute candles closing at the given prices.
    fn closing(prices: &[u32]) -> Vec<Candle> {
        prices
            .iter()
            .enumerate()
            .map(|(t, p)| flat(t as u64, &p.to_string()))
            .collect()
    }

    #[test]
    fn the_average_and_spread_of_a_known_series() {
        let s = stats(
            &closing(&[100, 101, 102, 103, 104, 105, 106, 107, 108, 109]),
            None,
        )
        .unwrap();
        assert!((s.mean.unwrap() - 104.5).abs() < 1e-9);
        // Sample standard deviation: the sum of squares 82.5 over 9.
        assert!((s.sd.unwrap() - (82.5f64 / 9.0).sqrt()).abs() < 1e-9);
    }

    #[test]
    fn the_newest_close_is_the_live_price() {
        let candles = closing(&[100, 100, 100, 100, 100, 100, 100, 100, 100, 100]);
        // Without a live price the market is flat: no spread.
        assert_eq!(stats(&candles, None).unwrap().sd, None);
        // A live price of 110 is the tenth close.
        let s = stats(&candles, Some(d("110"))).unwrap();
        assert!((s.mean.unwrap() - 101.0).abs() < 1e-9);
        assert!(s.sd.unwrap() > 0.0);
    }

    #[test]
    fn a_short_period_has_no_average_or_spread() {
        let s = stats(&closing(&[100, 103, 101]), None).unwrap();
        assert_eq!((s.mean, s.sd), (None, None));
        assert_eq!(spread(&[1.0; 9]), None);
        assert!(spread(&[1.0, 2.0, 1.0, 2.0, 1.0, 2.0, 1.0, 2.0, 1.0, 2.0]).is_some());
    }

    #[test]
    fn a_steady_market_has_no_spread() {
        let s = stats(&closing(&[100; 12]), None).unwrap();
        assert_eq!((s.mean, s.sd), (None, None));
    }

    fn ranged(low: &str, high: &str, mean: Option<f64>, sd: Option<f64>) -> Stats {
        Stats {
            start: d(low),
            change: 0.0,
            low: d(low),
            high: d(high),
            marker: 0.5,
            volatility: None,
            mean,
            sd,
        }
    }

    #[test]
    fn two_and_a_half_deviations_below_the_average() {
        let s = ranged("100", "110", Some(105.0), Some(2.0));
        let at = s.placement(d("100"));
        assert_eq!(at.tick, 0.0);
        let (from, to) = at.band.unwrap();
        assert!((from - 0.3).abs() < 1e-9 && (to - 0.7).abs() < 1e-9);
        assert_eq!(at.z, Some(-2.5));
        assert!(at.stretched());
    }

    #[test]
    fn at_the_average_is_zero_and_not_stretched() {
        let at = ranged("100", "110", Some(105.0), Some(2.0)).placement(d("105"));
        assert_eq!(at.z, Some(0.0));
        assert_eq!(at.tick, 0.5);
        assert!(!at.stretched());
        // 1.9 deviations is not yet stretched; 2.0 is.
        let s = ranged("100", "110", Some(105.0), Some(1.0));
        assert!(!s.placement(d("106.9")).stretched());
        assert!(s.placement(d("107")).stretched());
    }

    #[test]
    fn a_band_past_the_high_is_clipped_to_the_bar() {
        let at = ranged("100", "110", Some(108.0), Some(4.0)).placement(d("109"));
        assert_eq!(at.band, Some((0.4, 1.0)));
        assert_eq!(at.z, Some(0.25));
    }

    #[test]
    fn a_flat_range_puts_the_tick_in_the_middle() {
        let at = ranged("100", "100", None, None).placement(d("100"));
        assert_eq!(at.tick, 0.5);
        assert_eq!((at.band, at.z), (None, None));
        assert!(!at.stretched());
    }
}
