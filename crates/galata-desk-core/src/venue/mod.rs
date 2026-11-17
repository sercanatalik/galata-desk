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

    /// Spot tokens and the pairs they trade in.
    pub async fn spot_meta(&self) -> Result<SpotMeta, VenueError> {
        self.ask("spotMeta", json!({ "type": "spotMeta" })).await
    }

    /// Mid prices of the main dex and of spot pairs, by the venue's coin name.
    pub async fn all_mids(&self) -> Result<HashMap<String, Decimal>, VenueError> {
        self.ask("allMids", json!({ "type": "allMids" })).await
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
