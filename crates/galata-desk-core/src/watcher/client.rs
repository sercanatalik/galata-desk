//! Requests to the watcher's API.
//!
//! **The token goes only in the `Authorization` header, only to the saved
//! address.** Redirects are not followed, so an answer can't send it
//! elsewhere. No error carries the token or a URL: [`WatcherError`]'s text is
//! written here from its cases, never from the HTTP library's error.

use std::fmt;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, ETAG, IF_MATCH};
use reqwest::{StatusCode, redirect};

use super::types::{AlertRow, Alerts, Health, Problems, WatcherConfig};
use super::url::WatcherUrl;

/// Why a request to the watcher did not give what was asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatcherError {
    /// Nothing answered, or not in time.
    #[error("The watcher could not be reached at this address.")]
    Unreachable,
    /// 401: the token is wrong.
    #[error("The watcher refused the token.")]
    TokenRefused,
    /// 404 on `/config`: the watcher has never been configured.
    #[error("The watcher has no configuration yet.")]
    NoConfig,
    /// 412: the configuration changed since it was read.
    #[error(
        "The configuration changed elsewhere (now version {version}), so this edit was not saved."
    )]
    Changed { version: u64 },
    /// 422: the watcher named what is wrong.
    #[error("The watcher refused the configuration: {}", problems.join("; "))]
    Refused { problems: Vec<String> },
    /// Any other answer.
    #[error("The watcher answered {status}.")]
    Unexpected { status: u16 },
}

/// A connection to one watcher.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    url: WatcherUrl,
    token: String,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("url", &self.url)
            .field("token", &"<hidden>")
            .finish()
    }
}

impl Client {
    pub fn new(url: WatcherUrl, token: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .redirect(redirect::Policy::none())
            .user_agent(concat!("galata-desk/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("the HTTP client builds with static settings");
        Self {
            http,
            url,
            token: token.into().trim().to_string(),
        }
    }

    pub fn url(&self) -> &WatcherUrl {
        &self.url
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(self.url.route(path))
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
    }

    pub async fn health(&self) -> Result<Health, WatcherError> {
        let r = send(self.get("/health")).await?;
        match r.status() {
            StatusCode::OK => json(r).await,
            s => Err(refusal(s)),
        }
    }

    /// The configuration with its version.
    pub async fn config(&self) -> Result<WatcherConfig, WatcherError> {
        let r = send(self.get("/config")).await?;
        match r.status() {
            StatusCode::OK => {
                let version = etag(&r);
                let mut config: WatcherConfig = json(r).await?;
                if let Some(v) = version {
                    config.version = v;
                }
                Ok(config)
            }
            StatusCode::NOT_FOUND => Err(WatcherError::NoConfig),
            s => Err(refusal(s)),
        }
    }

    /// Alerts detected after `since_ms`, newest first.
    pub async fn alerts(&self, since_ms: u64) -> Result<Vec<AlertRow>, WatcherError> {
        let r = send(self.get(&format!("/alerts?since={since_ms}"))).await?;
        match r.status() {
            StatusCode::OK => json::<Alerts>(r).await.map(|a| a.alerts),
            s => Err(refusal(s)),
        }
    }

    /// Replace the configuration, if it is still at `config.version`.
    /// Returns the configuration as the watcher saved it.
    pub async fn put_config(&self, config: &WatcherConfig) -> Result<WatcherConfig, WatcherError> {
        let request = self
            .http
            .put(self.url.route("/config"))
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(IF_MATCH, format!("\"{}\"", config.version))
            .json(config);
        let r = send(request).await?;
        match r.status() {
            StatusCode::OK => {
                let version = etag(&r);
                let mut saved: WatcherConfig = json(r).await?;
                if let Some(v) = version {
                    saved.version = v;
                }
                Ok(saved)
            }
            StatusCode::PRECONDITION_FAILED => Err(WatcherError::Changed {
                version: etag(&r).unwrap_or_default(),
            }),
            StatusCode::UNPROCESSABLE_ENTITY => {
                let problems = json::<Problems>(r)
                    .await
                    .map(|p| p.problems)
                    .unwrap_or_default();
                Err(WatcherError::Refused { problems })
            }
            s => Err(refusal(s)),
        }
    }
}

async fn send(request: reqwest::RequestBuilder) -> Result<reqwest::Response, WatcherError> {
    // The library's error names the URL; it is dropped here, not shown.
    request.send().await.map_err(|_| WatcherError::Unreachable)
}

async fn json<T: serde::de::DeserializeOwned>(r: reqwest::Response) -> Result<T, WatcherError> {
    let status = r.status().as_u16();
    r.json()
        .await
        .map_err(|_| WatcherError::Unexpected { status })
}

fn refusal(status: StatusCode) -> WatcherError {
    match status {
        StatusCode::UNAUTHORIZED => WatcherError::TokenRefused,
        s => WatcherError::Unexpected { status: s.as_u16() },
    }
}

/// The version an `ETag: "7"` names.
fn etag(r: &reqwest::Response) -> Option<u64> {
    let raw = r.headers().get(ETAG)?.to_str().ok()?.trim();
    raw.strip_prefix("W/")
        .unwrap_or(raw)
        .trim_matches('"')
        .parse()
        .ok()
}
