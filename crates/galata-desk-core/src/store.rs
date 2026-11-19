//! What the app keeps between launches: which account it shows, and its
//! configuration. Never a key, a seed phrase or a wallet session.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::connect::Address;
use crate::market::MarketId;
use crate::market::watchlist::Watchlist;
use crate::network::Network;

/// The environment variable that names the WalletConnect relay project.
pub const PROJECT_ID_ENV: &str = "GALATA_DESK_REOWN_PROJECT_ID";

const ACCOUNT_FILE: &str = "account.json";
const CONFIG_FILE: &str = "config.toml";
const WATCHLIST_FILE: &str = "watchlist.json";
const SETTINGS_FILE: &str = "settings.json";

/// The connected account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub address: Address,
    /// When ownership was proven.
    pub proven_at: OffsetDateTime,
}

impl Account {
    /// An account whose ownership was proven just now.
    pub fn proven_now(address: Address) -> Self {
        Self {
            address,
            proven_at: OffsetDateTime::now_utc(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct AccountRecord {
    address: String,
    #[serde(with = "time::serde::rfc3339")]
    proven_at: OffsetDateTime,
}

/// The watchlist on disk: tickers and dex names, in order, and nothing else.
#[derive(Serialize, Deserialize)]
struct WatchlistRecord {
    v: u32,
    markets: Vec<WatchedMarket>,
    /// The market the panel shows. Absent in files written before there was a
    /// panel, which read as "the first market".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected: Option<WatchedMarket>,
}

#[derive(Serialize, Deserialize)]
struct WatchedMarket {
    dex: String,
    ticker: String,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    reown_project_id: Option<String>,
}

/// What the user has chosen in settings. No secret is ever part of it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    pub network: Network,
    /// Order actions are allowed on the main network. Off unless the user
    /// turns it on; the test network needs no such permission.
    pub allow_mainnet_trading: bool,
    /// The order ticket's Quick buttons, the same on both networks.
    pub quick_sizes: QuickSizes,
}

/// The order ticket's quick sizes: one to four whole US dollar amounts, each
/// at least the venue's $10 minimum, no two alike, smallest first. Only
/// [`QuickSizes::check`] and [`QuickSizes::from_amounts`] make one, so a list
/// that breaks a rule never reaches the ticket or the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickSizes(Vec<u64>);

/// Why a quick size field is not usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickSizeRule {
    /// Not a whole number of dollars.
    NotWhole,
    /// Under the venue's $10 minimum.
    BelowMinimum,
    /// The same amount as another field. Every field holding it says so.
    Duplicate,
}

impl QuickSizeRule {
    /// What the field says under it.
    pub fn message(self) -> &'static str {
        match self {
            Self::NotWhole => "Whole dollars only.",
            Self::BelowMinimum => "At least $10.",
            Self::Duplicate => "Already listed.",
        }
    }
}

impl QuickSizes {
    /// The venue's smallest order value, in dollars.
    pub const MINIMUM: u64 = 10;
    /// The most quick sizes the ticket shows.
    pub const MOST: usize = 4;
    /// The amounts a first launch starts with.
    pub const DEFAULT: [u64; 3] = [5_000, 10_000, 25_000];

    /// Check the Settings fields, in their order. On success, the list sorted
    /// ascending; otherwise one result per field, in the fields' order, with
    /// the rule each field breaks. A list of no fields or more than
    /// [`Self::MOST`] fails with no field to blame.
    pub fn check(texts: &[&str]) -> Result<Self, Vec<Option<QuickSizeRule>>> {
        let parsed: Vec<Option<u64>> = texts.iter().map(|t| whole_dollars(t)).collect();
        let results: Vec<Result<u64, QuickSizeRule>> = parsed
            .iter()
            .map(|amount| {
                let amount = amount.ok_or(QuickSizeRule::NotWhole)?;
                if amount < Self::MINIMUM {
                    return Err(QuickSizeRule::BelowMinimum);
                }
                if parsed.iter().filter(|a| **a == Some(amount)).count() > 1 {
                    return Err(QuickSizeRule::Duplicate);
                }
                Ok(amount)
            })
            .collect();
        let count_ok = (1..=Self::MOST).contains(&texts.len());
        if count_ok && results.iter().all(Result::is_ok) {
            let mut amounts: Vec<u64> = results.into_iter().flatten().collect();
            amounts.sort_unstable();
            Ok(Self(amounts))
        } else {
            Err(results.into_iter().map(Result::err).collect())
        }
    }

