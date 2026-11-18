//! The account's PNL in each market over three windows: the last 30 days,
//! the year to date and all time.
//!
//! A window's PNL is what the account's fills in that market realised within
//! the window, less those fills' fees, plus the open position's whole
//! unrealised PNL. Funding payments are not counted: the venue cannot give
//! them by market. The unrealised part is the same in every window.

use std::collections::HashMap;

use rust_decimal::Decimal;
use time::{Date, Month, OffsetDateTime};

use super::MarketId;
use super::fills::Fills;
use crate::model::PositionRow;

/// Thirty days, in milliseconds.
pub const THIRTY_DAYS_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// One window's PNL in one market.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPnl {
    pub usd: Decimal,
    /// Older fills may exist that fall in the window but were not counted.
    pub partial: bool,
}

/// A market's PNL over each window. A window is `None` when the account has
/// no fill in it and no open position.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MarketPnl {
    pub d30: Option<WindowPnl>,
    pub ytd: Option<WindowPnl>,
    pub all: Option<WindowPnl>,
}

/// 00:00 UTC on 1 January of the year `now_ms` falls in, in milliseconds.
pub fn year_start_ms(now_ms: u64) -> u64 {
    let now = OffsetDateTime::from_unix_timestamp((now_ms / 1000) as i64)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let start = Date::from_calendar_date(now.year(), Month::January, 1)
        .expect("1 January exists")
        .midnight()
        .assume_utc();
    (start.unix_timestamp().max(0) as u64) * 1000
}

