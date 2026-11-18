//! The gate against a fake venue: what stops an action before anything is
//! sent, what is sent when nothing stops it, and that the key never shows.

use std::str::FromStr;
use std::sync::{Arc, Mutex};

use galata_desk_core::connect::Address;
use galata_desk_core::network::Network;
use galata_desk_core::secrets::{MemorySecrets, Secrets, api_key_name};
use galata_desk_core::trade::assets::Asset;
use galata_desk_core::trade::rules::{self, Current};
use galata_desk_core::trade::*;
use galata_desk_core::venue::{Side, Transport, VenueError};
use rust_decimal::Decimal;
use serde_json::{Value, json};

const ACCOUNT: &str = "0x010461c14e146ac35fe42271bdc1134ee31c703a";
const OTHER: &str = "0x00000000000000000000000000000000000000aa";
/// The key the signing vectors were made with, and its wallet.
const KEY: &str = "0123456789012345678901234567890123456789012345678901234567890123";
const WALLET: &str = "0x14791697260e4c9a71f18484c9f997b308e59325";

/// How the fake venue answers the question "what is this address?".
#[derive(Clone, Copy, PartialEq)]
enum Role {
    AgentOf(&'static str),
    User,
    Missing,
    Down,
}

/// How it answers an action.
#[derive(Clone)]
enum Reply {
    Json(Value),
    Fail(VenueError),
}

#[derive(Clone)]
struct Venue {
    role: Arc<Mutex<Role>>,
    reply: Arc<Mutex<Reply>>,
    /// Replies to use first, one per action, before `reply`.
    queued: Arc<Mutex<std::collections::VecDeque<Reply>>>,
    /// Role questions asked.
    asked: Arc<Mutex<Vec<Value>>>,
    /// Actions received, whole request bodies.
    sent: Arc<Mutex<Vec<Value>>>,
}

impl Venue {
    fn new(role: Role) -> Self {
        Self {
            role: Arc::new(Mutex::new(role)),
            reply: Arc::new(Mutex::new(Reply::Json(ok(json!(["success"]))))),
            queued: Arc::default(),
            asked: Arc::default(),
            sent: Arc::default(),
        }
    }
    fn replies(&self, reply: Reply) {
        *self.reply.lock().unwrap() = reply;
    }
    fn sent(&self) -> Vec<Value> {
        self.sent.lock().unwrap().clone()
    }
    fn asked(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

fn ok(statuses: Value) -> Value {
    json!({ "status": "ok", "response": { "type": "cancel", "data": { "statuses": statuses } } })
}

impl Transport for Venue {
    async fn post(&self, body: Value) -> Result<Vec<u8>, VenueError> {
        if body.get("action").is_some() {
            self.sent.lock().unwrap().push(body);
            let next = self.queued.lock().unwrap().pop_front();
            return match next.unwrap_or_else(|| self.reply.lock().unwrap().clone()) {
                Reply::Json(value) => Ok(value.to_string().into_bytes()),
                Reply::Fail(e) => Err(e),
            };
        }
        if body["type"] == "allMids" {
            // Mids for the main dex, or for the dex asked, keyed as the
            // venue keys them.
            let answer = match body.get("dex").and_then(Value::as_str) {
                Some("xyz") => json!({ "xyz:NVDA": "154.2" }),
                _ => json!({ "BTC": "91437.5" }),
            };
            return Ok(answer.to_string().into_bytes());
        }
        assert_eq!(body["type"], "userRole", "the gate asks nothing else");
        self.asked.lock().unwrap().push(body);
        let answer = match *self.role.lock().unwrap() {
            Role::AgentOf(user) => json!({ "role": "agent", "data": { "user": user } }),
            Role::User => json!({ "role": "user" }),
            Role::Missing => json!({ "role": "missing" }),
            Role::Down => return Err(VenueError::Unreachable("no connection".into())),
        };
        Ok(answer.to_string().into_bytes())
    }
}

fn standing(network: Network) -> Standing {
    Standing {
        network,
        overridden: false,
        mainnet_allowed: false,
        account: Address::parse(ACCOUNT).unwrap(),
    }
}

fn secrets_with_key(network: Network) -> Arc<MemorySecrets> {
    let secrets = Arc::new(MemorySecrets::default());
    secrets.set(api_key_name(network), KEY).unwrap();
    secrets
}

fn trader(venue: &Venue, secrets: Arc<MemorySecrets>, standing: Standing) -> Trader<Venue> {
    Trader::new(venue.clone(), venue.clone(), secrets, standing)
}

const ORDER: Target = Target {
    asset: 5,
    oid: 607756224194,
};

#[tokio::test]
async fn an_approved_key_on_testnet_cancels_and_the_request_is_signed_for_testnet() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        standing(Network::Testnet),
    );
    assert_eq!(t.availability(), Ok(()));

