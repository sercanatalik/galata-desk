//! The History section: the account's trades, orders, funding payments, and
//! deposits and withdrawals, over a chosen period.
//!
//! Trades are the fills the Portfolio view already holds. The other three
//! come from the History feed, which loads a tab the first time it is shown
//! and refreshes it only while it is.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use chrono::{DateTime, Local};
use galata_desk_core::format as f;
use galata_desk_core::history::feed::{HistoryFeed, HistorySnapshot, Tab};
use galata_desk_core::history::funding::{self as hfunding, FundingFilter, MarketFunding, Payment};
use galata_desk_core::history::ledger::{self, Transfer};
use galata_desk_core::history::orders::{self, OrderFilter, PastOrder, Status};
use galata_desk_core::history::trades::{self, TradeFilter, TradeRow};
use galata_desk_core::history::{Period, csv};
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::venue::Http;
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Entity, SharedString, Subscription, Task, TestSupportExt,
    Window, div, px,
};

use crate::Core;
use crate::grid::{Cell, Col, Glyph, Grid, Key, RowButton, Tone};
use crate::portfolio::Portfolio;
use crate::theme::{MONO, Palette};
use crate::ui;

const DASH: &str = "\u{2014}";

/// A time as a row shows it, on this machine's clock: `Nov 19 15:14:38`.
fn when(ms: u64) -> String {
    DateTime::from_timestamp_millis(ms as i64)
        .unwrap_or_default()
        .with_timezone(&Local)
        .format("%b %-d %H:%M:%S")
        .to_string()
}

/// A day on this machine's clock: `Nov 3`.
fn day(ms: u64) -> String {
    DateTime::from_timestamp_millis(ms as i64)
        .unwrap_or_default()
        .with_timezone(&Local)
        .format("%b %-d")
        .to_string()
}

/// `xyz:NVDA` as its ticker and dex.
fn split(coin: &str) -> (&str, Option<&str>) {
    match coin.split_once(':') {
        Some((dex, ticker)) => (ticker, Some(dex)),
        None => (coin, None),
    }
}

fn market_cell(coin: &str) -> Cell {
    let (ticker, dex) = split(coin);
    let cell = Cell::text(ticker.to_string());
    match dex {
        Some(dex) => cell.and(dex.to_string(), Tone::Tag),
        None => cell,
    }
}

fn sign(d: Decimal) -> i32 {
    if d > Decimal::ZERO {
        1
    } else if d < Decimal::ZERO {
        -1
    } else {
        0
    }
}

/// A small amount to four decimals, signed: a funding payment.
fn usd4(d: Decimal) -> String {
    let body = format!("${:.4}", d.abs().round_dp(4));
    match sign(d) {
        1 => format!("+{body}"),
        -1 => format!("-{body}"),
        _ => body,
    }
}

fn signed_cell(d: Decimal) -> Cell {
    Cell::new(f::usd_signed(d), Tone::signed(sign(d)))
}

fn now_ms() -> u64 {
    galata_desk_core::history::feed::now()
}

/// The words for "in the period" in an empty table's message.
fn in_period(period: Period) -> &'static str {
    match period {
        Period::Day => "in the last 24 hours",
        Period::Week => "in the last 7 days",
        Period::Month => "in the last 30 days",
        Period::All => "yet",
    }
}

/// The rows each tab lists now, after period, filters and grouping: what is
/// drawn, totalled and exported.
#[derive(Default)]
struct Listed {
    trades: Vec<TradeRow>,
    orders: Vec<PastOrder>,
    markets: Vec<MarketFunding>,
    payments: Vec<Payment>,
    transfers: Vec<Transfer>,
    /// Rows in the period before filters, for the counts beside the tabs.
    counts: HashMap<Tab, usize>,
    /// The values each filter can take, from the rows in the period.
    coins: [BTreeSet<String>; 3],
    dirs: BTreeSet<String>,
    kinds: BTreeSet<String>,
    actions: BTreeSet<String>,
}

pub struct History {
    portfolio: Entity<Portfolio>,
    account: String,
    feed: HistoryFeed,
    snapshot: HistorySnapshot,
    shown: bool,
    tab: Tab,
    period: Period,
    grouped: bool,
    trade_filter: TradeFilter,
    order_filter: OrderFilter,
    funding_filter: FundingFilter,
    every_payment: bool,
    action: Option<String>,
    trades: Entity<TableState<Grid>>,
    orders: Entity<TableState<Grid>>,
    by_market: Entity<TableState<Grid>>,
    payments: Entity<TableState<Grid>>,
    transfers: Entity<TableState<Grid>>,
    listed: Listed,
    /// What the last export or copy did: the words, and whether it failed.
    note: Option<(SharedString, bool)>,
    /// The fills last drawn: how many, and the newest's id.
    fills_seen: Option<(usize, u64, bool)>,
    /// For a screenshot: show the orders as if their refresh had failed.
    #[cfg(feature = "shot")]
    stale_shot: bool,
    _watch: Task<()>,
    _portfolio: Subscription,
}

fn table(grid: Grid, window: &mut Window, cx: &mut Context<History>) -> Entity<TableState<Grid>> {
    cx.new(|cx| {
        TableState::new(grid, window, cx)
            .col_movable(false)
            .row_selectable(false)
            .col_selectable(false)
    })
}

