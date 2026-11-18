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
    /// When each query was asked, by the test clock.
    asked_at: Arc<Mutex<Vec<tokio::time::Instant>>>,
    /// When set, the venue lists this many builder dexes, as the test
    /// network does, in place of the fixture's two.
    dexes: Arc<Mutex<Option<usize>>>,
    /// Queries answered with a failure, by name (`meta:xyz`).
    refused: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            mood: Arc::new(Mutex::new(Mood::Fine)),
            asked: Arc::default(),
            asked_at: Arc::default(),
            dexes: Arc::default(),
            refused: Arc::default(),
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
        let name = match dex {
            Some(dex) => format!("{kind}:{dex}"),
            None => kind.to_string(),
        };
        let refused = self.refused.lock().unwrap().contains(&name);
        self.asked.lock().unwrap().push(name);
        if refused {
            return Err(VenueError::Unreachable("refused for the test".into()));
        }
        self.asked_at
            .lock()
            .unwrap()
            .push(tokio::time::Instant::now());
        if kind == "perpDexs"
            && let Some(count) = *self.dexes.lock().unwrap()
        {
            let names: Vec<String> = (0..count)
                .map(|i| format!(r#"{{"name":"d{i}","fullName":"dex {i}"}}"#))
                .collect();
            return Ok(format!("[null,{}]", names.join(",")).into_bytes());
        }
        match *self.mood.lock().unwrap() {
            Mood::Down => return Err(VenueError::Unreachable("no connection".into())),
            Mood::RateLimiting => return Err(VenueError::RateLimited),
            Mood::Fine => {}
        }
        // The account is on the main dex and on `xyz`; `flx` is empty.
        let file = match (kind, dex) {
            ("clearinghouseState", Some("xyz")) => "clearinghouseState_xyz".to_string(),
            ("metaAndAssetCtxs", Some("xyz")) => "metaAndAssetCtxs_xyz".to_string(),
            ("meta", Some("xyz")) => "meta_xyz".to_string(),
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
async fn orders_learn_their_markets_numbers_from_one_question_per_dex() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_orders(true).await;
    // Every order is a plain SOL limit order: the third market on the main dex.
    for order in r.snapshot().orders.data.as_ref().unwrap() {
        assert_eq!(order.asset.map(|a| a.number), Some(2));
        assert!(order.amendable);
    }
    // Only the dex with orders was asked for its markets, and only once.
    r.refresh_orders(true).await;
    assert_eq!(fake.count("meta"), 1);
    assert_eq!(fake.count("meta:xyz"), 0);
    assert_eq!(fake.count("meta:flx"), 0);
}

#[tokio::test]
async fn positions_learn_their_markets_numbers_from_one_question_per_dex() {
    let fake = Fake::new();
    fake.refused.lock().unwrap().push("meta:xyz".into());
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_holdings(true).await;
    let nvda = |r: &Refresher<Fake>| {
        r.snapshot()
            .holdings
            .data
            .as_ref()
            .unwrap()
            .positions
            .iter()
            .find(|p| p.market == "NVDA")
            .unwrap()
            .asset
    };
    // The `xyz` list could not be had: its position shows with no number.
    assert_eq!(nvda(&r), None);

    fake.refused.lock().unwrap().clear();
    r.refresh_holdings(false).await;
    assert_eq!(nvda(&r).map(|a| a.number), Some(110_000));

    // Each dex with a position was asked once; `flx`, empty, never.
    r.refresh_holdings(true).await;
    assert_eq!(fake.count("meta"), 1);
    assert_eq!(fake.count("meta:xyz"), 2);
    assert_eq!(fake.count("meta:flx"), 0);
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
async fn after_an_action_positions_and_orders_are_fetched_at_once() {
    let fake = Fake::new();
    let feed = Feed::start(fake.clone(), USER);
    tokio::time::sleep(Duration::from_millis(100)).await;
    fake.clear();

    feed.refresh_after_action();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.count("clearinghouseState"), 1);
    assert_eq!(fake.count("frontendOpenOrders"), 1);
    // Not the slower sections, and not a sweep of every dex.
    assert_eq!(fake.count("portfolio"), 0);
    assert_eq!(fake.count("clearinghouseState:flx"), 0);
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

#[tokio::test]
async fn a_watched_market_on_a_dex_with_no_position_gets_a_rate() {
    use std::collections::BTreeSet;
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_holdings(true).await;
    // Watching `flx`, where the account holds nothing, and `xyz`, where it does.
    assert!(r.watch_dexes(BTreeSet::from(["flx".to_string(), "xyz".to_string()])));
    r.refresh_rates().await;
    // The watched-only dex is asked, and a dex both held and watched once.
    assert_eq!(fake.count("metaAndAssetCtxs:flx"), 1);
    assert_eq!(fake.count("metaAndAssetCtxs:xyz"), 1);
    assert_eq!(fake.count("metaAndAssetCtxs:"), 1);
    assert!(
        r.snapshot()
            .rates
            .data
            .as_ref()
            .unwrap()
            .contains_key("xyz:NVDA")
    );
    // Saying the same again changes nothing.
    assert!(!r.watch_dexes(BTreeSet::from(["flx".to_string(), "xyz".to_string()])));
}

#[tokio::test(start_paused = true)]
async fn the_feed_fetches_rates_at_once_when_the_watched_dexes_change() {
    use std::collections::BTreeSet;
    let fake = Fake::new();
    let feed = Feed::start(fake.clone(), USER);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.count("metaAndAssetCtxs:flx"), 0);

    feed.watch_dexes(BTreeSet::from(["flx".to_string()]));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.count("metaAndAssetCtxs:flx"), 1);
    // The same list again asks nothing more.
    feed.watch_dexes(BTreeSet::from(["flx".to_string()]));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.count("metaAndAssetCtxs:flx"), 1);
    // And it stays in the regular refresh.
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert_eq!(fake.count("metaAndAssetCtxs:flx"), 2);
}

