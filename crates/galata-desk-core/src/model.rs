//! The portfolio as the screen shows it, built from the venue's answers.
//!
//! Everything here is arithmetic on values already fetched: no I/O.

use std::collections::HashMap;

use rust_decimal::Decimal;

use crate::drawdown::max_drawdown;
use crate::trade::assets::{self, Asset};
use crate::trade::sign::Tpsl;
use crate::venue::{
    ClearinghouseState, DelegatorSummary, MarginMode, OpenOrder, PerpMeta, PortfolioHistory, Side,
    SpotMeta, SpotState, UserFees, VaultEquity,
};

/// One open perpetual position.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionRow {
    /// The asset, without any dex prefix: `NVDA`.
    pub market: String,
    /// The builder dex it trades on; empty for the main dex.
    pub dex: String,
    pub leverage: u32,
    pub mode: MarginMode,
    /// Signed: negative is short.
    pub size: Decimal,
    pub value: Decimal,
    pub entry: Decimal,
    /// Position value over size; `None` for a zero size.
    pub mark: Option<Decimal>,
    pub pnl: Decimal,
    /// A fraction.
    pub roe: Decimal,
    pub liquidation: Option<Decimal>,
    pub margin: Decimal,
    /// Funding since the position opened: positive was received.
    pub funding: Decimal,
    /// The market as an action names it. `None` until the venue's list of
    /// the dex's markets is known; the position cannot be acted on then.
    pub asset: Option<Asset>,
}

impl PositionRow {
    pub fn is_long(&self) -> bool {
        self.size >= Decimal::ZERO
    }

    /// The position's value with the side as its sign: negative for a short.
    /// The venue reports `value` unsigned.
    pub fn signed_value(&self) -> Decimal {
        if self.is_long() {
            self.value.abs()
        } else {
            -self.value.abs()
        }
    }
}

/// Which account a balance sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wallet {
    Perps,
    Spot,
}

/// One balance.
#[derive(Debug, Clone, PartialEq)]
pub struct BalanceRow {
    pub asset: String,
    pub wallet: Wallet,
    pub total: Decimal,
    /// Held by resting orders (spot) or as margin (perps).
    pub on_hold: Decimal,
    /// What could be moved out now.
    pub available: Decimal,
    /// `None` when the venue gives no price for the asset.
    pub usd: Option<Decimal>,
}

/// One resting order.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderRow {
    /// Milliseconds since the epoch.
    pub placed_ms: u64,
    pub market: String,
    /// Builder dex, empty for the main dex and for spot.
    pub dex: String,
    pub kind: String,
    pub side: Side,
    pub remaining: Decimal,
    pub original: Decimal,
    pub price: Decimal,
    pub reduce_only: bool,
    /// The trigger condition, for trigger orders.
    pub trigger: Option<String>,
    pub oid: u64,
    /// Time in force as the venue spells it. Absent for trigger orders.
    pub tif: Option<String>,
    /// A plain resting limit order: its price and size can be amended.
    pub amendable: bool,
    /// The market as an action names it. `None` until the venue's list of
    /// markets is known, or when the market is no longer in it; the order
    /// cannot be acted on then.
    pub asset: Option<Asset>,
    /// The trigger price, for trigger orders.
    pub trigger_px: Option<Decimal>,
    /// Set when the order is a position's take-profit or stop-loss, which
    /// covers the whole position. A trigger order with a fixed size is not
    /// one, and has `None`.
    pub tpsl: Option<Tpsl>,
}

/// Where the account's collateral is held, which decides how its parts add up.
///
/// Hyperliquid has account modes in which perpetuals are margined from the
/// spot balance. There the perpetuals account value is a share of the spot
/// balance, not money beside it, and adding the two counts it twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Collateral {
    /// `default`, `disabled`: perpetuals and spot are separate pots.
    Separate,
    /// `unifiedAccount`, `portfolioMargin`: one pot, held in spot.
    InSpot,
    /// The venue did not say, or said something this build does not know.
    /// Never assumed to be one of the others.
    #[default]
    Unknown,
}

impl Collateral {
    /// The mode for the venue's own word.
    pub fn from_venue(mode: &str) -> Self {
        match mode {
            "default" | "disabled" => Collateral::Separate,
            "unifiedAccount" | "portfolioMargin" => Collateral::InSpot,
            _ => Collateral::Unknown,
        }
    }
}

