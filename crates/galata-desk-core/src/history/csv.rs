//! Each tab's rows as CSV: one header line, one line per listed row, full
//! precision, signed amounts, times in UTC.

use rust_decimal::Decimal;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::funding::{MarketFunding, Payment};
use super::ledger::Transfer;
use super::orders::PastOrder;
use super::trades::TradeRow;

/// A time in milliseconds as ISO 8601 in UTC: `2026-11-19T15:14:38.000Z`.
pub fn utc(ms: u64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_default()
}

/// One field, quoted when it holds a comma, a quote or a line break.
fn field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

fn num(d: Decimal) -> String {
    d.normalize().to_string()
}

fn opt(d: Option<Decimal>) -> String {
    d.map(num).unwrap_or_default()
}

fn write(header: &[&str], lines: impl IntoIterator<Item = Vec<String>>) -> String {
    let mut out = header
        .iter()
        .map(|h| field(h))
        .collect::<Vec<_>>()
        .join(",");
    out.push('\n');
    for line in lines {
        out.push_str(&line.iter().map(|f| field(f)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

pub fn trades<'a>(rows: impl IntoIterator<Item = &'a TradeRow>) -> String {
    write(
        &[
            "time_utc",
            "market",
            "direction",
            "price",
            "size",
            "value_usd",
            "fee_usd",
            "role",
            "closed_pnl_usd",
            "fills",
            "order_id",
            "hash",
        ],
        rows.into_iter().map(|r| {
            vec![
                utc(r.time_ms),
                r.coin.clone(),
                r.dir.clone(),
                num(r.price),
                num(r.size),
                num(r.value),
                num(r.fee),
                r.role.label().to_string(),
                opt(r.closed_pnl),
                r.fills.to_string(),
                r.oid.to_string(),
                r.hash.clone(),
            ]
        }),
    )
}

pub fn orders<'a>(rows: impl IntoIterator<Item = &'a PastOrder>) -> String {
    write(
        &[
            "placed_utc",
            "status_utc",
            "market",
            "direction",
            "type",
            "size",
            "filled",
            "price",
            "value_usd",
            "reduce_only",
            "trigger",
            "status",
            "venue_status",
            "order_id",
        ],
        rows.into_iter().map(|r| {
            vec![
                utc(r.placed_ms),
                utc(r.status_ms),
                r.coin.clone(),
                r.direction.to_string(),
                r.kind.clone(),
                r.size.map(num).unwrap_or_else(|| "position".into()),
                opt(r.filled),
                r.price.map(num).unwrap_or_else(|| "market".into()),
                opt(r.value),
                r.reduce_only.to_string(),
                r.trigger.clone().unwrap_or_default(),
                r.status.label().to_string(),
                r.venue_status.clone(),
                r.oid.to_string(),
            ]
        }),
    )
}

pub fn payments<'a>(rows: impl IntoIterator<Item = &'a Payment>) -> String {
    write(
        &[
            "time_utc",
            "market",
            "side",
            "size",
            "payment_usd",
            "hourly_rate",
            "payments",
        ],
        rows.into_iter().map(|p| {
            vec![
                utc(p.time_ms),
                p.coin.clone(),
                if p.long { "Long" } else { "Short" }.to_string(),
                num(p.size),
                num(p.usdc),
                num(p.rate),
                p.payments.to_string(),
            ]
        }),
    )
}

pub fn funding_by_market<'a>(rows: impl IntoIterator<Item = &'a MarketFunding>) -> String {
    write(
        &[
            "market",
            "side",
            "size_now",
            "payments",
            "paid_usd",
            "received_usd",
            "net_usd",
            "mean_hourly_rate",
        ],
        rows.into_iter().map(|m| {
            vec![
                m.coin.clone(),
                if m.long { "Long" } else { "Short" }.to_string(),
                opt(m.size_now),
                m.payments.to_string(),
                num(m.paid),
                num(m.received),
                num(m.net),
                num(m.mean_rate),
            ]
        }),
    )
}

