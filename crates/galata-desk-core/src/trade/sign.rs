//! Signing an action the way Hyperliquid checks it.
//!
//! The venue does not see the key. It recovers the signer from an EIP-712
//! signature over a small `Agent` struct whose `connectionId` is the hash of
//! the action itself: the action encoded as MessagePack, then the nonce, then
//! a byte saying no vault is involved. One wrong byte in that encoding does
//! not fail loudly: the venue recovers some other address and answers that
//! the wallet does not exist. So the encoding is pinned by vectors made with
//! the venue's own SDK (`tests/fixtures/signing_vectors.json`).
//!
//! Nothing here does I/O.

use serde::Serialize;
use sha3::{Digest, Keccak256};

use crate::api_key::ApiKey;
use crate::network::Network;

/// One order to cancel: the market's asset number and the order's id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CancelOne {
    pub a: u32,
    pub o: u64,
}

/// An order as the venue takes it. Field order is the venue's and is part of
/// what is signed: do not reorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderWire {
    /// Asset number.
    pub a: u32,
    /// Buy.
    pub b: bool,
    /// Price, as text.
    pub p: String,
    /// Size, as text.
    pub s: String,
    /// Reduce-only.
    pub r: bool,
    pub t: OrderKind,
}

/// What kind of order: one that rests or crosses at a limit price, or one
/// that waits for a trigger. Encoded as a map of one entry, `{"limit": ..}`
/// or `{"trigger": ..}`, as the venue takes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OrderKind {
    Limit(Limit),
    Trigger(Trigger),
}

impl OrderKind {
    /// A limit order with this time in force.
    pub fn limit(tif: impl Into<String>) -> Self {
        OrderKind::Limit(Limit { tif: tif.into() })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Limit {
    /// Time in force, in the venue's spelling: `Gtc`, `Alo`, `Ioc`.
    pub tif: String,
}

/// A take-profit or stop-loss. Field order is the venue's: do not reorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Trigger {
    /// Close at market when triggered, rather than at the order's price.
    pub is_market: bool,
    /// The trigger price, as text.
    pub trigger_px: String,
    pub tpsl: Tpsl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tpsl {
    Tp,
    Sl,
}

/// How the orders of one `order` action relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Grouping {
    /// Independent orders.
    Na,
    /// A position's take-profit and stop-loss, which cover the whole
    /// position whatever its size.
    PositionTpsl,
    /// An entry order followed by its own take-profit and stop-loss, sized
    /// to the entry.
    NormalTpsl,
}

/// One order to replace with another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Modify {
    pub oid: u64,
    pub order: OrderWire,
}

/// What this app can ask the venue to do. Serialising one gives exactly what
/// is signed and exactly what is sent, so the two cannot differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    Order {
        orders: Vec<OrderWire>,
        grouping: Grouping,
    },
    Cancel {
        cancels: Vec<CancelOne>,
    },
    BatchModify {
        modifies: Vec<Modify>,
    },
}

/// A signature in the venue's form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Signature {
    /// `0x` and the number in hex.
    pub r: String,
    pub s: String,
    /// 27 or 28.
    pub v: u8,
}

/// The action as MessagePack, maps keyed by field name.
pub fn encode(action: &Action) -> Vec<u8> {
    rmp_serde::to_vec_named(action).expect("an action of plain fields encodes")
}

/// The hash the venue calls the connection id: the action, the nonce, and a
/// zero byte for "no vault".
pub fn action_hash(action: &Action, nonce: u64) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(encode(action));
    hasher.update(nonce.to_be_bytes());
    hasher.update([0u8]);
    hasher.finalize().into()
}

fn keccak(bytes: &[u8]) -> [u8; 32] {
    Keccak256::digest(bytes).into()
}