/// What the account is worth, by where it sits.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Equity {
    /// The perpetuals account value. With [`Collateral::InSpot`] this is the
    /// share of the spot balance that perpetuals account for.
    pub perps: Decimal,
    pub spot: Decimal,
    pub vault: Decimal,
    /// Staked and unstaked HYPE in the staking account.
    pub staking_hype: Decimal,
    pub collateral: Collateral,
    /// The latest account value in the venue's own history, up to a minute old.
    pub venue_total: Option<Decimal>,
}

impl Equity {
    /// What the whole account is worth. Staking is reported in HYPE beside it.
    ///
    /// - Separate pots: perpetuals, spot and vaults.
    /// - Collateral in spot: spot and vaults; perpetuals are already in spot.
    /// - Unknown mode: the venue's own latest figure, because either sum
    ///   could be wrong; the three-way sum only if the venue gave none.
    pub fn total(&self) -> Decimal {
        match self.collateral {
            Collateral::Separate => self.perps + self.spot + self.vault,
            Collateral::InSpot => self.spot + self.vault,
            Collateral::Unknown => self
                .venue_total
                .unwrap_or(self.perps + self.spot + self.vault),
        }
    }
}

/// Positions, balances and orders: the part that moves with the market.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Holdings {
    pub positions: Vec<PositionRow>,
    pub balances: Vec<BalanceRow>,
    pub perps_equity: Decimal,
    pub spot_equity: Decimal,
    pub collateral: Collateral,
}

/// The perpetuals state of every dex the account has anything on.
pub struct DexState<'a> {
    pub dex: &'a str,
    pub state: &'a ClearinghouseState,
    /// Every dex's name, main dex first, and this dex's markets once known:
    /// together they number a market for an action.
    pub dexes: &'a [String],
    pub perp: Option<&'a PerpMeta>,
}

/// Strip a builder dex's prefix: `xyz:NVDA` on dex `xyz` is `NVDA`.
fn market_name(coin: &str, dex: &str) -> String {
    match coin.split_once(':') {
        Some((prefix, name)) if prefix == dex => name.to_string(),
        _ => coin.to_string(),
    }
}