    /// A list from stored amounts, when it keeps every rule.
    pub fn from_amounts(amounts: &[u64]) -> Option<Self> {
        let texts: Vec<String> = amounts.iter().map(u64::to_string).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        Self::check(&texts).ok()
    }

    /// The amounts in whole dollars, smallest first.
    pub fn amounts(&self) -> &[u64] {
        &self.0
    }

    /// A button's label for `amount`: `$500`, `$5k`, `$2.5k`, and in full
    /// (`$1,250`) when thousands would round it.
    pub fn label(amount: u64) -> String {
        if amount < 1_000 {
            format!("${amount}")
        } else if amount.is_multiple_of(1_000) {
            format!("${}k", amount / 1_000)
        } else if amount.is_multiple_of(100) {
            format!("${}.{}k", amount / 1_000, (amount % 1_000) / 100)
        } else {
            let digits = amount.to_string();
            let mut out = String::new();
            for (i, c) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(c);
            }
            format!("${out}")
        }
    }
}

impl Default for QuickSizes {
    fn default() -> Self {
        Self(Self::DEFAULT.to_vec())
    }
}

/// A field's text as whole dollars: digits, with an optional leading `$`
/// and thousands commas.
fn whole_dollars(text: &str) -> Option<u64> {
    let text = text.trim();
    let text = text.strip_prefix('$').unwrap_or(text).replace(',', "");
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[derive(Serialize, Deserialize)]
struct SettingsRecord {
    v: u32,
    network: Network,
    /// Absent in files written before the switch existed: off.
    #[serde(default)]
    mainnet_trading: bool,
    /// Absent in files written before quick sizes existed: the default. A
    /// value that is not a list of whole amounts is dropped on its own, so it
    /// never costs the network or the mainnet switch.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "amounts_or_none"
    )]
    quick_sizes: Option<Vec<u64>>,
}

fn amounts_or_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<u64>>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

