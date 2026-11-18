//! A market's funding rate over time: the hourly rows the venue keeps, cut
//! into buckets for a chart, with the figures that sum a window up.
//!
//! Rates here keep the venue's sign: positive means longs paid shorts. Which
//! way that runs for the account is applied by whoever draws it.

use std::collections::BTreeMap;

use rust_decimal::Decimal;

pub const HOUR_MS: u64 = 3_600_000;

/// The most rows the venue returns for one request.
pub const PAGE_HOURS: u64 = 500;

/// The hour a time falls in.
pub fn hour_of(ms: u64) -> u64 {
    ms - ms % HOUR_MS
}

/// How the chart cuts time: the bucket, and with it the window shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Resolution {
    Hour,
    EightHours,
    #[default]
    Day,
}

impl Resolution {
    pub const ALL: [Self; 3] = [Self::Hour, Self::EightHours, Self::Day];

    /// The control's name for it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Hour => "1h",
            Self::EightHours => "8h",
            Self::Day => "D",
        }
    }

    pub fn bucket_hours(self) -> u64 {
        match self {
            Self::Hour => 1,
            Self::EightHours => 8,
            Self::Day => 24,
        }
    }

    /// How many buckets the window holds: 7 days of hours, 30 days of
    /// 8-hour buckets, 90 days.
    pub fn buckets(self) -> usize {
        match self {
            Self::Hour => 168,
            Self::EightHours => 90,
            Self::Day => 90,
        }
    }

    pub fn window_hours(self) -> u64 {
        self.buckets() as u64 * self.bucket_hours()
    }

    /// How many requests a window never loaded before takes.
    pub fn pages(self) -> u64 {
        self.window_hours().div_ceil(PAGE_HOURS)
    }

    pub fn window_words(self) -> &'static str {
        match self {
            Self::Hour => "Last 7 days",
            Self::EightHours => "Last 30 days",
            Self::Day => "Last 90 days",
        }
    }

    /// What one bucket's rate is a rate per.
    pub fn bucket_words(self) -> &'static str {
        match self {
            Self::Hour => "per hour",
            Self::EightHours => "per 8 hours",
            Self::Day => "per day",
        }
    }

    /// What buckets are counted in.
    pub fn bucket_noun(self) -> &'static str {
        match self {
            Self::Hour => "hours",
            Self::EightHours => "periods",
            Self::Day => "days",
        }
    }
}

/// One market's hourly funding rates, as much as has been loaded.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct History {
    /// The rate charged at each hour, by the hour's start.
    rows: BTreeMap<u64, Decimal>,
    /// Pages have been asked for every hour from here to the newest row.
    /// `None` until the first page has been answered.
    reached_ms: Option<u64>,
    /// The venue has nothing older than the oldest row here.
    exhausted: bool,
}

impl History {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// The hour of the oldest row.
    pub fn first_ms(&self) -> Option<u64> {
        self.rows.keys().next().copied()
    }

    /// The hour of the newest row.
    pub fn last_ms(&self) -> Option<u64> {
        self.rows.keys().next_back().copied()
    }

    /// The first page has been answered, with or without rows.
    pub fn is_started(&self) -> bool {
        self.reached_ms.is_some()
    }

    /// When the market's history begins, once it is known that the venue has
    /// nothing older.
    pub fn starts_ms(&self) -> Option<u64> {
        self.first_ms().filter(|_| self.exhausted)
    }

    /// Take in the answer to a page asked for `from_ms..=to_ms`. Rows already
    /// held are replaced, never doubled.
    ///
    /// A page that comes back empty, or whose oldest row is later than the
    /// hour it was asked from, means the venue has nothing older.
    pub fn merge_page(&mut self, from_ms: u64, rows: impl IntoIterator<Item = (u64, Decimal)>) {
        let mut oldest: Option<u64> = None;
        for (time, rate) in rows {
            let hour = hour_of(time);
            oldest = Some(oldest.map_or(hour, |o: u64| o.min(hour)));
            self.rows.insert(hour, rate);
        }
        // The first hour whose row falls inside the range asked for.
        let first_asked = hour_of(from_ms + HOUR_MS - 1);
        if oldest.is_none_or(|o| o > first_asked) {
            self.exhausted = true;
        }
        self.reached_ms = Some(self.reached_ms.map_or(from_ms, |r| r.min(from_ms)));
    }

