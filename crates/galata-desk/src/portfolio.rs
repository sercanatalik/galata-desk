//! The portfolio screen: what the account is worth, how it has performed,
//! what it holds and what orders it has resting. Read-only.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local};
use galata_desk_core::connect::Address;
use galata_desk_core::feed::{Feed, Overview, Snapshot};
use galata_desk_core::format as f;
use galata_desk_core::model::{
    self, BalanceRow, Collateral, Equity, Holdings, OrderRow, Period, PositionRow, Scope, Series,
    Wallet,
};
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::rust_decimal::prelude::ToPrimitive;
use galata_desk_core::venue::{Http, MarginMode, Side};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::chart::LineChart;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Entity, EventEmitter, SharedString, Task, TestSupportExt,
    Window, div, px,
};

use crate::Core;
use crate::grid::{Cell, Col, Grid, Tone};
use crate::theme::{MONO, Palette};
use crate::ui;

pub enum PortfolioEvent {
    /// The user asked to forget this account.
    Disconnect,
}

#[derive(Clone, Copy, PartialEq)]
enum FeeMarket {
    Perps,
    Spot,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Balances,
    Positions,
    Orders,
}

pub struct Portfolio {
    address: Address,
    snapshot: Snapshot,
    feed: Feed,
    _watch: Task<()>,
    scope: Scope,
    period: Period,
    series: Series,
    chart_period: Period,
    fee_market: FeeMarket,
    tab: Tab,
    positions: Entity<TableState<Grid>>,
    balances: Entity<TableState<Grid>>,
    orders: Entity<TableState<Grid>>,
    /// Something the user should know that is not about the venue.
    warning: Option<SharedString>,
    copied: bool,
}

impl EventEmitter<PortfolioEvent> for Portfolio {}

fn table(grid: Grid, window: &mut Window, cx: &mut Context<Portfolio>) -> Entity<TableState<Grid>> {
    cx.new(|cx| {
        TableState::new(grid, window, cx)
            .sortable(false)
            .col_movable(false)
            .row_selectable(false)
            .col_selectable(false)
    })
}