/// The EIP-712 digest of `Agent { source, connectionId }` in the venue's
/// `Exchange` domain. `source` is `a` on the main network and `b` on the test
/// network, which is what stops a signature for one being replayed on the other.
fn digest(connection_id: &[u8; 32], network: Network) -> [u8; 32] {
    let mut domain = Vec::with_capacity(5 * 32);
    domain.extend(keccak(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    ));
    domain.extend(keccak(b"Exchange"));
    domain.extend(keccak(b"1"));
    let mut chain = [0u8; 32];
    chain[24..].copy_from_slice(&1337u64.to_be_bytes());
    domain.extend(chain);
    // The verifying contract is the zero address.
    domain.extend([0u8; 32]);

    let source: &[u8] = match network {
        Network::Mainnet => b"a",
        Network::Testnet => b"b",
    };
    let mut agent = Vec::with_capacity(3 * 32);
    agent.extend(keccak(b"Agent(string source,bytes32 connectionId)"));
    agent.extend(keccak(source));
    agent.extend(connection_id);

    let mut hasher = Keccak256::new();
    hasher.update([0x19, 0x01]);
    hasher.update(keccak(&domain));
    hasher.update(keccak(&agent));
    hasher.finalize().into()
}

/// `0x` and the number without leading zeros, as the venue's SDK writes it.
fn quantity(bytes: &[u8]) -> String {
    let hex = hex::encode(bytes);
    let trimmed = hex.trim_start_matches('0');
    format!("0x{}", if trimmed.is_empty() { "0" } else { trimmed })
}

