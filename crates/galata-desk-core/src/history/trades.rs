//! The Trades tab: each fill, or each order's fills together.

use rust_decimal::Decimal;

use crate::market::fills::Fill;

/// Who took liquidity for a row: the account (taker), the other side
/// (maker), or, for an order's fills together, both at different times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Maker,
    Taker,
    Mixed,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Maker => "Maker",
            Role::Taker => "Taker",
            Role::Mixed => "Mixed",
        }
    }
}

/// One row of the Trades tab: a fill, or an order's fills together.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeRow {
    /// The latest fill's time.
    pub time_ms: u64,
    /// The venue's coin name: `BTC`, `xyz:NVDA`.
    pub coin: String,
    /// As the venue names it: `Open Long`, `Close Short`.
    pub dir: String,
    /// The fill's price, or the size-weighted average of an order's fills.
    pub price: Decimal,
    pub size: Decimal,
    pub value: Decimal,
    pub fee: Decimal,
    pub role: Role,
    /// PNL realised before fees; `None` for a fill that opened or added to
    /// a position, which realises nothing.
    pub closed_pnl: Option<Decimal>,
    /// How many fills the row holds.
    pub fills: usize,
    pub oid: u64,
    /// The latest fill's transaction.
    pub hash: String,
}

impl TradeRow {
    fn of(fill: &Fill) -> Self {
        TradeRow {
            time_ms: fill.time_ms,
            coin: fill.coin.clone(),
            dir: fill.dir.clone(),
            price: fill.price,
            size: fill.size,
            value: fill.price * fill.size,
            fee: fill.fee,
            role: if fill.taker { Role::Taker } else { Role::Maker },
            closed_pnl: realised(fill),
            fills: 1,
            oid: fill.oid,
            hash: fill.hash.clone(),
        }
    }

    /// Whether the row buys: opening a long, closing a short, flipping a
    /// short to a long, or buying spot.
    pub fn buys(&self) -> bool {
        matches!(
            self.dir.as_str(),
            "Open Long" | "Close Short" | "Short > Long" | "Buy"
        )
    }
}

/// What a fill realised, or `None` when it opened or added to a position.
fn realised(fill: &Fill) -> Option<Decimal> {
    if fill.dir.starts_with("Close") || !fill.realised.is_zero() {
        Some(fill.realised)
    } else {
        None
    }
}

/// The Trades tab's rows, newest first: one per fill, or with `grouped` one
/// per run of consecutive fills of the same order.
pub fn rows(fills: &[Fill], grouped: bool) -> Vec<TradeRow> {
    let mut sorted: Vec<&Fill> = fills.iter().collect();
    sorted.sort_by(|a, b| b.time_ms.cmp(&a.time_ms).then(b.id.cmp(&a.id)));
    let mut out: Vec<TradeRow> = Vec::new();
    for fill in sorted {
        match out.last_mut() {
            Some(row) if grouped && row.oid == fill.oid && fill.oid != 0 => {
                row.size += fill.size;
                row.value += fill.price * fill.size;
                row.fee += fill.fee;
                row.fills += 1;
                row.closed_pnl = match (row.closed_pnl, realised(fill)) {
                    (None, None) => None,
                    (a, b) => Some(a.unwrap_or_default() + b.unwrap_or_default()),
                };
                let role = if fill.taker { Role::Taker } else { Role::Maker };
                if row.role != role {
                    row.role = Role::Mixed;
                }
                if !row.size.is_zero() {
                    row.price = row.value / row.size;
                }
            }
            _ => out.push(TradeRow::of(fill)),
        }
    }
    out
}

/// What the Trades tab sums over the rows it lists.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TradeTotals {
    pub rows: usize,
    pub fills: usize,
    pub volume: Decimal,
    pub fees: Decimal,
    pub closed_pnl: Decimal,
}

pub fn totals<'a>(rows: impl IntoIterator<Item = &'a TradeRow>) -> TradeTotals {
    rows.into_iter()
        .fold(TradeTotals::default(), |t, r| TradeTotals {
            rows: t.rows + 1,
            fills: t.fills + r.fills,
            volume: t.volume + r.value,
            fees: t.fees + r.fee,
            closed_pnl: t.closed_pnl + r.closed_pnl.unwrap_or_default(),
        })
}

/// The Trades tab's filters: one market, one direction, or any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TradeFilter {
    pub coin: Option<String>,
    pub dir: Option<String>,
}

