//! The account's own fills, grouped for the chart: what was bought and sold
//! in each candle.

use rust_decimal::Decimal;

use super::MarketId;
use super::candles::Candle;

/// The most fills the venue returns in one answer. An answer this long may
/// have been cut short: there can be older fills it did not include.
pub const VENUE_CAP: usize = 2000;

/// Which way a fill went, from the account's side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FillSide {
    Bought,
    Sold,
}

/// One fill.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    /// The venue's coin name: `BTC`, `xyz:NVDA`.
    pub coin: String,
    pub side: FillSide,
    pub price: Decimal,
    pub size: Decimal,
    /// Milliseconds since the epoch.
    pub time_ms: u64,
    /// PNL realised by the fill, as the venue reports it; zero for a fill
    /// that opened or added to a position.
    pub realised: Decimal,
}

/// The account's latest fills, across all markets.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Fills {
    pub rows: Vec<Fill>,
    /// The venue returned as many as it gives at once, so older fills may
    /// exist that are not here.
    pub capped: bool,
}

impl Fills {
    pub fn new(rows: Vec<Fill>) -> Self {
        let capped = rows.len() >= VENUE_CAP;
        Self { rows, capped }
    }
}

/// Everything bought, or everything sold, within one candle.
#[derive(Debug, Clone, PartialEq)]
pub struct SideFills {
    /// Total size.
    pub size: Decimal,
    /// The size-weighted average price.
    pub average: Decimal,
    /// How many fills.
    pub count: usize,
    /// Their realised PNL, summed.
    pub realised: Decimal,
}

/// One candle's fills. Either side may be absent.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CandleFills {
    pub bought: Option<SideFills>,
    pub sold: Option<SideFills>,
}

impl CandleFills {
    pub fn is_empty(&self) -> bool {
        self.bought.is_none() && self.sold.is_none()
    }
}

/// One market's fills, grouped by candle and side: an entry per candle, in
/// the candles' order.
///
/// A fill belongs to the candle whose span contains its time. The last
/// candle is the one still forming, so it takes every fill from its open
/// onwards. Fills from before the first candle are left out.
pub fn group(
    fills: &[Fill],
    market: &MarketId,
    candles: &[Candle],
    interval_ms: u64,
) -> Vec<CandleFills> {
    #[derive(Default, Clone)]
    struct Sum {
        size: Decimal,
        notional: Decimal,
        count: usize,
        realised: Decimal,
    }
    let mut sums = vec![(Sum::default(), Sum::default()); candles.len()];
    let coin = market.coin();
    for fill in fills.iter().filter(|f| f.coin == coin) {
        // The last candle that opened at or before the fill.
        let Some(at) = candles
            .partition_point(|c| c.open_ms <= fill.time_ms)
            .checked_sub(1)
        else {
            continue;
        };
        // A gap in the candles: the fill is in none of them.
        let last = at + 1 == candles.len();
        if !last && fill.time_ms >= candles[at].open_ms + interval_ms {
            continue;
        }
        let sum = match fill.side {
            FillSide::Bought => &mut sums[at].0,
            FillSide::Sold => &mut sums[at].1,
        };
        sum.size += fill.size;
        sum.notional += fill.size * fill.price;
        sum.count += 1;
        sum.realised += fill.realised;
    }
    let side = |sum: Sum| {
        (sum.count > 0 && !sum.size.is_zero()).then(|| SideFills {
            size: sum.size,
            average: sum.notional / sum.size,
            count: sum.count,
            realised: sum.realised,
        })
    };
    sums.into_iter()
        .map(|(bought, sold)| CandleFills {
            bought: side(bought),
            sold: side(sold),
        })
        .collect()
}