impl History {
    pub fn new(
        portfolio: Entity<Portfolio>,
        account: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let feed = {
            let core = Core::get(cx);
            let _io = core.runtime.enter();
            HistoryFeed::start(Http::at(core.info_url()), account.as_str())
        };
        #[allow(unused_mut)]
        let mut snapshots = feed.snapshots();
        // As in Portfolio: the headless tests give the view its snapshots.
        #[cfg(test)]
        let watch = {
            drop(snapshots);
            Task::ready(())
        };
        #[cfg(not(test))]
        let watch = cx.spawn(async move |this, cx| {
            while snapshots.changed().await.is_ok() {
                let snapshot = snapshots.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| this.set_snapshot(snapshot, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        // New fills, and the positions' sizes, come from the portfolio.
        let observe = cx.observe(&portfolio, |this, _, cx| {
            let fills = this.portfolio.read(cx).fills().map(|f| {
                (
                    f.rows.len(),
                    f.rows.iter().map(|r| r.id).max().unwrap_or_default(),
                    f.capped,
                )
            });
            if fills != this.fills_seen {
                this.fills_seen = fills;
                this.rebuild(cx);
            }
        });

        let trades = Grid::new(
            vec![
                Col::text("Time", 140.).sortable(),
                Col::text("Market", 150.).sortable(),
                Col::text("Direction", 130.).sortable(),
                Col::number("Price", 110.).sortable(),
                Col::number("Size", 190.).sortable(),
                Col::number("Value", 120.).sortable(),
                Col::number("Fee", 90.).sortable(),
                Col::text("Role", 70.),
                Col::number("Closed PNL", 120.).sortable(),
            ],
            "No trades.",
        );
        let orders = Grid::new(
            vec![
                Col::text("Placed", 140.).sortable(),
                Col::text("Market", 140.).sortable(),
                Col::text("Direction", 100.).sortable(),
                Col::text("Type", 150.).sortable(),
                Col::number("Size", 160.).sortable(),
                Col::number("Filled", 110.).sortable(),
                Col::number("Price", 100.).sortable(),
                Col::number("Value", 110.).sortable(),
                Col::text("Reduce only", 90.),
                Col::text("Trigger", 150.),
                Col::text("Status", 100.).sortable(),
                Col::number("Order ID", 130.),
            ],
            "No orders.",
        );
        let by_market = Grid::new(
            vec![
                Col::text("Market", 160.).sortable(),
                Col::text("Side", 70.),
                Col::number("Size now", 170.).sortable(),
                Col::number("Payments", 90.).sortable(),
                Col::number("Paid", 110.).sortable(),
                Col::number("Received", 110.).sortable(),
                Col::number("Net", 110.).sortable(),
                Col::number("Average rate, 1h", 130.).sortable(),
            ],
            "No funding payments.",
        );
        let payments = Grid::new(
            vec![
                Col::text("Time", 200.).sortable(),
                Col::text("Market", 160.).sortable(),
                Col::text("Side", 70.),
                Col::number("Size", 170.).sortable(),
                Col::number("Payment", 120.).sortable(),
                Col::number("Rate, 1h", 110.).sortable(),
            ],
            "No funding payments.",
        );
        let transfers = Grid::new(
            vec![
                Col::text("Time", 140.).sortable(),
                Col::text("Action", 160.).sortable(),
                Col::text("Status", 90.),
                Col::text("From", 140.),
                Col::text("To", 140.),
                Col::number("Account value change", 160.).sortable(),
                Col::number("Fee", 90.).sortable(),
                Col::number("Transaction", 170.),
            ],
            "No deposits or withdrawals.",
        )
        .on_press({
            let this = cx.weak_entity();
            std::rc::Rc::new(move |id, key, _, cx| {
                let _ = this.update(cx, |this, cx| this.press(id, key, cx));
            })
        });

        #[allow(unused_mut)]
        let mut this = History {
            portfolio,
            account,
            feed,
            snapshot: HistorySnapshot::default(),
            shown: false,
            tab: Tab::Trades,
            period: Period::default(),
            grouped: true,
            trade_filter: TradeFilter::default(),
            order_filter: OrderFilter::default(),
            funding_filter: FundingFilter::default(),
            every_payment: false,
            action: None,
            trades: table(trades, window, cx),
            orders: table(orders, window, cx),
            by_market: table(by_market, window, cx),
            payments: table(payments, window, cx),
            transfers: table(transfers, window, cx),
            listed: Listed::default(),
            note: None,
            fills_seen: None,
            #[cfg(feature = "shot")]
            stale_shot: false,
            _watch: watch,
            _portfolio: observe,
        };
        #[cfg(feature = "shot")]
        this.preselect();
        this.rebuild(cx);
        this
    }

    /// The trades list as History draws it, for a blotter to mirror.
    pub fn trades_grid<'a>(&self, cx: &'a gpui_kit::App) -> &'a Grid {
        self.trades.read(cx).delegate()
    }

    /// The section is shown, or hidden: the feed loads and refreshes only
    /// while it is.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.shown = shown;
        self.tell_feed();
        if shown {
            self.rebuild(cx);
        }
    }

    fn tell_feed(&self) {
        let since = self.period.since_ms(now_ms()).unwrap_or(0);
        self.feed.show(self.shown.then_some(self.tab), since);
    }

    pub fn set_snapshot(&mut self, snapshot: HistorySnapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        // A screenshot of a tab whose refresh failed.
        #[cfg(feature = "shot")]
        if self.stale_shot && self.snapshot.orders.data.is_some() {
            self.snapshot.orders.error = Some(galata_desk_core::venue::VenueError::Status(503));
        }
        self.rebuild(cx);
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.note = None;
        self.tell_feed();
        cx.notify();
    }

    pub fn set_period(&mut self, period: Period, cx: &mut Context<Self>) {
        self.period = period;
        self.tell_feed();
        self.rebuild(cx);
    }

    pub fn set_grouped(&mut self, grouped: bool, cx: &mut Context<Self>) {
        self.grouped = grouped;
        self.rebuild(cx);
    }

    fn held_sizes(&self, cx: &Context<Self>) -> HashMap<String, Decimal> {
        self.portfolio
            .read(cx)
            .positions()
            .unwrap_or_default()
            .iter()
            .map(|p| {
                let coin = if p.dex.is_empty() {
                    p.market.clone()
                } else {
                    format!("{}:{}", p.dex, p.market)
                };
                (coin, p.size)
            })
            .collect()
    }

    /// Work out every tab's rows from what is held, and give them to the
    /// tables.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        let period = self.period;
        let mut listed = Listed::default();

        // Trades.
        if let Some(fills) = self.portfolio.read(cx).fills() {
            let all = trades::rows(&fills.rows, self.grouped);
            let in_period: Vec<&TradeRow> = period.keep(&all, now, |r| r.time_ms).collect();
            listed.counts.insert(Tab::Trades, in_period.len());
            for r in &in_period {
                listed.coins[0].insert(r.coin.clone());
                listed.dirs.insert(r.dir.clone());
            }
            listed.trades = in_period
                .into_iter()
                .filter(|r| self.trade_filter.admits(r))
                .cloned()
                .collect();
        }
        // Orders.
        if let Some(held) = &self.snapshot.orders.data {
            let all = orders::collapse(&held.rows);
            let in_period: Vec<&PastOrder> = period.keep(&all, now, |o| o.placed_ms).collect();
            listed.counts.insert(Tab::Orders, in_period.len());
            for o in &in_period {
                listed.coins[1].insert(o.coin.clone());
                listed.kinds.insert(o.kind.clone());
            }
            listed.orders = in_period
                .into_iter()
                .filter(|o| self.order_filter.admits(o))
                .cloned()
                .collect();
        }
        // Funding payments.
        if let Some(held) = &self.snapshot.funding.data {
            let all = hfunding::payments(&held.rows);
            let in_period: Vec<&Payment> = period.keep(&all, now, |p| p.time_ms).collect();
            listed.counts.insert(
                Tab::Funding,
                in_period.iter().map(|p| p.payments as usize).sum(),
            );
            for p in &in_period {
                listed.coins[2].insert(p.coin.clone());
            }
            listed.payments = in_period
                .into_iter()
                .filter(|p| self.funding_filter.admits(p))
                .cloned()
                .collect();
            listed.markets = hfunding::by_market(&listed.payments, &self.held_sizes(cx));
        }
        // Deposits and withdrawals.
        if let Some(held) = &self.snapshot.ledger.data {
            let all = ledger::transfers(&held.rows, &self.account);
            let in_period: Vec<&Transfer> = period.keep(&all, now, |t| t.time_ms).collect();
            listed.counts.insert(Tab::Transfers, in_period.len());
            for t in &in_period {
                listed.actions.insert(t.action.clone());
            }
            listed.transfers = in_period
                .into_iter()
                .filter(|t| self.action.as_ref().is_none_or(|a| *a == t.action))
                .cloned()
                .collect();
        }

        let words = in_period(period);
        let trade_rows: Vec<(Vec<Cell>, Vec<Key>)> = listed.trades.iter().map(trade_row).collect();
        let order_rows: Vec<(Vec<Cell>, Vec<Key>)> = listed.orders.iter().map(order_row).collect();
        let market_rows: Vec<(Vec<Cell>, Vec<Key>)> =
            listed.markets.iter().map(market_row).collect();
        let payment_rows: Vec<(Vec<Cell>, Vec<Key>)> =
            listed.payments.iter().map(payment_row).collect();
        let transfer_rows: Vec<(Vec<Cell>, Vec<Key>)> = listed
            .transfers
            .iter()
            .enumerate()
            .map(|(i, t)| transfer_row(i, t))
            .collect();
        let fill = |state: &Entity<TableState<Grid>>,
                    rows: Vec<(Vec<Cell>, Vec<Key>)>,
                    empty: String,
                    cx: &mut Context<Self>| {
            let (cells, keys): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
            state.update(cx, |t, cx| {
                t.delegate_mut().set_empty(empty);
                t.delegate_mut().set_keyed_rows(cells, keys);
                t.refresh(cx);
            });
        };
        fill(&self.trades, trade_rows, format!("No trades {words}."), cx);
        fill(&self.orders, order_rows, format!("No orders {words}."), cx);
        fill(
            &self.by_market,
            market_rows,
            format!("No funding payments {words}."),
            cx,
        );
        fill(
            &self.payments,
            payment_rows,
            format!("No funding payments {words}."),
            cx,
        );
        fill(
            &self.transfers,
            transfer_rows,
            format!("No deposits or withdrawals {words}."),
            cx,
        );
        self.listed = listed;
        cx.notify();
    }

