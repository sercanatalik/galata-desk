//! The watcher client against a fake API that answers as the real one does.

mod watcher_fake;

use axum::http::StatusCode;
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::watcher::{Client, Direction, Level, WatcherError, WatcherUrl};
use serde_json::json;
use watcher_fake::{Fake, TOKEN};

fn client(base: &str, token: &str) -> Client {
    Client::new(WatcherUrl::parse(base).unwrap(), token)
}

#[tokio::test]
async fn reads_health_config_and_alerts_with_the_token_in_the_header_only() {
    let fake = Fake::configured();
    let base = fake.serve().await;
    let c = client(&base, TOKEN);

    let health = c.health().await.unwrap();
    assert_eq!(health.config_version, Some(1));
    let config = c.config().await.unwrap();
    assert_eq!((config.version, config.levels.len()), (1, 2));
    let alerts = c.alerts(0).await.unwrap();
    assert_eq!(alerts[0].id, "sl-9");

    for seen in fake.seen() {
        assert_eq!(
            seen.authorization.as_deref(),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        assert!(!seen.path.contains(TOKEN), "{}", seen.path);
    }
    assert!(fake.seen().iter().any(|s| s.path == "/alerts?since=0"));
}

#[tokio::test]
async fn a_wrong_token_is_refused_on_every_route() {
    let fake = Fake::configured();
    let c = client(&fake.serve().await, "wrong");
    assert_eq!(c.health().await, Err(WatcherError::TokenRefused));
    assert_eq!(
        c.config().await.map(|_| ()),
        Err(WatcherError::TokenRefused)
    );
    assert_eq!(
        c.alerts(0).await.map(|_| ()),
        Err(WatcherError::TokenRefused)
    );
}

#[tokio::test]
async fn a_watcher_without_configuration_says_so() {
    let fake = Fake::default();
    let c = client(&fake.serve().await, TOKEN);
    assert_eq!(c.config().await.map(|_| ()), Err(WatcherError::NoConfig));
}

#[tokio::test]
async fn a_write_names_its_version_and_returns_the_saved_configuration() {
    let fake = Fake::configured();
    let c = client(&fake.serve().await, TOKEN);
    let mut config = c.config().await.unwrap();
    config.levels.push(Level::new(
        "ETH",
        Decimal::new(3450, 0),
        Direction::Above,
        None,
    ));
    let saved = c.put_config(&config).await.unwrap();
    assert_eq!(saved.version, 2);
    assert_eq!(saved.levels[2].id.as_deref(), Some("ETH-above-3450"));

    let write = &fake.writes()[0];
    assert_eq!(write.if_match.as_deref(), Some("\"1\""));
    let body = write.body.as_ref().unwrap();
    assert!(body.get("version").is_none());
    assert_eq!(body["telegram_chat"], json!("123456789"));
    assert_eq!(
        body["digest"],
        json!({"time": "07:00", "zone": "Europe/Istanbul"})
    );
}

#[tokio::test]
async fn a_stale_write_is_refused_with_the_current_version() {
    let fake = Fake::configured();
    let c = client(&fake.serve().await, TOKEN);
    let config = c.config().await.unwrap();
    fake.edit_elsewhere(|c| c["near_liquidation_pct"] = json!("20"));
    assert_eq!(
        c.put_config(&config).await.map(|_| ()),
        Err(WatcherError::Changed { version: 2 })
    );
    assert_eq!(fake.config().unwrap()["near_liquidation_pct"], json!("20"));
}

#[tokio::test]
async fn a_refused_configuration_names_its_problems() {
    let fake = Fake::configured();
    let c = client(&fake.serve().await, TOKEN);
    let mut config = c.config().await.unwrap();
    config.accounts = Some(vec![galata_desk_core::watcher::WatchedAccount::new("hyperliquid", "0x00000000000000000000000000000000000000aa")]);
    match c.put_config(&config).await {
        Err(WatcherError::Refused { problems }) => {
            assert_eq!(problems.len(), 2);
            assert!(problems[0].contains("`accounts[0].alias`"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn other_answers_are_unexpected() {
    let fake = Fake::configured();
    let c = client(&fake.serve().await, TOKEN);
    fake.fail_with(Some(StatusCode::PRECONDITION_REQUIRED));
    let config = galata_desk_core::watcher::WatcherConfig {
        version: 1,
        ..serde_json::from_value(watcher_fake::fixture("config.json")).unwrap()
    };
    assert_eq!(
        c.put_config(&config).await.map(|_| ()),
        Err(WatcherError::Unexpected { status: 428 })
    );
    fake.fail_with(Some(StatusCode::INTERNAL_SERVER_ERROR));
    assert_eq!(
        c.health().await,
        Err(WatcherError::Unexpected { status: 500 })
    );
}

#[tokio::test]
async fn nothing_listening_is_unreachable_and_no_error_reveals_the_token() {
    // Port 9 (discard) has nothing listening on a developer machine.
    let c = client("http://127.0.0.1:9", TOKEN);
    let e = c.health().await.unwrap_err();
    assert_eq!(e, WatcherError::Unreachable);

    let errors = [
        e,
        WatcherError::TokenRefused,
        WatcherError::NoConfig,
        WatcherError::Changed { version: 3 },
        WatcherError::Refused {
            problems: vec!["`address`: bad".into()],
        },
        WatcherError::Unexpected { status: 500 },
    ];
    for e in errors {
        assert!(!e.to_string().contains(TOKEN) && !format!("{e:?}").contains(TOKEN));
    }
    assert!(!format!("{c:?}").contains(TOKEN));
}

#[tokio::test]
async fn a_redirect_is_not_followed_so_the_token_stays_with_the_watcher() {
    let fake = Fake::configured();
    let base = fake.serve().await;
    // A second server stands where the redirect points. It must see nothing.
    let elsewhere = Fake::configured();
    fake.redirect_to(Some(elsewhere.serve().await));
    let c = client(&base, TOKEN);
    assert_eq!(
        c.health().await,
        Err(WatcherError::Unexpected { status: 307 })
    );
    assert_eq!(
        c.config().await.map(|_| ()),
        Err(WatcherError::Unexpected { status: 307 })
    );
    assert!(elsewhere.seen().is_empty(), "{:?}", elsewhere.seen());
    assert_eq!(fake.seen().len(), 2);
}

#[tokio::test]
async fn the_accounts_are_listed_by_venue_and_alias_with_whether_each_is_watched() {
    let fake = Fake::configured();
    let c = client(&fake.serve().await, TOKEN);
    let listed = c.accounts().await.unwrap();
    let names: Vec<(String, bool)> = listed.accounts.iter().map(|a| (a.account().to_string(), a.watched)).collect();
    assert_eq!(names, [("derive-xyz owner_s3".to_string(), false), ("hyperliquid main".to_string(), true)]);
    let asked = fake.seen().into_iter().find(|s| s.path == "/accounts").unwrap();
    assert_eq!(asked.authorization.as_deref(), Some(&*format!("Bearer {TOKEN}")));

    let fresh = Fake::default();
    let c = client(&fresh.serve().await, TOKEN);
    assert_eq!(c.accounts().await, Err(WatcherError::NoConfig));
}

#[tokio::test]
async fn the_recorded_configuration_names_accounts_and_never_an_address() {
    let config: galata_desk_core::watcher::WatcherConfig = serde_json::from_value(watcher_fake::fixture("config.json")).unwrap();
    assert_eq!(config.watched(), "hyperliquid main");
    let all = galata_desk_core::watcher::WatcherConfig { accounts: None, ..config.clone() };
    assert_eq!(all.watched(), "every account");
    let written = serde_json::to_value(&all).unwrap();
    assert!(written.get("accounts").is_none(), "every account: none named");
    assert!(written.get("address").is_none());
}