/// From when the account's trades are shown, when that is later than the
/// chart begins.
///
/// The venue returns only the latest fills. If it returned as many as it
/// gives at once and the earliest of them is after the chart's first candle,
/// the chart's earlier candles show no trades because none were fetched, not
/// because there were none. Otherwise there is nothing to say.
pub fn shown_since(fills: &Fills, first_candle_open_ms: u64) -> Option<u64> {
    if !fills.capped {
        return None;
    }
    let earliest = fills.rows.iter().map(|f| f.time_ms).min()?;
    (earliest > first_candle_open_ms).then_some(earliest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const M15: u64 = 15 * 60_000;
    const T0: u64 = 1_794_900_000_000;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn candles(n: u64) -> Vec<Candle> {
        (0..n)
            .map(|i| Candle {
                open_ms: T0 + i * M15,
                open: d("100"),
                high: d("101"),
                low: d("99"),
                close: d("100"),
            })
            .collect()
    }

    fn fill(
        coin: &str,
        side: FillSide,
        price: &str,
        size: &str,
        at_ms: u64,
        realised: &str,
    ) -> Fill {
        Fill {
            coin: coin.into(),
            side,
            price: d(price),
            size: d(size),
            time_ms: at_ms,
            realised: d(realised),
        }
    }

    use FillSide::{Bought, Sold};

    #[test]
    fn several_buys_in_one_candle_are_one_group_at_the_weighted_average() {
        let fills = [
            fill("NVDA", Bought, "100", "2", T0 + M15 + 10, "0"),
            fill("NVDA", Bought, "104", "6", T0 + M15 + 500_000, "0"),
        ];
        let grouped = group(&fills, &MarketId::new("NVDA"), &candles(4), M15);
        assert_eq!(grouped.len(), 4);
        let bought = grouped[1].bought.as_ref().unwrap();
        assert_eq!(bought.size, d("8"));
        assert_eq!(bought.average, d("103"));
        assert_eq!(bought.count, 2);
        assert!(grouped[1].sold.is_none());
        assert!(grouped[0].is_empty() && grouped[2].is_empty() && grouped[3].is_empty());
    }

    #[test]
    fn a_buy_and_a_sell_in_one_candle_are_two_groups_and_realised_pnl_adds_up() {
        let fills = [
            fill("NVDA", Bought, "100", "8", T0 + 1, "0"),
            fill("NVDA", Sold, "103", "5", T0 + 2, "15"),
            fill("NVDA", Sold, "103", "3", T0 + 3, "27.10"),
        ];
        let grouped = group(&fills, &MarketId::new("NVDA"), &candles(2), M15);
        let sold = grouped[0].sold.as_ref().unwrap();
        assert_eq!((sold.size, sold.average, sold.count), (d("8"), d("103"), 2));
        assert_eq!(sold.realised, d("42.10"));
        assert_eq!(grouped[0].bought.as_ref().unwrap().size, d("8"));
    }

    #[test]
    fn a_fill_belongs_to_the_candle_whose_span_contains_it() {
        let fills = [
            // The first instant of a candle is that candle's.
            fill("NVDA", Bought, "100", "1", T0 + M15, "0"),
            // The last instant before the next is still the earlier one's.
            fill("NVDA", Sold, "100", "1", T0 + 2 * M15 - 1, "0"),
        ];
        let grouped = group(&fills, &MarketId::new("NVDA"), &candles(3), M15);
        assert!(grouped[1].bought.is_some() && grouped[1].sold.is_some());
        assert!(grouped[0].is_empty() && grouped[2].is_empty());
    }

    #[test]
    fn a_fill_newer_than_the_last_closed_candle_is_on_the_one_forming() {
        let fills = [fill(
            "NVDA",
            Bought,
            "100",
            "1",
            T0 + 3 * M15 + 5 * M15,
            "0",
        )];
        let grouped = group(&fills, &MarketId::new("NVDA"), &candles(4), M15);
        assert!(grouped[3].bought.is_some());
    }

    #[test]
    fn fills_before_the_chart_and_in_other_markets_are_left_out() {
        let fills = [
            fill("NVDA", Bought, "100", "1", T0 - 1, "0"),
            fill("ETH", Bought, "100", "1", T0 + 1, "0"),
            fill("xyz:NVDA", Sold, "100", "1", T0 + 1, "0"),
        ];
        let grouped = group(&fills, &MarketId::new("NVDA"), &candles(2), M15);
        assert!(grouped.iter().all(CandleFills::is_empty));
        // A builder-dex market is its own market, matched by its full name.
        let xyz = group(&fills, &MarketId::new("xyz:NVDA"), &candles(2), M15);
        assert!(xyz[0].sold.is_some() && xyz[0].bought.is_none());
    }

    #[test]
    fn a_fill_in_a_gap_between_candles_is_in_neither() {
        let mut gapped = candles(3);
        gapped.remove(1);
        let fills = [fill("NVDA", Bought, "100", "1", T0 + M15 + 1, "0")];
        let grouped = group(&fills, &MarketId::new("NVDA"), &gapped, M15);
        assert!(grouped.iter().all(CandleFills::is_empty));
    }

    #[test]
    fn longer_candles_regroup_the_same_fills() {
        let fills = [
            fill("NVDA", Bought, "100", "1", T0 + 1, "0"),
            fill("NVDA", Bought, "102", "1", T0 + M15 + 1, "0"),
        ];
        let quarter = group(&fills, &MarketId::new("NVDA"), &candles(4), M15);
        assert_eq!(quarter.iter().filter(|c| c.bought.is_some()).count(), 2);
        let hour = [Candle {
            open_ms: T0,
            open: d("100"),
            high: d("101"),
            low: d("99"),
            close: d("100"),
        }];
        let grouped = group(&fills, &MarketId::new("NVDA"), &hour, 4 * M15);
        let bought = grouped[0].bought.as_ref().unwrap();
        assert_eq!(
            (bought.size, bought.average, bought.count),
            (d("2"), d("101"), 2)
        );
    }

    #[test]
    fn no_fills_or_no_candles_group_to_nothing() {
        assert!(
            group(&[], &MarketId::new("NVDA"), &candles(2), M15)
                .iter()
                .all(CandleFills::is_empty)
        );
        let fills = [fill("NVDA", Bought, "100", "1", T0, "0")];
        assert!(group(&fills, &MarketId::new("NVDA"), &[], M15).is_empty());
    }

    #[test]
    fn the_chart_says_how_far_back_only_when_the_venue_cut_the_fills_short() {
        let rows = vec![
            fill("ETH", Bought, "1", "1", T0 + 9 * M15, "0"),
            fill("NVDA", Bought, "1", "1", T0 + 5 * M15, "0"),
        ];
        // Cut short, and the earliest fill is after the chart begins.
        let capped = Fills {
            rows: rows.clone(),
            capped: true,
        };
        assert_eq!(shown_since(&capped, T0), Some(T0 + 5 * M15));
        // Cut short, but the fills reach back past the chart's start.
        assert_eq!(shown_since(&capped, T0 + 6 * M15), None);
        // Not cut short: these are all the fills there are.
        let all = Fills {
            rows,
            capped: false,
        };
        assert_eq!(shown_since(&all, T0), None);
        assert_eq!(shown_since(&Fills::default(), T0), None);
    }

    #[test]
    fn an_answer_as_long_as_the_venues_limit_counts_as_cut_short() {
        let row = fill("NVDA", Bought, "1", "1", T0, "0");
        assert!(Fills::new(vec![row.clone(); VENUE_CAP]).capped);
        assert!(!Fills::new(vec![row; VENUE_CAP - 1]).capped);
    }
}