    /// A row button was pressed: copy a transaction's hash.
    fn press(&mut self, id: &'static str, key: u64, cx: &mut Context<Self>) {
        if id != "copy-hash" {
            return;
        }
        if let Some(t) = self.listed.transfers.get(key as usize) {
            cx.write_to_clipboard(ClipboardItem::new_string(t.hash.clone()));
            self.note = Some(("Copied the transaction hash.".into(), false));
            cx.notify();
        }
    }

    /// The listed rows of the shown tab as CSV, and a name for the file.
    pub fn csv(&self) -> (String, String) {
        let date = Local::now().format("%Y-%m-%d");
        match self.tab {
            Tab::Trades => (
                csv::trades(&self.listed.trades),
                format!("galata-trades-{date}.csv"),
            ),
            Tab::Orders => (
                csv::orders(&self.listed.orders),
                format!("galata-orders-{date}.csv"),
            ),
            Tab::Funding if self.every_payment => (
                csv::payments(&self.listed.payments),
                format!("galata-funding-payments-{date}.csv"),
            ),
            Tab::Funding => (
                csv::funding_by_market(&self.listed.markets),
                format!("galata-funding-by-market-{date}.csv"),
            ),
            Tab::Transfers => (
                csv::transfers(&self.listed.transfers),
                format!("galata-deposits-withdrawals-{date}.csv"),
            ),
        }
    }

