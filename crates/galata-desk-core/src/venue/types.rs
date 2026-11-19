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

/// One change of status of one of the account's orders. The venue lists a
/// row for each change, newest first, so an order that rested and then
/// filled appears twice.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalOrder {
    pub order: OpenOrder,
    /// As the venue words it: `open`, `filled`, `canceled`, `triggered`,
    /// `reduceOnlyCanceled`, `badAloPxRejected`, and others. Kept as text so
    /// a status this build has not met still decodes.
    pub status: String,
    /// Milliseconds since the epoch.
    pub status_timestamp: u64,
}

/// One funding payment of the account's, as the venue reports it. Older
/// payments come summed by day: `n_samples` says how many hourly payments
/// a row stands for.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserFundingRow {
    /// Milliseconds since the epoch: the hour, or for a summed row the day.
    pub time: u64,
    pub delta: FundingDelta,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FundingDelta {
    /// The venue's coin name: `BTC`, `xyz:NVDA`.
    pub coin: String,
    /// The payment in USDC: positive when the position received funding.
    pub usdc: Decimal,
    /// The position's signed size when it was paid: negative is short.
    pub szi: Decimal,
    /// The hourly rate, averaged over `n_samples` hours for a summed row.
    pub funding_rate: Decimal,
    /// How many hourly payments the row stands for. The venue sends `null`
    /// for an hourly row, and older answers leave the field out: both are 1.
    #[serde(default = "one", deserialize_with = "one_if_null")]
    pub n_samples: u32,
}

fn one() -> u32 {
    1
}

fn one_if_null<'de, D: serde::Deserializer<'de>>(de: D) -> Result<u32, D::Error> {
    Ok(Option::<u32>::deserialize(de)?.unwrap_or(1))
}

/// One movement of the account's money that is not funding: a deposit, a
/// withdrawal, a transfer, a vault deposit, and others.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LedgerRow {
    /// Milliseconds since the epoch.
    pub time: u64,
    /// The transaction it was settled in.
    pub hash: String,
    pub delta: LedgerDelta,
}

/// What a ledger row did, by the venue's `type`. A type this build does not
/// read is kept as `Other`, with its name and fields, so that it is still
/// listed.
#[derive(Debug, Clone, PartialEq)]
pub enum LedgerDelta {
    /// Bridged in. The venue reports no fee.
    Deposit {
        usdc: Decimal,
    },
    /// Bridged out.
    Withdraw {
        usdc: Decimal,
        fee: Decimal,
    },
    /// Between the account's spot and perpetuals balances.
    AccountClassTransfer {
        usdc: Decimal,
        to_perp: bool,
    },
    /// USDC from `user` to `destination`, on the perpetuals side.
    InternalTransfer {
        usdc: Decimal,
        user: String,
        destination: String,
        fee: Decimal,
    },
    /// USDC between an account and one of its sub-accounts.
    SubAccountTransfer {
        usdc: Decimal,
        user: String,
        destination: String,
    },
    /// A token from `user` to `destination`, possibly between dexes.
    Send {
        user: String,
        destination: String,
        source_dex: String,
        destination_dex: String,
        token: String,
        amount: Decimal,
        usdc_value: Decimal,
        fee: Decimal,
    },
    /// A spot token from `user` to `destination`.
    SpotTransfer {
        user: String,
        destination: String,
        token: String,
        amount: Decimal,
        usdc_value: Decimal,
        fee: Decimal,
    },
    VaultCreate {
        vault: String,
        usdc: Decimal,
        fee: Decimal,
    },
    VaultDeposit {
        vault: String,
        usdc: Decimal,
    },
    /// `net_withdrawn_usd` is what reached the account, after the leader's
    /// commission and the cost of closing positions.
    VaultWithdraw {
        vault: String,
        requested_usd: Decimal,
        net_withdrawn_usd: Decimal,
    },
    VaultDistribution {
        vault: String,
        usdc: Decimal,
    },
    Other {
        kind: String,
        fields: serde_json::Value,
    },
}

