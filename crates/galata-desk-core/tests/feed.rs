//! The refresher against a venue made of recorded answers: what it asks, how
//! often, and what it shows when the venue fails.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use galata_desk_core::feed::*;
use galata_desk_core::venue::*;
use serde_json::Value;

/// How the fake venue is behaving.
#[derive(Clone, Copy, PartialEq)]
enum Mood {
    Fine,
    Down,
    RateLimiting,
}

#[derive(Clone)]
struct Fake {
    mood: Arc<Mutex<Mood>>,
    /// Every query asked, as `type` or `type:dex`.
    asked: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            mood: Arc::new(Mutex::new(Mood::Fine)),
            asked: Arc::default(),
        }
    }
    fn set(&self, mood: Mood) {
        *self.mood.lock().unwrap() = mood;
    }
    fn count(&self, name: &str) -> usize {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|a| *a == name)
            .count()
    }
    fn clear(&self) {
        self.asked.lock().unwrap().clear();
    }
}

impl Transport for Fake {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        let kind = body["type"].as_str().unwrap();
        let dex = body.get("dex").and_then(Value::as_str);
        self.asked.lock().unwrap().push(match dex {
            Some(dex) => format!("{kind}:{dex}"),
            None => kind.to_string(),
        });
        match *self.mood.lock().unwrap() {
            Mood::Down => return Err(VenueError::Unreachable("no connection".into())),
            Mood::RateLimiting => return Err(VenueError::RateLimited),
            Mood::Fine => {}
        }
        // The account is on the main dex and on `xyz`; `flx` is empty.
        let file = match (kind, dex) {
            ("clearinghouseState", Some("xyz")) => "clearinghouseState_xyz".to_string(),
            ("metaAndAssetCtxs", Some("xyz")) => "metaAndAssetCtxs_xyz".to_string(),
            ("metaAndAssetCtxs", Some(_)) => "metaAndAssetCtxs".to_string(),
            ("clearinghouseState", Some(_)) => return Ok(EMPTY_DEX.as_bytes().to_vec()),
            ("frontendOpenOrders", Some(_)) => return Ok(b"[]".to_vec()),
            (kind, _) => kind.to_string(),
        };
        let path = format!("{}/tests/fixtures/{file}.json", env!("CARGO_MANIFEST_DIR"));
        Ok(std::fs::read(path).unwrap())
    }
}

const EMPTY_DEX: &str = r#"{"marginSummary":{"accountValue":"0.0","totalNtlPos":"0.0","totalRawUsd":"0.0","totalMarginUsed":"0.0"},"withdrawable":"0.0","assetPositions":[]}"#;
const USER: &str = "0x010461c14e146ac35fe42271bdc1134ee31c703a";

#[tokio::test]
async fn a_sweep_finds_the_dexes_the_account_is_on() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    assert!(r.snapshot().holdings.is_loading());

    r.refresh_holdings(true).await;
    let holdings = r.snapshot().holdings.data.as_ref().unwrap();
    assert_eq!(holdings.positions.len(), 8);
    assert_eq!(fake.count("clearinghouseState"), 1);
    assert_eq!(fake.count("clearinghouseState:xyz"), 1);
    assert_eq!(fake.count("clearinghouseState:flx"), 1);

    // After the sweep only the main dex and `xyz` are asked.
    fake.clear();
    r.refresh_holdings(false).await;
    assert_eq!(fake.count("clearinghouseState"), 1);
    assert_eq!(fake.count("clearinghouseState:xyz"), 1);
    assert_eq!(fake.count("clearinghouseState:flx"), 0);
    // The dex list and token list are asked once, not every time.
    assert_eq!(fake.count("perpDexs"), 0);
    assert_eq!(fake.count("spotMeta"), 0);
}

