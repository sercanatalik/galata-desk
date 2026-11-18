//! Check the app's signing against the live venue without any account.
//!
//! Makes a throwaway key that no account has approved, signs a cancel and an amend of an
//! order that does not exist, and sends them. The venue recovers the signer
//! from the signature and refuses, naming the wallet it recovered. If that is
//! the throwaway key's wallet, the venue and this app agree on every byte of
//! what is signed. Nothing can come of the request: the key belongs to no one.
//!
//! ```text
//! cargo run -p galata-desk-core --example signing
//! ```

use galata_desk_core::api_key::ApiKey;
use galata_desk_core::network::Network;
use galata_desk_core::trade::exchange::{self, Answer};
use galata_desk_core::trade::sign::{self, Action, CancelOne, Modify, OrderKind, OrderWire};
use galata_desk_core::venue::Http;

#[tokio::main]
async fn main() {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the system gives random bytes");
    let key = ApiKey::parse(&hex::encode(bytes)).expect("32 random bytes are a key");
    let wallet = key.address();
    println!("throwaway wallet: {wallet}");

    // The two shapes of action the app sends.
    let actions = [
        Action::Cancel {
            cancels: vec![CancelOne { a: 0, o: 1 }],
        },
        Action::BatchModify {
            modifies: vec![Modify {
                oid: 1,
                order: OrderWire {
                    a: 0,
                    b: true,
                    p: "1000".into(),
                    s: "0.01".into(),
                    r: false,
                    t: OrderKind::limit("Gtc"),
                },
            }],
        },
    ];
    let mut agreed = true;
    for (action, network) in actions
        .iter()
        .flat_map(|a| [(a, Network::Testnet), (a, Network::Mainnet)])
    {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let signature = sign::sign(&key, action, nonce, network);
        let venue = Http::at(network.exchange_url());
        let answer = exchange::send(&venue, action, nonce, &signature).await;
        let recovered_us = match &answer {
            Answer::Refused(reason) => reason.to_lowercase().contains(wallet.as_str()),
            _ => false,
        };
        println!("{}: {answer:?}", network.label());
        println!(
            "  the venue recovered {}",
            if recovered_us {
                "this wallet: signing agrees"
            } else {
                "something else: signing does NOT agree, or the venue did not answer"
            }
        );
        agreed &= recovered_us;
    }
    if !agreed {
        std::process::exit(1);
    }
}