impl Portfolio {
    pub fn new(address: Address, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The feed's task runs on the I/O runtime.
        let feed = {
            let core = Core::get(cx);
            let _io = core.runtime.enter();
            Feed::start(Http::at(core.info_url()), address.as_str())
        };
        #[allow(unused_mut)]
        let mut snapshots = feed.snapshots();
        // In the headless tests the view is given its snapshots directly. The
        // feed still runs, on the I/O runtime's threads, and a wake-up from
        // one of them is activity the deterministic test scheduler refuses:
        // so there the view does not listen to it.
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
                    .update(cx, |this, cx| this.apply(snapshot, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let positions = Grid::new(
            vec![
                Col::text("Market", 170.),
                Col::number("Size", 170.),
                Col::number("Position value", 140.),
                Col::number("Entry price", 110.),
                Col::number("Mark price", 110.),
                Col::number("PNL (ROE)", 200.),
                Col::number("Liq. price", 110.),
                Col::number("Margin", 170.),
                Col::number("Funding", 110.),
            ],
            "No open positions",
        );
        let balances = Grid::new(
            vec![
                Col::text("Asset", 170.),
                Col::number("Total", 190.),
                Col::number("On hold", 170.),
                Col::number("Available", 190.),
                Col::number("USD value", 170.),
            ],
            "No balances",
        );
        let orders = Grid::new(
            vec![
                Col::text("Placed", 160.),
                Col::text("Market", 170.),
                Col::text("Type", 130.),
                Col::text("Side", 70.),
                Col::number("Size (left / original)", 220.),
                Col::number("Price", 130.),
                Col::text("Conditions", 260.),
            ],
            "No open orders",
        );

        #[allow(unused_mut)]
        let mut this = Self {
            address,
            snapshot: Snapshot::default(),
            feed,
            _watch: watch,
            scope: Scope::default(),
            period: Period::default(),
            series: Series::default(),
            chart_period: Period::Month,
            fee_market: FeeMarket::Perps,
            tab: Tab::Positions,
            positions: table(positions, window, cx),
            balances: table(balances, window, cx),
            orders: table(orders, window, cx),
            warning: None,
            copied: false,
        };
        #[cfg(feature = "shot")]
        this.preselect();
        this
    }

    /// `GALATA_DESK_SHOT_STATE=scope=perps,period=week,series=value,chart=day,fees=spot,tab=orders`:
    /// open with these selections, so a screenshot can show a state that
    /// otherwise takes a click to reach.
    #[cfg(feature = "shot")]
    fn preselect(&mut self) {
        let period = |v: &str| match v {
            "day" => Period::Day,
            "week" => Period::Week,
            "month" => Period::Month,
            _ => Period::AllTime,
        };
        let state = std::env::var("GALATA_DESK_SHOT_STATE").unwrap_or_default();
        for (key, value) in state.split(',').filter_map(|kv| kv.split_once('=')) {
            match key {
                "scope" => {
                    self.scope = if value == "perps" {
                        Scope::Perps
                    } else {
                        Scope::All
                    }
                }
                "period" => self.period = period(value),
                "chart" => self.chart_period = period(value),
                "series" => {
                    self.series = if value == "value" {
                        Series::AccountValue
                    } else {
                        Series::Pnl
                    }
                }
                "fees" => {
                    self.fee_market = if value == "spot" {
                        FeeMarket::Spot
                    } else {
                        FeeMarket::Perps
                    }
                }
                "tab" => {
                    self.tab = match value {
                        "balances" => Tab::Balances,
                        "orders" => Tab::Orders,
                        _ => Tab::Positions,
                    }
                }
                _ => {}
            }
        }
    }

    pub fn warn(&mut self, text: String, cx: &mut Context<Self>) {
        self.warning = Some(text.into());
        cx.notify();
    }

    fn apply(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        if snapshot.holdings.data != self.snapshot.holdings.data
            && let Some(holdings) = &snapshot.holdings.data
        {
            let rows = holdings.positions.iter().map(position_row).collect();
            self.positions.update(cx, |t, cx| {
                t.delegate_mut().set_rows(rows);
                t.refresh(cx);
            });
            let rows = holdings.balances.iter().map(balance_row).collect();
            self.balances.update(cx, |t, cx| {
                t.delegate_mut().set_rows(rows);
                t.refresh(cx);
            });
        }
        if snapshot.orders.data != self.snapshot.orders.data
            && let Some(orders) = &snapshot.orders.data
        {
            let rows = orders.iter().map(order_row).collect();
            self.orders.update(cx, |t, cx| {
                t.delegate_mut().set_rows(rows);
                t.refresh(cx);
            });
        }
        self.snapshot = snapshot;
        cx.notify();
    }

    fn holdings(&self) -> Option<&Holdings> {
        self.snapshot.holdings.data.as_ref()
    }

    fn overview(&self) -> Option<&Overview> {
        self.snapshot.overview.data.as_ref()
    }

    fn equity(&self) -> Option<Equity> {
        let (h, o) = (self.holdings()?, self.overview()?);
        Some(Equity {
            perps: h.perps_equity,
            spot: h.spot_equity,
            vault: o.vault_equity,
            staking_hype: o.staking_hype,
            collateral: h.collateral,
            venue_total: o
                .history
                .period("day")
                .and_then(|d| d.account_value_history.last())
                .map(|p| p.1),
        })
    }

    /// The Portfolio section's part of the title bar: how fresh the figures
    /// are, and a manual refresh.
    pub fn freshness(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let updated = match self.snapshot.last_refreshed() {
            Some(at) => format!("Updated {}", clock(at)),
            None if self.snapshot.failure().is_some() => "Not loaded".into(),
            None => "Loading\u{2026}".into(),
        };
        h_flex()
            .gap_2()
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .text_color(if self.snapshot.failure().is_some() {
                        Palette::amber()
                    } else {
                        Palette::faint()
                    })
                    .child(updated),
            )
            .child(
                Button::new("refresh")
                    .ghost()
                    .small()
                    .label("Refresh")
                    .on_click(cx.listener(|this, _, _, _| this.feed.refresh_now())),
            )
            .into_any_element()
    }

    /// The account's part of the title bar, shown in every section: the
    /// address, copy, and disconnect.
    pub fn account_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let full = self.address.as_str().to_string();
        h_flex()
            .gap_2()
            .pr_3()
            .child(div().w(px(1.)).h(px(16.)).bg(Palette::line()))
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(12.5))
                    .child(f::short_address(&full)),
            )
            .child(
                Button::new("copy-address")
                    .ghost()
                    .small()
                    .label(if self.copied { "Copied" } else { "Copy" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(full.clone()));
                        this.copied = true;
                        cx.notify();
                        cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(1500))
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                this.copied = false;
                                cx.notify();
                            });
                        })
                        .detach();
                    })),
            )
            .child(
                Button::new("disconnect")
                    .ghost()
                    .small()
                    .label("Disconnect")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PortfolioEvent::Disconnect))),
            )
            .into_any_element()
    }

    /// The account in one line, once both the holdings and the history have
    /// loaded.
    pub fn account_summary(&self) -> Option<model::AccountSummary> {
        Some(model::account_summary(
            self.holdings()?,
            &self.equity()?,
            &self.overview()?.history,
        ))
    }

    /// The footer every section shows: equity and PNL at a glance.
    ///
    /// Placeholders until the figures arrive, never zeros. When the venue has
    /// stopped answering the last figures stay, and the time they are from
    /// turns amber.
    pub fn footer(&self) -> AnyElement {
        let summary = self.account_summary();
        let stale = self.snapshot.failure().is_some();
        let figure = |text: String, tone: gpui_kit::Hsla| {
            div()
                .font_family(MONO)
                .text_color(tone)
                .child(text)
                .into_any_element()
        };
        let signed = |value: Decimal| figure(f::usd_signed(value), ui::tone_of(sign(value)));
        let pending = |width: f32| ui::placeholder(px(width), px(12.)).into_any_element();
        // One labelled figure; every cell after the first has a rule before it.
        let cell = |first: bool, label: &'static str, values: Vec<AnyElement>| {
            h_flex()
                .gap_2()
                .when(first, |d| d.pr(px(18.)))
                .when(!first, |d| {
                    d.px(px(18.)).border_l_1().border_color(Palette::line())
                })
                .child(div().text_color(Palette::muted()).child(label))
                .children(values)
        };

        let equity = match &summary {
            Some(s) => vec![
                div()
                    .font_family(MONO)
                    .text_size(px(13.5))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(f::usd(s.equity))
                    .into_any_element(),
            ],
            None => vec![pending(84.)],
        };
        let unrealised = match &summary {
            Some(s) => {
                let mut values = vec![signed(s.unrealised)];
                if let Some(share) = s.unrealised_share.filter(|_| s.positions > 0) {
                    values.push(figure(f::pct_signed(share, 1), Palette::muted()));
                }
                values
            }
            None => vec![pending(62.)],
        };
        let period = |pnl: Option<Decimal>| match (&summary, pnl) {
            (Some(_), Some(pnl)) => vec![signed(pnl)],
            // Loaded, and the venue has no history for the period.
            (Some(_), None) => vec![figure("\u{2014}".into(), Palette::muted())],
            (None, _) => vec![pending(62.)],
        };

        let right = match &summary {
            Some(s) if s.positions == 0 => "No positions".to_string(),
            Some(s) => {
                let count = if s.positions == 1 {
                    "1 position".to_string()
                } else {
                    format!("{} positions", s.positions)
                };
                match s.margin_share {
                    Some(share) => format!(
                        "{count} \u{b7} Margin used {}%",
                        (share * Decimal::from(100)).round()
                    ),
                    None => count,
                }
            }
            None => String::new(),
        };
        let time = match (self.snapshot.last_refreshed(), stale) {
            (Some(at), true) => format!("Figures from {}", clock(at)),
            (Some(at), false) => clock(at),
            (None, true) => "Not loaded".into(),
            (None, false) => "Loading\u{2026}".into(),
        };

        h_flex()
            .id("account-footer")
            .flex_shrink_0()
            .flex_wrap()
            .gap_y_1()
            .min_h(px(34.))
            .px_4()
            .py(px(6.))
            .border_t_1()
            .border_color(Palette::rule())
            .bg(Palette::panel())
            .text_size(px(12.5))
            .child(cell(true, "Equity", equity))
            .child(cell(false, "Unrealised PNL", unrealised))
            .child(cell(
                false,
                "PNL 24H",
                period(summary.as_ref().and_then(|s| s.pnl_day)),
            ))
            .child(cell(
                false,
                "All-time",
                period(summary.as_ref().and_then(|s| s.pnl_all_time)),
            ))
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap(px(14.))
                    .text_color(if stale {
                        Palette::amber()
                    } else {
                        Palette::muted()
                    })
                    .child(right)
                    .child(div().font_family(MONO).text_size(px(12.)).child(time)),
            )
            .test_support()
            .into_any_element()
    }

    /// The account's latest fills, once they have loaded. The Markets chart
    /// marks them.
    pub fn fills(&self) -> Option<&galata_desk_core::market::fills::Fills> {
        self.snapshot.fills.data.as_ref()
    }

    /// The account's open positions, once they have loaded. The Markets
    /// section marks the markets among them.
    pub fn positions(&self) -> Option<&[PositionRow]> {
        self.holdings().map(|h| h.positions.as_slice())
    }

    /// The Funding section, drawn from the positions and rates held here.
    pub fn funding(&self) -> AnyElement {
        crate::funding::render(self.positions(), &self.snapshot.rates)
    }

    /// A section that never loaded: what went wrong, and a way to try again.
    fn failed(&self, what: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let error = self.snapshot.failure()?;
        Some(
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .p_4()
                .child(
                    div()
                        .text_color(Palette::amber_text())
                        .child(format!("{what} could not be loaded")),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(Palette::muted())
                        .child(ui::explain(error)),
                )
                .child(
                    Button::new(SharedString::from(format!("retry-{what}")))
                        .small()
                        .label("Retry")
                        .on_click(cx.listener(|this, _, _, _| this.feed.refresh_now())),
                )
                .into_any_element(),
        )
    }

    fn banners(&self) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if let Some(warning) = &self.warning {
            out.push(ui::notice(warning.clone()).into_any_element());
        }
        // Stale: something is on screen, and the latest attempt to renew it failed.
        if let (Some(error), Some(at)) = (self.snapshot.failure(), self.snapshot.last_refreshed()) {
            out.push(
                ui::notice(format!(
                    "Showing figures from {}. {} The app keeps retrying.",
                    clock(at),
                    ui::explain(error)
                ))
                .into_any_element(),
            );
        }
        out
    }

    fn summary(&self, cx: &mut Context<Self>) -> AnyElement {
        let tile = || ui::panel().px_4().py_3().gap_2().justify_center();

        let value = tile()
            .flex_1()
            .child(ui::label("Account value"))
            .child(match self.equity() {
                Some(e) => ui::figure(f::usd(e.total()), px(30.)).into_any_element(),
                None => ui::placeholder(px(190.), px(34.)).into_any_element(),
            });

        let volume = tile()
            .child(ui::label("14 day volume"))
            .child(match self.overview() {
                Some(o) => ui::figure(f::usd_compact(model::volume_14d(&o.fees)), px(22.))
                    .into_any_element(),
                None => ui::placeholder(px(90.), px(26.)).into_any_element(),
            });

        let fees = tile()
            .child(
                h_flex()
                    .justify_between()
                    .child(ui::label("Fees (taker / maker)"))
                    .child(
                        TabBar::new("fee-market")
                            .segmented()
                            .xsmall()
                            .selected_index(match self.fee_market {
                                FeeMarket::Perps => 0,
                                FeeMarket::Spot => 1,
                            })
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.fee_market = if *ix == 0 {
                                    FeeMarket::Perps
                                } else {
                                    FeeMarket::Spot
                                };
                                cx.notify();
                            }))
                            .children(["Perps", "Spot"]),
                    ),
            )
            .child(match self.overview() {
                Some(o) => {
                    let (taker, maker) = match self.fee_market {
                        FeeMarket::Perps => (o.fees.user_cross_rate, o.fees.user_add_rate),
                        FeeMarket::Spot => (o.fees.user_spot_cross_rate, o.fees.user_spot_add_rate),
                    };
                    ui::figure(f::fee_pair(taker, maker), px(18.)).into_any_element()
                }
                None => ui::placeholder(px(170.), px(22.)).into_any_element(),
            });

        v_flex()
            .w(px(280.))
            .flex_shrink_0()
            .gap_3()
            .child(value)
            .child(volume)
            .child(fees)
            .into_any_element()
    }

    fn breakdown(&self, cx: &mut Context<Self>) -> AnyElement {
        let header = h_flex()
            .justify_between()
            .px_3()
            .h(px(44.))
            .flex_shrink_0()
            .border_b_1()
            .border_color(Palette::rule())
            .child(
                TabBar::new("scope")
                    .segmented()
                    .xsmall()
                    .selected_index(
                        Scope::ALL
                            .iter()
                            .position(|s| *s == self.scope)
                            .unwrap_or(0),
                    )
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.scope = Scope::ALL[*ix];
                        cx.notify();
                    }))
                    .children(["All", "Perps"]),
            )
            .child(self.period_picker("period", self.period, cx, |this, p| this.period = p));

        let pending = || ui::placeholder(px(96.), px(16.)).into_any_element();
        let figure = |text: String, sign: i32| {
            ui::figure(text, px(13.5))
                .text_color(ui::tone_of(sign))
                .into_any_element()
        };

        let stats = self
            .overview()
            .and_then(|o| model::period_stats(&o.history, self.scope, self.period));
        let overview_loaded = self.overview().is_some();
        let stat = |value: Option<AnyElement>| match value {
            Some(v) => v,
            // Loaded, but the venue has nothing for this period.
            None if overview_loaded => figure("\u{2014}".into(), 0),
            None => pending(),
        };
        let equity = self.equity();
        let in_spot = equity
            .as_ref()
            .is_some_and(|e| e.collateral == Collateral::InSpot);
        let eq = |pick: fn(&Equity) -> Decimal| match &equity {
            Some(e) => figure(f::usd(pick(e)), 0),
            None => pending(),
        };

        let body = v_flex()
            .px_3()
            .py_1()
            .child(ui::row(
                "PNL",
                stat(
                    stats
                        .as_ref()
                        .and_then(|s| s.pnl)
                        .map(|p| figure(f::usd_signed(p), sign(p))),
                ),
            ))
            .child(ui::row(
                "Volume",
                stat(stats.as_ref().map(|s| figure(f::usd(s.volume), 0))),
            ))
            .child(ui::row(
                "Max drawdown",
                stat(
                    stats
                        .as_ref()
                        .map(|s| figure(f::drawdown(s.max_drawdown), 0)),
                ),
            ))
            .child(div().h(px(1.)).my_1().bg(Palette::rule()))
            .child(ui::row("Total equity", eq(Equity::total)))
            // With collateral held in spot, the perpetuals figure is a share
            // of the spot balance, and the labels say so.
            .child(ui::row(
                if in_spot {
                    "Spot equity (all collateral)"
                } else {
                    "Spot equity"
                },
                eq(|e| e.spot),
            ))
            .child(ui::row(
                if in_spot {
                    "of which perps"
                } else {
                    "Perps equity"
                },
                eq(|e| e.perps),
            ))
            .child(ui::row("Vault equity", eq(|e| e.vault)))
            .child(ui::row(
                "Staking balance",
                match &equity {
                    Some(e) => figure(f::size(e.staking_hype, "HYPE"), 0),
                    None => pending(),
                },
            ));

        ui::panel()
            .w(px(360.))
            .flex_shrink_0()
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn period_picker(
        &self,
        id: &'static str,
        selected: Period,
        cx: &mut Context<Self>,
        set: fn(&mut Self, Period),
    ) -> impl IntoElement + use<> {
        TabBar::new(id)
            .segmented()
            .xsmall()
            .selected_index(Period::ALL.iter().position(|p| *p == selected).unwrap_or(0))
            .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                set(this, Period::ALL[*ix]);
                cx.notify();
            }))
            .children(Period::ALL.map(|p| match p {
                Period::AllTime => "All",
                other => other.label(),
            }))
    }

    fn chart(&self, cx: &mut Context<Self>) -> AnyElement {
        let header = h_flex()
            .justify_between()
            .px_3()
            .h(px(44.))
            .flex_shrink_0()
            .border_b_1()
            .border_color(Palette::rule())
            .child(
                TabBar::new("series")
                    .segmented()
                    .xsmall()
                    .selected_index(match self.series {
                        Series::AccountValue => 0,
                        Series::Pnl => 1,
                    })
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.series = if *ix == 0 {
                            Series::AccountValue
                        } else {
                            Series::Pnl
                        };
                        cx.notify();
                    }))
                    .children(["Account value", "PNL"]),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(ui::label(match self.scope {
                        Scope::All => "Perps + Spot + Vaults",
                        Scope::Perps => "Perps only",
                    }))
                    .child(
                        self.period_picker("chart-period", self.chart_period, cx, |this, p| {
                            this.chart_period = p
                        }),
                    ),
            );

        let body: AnyElement = match self.overview() {
            None => match self.failed("The history", cx) {
                Some(failed) => failed,
                None => div()
                    .size_full()
                    .p_4()
                    .child(ui::placeholder(px(9999.), px(9999.)))
                    .into_any_element(),
            },
            Some(o) => {
                let points = model::series(&o.history, self.series, self.scope, self.chart_period);
                if points.len() < 2 {
                    h_flex()
                        .size_full()
                        .justify_center()
                        .text_color(Palette::faint())
                        .child(format!("No history for {}", self.chart_period.label()))
                        .into_any_element()
                } else {
                    div()
                        .size_full()
                        .pt_4()
                        .pb_2()
                        .px_3()
                        .child(history_chart(
                            points,
                            self.series,
                            self.scope,
                            self.chart_period,
                        ))
                        .into_any_element()
                }
            }
        };

        ui::panel()
            .flex_1()
            .min_w_0()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn tables(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = |n: Option<usize>, name: &str| match n {
            Some(n) => format!("{name} ({n})"),
            None => name.to_string(),
        };
        let holdings = self.holdings();
        let labels = [
            count(holdings.map(|h| h.balances.len()), "Balances"),
            count(holdings.map(|h| h.positions.len()), "Positions"),
            count(
                self.snapshot.orders.data.as_ref().map(Vec::len),
                "Open orders",
            ),
        ];
        let tabs = TabBar::new("holdings")
            .underline()
            .selected_index(match self.tab {
                Tab::Balances => 0,
                Tab::Positions => 1,
                Tab::Orders => 2,
            })
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.tab = [Tab::Balances, Tab::Positions, Tab::Orders][*ix];
                cx.notify();
            }))
            .children(labels);

        let (state, loaded, what) = match self.tab {
            Tab::Balances => (&self.balances, holdings.is_some(), "Balances"),
            Tab::Positions => (&self.positions, holdings.is_some(), "Positions"),
            Tab::Orders => (
                &self.orders,
                self.snapshot.orders.data.is_some(),
                "Open orders",
            ),
        };
        let body: AnyElement = if loaded {
            DataTable::new(state)
                .stripe(false)
                .bordered(false)
                .into_any_element()
        } else if let Some(failed) = self.failed(what, cx) {
            failed
        } else {
            // Rows in the shape of the table to come.
            v_flex()
                .p_4()
                .gap_3()
                .children((0..5).map(|_| ui::placeholder(px(9999.), px(18.))))
                .into_any_element()
        };

        ui::panel()
            .flex_1()
            .min_h_0()
            .child(div().px_2().pt_1().flex_shrink_0().child(tabs))
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