    /// Write the shown tab's rows to `path`, and say how it went.
    pub fn export_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let (text, _) = self.csv();
        let lines = text.lines().count().saturating_sub(1);
        self.note = Some(match std::fs::write(&path, text) {
            Ok(()) => (
                format!(
                    "Exported {lines} rows to {}.",
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("the file")
                )
                .into(),
                false,
            ),
            Err(e) => (format!("The file could not be written: {e}").into(), true),
        });
        cx.notify();
    }

    /// Ask where to save the shown tab, then save it there.
    fn export(&mut self, cx: &mut Context<Self>) {
        let (_, name) = self.csv();
        let folder = dirs_downloads();
        let chosen = cx.prompt_for_new_path(&folder, Some(&name));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = chosen.await {
                let _ = this.update(cx, |this, cx| this.export_to(path, cx));
            }
        })
        .detach();
    }
}

/// The Downloads folder, else the home folder.
fn dirs_downloads() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

fn trade_row(r: &TradeRow) -> (Vec<Cell>, Vec<Key>) {
    let (ticker, _) = split(&r.coin);
    let mut size = Cell::text(f::size(r.size, ticker));
    if r.fills > 1 {
        size = size.and(format!("{} fills", r.fills), Tone::Tag);
    }
    let dir = Cell::new(
        r.dir.clone(),
        if r.buys() { Tone::Gain } else { Tone::Loss },
    );
    let pnl = match r.closed_pnl {
        Some(p) => signed_cell(p),
        None => Cell::muted(DASH),
    };
    (
        vec![
            Cell::muted(when(r.time_ms)),
            market_cell(&r.coin),
            dir,
            Cell::text(f::price(r.price)),
            size,
            Cell::text(f::usd(r.value)),
            Cell::muted(f::usd(r.fee)),
            Cell::muted(r.role.label()),
            pnl,
        ],
        vec![
            Key::Num(r.time_ms.into()),
            Key::Text(r.coin.clone()),
            Key::Text(r.dir.clone()),
            Key::Num(r.price),
            Key::Num(r.size),
            Key::Num(r.value),
            Key::Num(r.fee),
            Key::Missing,
            Key::num(r.closed_pnl),
        ],
    )
}

fn order_row(o: &PastOrder) -> (Vec<Cell>, Vec<Key>) {
    let (ticker, _) = split(&o.coin);
    let status_tone = match o.status {
        Status::Rejected => Tone::Loss,
        Status::Open => Tone::Text,
        Status::Filled | Status::Triggered => Tone::Text,
        Status::Cancelled | Status::Other => Tone::Muted,
    };
    let status_word = if o.status == Status::Other {
        galata_desk_core::history::words(&o.venue_status)
    } else {
        o.status.label().to_string()
    };
    let mut status = Cell::new(status_word, status_tone);
    if let Some(reason) = orders::reason(&o.venue_status) {
        status = status.tip(reason);
    }
    (
        vec![
            Cell::muted(when(o.placed_ms)),
            market_cell(&o.coin),
            Cell::new(o.direction, if o.buys() { Tone::Gain } else { Tone::Loss }),
            Cell::text(o.kind.clone()),
            Cell::text(
                o.size
                    .map(|s| f::size(s, ticker))
                    .unwrap_or_else(|| "Position".into()),
            ),
            Cell::muted(o.filled.map(f::amount).unwrap_or_else(|| DASH.into())),
            Cell::text(o.price.map(f::price).unwrap_or_else(|| "Market".into())),
            Cell::text(o.value.map(f::usd).unwrap_or_else(|| DASH.into())),
            Cell::muted(if o.reduce_only { "Yes" } else { "No" }),
            Cell::muted(o.trigger.clone().unwrap_or_else(|| DASH.into())),
            status,
            Cell::muted(o.oid.to_string()),
        ],
        vec![
            Key::Num(o.placed_ms.into()),
            Key::Text(o.coin.clone()),
            Key::Text(o.direction.to_string()),
            Key::Text(o.kind.clone()),
            Key::num(o.size),
            Key::num(o.filled),
            Key::num(o.price),
            Key::num(o.value),
            Key::Missing,
            Key::Missing,
            Key::Text(o.status.label().to_string()),
            Key::Missing,
        ],
    )
}

fn side_cell(long: bool) -> Cell {
    if long {
        Cell::new("Long", Tone::Gain)
    } else {
        Cell::new("Short", Tone::Loss)
    }
}