impl TradeFilter {
    pub fn admits(&self, row: &TradeRow) -> bool {
        self.coin.as_ref().is_none_or(|c| *c == row.coin)
            && self.dir.as_ref().is_none_or(|d| *d == row.dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::fills::FillSide;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn fill(
        id: u64,
        oid: u64,
        coin: &str,
        dir: &str,
        price: &str,
        size: &str,
        fee: &str,
        taker: bool,
        pnl: &str,
    ) -> Fill {
        Fill {
            coin: coin.into(),
            side: if dir.ends_with("Long") == dir.starts_with("Open") {
                FillSide::Bought
            } else {
                FillSide::Sold
            },
            price: d(price),
            size: d(size),
            time_ms: 1_795_000_000_000 + id * 1000,
            realised: d(pnl),
            fee: d(fee),
            id,
            dir: dir.into(),
            taker,
            oid,
            hash: format!("0x{id}"),
        }
    }

    /// The spec's NVDA order: five fills totalling 19.984 at 150.12.
    fn nvda() -> Vec<Fill> {
        [
            ("0.347", "0"),
            ("6.65", "0.03"),
            ("5.97", "0.03"),
            ("0.367", "0"),
            ("6.65", "0.03"),
        ]
        .iter()
        .enumerate()
        .map(|(i, (size, fee))| {
            fill(
                i as u64 + 1,
                68,
                "xyz:NVDA",
                "Open Long",
                "150.12",
                size,
                fee,
                true,
                "0",
            )
        })
        .collect()
    }

    #[test]
    fn an_opening_fill_has_no_closed_pnl_and_a_closing_one_does() {
        let fills = [
            fill(
                1,
                1,
                "xyz:ANT",
                "Open Short",
                "105",
                "28.57",
                "0.09",
                false,
                "0",
            ),
            fill(
                2,
                2,
                "xyz:ANT",
                "Close Short",
                "101",
                "14.85",
                "0.04",
                false,
                "21.24",
            ),
        ];
        let rows = rows(&fills, false);
        // Newest first.
        assert_eq!(rows[0].dir, "Close Short");
        assert_eq!(rows[0].closed_pnl, Some(d("21.24")));
        assert!(rows[0].buys());
        assert_eq!(rows[1].closed_pnl, None);
        assert_eq!(rows[1].value, d("2999.85"));
        assert_eq!(rows[1].role, Role::Maker);
        assert!(!rows[1].buys());
    }

    #[test]
    fn five_fills_of_one_order_are_one_row_when_grouped() {
        let grouped = rows(&nvda(), true);
        assert_eq!(grouped.len(), 1);
        let row = &grouped[0];
        assert_eq!(row.size, d("19.984"));
        assert_eq!(row.price, d("150.12"));
        assert_eq!(row.fills, 5);
        assert_eq!(row.fee, d("0.09"));
        assert_eq!(row.role, Role::Taker);
        assert_eq!(row.closed_pnl, None);
        assert_eq!(row.time_ms, 1_795_000_005_000);
        assert_eq!(rows(&nvda(), false).len(), 5);
    }

    #[test]
    fn a_grouped_price_is_weighted_by_size_and_mixed_roles_say_so() {
        let fills = [
            fill(1, 9, "BTC", "Close Long", "100", "1", "0.1", true, "5"),
            fill(2, 9, "BTC", "Close Long", "110", "3", "0.2", false, "-2"),
        ];
        let row = &rows(&fills, true)[0];
        assert_eq!(row.price, d("107.5"));
        assert_eq!(row.role, Role::Mixed);
        assert_eq!(row.closed_pnl, Some(d("3")));
    }

    #[test]
    fn only_consecutive_fills_of_an_order_are_grouped() {
        let fills = [
            fill(1, 7, "BTC", "Open Long", "100", "1", "0", true, "0"),
            fill(2, 8, "ETH", "Open Long", "10", "1", "0", true, "0"),
            fill(3, 7, "BTC", "Open Long", "100", "1", "0", true, "0"),
        ];
        assert_eq!(rows(&fills, true).len(), 3);
    }

    #[test]
    fn totals_follow_the_filter() {
        let mut fills = nvda();
        fills.push(fill(
            10,
            1,
            "xyz:ANT",
            "Open Short",
            "105",
            "28.57",
            "0.09",
            false,
            "0",
        ));
        fills.push(fill(
            11,
            2,
            "xyz:ANT",
            "Close Short",
            "101",
            "14.85",
            "0.04",
            false,
            "21.24",
        ));
        let all = rows(&fills, true);
        let ant = TradeFilter {
            coin: Some("xyz:ANT".into()),
            dir: None,
        };
        let t = totals(all.iter().filter(|r| ant.admits(r)));
        assert_eq!(t.rows, 2);
        assert_eq!(t.fills, 2);
        assert_eq!(t.volume, d("2999.85") + d("1499.85"));
        assert_eq!(t.fees, d("0.13"));
        assert_eq!(t.closed_pnl, d("21.24"));
        let opens = TradeFilter {
            coin: None,
            dir: Some("Open Long".into()),
        };
        assert_eq!(totals(all.iter().filter(|r| opens.admits(r))).fills, 5);
    }
}
