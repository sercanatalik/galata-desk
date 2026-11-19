//! The Orders tab: each order once, at its latest status.

use std::collections::HashMap;

use rust_decimal::Decimal;

use crate::venue::{HistoricalOrder, Side};

/// The most rows the venue returns. A full answer may have left out older
/// orders, and cannot be paged.
pub const VENUE_CAP: usize = 2000;

/// An order's status in plain words. The venue's own word is kept beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Open,
    Filled,
    Cancelled,
    Triggered,
    Rejected,
    /// A status this build cannot place; shown in the venue's words.
    Other,
}

impl Status {
    pub const LISTED: [Status; 5] = [
        Status::Open,
        Status::Filled,
        Status::Cancelled,
        Status::Triggered,
        Status::Rejected,
    ];

    pub fn of(venue: &str) -> Status {
        match venue {
            "open" => Status::Open,
            "filled" => Status::Filled,
            "triggered" => Status::Triggered,
            "canceled" | "cancelled" => Status::Cancelled,
            "rejected" => Status::Rejected,
            v if v.ends_with("Canceled") || v.ends_with("Cancelled") => Status::Cancelled,
            v if v.ends_with("Rejected") => Status::Rejected,
            _ => Status::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Open => "Open",
            Status::Filled => "Filled",
            Status::Cancelled => "Cancelled",
            Status::Triggered => "Triggered",
            Status::Rejected => "Rejected",
            Status::Other => "Other",
        }
    }
}

/// Why the venue gave a status, in words, when it is a variant of a plain
/// one: what a status's tooltip says.
pub fn reason(venue: &str) -> Option<String> {
    let known = match venue {
        "open" | "filled" | "triggered" | "canceled" | "rejected" => return None,
        "reduceOnlyCanceled" => {
            "Cancelled because it was reduce-only and would no longer have reduced the position"
        }
        "marginCanceled" => "Cancelled because the account lacked the margin for it",
        "selfTradeCanceled" => {
            "Cancelled because it would have traded against the account's own order"
        }
        "siblingFilledCanceled" => "Cancelled because its paired take-profit or stop-loss filled",
        "scheduledCancel" => "Cancelled by a scheduled cancel",
        "vaultWithdrawalCanceled" => "Cancelled for a vault withdrawal",
        "liquidatedCanceled" => "Cancelled when the position was liquidated",
        "openInterestCapCanceled" => "Cancelled because the market's open interest was at its cap",
        "badAloPxRejected" => {
            "Rejected because a post-only order at that price would have crossed the book"
        }
        "iocCancelRejected" => {
            "Rejected because an immediate-or-cancel order found nothing to fill against"
        }
        "insufficientSpotBalanceRejected" => "Rejected for lack of spot balance",
        "oracleRejected" => "Rejected because its price was too far from the oracle price",
        "perpMarginRejected" => "Rejected for lack of margin",
        "reduceOnlyRejected" => "Rejected because it would not have reduced the position",
        "minTradeNtlRejected" => "Rejected because it was worth less than the $10 minimum",
        _ => return Some(format!("The venue says: {}", super::words(venue))),
    };
    Some(known.to_string())
}

/// One row of the Orders tab.
#[derive(Debug, Clone, PartialEq)]
pub struct PastOrder {
    pub placed_ms: u64,
    /// When the order reached its status.
    pub status_ms: u64,
    pub coin: String,
    /// `Long`, `Short`, `Close Long` or `Close Short`.
    pub direction: &'static str,
    /// The venue's order type: `Limit`, `Take Profit Market`.
    pub kind: String,
    /// `None` for a take-profit or stop-loss that covers the whole position.
    pub size: Option<Decimal>,
    /// `None` when nothing filled.
    pub filled: Option<Decimal>,
    /// `None` for an order at market.
    pub price: Option<Decimal>,
    /// Size times price; `None` at market or for a whole-position order.
    pub value: Option<Decimal>,
    pub reduce_only: bool,
    /// The trigger condition, for a trigger order: `Price below 85`.
    pub trigger: Option<String>,
    pub status: Status,
    /// The venue's own word for the status.
    pub venue_status: String,
    pub oid: u64,
}