    let answer = t.cancel(&[ORDER]).await.unwrap();
    assert_eq!(answer, Answer::Each(vec![Outcome::DONE]));

    let sent = venue.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]["action"],
        json!({ "type": "cancel", "cancels": [{ "a": 5, "o": 607756224194u64 }] })
    );
    assert_eq!(sent[0]["vaultAddress"], Value::Null);
    // The venue was asked about the key's wallet, not about the account.
    assert_eq!(venue.asked.lock().unwrap()[0]["user"], WALLET);

    // The signature is the one the venue's SDK makes for this action, nonce
    // and network: recompute it from what was sent.
    let nonce = sent[0]["nonce"].as_u64().unwrap();
    let key = galata_desk_core::api_key::ApiKey::parse(KEY).unwrap();
    let action = sign::Action::Cancel {
        cancels: vec![sign::CancelOne {
            a: 5,
            o: 607756224194,
        }],
    };
    let expected = sign::sign(&key, &action, nonce, Network::Testnet);
    assert_eq!(
        sent[0]["signature"],
        serde_json::to_value(&expected).unwrap()
    );
    assert_ne!(
        sent[0]["signature"],
        serde_json::to_value(sign::sign(&key, &action, nonce, Network::Mainnet)).unwrap()
    );
}

#[tokio::test]
async fn each_guard_stops_the_action_before_anything_is_asked_or_sent() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));

    // A development override: read-only, whatever else is true.
    let t = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        Standing {
            overridden: true,
            ..standing(Network::Testnet)
        },
    );
    assert_eq!(t.availability(), Err(Unavailable::Overridden));
    assert_eq!(t.cancel(&[ORDER]).await, Err(Unavailable::Overridden));

    // Mainnet with the switch off.
    let t = trader(
        &venue,
        secrets_with_key(Network::Mainnet),
        standing(Network::Mainnet),
    );
    assert_eq!(t.availability(), Err(Unavailable::MainnetOff));
    assert_eq!(t.cancel(&[ORDER]).await, Err(Unavailable::MainnetOff));

    // A key for the other network only.
    let t = trader(
        &venue,
        secrets_with_key(Network::Mainnet),
        standing(Network::Testnet),
    );
    assert_eq!(t.availability(), Err(Unavailable::NoKey(Network::Testnet)));
    assert_eq!(
        t.cancel(&[ORDER]).await,
        Err(Unavailable::NoKey(Network::Testnet))
    );

    // Something in the keychain that is not a key.
    let junk = Arc::new(MemorySecrets::default());
    junk.set(api_key_name(Network::Testnet), "hello").unwrap();
    let t = trader(&venue, junk, standing(Network::Testnet));
    assert!(matches!(t.availability(), Err(Unavailable::Keychain(_))));
    assert!(matches!(
        t.cancel(&[ORDER]).await,
        Err(Unavailable::Keychain(_))
    ));

    assert_eq!(venue.asked(), 0, "the venue was not asked about any key");
    assert!(venue.sent().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn mainnet_works_once_the_switch_is_on() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = trader(
        &venue,
        secrets_with_key(Network::Mainnet),
        Standing {
            mainnet_allowed: true,
            ..standing(Network::Mainnet)
        },
    );
    assert_eq!(t.availability(), Ok(()));
    assert!(t.cancel(&[ORDER]).await.is_ok());
    assert_eq!(venue.sent().len(), 1);
}