fn market_row(m: &MarketFunding) -> (Vec<Cell>, Vec<Key>) {
    let (ticker, _) = split(&m.coin);
    let money = |d: Decimal| {
        if d.is_zero() {
            Cell::muted(DASH)
        } else {
            signed_cell(d)
        }
    };
    (
        vec![
            market_cell(&m.coin),
            side_cell(m.long),
            Cell::text(
                m.size_now
                    .map(|s| f::size(s, ticker))
                    .unwrap_or_else(|| DASH.into()),
            ),
            Cell::muted(m.payments.to_string()),
            money(m.paid),
            money(m.received),
            signed_cell(m.net),
            Cell::muted(f::funding_hourly(m.mean_rate)),
        ],
        vec![
            Key::Text(m.coin.clone()),
            Key::Missing,
            Key::num(m.size_now),
            Key::Num(m.payments.into()),
            Key::Num(m.paid),
            Key::Num(m.received),
            Key::Num(m.net),
            Key::Num(m.mean_rate),
        ],
    )
}

fn payment_row(p: &Payment) -> (Vec<Cell>, Vec<Key>) {
    let (ticker, _) = split(&p.coin);
    // An older day the venue summed: the day, and how many it holds.
    let time = if p.payments > 1 {
        Cell::muted(day(p.time_ms)).and(format!("{} payments", p.payments), Tone::Tag)
    } else {
        Cell::muted(when(p.time_ms))
    };
    (
        vec![
            time,
            market_cell(&p.coin),
            side_cell(p.long),
            Cell::text(f::size(p.size, ticker)),
            Cell::new(usd4(p.usdc), Tone::signed(sign(p.usdc))),
            Cell::muted(f::funding_hourly(p.rate)),
        ],
        vec![
            Key::Num(p.time_ms.into()),
            Key::Text(p.coin.clone()),
            Key::Missing,
            Key::Num(p.size),
            Key::Num(p.usdc),
            Key::Num(p.rate),
        ],
    )
}

fn transfer_row(ix: usize, t: &Transfer) -> (Vec<Cell>, Vec<Key>) {
    let change = match t.change {
        Some(c) => signed_cell(c),
        None => Cell::muted(DASH),
    };
    let hash = Cell::muted(f::short_address(&t.hash)).with_buttons(vec![RowButton {
        id: "copy-hash",
        key: ix as u64,
        glyph: Glyph::Copy,
        name: "Copy transaction hash".into(),
        disabled: false,
        busy: false,
    }]);
    (
        vec![
            Cell::muted(when(t.time_ms)),
            Cell::text(t.action.clone()),
            Cell::muted("Completed"),
            Cell::text(t.from.clone()),
            Cell::text(t.to.clone()),
            change,
            Cell::muted(t.fee.map(f::usd).unwrap_or_else(|| DASH.into())),
            hash,
        ],
        vec![
            Key::Num(t.time_ms.into()),
            Key::Text(t.action.clone()),
            Key::Missing,
            Key::Missing,
            Key::Missing,
            Key::num(t.change),
            Key::num(t.fee),
            Key::Missing,
        ],
    )
}

/// One figure in a tab's line of totals.
fn total(label: &'static str, value: impl Into<SharedString>, tone: Tone) -> AnyElement {
    let color = match tone {
        Tone::Gain => Palette::gain(),
        Tone::Loss => Palette::loss(),
        _ => Palette::text(),
    };
    h_flex()
        .gap_1()
        .child(ui::label(label))
        .child(
            div()
                .font_family(MONO)
                .text_size(px(12.5))
                .text_color(color)
                .child(value.into()),
        )
        .into_any_element()
}

/// A filter menu: `All` and each value, the chosen one checked.
fn menu(
    id: &'static str,
    name: &'static str,
    chosen: Option<String>,
    values: Vec<(String, String)>,
    on_pick: impl Fn(&mut History, Option<String>, &mut Context<History>) + Clone + 'static,
    cx: &mut Context<History>,
) -> AnyElement {
    let view = cx.entity().downgrade();
    let shown = chosen
        .as_ref()
        .and_then(|c| values.iter().find(|(v, _)| v == c).map(|(_, w)| w.clone()))
        .unwrap_or_else(|| "All".into());
    Button::new(id)
        .small()
        .outline()
        .label(format!("{name}: {shown} \u{25be}"))
        .dropdown_menu(move |mut m, _, _| {
            let all = std::iter::once((None, "All".to_string()))
                .chain(values.iter().map(|(v, w)| (Some(v.clone()), w.clone())));
            for (value, words) in all {
                let view = view.clone();
                let on_pick = on_pick.clone();
                let checked = value == chosen;
                m = m.item(
                    PopupMenuItem::new(words)
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| on_pick(this, value.clone(), cx));
                        }),
                );
            }
            m
        })
        .into_any_element()
}

fn coin_values(coins: &BTreeSet<String>) -> Vec<(String, String)> {
    coins
        .iter()
        .map(|c| {
            let (ticker, dex) = split(c);
            let words = match dex {
                Some(dex) => format!("{ticker} ({dex})"),
                None => ticker.to_string(),
            };
            (c.clone(), words)
        })
        .collect()
}