/// Positions across every dex, largest position value first.
pub fn positions(dexes: &[DexState<'_>]) -> Vec<PositionRow> {
    let mut rows: Vec<PositionRow> = dexes
        .iter()
        .flat_map(|d| {
            d.state.asset_positions.iter().map(|ap| {
                let p = &ap.position;
                PositionRow {
                    market: market_name(&p.coin, d.dex),
                    dex: d.dex.to_string(),
                    leverage: p.leverage.value,
                    mode: p.leverage.mode,
                    size: p.szi,
                    value: p.position_value,
                    entry: p.entry_px,
                    mark: (!p.szi.is_zero()).then(|| p.position_value / p.szi.abs()),
                    pnl: p.unrealized_pnl,
                    roe: p.return_on_equity,
                    liquidation: p.liquidation_px,
                    margin: p.margin_used,
                    funding: -p.cum_funding.since_open,
                    asset: d
                        .perp
                        .and_then(|perp| assets::perp(d.dexes, d.dex, perp, &p.coin)),
                }
            })
        })
        .collect();
    rows.sort_by(|a, b| b.value.cmp(&a.value).then_with(|| a.market.cmp(&b.market)));
    rows
}

/// The USD price of a spot token: 1 for USDC, otherwise the mid of its USDC pair.
fn spot_price(token: u32, meta: &SpotMeta, mids: &HashMap<String, Decimal>) -> Option<Decimal> {
    const USDC: u32 = 0;
    if token == USDC {
        return Some(Decimal::ONE);
    }
    let pair = meta.universe.iter().find(|p| p.tokens == [token, USDC])?;
    mids.get(&pair.name).copied()
}

/// Positions and balances, with the equity each side adds up to.
///
/// A spot token without a price is listed with no USD value and left out of
/// spot equity. A token with a zero total is not listed.
///
/// With collateral held in spot there is no separate perpetuals balance to
/// list: the spot USDC row is the one balance, as on the venue's own page.
pub fn holdings(
    dexes: &[DexState<'_>],
    spot: &SpotState,
    meta: &SpotMeta,
    mids: &HashMap<String, Decimal>,
    collateral: Collateral,
) -> Holdings {
    let perps_equity: Decimal = dexes
        .iter()
        .map(|d| d.state.margin_summary.account_value)
        .sum();
    let withdrawable: Decimal = dexes.iter().map(|d| d.state.withdrawable).sum();
    let margin_used: Decimal = dexes
        .iter()
        .map(|d| d.state.margin_summary.total_margin_used)
        .sum();

    let mut balances = Vec::new();
    if !perps_equity.is_zero() && collateral != Collateral::InSpot {
        balances.push(BalanceRow {
            asset: "USDC".into(),
            wallet: Wallet::Perps,
            total: perps_equity,
            on_hold: margin_used,
            available: withdrawable,
            usd: Some(perps_equity),
        });
    }
    let mut spot_equity = Decimal::ZERO;
    for b in spot.balances.iter().filter(|b| !b.total.is_zero()) {
        let usd = spot_price(b.token, meta, mids).map(|px| px * b.total);
        spot_equity += usd.unwrap_or_default();
        balances.push(BalanceRow {
            asset: b.coin.clone(),
            wallet: Wallet::Spot,
            total: b.total,
            on_hold: b.hold,
            available: b.total - b.hold,
            usd,
        });
    }

    Holdings {
        positions: positions(dexes),
        balances,
        perps_equity,
        spot_equity,
        collateral,
    }
}

/// Resting orders of one dex as rows. Spot pairs are named by their tokens.
/// `dexes` is the venue's list of dexes and `perp` this dex's markets, when
/// known: together they give each order's asset number.
pub fn orders(
    dex: &str,
    orders: &[OpenOrder],
    meta: &SpotMeta,
    dexes: &[String],
    perp: Option<&PerpMeta>,
) -> Vec<OrderRow> {
    orders
        .iter()
        .map(|o| OrderRow {
            placed_ms: o.timestamp,
            market: spot_pair_name(&o.coin, meta).unwrap_or_else(|| market_name(&o.coin, dex)),
            dex: dex.to_string(),
            kind: o.order_type.clone(),
            side: o.side,
            remaining: o.sz,
            original: o.orig_sz,
            price: o.limit_px,
            reduce_only: o.reduce_only,
            trigger: o.is_trigger.then(|| o.trigger_condition.clone()),
            oid: o.oid,
            tif: o.tif.clone(),
            amendable: o.order_type == "Limit" && !o.is_trigger && o.tif.is_some(),
            asset: assets::spot(meta, &o.coin)
                .or_else(|| perp.and_then(|perp| assets::perp(dexes, dex, perp, &o.coin))),
            trigger_px: o.is_trigger.then_some(o.trigger_px),
            tpsl: position_tpsl(o),
        })
        .collect()
}

/// Whether an order is a position's take-profit or stop-loss, and which.
/// The venue names them `Take Profit Market` and `Stop Market` (and, for a
/// limit trigger, `Take Profit Limit` and `Stop Limit`).
fn position_tpsl(o: &OpenOrder) -> Option<Tpsl> {
    if !(o.is_trigger && o.is_position_tpsl) {
        return None;
    }
    if o.order_type.starts_with("Take Profit") {
        Some(Tpsl::Tp)
    } else if o.order_type.starts_with("Stop") {
        Some(Tpsl::Sl)
    } else {
        None
    }
}

/// Newest first; ties broken by order id so the order is stable.
pub fn sort_orders(rows: &mut [OrderRow]) {
    rows.sort_by(|a, b| {
        b.placed_ms
            .cmp(&a.placed_ms)
            .then_with(|| b.oid.cmp(&a.oid))
    });
}

/// `@107` is `HYPE/USDC`. `None` when the coin is not a spot pair.
fn spot_pair_name(coin: &str, meta: &SpotMeta) -> Option<String> {
    let pair = meta.universe.iter().find(|p| p.name == coin)?;
    let token = |index: u32| {
        meta.tokens
            .iter()
            .find(|t| t.index == index)
            .map(|t| t.name.as_str())
    };
    Some(format!(
        "{}/{}",
        token(pair.tokens[0])?,
        token(pair.tokens[1])?
    ))
}

/// Vault deposits and staking, which change slowly.
pub fn slow_equity(vaults: &[VaultEquity], staking: &DelegatorSummary) -> (Decimal, Decimal) {
    (
        vaults.iter().map(|v| v.equity).sum(),
        staking.delegated + staking.undelegated,
    )
}

/// Volume over the last 14 days the venue reports, taker and maker together.
pub fn volume_14d(fees: &UserFees) -> Decimal {
    let days = &fees.daily_user_vlm;
    days[days.len().saturating_sub(14)..]
        .iter()
        .map(|d| d.user_cross + d.user_add)
        .sum()
}

/// What the breakdown and chart cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Scope {
    /// Perpetuals, spot and vaults.
    #[default]
    All,
    Perps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Period {
    Day,
    Week,
    Month,
    #[default]
    AllTime,
}

impl Period {
    pub const ALL: [Period; 4] = [Period::Day, Period::Week, Period::Month, Period::AllTime];

    pub fn label(self) -> &'static str {
        match self {
            Period::Day => "24H",
            Period::Week => "7D",
            Period::Month => "30D",
            Period::AllTime => "All-time",
        }
    }
}

impl Scope {
    pub const ALL: [Scope; 2] = [Scope::All, Scope::Perps];

    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "Perps + Spot + Vaults",
            Scope::Perps => "Perps",
        }
    }
}