#[tokio::test]
async fn a_key_that_is_not_this_accounts_sends_nothing_and_is_remembered() {
    for role in [Role::AgentOf(OTHER), Role::User, Role::Missing] {
        let venue = Venue::new(role);
        let t = trader(
            &venue,
            secrets_with_key(Network::Testnet),
            standing(Network::Testnet),
        );
        // Not known yet: the controls are offered.
        assert_eq!(t.availability(), Ok(()));
        assert_eq!(t.cancel(&[ORDER]).await, Err(Unavailable::KeyNotApproved));
        // Now it is known, and it is not asked again.
        assert_eq!(t.availability(), Err(Unavailable::KeyNotApproved));
        assert_eq!(t.cancel(&[ORDER]).await, Err(Unavailable::KeyNotApproved));
        assert_eq!(venue.asked(), 1);
        assert!(venue.sent().is_empty());
    }
}

#[tokio::test]
async fn the_account_is_matched_whatever_its_letter_case() {
    let venue = Venue::new(Role::AgentOf("0x010461C14E146AC35FE42271BDC1134EE31C703A"));
    let t = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        standing(Network::Testnet),
    );
    assert!(t.cancel(&[ORDER]).await.is_ok());
}

#[tokio::test]
async fn a_check_that_cannot_be_made_sends_nothing_and_is_tried_again() {
    let venue = Venue::new(Role::Down);
    let t = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        standing(Network::Testnet),
    );
    assert!(matches!(
        t.cancel(&[ORDER]).await,
        Err(Unavailable::KeyCheckFailed(_))
    ));
    assert!(venue.sent().is_empty());
    // Not held against the key.
    assert_eq!(t.availability(), Ok(()));

    *venue.role.lock().unwrap() = Role::AgentOf(ACCOUNT);
    assert!(t.cancel(&[ORDER]).await.is_ok());
    // Approved once, not asked again: one question failed, one was answered.
    assert!(t.cancel(&[ORDER]).await.is_ok());
    assert_eq!(venue.asked(), 2);
    assert_eq!(venue.sent().len(), 2);
}

#[tokio::test]
async fn cancel_all_is_one_request_and_each_order_gets_its_outcome() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    venue.replies(Reply::Json(ok(json!([
        "success",
        { "error": "Order was never placed, already canceled, or filled." },
        "success",
    ]))));
    let t = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        standing(Network::Testnet),
    );
    let orders = [
        Target { asset: 5, oid: 1 },
        Target {
            asset: 110_003,
            oid: 2,
        },
        Target {
            asset: 10_107,
            oid: 3,
        },
    ];
    let answer = t.cancel(&orders).await.unwrap();
    assert_eq!(
        answer,
        Answer::Each(vec![
            Outcome::DONE,
            Outcome::Refused("Order was never placed, already canceled, or filled.".into()),
            Outcome::DONE,
        ])
    );
    let sent = venue.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]["action"]["cancels"],
        json!([{ "a": 5, "o": 1 }, { "a": 110_003, "o": 2 }, { "a": 10_107, "o": 3 }])
    );
}

#[tokio::test]
async fn an_amend_sends_the_checked_text_and_keeps_side_type_and_reduce_only() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    venue.replies(Reply::Json(json!({
        "status": "ok",
        "response": { "type": "order", "data": { "statuses": [{ "resting": { "oid": 99 } }] } },
    })));
    let t = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        standing(Network::Testnet),
    );
    let nvda = Asset {
        number: 110_000,
        sz_decimals: 2,
        spot: false,
    };
    let now = Current {
        price: Decimal::from_str("154.20").unwrap(),
        size: Decimal::from(12),
    };
    let to = rules::check("153.80", "15", &nvda, &now).unwrap();
    let kept = Kept {
        side: Side::Buy,
        reduce_only: false,
        tif: "Gtc".into(),
    };
    let answer = t
        .amend(
            Target {
                asset: nvda.number,
                oid: 42,
            },
            &to,
            &kept,
        )
        .await
        .unwrap();
    assert_eq!(
        answer,
        Answer::Each(vec![Outcome::Done {
            filled: None,
            resting: true
        }])
    );
    assert_eq!(
        venue.sent()[0]["action"],
        json!({
            "type": "batchModify",
            "modifies": [{
                "oid": 42,
                "order": {
                    "a": 110_000, "b": true, "p": "153.8", "s": "15", "r": false,
                    "t": { "limit": { "tif": "Gtc" } },
                },
            }],
        })
    );
}

