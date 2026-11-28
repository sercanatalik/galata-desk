//! The watcher API's JSON, as the desk reads and writes it.
//!
//! **A write sends back what was read.** `PUT /config` replaces the whole
//! configuration, and the watcher refuses fields it doesn't know. So every
//! field the desk doesn't edit (the Telegram chat, the digest, anything a
//! newer watcher adds) is kept in `rest` exactly as read and written back
//! unchanged. `version` and a level's `generation` are the watcher's to set:
//! they are read, and never written.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::network::Network;

/// The watcher's configuration, from `GET /config`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatcherConfig {
    /// Raised by the watcher on every accepted write. Sent as `If-Match`,
    /// never in the body.
    #[serde(default, skip_serializing)]
    pub version: u64,
    /// The watched accounts, by venue and alias, as galata-markets names
    /// them. `None` watches every account galata-markets serves the watcher.
    /// The desk never sends an address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<WatchedAccount>>,
    #[serde(default)]
    pub network: Network,
    /// Percent, 2 to 50.
    pub near_liquidation_pct: Decimal,
    /// Take-profit and stop-loss fill alerts, for every ticker at once.
    #[serde(default = "on", deserialize_with = "switch")]
    pub stop_alerts: bool,
    /// The half-hourly PnL summary. `None` from a watcher that predates the
    /// setting: then it is never written, since that watcher would refuse it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pnl_summary: Option<bool>,
    /// The quarter-hourly market update. `None` from a watcher that predates
    /// it, and then never written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_update: Option<bool>,
    /// The markets the market update lists (`BTC`, `xyz:NVDA`), in order:
    /// the desk keeps it equal to its own watchlist. `None` from a watcher
    /// that predates it, and then never written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watchlist: Option<Vec<String>>,
    /// Alerts when a watched market moves `move_pct` from its pinned price.
    /// `None` from a watcher that predates them, and then never written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_alerts: Option<bool>,
    /// Percent of the pinned price, 0.5 to 10. As `move_alerts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_pct: Option<Decimal>,
    #[serde(default)]
    pub levels: Vec<Level>,
    /// Every other field, as read.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// An account the watcher watches: its venue and alias.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WatchedAccount {
    pub venue: String,
    pub alias: String,
}

impl WatchedAccount {
    pub fn new(venue: impl Into<String>, alias: impl Into<String>) -> Self {
        Self {
            venue: venue.into(),
            alias: alias.into(),
        }
    }
}

impl std::fmt::Display for WatchedAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.venue, self.alias)
    }
}

impl WatcherConfig {
    /// What the watcher watches, as a person reads it: `every account`, or
    /// `hyperliquid main, derive-xyz owner_s3`.
    pub fn watched(&self) -> String {
        match &self.accounts {
            None => "every account".into(),
            Some(named) => named
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

/// One account `GET /accounts` lists: one galata-markets serves the watcher.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ListedAccount {
    pub venue: String,
    pub alias: String,
    /// Whether the watcher watches it now.
    #[serde(default)]
    pub watched: bool,
}

impl ListedAccount {
    pub fn account(&self) -> WatchedAccount {
        WatchedAccount::new(&self.venue, &self.alias)
    }
}

/// `GET /accounts`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Accounts {
    pub accounts: Vec<ListedAccount>,
}

fn on() -> bool {
    true
}

/// `true` or `false`; an earlier watcher's per-ticker map reads as on unless
/// it switched some ticker off.
fn switch<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        One(bool),
        ByTicker(BTreeMap<String, bool>),
    }
    Ok(match Either::deserialize(d)? {
        Either::One(on) => on,
        Either::ByTicker(m) => m.values().all(|on| *on),
    })
}

impl WatcherConfig {
    /// The levels still waiting to fire in `market` (`BTC`, `xyz:NVDA`).
    pub fn armed_in<'a>(&'a self, market: &'a str) -> impl Iterator<Item = &'a Level> + 'a {
        self.levels
            .iter()
            .filter(move |l| l.market == market && l.is_armed())
    }
}