#[tokio::test]
async fn a_failure_keeps_the_last_data_and_says_why() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_holdings(true).await;
    let before = r.snapshot().holdings.clone();
    assert!(!before.is_stale());

    fake.set(Mood::Down);
    r.refresh_holdings(false).await;
    let after = &r.snapshot().holdings;
    assert!(after.is_stale());
    assert_eq!(after.data, before.data);
    assert_eq!(after.fetched_at, before.fetched_at);
    assert_eq!(
        after.error,
        Some(VenueError::Unreachable("no connection".into()))
    );
    assert_eq!(r.snapshot().failure(), after.error.as_ref());

    fake.set(Mood::Fine);
    r.refresh_holdings(false).await;
    assert!(!r.snapshot().holdings.is_stale());
    assert!(r.snapshot().holdings.fetched_at >= before.fetched_at);
}

#[tokio::test]
async fn a_first_load_that_fails_has_an_error_and_no_data() {
    let fake = Fake::new();
    fake.set(Mood::Down);
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_overview().await;
    let overview = &r.snapshot().overview;
    assert!(overview.data.is_none());
    assert!(overview.error.is_some());
    assert!(!overview.is_loading());
    assert!(!overview.is_stale());
}

#[tokio::test]
async fn rate_limiting_slows_every_tier_and_recovery_restores_it() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_overview().await;
    assert!(!r.snapshot().slowed);

    fake.set(Mood::RateLimiting);
    r.refresh_overview().await;
    assert!(r.snapshot().slowed);
    assert_eq!(
        r.cadence().interval(Tier::Holdings),
        Duration::from_secs(10)
    );
    assert_eq!(r.snapshot().overview.error, Some(VenueError::RateLimited));

    fake.set(Mood::Fine);
    r.refresh_overview().await;
    assert!(!r.snapshot().slowed);
    assert_eq!(r.cadence().interval(Tier::Holdings), Duration::from_secs(5));
}

#[tokio::test]
async fn orders_come_from_every_dex_on_a_sweep() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_orders(true).await;
    assert_eq!(r.snapshot().orders.data.as_ref().unwrap().len(), 6);
    assert_eq!(fake.count("frontendOpenOrders"), 1);
    assert_eq!(fake.count("frontendOpenOrders:xyz"), 1);
    assert_eq!(fake.count("frontendOpenOrders:flx"), 1);
}

#[tokio::test]
async fn the_overview_carries_history_fees_vaults_and_staking() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_overview().await;
    let o = r.snapshot().overview.data.as_ref().unwrap();
    assert!(o.history.period("allTime").is_some());
    assert!(o.vault_equity > rust_decimal::Decimal::ZERO);
    assert_eq!(o.staking_hype, rust_decimal::Decimal::ZERO);
}

#[tokio::test(start_paused = true)]
async fn the_feed_refreshes_each_tier_at_its_own_pace() {
    let fake = Fake::new();
    let feed = Feed::start(fake.clone(), USER);
    let mut snapshots = feed.snapshots();

    // The first pass fills every section.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let first = snapshots.borrow_and_update().clone();
    assert!(first.holdings.data.is_some());
    assert!(first.orders.data.is_some());
    assert!(first.overview.data.is_some());

    fake.clear();
    tokio::time::sleep(Duration::from_secs(60)).await;
    // In a minute: holdings every 5 s, orders every 10 s, the overview once.
    // The main dex is asked on every holdings pass.
    let holdings = fake.count("clearinghouseState");
    assert!((11..=13).contains(&holdings), "holdings passes: {holdings}");
    let orders = fake.count("frontendOpenOrders");
    assert!((5..=7).contains(&orders), "orders passes: {orders}");
    assert_eq!(fake.count("portfolio"), 1);
    // The empty dex is asked only by the once-a-minute sweep.
    assert_eq!(fake.count("clearinghouseState:flx"), 1);
}