#[tokio::test]
async fn a_refusal_and_a_silence_are_told_apart() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        standing(Network::Testnet),
    );

    venue.replies(Reply::Json(
        json!({ "status": "err", "response": "Too many cumulative requests sent" }),
    ));
    assert_eq!(
        t.cancel(&[ORDER]).await.unwrap(),
        Answer::Refused("Too many cumulative requests sent".into())
    );

    venue.replies(Reply::Fail(VenueError::Status(422)));
    assert!(matches!(
        t.cancel(&[ORDER]).await.unwrap(),
        Answer::Refused(_)
    ));
    venue.replies(Reply::Fail(VenueError::RateLimited));
    assert!(matches!(
        t.cancel(&[ORDER]).await.unwrap(),
        Answer::Refused(_)
    ));

    // No answer, or a server error: the action may have been handled.
    for silence in [
        VenueError::Unreachable("the request timed out".into()),
        VenueError::Status(502),
    ] {
        venue.replies(Reply::Fail(silence));
        assert!(matches!(
            t.cancel(&[ORDER]).await.unwrap(),
            Answer::Unknown(_)
        ));
    }
}

#[tokio::test]
async fn nonces_only_rise() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        standing(Network::Testnet),
    );
    for _ in 0..5 {
        t.cancel(&[ORDER]).await.unwrap();
    }
    let nonces: Vec<u64> = venue
        .sent()
        .iter()
        .map(|b| b["nonce"].as_u64().unwrap())
        .collect();
    assert!(nonces.windows(2).all(|w| w[1] > w[0]), "{nonces:?}");
    // A nonce is a time in milliseconds, which is what the venue expects.
    assert!(nonces[0] > 1_700_000_000_000);
}

#[tokio::test]
async fn the_key_is_in_nothing_that_is_sent_shown_or_reported() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        standing(Network::Testnet),
    );
    let done = t.cancel(&[ORDER]).await;
    venue.replies(Reply::Fail(VenueError::Unreachable("no connection".into())));
    let silent = t.cancel(&[ORDER]).await;
    *venue.role.lock().unwrap() = Role::Down;
    let unchecked = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        standing(Network::Testnet),
    )
    .cancel(&[ORDER])
    .await;

    let mut seen = format!("{done:?} {silent:?} {unchecked:?} {:?}", t.standing());
    for e in [&done, &silent, &unchecked] {
        if let Err(e) = e {
            seen.push_str(&e.to_string());
        }
    }
    for body in venue.sent() {
        seen.push_str(&body.to_string());
    }
    for body in venue.asked.lock().unwrap().iter() {
        seen.push_str(&body.to_string());
    }
    assert!(!seen.to_lowercase().contains(KEY), "{seen}");
    // The wallet's address is not a secret, and is all that names the key.
    assert!(seen.contains(WALLET));
}

const NVDA: Asset = Asset {
    number: 110_000,
    sz_decimals: 2,
    spot: false,
};
const BTC: Asset = Asset {
    number: 0,
    sz_decimals: 5,
    spot: false,
};

fn approved_on_testnet(venue: &Venue) -> Trader<Venue> {
    trader(
        venue,
        secrets_with_key(Network::Testnet),
        standing(Network::Testnet),
    )
}

fn filled(size: &str, price: &str) -> Value {
    json!({ "status": "ok", "response": { "type": "order", "data": { "statuses": [
        { "filled": { "totalSz": size, "avgPx": price, "oid": 1 } }
    ] } } })
}

/// The signature in a request body is the one this app makes for the action
/// in the same body: what was signed is what was sent.
fn signed_as_sent(body: &Value, action: &sign::Action) {
    assert_eq!(body["action"], serde_json::to_value(action).unwrap());
    let key = galata_desk_core::api_key::ApiKey::parse(KEY).unwrap();
    let nonce = body["nonce"].as_u64().unwrap();
    assert_eq!(
        body["signature"],
        serde_json::to_value(sign::sign(&key, action, nonce, Network::Testnet)).unwrap()
    );
}