impl PastOrder {
    fn of(row: &HistoricalOrder) -> Self {
        let o = &row.order;
        let buy = o.side == Side::Buy;
        let direction = match (buy, o.reduce_only) {
            (true, false) => "Long",
            (false, false) => "Short",
            (true, true) => "Close Short",
            (false, true) => "Close Long",
        };
        let size = (!o.orig_sz.is_zero()).then_some(o.orig_sz);
        let filled = o.orig_sz - o.sz;
        let price = (!o.order_type.contains("Market")).then_some(o.limit_px);
        PastOrder {
            placed_ms: o.timestamp,
            status_ms: row.status_timestamp,
            coin: o.coin.clone(),
            direction,
            kind: o.order_type.clone(),
            size,
            filled: (filled > Decimal::ZERO).then_some(filled),
            price,
            value: size.zip(price).map(|(s, p)| s * p),
            reduce_only: o.reduce_only,
            trigger: o.is_trigger.then(|| o.trigger_condition.clone()),
            status: Status::of(&row.status),
            venue_status: row.status.clone(),
            oid: o.oid,
        }
    }

    pub fn buys(&self) -> bool {
        matches!(self.direction, "Long" | "Close Short")
    }
}

/// One row per order, at its latest status, newest placed first.
pub fn collapse(rows: &[HistoricalOrder]) -> Vec<PastOrder> {
    let mut latest: HashMap<u64, &HistoricalOrder> = HashMap::new();
    for row in rows {
        latest
            .entry(row.order.oid)
            .and_modify(|held| {
                if row.status_timestamp > held.status_timestamp {
                    *held = row;
                }
            })
            .or_insert(row);
    }
    let mut out: Vec<PastOrder> = latest.values().map(|r| PastOrder::of(r)).collect();
    out.sort_by(|a, b| b.placed_ms.cmp(&a.placed_ms).then(b.oid.cmp(&a.oid)));
    out
}

/// Merge a newer answer into the rows held: a change of status is kept
/// once, however many answers carry it.
pub fn merge(held: &mut Vec<HistoricalOrder>, answer: Vec<HistoricalOrder>) {
    for row in answer {
        let seen = held.iter().any(|h| {
            h.order.oid == row.order.oid
                && h.status == row.status
                && h.status_timestamp == row.status_timestamp
        });
        if !seen {
            held.push(row);
        }
    }
    held.sort_by_key(|r| std::cmp::Reverse(r.status_timestamp));
}

/// From when orders are shown, when the venue's answer was full: the
/// oldest change of status in it. `None` when the answer held every row.
pub fn shown_from(answer_len: usize, rows: &[HistoricalOrder]) -> Option<u64> {
    if answer_len < VENUE_CAP {
        return None;
    }
    rows.iter().map(|r| r.status_timestamp).min()
}

/// How many orders the tab lists, and how many are in each status.
pub fn counts<'a>(
    rows: impl IntoIterator<Item = &'a PastOrder>,
) -> (usize, HashMap<Status, usize>) {
    let mut by: HashMap<Status, usize> = HashMap::new();
    let mut all = 0;
    for r in rows {
        all += 1;
        *by.entry(r.status).or_default() += 1;
    }
    (all, by)
}

/// The Orders tab's filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderFilter {
    pub status: Option<Status>,
    pub coin: Option<String>,
    pub kind: Option<String>,
}

