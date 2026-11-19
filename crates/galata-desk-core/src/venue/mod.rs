//! Hyperliquid's public info API: unsigned queries keyed by an address.
//!
//! Everything a portfolio screen shows comes from here, and nothing here can
//! change an account: the info endpoint takes no signature and has no
//! mutating query.

mod types;

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use rust_decimal::Decimal;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub use types::*;

/// The venue's production info endpoint.
pub const INFO_URL: &str = "https://api.hyperliquid.xyz/info";

/// Why a query gave no answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VenueError {
    /// HTTP 429: the per-IP weight budget is spent.
    #[error("the venue asked the app to slow down")]
    RateLimited,
    /// Any other non-success status.
    #[error("the venue answered HTTP {0}")]
    Status(u16),
    /// The request did not complete: no network, DNS, TLS, timeout.
    #[error("the venue could not be reached: {0}")]
    Unreachable(String),
    /// The answer was not the shape this app reads. `path` names the field.
    #[error("the venue's `{query}` answer was not understood at `{path}`: {detail}")]
    Shape {
        query: &'static str,
        path: String,
        detail: String,
    },
}

/// Something that can carry one info query to the venue and bring back the
/// raw answer. The HTTP client is one; tests use a table of recorded answers.
pub trait Transport: Send + Sync + 'static {
    fn post(&self, body: Value) -> impl Future<Output = Result<Vec<u8>, VenueError>> + Send;
}

/// The real transport.
#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    url: String,
}

impl Http {
    /// A client for the production endpoint.
    pub fn new() -> Self {
        Self::at(INFO_URL)
    }

    /// A client for another endpoint (a test server).
    pub fn at(url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("galata-desk/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("the HTTP client builds with static settings");
        Self {
            client,
            url: url.into(),
        }
    }
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for Http {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        let response = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| VenueError::Unreachable(describe(&e)))?;
        match response.status().as_u16() {
            200 => response
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| VenueError::Unreachable(describe(&e))),
            429 => Err(VenueError::RateLimited),
            other => Err(VenueError::Status(other)),
        }
    }
}

/// A transport error in plain words, without the URL.
fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "the request timed out".into()
    } else if error.is_connect() {
        "no connection".into()
    } else {
        "the request failed".into()
    }
}

/// The typed queries, over any transport.
#[derive(Clone)]
pub struct Info<T> {
    transport: T,
}

