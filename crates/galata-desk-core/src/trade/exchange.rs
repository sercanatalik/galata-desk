//! Carrying a signed action to the venue's exchange endpoint and reading
//! what it says.
//!
//! The venue answers an action that names several orders one order at a
//! time, so some can succeed while others are refused. A request that gets
//! no answer at all is a third thing: it may or may not have taken effect.

use std::str::FromStr;

use rust_decimal::Decimal;
use serde_json::{Value, json};

use super::sign::{Action, Signature};
use crate::venue::{Transport, VenueError};

/// What became of one order named in an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The venue did it. `filled` is set when the order traded, at once and
    /// in whole or in part; `resting` when it, or what was left of it, rests
    /// on the book. A cancel has neither.
    Done {
        filled: Option<Traded>,
        resting: bool,
    },
    /// The venue refused this order, in its own words.
    Refused(String),
}

impl Outcome {
    /// Done, with nothing filled and nothing resting: what a cancel answers.
    pub const DONE: Outcome = Outcome::Done {
        filled: None,
        resting: false,
    };
}

/// What traded of one order, as the venue reports it. Either figure is `None`
/// when the venue's text could not be read as a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Traded {
    pub size: Option<Decimal>,
    pub avg_price: Option<Decimal>,
}

/// The venue's answer to an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// One outcome per order, in the order the action named them.
    Each(Vec<Outcome>),
    /// The whole action was refused; nothing was done.
    Refused(String),
    /// No answer that can be read. The action may have taken effect.
    Unknown(String),
}

/// The request body for a signed action. The action is serialised from the
/// same value that was signed.
pub fn request(action: &Action, nonce: u64, signature: &Signature) -> Value {
    json!({
        "action": action,
        "nonce": nonce,
        "signature": signature,
        "vaultAddress": null,
    })
}

/// Send a signed action and read the answer. Never an error: every way this
/// can go is something to tell the user.
pub async fn send<T: Transport>(
    transport: &T,
    action: &Action,
    nonce: u64,
    signature: &Signature,
) -> Answer {
    match transport.post(request(action, nonce, signature)).await {
        Ok(bytes) => read(&bytes),
        // The venue said no before looking at the action.
        Err(e @ VenueError::RateLimited) => Answer::Refused(e.to_string()),
        Err(VenueError::Status(code)) if (400..500).contains(&code) => {
            Answer::Refused(format!("the venue answered HTTP {code}"))
        }
        // A server error or a dropped connection says nothing about whether
        // the action was handled.
        Err(e) => Answer::Unknown(e.to_string()),
    }
}

/// Read the venue's answer.
pub fn read(bytes: &[u8]) -> Answer {
    let not_understood = || Answer::Unknown("the venue's answer was not understood".into());
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return not_understood();
    };
    match value["status"].as_str() {
        Some("err") => Answer::Refused(
            value["response"]
                .as_str()
                .unwrap_or("the venue gave no reason")
                .to_string(),
        ),
        Some("ok") => match value["response"]["data"]["statuses"].as_array() {
            Some(statuses) => Answer::Each(statuses.iter().map(outcome).collect()),
            None => not_understood(),
        },
        _ => not_understood(),
    }
}