#[tokio::test(start_paused = true)]
async fn a_manual_refresh_fetches_every_section_at_once() {
    let fake = Fake::new();
    let feed = Feed::start(fake.clone(), USER);
    tokio::time::sleep(Duration::from_millis(100)).await;
    fake.clear();

    feed.refresh_now();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.count("clearinghouseState"), 1);
    assert_eq!(fake.count("frontendOpenOrders"), 1);
    assert_eq!(fake.count("portfolio"), 1);
    // A manual refresh sweeps every dex.
    assert_eq!(fake.count("clearinghouseState:flx"), 1);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_feed_stops_the_asking() {
    let fake = Fake::new();
    let feed = Feed::start(fake.clone(), USER);
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(feed);
    tokio::time::sleep(Duration::from_millis(100)).await;
    fake.clear();
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(fake.asked.lock().unwrap().len(), 0);
}

/// A venue that reports a unified account.
#[derive(Clone)]
struct Unified(Fake);

impl Transport for Unified {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        if body["type"] == "userAbstraction" {
            self.0.asked.lock().unwrap().push("userAbstraction".into());
            return Ok(b"\"unifiedAccount\"".to_vec());
        }
        self.0.post(body).await
    }
}

#[tokio::test]
async fn the_account_mode_is_asked_with_each_sweep_and_carried_in_the_holdings() {
    use galata_desk_core::model::{Collateral, Wallet};

    let fake = Fake::new();
    let mut r = Refresher::new(Unified(fake.clone()), USER);
    r.refresh_holdings(true).await;
    let holdings = r.snapshot().holdings.data.as_ref().unwrap();
    assert_eq!(holdings.collateral, Collateral::InSpot);
    assert!(holdings.balances.iter().all(|b| b.wallet == Wallet::Spot));
    assert_eq!(fake.count("userAbstraction"), 1);

    // Between sweeps the mode is not asked again.
    r.refresh_holdings(false).await;
    assert_eq!(fake.count("userAbstraction"), 1);
    r.refresh_holdings(true).await;
    assert_eq!(fake.count("userAbstraction"), 2);
}

#[tokio::test]
async fn a_separate_account_keeps_its_perps_balance() {
    use galata_desk_core::model::{Collateral, Wallet};

    let mut r = Refresher::new(Fake::new(), USER);
    r.refresh_holdings(true).await;
    let holdings = r.snapshot().holdings.data.as_ref().unwrap();
    assert_eq!(holdings.collateral, Collateral::Separate);
    assert!(holdings.balances.iter().any(|b| b.wallet == Wallet::Perps));
}

// ---- Funding rates --------------------------------------------------------

#[tokio::test]
async fn funding_rates_are_asked_for_the_dexes_with_positions() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    // Nothing is asked until the positions say which dexes matter.
    r.refresh_rates().await;
    assert!(r.snapshot().rates.is_loading());
    assert_eq!(fake.count("metaAndAssetCtxs:"), 0);

    r.refresh_holdings(true).await;
    r.refresh_rates().await;
    let rates = r.snapshot().rates.data.clone().expect("rates arrived");
    // The account is on the main dex and on `xyz`; the empty `flx` is not asked.
    assert_eq!(fake.count("metaAndAssetCtxs:"), 1);
    assert_eq!(fake.count("metaAndAssetCtxs:xyz"), 1);
    assert_eq!(fake.count("metaAndAssetCtxs:flx"), 0);
    assert_eq!(rates["BTC"].to_string(), "0.0000125");
    assert!(
        rates.contains_key("xyz:NVDA"),
        "builder-dex markets keep their prefix"
    );
}

#[tokio::test]
async fn a_failed_refresh_keeps_the_last_funding_rates() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_holdings(true).await;
    r.refresh_rates().await;
    let before = r.snapshot().rates.data.clone();
    fake.set(Mood::Down);
    r.refresh_rates().await;
    assert!(r.snapshot().rates.is_stale());
    assert_eq!(r.snapshot().rates.data, before);
    fake.set(Mood::Fine);
    r.refresh_rates().await;
    assert!(!r.snapshot().rates.is_stale());
}