/// Each market's PNL, for every market the account has a fill or an open
/// position in. Markets missing from the answer have none of either.
pub fn market_pnl(
    fills: &Fills,
    positions: &[PositionRow],
    now_ms: u64,
) -> HashMap<MarketId, MarketPnl> {
    let starts = [
        now_ms.saturating_sub(THIRTY_DAYS_MS),
        year_start_ms(now_ms),
        0,
    ];
    let reaches_from = fills.reaches_from();

    // Net realised PNL per market and window, and whether any fill fell in it.
    let mut realised: HashMap<MarketId, [Option<Decimal>; 3]> = HashMap::new();
    for fill in &fills.rows {
        let sums = realised
            .entry(MarketId::new(fill.coin.as_str()))
            .or_default();
        for (sum, start) in sums.iter_mut().zip(starts) {
            if fill.time_ms >= start {
                *sum = Some(sum.unwrap_or_default() + fill.realised - fill.fee);
            }
        }
    }

    let unrealised: HashMap<MarketId, Decimal> = positions
        .iter()
        .filter(|p| !p.size.is_zero())
        .map(|p| (MarketId::from_parts(&p.dex, &p.market), p.pnl))
        .collect();

    let markets: Vec<MarketId> = realised.keys().chain(unrealised.keys()).cloned().collect();
    markets
        .into_iter()
        .map(|market| {
            let sums = realised.get(&market).copied().unwrap_or_default();
            let open = unrealised.get(&market).copied();
            let window = |i: usize| {
                let usd = match (sums[i], open) {
                    (None, None) => return None,
                    (sum, open) => sum.unwrap_or_default() + open.unwrap_or_default(),
                };
                Some(WindowPnl {
                    usd,
                    partial: reaches_from.is_some_and(|from| from > starts[i]),
                })
            };
            let pnl = MarketPnl {
                d30: window(0),
                ytd: window(1),
                all: window(2),
            };
            (market, pnl)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::fills::{Fill, FillSide};
    use crate::venue::MarginMode;

    const DAY: u64 = 24 * 60 * 60 * 1000;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    /// 2026-11-18 12:00 UTC.
    const NOW: u64 = 1_795_003_200_000;

    /// `YYYY-MM-DD HH:MM` in UTC, in milliseconds.
    fn ms(date: &str) -> u64 {
        let n = |r: std::ops::Range<usize>| date[r].parse::<u8>().unwrap();
        let year: i32 = date[0..4].parse().unwrap();
        let month = Month::try_from(n(5..7)).unwrap();
        let at = Date::from_calendar_date(year, month, n(8..10))
            .unwrap()
            .with_hms(n(11..13), n(14..16), 0)
            .unwrap()
            .assume_utc();
        at.unix_timestamp() as u64 * 1000
    }

    fn fill(coin: &str, time_ms: u64, realised: &str, fee: &str) -> Fill {
        Fill {
            coin: coin.into(),
            side: FillSide::Sold,
            price: Decimal::ONE,
            size: Decimal::ONE,
            time_ms,
            realised: d(realised),
            fee: d(fee),
            id: time_ms,
        }
    }

    fn position(dex: &str, market: &str, size: &str, pnl: &str) -> PositionRow {
        PositionRow {
            asset: None,
            market: market.into(),
            dex: dex.into(),
            leverage: 10,
            mode: MarginMode::Cross,
            size: d(size),
            value: Decimal::ZERO,
            entry: Decimal::ZERO,
            mark: None,
            pnl: d(pnl),
            roe: Decimal::ZERO,
            liquidation: None,
            margin: Decimal::ZERO,
            funding: Decimal::ZERO,
        }
    }

    fn all(rows: Vec<Fill>) -> Fills {
        Fills {
            rows,
            capped: false,
            walked: true,
        }
    }

    fn usd(w: Option<WindowPnl>) -> Option<Decimal> {
        w.map(|w| w.usd)
    }

    #[test]
    fn the_test_clock_is_where_it_says() {
        assert_eq!(NOW, ms("2026-11-18 12:00"));
        assert_eq!(year_start_ms(NOW), ms("2026-01-01 00:00"));
    }

    #[test]
    fn trades_and_an_open_position_add_up() {
        // Realised 1,200 in 30 days, with 40 in fees; open position -2,698.
        let fills = all(vec![
            fill("HYPE", NOW - 3 * DAY, "700", "25"),
            fill("HYPE", NOW - 10 * DAY, "500", "15"),
        ]);
        let pnl = market_pnl(&fills, &[position("", "HYPE", "843.85", "-2698")], NOW);
        let hype = pnl[&MarketId::new("HYPE")];
        assert_eq!(usd(hype.d30), Some(d("-1538")));
        assert!(!hype.d30.unwrap().partial);
    }

    #[test]
    fn no_trades_in_the_window_with_a_position_is_the_unrealised_pnl() {
        let fills = all(vec![fill("HYPE", NOW - 90 * DAY, "300", "0")]);
        let pnl = market_pnl(&fills, &[position("", "HYPE", "10", "820")], NOW);
        let hype = pnl[&MarketId::new("HYPE")];
        assert_eq!(usd(hype.d30), Some(d("820")));
        assert_eq!(usd(hype.ytd), Some(d("1120")));
        assert_eq!(usd(hype.all), Some(d("1120")));
    }

    #[test]
    fn a_position_closed_earlier_in_the_year_counts_in_the_year_not_the_month() {
        let fills = all(vec![fill("DOGE", ms("2026-03-12 09:00"), "-2180", "10")]);
        let pnl = market_pnl(&fills, &[], NOW);
        let doge = pnl[&MarketId::new("DOGE")];
        assert_eq!(doge.d30, None);
        assert_eq!(usd(doge.ytd), Some(d("-2190")));
        assert_eq!(usd(doge.all), Some(d("-2190")));
    }

    #[test]
    fn a_market_never_traded_has_no_entry() {
        let fills = all(vec![fill("BTC", NOW - DAY, "1", "0")]);
        let pnl = market_pnl(&fills, &[], NOW);
        assert!(!pnl.contains_key(&MarketId::new("xyz:GOLD")));
    }

    #[test]
    fn a_fill_on_the_last_evening_of_last_year_is_not_this_years() {
        let now = ms("2027-01-05 10:00");
        let fills = all(vec![fill("ETH", ms("2026-12-31 23:30"), "500", "0")]);
        let pnl = market_pnl(&fills, &[], now);
        let eth = pnl[&MarketId::new("ETH")];
        assert_eq!(usd(eth.d30), Some(d("500")));
        assert_eq!(eth.ytd, None);
        assert_eq!(usd(eth.all), Some(d("500")));
    }

    #[test]
    fn a_builder_dex_fill_and_position_meet_under_one_market() {
        let fills = all(vec![fill("xyz:NVDA", NOW - DAY, "524", "0")]);
        let pnl = market_pnl(&fills, &[position("xyz", "NVDA", "-123.467", "100")], NOW);
        assert_eq!(pnl.len(), 1);
        assert_eq!(usd(pnl[&MarketId::new("xyz:NVDA")].d30), Some(d("624")));
    }

    #[test]
    fn a_rebate_adds_to_the_pnl() {
        let fills = all(vec![fill("SOL", NOW - DAY, "10", "-0.5")]);
        let pnl = market_pnl(&fills, &[], NOW);
        assert_eq!(usd(pnl[&MarketId::new("SOL")].d30), Some(d("10.5")));
    }

    #[test]
    fn windows_that_start_before_the_earliest_fill_held_are_partial() {
        // The venue held back older fills; the earliest held is from February.
        let fills = Fills {
            rows: vec![
                fill("BTC", ms("2026-02-02 08:00"), "100", "0"),
                fill("BTC", NOW - DAY, "50", "0"),
            ],
            capped: true,
            walked: true,
        };
        let btc = market_pnl(&fills, &[], NOW)[&MarketId::new("BTC")];
        assert!(!btc.d30.unwrap().partial);
        assert!(btc.ytd.unwrap().partial);
        assert!(btc.all.unwrap().partial);

        // The same fills, but they are all there are.
        let complete = Fills {
            capped: false,
            ..fills
        };
        let btc = market_pnl(&complete, &[], NOW)[&MarketId::new("BTC")];
        assert!(!btc.ytd.unwrap().partial);
        assert!(!btc.all.unwrap().partial);
    }

    #[test]
    fn a_closed_position_with_size_zero_adds_no_unrealised_pnl() {
        let pnl = market_pnl(&all(vec![]), &[position("", "ARB", "0", "5")], NOW);
        assert!(pnl.is_empty());
    }
}