fn close_order(a: u32, buy: bool, p: &str, s: &str, tif: &str) -> sign::OrderWire {
    sign::OrderWire {
        a,
        b: buy,
        p: p.into(),
        s: s.into(),
        r: true,
        t: sign::OrderKind::limit(tif),
    }
}

#[tokio::test]
async fn a_market_close_of_a_long_is_a_reduce_only_ioc_sell_at_the_worst_price() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    venue.replies(Reply::Json(filled("4", "153.91")));
    let t = approved_on_testnet(&venue);
    let long = Holding {
        asset: NVDA,
        size: Decimal::from(10),
    };
    let mid = t.mid("xyz", "NVDA").await.unwrap().unwrap();
    let worst = rules::slippage(mid, false, &NVDA, rules::MARKET_SLIPPAGE_PCT);
    assert_eq!(rules::wire(worst), "146.49");

    let answer = t
        .close(&long, Decimal::from(4), Pricing::Market { worst })
        .await
        .unwrap();
    assert_eq!(
        answer,
        Answer::Each(vec![Outcome::Done {
            filled: Some(Traded {
                size: Some(Decimal::from(4)),
                avg_price: Some(Decimal::from_str("153.91").unwrap()),
            }),
            resting: false,
        }])
    );
    let sent = venue.sent();
    assert_eq!(sent.len(), 1);
    signed_as_sent(
        &sent[0],
        &sign::Action::Order {
            orders: vec![close_order(110_000, false, "146.49", "4", "Ioc")],
            grouping: sign::Grouping::Na,
        },
    );
    assert_eq!(sent[0]["action"]["grouping"], "na");
}

#[tokio::test]
async fn a_limit_close_of_a_short_is_a_reduce_only_gtc_buy() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = approved_on_testnet(&venue);
    let short = Holding {
        asset: NVDA,
        size: Decimal::from(-10),
    };
    t.close(
        &short,
        Decimal::from_str("2.50").unwrap(),
        Pricing::Limit {
            price: Decimal::from_str("150.00").unwrap(),
        },
    )
    .await
    .unwrap();
    signed_as_sent(
        &venue.sent()[0],
        &sign::Action::Order {
            orders: vec![close_order(110_000, true, "150", "2.5", "Gtc")],
            grouping: sign::Grouping::Na,
        },
    );
}

#[tokio::test]
async fn close_all_closes_each_position_whole_in_one_request() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    venue.replies(Reply::Json(
        json!({ "status": "ok", "response": { "type": "order", "data": { "statuses": [
        { "filled": { "totalSz": "0.00162", "avgPx": "91400", "oid": 1 } },
        { "error": "Order could not immediately match against any resting orders. asset=110000" }
    ] } } }),
    ));
    let t = approved_on_testnet(&venue);
    let positions = [
        (
            Holding {
                asset: BTC,
                size: Decimal::from_str("0.00162").unwrap(),
            },
            Decimal::from(86866),
        ),
        (
            Holding {
                asset: NVDA,
                size: Decimal::from(-10),
            },
            Decimal::from_str("161.91").unwrap(),
        ),
    ];
    let answer = t.close_all(&positions).await.unwrap();
    let Answer::Each(outcomes) = answer else {
        panic!("one outcome per position")
    };
    assert_eq!(outcomes.len(), 2);
    assert!(matches!(outcomes[1], Outcome::Refused(_)));
    let sent = venue.sent();
    assert_eq!(sent.len(), 1, "one request for every position");
    signed_as_sent(
        &sent[0],
        &sign::Action::Order {
            orders: vec![
                close_order(0, false, "86866", "0.00162", "Ioc"),
                close_order(110_000, true, "161.91", "10", "Ioc"),
            ],
            grouping: sign::Grouping::Na,
        },
    );
}

#[tokio::test]
async fn every_close_order_is_reduce_only_and_against_the_position() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = approved_on_testnet(&venue);
    for size in ["10", "-10"] {
        let p = Holding {
            asset: NVDA,
            size: Decimal::from_str(size).unwrap(),
        };
        let one = Decimal::ONE;
        t.close(&p, one, Pricing::Market { worst: one })
            .await
            .unwrap();
        t.close(&p, one, Pricing::Limit { price: one })
            .await
            .unwrap();
        t.close_all(&[(p, one)]).await.unwrap();
    }
    let sent = venue.sent();
    assert_eq!(sent.len(), 6);
    for (n, body) in sent.iter().enumerate() {
        let long = n < 3;
        for order in body["action"]["orders"].as_array().unwrap() {
            assert_eq!(order["r"], true, "reduce-only: {body}");
            assert_eq!(order["b"], !long, "the other side: {body}");
        }
    }
}