// ---- A network with hundreds of dexes -------------------------------------

#[tokio::test(start_paused = true)]
async fn a_sweep_of_hundreds_of_dexes_goes_out_in_batches() {
    let fake = Fake::new();
    // The test network lists hundreds of dexes; the main one about a dozen.
    *fake.dexes.lock().unwrap() = Some(284);
    let mut r = Refresher::new(fake.clone(), USER);
    assert!(!r.many_dexes(), "not known until the list has been read");
    r.refresh_holdings(true).await;
    assert!(
        r.snapshot().holdings.data.is_some(),
        "{:?}",
        r.snapshot().holdings.error
    );
    assert!(r.many_dexes());

    // Every dex was asked, the main one included.
    let asked = fake.asked.lock().unwrap().clone();
    let states = asked
        .iter()
        .filter(|a| a.starts_with("clearinghouseState"))
        .count();
    assert_eq!(states, 285);

    // But never more than a batch at one moment.
    let times = fake.asked_at.lock().unwrap().clone();
    let mut at_once = std::collections::HashMap::new();
    for (name, at) in asked.iter().zip(&times) {
        if name.starts_with("clearinghouseState") {
            *at_once.entry(*at).or_insert(0usize) += 1;
        }
    }
    assert!(at_once.len() >= 9, "{} batches", at_once.len());
    assert!(at_once.values().all(|n| *n <= 32), "{:?}", at_once.values());
    // And it is not repeated every minute.
    assert_eq!(r.sweep_interval(), Duration::from_secs(600));
}

#[tokio::test(start_paused = true)]
async fn with_hundreds_of_dexes_orders_are_asked_only_where_the_account_is() {
    let fake = Fake::new();
    *fake.dexes.lock().unwrap() = Some(284);
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_holdings(true).await;
    fake.clear();
    r.refresh_orders(true).await;
    assert!(r.snapshot().orders.data.is_some());
    let asked = fake.asked.lock().unwrap().clone();
    let orders: Vec<&String> = asked
        .iter()
        .filter(|a| a.starts_with("frontendOpenOrders"))
        .collect();
    // The main dex only: the account has nothing on any of the others.
    assert_eq!(
        orders,
        ["frontendOpenOrders"],
        "{} order queries",
        orders.len()
    );
}

