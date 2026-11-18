//! What open positions pay and earn in funding.
//!
//! Everything here is from the account's side: a plus is funding received.
//! The venue's rate keeps its own sign: positive means longs pay shorts.

pub mod feed;
pub mod history;

use std::collections::HashMap;

use rust_decimal::Decimal;

use crate::market::MarketId;
use crate::model::PositionRow;

/// Hourly funding rates by the venue's coin name (`BTC`, `xyz:NVDA`), as
/// fractions.
pub type FundingRates = HashMap<String, Decimal>;

/// One open position's funding.
#[derive(Debug, Clone, PartialEq)]
pub struct FundingRow {
    /// The asset, without any dex prefix.
    pub market: String,
    /// The builder dex; empty for the main dex.
    pub dex: String,
    pub leverage: u32,
    /// Signed: negative is short.
    pub size: Decimal,
    pub value: Decimal,
    /// The market's current hourly rate; `None` when the venue's rates do not
    /// include the market.
    pub rate: Option<Decimal>,
    /// What the position receives per hour at that rate; negative is paid.
    pub per_hour: Option<Decimal>,
    /// Funding since the position opened; positive was received.
    pub since_open: Decimal,
}

impl FundingRow {
    pub fn per_day(&self) -> Option<Decimal> {
        self.per_hour.map(|h| h * Decimal::from(24))
    }

    /// The rate over a year, as a fraction.
    pub fn yearly_rate(&self) -> Option<Decimal> {
        self.rate.map(|r| r * Decimal::from(24 * 365))
    }
}

/// The venue's coin name for a position's market.
pub fn coin(market: &str, dex: &str) -> String {
    if dex.is_empty() {
        market.to_string()
    } else {
        format!("{dex}:{market}")
    }
}

/// One row per position, the largest hourly flow first whichever way it
/// runs; positions whose rate is not known last, in the order given.
pub fn rows(positions: &[PositionRow], rates: &FundingRates) -> Vec<FundingRow> {
    let mut rows: Vec<FundingRow> = positions
        .iter()
        .map(|p| {
            let rate = rates.get(&coin(&p.market, &p.dex)).copied();
            // A long pays a positive rate; a short receives it.
            let side = if p.is_long() {
                -Decimal::ONE
            } else {
                Decimal::ONE
            };
            FundingRow {
                market: p.market.clone(),
                dex: p.dex.clone(),
                leverage: p.leverage,
                size: p.size,
                value: p.value,
                rate,
                per_hour: rate.map(|r| side * p.value.abs() * r),
                since_open: p.funding,
            }
        })
        .collect();
    // Stable: equal flows, and the rows without one, keep the order given.
    rows.sort_by(|a, b| match (a.per_hour, b.per_hour) {
        (Some(x), Some(y)) => y.abs().cmp(&x.abs()),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    rows
}

/// A watched market the account holds no position in.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchedRow {
    pub market: MarketId,
    /// The market's current hourly rate; `None` when the venue's rates do not
    /// include the market.
    pub rate: Option<Decimal>,
}

impl WatchedRow {
    /// The rate over a year, as a fraction.
    pub fn yearly_rate(&self) -> Option<Decimal> {
        self.rate.map(|r| r * Decimal::from(24 * 365))
    }
}

/// One row per watched market without a position, in the watchlist's order.
/// A market that is held is a position's row and is left out here.
pub fn watched(
    watchlist: &[MarketId],
    positions: &[PositionRow],
    rates: &FundingRates,
) -> Vec<WatchedRow> {
    watchlist
        .iter()
        .filter(|market| {
            !positions
                .iter()
                .any(|p| p.market == market.ticker() && p.dex == market.dex())
        })
        .map(|market| WatchedRow {
            market: market.clone(),
            rate: rates.get(market.coin()).copied(),
        })
        .collect()
}

/// The account's funding in one line.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FundingSummary {
    /// Net received per hour over the positions with a known rate.
    pub per_hour: Decimal,
    /// Funding since the open positions opened.
    pub since_open: Decimal,
    /// Positions that receive, and that pay, at current rates. A position at
    /// a zero rate is in neither.
    pub earning: usize,
    pub paying: usize,
    /// Positions left out of the figures above: no rate for their market.
    pub without_rate: usize,
    pub positions: usize,
}