/// One entry of `statuses`: `"success"`, `{"resting": ..}`, `{"filled": ..}`
/// or `{"error": "<reason>"}`.
fn outcome(status: &Value) -> Outcome {
    if let Some(reason) = status.get("error") {
        return Outcome::Refused(
            reason
                .as_str()
                .unwrap_or("the venue gave no reason")
                .to_string(),
        );
    }
    let number = |v: &Value| v.as_str().and_then(|t| Decimal::from_str(t).ok());
    Outcome::Done {
        filled: status.get("filled").map(|f| Traded {
            size: number(&f["totalSz"]),
            avg_price: number(&f["avgPx"]),
        }),
        resting: status.get("resting").is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::sign::CancelOne;

    #[test]
    fn each_order_gets_its_own_outcome() {
        let answer = read(
            br#"{"status":"ok","response":{"type":"cancel","data":{"statuses":["success",{"error":"Order was never placed, already canceled, or filled."}]}}}"#,
        );
        assert_eq!(
            answer,
            Answer::Each(vec![
                Outcome::DONE,
                Outcome::Refused("Order was never placed, already canceled, or filled.".into()),
            ])
        );
    }

    #[test]
    fn an_amended_order_rests_or_fills() {
        let resting =
            br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":77}}]}}}"#;
        assert_eq!(
            read(resting),
            Answer::Each(vec![Outcome::Done {
                filled: None,
                resting: true
            }])
        );
        let filled = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"totalSz":"12","avgPx":"153.9","oid":77}}]}}}"#;
        assert_eq!(
            read(filled),
            Answer::Each(vec![Outcome::Done {
                filled: Some(Traded {
                    size: Some(Decimal::from(12)),
                    avg_price: Some(Decimal::from_str("153.9").unwrap()),
                }),
                resting: false
            }])
        );
    }

    #[test]
    fn a_trigger_order_waiting_for_its_price_is_done() {
        // As the venue answered a position take-profit and stop-loss on
        // testnet.
        let waiting = br#"{"response":{"data":{"statuses":["waitingForTrigger","waitingForTrigger"]},"type":"order"},"status":"ok"}"#;
        assert_eq!(
            read(waiting),
            Answer::Each(vec![Outcome::DONE, Outcome::DONE])
        );
    }

    #[test]
    fn a_partial_fill_reports_the_size_that_traded() {
        // An IOC close of 10 of which 6 traded: the venue reports the 6, and
        // the rest is cancelled, not left resting.
        let partial = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"totalSz":"6","avgPx":"153.91","oid":78}}]}}}"#;
        let Answer::Each(outcomes) = read(partial) else {
            panic!("an answer per order")
        };
        assert_eq!(
            outcomes,
            vec![Outcome::Done {
                filled: Some(Traded {
                    size: Some(Decimal::from(6)),
                    avg_price: Some(Decimal::from_str("153.91").unwrap()),
                }),
                resting: false
            }]
        );
    }

    #[test]
    fn an_ioc_that_did_not_match_is_refused_with_the_venues_words() {
        let none = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"Order could not immediately match against any resting orders. asset=0"}]}}}"#;
        assert_eq!(
            read(none),
            Answer::Each(vec![Outcome::Refused(
                "Order could not immediately match against any resting orders. asset=0".into()
            )])
        );
    }

    #[test]
    fn a_fill_whose_figures_cannot_be_read_is_still_a_fill() {
        let odd = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"totalSz":"x","oid":78}}]}}}"#;
        assert_eq!(
            read(odd),
            Answer::Each(vec![Outcome::Done {
                filled: Some(Traded {
                    size: None,
                    avg_price: None
                }),
                resting: false
            }])
        );
    }

    #[test]
    fn a_refusal_of_the_whole_action_carries_the_venues_reason() {
        assert_eq!(
            read(br#"{"status":"err","response":"User or API Wallet 0xabc does not exist."}"#),
            Answer::Refused("User or API Wallet 0xabc does not exist.".into())
        );
    }

    #[test]
    fn an_answer_that_cannot_be_read_is_unknown_not_a_refusal() {
        for bad in [
            &b"<html>bad gateway</html>"[..],
            br#"{"status":"ok"}"#,
            br#"{"status":"ok","response":{"type":"default"}}"#,
            br#"{}"#,
        ] {
            assert!(matches!(read(bad), Answer::Unknown(_)), "{bad:?}");
        }
    }

    #[test]
    fn the_request_carries_the_action_the_nonce_and_the_signature_and_no_vault() {
        let action = Action::Cancel {
            cancels: vec![CancelOne { a: 5, o: 9 }],
        };
        let signature = Signature {
            r: "0x1".into(),
            s: "0x2".into(),
            v: 27,
        };
        let body = request(&action, 1234, &signature);
        assert_eq!(
            body,
            json!({
                "action": { "type": "cancel", "cancels": [{ "a": 5, "o": 9 }] },
                "nonce": 1234,
                "signature": { "r": "0x1", "s": "0x2", "v": 27 },
                "vaultAddress": null,
            })
        );
    }
}