pub fn transfers<'a>(rows: impl IntoIterator<Item = &'a Transfer>) -> String {
    write(
        &[
            "time_utc",
            "action",
            "status",
            "from",
            "to",
            "account_value_change_usd",
            "fee_usd",
            "hash",
        ],
        rows.into_iter().map(|t| {
            vec![
                utc(t.time_ms),
                t.action.clone(),
                "Completed".to_string(),
                t.from.clone(),
                t.to.clone(),
                opt(t.change),
                opt(t.fee),
                t.hash.clone(),
            ]
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::orders::Status;
    use crate::history::trades::Role;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn times_are_utc_and_fields_with_commas_or_quotes_are_quoted() {
        assert_eq!(utc(1_795_100_000_000), "2026-11-19T14:53:20Z");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("plain"), "plain");
    }

    #[test]
    fn trades_keep_full_precision_and_one_line_per_row() {
        let row = TradeRow {
            time_ms: 1_795_100_000_000,
            coin: "xyz:ANT".into(),
            dir: "Close Short".into(),
            price: d("101.00"),
            size: d("14.85"),
            value: d("1499.8500"),
            fee: d("0.041234"),
            role: Role::Maker,
            closed_pnl: Some(d("21.24")),
            fills: 1,
            oid: 63,
            hash: "0xabc".into(),
        };
        let text = trades([&row]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("time_utc,market,direction"));
        assert_eq!(
            lines[1],
            "2026-11-19T14:53:20Z,xyz:ANT,Close Short,101,14.85,1499.85,0.041234,Maker,21.24,1,63,0xabc"
        );
        // An opening fill has an empty closed PNL, not zero.
        let open = TradeRow {
            closed_pnl: None,
            dir: "Open Short".into(),
            ..row
        };
        assert!(
            trades([&open])
                .lines()
                .nth(1)
                .unwrap()
                .contains(",Maker,,1,")
        );
    }

    #[test]
    fn orders_name_a_whole_position_and_a_market_price() {
        let o = PastOrder {
            placed_ms: 0,
            status_ms: 0,
            coin: "xyz:ANT".into(),
            direction: "Close Short",
            kind: "Take Profit Market".into(),
            size: None,
            filled: None,
            price: None,
            value: None,
            reduce_only: true,
            trigger: Some("Price below 85".into()),
            status: Status::Cancelled,
            venue_status: "reduceOnlyCanceled".into(),
            oid: 9,
        };
        let line = orders([&o]).lines().nth(1).unwrap().to_string();
        assert!(
            line.contains(",position,,market,,true,Price below 85,Cancelled,reduceOnlyCanceled,9"),
            "{line}"
        );
    }

    #[test]
    fn payments_and_markets_are_signed() {
        let p = Payment {
            time_ms: 0,
            coin: "BTC".into(),
            long: true,
            size: d("0.31266"),
            usdc: d("-0.0406"),
            rate: d("0.000002"),
            payments: 1,
        };
        assert!(payments([&p]).contains(",Long,0.31266,-0.0406,0.000002,1"));
        let m = MarketFunding {
            coin: "a,b".into(),
            long: false,
            size_now: None,
            payments: 2,
            paid: d("-1"),
            received: d("0.5"),
            net: d("-0.5"),
            mean_rate: d("0.00001"),
        };
        assert!(funding_by_market([&m]).contains("\"a,b\",Short,,2,-1,0.5,-0.5,0.00001"));
    }

    #[test]
    fn transfers_leave_an_unreported_fee_empty() {
        let t = Transfer {
            time_ms: 0,
            hash: "0x1".into(),
            action: "Deposit".into(),
            from: "Arbitrum".into(),
            to: "Perpetuals".into(),
            change: Some(d("19.8")),
            fee: None,
            bridged: true,
        };
        assert!(transfers([&t]).ends_with("Deposit,Completed,Arbitrum,Perpetuals,19.8,,0x1\n"));
    }
}