impl FundingSummary {
    pub fn per_day(&self) -> Decimal {
        self.per_hour * Decimal::from(24)
    }
}

pub fn summary(rows: &[FundingRow]) -> FundingSummary {
    let mut s = FundingSummary {
        positions: rows.len(),
        ..FundingSummary::default()
    };
    for row in rows {
        s.since_open += row.since_open;
        match row.per_hour {
            Some(flow) => {
                s.per_hour += flow;
                if flow > Decimal::ZERO {
                    s.earning += 1;
                } else if flow < Decimal::ZERO {
                    s.paying += 1;
                }
            }
            None => s.without_rate += 1,
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::MarginMode;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn position(market: &str, dex: &str, size: &str, value: &str, funding: &str) -> PositionRow {
        PositionRow {
            asset: None,
            market: market.into(),
            dex: dex.into(),
            leverage: 20,
            mode: MarginMode::Cross,
            size: d(size),
            value: d(value),
            entry: Decimal::ONE,
            mark: None,
            pnl: Decimal::ZERO,
            roe: Decimal::ZERO,
            liquidation: None,
            margin: Decimal::ZERO,
            funding: d(funding),
        }
    }

    fn rates(pairs: &[(&str, &str)]) -> FundingRates {
        pairs.iter().map(|(c, r)| (c.to_string(), d(r))).collect()
    }

    #[test]
    fn a_long_pays_a_positive_rate_and_a_short_earns_it() {
        let positions = [
            position("BTC", "", "1", "50000", "0"),
            position("ETH", "", "-10", "50000", "0"),
        ];
        // +0.0010% an hour.
        let rows = rows(
            &positions,
            &rates(&[("BTC", "0.00001"), ("ETH", "0.00001")]),
        );
        let btc = rows.iter().find(|r| r.market == "BTC").unwrap();
        let eth = rows.iter().find(|r| r.market == "ETH").unwrap();
        assert_eq!(btc.per_hour, Some(d("-0.5")));
        assert_eq!(btc.per_day(), Some(d("-12")));
        assert_eq!(eth.per_hour, Some(d("0.5")));
        assert_eq!(eth.per_day(), Some(d("12")));
    }

    #[test]
    fn a_negative_rate_reverses_who_pays() {
        let positions = [
            position("BTC", "", "1", "50000", "0"),
            position("ETH", "", "-10", "50000", "0"),
        ];
        let rows = rows(
            &positions,
            &rates(&[("BTC", "-0.00001"), ("ETH", "-0.00001")]),
        );
        assert_eq!(
            rows.iter().find(|r| r.market == "BTC").unwrap().per_hour,
            Some(d("0.5"))
        );
        assert_eq!(
            rows.iter().find(|r| r.market == "ETH").unwrap().per_hour,
            Some(d("-0.5"))
        );
    }

    #[test]
    fn the_yearly_rate_is_the_hourly_rate_over_a_year() {
        let rows = rows(
            &[position("BTC", "", "1", "1", "0")],
            &rates(&[("BTC", "0.000013")]),
        );
        // 0.0013% x 8,760 hours = 11.388%.
        assert_eq!(rows[0].yearly_rate(), Some(d("0.11388")));
    }

    #[test]
    fn a_builder_dex_market_is_found_by_its_prefixed_name() {
        let rows = rows(
            &[position("NVDA", "xyz", "100", "15000", "0")],
            &rates(&[("xyz:NVDA", "0.00002"), ("NVDA", "0.5")]),
        );
        assert_eq!(rows[0].rate, Some(d("0.00002")));
        assert_eq!(rows[0].per_hour, Some(d("-0.3")));
        assert_eq!(coin("NVDA", "xyz"), "xyz:NVDA");
        assert_eq!(coin("BTC", ""), "BTC");
    }

    #[test]
    fn the_largest_flows_come_first_whichever_way_they_run() {
        let positions = [
            position("A", "", "1", "20000", "0"),   // -0.20
            position("B", "", "-1", "133000", "0"), // +1.33
            position("C", "", "1", "71000", "0"),   // -0.71
        ];
        let all = rates(&[("A", "0.00001"), ("B", "0.00001"), ("C", "0.00001")]);
        let flows: Vec<Decimal> = rows(&positions, &all)
            .iter()
            .map(|r| r.per_hour.unwrap())
            .collect();
        assert_eq!(flows, [d("1.33"), d("-0.71"), d("-0.2")]);
    }

    #[test]
    fn a_position_without_a_rate_comes_last_and_is_left_out_of_the_net() {
        let positions = [
            position("NEW", "", "1", "999999", "5"),
            position("B", "", "-1", "133000", "10"),
            position("C", "", "1", "71000", "-3"),
        ];
        let rows = rows(&positions, &rates(&[("B", "0.00001"), ("C", "0.00001")]));
        assert_eq!(rows.last().unwrap().market, "NEW");
        assert_eq!(rows.last().unwrap().per_hour, None);
        assert_eq!(rows.last().unwrap().per_day(), None);
        let s = summary(&rows);
        assert_eq!(s.per_hour, d("0.62"));
        assert_eq!(s.per_day(), d("14.88"));
        assert_eq!(
            (s.earning, s.paying, s.without_rate, s.positions),
            (1, 1, 1, 3)
        );
        // Funding since open is the venue's own figure, so it counts them all.
        assert_eq!(s.since_open, d("12"));
    }

    #[test]
    fn a_zero_rate_neither_earns_nor_pays() {
        let rows = rows(&[position("A", "", "1", "100", "0")], &rates(&[("A", "0")]));
        let s = summary(&rows);
        assert_eq!((s.earning, s.paying, s.without_rate), (0, 0, 0));
        assert_eq!(s.per_hour, Decimal::ZERO);
    }

    fn markets(coins: &[&str]) -> Vec<MarketId> {
        coins.iter().map(|c| MarketId::new(*c)).collect()
    }

    #[test]
    fn watched_rows_keep_the_watchlists_order_and_leave_out_what_is_held() {
        let positions = [
            position("NVDA", "xyz", "100", "15000", "0"),
            position("BTC", "", "1", "50000", "0"),
        ];
        let all = rates(&[
            ("DOGE", "-0.000005"),
            ("XRP", "0.000019"),
            ("xyz:TSLA", "0.000012"),
            ("xyz:NVDA", "0.00002"),
            ("BTC", "0.00001"),
        ]);
        let rows = watched(
            &markets(&["DOGE", "xyz:NVDA", "XRP", "BTC", "xyz:TSLA"]),
            &positions,
            &all,
        );
        // XRP has the highest rate and still comes second: no sorting.
        let names: Vec<&str> = rows.iter().map(|r| r.market.coin()).collect();
        assert_eq!(names, ["DOGE", "XRP", "xyz:TSLA"]);
        assert_eq!(rows[1].rate, Some(d("0.000019")));
        // 0.0019% x 8,760 hours.
        assert_eq!(rows[1].yearly_rate(), Some(d("0.16644")));
        // A builder-dex market keeps its dex.
        assert_eq!(
            (rows[2].market.dex(), rows[2].market.ticker()),
            ("xyz", "TSLA")
        );
        assert_eq!(rows[2].rate, Some(d("0.000012")));
    }

    #[test]
    fn a_main_dex_position_does_not_hide_a_builder_dex_market_of_the_same_name() {
        let positions = [position("NVDA", "", "1", "100", "0")];
        let rows = watched(&markets(&["xyz:NVDA"]), &positions, &FundingRates::new());
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn a_watched_market_the_rates_do_not_include_has_no_rate() {
        let rows = watched(&markets(&["GONE"]), &[], &rates(&[("BTC", "0.00001")]));
        assert_eq!(rows[0].rate, None);
        assert_eq!(rows[0].yearly_rate(), None);
    }

    #[test]
    fn an_empty_watchlist_has_no_watched_rows() {
        let positions = [position("BTC", "", "1", "50000", "0")];
        assert!(watched(&[], &positions, &FundingRates::new()).is_empty());
    }

    #[test]
    fn no_positions_is_an_empty_summary() {
        let s = summary(&rows(&[], &FundingRates::new()));
        assert_eq!(s, FundingSummary::default());
    }
}