impl History {
    /// The shown tab's filters and switches.
    fn controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut row = h_flex().gap_2().flex_wrap();
        match self.tab {
            Tab::Trades => {
                row = row
                    .child(menu(
                        "h-market",
                        "Market",
                        self.trade_filter.coin.clone(),
                        coin_values(&self.listed.coins[0]),
                        |this, v, cx| {
                            this.trade_filter.coin = v;
                            this.rebuild(cx);
                        },
                        cx,
                    ))
                    .child(menu(
                        "h-dir",
                        "Direction",
                        self.trade_filter.dir.clone(),
                        self.listed
                            .dirs
                            .iter()
                            .map(|d| (d.clone(), d.clone()))
                            .collect(),
                        |this, v, cx| {
                            this.trade_filter.dir = v;
                            this.rebuild(cx);
                        },
                        cx,
                    ))
                    .child(
                        Checkbox::new("h-grouped")
                            .label("Group fills by order")
                            .checked(self.grouped)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_grouped(*checked, cx)
                            })),
                    );
            }
            Tab::Orders => {
                let chosen = self.order_filter.status;
                let statuses = TabBar::new("h-status")
                    .segmented()
                    .xsmall()
                    .selected_index(match chosen {
                        None => 0,
                        Some(s) => 1 + Status::LISTED.iter().position(|x| *x == s).unwrap_or(0),
                    })
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.order_filter.status = ix.checked_sub(1).map(|i| Status::LISTED[i]);
                        this.rebuild(cx);
                    }))
                    .children(
                        std::iter::once("All").chain(Status::LISTED.iter().map(|s| s.label())),
                    );
                row = row
                    .child(statuses)
                    .child(menu(
                        "h-omarket",
                        "Market",
                        self.order_filter.coin.clone(),
                        coin_values(&self.listed.coins[1]),
                        |this, v, cx| {
                            this.order_filter.coin = v;
                            this.rebuild(cx);
                        },
                        cx,
                    ))
                    .child(menu(
                        "h-kind",
                        "Type",
                        self.order_filter.kind.clone(),
                        self.listed
                            .kinds
                            .iter()
                            .map(|k| (k.clone(), k.clone()))
                            .collect(),
                        |this, v, cx| {
                            this.order_filter.kind = v;
                            this.rebuild(cx);
                        },
                        cx,
                    ));
            }
            Tab::Funding => {
                let view = TabBar::new("h-fview")
                    .segmented()
                    .xsmall()
                    .selected_index(usize::from(self.every_payment))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.every_payment = *ix == 1;
                        this.note = None;
                        cx.notify();
                    }))
                    .children(["By market", "Every payment"]);
                let side = match self.funding_filter.long {
                    None => None,
                    Some(true) => Some("long".to_string()),
                    Some(false) => Some("short".to_string()),
                };
                row = row
                    .child(view)
                    .child(menu(
                        "h-fmarket",
                        "Market",
                        self.funding_filter.coin.clone(),
                        coin_values(&self.listed.coins[2]),
                        |this, v, cx| {
                            this.funding_filter.coin = v;
                            this.rebuild(cx);
                        },
                        cx,
                    ))
                    .child(menu(
                        "h-side",
                        "Side",
                        side,
                        vec![
                            ("long".into(), "Long".into()),
                            ("short".into(), "Short".into()),
                        ],
                        |this, v, cx| {
                            this.funding_filter.long = v.map(|s| s == "long");
                            this.rebuild(cx);
                        },
                        cx,
                    ));
            }
            Tab::Transfers => {
                row = row.child(menu(
                    "h-action",
                    "Action",
                    self.action.clone(),
                    self.listed
                        .actions
                        .iter()
                        .map(|a| (a.clone(), a.clone()))
                        .collect(),
                    |this, v, cx| {
                        this.action = v;
                        this.rebuild(cx);
                    },
                    cx,
                ));
            }
        }
        row.into_any_element()
    }

    /// The shown tab's line of totals.
    fn totals(&self) -> AnyElement {
        let row = h_flex().gap_6().flex_wrap().px_3().py_2();
        let row = match self.tab {
            Tab::Trades => {
                let t = trades::totals(&self.listed.trades);
                let count = if self.grouped {
                    format!("{} orders, {} fills", t.rows, t.fills)
                } else {
                    format!("{} fills", t.fills)
                };
                row.child(ui::label(count))
                    .child(total("Volume", f::usd(t.volume), Tone::Text))
                    .child(total("Fees", f::usd(t.fees), Tone::Text))
                    .child(total(
                        "Closed PNL",
                        f::usd_signed(t.closed_pnl),
                        Tone::signed(sign(t.closed_pnl)),
                    ))
            }
            Tab::Orders => {
                let (all, by) = orders::counts(&self.listed.orders);
                let mut row = row.child(ui::label(format!("{all} orders")));
                for status in Status::LISTED {
                    let n = by.get(&status).copied().unwrap_or_default();
                    row = row.child(ui::label(format!("{n} {}", status.label().to_lowercase())));
                }
                row
            }
            Tab::Funding => {
                let t = hfunding::totals(&self.listed.payments);
                row.child(total(
                    "Net funding",
                    f::usd_signed(t.net),
                    Tone::signed(sign(t.net)),
                ))
                .child(total(
                    "Paid",
                    f::usd_signed(t.paid),
                    Tone::signed(sign(t.paid)),
                ))
                .child(total(
                    "Received",
                    f::usd_signed(t.received),
                    Tone::signed(sign(t.received)),
                ))
                .child(ui::label(format!(
                    "{} payments across {} markets",
                    t.payments, t.markets
                )))
            }
            Tab::Transfers => {
                let t = ledger::totals(&self.listed.transfers);
                row.child(total("Deposited", f::usd(t.deposited), Tone::Text))
                    .child(total("Withdrawn", f::usd(t.withdrawn), Tone::Text))
                    .child(total("Fees", f::usd(t.fees), Tone::Text))
            }
        };
        row.border_b_1()
            .border_color(Palette::line())
            .into_any_element()
    }

    /// How far back the shown tab reaches, when not as far as the period.
    fn reach_note(&self, cx: &Context<Self>) -> Option<String> {
        let since = self.period.since_ms(now_ms()).unwrap_or(0);
        match self.tab {
            Tab::Trades => {
                let fills = self.portfolio.read(cx).fills()?;
                if !fills.walked {
                    return Some("* Older trades are still loading.".into());
                }
                if !fills.capped {
                    return None;
                }
                let earliest = fills.rows.iter().map(|r| r.time_ms).min()?;
                (earliest > since).then(|| {
                    format!(
                        "* Trades before {} are not shown: the venue keeps an account's latest 10,000 fills.",
                        day(earliest)
                    )
                })
            }
            Tab::Orders => {
                let from = self.snapshot.orders.data.as_ref()?.shown_from?;
                (from > since).then(|| {
                    format!(
                        "* Orders before {} are not shown: the venue keeps an account's latest 2,000 order updates.",
                        when(from)
                    )
                })
            }
            Tab::Funding => {
                let at = self.snapshot.funding.data.as_ref()?.stopped_at?;
                Some(format!("* Payments after {} are still loading.", when(at)))
            }
            Tab::Transfers => {
                let at = self.snapshot.ledger.data.as_ref()?.stopped_at?;
                Some(format!("* Movements after {} are still loading.", when(at)))
            }
        }
    }

    /// Whether the shown tab has rows to draw, and the failure to show
    /// beside or instead of them.
    fn state(&self, cx: &Context<Self>) -> (bool, Option<String>, Option<std::time::SystemTime>) {
        let section = |loaded: bool, error: Option<&galata_desk_core::venue::VenueError>, at| {
            (loaded, error.map(ui::explain), at)
        };
        match self.tab {
            Tab::Trades => (self.portfolio.read(cx).fills().is_some(), None, None),
            Tab::Orders => {
                let s = &self.snapshot.orders;
                section(s.data.is_some(), s.error.as_ref(), s.fetched_at)
            }
            Tab::Funding => {
                let s = &self.snapshot.funding;
                section(s.data.is_some(), s.error.as_ref(), s.fetched_at)
            }
            Tab::Transfers => {
                let s = &self.snapshot.ledger;
                section(s.data.is_some(), s.error.as_ref(), s.fetched_at)
            }
        }
    }
}