impl OrderFilter {
    pub fn admits(&self, row: &PastOrder) -> bool {
        self.status.is_none_or(|s| s == row.status)
            && self.coin.as_ref().is_none_or(|c| *c == row.coin)
            && self.kind.as_ref().is_none_or(|k| *k == row.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::OpenOrder;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn row(
        oid: u64,
        status: &str,
        at: u64,
        sz: &str,
        orig: &str,
        kind: &str,
        reduce: bool,
    ) -> HistoricalOrder {
        HistoricalOrder {
            order: OpenOrder {
                coin: "xyz:ANT".into(),
                side: Side::Sell,
                limit_px: d("105"),
                sz: d(sz),
                orig_sz: d(orig),
                oid,
                timestamp: 1_000 + oid,
                order_type: kind.into(),
                reduce_only: reduce,
                is_trigger: kind != "Limit",
                trigger_condition: if kind == "Limit" {
                    "N/A".into()
                } else {
                    "Price below 85".into()
                },
                trigger_px: if kind == "Limit" {
                    Decimal::ZERO
                } else {
                    d("85")
                },
                is_position_tpsl: orig == "0",
                tif: None,
            },
            status: status.into(),
            status_timestamp: at,
        }
    }

    #[test]
    fn an_order_that_rested_and_filled_is_one_row_at_filled() {
        let rows = [
            row(1, "filled", 20, "0", "28.57", "Limit", false),
            row(1, "open", 10, "28.57", "28.57", "Limit", false),
        ];
        let orders = collapse(&rows);
        assert_eq!(orders.len(), 1);
        let o = &orders[0];
        assert_eq!(o.status, Status::Filled);
        assert_eq!(o.placed_ms, 1_001);
        assert_eq!(o.direction, "Short");
        assert_eq!(o.size, Some(d("28.57")));
        assert_eq!(o.filled, Some(d("28.57")));
        assert_eq!(o.value, Some(d("2999.85")));
        assert_eq!(o.trigger, None);
    }

    #[test]
    fn a_take_profit_at_market_has_no_price_or_value() {
        let o = &collapse(&[row(2, "open", 5, "0", "0", "Take Profit Market", true)])[0];
        assert_eq!(o.direction, "Close Long");
        assert_eq!(o.price, None);
        assert_eq!(o.size, None);
        assert_eq!(o.value, None);
        assert_eq!(o.filled, None);
        assert_eq!(o.trigger.as_deref(), Some("Price below 85"));
        assert!(o.reduce_only);
    }

    #[test]
    fn each_venue_status_has_a_plain_word_and_variants_a_reason() {
        assert_eq!(Status::of("open"), Status::Open);
        assert_eq!(Status::of("filled"), Status::Filled);
        assert_eq!(Status::of("triggered"), Status::Triggered);
        assert_eq!(Status::of("canceled"), Status::Cancelled);
        assert_eq!(Status::of("reduceOnlyCanceled"), Status::Cancelled);
        assert_eq!(Status::of("marginCanceled"), Status::Cancelled);
        assert_eq!(Status::of("badAloPxRejected"), Status::Rejected);
        assert_eq!(Status::of("iocCancelRejected"), Status::Rejected);
        assert_eq!(Status::of("somethingNew"), Status::Other);
        assert_eq!(reason("canceled"), None);
        assert!(
            reason("reduceOnlyCanceled")
                .unwrap()
                .contains("reduce-only")
        );
        assert!(reason("badAloPxRejected").unwrap().contains("post-only"));
        assert_eq!(
            reason("somethingNew").unwrap(),
            "The venue says: Something new"
        );
    }

    #[test]
    fn a_refresh_adds_a_change_of_status_once() {
        let mut held = vec![row(1, "open", 10, "28.57", "28.57", "Limit", false)];
        merge(
            &mut held,
            vec![
                row(1, "filled", 20, "0", "28.57", "Limit", false),
                row(1, "open", 10, "28.57", "28.57", "Limit", false),
            ],
        );
        assert_eq!(held.len(), 2);
        assert_eq!(held[0].status, "filled");
        assert_eq!(collapse(&held)[0].status, Status::Filled);
    }

    #[test]
    fn a_full_answer_says_from_when_orders_are_shown() {
        let rows: Vec<HistoricalOrder> = (0..VENUE_CAP as u64)
            .map(|i| row(i, "canceled", 5_000 - i, "1", "1", "Limit", false))
            .collect();
        assert_eq!(
            shown_from(rows.len(), &rows),
            Some(5_000 - (VENUE_CAP as u64 - 1))
        );
        assert_eq!(shown_from(3, &rows[..3]), None);
    }

    #[test]
    fn filters_and_counts_by_status() {
        let rows = [
            row(1, "filled", 20, "0", "1", "Limit", false),
            row(2, "open", 21, "1", "1", "Limit", false),
            row(
                3,
                "reduceOnlyCanceled",
                22,
                "1",
                "1",
                "Take Profit Market",
                true,
            ),
        ];
        let orders = collapse(&rows);
        let open = OrderFilter {
            status: Some(Status::Open),
            ..Default::default()
        };
        assert_eq!(orders.iter().filter(|o| open.admits(o)).count(), 1);
        let (all, by) = counts(&orders);
        assert_eq!(all, 3);
        assert_eq!(by[&Status::Cancelled], 1);
        let tp = OrderFilter {
            kind: Some("Take Profit Market".into()),
            ..Default::default()
        };
        assert_eq!(orders.iter().filter(|o| tp.admits(o)).count(), 1);
    }
}