#[tokio::test]
async fn each_guard_stops_a_close_before_anything_is_asked_or_sent() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let p = Holding {
        asset: NVDA,
        size: Decimal::from(10),
    };
    let one = Decimal::ONE;
    let stopped = [
        (
            trader(
                &venue,
                secrets_with_key(Network::Testnet),
                Standing {
                    overridden: true,
                    ..standing(Network::Testnet)
                },
            ),
            Unavailable::Overridden,
        ),
        (
            trader(
                &venue,
                secrets_with_key(Network::Mainnet),
                standing(Network::Mainnet),
            ),
            Unavailable::MainnetOff,
        ),
        (
            trader(
                &venue,
                secrets_with_key(Network::Mainnet),
                standing(Network::Testnet),
            ),
            Unavailable::NoKey(Network::Testnet),
        ),
    ];
    for (t, why) in stopped {
        assert_eq!(
            t.close(&p, one, Pricing::Market { worst: one }).await,
            Err(why.clone())
        );
        assert_eq!(t.close_all(&[(p, one)]).await, Err(why));
    }
    assert_eq!(venue.asked(), 0);
    assert!(venue.sent().is_empty());

    // A key the venue does not know for this account.
    let stranger = Venue::new(Role::User);
    let t = approved_on_testnet(&stranger);
    assert_eq!(
        t.close(&p, one, Pricing::Market { worst: one }).await,
        Err(Unavailable::KeyNotApproved)
    );
    assert!(stranger.sent().is_empty());
}

fn trigger_order(buy: bool, p: &str, at: &str, tpsl: sign::Tpsl) -> sign::OrderWire {
    sign::OrderWire {
        a: 110_000,
        b: buy,
        p: p.into(),
        s: "0".into(),
        r: true,
        t: sign::OrderKind::Trigger(sign::Trigger {
            is_market: true,
            trigger_px: at.into(),
            tpsl,
        }),
    }
}

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).unwrap()
}

const LONG_NVDA: Holding = Holding {
    asset: NVDA,
    size: Decimal::from_parts(10, 0, 0, false, 0),
};

#[tokio::test]
async fn new_levels_are_placed_together_as_the_positions_take_profit_and_stop_loss() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = approved_on_testnet(&venue);
    let steps = t
        .set_tpsl(
            &LONG_NVDA,
            TpslNow::default(),
            Level::At(dec("160")),
            Level::At(dec("148")),
        )
        .await;
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].kind, TpslRequest::Place);
    assert_eq!(steps[0].levels, vec![sign::Tpsl::Tp, sign::Tpsl::Sl]);
    let sent = venue.sent();
    assert_eq!(sent.len(), 1);
    // Size 0 and grouping `positionTpsl`: they cover the whole position.
    // Each closes the long by selling, at most 10% past its trigger.
    signed_as_sent(
        &sent[0],
        &sign::Action::Order {
            orders: vec![
                trigger_order(false, "144", "160", sign::Tpsl::Tp),
                trigger_order(false, "133.2", "148", sign::Tpsl::Sl),
            ],
            grouping: sign::Grouping::PositionTpsl,
        },
    );
}

#[tokio::test]
async fn a_level_that_exists_is_moved_in_place_never_cancelled() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = approved_on_testnet(&venue);
    let short = Holding {
        asset: NVDA,
        size: dec("-10"),
    };
    let now = TpslNow {
        tp: None,
        sl: Some(77),
    };
    let steps = t
        .set_tpsl(&short, now, Level::Keep, Level::At(dec("160")))
        .await;
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].kind, TpslRequest::Move);
    let sent = venue.sent();
    assert_eq!(sent.len(), 1, "no cancel is sent");
    // A short's stop buys, at most 10% above its trigger.
    signed_as_sent(
        &sent[0],
        &sign::Action::BatchModify {
            modifies: vec![sign::Modify {
                oid: 77,
                order: trigger_order(true, "176", "160", sign::Tpsl::Sl),
            }],
        },
    );
}