    /// Take in rows newer than those held: the hours that have passed since.
    /// It says nothing about how far back the history goes.
    pub fn extend(&mut self, rows: impl IntoIterator<Item = (u64, Decimal)>) {
        for (time, rate) in rows {
            self.rows.insert(hour_of(time), rate);
        }
    }

    /// The next page to ask for so that `resolution`'s window ending at
    /// `latest_ms` (the latest funding hour) is covered: the newest 500
    /// hours first, then backwards. `None` when nothing more is needed, or
    /// nothing more exists.
    pub fn next_page(&self, latest_ms: u64, resolution: Resolution) -> Option<(u64, u64)> {
        let latest = hour_of(latest_ms);
        let needed_from = latest.saturating_sub((resolution.window_hours() - 1) * HOUR_MS);
        match self.reached_ms {
            None => {
                let from = latest.saturating_sub((PAGE_HOURS - 1) * HOUR_MS);
                Some((from.max(needed_from), latest + HOUR_MS - 1))
            }
            Some(_) if self.exhausted => None,
            Some(reached) if reached > needed_from => {
                let from = reached.saturating_sub(PAGE_HOURS * HOUR_MS);
                Some((from.max(needed_from), reached - 1))
            }
            Some(_) => None,
        }
    }

    /// The range to ask for to catch up with `latest_ms`, when hours have
    /// passed since the newest row. `None` when there is nothing to catch
    /// up, or nothing loaded to catch up from.
    pub fn top_up(&self, latest_ms: u64) -> Option<(u64, u64)> {
        let latest = hour_of(latest_ms);
        let last = self.last_ms()?;
        (last < latest).then(|| (last + HOUR_MS, latest + HOUR_MS - 1))
    }

    /// The window `resolution` shows, as far as it has loaded.
    ///
    /// Buckets end at the newest row and run backwards, so every bucket is
    /// whole: a bucket that would start before the oldest row is left out.
    pub fn window(&self, resolution: Resolution) -> Window {
        let mut buckets = Vec::new();
        if let (Some(first), Some(last)) = (self.first_ms(), self.last_ms()) {
            let span = resolution.bucket_hours() * HOUR_MS;
            for i in 0..resolution.buckets() as u64 {
                // The bucket's newest and oldest hours.
                let Some(end) = last.checked_sub(i * span) else {
                    break;
                };
                let Some(start) = (end + HOUR_MS).checked_sub(span) else {
                    break;
                };
                if start < first {
                    break;
                }
                let rate = self.rows.range(start..=end).map(|(_, r)| *r).sum();
                buckets.push(Bucket { end_ms: end, rate });
            }
            buckets.reverse();
        }
        Window {
            resolution,
            buckets,
        }
    }
}

/// One point of the chart: the funding rate summed over a bucket.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bucket {
    /// The bucket's last hour.
    pub end_ms: u64,
    /// The sum of its hourly rates, as a fraction.
    pub rate: Decimal,
}

/// What the chart draws: the buckets that have loaded, oldest first. There
/// may be fewer than the window holds; they are its newest ones.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub resolution: Resolution,
    pub buckets: Vec<Bucket>,
}

impl Window {
    /// The running total of the buckets' rates: the last is the cumulative
    /// rate over what is shown.
    pub fn cumulative(&self) -> Vec<Decimal> {
        let mut total = Decimal::ZERO;
        self.buckets
            .iter()
            .map(|b| {
                total += b.rate;
                total
            })
            .collect()
    }