impl Render for Portfolio {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The top row never loaded and never will without a retry: say so once,
        // across the row, instead of three sets of placeholders.
        let top_failed = self.overview().is_none() && self.snapshot.overview.error.is_some();
        let top: AnyElement = if top_failed {
            ui::panel()
                .size_full()
                .children(self.failed("The account summary", cx))
                .into_any_element()
        } else {
            h_flex()
                .size_full()
                .gap_3()
                .items_stretch()
                .child(self.summary(cx))
                .child(self.breakdown(cx))
                .child(self.chart(cx))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .p_3()
            .gap_3()
            .children(self.banners())
            .child(div().h(px(324.)).flex_shrink_0().child(top))
            .child(self.tables(cx))
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

fn local(ms: u64) -> DateTime<Local> {
    DateTime::from_timestamp_millis(ms as i64)
        .unwrap_or_default()
        .with_timezone(&Local)
}

/// A time of day on this machine's clock: `04:21:07`.
fn clock(at: SystemTime) -> String {
    DateTime::<Local>::from(at).format("%H:%M:%S").to_string()
}

fn position_row(p: &PositionRow) -> Vec<Cell> {
    let side = if p.is_long() { Tone::Gain } else { Tone::Loss };
    let mut market = Cell::text(p.market.clone()).and(format!("{}x", p.leverage), Tone::Muted);
    if !p.dex.is_empty() {
        market = market.and(p.dex.clone(), Tone::Tag);
    }
    vec![
        market,
        Cell::new(f::size(p.size, &p.market), side),
        Cell::text(f::usd(p.value)),
        Cell::text(f::price(p.entry)),
        Cell::text(
            p.mark
                .map(|m| f::price_like(m, p.entry))
                .unwrap_or_else(|| "\u{2014}".into()),
        ),
        Cell::new(f::pnl_with_roe(p.pnl, p.roe), Tone::signed(sign(p.pnl))),
        Cell::text(
            p.liquidation
                .map(|l| f::price_like(l, p.entry))
                .unwrap_or_else(|| "N/A".into()),
        ),
        Cell::text(f::usd(p.margin)).and(
            match p.mode {
                MarginMode::Cross => "Cross",
                MarginMode::Isolated => "Isolated",
            },
            Tone::Muted,
        ),
        Cell::new(f::usd_signed(p.funding), Tone::signed(sign(p.funding))),
    ]
}

fn balance_row(b: &BalanceRow) -> Vec<Cell> {
    // Balances are shown to six decimals: enough for any token, without the
    // eighteen a raw amount can carry.
    let qty = |v: Decimal| f::amount(v.round_dp(6));
    vec![
        Cell::text(b.asset.clone()).and(
            match b.wallet {
                Wallet::Perps => "Perps",
                Wallet::Spot => "Spot",
            },
            Tone::Tag,
        ),
        Cell::text(qty(b.total)),
        Cell::muted(qty(b.on_hold)),
        Cell::text(qty(b.available)),
        Cell::text(f::usd_or_dash(b.usd)),
    ]
}

fn order_row(o: &OrderRow) -> Vec<Cell> {
    let (side, tone) = match o.side {
        Side::Buy => ("Buy", Tone::Gain),
        Side::Sell => ("Sell", Tone::Loss),
    };
    let mut market = Cell::text(o.market.clone());
    if !o.dex.is_empty() {
        market = market.and(o.dex.clone(), Tone::Tag);
    }
    let mut conditions = Cell::default();
    if o.reduce_only {
        conditions = conditions.and("Reduce only", Tone::Tag);
    }
    if let Some(trigger) = &o.trigger {
        conditions = conditions.and(trigger.clone(), Tone::Muted);
    }
    vec![
        Cell::muted(local(o.placed_ms).format("%b %-d %H:%M:%S").to_string()),
        market,
        Cell::text(o.kind.clone()),
        Cell::new(side, tone),
        Cell::text(f::amount(o.remaining)).and(format!("/ {}", f::amount(o.original)), Tone::Muted),
        Cell::text(f::price(o.price)),
        conditions,
    ]
}

/// One point of the history chart.
struct Point {
    /// The axis label. Unique per point: the chart places points by label.
    label: SharedString,
    /// The tooltip's heading: the full date and time.
    title: SharedString,
    value: f64,
}

fn history_chart(
    history: &[(u64, Decimal)],
    series: Series,
    scope: Scope,
    period: Period,
) -> impl IntoElement {
    let mut points: Vec<Point> = Vec::with_capacity(history.len());
    for (ms, value) in history {
        let at = local(*ms);
        points.push(Point {
            // The chart places points by label; the time itself is unique.
            // The labels people read are drawn below, by `time_ticks`.
            label: ms.to_string().into(),
            title: at.format("%b %-d %Y, %H:%M").to_string().into(),
            value: value.to_f64().unwrap_or_default(),
        });
    }
    let low = points.iter().map(|p| p.value).fold(f64::INFINITY, f64::min);
    let high = points
        .iter()
        .map(|p| p.value)
        .fold(f64::NEG_INFINITY, f64::max);
    let signed = series == Series::Pnl;

    let money = move |v: f64| {
        let d = Decimal::try_from(v).unwrap_or_default();
        if signed { f::usd_signed(d) } else { f::usd(d) }
    };

    let chart = LineChart::new(points)
        .id("history")
        // Draw in again when the selection changes, not on every refresh.
        .appear_key((series, scope, period))
        .x(|p: &Point| p.label.clone())
        .y(|p: &Point| p.value)
        .step_after()
        .stroke(Palette::amber())
        .y_axis(true)
        .y_tick_count(4)
        .y_tick_format(move |v| axis_label(v, high - low))
        .x_axis(false)
        .tooltip_title(|p: &Point| p.title.clone())
        .tooltip_value(move |_, v| money(v).into());
    // Scale to the data, with a little air above and below: an account worth
    // millions that moved by thousands should not be a flat line under the
    // top edge of an axis that starts at zero.
    let air = ((high - low) * 0.08)
        .max(high.abs().max(low.abs()) * 0.001)
        .max(0.01);
    let chart = chart.y_domain(low - air, high + air);
    // Mark zero where the series crosses it.
    let chart = if low < 0. && high > 0. {
        chart.reference_line(0.)
    } else {
        chart
    };
    let times: Vec<u64> = history.iter().map(|(ms, _)| *ms).collect();
    let ticks = time_ticks(&times, |ms, format| local(ms).format(format).to_string());
    v_flex()
        .size_full()
        .gap_1()
        .child(div().flex_1().min_h_0().child(chart))
        .child(
            h_flex()
                .justify_between()
                // The plot starts after the value axis's labels, on the left.
                .pl(px(VALUE_AXIS_WIDTH))
                .font_family(MONO)
                .text_size(px(11.))
                .text_color(Palette::muted())
                .children(ticks),
        )
}

/// The room the chart's value axis takes on its left.
const VALUE_AXIS_WIDTH: f32 = 34.;

/// How a time on the axis is written, by how much time the chart spans: the
/// time of day for a day, the date with it for a week, the date alone for a
/// month, and the month for longer.
fn tick_format(span_ms: u64) -> &'static str {
    const HOUR: u64 = 3_600_000;
    const DAY: u64 = 24 * HOUR;
    if span_ms <= 36 * HOUR {
        "%H:%M"
    } else if span_ms <= 10 * DAY {
        "%b %-d %H:%M"
    } else if span_ms <= 400 * DAY {
        "%b %-d"
    } else {
        "%b %Y"
    }
}

/// The time axis's labels: up to five, evenly spaced from the first point to
/// the last, no two alike. `write` formats one time; it is passed in so the
/// rule can be tested without this machine's time zone.
fn time_ticks(times: &[u64], write: impl Fn(u64, &str) -> String) -> Vec<String> {
    const MOST: usize = 5;
    let (Some(first), Some(last)) = (times.first(), times.last()) else {
        return Vec::new();
    };
    let format = tick_format(last.saturating_sub(*first));
    let count = times.len().min(MOST);
    let mut labels: Vec<String> = Vec::with_capacity(count);
    for i in 0..count {
        let at = if count == 1 {
            0
        } else {
            (times.len() - 1) * i / (count - 1)
        };
        let label = write(times[at], format);
        // Two points close together can still read the same: say it once.
        if labels.last() != Some(&label) {
            labels.push(label);
        }
    }
    labels
}

/// An axis tick: abbreviated like `$3.2M`, with as many decimals as it takes
/// for neighbouring ticks to differ when the whole axis spans less than a unit.
fn axis_label(value: f64, span: f64) -> String {
    let magnitude = value.abs();
    let (unit, suffix) = if magnitude >= 1e9 {
        (1e9, "B")
    } else if magnitude >= 1e6 {
        (1e6, "M")
    } else if magnitude >= 1e3 {
        (1e3, "k")
    } else {
        (1., "")
    };
    // Four ticks divide the span in three; each step must show in the last digit.
    let places = match span / unit {
        s if s >= 3. => 0,
        s if s >= 0.3 => 1,
        s if s >= 0.03 => 2,
        _ => 3,
    };
    let sign = if value < 0. { "-" } else { "" };
    format!("{sign}${:.*}{suffix}", places, magnitude / unit)
}

#[cfg(test)]
impl Portfolio {
    /// Give the view positions, as if the venue had reported them.
    pub fn set_positions(&mut self, positions: Vec<PositionRow>, cx: &mut Context<Self>) {
        let mut snapshot = self.snapshot.clone();
        let holdings = Holdings {
            positions,
            ..Holdings::default()
        };
        snapshot.holdings.data = Some(holdings);
        snapshot.holdings.error = None;
        self.apply(snapshot, cx);
    }