/// A call level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Level {
    /// Set by the watcher for a new level, from its market, direction and
    /// price. A level the desk adds goes without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `BTC`, or `dex:TICKER` for a builder dex's market.
    pub market: String,
    pub price: Decimal,
    pub direction: Direction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Absent on a level not yet saved. Writing `Armed` over a fired level
    /// re-arms it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<LevelState>,
    /// When it fired, milliseconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<u64>,
    /// The watcher's own count of re-arms.
    #[serde(default, skip_serializing)]
    pub generation: u64,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl Level {
    /// A new, armed level.
    pub fn new(
        market: impl Into<String>,
        price: Decimal,
        direction: Direction,
        note: Option<String>,
    ) -> Self {
        Self {
            id: None,
            market: market.into(),
            price,
            direction,
            note: note.filter(|n| !n.trim().is_empty()),
            state: None,
            fired_at: None,
            generation: 0,
            rest: Map::new(),
        }
    }

    pub fn is_armed(&self) -> bool {
        self.state != Some(LevelState::Fired)
    }

    /// The fired time, when it has fired.
    pub fn fired(&self) -> Option<u64> {
        (self.state == Some(LevelState::Fired))
            .then_some(self.fired_at)
            .flatten()
    }

    /// Ask the watcher to arm it again.
    pub fn rearm(&mut self) {
        self.state = Some(LevelState::Armed);
        self.fired_at = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Above,
    Below,
}

impl Direction {
    /// The direction a new level at `price` takes, seen from the market's
    /// `last` price: above it, or else below.
    pub fn from_last(price: Decimal, last: Decimal) -> Self {
        if price > last {
            Self::Above
        } else {
            Self::Below
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Above => "above",
            Self::Below => "below",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LevelState {
    Armed,
    Fired,
}

/// `GET /health`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Health {
    /// The watcher's last successful check, milliseconds since the epoch.
    pub last_ok_ms: Option<u64>,
    pub last_error: Option<String>,
    pub last_error_ms: Option<u64>,
    pub config_version: Option<u64>,
}

/// One row of `GET /alerts`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AlertRow {
    pub id: String,
    /// `stop_loss`, `take_profit`, `liquidation`, `level`, `digest`, …
    pub kind: String,
    #[serde(default)]
    pub market: String,
    pub event_ms: u64,
    pub detected_ms: u64,
    /// Found after the fact: the watcher was down when it happened.
    pub late: bool,
    /// The message as sent to Telegram.
    pub text: String,
    /// Telegram accepted it.
    pub delivered: bool,
}

#[derive(Deserialize)]
pub(crate) struct Alerts {
    pub alerts: Vec<AlertRow>,
}

#[derive(Deserialize)]
pub(crate) struct Problems {
    pub problems: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CONFIG: &str = include_str!("../../tests/fixtures/watcher/config.json");

    #[test]
    fn a_read_config_is_written_back_unchanged_but_for_version_and_generation() {
        let read: Value = serde_json::from_str(CONFIG).unwrap();
        let config: WatcherConfig = serde_json::from_value(read.clone()).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.levels[0].fired(), Some(1_795_127_520_000));
        assert!(config.levels[1].is_armed());
        assert!(!config.stop_alerts);

        let mut expected = read;
        expected.as_object_mut().unwrap().remove("version");
        for level in expected["levels"].as_array_mut().unwrap() {
            level.as_object_mut().unwrap().remove("generation");
        }
        assert_eq!(serde_json::to_value(&config).unwrap(), expected);
    }

    #[test]
    fn fields_the_desk_does_not_know_survive_a_round_trip() {
        let mut read: Value = serde_json::from_str(CONFIG).unwrap();
        read["quiet_hours"] = json!({"from": "23:00", "to": "07:00"});
        read["levels"][1]["colour"] = json!("violet");
        let config: WatcherConfig = serde_json::from_value(read).unwrap();
        let written = serde_json::to_value(&config).unwrap();
        assert_eq!(
            written["quiet_hours"],
            json!({"from": "23:00", "to": "07:00"})
        );
        assert_eq!(written["levels"][1]["colour"], json!("violet"));
        assert_eq!(written["telegram_chat"], json!("123456789"));
        assert_eq!(
            written["digest"],
            json!({"time": "07:00", "zone": "Europe/Istanbul"})
        );
    }

    #[test]
    fn a_new_level_goes_without_id_or_state_and_rearming_clears_the_firing() {
        let level = Level::new(
            "ETH",
            Decimal::new(3450, 0),
            Direction::Above,
            Some("  ".into()),
        );
        assert_eq!(
            serde_json::to_value(&level).unwrap(),
            json!({"market": "ETH", "price": "3450", "direction": "above"})
        );
        let mut config: WatcherConfig = serde_json::from_str(CONFIG).unwrap();
        config.levels[0].rearm();
        let written = serde_json::to_value(&config.levels[0]).unwrap();
        assert_eq!(written["state"], json!("armed"));
        assert!(written.get("fired_at").is_none());
    }

    #[test]
    fn the_pnl_summary_switch_is_written_only_when_the_watcher_has_it() {
        let read: Value = serde_json::from_str(CONFIG).unwrap();
        let config: WatcherConfig = serde_json::from_value(read.clone()).unwrap();
        assert_eq!(config.pnl_summary, Some(true));
        let mut off = config.clone();
        off.pnl_summary = Some(false);
        assert_eq!(
            serde_json::to_value(&off).unwrap()["pnl_summary"],
            json!(false)
        );

        let mut older = read;
        older.as_object_mut().unwrap().remove("pnl_summary");
        let config: WatcherConfig = serde_json::from_value(older).unwrap();
        assert_eq!(config.pnl_summary, None);
        assert!(!config.rest.contains_key("pnl_summary"));
        let written = serde_json::to_value(&config).unwrap();
        assert!(
            written.get("pnl_summary").is_none(),
            "an older watcher would refuse it"
        );
    }

    #[test]
    fn the_watchlist_and_market_update_are_written_only_when_the_watcher_has_them() {
        let mut read: Value = serde_json::from_str(CONFIG).unwrap();
        let older: WatcherConfig = serde_json::from_value(read.clone()).unwrap();
        assert_eq!((older.market_update, older.watchlist.clone()), (None, None));
        let written = serde_json::to_value(&older).unwrap();
        assert!(written.get("watchlist").is_none() && written.get("market_update").is_none());

        read["market_update"] = json!(true);
        read["watchlist"] = json!(["BTC"]);
        let mut config: WatcherConfig = serde_json::from_value(read).unwrap();
        assert_eq!(config.market_update, Some(true));
        assert!(!config.rest.contains_key("watchlist"));
        config.watchlist = Some(vec!["ETH".into(), "xyz:NVDA".into()]);
        let written = serde_json::to_value(&config).unwrap();
        assert_eq!(written["watchlist"], json!(["ETH", "xyz:NVDA"]));
        assert_eq!(written["market_update"], json!(true));
    }

    #[test]
    fn move_settings_are_written_only_when_the_watcher_has_them() {
        let mut read: Value = serde_json::from_str(CONFIG).unwrap();
        let config: WatcherConfig = serde_json::from_value(read.clone()).unwrap();
        assert_eq!(
            (config.move_alerts, config.move_pct),
            (Some(true), Some(Decimal::ONE))
        );
        let mut set = config.clone();
        set.move_alerts = Some(false);
        set.move_pct = Some(Decimal::new(25, 1));
        let written = serde_json::to_value(&set).unwrap();
        assert_eq!(
            (written["move_alerts"].clone(), written["move_pct"].clone()),
            (json!(false), json!("2.5"))
        );

        read.as_object_mut().unwrap().remove("move_alerts");
        read.as_object_mut().unwrap().remove("move_pct");
        let older: WatcherConfig = serde_json::from_value(read).unwrap();
        assert_eq!((older.move_alerts, older.move_pct), (None, None));
        assert!(!older.rest.contains_key("move_pct"));
        let written = serde_json::to_value(&older).unwrap();
        assert!(
            written.get("move_alerts").is_none() && written.get("move_pct").is_none(),
            "an older watcher would refuse them"
        );
    }

    #[test]
    fn stop_alerts_are_one_switch_on_when_absent() {
        let mut read: Value = serde_json::from_str(CONFIG).unwrap();
        let on = |read: &Value| {
            serde_json::from_value::<WatcherConfig>(read.clone())
                .unwrap()
                .stop_alerts
        };
        read.as_object_mut().unwrap().remove("stop_alerts");
        assert!(on(&read));
        read["stop_alerts"] = json!({});
        assert!(on(&read), "an earlier watcher's empty map");
        read["stop_alerts"] = json!({"SOL": false});
        assert!(!on(&read));
    }

    #[test]
    fn a_new_level_points_away_from_the_last_price() {
        let last = Decimal::new(60_000, 0);
        assert_eq!(
            Direction::from_last(Decimal::new(65_000, 0), last),
            Direction::Above
        );
        assert_eq!(
            Direction::from_last(Decimal::new(58_000, 0), last),
            Direction::Below
        );
    }

    #[test]
    fn health_and_alerts_decode() {
        let health: Health =
            serde_json::from_str(include_str!("../../tests/fixtures/watcher/health.json")).unwrap();
        assert_eq!(health.config_version, Some(1));
        assert!(health.last_error.unwrap().contains("503"));
        let alerts: Alerts =
            serde_json::from_str(include_str!("../../tests/fixtures/watcher/alerts.json")).unwrap();
        assert_eq!(alerts.alerts.len(), 2);
        assert!(alerts.alerts[0].late && !alerts.alerts[0].delivered);
        let problems: Problems =
            serde_json::from_str(include_str!("../../tests/fixtures/watcher/problems.json"))
                .unwrap();
        assert_eq!(problems.problems.len(), 2);
    }
}