/// Sign `action` for `network` with `key`.
pub fn sign(key: &ApiKey, action: &Action, nonce: u64, network: Network) -> Signature {
    let digest = digest(&action_hash(action, nonce), network);
    let (signature, recovery) = key
        .signing_key()
        .sign_prehash_recoverable(&digest)
        .expect("signing a 32-byte digest succeeds");
    let bytes = signature.to_bytes();
    Signature {
        r: quantity(&bytes[..32]),
        s: quantity(&bytes[32..]),
        v: 27 + recovery.to_byte(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{RecoveryId, VerifyingKey};
    use serde_json::Value;

    use crate::connect::Address;

    fn vectors() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/signing_vectors.json")).unwrap()
    }

    fn order_wire(o: &Value) -> OrderWire {
        let t = &o["t"];
        let kind = if let Some(limit) = t.get("limit") {
            OrderKind::limit(limit["tif"].as_str().unwrap())
        } else {
            let trigger = &t["trigger"];
            OrderKind::Trigger(Trigger {
                is_market: trigger["isMarket"].as_bool().unwrap(),
                trigger_px: trigger["triggerPx"].as_str().unwrap().into(),
                tpsl: match trigger["tpsl"].as_str().unwrap() {
                    "tp" => Tpsl::Tp,
                    "sl" => Tpsl::Sl,
                    other => panic!("no such tpsl: {other}"),
                },
            })
        };
        OrderWire {
            a: o["a"].as_u64().unwrap() as u32,
            b: o["b"].as_bool().unwrap(),
            p: o["p"].as_str().unwrap().into(),
            s: o["s"].as_str().unwrap().into(),
            r: o["r"].as_bool().unwrap(),
            t: kind,
        }
    }

    /// The fixture's action, rebuilt as this crate's type.
    fn action(json: &Value) -> Action {
        match json["type"].as_str().unwrap() {
            "order" => Action::Order {
                orders: json["orders"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(order_wire)
                    .collect(),
                grouping: match json["grouping"].as_str().unwrap() {
                    "na" => Grouping::Na,
                    "positionTpsl" => Grouping::PositionTpsl,
                    "normalTpsl" => Grouping::NormalTpsl,
                    other => panic!("no such grouping: {other}"),
                },
            },
            "cancel" => Action::Cancel {
                cancels: json["cancels"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| CancelOne {
                        a: c["a"].as_u64().unwrap() as u32,
                        o: c["o"].as_u64().unwrap(),
                    })
                    .collect(),
            },
            "batchModify" => Action::BatchModify {
                modifies: json["modifies"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| Modify {
                        oid: m["oid"].as_u64().unwrap(),
                        order: order_wire(&m["order"]),
                    })
                    .collect(),
            },
            other => panic!("no such action in this app: {other}"),
        }
    }

    fn network(vector: &Value) -> Network {
        if vector["mainnet"].as_bool().unwrap() {
            Network::Mainnet
        } else {
            Network::Testnet
        }
    }

    #[test]
    fn actions_encode_to_the_bytes_the_venues_sdk_signs() {
        let all = vectors();
        for v in all["vectors"].as_array().unwrap() {
            let action = action(&v["action"]);
            assert_eq!(
                hex::encode(encode(&action)),
                v["msgpack"].as_str().unwrap(),
                "{}",
                v["name"]
            );
            assert_eq!(
                hex::encode(action_hash(&action, all["nonce"].as_u64().unwrap())),
                v["hash"].as_str().unwrap(),
                "{}",
                v["name"]
            );
        }
    }

    #[test]
    fn what_is_sent_as_json_is_the_action_that_was_signed() {
        for v in vectors()["vectors"].as_array().unwrap() {
            assert_eq!(
                serde_json::to_value(action(&v["action"])).unwrap(),
                v["action"],
                "{}",
                v["name"]
            );
        }
    }

    #[test]
    fn signatures_match_the_venues_sdk_on_both_networks() {
        let all = vectors();
        let key = ApiKey::parse(all["key"].as_str().unwrap()).unwrap();
        assert_eq!(key.address().as_str(), all["address"].as_str().unwrap());
        let nonce = all["nonce"].as_u64().unwrap();
        let mut seen = 0;
        for v in all["vectors"].as_array().unwrap() {
            let signed = sign(&key, &action(&v["action"]), nonce, network(v));
            let label = format!("{} mainnet={}", v["name"], v["mainnet"]);
            assert_eq!(signed.r, v["r"].as_str().unwrap(), "{label}");
            assert_eq!(signed.s, v["s"].as_str().unwrap(), "{label}");
            assert_eq!(u64::from(signed.v), v["v"].as_u64().unwrap(), "{label}");
            seen += 1;
        }
        // Ten actions, each on both networks.
        assert_eq!(seen, 20);
    }

    #[test]
    fn the_signer_recovered_from_a_signature_is_the_keys_wallet() {
        let key = ApiKey::parse(&"ab".repeat(32)).unwrap();
        let action = Action::Cancel {
            cancels: vec![CancelOne { a: 0, o: 1 }],
        };
        for network in [Network::Mainnet, Network::Testnet] {
            let signed = sign(&key, &action, 7, network);
            let pad = |q: &str| format!("{:0>64}", q.trim_start_matches("0x"));
            let bytes = hex::decode(pad(&signed.r) + &pad(&signed.s)).unwrap();
            let signature = k256::ecdsa::Signature::from_slice(&bytes).unwrap();
            let recovered = VerifyingKey::recover_from_prehash(
                &digest(&action_hash(&action, 7), network),
                &signature,
                RecoveryId::from_byte(signed.v - 27).unwrap(),
            )
            .unwrap();
            assert_eq!(Address::from_key(&recovered), key.address());
        }
    }

    #[test]
    fn a_signature_for_one_network_is_not_one_for_the_other() {
        let key = ApiKey::parse(&"ab".repeat(32)).unwrap();
        let action = Action::Cancel {
            cancels: vec![CancelOne { a: 0, o: 1 }],
        };
        assert_ne!(
            sign(&key, &action, 7, Network::Mainnet),
            sign(&key, &action, 7, Network::Testnet)
        );
        // Nor is it one for another nonce.
        assert_ne!(
            sign(&key, &action, 7, Network::Mainnet),
            sign(&key, &action, 8, Network::Mainnet)
        );
    }

    #[test]
    fn numbers_are_written_without_leading_zeros() {
        assert_eq!(quantity(&[0, 0, 0x0a, 0xbc]), "0xabc");
        assert_eq!(quantity(&[0, 0]), "0x0");
    }
}