#[tokio::test]
async fn a_change_goes_out_as_move_then_place_then_remove() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = approved_on_testnet(&venue);
    let kinds = |steps: &[TpslStep]| steps.iter().map(|s| s.kind).collect::<Vec<_>>();
    let types = |venue: &Venue| {
        venue
            .sent()
            .iter()
            .map(|b| b["action"]["type"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    // Add a take-profit and move the stop.
    let steps = t
        .set_tpsl(
            &LONG_NVDA,
            TpslNow {
                tp: None,
                sl: Some(2),
            },
            Level::At(dec("160")),
            Level::At(dec("150")),
        )
        .await;
    assert_eq!(kinds(&steps), [TpslRequest::Move, TpslRequest::Place]);
    assert_eq!(types(&venue), ["batchModify", "order"]);

    // Move the stop and remove the take-profit.
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = approved_on_testnet(&venue);
    let steps = t
        .set_tpsl(
            &LONG_NVDA,
            TpslNow {
                tp: Some(1),
                sl: Some(2),
            },
            Level::Remove,
            Level::At(dec("150")),
        )
        .await;
    assert_eq!(kinds(&steps), [TpslRequest::Move, TpslRequest::Remove]);
    assert_eq!(types(&venue), ["batchModify", "cancel"]);
    assert_eq!(
        venue.sent()[1]["action"]["cancels"],
        json!([{ "a": 110_000, "o": 1 }])
    );

    // Remove both: one cancel naming both.
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = approved_on_testnet(&venue);
    let steps = t
        .set_tpsl(
            &LONG_NVDA,
            TpslNow {
                tp: Some(1),
                sl: Some(2),
            },
            Level::Remove,
            Level::Remove,
        )
        .await;
    assert_eq!(kinds(&steps), [TpslRequest::Remove]);
    assert_eq!(steps[0].levels, vec![sign::Tpsl::Tp, sign::Tpsl::Sl]);

    // Nothing to do: nothing sent. Removing a level that is not set is
    // nothing to do.
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = approved_on_testnet(&venue);
    let steps = t
        .set_tpsl(&LONG_NVDA, TpslNow::default(), Level::Keep, Level::Remove)
        .await;
    assert!(steps.is_empty());
    assert!(venue.sent().is_empty());
}

#[tokio::test]
async fn a_refused_part_is_reported_and_the_rest_still_sent() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    venue.queued.lock().unwrap().extend([
        Reply::Json(json!({ "status": "err", "response": "Cannot modify canceled or filled order" })),
        Reply::Json(json!({ "status": "ok", "response": { "type": "order", "data": { "statuses": ["waitingForTrigger"] } } })),
    ]);
    let t = approved_on_testnet(&venue);
    let steps = t
        .set_tpsl(
            &LONG_NVDA,
            TpslNow {
                tp: None,
                sl: Some(2),
            },
            Level::At(dec("160")),
            Level::At(dec("150")),
        )
        .await;
    assert_eq!(
        steps[0].answer,
        Ok(Answer::Refused(
            "Cannot modify canceled or filled order".into()
        ))
    );
    assert_eq!(steps[1].answer, Ok(Answer::Each(vec![Outcome::DONE])));
    assert_eq!(venue.sent().len(), 2);
}

#[tokio::test]
async fn each_guard_stops_a_tpsl_change_before_anything_is_sent() {
    let venue = Venue::new(Role::AgentOf(ACCOUNT));
    let t = trader(
        &venue,
        secrets_with_key(Network::Testnet),
        Standing {
            overridden: true,
            ..standing(Network::Testnet)
        },
    );
    let steps = t
        .set_tpsl(
            &LONG_NVDA,
            TpslNow {
                tp: Some(1),
                sl: None,
            },
            Level::Remove,
            Level::At(dec("150")),
        )
        .await;
    assert_eq!(steps.len(), 2);
    assert!(
        steps
            .iter()
            .all(|s| s.answer == Err(Unavailable::Overridden))
    );
    assert!(venue.sent().is_empty());
}