impl Render for History {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = |tab: Tab| match self.listed.counts.get(&tab) {
            Some(n) => format!("{} {n}", tab.label()),
            None => tab.label().to_string(),
        };
        let tabs = TabBar::new("history-tabs")
            .underline()
            .selected_index(Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0))
            .on_click(cx.listener(|this, ix: &usize, _, cx| this.set_tab(Tab::ALL[*ix], cx)))
            .children(Tab::ALL.map(count));
        let period = TabBar::new("history-period")
            .segmented()
            .small()
            .selected_index(
                Period::ALL
                    .iter()
                    .position(|p| *p == self.period)
                    .unwrap_or(2),
            )
            .on_click(cx.listener(|this, ix: &usize, _, cx| this.set_period(Period::ALL[*ix], cx)))
            .children(Period::ALL.map(Period::label));
        let export = Button::new("history-export")
            .small()
            .outline()
            .icon(ui::download_icon())
            .tooltip("Export CSV")
            .on_click(cx.listener(|this, _, _, cx| this.export(cx)));
        let heading = h_flex()
            .gap_4()
            .flex_wrap()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("History"),
            )
            .child(ui::label(
                "What the account traded, ordered, paid in funding and moved in and out",
            ))
            .child(div().flex_1())
            .child(period)
            .child(export);

        let (loaded, failure, at) = self.state(cx);
        let state = match self.tab {
            Tab::Trades => &self.trades,
            Tab::Orders => &self.orders,
            Tab::Funding if self.every_payment => &self.payments,
            Tab::Funding => &self.by_market,
            Tab::Transfers => &self.transfers,
        };
        let body: AnyElement = if loaded {
            DataTable::new(state)
                .stripe(false)
                .bordered(false)
                .into_any_element()
        } else if let Some(failure) = &failure {
            v_flex()
                .id("history-failed")
                .p_4()
                .gap_2()
                .child(
                    div()
                        .text_color(Palette::amber_text())
                        .child(format!("{} could not be loaded", self.tab.label())),
                )
                .child(ui::label(format!("{failure} Trying again in a minute.")))
                .test_support()
                .into_any_element()
        } else {
            v_flex()
                .id("history-loading")
                .p_4()
                .gap_3()
                .children((0..6).map(|_| ui::placeholder(px(9999.), px(18.))))
                .test_support()
                .into_any_element()
        };
        // Rows on screen and a refresh that failed: say they are not current.
        let stale = (loaded && failure.is_some()).then(|| {
            let since = at
                .map(|t| DateTime::<Local>::from(t).format("%H:%M:%S").to_string())
                .unwrap_or_default();
            div()
                .id("history-stale")
                .px_3()
                .pt_2()
                .child(ui::notice(format!(
                    "Not current: last updated at {since}. {}",
                    failure.clone().unwrap_or_default()
                )))
                .test_support()
        });
        let note = self.reach_note(cx);
        let report = self.note.clone().map(|(text, failed)| {
            div()
                .id("history-note")
                .text_color(if failed {
                    Palette::loss()
                } else {
                    Palette::muted()
                })
                .child(text)
                .test_support()
        });

        v_flex().size_full().p_3().gap_3().child(heading).child(
            ui::panel()
                .flex_1()
                .min_h_0()
                .child(
                    h_flex()
                        .px_2()
                        .pt_1()
                        .gap_4()
                        .flex_wrap()
                        .flex_shrink_0()
                        .border_b_1()
                        .border_color(Palette::line())
                        .child(div().flex_1().min_w_0().child(tabs))
                        .child(div().py_1().child(self.controls(cx))),
                )
                .when(loaded, |d| d.child(self.totals()))
                .children(stale)
                .child(div().flex_1().min_h_0().child(body))
                .child(
                    h_flex()
                        .px_3()
                        .py_2()
                        .gap_4()
                        .flex_shrink_0()
                        .border_t_1()
                        .border_color(Palette::line())
                        .text_size(px(12.))
                        .children(note.map(|n| {
                            div()
                                .id("history-reach")
                                .text_color(Palette::amber_text())
                                .child(n)
                                .test_support()
                        }))
                        .child(div().flex_1())
                        .children(report),
                ),
        )
    }
}