    /// `None` with no bucket to sum up.
    pub fn summary(&self) -> Option<Summary> {
        if self.buckets.is_empty() {
            return None;
        }
        let count = self.buckets.len();
        let cumulative: Decimal = self.buckets.iter().map(|b| b.rate).sum();
        let average = cumulative / Decimal::from(count as u64);
        Some(Summary {
            buckets: count,
            positive: self
                .buckets
                .iter()
                .filter(|b| b.rate > Decimal::ZERO)
                .count(),
            average,
            // Multiplied before it is divided, so a round rate stays round.
            yearly: cumulative * Decimal::from(24 * 365)
                / Decimal::from(count as u64 * self.resolution.bucket_hours()),
            cumulative,
        })
    }
}

/// A window in four figures. Rates are fractions.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub buckets: usize,
    /// How many buckets had a rate above zero.
    pub positive: usize,
    /// The average rate per bucket.
    pub average: Decimal,
    /// That average, held for a year.
    pub yearly: Decimal,
    pub cumulative: Decimal,
}

/// Which side of a market a position is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    Long,
    Short,
}

impl Held {
    /// What this side does with a rate: a short receives a positive one.
    /// `1` received, `-1` paid, `0` for a zero rate.
    pub fn sign_of(self, rate: Decimal) -> i32 {
        let venue = if rate > Decimal::ZERO {
            1
        } else if rate < Decimal::ZERO {
            -1
        } else {
            0
        };
        match self {
            Self::Short => venue,
            Self::Long => -venue,
        }
    }
}

/// What a position of `value` on `side` would have received over a window
/// whose rates sum to `cumulative`: positive received, negative paid. It
/// supposes the position was that size throughout.
pub fn what_if(side: Held, value: Decimal, cumulative: Decimal) -> Decimal {
    let amount = value.abs() * cumulative;
    match side {
        Held::Short => amount,
        Held::Long => -amount,
    }
}

/// Two scales that share their zero line and every grid line: the same
/// number of steps above and below zero on both, each with its own step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axes {
    /// Steps above and below zero.
    pub up: u32,
    pub down: u32,
    /// The left scale's step, and the right's.
    pub left_step: f64,
    pub right_step: f64,
}

impl Axes {
    pub fn steps(&self) -> u32 {
        self.up + self.down
    }

    /// Where a left-scale value sits down the plot: 0 at the top, 1 at the
    /// bottom.
    pub fn left_y(&self, value: f64) -> f64 {
        self.y(value, self.left_step)
    }

    pub fn right_y(&self, value: f64) -> f64 {
        self.y(value, self.right_step)
    }

    fn y(&self, value: f64, step: f64) -> f64 {
        (self.up as f64 * step - value) / (self.steps() as f64 * step)
    }

    /// Where zero sits down the plot.
    pub fn zero_y(&self) -> f64 {
        self.up as f64 / self.steps() as f64
    }

    /// The grid lines' values on each scale, top to bottom.
    pub fn ticks(&self) -> Vec<(f64, f64)> {
        (-(self.down as i64)..=self.up as i64)
            .rev()
            .map(|k| (k as f64 * self.left_step, k as f64 * self.right_step))
            .collect()
    }
}

/// The most steps a scale is cut into.
const MAX_STEPS: u32 = 4;

/// Steps a scale may use: 1, 2, 2.5 and 5 times a power of ten.
fn nice_steps() -> impl Iterator<Item = f64> {
    (-9..=6).flat_map(|power| [1.0, 2.0, 2.5, 5.0].map(move |m| m * 10f64.powi(power)))
}

/// How many steps of `step` reach `value`.
fn steps_to(value: f64, step: f64) -> u32 {
    if value <= 0.0 {
        0
    } else {
        // A value a hair over a whole number of steps is that number.
        (value / step - 1e-9).ceil().max(1.0) as u32
    }
}