/// The venue's key for a scope and period.
fn history_key(scope: Scope, period: Period) -> &'static str {
    match (scope, period) {
        (Scope::All, Period::Day) => "day",
        (Scope::All, Period::Week) => "week",
        (Scope::All, Period::Month) => "month",
        (Scope::All, Period::AllTime) => "allTime",
        (Scope::Perps, Period::Day) => "perpDay",
        (Scope::Perps, Period::Week) => "perpWeek",
        (Scope::Perps, Period::Month) => "perpMonth",
        (Scope::Perps, Period::AllTime) => "perpAllTime",
    }
}

/// Which history the chart draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Series {
    AccountValue,
    #[default]
    Pnl,
}

/// PNL, volume and drawdown for one scope and period.
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodStats {
    /// `None` when the venue has no history for the period.
    pub pnl: Option<Decimal>,
    pub volume: Decimal,
    /// A fraction; `None` with fewer than two history points.
    pub max_drawdown: Option<Decimal>,
}

pub fn period_stats(
    history: &PortfolioHistory,
    scope: Scope,
    period: Period,
) -> Option<PeriodStats> {
    let h = history.period(history_key(scope, period))?;
    Some(PeriodStats {
        pnl: h.pnl_history.last().map(|p| p.1),
        volume: h.vlm,
        max_drawdown: max_drawdown(&h.pnl_history, &h.account_value_history),
    })
}

/// The chart's points for one series, scope and period, oldest first.
pub fn series(
    history: &PortfolioHistory,
    which: Series,
    scope: Scope,
    period: Period,
) -> &[(u64, Decimal)] {
    match history.period(history_key(scope, period)) {
        Some(h) => match which {
            Series::AccountValue => &h.account_value_history,
            Series::Pnl => &h.pnl_history,
        },
        None => &[],
    }
}

/// The account in one line, for the footer every section shows.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountSummary {
    /// What the whole account is worth: [`Equity::total`].
    pub equity: Decimal,
    /// Unrealised PNL summed over open positions.
    pub unrealised: Decimal,
    /// `unrealised` as a share of equity; `None` when equity is not positive.
    pub unrealised_share: Option<Decimal>,
    /// PNL over the last 24 hours and over all time, from the venue's history.
    /// `None` where the venue has none.
    pub pnl_day: Option<Decimal>,
    pub pnl_all_time: Option<Decimal>,
    pub positions: usize,
    /// Margin held by open positions as a share of equity; `None` when equity
    /// is not positive.
    pub margin_share: Option<Decimal>,
}

/// Summarise an account from its holdings, its equity and the venue's history.
pub fn account_summary(
    holdings: &Holdings,
    equity: &Equity,
    history: &PortfolioHistory,
) -> AccountSummary {
    let total = equity.total();
    let unrealised: Decimal = holdings.positions.iter().map(|p| p.pnl).sum();
    let margin: Decimal = holdings.positions.iter().map(|p| p.margin).sum();
    let share = |part: Decimal| (total > Decimal::ZERO).then(|| part / total);
    let pnl = |period| period_stats(history, Scope::All, period).and_then(|s| s.pnl);
    AccountSummary {
        equity: total,
        unrealised,
        unrealised_share: share(unrealised),
        pnl_day: pnl(Period::Day),
        pnl_all_time: pnl(Period::AllTime),
        positions: holdings.positions.len(),
        margin_share: share(margin),
    }
}
