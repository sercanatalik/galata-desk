//! The watcher feed: reading on schedule, keeping stale data, and saving one
//! edit at a time with its version.

mod watcher_fake;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::watcher::{
    Client, ConfigState, Direction, Level, Problem, WatcherError, WatcherFeed, WatcherSnapshot,
    WatcherUrl,
};
use serde_json::json;
use tokio::sync::watch;
use watcher_fake::{Fake, TOKEN};

const SLOW: Duration = Duration::from_secs(3600);

async fn feed(fake: &Fake, every: Duration) -> WatcherFeed {
    let base = fake.serve().await;
    WatcherFeed::start_every(Client::new(WatcherUrl::parse(&base).unwrap(), TOKEN), every)
}

/// Wait, at most two seconds, for a snapshot that passes `ok`.
async fn until(
    rx: &mut watch::Receiver<WatcherSnapshot>,
    ok: impl Fn(&WatcherSnapshot) -> bool,
) -> WatcherSnapshot {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            {
                let s = rx.borrow_and_update();
                if ok(&s) {
                    return s.clone();
                }
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("the snapshot never got there")
}

fn reads(fake: &Fake) -> usize {
    fake.seen()
        .iter()
        .filter(|s| s.method == Method::GET && s.path == "/health")
        .count()
}

#[tokio::test]
async fn reads_at_once_then_on_schedule() {
    let fake = Fake::configured();
    let f = feed(&fake, Duration::from_millis(50)).await;
    let mut rx = f.snapshots();
    let s = until(&mut rx, |s| s.reached_ms.is_some()).await;
    assert_eq!(s.problem, None);
    assert_eq!(s.config.config().map(|c| c.version), Some(1));
    assert_eq!(s.alerts.len(), 2);
    assert!(s.health.is_some());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(reads(&fake) >= 4, "{} reads", reads(&fake));
}

#[tokio::test]
async fn a_failed_read_keeps_the_last_data_and_says_why() {
    let fake = Fake::configured();
    let f = feed(&fake, Duration::from_millis(50)).await;
    let mut rx = f.snapshots();
    let good = until(&mut rx, |s| s.reached_ms.is_some()).await;

    fake.fail_with(Some(StatusCode::UNAUTHORIZED));
    let stale = until(&mut rx, |s| s.problem.is_some()).await;
    assert_eq!(stale.problem, Some(Problem::TokenRefused));
    assert_eq!(stale.config, good.config);
    assert_eq!(stale.alerts, good.alerts);
    assert_eq!(stale.reached_ms, good.reached_ms);
    assert!(stale.editable().is_none());

    fake.fail_with(None);
    let back = until(&mut rx, |s| s.problem.is_none()).await;
    assert!(back.editable().is_some());
    assert!(back.reached_ms > good.reached_ms);
}

#[tokio::test]
async fn nothing_listening_is_unreachable_with_nothing_to_show() {
    let f = WatcherFeed::start_every(
        Client::new(WatcherUrl::parse("http://127.0.0.1:9").unwrap(), TOKEN),
        SLOW,
    );
    let mut rx = f.snapshots();
    let s = until(&mut rx, |s| s.problem.is_some()).await;
    assert_eq!(s.problem, Some(Problem::Unreachable));
    assert_eq!(s.config, ConfigState::Unknown);
    assert_eq!(s.reached_ms, None);
}

#[tokio::test]
async fn an_unconfigured_watcher_is_reached_but_has_nothing_to_edit() {
    let fake = Fake::default();
    let f = feed(&fake, SLOW).await;
    let mut rx = f.snapshots();
    let s = until(&mut rx, |s| s.reached_ms.is_some()).await;
    assert_eq!(s.config, ConfigState::Missing);
    assert!(s.editable().is_none());
    assert_eq!(
        f.save(|c| c.near_liquidation_pct = Decimal::TEN)
            .await
            .map(|_| ()),
        Err(WatcherError::NoConfig)
    );
    assert!(fake.writes().is_empty());
}

#[tokio::test]
async fn two_quick_saves_both_land_in_order() {
    let fake = Fake::configured();
    let f = feed(&fake, SLOW).await;
    let mut rx = f.snapshots();
    until(&mut rx, |s| s.reached_ms.is_some()).await;

    let first = f.save(|c| {
        c.levels.push(Level::new(
            "ETH",
            Decimal::new(3450, 0),
            Direction::Above,
            None,
        ))
    });
    let second = f.save(|c| {
        c.stop_alerts = true;
    });
    let (a, b) = tokio::join!(first, second);
    assert_eq!(a.unwrap().version, 2);
    let saved = b.unwrap();
    assert_eq!(saved.version, 3);
    assert_eq!(
        saved.levels.len(),
        3,
        "the second save was applied to the first's result"
    );
    let matches: Vec<_> = fake
        .writes()
        .iter()
        .map(|w| w.if_match.clone().unwrap())
        .collect();
    assert_eq!(matches, ["\"1\"", "\"2\""]);
    let s = until(&mut rx, |s| {
        s.config.config().is_some_and(|c| c.version == 3)
    })
    .await;
    assert!(s.config.config().unwrap().stop_alerts);
}

#[tokio::test]
async fn the_pnl_summary_switch_is_saved_and_read_back() {
    let fake = Fake::configured();
    let f = feed(&fake, SLOW).await;
    let mut rx = f.snapshots();
    until(&mut rx, |s| s.reached_ms.is_some()).await;
    assert_eq!(rx.borrow().config.config().unwrap().pnl_summary, Some(true));

    let saved = f.save(|c| c.pnl_summary = Some(false)).await.unwrap();
    assert_eq!(saved.pnl_summary, Some(false));
    let body = fake.writes().last().unwrap().body.clone().unwrap();
    assert_eq!(body["pnl_summary"], json!(false));
    let s = until(&mut rx, |s| {
        s.config.config().is_some_and(|c| c.version == 2)
    })
    .await;
    assert_eq!(s.config.config().unwrap().pnl_summary, Some(false));
}

#[tokio::test]
async fn a_stale_save_is_refused_and_the_newer_configuration_shown() {
    let fake = Fake::configured();
    let f = feed(&fake, SLOW).await;
    let mut rx = f.snapshots();
    until(&mut rx, |s| s.reached_ms.is_some()).await;

    fake.edit_elsewhere(|c| c["near_liquidation_pct"] = json!("20"));
    let outcome = f.save(|c| {
        c.levels.remove(0);
    });
    assert_eq!(
        outcome.await.map(|_| ()),
        Err(WatcherError::Changed { version: 2 })
    );
    let s = rx.borrow().clone();
    let shown = s.config.config().unwrap();
    assert_eq!(
        (shown.version, shown.levels.len()),
        (2, 2),
        "nothing deleted, newer config shown"
    );
    assert_eq!(shown.near_liquidation_pct, Decimal::new(20, 0));
    assert_eq!(fake.writes().len(), 1, "not retried");
}

#[tokio::test]
async fn a_refused_save_returns_its_problems_and_changes_nothing() {
    let fake = Fake::configured();
    let f = feed(&fake, SLOW).await;
    let mut rx = f.snapshots();
    let before = until(&mut rx, |s| s.reached_ms.is_some()).await;
    match f.save(|c| c.accounts = Some(vec![galata_desk_core::watcher::WatchedAccount::new("hyperliquid", "0x00000000000000000000000000000000000000aa")])).await {
        Err(WatcherError::Refused { problems }) => assert_eq!(problems.len(), 2),
        other => panic!("{other:?}"),
    }
    assert_eq!(rx.borrow().config, before.config);
}

#[tokio::test]
async fn a_save_keeps_every_field_it_did_not_edit() {
    let fake = Fake::configured();
    fake.edit_elsewhere(|c| {
        c["quiet_hours"] = json!({"from": "23:00"});
        c["levels"][1]["colour"] = json!("violet");
    });
    let f = feed(&fake, SLOW).await;
    let mut rx = f.snapshots();
    until(&mut rx, |s| s.reached_ms.is_some()).await;
    f.save(|c| {
        c.levels.push(Level::new(
            "SOL",
            Decimal::new(150, 0),
            Direction::Below,
            Some("range low".into()),
        ))
    })
    .await
    .unwrap();
    let body = fake.writes()[0].body.clone().unwrap();
    assert_eq!(body["telegram_chat"], json!("123456789"));
    assert_eq!(
        body["digest"],
        json!({"time": "07:00", "zone": "Europe/Istanbul"})
    );
    assert_eq!(body["quiet_hours"], json!({"from": "23:00"}));
    assert_eq!(body["levels"][1]["colour"], json!("violet"));
    assert_eq!(
        body["levels"][2],
        json!({"market": "SOL", "price": "150", "direction": "below", "note": "range low"})
    );
}