impl<T: Transport> Info<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    async fn ask<R: DeserializeOwned>(
        &self,
        query: &'static str,
        body: Value,
    ) -> Result<R, VenueError> {
        let bytes = self.transport.post(body).await?;
        decode(query, &bytes)
    }

    /// The names of every perpetuals dex. The main dex is the empty name.
    pub async fn perp_dexs(&self) -> Result<Vec<String>, VenueError> {
        let dexs: Vec<Option<PerpDex>> =
            self.ask("perpDexs", json!({ "type": "perpDexs" })).await?;
        Ok(dexs
            .into_iter()
            .map(|d| d.map(|d| d.name).unwrap_or_default())
            .collect())
    }

    /// One account's perpetuals state on one dex. `dex` is empty for the main one.
    pub async fn clearinghouse_state(
        &self,
        user: &str,
        dex: &str,
    ) -> Result<ClearinghouseState, VenueError> {
        self.ask(
            "clearinghouseState",
            with_dex(json!({ "type": "clearinghouseState", "user": user }), dex),
        )
        .await
    }

    /// One account's spot balances.
    pub async fn spot_state(&self, user: &str) -> Result<SpotState, VenueError> {
        self.ask(
            "spotClearinghouseState",
            json!({ "type": "spotClearinghouseState", "user": user }),
        )
        .await
    }

    /// One account's resting orders on one dex. Spot orders come with the main dex.
    pub async fn open_orders(&self, user: &str, dex: &str) -> Result<Vec<OpenOrder>, VenueError> {
        self.ask(
            "frontendOpenOrders",
            with_dex(json!({ "type": "frontendOpenOrders", "user": user }), dex),
        )
        .await
    }

    /// The account's orders and each change of their status, newest first,
    /// across every dex. The venue returns the latest 2,000 rows and cannot
    /// be asked for older ones.
    pub async fn historical_orders(&self, user: &str) -> Result<Vec<HistoricalOrder>, VenueError> {
        self.ask(
            "historicalOrders",
            json!({ "type": "historicalOrders", "user": user }),
        )
        .await
    }

    /// The account's funding payments from `start_ms`, oldest first, across
    /// every dex: at most 500 rows per answer.
    pub async fn user_funding(
        &self,
        user: &str,
        start_ms: u64,
        end_ms: Option<u64>,
    ) -> Result<Vec<UserFundingRow>, VenueError> {
        let mut body = json!({ "type": "userFunding", "user": user, "startTime": start_ms });
        if let Some(end) = end_ms {
            body["endTime"] = json!(end);
        }
        self.ask("userFunding", body).await
    }

    /// The account's deposits, withdrawals and transfers from `start_ms`,
    /// oldest first: at most 2,000 rows per answer.
    pub async fn user_ledger(
        &self,
        user: &str,
        start_ms: u64,
        end_ms: Option<u64>,
    ) -> Result<Vec<LedgerRow>, VenueError> {
        let mut body = json!({
            "type": "userNonFundingLedgerUpdates",
            "user": user,
            "startTime": start_ms,
        });
        if let Some(end) = end_ms {
            body["endTime"] = json!(end);
        }
        self.ask("userNonFundingLedgerUpdates", body).await
    }

    /// The account's latest fills across all markets, newest first. The
    /// venue returns at most 2,000.
    pub async fn user_fills(&self, user: &str) -> Result<Vec<UserFill>, VenueError> {
        self.ask("userFills", json!({ "type": "userFills", "user": user }))
            .await
    }

    /// Account value and PNL history, and volume, per period and scope.
    pub async fn portfolio(&self, user: &str) -> Result<PortfolioHistory, VenueError> {
        let periods: Vec<(String, PeriodHistory)> = self
            .ask("portfolio", json!({ "type": "portfolio", "user": user }))
            .await?;
        Ok(PortfolioHistory { periods })
    }

    /// Fee rates and daily volume.
    pub async fn user_fees(&self, user: &str) -> Result<UserFees, VenueError> {
        self.ask("userFees", json!({ "type": "userFees", "user": user }))
            .await
    }

    /// The account's deposits in vaults.
    pub async fn vault_equities(&self, user: &str) -> Result<Vec<VaultEquity>, VenueError> {
        self.ask(
            "userVaultEquities",
            json!({ "type": "userVaultEquities", "user": user }),
        )
        .await
    }

    /// The account's staking balance.
    pub async fn delegator_summary(&self, user: &str) -> Result<DelegatorSummary, VenueError> {
        self.ask(
            "delegatorSummary",
            json!({ "type": "delegatorSummary", "user": user }),
        )
        .await
    }

    /// How the account's collateral is held, as the venue's own word for it:
    /// `unifiedAccount`, `portfolioMargin`, `disabled` or `default`.
    ///
    /// The query is not in the venue's documentation; galata-datawatch
    /// measured it (its `design/measured.md`, 2026-09-25).
    pub async fn account_mode(&self, user: &str) -> Result<String, VenueError> {
        self.ask(
            "userAbstraction",
            json!({ "type": "userAbstraction", "user": user }),
        )
        .await
    }

    /// Every perpetual market on every dex: one entry per dex, the main one
    /// first, each with its markets' names and maximum leverage.
    pub async fn all_perp_metas(&self) -> Result<Vec<PerpMeta>, VenueError> {
        self.ask("allPerpMetas", json!({ "type": "allPerpMetas" }))
            .await
    }

    /// The perpetual markets of one dex, in the venue's order: a market's
    /// place in the list is part of its asset number.
    pub async fn perp_meta(&self, dex: &str) -> Result<PerpMeta, VenueError> {
        self.ask("meta", with_dex(json!({ "type": "meta" }), dex))
            .await
    }

    /// What `address` is to the venue: an account, an API wallet acting for
    /// one, or nothing it knows.
    pub async fn user_role(&self, address: &str) -> Result<UserRole, VenueError> {
        self.ask("userRole", json!({ "type": "userRole", "user": address }))
            .await
    }

    /// The account's leverage and tradeable size in one market. `coin` is
    /// the venue's name: `HYPE`, or `xyz:NVDA` on a builder dex.
    pub async fn active_asset(&self, user: &str, coin: &str) -> Result<ActiveAsset, VenueError> {
        self.ask(
            "activeAssetData",
            json!({ "type": "activeAssetData", "user": user, "coin": coin }),
        )
        .await
    }

    /// The current hourly funding rate of every market on one dex, by the
    /// venue's coin name. `dex` is empty for the main dex.
    pub async fn funding_rates(&self, dex: &str) -> Result<HashMap<String, Decimal>, VenueError> {
        let (meta, ctxs): (PerpMeta, Vec<AssetCtx>) = self
            .ask(
                "metaAndAssetCtxs",
                json!({ "type": "metaAndAssetCtxs", "dex": dex }),
            )
            .await?;
        // The two lists are in step: the nth context is the nth market's.
        Ok(meta
            .universe
            .into_iter()
            .zip(ctxs)
            .map(|(market, ctx)| (market.name, ctx.funding))
            .collect())
    }

    /// One market's candles at one interval, oldest first, for the candles
    /// that open between `start_ms` and `end_ms`.
    pub async fn candle_snapshot(
        &self,
        coin: &str,
        interval: &str,
        start_ms: u64,
        end_ms: u64,
    ) -> Result<Vec<WireCandle>, VenueError> {
        self.ask(
            "candleSnapshot",
            json!({
                "type": "candleSnapshot",
                "req": { "coin": coin, "interval": interval, "startTime": start_ms, "endTime": end_ms },
            }),
        )
        .await
    }

    /// One market's hourly funding rates charged between `start_ms` and
    /// `end_ms`, both included, oldest first. The venue returns at most 500
    /// rows, the oldest in the range; nothing for a time before the market
    /// was listed; and `null` for a market it does not know, which is read
    /// here as nothing.
    pub async fn funding_history(
        &self,
        coin: &str,
        start_ms: u64,
        end_ms: u64,
    ) -> Result<Vec<WireFunding>, VenueError> {
        let rows: Option<Vec<WireFunding>> = self
            .ask(
                "fundingHistory",
                json!({ "type": "fundingHistory", "coin": coin, "startTime": start_ms, "endTime": end_ms }),
            )
            .await?;
        Ok(rows.unwrap_or_default())
    }

    /// The account's fills from `start_ms` on, at most 2,000 in one answer.
    /// The venue keeps only the account's 10,000 most recent fills, so a
    /// start before those returns from the earliest it still has.
    pub async fn user_fills_by_time(
        &self,
        user: &str,
        start_ms: u64,
    ) -> Result<Vec<UserFill>, VenueError> {
        self.ask(
            "userFillsByTime",
            json!({ "type": "userFillsByTime", "user": user, "startTime": start_ms }),
        )
        .await
    }

    /// Spot tokens and the pairs they trade in.
    pub async fn spot_meta(&self) -> Result<SpotMeta, VenueError> {
        self.ask("spotMeta", json!({ "type": "spotMeta" })).await
    }

    /// Mid prices of the main dex and of spot pairs, by the venue's coin name.
    pub async fn all_mids(&self) -> Result<HashMap<String, Decimal>, VenueError> {
        self.all_mids_on("").await
    }

    /// Mid prices of one perpetuals dex: the main dex (with spot) for `""`.
    /// A builder dex's are keyed `dex:COIN`, as the venue names them.
    pub async fn all_mids_on(&self, dex: &str) -> Result<HashMap<String, Decimal>, VenueError> {
        self.ask("allMids", with_dex(json!({ "type": "allMids" }), dex))
            .await
    }

    /// The mid price of `coin` on `dex`, or `None` when the venue lists no
    /// mid for it.
    pub async fn mid(&self, dex: &str, coin: &str) -> Result<Option<Decimal>, VenueError> {
        let mids = self.all_mids_on(dex).await?;
        Ok(mids.get(&mid_key(dex, coin)).copied())
    }
}

/// How `allMids` names a market: `BTC` on the main dex, `xyz:NVDA` on a
/// builder dex.
pub fn mid_key(dex: &str, coin: &str) -> String {
    if dex.is_empty() {
        coin.to_string()
    } else {
        format!("{dex}:{coin}")
    }
}

/// The main dex is asked without the key: an explicit empty string is not a
/// spelling the venue documents (galata-datawatch measured this).
fn with_dex(mut body: Value, dex: &str) -> Value {
    if !dex.is_empty() {
        body["dex"] = Value::String(dex.to_string());
    }
    body
}

/// Decode an answer, naming the field where it stops making sense. A missing
/// field is an error and never a zero: a statement quietly missing a number
/// would read as flat.
pub fn decode<R: DeserializeOwned>(query: &'static str, bytes: &[u8]) -> Result<R, VenueError> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize(&mut de).map_err(|e| VenueError::Shape {
        query,
        path: e.path().to_string(),
        detail: e.inner().to_string(),
    })
}