#[cfg(feature = "shot")]
impl History {
    /// `GALATA_DESK_SHOT_STATE=section=history,htab=orders|funding|transfers,hgroup=0,hview=every,hperiod=all,hstale=1`
    fn preselect(&mut self) {
        let state = std::env::var("GALATA_DESK_SHOT_STATE").unwrap_or_default();
        for (key, value) in state.split(',').filter_map(|kv| kv.split_once('=')) {
            match key {
                "htab" => {
                    self.tab = match value {
                        "orders" => Tab::Orders,
                        "funding" => Tab::Funding,
                        "transfers" => Tab::Transfers,
                        _ => Tab::Trades,
                    }
                }
                "hgroup" => self.grouped = value != "0",
                "hstale" => self.stale_shot = value == "1",
                "hview" => self.every_payment = value == "every",
                "hperiod" => {
                    self.period = match value {
                        "day" => Period::Day,
                        "week" => Period::Week,
                        "all" => Period::All,
                        _ => Period::Month,
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
impl History {
    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn period(&self) -> Period {
        self.period
    }

    pub fn order_filter_mut(&mut self) -> &mut OrderFilter {
        &mut self.order_filter
    }

    pub fn trade_filter_mut(&mut self) -> &mut TradeFilter {
        &mut self.trade_filter
    }

    pub fn set_every_payment(&mut self, every: bool, cx: &mut Context<Self>) {
        self.every_payment = every;
        cx.notify();
    }

    pub fn refilter(&mut self, cx: &mut Context<Self>) {
        self.rebuild(cx);
    }

    /// How many rows each tab holds in the period, as its tab label says.
    pub fn count(&self, tab: Tab) -> Option<usize> {
        self.listed.counts.get(&tab).copied()
    }

    /// The shown table's cell texts in one column, top to bottom.
    pub fn column(&self, col: usize, cx: &gpui_kit::App) -> Vec<String> {
        let state = match self.tab {
            Tab::Trades => &self.trades,
            Tab::Orders => &self.orders,
            Tab::Funding if self.every_payment => &self.payments,
            Tab::Funding => &self.by_market,
            Tab::Transfers => &self.transfers,
        };
        state.read(cx).delegate().column_texts(col)
    }

    /// The note saying how far back the shown tab reaches, if any.
    pub fn reach_text(&self, cx: &Context<Self>) -> Option<String> {
        self.reach_note(cx)
    }

    /// What the shown table says when it has no rows.
    pub fn empty_text(&self, cx: &gpui_kit::App) -> String {
        let state = match self.tab {
            Tab::Trades => &self.trades,
            Tab::Orders => &self.orders,
            Tab::Funding if self.every_payment => &self.payments,
            Tab::Funding => &self.by_market,
            Tab::Transfers => &self.transfers,
        };
        state.read(cx).delegate().empty_text()
    }

    /// The tooltip of a cell of the shown table.
    pub fn tip(&self, row: usize, col: usize, cx: &gpui_kit::App) -> Option<String> {
        let state = match self.tab {
            Tab::Orders => &self.orders,
            _ => return None,
        };
        state
            .read(cx)
            .delegate()
            .cell(row, col)
            .and_then(|c| c.tip_text().map(str::to_string))
    }

    pub fn note(&self) -> Option<String> {
        self.note.as_ref().map(|(t, _)| t.to_string())
    }

    pub fn press_for_test(&mut self, id: &'static str, key: u64, cx: &mut Context<Self>) {
        self.press(id, key, cx);
    }

    /// The line of totals of the shown tab, as figures.
    pub fn trade_totals(&self) -> galata_desk_core::history::trades::TradeTotals {
        trades::totals(&self.listed.trades)
    }

    pub fn funding_totals(&self) -> galata_desk_core::history::funding::FundingTotals {
        hfunding::totals(&self.listed.payments)
    }

    pub fn ledger_totals(&self) -> galata_desk_core::history::ledger::LedgerTotals {
        ledger::totals(&self.listed.transfers)
    }

    pub fn order_counts(&self) -> (usize, HashMap<Status, usize>) {
        orders::counts(&self.listed.orders)
    }
}