/// The app's directory on disk.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// `~/Library/Application Support/galata-desk` on macOS.
    pub fn default_location() -> Option<Self> {
        dirs::data_dir().map(|dir| Self::at(dir.join("galata-desk")))
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The remembered account. A record that is missing or unreadable is no
    /// account: the app asks to connect again instead of guessing.
    pub fn account(&self) -> Option<Account> {
        let text = std::fs::read_to_string(self.dir.join(ACCOUNT_FILE)).ok()?;
        let record: AccountRecord = serde_json::from_str(&text).ok()?;
        Some(Account {
            address: Address::parse(&record.address)?,
            proven_at: record.proven_at,
        })
    }

    /// Remember `account`. The file is written whole under another name and
    /// renamed into place, readable by its owner only.
    pub fn remember(&self, account: &Account) -> std::io::Result<()> {
        let record = AccountRecord {
            address: account.address.as_str().to_string(),
            proven_at: account.proven_at,
        };
        let text = serde_json::to_string_pretty(&record).expect("the record serialises");
        self.write_whole(ACCOUNT_FILE, &text)
    }

    /// Forget the account. Forgetting what was never remembered is not an error.
    pub fn forget(&self) -> std::io::Result<()> {
        match std::fs::remove_file(self.dir.join(ACCOUNT_FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The remembered watchlist. `None` when none was ever saved or the file
    /// cannot be read: the caller starts from the default. An empty list is a
    /// remembered choice, and comes back as an empty list.
    pub fn watchlist(&self) -> Option<Watchlist> {
        let text = std::fs::read_to_string(self.dir.join(WATCHLIST_FILE)).ok()?;
        let record: WatchlistRecord = serde_json::from_str(&text).ok()?;
        let id = |m: &WatchedMarket| MarketId::from_parts(&m.dex, &m.ticker);
        (record.v == 1).then(|| {
            Watchlist::with_selection(
                record.markets.iter().map(id),
                record.selected.as_ref().map(id),
            )
        })
    }

    /// Remember the watchlist and its order.
    pub fn save_watchlist(&self, watchlist: &Watchlist) -> std::io::Result<()> {
        let watched = |m: &MarketId| WatchedMarket {
            dex: m.dex().to_string(),
            ticker: m.ticker().to_string(),
        };
        let record = WatchlistRecord {
            v: 1,
            markets: watchlist.markets().iter().map(watched).collect(),
            selected: watchlist.selected().map(watched),
        };
        let text = serde_json::to_string_pretty(&record).expect("the record serialises");
        self.write_whole(WATCHLIST_FILE, &text)
    }

    /// The app's settings. A missing or unreadable file gives the defaults:
    /// the main network.
    pub fn settings(&self) -> Settings {
        std::fs::read_to_string(self.dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<SettingsRecord>(&text).ok())
            .filter(|record| record.v == 1)
            .map(|record| Settings {
                network: record.network,
                allow_mainnet_trading: record.mainnet_trading,
                // A missing list, or one that breaks a rule, is the default.
                quick_sizes: record
                    .quick_sizes
                    .as_deref()
                    .and_then(QuickSizes::from_amounts)
                    .unwrap_or_default(),
            })
            .unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &Settings) -> std::io::Result<()> {
        let record = SettingsRecord {
            v: 1,
            network: settings.network,
            mainnet_trading: settings.allow_mainnet_trading,
            quick_sizes: Some(settings.quick_sizes.amounts().to_vec()),
        };
        let text = serde_json::to_string_pretty(&record).expect("the record serialises");
        self.write_whole(SETTINGS_FILE, &text)
    }

    /// Write a file under another name and rename it into place, readable by
    /// its owner only, so a reader never sees half of it.
    fn write_whole(&self, name: &str, text: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("{name}.tmp"));
        {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, self.dir.join(name))
    }

    /// The WalletConnect relay project id: the environment variable if set,
    /// otherwise `reown_project_id` in `config.toml`. It is not a secret;
    /// every dapp ships its own in its page.
    pub fn project_id(&self) -> Option<String> {
        self.project_id_with(std::env::var(PROJECT_ID_ENV).ok())
    }

    fn project_id_with(&self, env: Option<String>) -> Option<String> {
        let from_file = || {
            let text = std::fs::read_to_string(self.dir.join(CONFIG_FILE)).ok()?;
            toml::from_str::<ConfigFile>(&text).ok()?.reown_project_id
        };
        env.or_else(from_file)
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "0xdfc24b077bc1425ad1dea75bcb6f8158e10df303";

    fn account() -> Account {
        Account {
            address: Address::parse(ADDRESS).unwrap(),
            proven_at: OffsetDateTime::from_unix_timestamp(1_794_888_000).unwrap(),
        }
    }

    #[test]
    fn an_account_is_remembered_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("nested"));
        assert_eq!(store.account(), None);
        store.remember(&account()).unwrap();
        assert_eq!(store.account(), Some(account()));
    }

    #[cfg(unix)]
    #[test]
    fn the_record_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store.remember(&account()).unwrap();
        let mode = std::fs::metadata(dir.path().join(ACCOUNT_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // No temporary file is left behind.
        assert!(!dir.path().join(format!("{ACCOUNT_FILE}.tmp")).exists());
    }

    #[test]
    fn the_record_holds_an_address_and_a_time_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store.remember(&account()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(ACCOUNT_FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["address", "proven_at"]);
    }

    #[test]
    fn forgetting_removes_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store.remember(&account()).unwrap();
        store.forget().unwrap();
        assert_eq!(store.account(), None);
        // Forgetting twice is fine.
        store.forget().unwrap();
    }

    #[test]
    fn an_unreadable_record_is_no_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        for bad in [
            "",
            "not json",
            r#"{"address":"0x1234","proven_at":"2026-11-17T00:00:00Z"}"#,
            r#"{"address":"0xdfc24b077bc1425ad1dea75bcb6f8158e10df303"}"#,
        ] {
            std::fs::write(dir.path().join(ACCOUNT_FILE), bad).unwrap();
            assert_eq!(store.account(), None, "{bad:?}");
        }
    }

    #[test]
    fn the_project_id_comes_from_the_environment_then_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        assert_eq!(store.project_id_with(None), None);

        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "reown_project_id = \"from-file\"\n",
        )
        .unwrap();
        assert_eq!(store.project_id_with(None).as_deref(), Some("from-file"));
        assert_eq!(
            store.project_id_with(Some("from-env".into())).as_deref(),
            Some("from-env")
        );
        // An empty value is no value.
        assert_eq!(store.project_id_with(Some("  ".into())), None);
    }

    fn watchlist(coins: &[&str]) -> Watchlist {
        Watchlist::from_markets(coins.iter().map(|c| MarketId::new(*c)))
    }

    #[test]
    fn a_watchlist_is_remembered_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        assert_eq!(store.watchlist(), None);
        let list = watchlist(&["ETH", "xyz:NVDA", "BTC"]);
        store.save_watchlist(&list).unwrap();
        assert_eq!(store.watchlist(), Some(list));
    }

    #[test]
    fn an_emptied_watchlist_is_remembered_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store.save_watchlist(&Watchlist::default()).unwrap();
        // Empty, not absent: the default must not come back.
        assert_eq!(store.watchlist(), Some(Watchlist::default()));
    }

    #[test]
    fn the_watchlist_file_holds_tickers_and_dex_names_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store.save_watchlist(&watchlist(&["xyz:NVDA"])).unwrap();
        let text = std::fs::read_to_string(dir.path().join(WATCHLIST_FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "v": 1,
                "markets": [{ "dex": "xyz", "ticker": "NVDA" }],
                "selected": { "dex": "xyz", "ticker": "NVDA" },
            })
        );
    }

    #[test]
    fn settings_default_to_the_main_network_and_are_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        // Never saved: the main network.
        assert_eq!(store.settings().network, Network::Mainnet);
        store
            .save_settings(&Settings {
                network: Network::Testnet,
                ..Settings::default()
            })
            .unwrap();
        assert_eq!(store.settings().network, Network::Testnet);
        let text = std::fs::read_to_string(dir.path().join(SETTINGS_FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "v": 1,
                "network": "testnet",
                "mainnet_trading": false,
                "quick_sizes": [5000, 10000, 25000],
            })
        );
    }

    #[test]
    fn mainnet_trading_is_off_until_switched_on_and_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        assert!(!store.settings().allow_mainnet_trading);
        // A file from before the switch existed: off, and the network kept.
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            r#"{"v":1,"network":"testnet"}"#,
        )
        .unwrap();
        assert_eq!(
            store.settings(),
            Settings {
                network: Network::Testnet,
                allow_mainnet_trading: false,
                quick_sizes: QuickSizes::default(),
            }
        );
        let on = Settings {
            network: Network::Mainnet,
            allow_mainnet_trading: true,
            ..Settings::default()
        };
        store.save_settings(&on).unwrap();
        assert_eq!(store.settings(), on);
    }

    #[test]
    fn unreadable_settings_fall_back_to_the_main_network() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        for bad in [
            "",
            "not json",
            r#"{"v":2,"network":"testnet"}"#,
            r#"{"v":1,"network":"moon"}"#,
        ] {
            std::fs::write(dir.path().join(SETTINGS_FILE), bad).unwrap();
            assert_eq!(store.settings().network, Network::Mainnet, "{bad:?}");
        }
    }

    #[test]
    fn the_selection_is_remembered_with_the_watchlist() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let mut list = watchlist(&["ETH", "xyz:NVDA", "xyz:SILVER"]);
        list.select(&MarketId::new("xyz:SILVER"));
        store.save_watchlist(&list).unwrap();
        let read = store.watchlist().unwrap();
        assert_eq!(read.selected(), Some(&MarketId::new("xyz:SILVER")));
        assert_eq!(read, list);
    }

    #[test]
    fn a_watchlist_stored_before_selections_existed_selects_its_first_market() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        std::fs::write(
            dir.path().join(WATCHLIST_FILE),
            r#"{"v":1,"markets":[{"dex":"","ticker":"ETH"},{"dex":"xyz","ticker":"NVDA"}]}"#,
        )
        .unwrap();
        let read = store.watchlist().unwrap();
        assert_eq!(read.markets().len(), 2);
        assert_eq!(read.selected(), Some(&MarketId::new("ETH")));
    }

    #[test]
    fn a_stored_selection_that_is_not_watched_gives_way_to_the_first_market() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        std::fs::write(
            dir.path().join(WATCHLIST_FILE),
            r#"{"v":1,"markets":[{"dex":"","ticker":"ETH"}],"selected":{"dex":"","ticker":"DOGE"}}"#,
        )
        .unwrap();
        assert_eq!(
            store.watchlist().unwrap().selected(),
            Some(&MarketId::new("ETH"))
        );
    }

    #[test]
    fn an_empty_watchlist_stores_no_selection() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store.save_watchlist(&Watchlist::default()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(WATCHLIST_FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value, serde_json::json!({ "v": 1, "markets": [] }));
    }

    #[test]
    fn an_unreadable_watchlist_is_no_watchlist() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        for bad in [
            "",
            "not json",
            r#"{"v":2,"markets":[]}"#,
            r#"{"markets":[]}"#,
        ] {
            std::fs::write(dir.path().join(WATCHLIST_FILE), bad).unwrap();
            assert_eq!(store.watchlist(), None, "{bad:?}");
        }
    }

    fn checked(texts: &[&str]) -> Result<Vec<u64>, Vec<Option<QuickSizeRule>>> {
        QuickSizes::check(texts).map(|list| list.amounts().to_vec())
    }

    #[test]
    fn quick_sizes_start_at_five_ten_and_twenty_five_thousand() {
        assert_eq!(QuickSizes::default().amounts(), [5_000, 10_000, 25_000]);
    }

    #[test]
    fn a_quick_size_list_is_checked_field_by_field() {
        use QuickSizeRule::*;
        // Valid lists come back sorted ascending.
        assert_eq!(
            checked(&["25000", "1000", "5000"]),
            Ok(vec![1_000, 5_000, 25_000])
        );
        assert_eq!(checked(&[" $10,000 "]), Ok(vec![10_000]));
        assert_eq!(checked(&["10"]), Ok(vec![10]));
        // One result per field, in the fields' order.
        assert_eq!(
            checked(&["5000", "2500.5", "10000"]),
            Err(vec![None, Some(NotWhole), None])
        );
        assert_eq!(checked(&["5", "5000"]), Err(vec![Some(BelowMinimum), None]));
        assert_eq!(checked(&["9"]), Err(vec![Some(BelowMinimum)]));
        assert_eq!(
            checked(&["5000", "10000", "25000", "10000"]),
            Err(vec![None, Some(Duplicate), None, Some(Duplicate)])
        );
        // Both fields say so, however the amount is written.
        assert_eq!(
            checked(&["10,000", "10000"]),
            Err(vec![Some(Duplicate), Some(Duplicate)])
        );
        for not_whole in ["", " ", "abc", "-5000", "1e4", "5k", "$"] {
            assert_eq!(
                checked(&[not_whole]),
                Err(vec![Some(NotWhole)]),
                "{not_whole:?}"
            );
        }
    }

    #[test]
    fn quick_size_labels_are_short_but_exact() {
        for (amount, label) in [
            (10, "$10"),
            (500, "$500"),
            (1_000, "$1k"),
            (2_500, "$2.5k"),
            (25_000, "$25k"),
            (1_250, "$1,250"),
            (1_234_567, "$1,234,567"),
        ] {
            assert_eq!(QuickSizes::label(amount), label);
        }
    }

    #[test]
    fn a_quick_size_list_has_one_to_four_entries() {
        assert_eq!(checked(&[]), Err(vec![]));
        assert_eq!(checked(&["10", "20", "30", "40"]), Ok(vec![10, 20, 30, 40]));
        // Five fields that are each fine: no field to blame, still refused.
        assert_eq!(checked(&["10", "20", "30", "40", "50"]), Err(vec![None; 5]));
    }

    #[test]
    fn settings_without_quick_sizes_load_the_default_and_keep_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            r#"{"v":1,"network":"testnet","mainnet_trading":true}"#,
        )
        .unwrap();
        assert_eq!(
            store.settings(),
            Settings {
                network: Network::Testnet,
                allow_mainnet_trading: true,
                quick_sizes: QuickSizes::default(),
            }
        );
    }

    #[test]
    fn a_stored_quick_size_list_that_breaks_a_rule_loads_as_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        for bad in [
            "[]",
            "[5]",
            "[5000, 5000]",
            "[10, 20, 30, 40, 50]",
            "[-5]",
            "[2500.5]",
            r#""5000""#,
        ] {
            std::fs::write(
                dir.path().join(SETTINGS_FILE),
                format!(
                    r#"{{"v":1,"network":"testnet","mainnet_trading":true,"quick_sizes":{bad}}}"#
                ),
            )
            .unwrap();
            let settings = store.settings();
            assert_eq!(settings.quick_sizes, QuickSizes::default(), "{bad}");
            // Only the list falls back: the rest is kept.
            assert_eq!(settings.network, Network::Testnet, "{bad}");
            assert!(settings.allow_mainnet_trading, "{bad}");
        }
    }

    #[test]
    fn a_saved_quick_size_list_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let settings = Settings {
            network: Network::Testnet,
            allow_mainnet_trading: false,
            quick_sizes: QuickSizes::check(&["20000", "2000"]).unwrap(),
        };
        store.save_settings(&settings).unwrap();
        assert_eq!(store.settings(), settings);
        assert_eq!(store.settings().quick_sizes.amounts(), [2_000, 20_000]);
    }

    #[test]
    fn the_record_from_before_quick_sizes_reads_a_new_file() {
        /// `SettingsRecord` as it was before quick sizes.
        #[derive(Deserialize)]
        struct OldRecord {
            v: u32,
            network: Network,
            #[serde(default)]
            mainnet_trading: bool,
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store
            .save_settings(&Settings {
                network: Network::Testnet,
                allow_mainnet_trading: true,
                quick_sizes: QuickSizes::check(&["2000"]).unwrap(),
            })
            .unwrap();
        let text = std::fs::read_to_string(dir.path().join(SETTINGS_FILE)).unwrap();
        let old: OldRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(old.v, 1);
        assert_eq!(old.network, Network::Testnet);
        assert!(old.mainnet_trading);
    }

    #[test]
    fn a_config_file_that_does_not_parse_gives_no_project_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        std::fs::write(dir.path().join(CONFIG_FILE), "reown_project_id = ").unwrap();
        assert_eq!(store.project_id_with(None), None);
    }
}