/// Scales for a series on the left and another on the right. The left one
/// decides how many steps lie above and below zero, with the smallest step
/// that fits it into four; the right one takes the smallest step that fits
/// into the same shape.
pub fn axes(left: &[f64], right: &[f64]) -> Axes {
    let high = |values: &[f64]| values.iter().copied().fold(0.0, f64::max);
    let low = |values: &[f64]| -values.iter().copied().fold(0.0, f64::min);
    let (left_high, left_low) = (high(left), low(left));
    let (right_high, right_low) = (high(right), low(right));

    let mut shape = (1, 0, 1.0);
    for step in nice_steps() {
        let (up, down) = (steps_to(left_high, step), steps_to(left_low, step));
        // A large value over a tiny step is more steps than fit a number.
        if up.saturating_add(down) <= MAX_STEPS {
            shape = (up, down, step);
            break;
        }
    }
    let (mut up, mut down, left_step) = shape;
    // The right series needs room on a side the left one never visits.
    if right_high > 0.0 && up == 0 {
        up = 1;
    }
    if right_low > 0.0 && down == 0 {
        down = 1;
    }
    if up + down == 0 {
        up = 1;
    }
    let right_step = nice_steps()
        .find(|step| {
            up as f64 * step >= right_high - 1e-12 && down as f64 * step >= right_low - 1e-12
        })
        .unwrap_or(1.0);
    Axes {
        up,
        down,
        left_step,
        right_step,
    }
}

/// A scale's value as a signed percentage, with as many decimals as the
/// scale's step needs: a step of 0.05 gives `+0.10%`, `0.00%`, `-0.05%`.
/// `value` and `step` are already percentages.
pub fn axis_label(value: f64, step: f64) -> String {
    let mut places = 0;
    while places < 9
        && ((step * 10f64.powi(places)).round() - step * 10f64.powi(places)).abs() > 1e-6
    {
        places += 1;
    }
    let places = places as usize;
    if value.abs() < step * 1e-6 {
        format!("{:.places$}%", 0.0)
    } else {
        format!("{value:+.places$}%")
    }
}

/// A stretch of a line that stays on one side of zero.
#[derive(Debug, Clone, PartialEq)]
pub struct Stretch {
    /// At or above zero.
    pub positive: bool,
    pub points: Vec<(f64, f64)>,
}

