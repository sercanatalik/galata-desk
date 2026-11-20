//! The Markets position panel: what the connected account holds, made and
//! has resting in the selected market, beside the chart.
//!
//! This module turns the account's positions, PNL, fills and orders into the
//! words the panel shows. It knows nothing of drawing, so its rules can be
//! tested on their own; `markets.rs` draws the result.

use chrono::{DateTime, Local};
use galata_desk_core::format as f;
use galata_desk_core::market::MarketId;
use galata_desk_core::market::fills::{Fill, FillSide, Fills};
use galata_desk_core::market::pnl::{MarketPnl, WindowPnl};
use galata_desk_core::model::{OrderRow, PositionRow};
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::trade::sign::Tpsl;
use galata_desk_core::venue::{MarginMode, Side};

const DASH: &str = "\u{2014}";

/// The panel's column, in pixels.
pub const WIDTH: f32 = 300.;

/// Everything the panel shows for one market. A field that is `None` has
/// not arrived yet and is drawn as a placeholder.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub holding: Holding,
    /// The PNL windows: 24 hours, 30 days, the year to date and all time.
    pub pnl: Option<Vec<PnlRow>>,
    /// The last trade: `Some(None)` when there is none among the fills held.
    pub trade: Option<Option<Trade>>,
    /// The market's open orders.
    pub orders: Option<Vec<OrderLine>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Holding {
    /// The account's positions have not arrived.
    Loading,
    Flat,
    Held(Held),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    /// `LONG · 5x cross`.
    pub side: String,
    pub long: bool,
    /// `+13.6429 ETH`.
    pub size: String,
    /// `$23,715`.
    pub value: String,
    /// `+$70.94`.
    pub pnl: String,
    /// `+1.50%`.
    pub roe: String,
    pub pnl_sign: i32,
    /// Entry, mark, liquidation, margin and funding, each with its sign
    /// where its colour should carry one.
    pub rows: Vec<(&'static str, String, i32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PnlRow {
    pub label: &'static str,
    pub realised: String,
    pub realised_sign: i32,
    pub total: String,
    pub total_sign: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Trade {
    /// `13:04`, or with its date when not today.
    pub time: String,
    pub buy: bool,
    /// `2.5`.
    pub size: String,
    pub price: String,
    /// `$2.18`.
    pub fee: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderLine {
    pub oid: u64,
    pub buy: bool,
    /// `Take profit`, `Stop loss`, or the venue's own name: `Limit`.
    pub kind: String,
    /// The remaining size, or `Position` for a level that covers it whole.
    pub size: String,
    /// The trigger price for a trigger order, the limit price otherwise.
    pub price: String,
    /// The venue's list names the market: the order can be acted on.
    pub cancellable: bool,
}

impl Model {
    /// The PNL header's figure: the 24-hour total.
    pub fn day_total(&self) -> Option<(String, i32)> {
        self.pnl
            .as_ref()
            .and_then(|rows| rows.first())
            .map(|r| (r.total.clone(), r.total_sign))
    }

    /// The open-orders header's summary: `TP · SL · 1 limit`.
    pub fn orders_summary(&self) -> String {
        self.orders.as_deref().map(summary).unwrap_or_default()
    }
}

/// Build the panel's model for `market`.
pub fn build(
    market: &MarketId,
    positions: Option<&[PositionRow]>,
    pnl: Option<Option<MarketPnl>>,
    fills: Option<&Fills>,
    orders: Option<&[OrderRow]>,
    now_ms: u64,
) -> Model {
    let holding = match positions {
        None => Holding::Loading,
        Some(all) => all
            .iter()
            .find(|p| p.dex == market.dex() && p.market == market.ticker() && !p.size.is_zero())
            .map(held)
            .map_or(Holding::Flat, Holding::Held),
    };
    Model {
        holding,
        pnl: pnl.map(|p| pnl_rows(p.unwrap_or_default())),
        trade: fills.map(|f| last_trade(&f.rows, market, now_ms)),
        orders: orders.map(|all| {
            all.iter()
                .filter(|o| o.market == market.ticker() && o.dex == market.dex())
                .map(order_line)
                .collect()
        }),
    }
}

fn sign(value: Decimal) -> i32 {
    if value > Decimal::ZERO {
        1
    } else if value < Decimal::ZERO {
        -1
    } else {
        0
    }
}

/// Whole dollars, grouped: `$23,715`.
fn usd_whole(value: Decimal) -> String {
    let text = f::usd(value.round());
    text.strip_suffix(".00").map(str::to_string).unwrap_or(text)
}

fn held(p: &PositionRow) -> Held {
    let long = p.is_long();
    let mode = match p.mode {
        MarginMode::Cross => "cross",
        MarginMode::Isolated => "isolated",
    };
    let size = f::size(p.size, &p.market);
    Held {
        side: format!(
            "{} \u{b7} {}x {mode}",
            if long { "LONG" } else { "SHORT" },
            p.leverage
        ),
        long,
        size: if long { format!("+{size}") } else { size },
        value: usd_whole(p.value.abs()),
        pnl: f::usd_signed(p.pnl),
        roe: f::pct_signed(p.roe, 2),
        pnl_sign: sign(p.pnl),
        rows: vec![
            ("Entry price", f::price(p.entry), 0),
            ("Mark price", derived(p.mark, p.entry), 0),
            (
                "Liquidation price",
                derived(p.liquidation, p.entry),
                if p.liquidation.is_some() { -1 } else { 0 },
            ),
            (
                "Margin",
                format!("{} \u{b7} {}x", usd_whole(p.margin), p.leverage),
                0,
            ),
            (
                "Funding since open",
                f::usd_signed(p.funding),
                sign(p.funding),
            ),
        ],
    }
}

/// A price the venue works out rather than quotes (mark, liquidation),
/// rounded to the entry's precision: `1,201.3`, not `1,201.3497914965`.
fn derived(value: Option<Decimal>, like: Decimal) -> String {
    value.map_or_else(|| DASH.to_string(), |v| f::price_like(v, like))
}

fn pnl_rows(pnl: MarketPnl) -> Vec<PnlRow> {
    let row = |label: &'static str, w: Option<WindowPnl>| {
        let text = |usd: Decimal, partial: bool| {
            let t = f::pnl_rounded(usd);
            if partial { format!("{t}*") } else { t }
        };
        match w {
            Some(w) => PnlRow {
                label,
                realised: text(w.realised, w.partial),
                realised_sign: sign(w.realised.round()),
                total: text(w.usd, w.partial),
                total_sign: sign(w.usd.round()),
            },
            None => PnlRow {
                label,
                realised: DASH.into(),
                realised_sign: 0,
                total: DASH.into(),
                total_sign: 0,
            },
        }
    };
    vec![
        row("24 hours", pnl.d1),
        row("30 days", pnl.d30),
        row("Year to date", pnl.ytd),
        row("All time", pnl.all),
    ]
}

fn last_trade(fills: &[Fill], market: &MarketId, now_ms: u64) -> Option<Trade> {
    let fill = fills
        .iter()
        .filter(|f| f.coin == market.coin())
        .max_by_key(|f| (f.time_ms, f.id))?;
    Some(Trade {
        time: trade_time(fill.time_ms, now_ms),
        buy: matches!(fill.side, FillSide::Bought),
        size: f::amount(fill.size),
        price: f::price(fill.price),
        fee: f::usd(fill.fee),
    })
}

/// `13:04` today, `Nov 18 13:04` before.
fn trade_time(at_ms: u64, now_ms: u64) -> String {
    let local = |ms: u64| {
        DateTime::from_timestamp_millis(ms as i64)
            .unwrap_or_default()
            .with_timezone(&Local)
    };
    let (at, now) = (local(at_ms), local(now_ms));
    if at.date_naive() == now.date_naive() {
        at.format("%H:%M").to_string()
    } else {
        at.format("%b %-d %H:%M").to_string()
    }
}

fn order_line(o: &OrderRow) -> OrderLine {
    OrderLine {
        oid: o.oid,
        buy: matches!(o.side, Side::Buy),
        kind: match o.tpsl {
            Some(Tpsl::Tp) => "Take profit".into(),
            Some(Tpsl::Sl) => "Stop loss".into(),
            None => o.kind.clone(),
        },
        size: if o.remaining.is_zero() {
            "Position".into()
        } else {
            f::amount(o.remaining)
        },
        price: f::price(o.trigger_px.unwrap_or(o.price)),
        cancellable: o.asset.is_some(),
    }
}

/// `TP · SL · 1 limit`: the take-profit, the stop-loss, and how many others.
fn summary(orders: &[OrderLine]) -> String {
    let tp = orders.iter().any(|o| o.kind == "Take profit");
    let sl = orders.iter().any(|o| o.kind == "Stop loss");
    let others: Vec<&OrderLine> = orders
        .iter()
        .filter(|o| o.kind != "Take profit" && o.kind != "Stop loss")
        .collect();
    let mut parts: Vec<String> = Vec::new();
    if tp {
        parts.push("TP".into());
    }
    if sl {
        parts.push("SL".into());
    }
    if !others.is_empty() {
        let all_limits = others.iter().all(|o| o.kind == "Limit");
        let n = others.len();
        parts.push(if all_limits {
            format!("{n} limit")
        } else {
            format!("{n} other")
        });
    }
    parts.join(" \u{b7} ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use galata_desk_core::market::pnl::WindowPnl;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn eth_long() -> PositionRow {
        PositionRow {
            asset: None,
            market: "ETH".into(),
            dex: String::new(),
            leverage: 5,
            mode: MarginMode::Cross,
            size: d("13.6429"),
            value: d("23715.42"),
            entry: d("1733.1"),
            mark: Some(d("1738.3")),
            pnl: d("70.94"),
            roe: d("0.015"),
            liquidation: Some(d("1412.6")),
            margin: d("4743.08"),
            funding: d("-18.42"),
        }
    }

    fn order(
        oid: u64,
        kind: &str,
        tpsl: Option<Tpsl>,
        side: Side,
        size: &str,
        px: &str,
    ) -> OrderRow {
        OrderRow {
            placed_ms: 0,
            market: "ETH".into(),
            dex: String::new(),
            kind: kind.into(),
            side,
            remaining: d(size),
            original: d(size),
            price: d(px),
            reduce_only: tpsl.is_some(),
            trigger: None,
            oid,
            tif: None,
            amendable: tpsl.is_none(),
            asset: None,
            trigger_px: tpsl.map(|_| d(px)),
            tpsl,
        }
    }

    fn fill(coin: &str, time_ms: u64, side: FillSide, size: &str, px: &str, fee: &str) -> Fill {
        Fill {
            coin: coin.into(),
            side,
            price: d(px),
            size: d(size),
            time_ms,
            fee: d(fee),
            id: time_ms,
            ..Fill::blank()
        }
    }

    const NOW: u64 = 1_795_003_200_000;
    const ETH: fn() -> MarketId = || MarketId::new("ETH");

    #[test]
    fn a_long_reads_as_the_spec_says() {
        let m = build(&ETH(), Some(&[eth_long()]), None, None, None, NOW);
        let Holding::Held(h) = m.holding else {
            panic!("held")
        };
        assert_eq!(h.side, "LONG \u{b7} 5x cross");
        assert_eq!(h.size, "+13.6429 ETH");
        assert_eq!(h.value, "$23,715");
        assert_eq!(h.pnl, "+$70.94");
        assert_eq!(h.roe, "+1.50%");
        let values: Vec<&str> = h.rows.iter().map(|r| r.1.as_str()).collect();
        assert_eq!(
            values,
            [
                "1,733.1",
                "1,738.3",
                "1,412.6",
                "$4,743 \u{b7} 5x",
                "-$18.42"
            ]
        );
    }

    #[test]
    fn a_short_is_signed_and_names_its_side() {
        let short = PositionRow {
            size: d("-2.5"),
            market: "BTC".into(),
            mode: MarginMode::Isolated,
            ..eth_long()
        };
        let m = build(&MarketId::new("BTC"), Some(&[short]), None, None, None, NOW);
        let Holding::Held(h) = m.holding else {
            panic!("held")
        };
        assert_eq!(h.size, "-2.5 BTC");
        assert!(h.side.starts_with("SHORT"));
        assert!(h.side.ends_with("isolated"));
    }

    #[test]
    fn worked_out_prices_take_the_entrys_precision() {
        let p = PositionRow {
            entry: d("1832.2"),
            liquidation: Some(d("1201.3497914965")),
            mark: Some(d("1748.51234")),
            ..eth_long()
        };
        let m = build(&ETH(), Some(&[p]), None, None, None, NOW);
        let Holding::Held(h) = m.holding else {
            panic!("held")
        };
        assert_eq!(h.rows[1].1, "1,748.5");
        assert_eq!(h.rows[2].1, "1,201.3");
    }

    #[test]
    fn no_liquidation_price_is_a_dash() {
        let p = PositionRow {
            liquidation: None,
            ..eth_long()
        };
        let m = build(&ETH(), Some(&[p]), None, None, None, NOW);
        let Holding::Held(h) = m.holding else {
            panic!("held")
        };
        assert_eq!(h.rows[2].1, DASH);
    }

    #[test]
    fn flat_and_loading_are_told_apart() {
        assert_eq!(
            build(&ETH(), None, None, None, None, NOW).holding,
            Holding::Loading
        );
        let btc = PositionRow {
            market: "BTC".into(),
            ..eth_long()
        };
        assert_eq!(
            build(&ETH(), Some(&[btc]), None, None, None, NOW).holding,
            Holding::Flat
        );
        let closed = PositionRow {
            size: Decimal::ZERO,
            ..eth_long()
        };
        assert_eq!(
            build(&ETH(), Some(&[closed]), None, None, None, NOW).holding,
            Holding::Flat
        );
    }

    #[test]
    fn pnl_rows_give_realised_and_total_and_mark_partial() {
        let w = |usd: &str, realised: &str, partial: bool| {
            Some(WindowPnl {
                usd: d(usd),
                realised: d(realised),
                partial,
            })
        };
        let pnl = MarketPnl {
            d1: w("-115.26", "-186.2", false),
            d30: w("-1218", "-1289", false),
            ytd: w("-1218", "-1289", true),
            all: None,
        };
        let m = build(&ETH(), Some(&[]), Some(Some(pnl)), None, None, NOW);
        let rows = m.pnl.as_ref().unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| (r.label, r.realised.as_str(), r.total.as_str()))
                .collect::<Vec<_>>(),
            [
                ("24 hours", "-$186", "-$115"),
                ("30 days", "-$1,289", "-$1,218"),
                ("Year to date", "-$1,289*", "-$1,218*"),
                ("All time", DASH, DASH),
            ]
        );
        assert_eq!(m.day_total(), Some(("-$115".into(), -1)));
    }

    #[test]
    fn pnl_waits_for_the_fills_and_a_market_never_traded_reads_dashes() {
        assert_eq!(build(&ETH(), Some(&[]), None, None, None, NOW).pnl, None);
        let m = build(&ETH(), Some(&[]), Some(None), None, None, NOW);
        assert!(m.pnl.unwrap().iter().all(|r| r.total == DASH));
    }

    #[test]
    fn the_last_trade_is_the_newest_fill_in_the_market() {
        let fills = Fills {
            rows: vec![
                fill("ETH", NOW - 3_600_000, FillSide::Sold, "1", "1700", "1"),
                fill(
                    "ETH",
                    NOW - 60_000,
                    FillSide::Bought,
                    "2.5",
                    "1741.2",
                    "2.18",
                ),
                fill("BTC", NOW, FillSide::Sold, "0.1", "60000", "3"),
            ],
            capped: false,
            walked: true,
        };
        let m = build(&ETH(), Some(&[]), None, Some(&fills), None, NOW);
        let t = m.trade.unwrap().unwrap();
        assert!(t.buy);
        assert_eq!(
            (t.size.as_str(), t.price.as_str(), t.fee.as_str()),
            ("2.5", "1,741.2", "$2.18")
        );
        assert_eq!(t.time.len(), 5, "today reads as a time: {}", t.time);
    }

    #[test]
    fn no_fill_in_the_market_is_no_recent_trade() {
        let fills = Fills {
            rows: vec![fill("BTC", NOW, FillSide::Sold, "0.1", "60000", "3")],
            capped: false,
            walked: true,
        };
        let m = build(&ETH(), Some(&[]), None, Some(&fills), None, NOW);
        assert_eq!(m.trade, Some(None));
        assert_eq!(build(&ETH(), Some(&[]), None, None, None, NOW).trade, None);
    }

    #[test]
    fn orders_are_the_markets_with_trigger_prices_and_a_summary() {
        let mut btc = order(9, "Limit", None, Side::Buy, "1", "50000");
        btc.market = "BTC".into();
        let orders = vec![
            order(
                1,
                "Take Profit Market",
                Some(Tpsl::Tp),
                Side::Sell,
                "0",
                "1900",
            ),
            order(2, "Stop Market", Some(Tpsl::Sl), Side::Sell, "0", "1690"),
            order(3, "Limit", None, Side::Buy, "2", "1720"),
            btc,
        ];
        let m = build(&ETH(), Some(&[]), None, None, Some(&orders), NOW);
        let lines = m.orders.as_ref().unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].kind, "Take profit");
        assert_eq!(lines[0].size, "Position");
        assert_eq!(lines[0].price, "1,900");
        assert!(lines[2].buy);
        assert_eq!(m.orders_summary(), "TP \u{b7} SL \u{b7} 1 limit");
    }

    #[test]
    fn the_summary_names_other_kinds_and_is_empty_with_none() {
        let orders = vec![
            order(3, "Limit", None, Side::Buy, "2", "1720"),
            order(4, "Stop Limit", None, Side::Buy, "2", "1800"),
        ];
        let m = build(&ETH(), Some(&[]), None, None, Some(&orders), NOW);
        assert_eq!(m.orders_summary(), "2 other");
        let none = build(&ETH(), Some(&[]), None, None, Some(&[]), NOW);
        assert_eq!(none.orders_summary(), "");
    }
}
