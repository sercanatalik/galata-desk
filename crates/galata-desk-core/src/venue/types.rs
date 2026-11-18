//! The venue's answers, as much of them as the portfolio reads.
//!
//! Amounts arrive as decimal strings and are kept as [`Decimal`], which
//! preserves the venue's own precision (`17.308` stays `17.308`). Fields this
//! app does not read are ignored; fields it does read are required.

use rust_decimal::Decimal;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct PerpDex {
    pub name: String,
}

/// One account's perpetuals state on one dex.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearinghouseState {
    pub margin_summary: MarginSummary,
    pub withdrawable: Decimal,
    pub asset_positions: Vec<AssetPosition>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginSummary {
    pub account_value: Decimal,
    pub total_ntl_pos: Decimal,
    pub total_margin_used: Decimal,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssetPosition {
    pub position: Position,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    /// The venue's name: `ETH` on the main dex, `xyz:NVDA` on a builder dex.
    pub coin: String,
    /// Signed size: negative is short.
    pub szi: Decimal,
    pub leverage: Leverage,
    pub entry_px: Decimal,
    pub position_value: Decimal,
    pub unrealized_pnl: Decimal,
    /// A fraction: `-0.233` is -23.3%.
    pub return_on_equity: Decimal,
    /// Absent when the position cannot be liquidated at any price.
    pub liquidation_px: Option<Decimal>,
    pub margin_used: Decimal,
    pub cum_funding: CumFunding,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Leverage {
    #[serde(rename = "type")]
    pub mode: MarginMode,
    pub value: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MarginMode {
    Cross,
    Isolated,
}

/// Funding paid by the account: positive is paid, negative is received.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CumFunding {
    pub since_open: Decimal,
}

/// One account's spot balances.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpotState {
    pub balances: Vec<SpotBalance>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpotBalance {
    pub coin: String,
    /// The token's index in [`SpotMeta::tokens`].
    pub token: u32,
    pub hold: Decimal,
    pub total: Decimal,
}

/// A resting order, as the venue's own front end reads it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenOrder {
    pub coin: String,
    pub side: Side,
    pub limit_px: Decimal,
    /// What is left to fill.
    pub sz: Decimal,
    pub orig_sz: Decimal,
    pub oid: u64,
    /// Milliseconds since the epoch.
    pub timestamp: u64,
    pub order_type: String,
    pub reduce_only: bool,
    pub is_trigger: bool,
    pub trigger_condition: String,
    /// The trigger price; `0.0` for an order that is not a trigger order.
    pub trigger_px: Decimal,
    /// A position's take-profit or stop-loss, which covers the whole
    /// position whatever its size. Listed with a size of `0.0`.
    #[serde(default)]
    pub is_position_tpsl: bool,
    /// Time in force: `Gtc`, `Alo`. Absent for trigger orders.
    #[serde(default)]
    pub tif: Option<String>,
}

/// One fill of the account's, as the venue reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserFill {
    /// The venue's coin name: `BTC`, `xyz:NVDA`.
    pub coin: String,
    pub px: Decimal,
    pub sz: Decimal,
    pub side: Side,
    /// Milliseconds since the epoch.
    pub time: u64,
    /// PNL the fill realised; zero when it opened or added to a position.
    /// It does not include the fee.
    pub closed_pnl: Decimal,
    /// The fee the fill paid, in `feeToken` (USDC for perpetuals); negative
    /// for a rebate.
    pub fee: Decimal,
    /// The venue's trade id: the same fill keeps it across answers.
    pub tid: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Side {
    /// Bid.
    #[serde(rename = "B")]
    Buy,
    /// Ask.
    #[serde(rename = "A")]
    Sell,
}

/// History and volume for every period and scope the venue keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct PortfolioHistory {
    /// Keyed by the venue's names: `day`, `week`, `month`, `allTime` and the
    /// same with a `perp` prefix.
    pub periods: Vec<(String, PeriodHistory)>,
}

impl PortfolioHistory {
    pub fn period(&self, key: &str) -> Option<&PeriodHistory> {
        self.periods.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeriodHistory {
    /// `(milliseconds, account value)`, oldest first.
    pub account_value_history: Vec<(u64, Decimal)>,
    /// `(milliseconds, PNL since the period began)`, oldest first.
    pub pnl_history: Vec<(u64, Decimal)>,
    pub vlm: Decimal,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserFees {
    pub daily_user_vlm: Vec<DailyVolume>,
    /// Perpetuals taker rate, as a fraction.
    pub user_cross_rate: Decimal,
    /// Perpetuals maker rate.
    pub user_add_rate: Decimal,
    pub user_spot_cross_rate: Decimal,
    pub user_spot_add_rate: Decimal,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyVolume {
    pub date: String,
    /// Taker volume that day.
    pub user_cross: Decimal,
    /// Maker volume that day.
    pub user_add: Decimal,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VaultEquity {
    pub equity: Decimal,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DelegatorSummary {
    pub delegated: Decimal,
    pub undelegated: Decimal,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpotMeta {
    pub tokens: Vec<SpotToken>,
    pub universe: Vec<SpotPair>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotToken {
    pub name: String,
    pub index: u32,
    /// Decimal places a size in this token may have.
    pub sz_decimals: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpotPair {
    /// The venue's coin name for the pair: `PURR/USDC` or `@107`.
    pub name: String,
    /// `[base, quote]` token indexes.
    pub tokens: [u32; 2],
    /// The pair's number among spot pairs; its asset number is this plus 10,000.
    pub index: u32,
}

/// The perpetual markets of one dex.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PerpMeta {
    pub universe: Vec<PerpMarket>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerpMarket {
    /// The venue's coin name: `BTC`, `xyz:NVDA`.
    pub name: String,
    /// Decimal places a size in this market may have.
    pub sz_decimals: u32,
    pub max_leverage: u32,
    /// Present and true for a market the venue has delisted.
    #[serde(default)]
    pub is_delisted: bool,
}

/// One candle, as the venue sends it over REST and over the stream.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireCandle {
    /// Open time, milliseconds since the epoch.
    pub t: u64,
    /// The coin, as the stream names it. REST answers carry it too.
    #[serde(default)]
    pub s: String,
    /// The interval, as the venue names it.
    #[serde(default)]
    pub i: String,
    pub o: Decimal,
    pub h: Decimal,
    pub l: Decimal,
    pub c: Decimal,
}

/// One hour's funding for one market, as the venue's history reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireFunding {
    /// When the rate was charged: a few tens of milliseconds past the hour.
    pub time: u64,
    /// The hourly rate, as a fraction. Positive: longs paid shorts.
    pub funding_rate: Decimal,
}

/// A market's live figures, as the stream sends them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetCtx {
    pub mark_px: Decimal,
    /// Absent when the book has no two sides to be between.
    pub mid_px: Option<Decimal>,
    pub oracle_px: Decimal,
    /// Mark over oracle, as a fraction. The venue sends none for some markets.
    pub premium: Option<Decimal>,
    /// The hourly funding rate, as a fraction.
    pub funding: Decimal,
    /// In units of the asset.
    pub open_interest: Decimal,
    /// 24-hour volume in USD.
    pub day_ntl_vlm: Decimal,
}

/// What an address is to the venue.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UserRole {
    /// `user`, `agent`, `vault`, `subAccount` or `missing`.
    pub role: String,
    #[serde(default)]
    pub data: Option<UserRoleData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UserRoleData {
    /// For an agent (an API wallet): the account it acts for.
    #[serde(default)]
    pub user: Option<String>,
}