/// Cut a line of `(x, value)` points where it crosses zero, so each stretch
/// can take its own colour. Neighbouring stretches meet on the zero line. A
/// value of exactly zero belongs with the positive side.
pub fn split_at_zero(points: &[(f64, f64)]) -> Vec<Stretch> {
    let mut stretches: Vec<Stretch> = Vec::new();
    for &(x, value) in points {
        let positive = value >= 0.0;
        match stretches.last_mut() {
            Some(current) if current.positive == positive => current.points.push((x, value)),
            Some(current) => {
                let &(last_x, last_value) = current.points.last().expect("a stretch has a point");
                // Where the straight line between the two points meets zero.
                let t = last_value / (last_value - value);
                let crossing = (last_x + t * (x - last_x), 0.0);
                current.points.push(crossing);
                stretches.push(Stretch {
                    positive,
                    points: vec![crossing, (x, value)],
                });
            }
            None => stretches.push(Stretch {
                positive,
                points: vec![(x, value)],
            }),
        }
    }
    stretches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    /// Hours counted from a fixed top of the hour.
    const T0: u64 = 1_794_988_800_000;

    fn at(hour: i64) -> u64 {
        (T0 as i64 + hour * HOUR_MS as i64) as u64
    }

    /// Rows for `hours`, each stamped a little past its hour as the venue does.
    fn rows(hours: std::ops::RangeInclusive<i64>, rate: &str) -> Vec<(u64, Decimal)> {
        hours.map(|h| (at(h) + 47, d(rate))).collect()
    }

    /// A history holding `hours`, loaded as one page.
    fn history(hours: std::ops::RangeInclusive<i64>, rate: &str) -> History {
        let mut history = History::default();
        history.merge_page(at(*hours.start()), rows(hours, rate));
        history
    }

    #[test]
    fn the_resolutions_need_one_two_and_five_pages() {
        assert_eq!(Resolution::Hour.window_hours(), 7 * 24);
        assert_eq!(Resolution::EightHours.window_hours(), 30 * 24);
        assert_eq!(Resolution::Day.window_hours(), 90 * 24);
        let pages: Vec<u64> = Resolution::ALL.iter().map(|r| r.pages()).collect();
        assert_eq!(pages, [1, 2, 5]);
        assert_eq!(Resolution::default(), Resolution::Day);
    }

    #[test]
    fn pages_are_asked_newest_first_and_stop_at_the_window() {
        let mut history = History::default();
        let mut asked = Vec::new();
        while let Some((from, to)) = history.next_page(at(0) + 1_234, Resolution::Day) {
            asked.push((from, to));
            let hours = ((from - at(-3000)) / HOUR_MS) as i64 - 3000
                ..=((hour_of(to) - at(-3000)) / HOUR_MS) as i64 - 3000;
            history.merge_page(from, rows(hours, "0.00001"));
        }
        assert_eq!(asked.len(), 5);
        // The first page is the newest 500 hours, up to the end of this hour.
        assert_eq!(asked[0], (at(-499), at(1) - 1));
        // Each next page ends where the one before began.
        assert_eq!(asked[1], (at(-999), at(-499) - 1));
        // The last is cut at the window: 2,160 hours in all.
        assert_eq!(asked[4], (at(-2159), at(-1999) - 1));
        assert_eq!(history.len(), 2160);
        assert_eq!(history.first_ms(), Some(at(-2159)));
        // Nothing more for a shorter window either.
        assert_eq!(history.next_page(at(0), Resolution::Hour), None);
        assert_eq!(history.starts_ms(), None);
    }

    #[test]
    fn the_hourly_window_is_one_page_cut_to_seven_days() {
        let history = History::default();
        assert_eq!(
            history.next_page(at(0), Resolution::Hour),
            Some((at(-167), at(1) - 1))
        );
    }

    #[test]
    fn a_longer_window_asks_only_for_what_is_missing() {
        let mut history = History::default();
        let (from, _) = history.next_page(at(0), Resolution::Hour).unwrap();
        history.merge_page(from, rows(-167..=0, "0.00001"));
        assert_eq!(history.next_page(at(0), Resolution::Hour), None);
        // 8h needs 720 hours: the 552 before the 168 held.
        assert_eq!(
            history.next_page(at(0), Resolution::EightHours),
            Some((at(-667), at(-167) - 1))
        );
    }

    #[test]
    fn a_short_page_marks_where_the_history_starts() {
        let mut history = History::default();
        history.merge_page(at(-499), rows(-499..=0, "0.00001"));
        assert_eq!(history.starts_ms(), None);
        // The next page asks from hour -999 and the venue's rows begin at -671.
        history.merge_page(at(-999), rows(-671..=-500, "0.00001"));
        assert_eq!(history.starts_ms(), Some(at(-671)));
        assert_eq!(history.next_page(at(0), Resolution::Day), None);
    }

    #[test]
    fn an_empty_page_means_there_is_nothing_older() {
        let mut history = history(-499..=0, "0.00001");
        history.merge_page(at(-999), []);
        assert_eq!(history.starts_ms(), Some(at(-499)));
        assert_eq!(history.next_page(at(0), Resolution::Day), None);
    }

    #[test]
    fn a_market_with_no_history_at_all_is_started_and_empty() {
        let mut history = History::default();
        assert!(!history.is_started());
        history.merge_page(at(-499), []);
        assert!(history.is_started() && history.is_empty());
        assert_eq!(history.next_page(at(0), Resolution::Day), None);
        assert!(history.window(Resolution::Day).buckets.is_empty());
        assert_eq!(history.window(Resolution::Day).summary(), None);
    }

    #[test]
    fn a_missing_newest_row_is_not_the_start_of_the_history() {
        // The hour has turned and the venue has not published its row yet.
        let mut history = History::default();
        history.merge_page(at(-499), rows(-499..=-1, "0.00001"));
        assert_eq!(history.starts_ms(), None);
        assert!(history.next_page(at(0), Resolution::Day).is_some());
    }

    #[test]
    fn a_top_up_overlapping_what_is_held_adds_no_duplicates() {
        let mut history = history(-499..=0, "0.00001");
        assert_eq!(history.top_up(at(0) + 5_000), None);
        // Two hours later.
        assert_eq!(history.top_up(at(2) + 5_000), Some((at(1), at(3) - 1)));
        history.extend(rows(0..=2, "0.00002"));
        assert_eq!(history.len(), 502);
        assert_eq!(history.last_ms(), Some(at(2)));
        assert_eq!(history.top_up(at(2) + 5_000), None);
        // The row that was already held took the newer answer.
        assert_eq!(
            history.window(Resolution::Hour).buckets[165].rate,
            d("0.00002")
        );
        // Topping up says nothing about the start.
        assert_eq!(history.starts_ms(), None);
    }

    #[test]
    fn nothing_loaded_has_nothing_to_top_up() {
        assert_eq!(History::default().top_up(at(5)), None);
    }

    #[test]
    fn twenty_four_hours_at_a_rate_make_one_day_at_24_times_it() {
        // +0.0010% an hour for 24 hours is +0.0240% for the day.
        let history = history(-23..=0, "0.00001");
        let window = history.window(Resolution::Day);
        assert_eq!(window.buckets.len(), 1);
        assert_eq!(window.buckets[0].rate, d("0.00024"));
        assert_eq!(window.buckets[0].end_ms, at(0));
    }

    #[test]
    fn buckets_end_at_the_newest_row_and_an_incomplete_oldest_one_is_dropped() {
        // 20 hours: two whole 8-hour buckets and four hours over.
        let mut history = History::default();
        let mut hourly = rows(-19..=0, "0.00001");
        // The newest eight hours carry a different rate.
        for row in hourly.iter_mut().skip(12) {
            row.1 = d("0.00003");
        }
        history.merge_page(at(-19), hourly);
        let window = history.window(Resolution::EightHours);
        let ends: Vec<u64> = window.buckets.iter().map(|b| b.end_ms).collect();
        assert_eq!(ends, [at(-8), at(0)]);
        assert_eq!(window.buckets[0].rate, d("0.00008"));
        assert_eq!(window.buckets[1].rate, d("0.00024"));
    }

    #[test]
    fn a_window_holds_no_more_buckets_than_it_shows() {
        let history = history(-499..=0, "0.00001");
        assert_eq!(history.window(Resolution::Hour).buckets.len(), 168);
        // 500 hours make 20 whole days.
        assert_eq!(history.window(Resolution::Day).buckets.len(), 20);
    }

    #[test]
    fn the_running_total_ends_at_the_cumulative_rate() {
        let mut history = History::default();
        history.merge_page(
            at(-2),
            [
                (at(-2), d("0.0002")),
                (at(-1), d("0.0003")),
                (at(0), d("-0.0001")),
            ],
        );
        let window = history.window(Resolution::Hour);
        assert_eq!(window.cumulative(), [d("0.0002"), d("0.0005"), d("0.0004")]);
        assert_eq!(window.summary().unwrap().cumulative, d("0.0004"));
    }

    #[test]
    fn a_window_is_summed_up_in_four_figures() {
        // 90 days: 84 at a positive rate and 6 at a negative one, +2.07% in all.
        let mut history = History::default();
        let mut hourly = Vec::new();
        for day in 0..90i64 {
            let rate = if day < 6 {
                d("-0.000005")
            } else {
                d("0.0000125")
            };
            for hour in 0..24 {
                hourly.push((at(-(89 - day) * 24 - (23 - hour)), rate));
            }
        }
        history.merge_page(at(-2159), hourly);
        let summary = history.window(Resolution::Day).summary().unwrap();
        // 6 x 24 x -0.0005% + 84 x 24 x 0.00125% = -0.072% + 2.52% = 2.448%.
        assert_eq!(summary.cumulative, d("0.02448"));
        assert_eq!((summary.positive, summary.buckets), (84, 90));
        assert_eq!(summary.average, d("0.000272"));
        // 0.0272% a day for 365 days.
        assert_eq!(summary.yearly, d("0.09928"));
    }

    #[test]
    fn the_spec_window_of_ninety_days_at_2_07_percent() {
        // +0.0230% a day for 90 days: +2.07%, and +8.4% over a year.
        let window = Window {
            resolution: Resolution::Day,
            buckets: (0..90)
                .map(|i| Bucket {
                    end_ms: at(i * 24),
                    rate: d("0.00023"),
                })
                .collect(),
        };
        let summary = window.summary().unwrap();
        assert_eq!(summary.cumulative, d("0.0207"));
        assert_eq!(summary.average, d("0.00023"));
        assert_eq!(crate::format::pct_signed(summary.yearly, 1), "+8.4%");
    }

    #[test]
    fn a_young_market_is_summed_over_the_days_it_has() {
        let history = history(-28 * 24 + 1..=0, "0.00001");
        let summary = history.window(Resolution::Day).summary().unwrap();
        assert_eq!(summary.buckets, 28);
        assert_eq!(summary.average, d("0.00024"));
    }

    #[test]
    fn what_a_position_of_todays_size_would_have_received_or_paid() {
        // A short of $48,210.00 over +2.00% receives $964.20.
        assert_eq!(what_if(Held::Short, d("48210"), d("0.02")), d("964.2"));
        // A long of $50,000.00 over +2.00% pays $1,000.00.
        assert_eq!(what_if(Held::Long, d("50000"), d("0.02")), d("-1000"));
        // A negative window reverses both, and a value's sign is ignored.
        assert_eq!(what_if(Held::Long, d("-50000"), d("-0.02")), d("1000"));
        assert_eq!(what_if(Held::Short, d("50000"), d("-0.02")), d("-1000"));
    }

    #[test]
    fn a_side_receives_or_pays_a_rate() {
        assert_eq!(Held::Short.sign_of(d("0.1")), 1);
        assert_eq!(Held::Long.sign_of(d("0.1")), -1);
        assert_eq!(Held::Short.sign_of(d("-0.1")), -1);
        assert_eq!(Held::Long.sign_of(d("-0.1")), 1);
        assert_eq!(Held::Long.sign_of(Decimal::ZERO), 0);
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn an_all_positive_series_has_no_steps_below_zero() {
        // Rates up to 0.12, a running total up to 1.6.
        let axes = axes(&[0.02, 0.12, 0.05], &[0.02, 0.9, 1.6]);
        assert_eq!((axes.up, axes.down), (3, 0));
        assert!(close(axes.left_step, 0.05) && close(axes.right_step, 1.0));
        assert!(close(axes.zero_y(), 1.0));
        assert!(close(axes.left_y(0.15), 0.0) && close(axes.left_y(0.0), 1.0));
    }

    #[test]
    fn a_mixed_series_shares_its_zero_line_between_both_scales() {
        // The canvas's example: rates from -0.07 to 0.12, a total up to 1.55.
        let axes = axes(&[0.02, 0.12, -0.07], &[0.02, 1.55, 1.2]);
        assert_eq!(axes.steps(), axes.up + axes.down);
        assert!(axes.steps() <= 4);
        // Both scales put zero at the same height.
        assert!(close(axes.left_y(0.0), axes.right_y(0.0)));
        assert!(close(axes.left_y(0.0), axes.zero_y()));
        // And every grid line is a whole step on both.
        let ticks = axes.ticks();
        assert_eq!(ticks.len() as u32, axes.steps() + 1);
        for (left, right) in &ticks {
            assert!(close(axes.left_y(*left), axes.right_y(*right)));
        }
        // Everything fits.
        assert!(axes.left_y(0.12) >= 0.0 && axes.left_y(-0.07) <= 1.0);
        assert!(axes.right_y(1.55) >= 0.0);
    }

    #[test]
    fn the_mixed_example_uses_steps_of_0_05_and_1() {
        let axes = axes(&[0.02, 0.12, -0.07], &[0.02, 1.55, 1.2]);
        assert_eq!((axes.up, axes.down), (2, 1));
        assert!(close(axes.left_step, 0.1), "{axes:?}");
        assert!(close(axes.right_step, 1.0), "{axes:?}");
    }

    #[test]
    fn an_all_negative_series_has_no_steps_above_zero() {
        let axes = axes(&[-0.02, -0.04], &[-0.02, -0.06]);
        assert_eq!((axes.up, axes.down), (0, 4));
        assert!(close(axes.zero_y(), 0.0));
        assert!(axes.right_y(-0.06) <= 1.0);
    }

    #[test]
    fn a_market_at_the_venues_funding_cap_still_has_a_scale() {
        // 4% an hour for a day, with a day the other way: percentages far
        // larger than a usual rate.
        let axes = axes(&[96.0, -3.5, 40.0], &[96.0, 92.5, 132.5]);
        assert!(axes.steps() <= 4, "{axes:?}");
        assert!(axes.left_y(96.0) >= 0.0 && axes.left_y(-3.5) <= 1.0);
        assert!(axes.right_y(132.5) >= 0.0);
    }

    #[test]
    fn a_flat_series_still_has_a_scale() {
        let axes = axes(&[0.0, 0.0], &[0.0, 0.0]);
        assert_eq!(axes.steps(), 1);
        assert!(axes.left_step > 0.0 && axes.right_step > 0.0);
    }

    #[test]
    fn a_scale_is_labelled_with_the_decimals_its_step_needs() {
        assert_eq!(axis_label(0.10, 0.05), "+0.10%");
        assert_eq!(axis_label(0.0, 0.05), "0.00%");
        assert_eq!(axis_label(-0.05, 0.05), "-0.05%");
        assert_eq!(axis_label(2.0, 1.0), "+2%");
        assert_eq!(axis_label(0.025, 0.025), "+0.025%");
        assert_eq!(axis_label(-0.5, 0.5), "-0.5%");
    }

    #[test]
    fn a_line_that_never_crosses_zero_is_one_stretch() {
        let stretches = split_at_zero(&[(0.0, 1.0), (1.0, 2.0), (2.0, 0.0)]);
        assert_eq!(stretches.len(), 1);
        assert!(stretches[0].positive);
        assert_eq!(stretches[0].points.len(), 3);
    }

    #[test]
    fn a_line_is_cut_where_it_crosses_zero_between_two_points() {
        // From +1 at x=0 to -3 at x=4: it meets zero a quarter of the way.
        let stretches = split_at_zero(&[(0.0, 1.0), (4.0, -3.0), (5.0, -1.0), (6.0, 1.0)]);
        assert_eq!(stretches.len(), 3);
        assert_eq!(stretches[0].points, [(0.0, 1.0), (1.0, 0.0)]);
        assert!(!stretches[1].positive);
        assert_eq!(
            stretches[1].points,
            [(1.0, 0.0), (4.0, -3.0), (5.0, -1.0), (5.5, 0.0)]
        );
        assert!(stretches[2].positive);
        assert_eq!(stretches[2].points, [(5.5, 0.0), (6.0, 1.0)]);
    }

    #[test]
    fn no_points_make_no_stretches() {
        assert!(split_at_zero(&[]).is_empty());
    }
}