    /// Give the view fills, as if the venue had reported them.
    pub fn set_fills(
        &mut self,
        fills: galata_desk_core::market::fills::Fills,
        cx: &mut Context<Self>,
    ) {
        self.snapshot.fills.data = Some(fills);
        cx.notify();
    }

    /// Give the view funding rates, as if the venue had reported them.
    pub fn set_rates(&mut self, rates: &[(&str, &str)], cx: &mut Context<Self>) {
        self.snapshot.rates.data = Some(
            rates
                .iter()
                .map(|(coin, rate)| (coin.to_string(), rate.parse().unwrap()))
                .collect(),
        );
        cx.notify();
    }

    /// The current selections, as short words, for tests that click them.
    pub fn selections(&self) -> String {
        format!(
            "scope={:?} period={:?} series={:?} chart={:?} fees={} tab={}",
            self.scope,
            self.period,
            self.series,
            self.chart_period,
            match self.fee_market {
                FeeMarket::Perps => "perps",
                FeeMarket::Spot => "spot",
            },
            match self.tab {
                Tab::Balances => "balances",
                Tab::Positions => "positions",
                Tab::Orders => "orders",
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{axis_label, tick_format, time_ticks};

    const HOUR: u64 = 3_600_000;
    const DAY: u64 = 24 * HOUR;
    /// 2026-11-10 11:00 UTC.
    const T0: u64 = 1_794_308_400_000;

    fn utc(ms: u64, format: &str) -> String {
        chrono::DateTime::from_timestamp_millis(ms as i64)
            .unwrap()
            .format(format)
            .to_string()
    }

    #[test]
    fn the_time_axis_is_written_by_how_much_time_it_spans() {
        assert_eq!(tick_format(DAY), "%H:%M");
        assert_eq!(tick_format(7 * DAY), "%b %-d %H:%M");
        assert_eq!(tick_format(30 * DAY), "%b %-d");
        assert_eq!(tick_format(3 * 365 * DAY), "%b %Y");
    }

    #[test]
    fn a_week_with_several_points_a_day_repeats_no_label() {
        // A point every six hours for a week: four a day.
        let times: Vec<u64> = (0..=28).map(|i| T0 + i * 6 * HOUR).collect();
        let ticks = time_ticks(&times, utc);
        assert_eq!(
            ticks,
            [
                "Nov 10 11:00",
                "Nov 12 05:00",
                "Nov 13 23:00",
                "Nov 15 17:00",
                "Nov 17 11:00"
            ]
        );
    }

    #[test]
    fn ticks_run_from_the_first_point_to_the_last() {
        let times: Vec<u64> = (0..=96).map(|i| T0 + i * HOUR / 4).collect();
        let ticks = time_ticks(&times, utc);
        assert_eq!(ticks, ["11:00", "17:00", "23:00", "05:00", "11:00"]);
        let times: Vec<u64> = (0..=30).map(|i| T0 + i * DAY).collect();
        let ticks = time_ticks(&times, utc);
        assert_eq!(ticks.first().unwrap(), "Nov 10");
        assert_eq!(ticks.last().unwrap(), "Dec 10");
    }

    #[test]
    fn neighbouring_labels_that_read_the_same_are_said_once() {
        // Years of history with its newest points days apart: one month, once.
        let mut times: Vec<u64> = (0..4).map(|i| T0 - (1000 - i * 300) * DAY).collect();
        times.extend([T0 - DAY, T0]);
        let ticks = time_ticks(&times, utc);
        let mut once = ticks.clone();
        once.dedup();
        assert_eq!(ticks, once);
        assert!(ticks.len() < 6);
    }

    #[test]
    fn fewer_points_than_labels_are_each_labelled_once() {
        assert_eq!(time_ticks(&[T0, T0 + HOUR], utc), ["11:00", "12:00"]);
        assert_eq!(time_ticks(&[T0], utc), ["11:00"]);
        assert!(time_ticks(&[], utc).is_empty());
    }

    #[test]
    fn neighbouring_ticks_on_a_narrow_axis_differ() {
        // An account worth 3.2M that moved by 100k in the period.
        let ticks: Vec<_> = [3.13e6, 3.17e6, 3.21e6, 3.25e6]
            .iter()
            .map(|v| axis_label(*v, 0.12e6))
            .collect();
        assert_eq!(ticks, ["$3.13M", "$3.17M", "$3.21M", "$3.25M"]);
    }

    #[test]
    fn a_wide_axis_stays_short() {
        assert_eq!(axis_label(94_000., 220_000.), "$94k");
        assert_eq!(axis_label(-126_000., 220_000.), "-$126k");
        assert_eq!(axis_label(0., 220_000.), "$0");
        assert_eq!(axis_label(550., 1_650.), "$550");
        assert_eq!(axis_label(-1_100., 1_650.), "-$1.1k");
    }
}