impl<'de> Deserialize<'de> for LedgerDelta {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(
            tag = "type",
            rename_all = "camelCase",
            rename_all_fields = "camelCase"
        )]
        enum Known {
            Deposit {
                usdc: Decimal,
            },
            Withdraw {
                usdc: Decimal,
                #[serde(default)]
                fee: Decimal,
            },
            AccountClassTransfer {
                usdc: Decimal,
                to_perp: bool,
            },
            InternalTransfer {
                usdc: Decimal,
                user: String,
                destination: String,
                #[serde(default)]
                fee: Decimal,
            },
            SubAccountTransfer {
                usdc: Decimal,
                user: String,
                destination: String,
            },
            Send {
                user: String,
                destination: String,
                #[serde(default)]
                source_dex: String,
                #[serde(default)]
                destination_dex: String,
                token: String,
                amount: Decimal,
                usdc_value: Decimal,
                #[serde(default)]
                fee: Decimal,
            },
            SpotTransfer {
                user: String,
                destination: String,
                token: String,
                amount: Decimal,
                usdc_value: Decimal,
                #[serde(default)]
                fee: Decimal,
            },
            VaultCreate {
                vault: String,
                usdc: Decimal,
                #[serde(default)]
                fee: Decimal,
            },
            VaultDeposit {
                vault: String,
                usdc: Decimal,
            },
            VaultWithdraw {
                vault: String,
                requested_usd: Decimal,
                net_withdrawn_usd: Decimal,
            },
            VaultDistribution {
                vault: String,
                usdc: Decimal,
            },
        }
        let value = serde_json::Value::deserialize(deserializer)?;
        let kind = value
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        // A known type whose fields do not read is kept as `Other` too:
        // listed under its own name rather than lost.
        Ok(match serde_json::from_value::<Known>(value.clone()) {
            Ok(Known::Deposit { usdc }) => LedgerDelta::Deposit { usdc },
            Ok(Known::Withdraw { usdc, fee }) => LedgerDelta::Withdraw { usdc, fee },
            Ok(Known::AccountClassTransfer { usdc, to_perp }) => {
                LedgerDelta::AccountClassTransfer { usdc, to_perp }
            }
            Ok(Known::InternalTransfer {
                usdc,
                user,
                destination,
                fee,
            }) => LedgerDelta::InternalTransfer {
                usdc,
                user,
                destination,
                fee,
            },
            Ok(Known::SubAccountTransfer {
                usdc,
                user,
                destination,
            }) => LedgerDelta::SubAccountTransfer {
                usdc,
                user,
                destination,
            },
            Ok(Known::Send {
                user,
                destination,
                source_dex,
                destination_dex,
                token,
                amount,
                usdc_value,
                fee,
            }) => LedgerDelta::Send {
                user,
                destination,
                source_dex,
                destination_dex,
                token,
                amount,
                usdc_value,
                fee,
            },
            Ok(Known::SpotTransfer {
                user,
                destination,
                token,
                amount,
                usdc_value,
                fee,
            }) => LedgerDelta::SpotTransfer {
                user,
                destination,
                token,
                amount,
                usdc_value,
                fee,
            },
            Ok(Known::VaultCreate { vault, usdc, fee }) => {
                LedgerDelta::VaultCreate { vault, usdc, fee }
            }
            Ok(Known::VaultDeposit { vault, usdc }) => LedgerDelta::VaultDeposit { vault, usdc },
            Ok(Known::VaultWithdraw {
                vault,
                requested_usd,
                net_withdrawn_usd,
            }) => LedgerDelta::VaultWithdraw {
                vault,
                requested_usd,
                net_withdrawn_usd,
            },
            Ok(Known::VaultDistribution { vault, usdc }) => {
                LedgerDelta::VaultDistribution { vault, usdc }
            }
            Err(_) => LedgerDelta::Other {
                kind,
                fields: value,
            },
        })
    }
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
    /// What the fill did to the position: `Open Long`, `Close Short`, and
    /// others the venue names (`Buy`, `Sell` for spot).
    #[serde(default)]
    pub dir: String,
    /// The fill crossed the book: the account was the taker.
    #[serde(default)]
    pub crossed: bool,
    /// The order the fill belongs to.
    #[serde(default)]
    pub oid: u64,
    /// The transaction the fill was settled in.
    #[serde(default)]
    pub hash: String,
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

/// What one account can do in one market now, as the venue works it out:
/// its leverage setting, and how much it could buy or sell.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveAsset {
    pub leverage: Leverage,
    /// The largest order size, in the coin, for a buy and for a sell.
    pub max_trade_szs: [Decimal; 2],
    /// The margin, in USD, behind a buy and behind a sell.
    pub available_to_trade: [Decimal; 2],
    pub mark_px: Decimal,
}

impl ActiveAsset {
    /// The largest order size on one side.
    pub fn max_size(&self, buy: bool) -> Decimal {
        self.max_trade_szs[if buy { 0 } else { 1 }]
    }

    /// The margin available on one side.
    pub fn available(&self, buy: bool) -> Decimal {
        self.available_to_trade[if buy { 0 } else { 1 }]
    }
}