#[tokio::test]
async fn a_dozen_dexes_are_still_swept_at_once_and_every_minute() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_holdings(true).await;
    assert!(!r.many_dexes());
    assert_eq!(r.sweep_interval(), Duration::from_secs(60));
    // The orders sweep still covers every dex.
    fake.clear();
    r.refresh_orders(true).await;
    assert_eq!(fake.count("frontendOpenOrders:flx"), 1);
}

// ---- Fills ----------------------------------------------------------------

#[tokio::test]
async fn the_accounts_fills_arrive_and_a_failed_refresh_keeps_them() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    assert!(r.snapshot().fills.is_loading());
    r.refresh_fills().await;
    let fills = r.snapshot().fills.data.clone().expect("fills arrived");
    assert_eq!(fake.count("userFills"), 1);
    assert_eq!(fills.rows.len(), 6);
    // A handful is all there are: the venue did not cut the answer short.
    assert!(!fills.capped);
    // A builder-dex fill keeps its market's full name.
    assert!(fills.rows.iter().any(|f| f.coin == "xyz:NVDA"));

    fake.set(Mood::Down);
    r.refresh_fills().await;
    assert!(r.snapshot().fills.is_stale());
    assert_eq!(r.snapshot().fills.data.as_ref(), Some(&fills));
    fake.set(Mood::Fine);
    r.refresh_fills().await;
    assert!(!r.snapshot().fills.is_stale());
}

#[tokio::test(start_paused = true)]
async fn the_walk_and_the_latest_fills_merge_without_repeats() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_fills().await;
    let before = r.snapshot().fills.data.clone().unwrap();
    assert!(!before.walked, "the latest fills alone are not the history");

    // The walk's page holds three of the same six fills.
    let walked = r.fill_walk().await;
    assert!(r.finish_fill_walk(walked));
    assert_eq!(fake.count("userFillsByTime"), 1);
    let fills = r.snapshot().fills.data.clone().unwrap();
    assert!(fills.walked);
    assert_eq!(fills.rows.len(), 6);
    assert!(fills.rows.windows(2).all(|w| w[0].time_ms <= w[1].time_ms));
    assert_eq!(fills.reaches_from(), None);

    // A later top-up adds nothing it already holds.
    r.refresh_fills().await;
    assert_eq!(r.snapshot().fills.data.as_ref().unwrap().rows.len(), 6);
}

#[tokio::test(start_paused = true)]
async fn a_failed_walk_keeps_the_latest_fills_and_a_failed_top_up_keeps_the_history() {
    let fake = Fake::new();
    let mut r = Refresher::new(fake.clone(), USER);
    r.refresh_fills().await;
    fake.set(Mood::Down);
    let walked = r.fill_walk().await;
    assert!(!r.finish_fill_walk(walked));
    let fills = r.snapshot().fills.data.clone().unwrap();
    assert_eq!(fills.rows.len(), 6);
    assert!(!fills.walked);

    fake.set(Mood::Fine);
    let walked = r.fill_walk().await;
    assert!(r.finish_fill_walk(walked));
    let history = r.snapshot().fills.data.clone().unwrap();
    fake.set(Mood::Down);
    r.refresh_fills().await;
    assert!(r.snapshot().fills.is_stale());
    assert_eq!(r.snapshot().fills.data.as_ref(), Some(&history));
}

#[tokio::test(start_paused = true)]
async fn the_feed_walks_the_fills_once_at_start_and_again_after_a_failure() {
    let fake = Fake::new();
    fake.set(Mood::Down);
    let feed = Feed::start(fake.clone(), USER);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(fake.count("userFillsByTime"), 1);
    fake.set(Mood::Fine);
    // Tried again a minute after failing, and not again once it succeeds.
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert_eq!(fake.count("userFillsByTime"), 2);
    let snapshots = feed.snapshots();
    assert!(
        snapshots
            .borrow()
            .fills
            .data
            .as_ref()
            .is_some_and(|f| f.walked)
    );
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert_eq!(fake.count("userFillsByTime"), 2);
}
