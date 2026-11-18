//! Learn, on testnet, what the venue does with the orders that close a
//! position and set its take-profit and stop-loss, before the app relies on
//! it. Each step prints the venue's raw answer; open orders are also saved as
//! files, to become fixtures.
//!
//! It runs on testnet only: there is no way to point it anywhere else. It
//! signs with the testnet API key the app saved in the keychain, which must
//! be approved for `--account`. It opens nothing: every order is
//! reduce-only. It needs a small open testnet position in `--coin`, worth at
//! least $16 for the closing steps.
//!
//! ```text
//! cargo run -p galata-desk-core --example tpsl_probe -- \
//!     --account 0xYOURACCOUNT --coin xyz:NVDA [--second BTC] [--close-position]
//! cargo run -p galata-desk-core --example tpsl_probe -- \
//!     --account 0xYOURACCOUNT --coin xyz:NVDA --orders-only
//! ```
//!
//! Steps, in order:
//!
//! 1. Cancel take-profit and stop-loss orders left on the coin by an earlier run.
//! 2. Place a position take-profit and stop-loss far from the mark, with size
//!    `0`, and save the open orders to see how the venue lists them.
//! 3. Move that stop-loss with `batchModify`, and save the open orders again.
//! 4. Place a limit close worth just over $10, far from the mark, then
//!    cancel it.
//! 5. With `--second`, close just over $10 of each position at market in one
//!    request.
//! 6. With `--close-position`: close at market all but about $5 of the
//!    position, then the last $5 (to learn whether closing a whole position
//!    worth under $10 is allowed), then save the open orders to see whether
//!    the take-profit and stop-loss went with the position.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use galata_desk_core::api_key::ApiKey;
use galata_desk_core::network::Network;
use galata_desk_core::secrets::{Keychain, Secrets, api_key_name};
use galata_desk_core::trade::assets::{self, Asset};
use galata_desk_core::trade::exchange;
use galata_desk_core::trade::rules::{self, MARKET_SLIPPAGE_PCT, TRIGGER_SLIPPAGE_PCT};
use galata_desk_core::trade::sign::{
    self, Action, CancelOne, Grouping, Modify, OrderKind, OrderWire, Tpsl, Trigger,
};
use galata_desk_core::venue::{Http, Info, Transport};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{Value, json};

const NETWORK: Network = Network::Testnet;

struct Args {
    account: String,
    coin: String,
    second: Option<String>,
    out: PathBuf,
    orders_only: bool,
    close_position: bool,
}

fn args() -> Args {
    let mut account = None;
    let mut coin = None;
    let mut second = None;
    let mut out = PathBuf::from("tpsl-probe-out");
    let mut orders_only = false;
    let mut close_position = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--account" => account = it.next(),
            "--coin" => coin = it.next(),
            "--second" => second = it.next(),
            "--out" => out = it.next().map(PathBuf::from).unwrap_or(out),
            "--orders-only" => orders_only = true,
            "--close-position" => close_position = true,
            "--mainnet" => fail("this probe runs on testnet only"),
            other => fail(&format!("unknown argument {other}")),
        }
    }
    Args {
        account: account
            .unwrap_or_else(|| fail("--account is needed"))
            .to_lowercase(),
        coin: coin.unwrap_or_else(|| fail("--coin is needed")),
        second,
        out,
        orders_only,
        close_position,
    }
}

fn fail(why: &str) -> ! {
    eprintln!("tpsl_probe: {why}");
    std::process::exit(2)
}

/// The dex a venue coin name trades on: `xyz` for `xyz:NVDA`, empty for `BTC`.
fn dex_of(coin: &str) -> &str {
    coin.split_once(':').map(|(dex, _)| dex).unwrap_or("")
}

/// A position as this probe needs it.
struct Held {
    coin: String,
    asset: Asset,
    /// Signed.
    size: Decimal,
    mark: Decimal,
}

impl Held {
    fn long(&self) -> bool {
        self.size > Decimal::ZERO
    }
    /// `usd` dollars of the position at `price`, rounded to the size step,
    /// up or down.
    fn worth(&self, usd: u32, price: Decimal, up: bool) -> Decimal {
        let strategy = if up {
            RoundingStrategy::AwayFromZero
        } else {
            RoundingStrategy::ToZero
        };
        (Decimal::from(usd) / price)
            .round_dp_with_strategy(self.asset.sz_decimals, strategy)
            .normalize()
    }
    /// A price `pct` percent from the mark, on the profitable side when
    /// `profit`, rounded to the market's rules.
    fn away(&self, pct: u32, profit: bool) -> Decimal {
        rules::slippage(self.mark, self.long() == profit, &self.asset, pct)
    }
    fn order(&self, price: Decimal, size: &str, kind: OrderKind) -> OrderWire {
        OrderWire {
            a: self.asset.number,
            // Closing a long sells; closing a short buys.
            b: !self.long(),
            p: rules::wire(price),
            s: size.to_string(),
            r: true,
            t: kind,
        }
    }
    fn trigger(&self, at: Decimal, tpsl: Tpsl, size: &str) -> OrderWire {
        let worst = rules::slippage(at, !self.long(), &self.asset, TRIGGER_SLIPPAGE_PCT);
        self.order(
            worst,
            size,
            OrderKind::Trigger(Trigger {
                is_market: true,
                trigger_px: rules::wire(at),
                tpsl,
            }),
        )
    }
}

struct Probe {
    info: Info<Http>,
    raw_info: Http,
    exchange: Http,
    key: ApiKey,
    account: String,
    out: PathBuf,
    last_nonce: u64,
}

impl Probe {
    /// The position in `coin`, or `None` when there is none.
    async fn held(&self, coin: &str) -> Option<Held> {
        let dex = dex_of(coin);
        let dexes = self.info.perp_dexs().await.expect("perpDexs answers");
        let meta = self.info.perp_meta(dex).await.expect("meta answers");
        let asset = assets::perp(&dexes, dex, &meta, coin)
            .unwrap_or_else(|| fail(&format!("{coin} is not a market on testnet")));
        let state = self
            .info
            .clearinghouse_state(&self.account, dex)
            .await
            .expect("clearinghouseState answers");
        let position = state
            .asset_positions
            .into_iter()
            .map(|p| p.position)
            .find(|p| p.coin == coin)?;
        let held = Held {
            coin: coin.to_string(),
            asset,
            size: position.szi,
            mark: position.position_value / position.szi.abs(),
        };
        println!(
            "{coin}: size {} mark {} (worth ${}) asset {} size decimals {}",
            held.size,
            held.mark,
            (held.mark * held.size.abs()).round_dp(2),
            held.asset.number,
            held.asset.sz_decimals
        );
        Some(held)
    }

    async fn mid(&self, coin: &str) -> Decimal {
        let mids = self
            .info
            .all_mids_on(dex_of(coin))
            .await
            .expect("allMids answers");
        *mids
            .get(coin)
            .unwrap_or_else(|| fail(&format!("no mid for {coin}")))
    }

    fn nonce(&mut self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        self.last_nonce = now.max(self.last_nonce + 1);
        self.last_nonce
    }

    async fn send(&mut self, step: &str, action: Action) -> Value {
        let nonce = self.nonce();
        let signature = sign::sign(&self.key, &action, nonce, NETWORK);
        println!("\n== {step}");
        println!(
            "sent:     {}",
            serde_json::to_string(&action).expect("an action serialises")
        );
        let answer = match self
            .exchange
            .post(exchange::request(&action, nonce, &signature))
            .await
        {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes))),
            Err(e) => json!(format!("no answer: {e}")),
        };
        println!("answered: {answer}");
        answer
    }

    /// Save the account's open orders on `dex`, as the venue lists them, and
    /// print the trigger orders on `coin`.
    async fn save_orders(&self, coin: &str, name: &str) -> Vec<Listed> {
        let mut body = json!({ "type": "frontendOpenOrders", "user": self.account });
        let dex = dex_of(coin);
        if !dex.is_empty() {
            body["dex"] = json!(dex);
        }
        let bytes = self
            .raw_info
            .post(body)
            .await
            .expect("frontendOpenOrders answers");
        let value: Value = serde_json::from_slice(&bytes).expect("the answer is JSON");
        std::fs::create_dir_all(&self.out).expect("the output directory can be made");
        let path = self.out.join(format!("{name}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap() + "\n")
            .expect("the file can be written");
        println!("\n== saved open orders to {}", path.display());
        let listed = triggers(&value, coin);
        if listed.is_empty() {
            println!("no trigger orders on {coin}");
        }
        for l in &listed {
            println!(
                "  {} oid {} isPositionTpsl {} sz {}",
                l.kind, l.oid, l.position_tpsl, l.size
            );
        }
        listed
    }
}

/// A trigger order as the venue lists it.
struct Listed {
    oid: u64,
    kind: String,
    position_tpsl: bool,
    size: String,
}

fn triggers(orders: &Value, coin: &str) -> Vec<Listed> {
    orders
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["coin"] == coin && o["isTrigger"] == true)
        .map(|o| Listed {
            oid: o["oid"].as_u64().unwrap_or_default(),
            kind: o["orderType"].as_str().unwrap_or_default().to_string(),
            position_tpsl: o["isPositionTpsl"] == true,
            size: o["sz"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

#[tokio::main]
async fn main() {
    let args = args();
    let key = Keychain::new()
        .get(api_key_name(NETWORK))
        .unwrap_or_else(|e| fail(&format!("the keychain refused: {e}")))
        .unwrap_or_else(|| fail("no testnet API key is saved in the app"));
    let key = ApiKey::parse(&key).unwrap_or_else(|_| fail("the saved testnet key is not a key"));
    println!("signing wallet: {}", key.address());

    let mut probe = Probe {
        info: Info::new(Http::at(NETWORK.info_url())),
        raw_info: Http::at(NETWORK.info_url()),
        exchange: Http::at(NETWORK.exchange_url()),
        key,
        account: args.account.clone(),
        out: args.out.clone(),
        last_nonce: 0,
    };
    let coin = args.coin.clone();

    if args.orders_only {
        probe.save_orders(&coin, "open_orders_now").await;
        return;
    }

    let held = probe
        .held(&coin)
        .await
        .unwrap_or_else(|| fail(&format!("the account holds no {coin} position")));

    // 1. Clear take-profits and stop-losses an earlier run left behind.
    let left = probe.save_orders(&coin, "open_orders_before").await;
    if !left.is_empty() {
        probe
            .send(
                "1. cancel trigger orders left by an earlier run",
                Action::Cancel {
                    cancels: left
                        .iter()
                        .map(|l| CancelOne {
                            a: held.asset.number,
                            o: l.oid,
                        })
                        .collect(),
                },
            )
            .await;
    }

    // 2. A position take-profit and stop-loss with size 0, far enough not
    //    to trigger.
    let tp = held.away(25, true);
    let sl = held.away(25, false);
    probe
        .send(
            "2. position TP and SL, size 0",
            Action::Order {
                orders: vec![
                    held.trigger(tp, Tpsl::Tp, "0"),
                    held.trigger(sl, Tpsl::Sl, "0"),
                ],
                grouping: Grouping::PositionTpsl,
            },
        )
        .await;
    let listed = probe.save_orders(&coin, "open_orders_after_tpsl").await;

    // 3. Move the stop-loss further away, in place, keeping its listed size.
    match listed.iter().find(|l| l.kind.contains("Stop")) {
        Some(stop) => {
            let size = if stop.position_tpsl {
                "0".to_string()
            } else {
                stop.size.clone()
            };
            let moved = held.away(30, false);
            probe
                .send(
                    "3. move the SL with batchModify",
                    Action::BatchModify {
                        modifies: vec![Modify {
                            oid: stop.oid,
                            order: held.trigger(moved, Tpsl::Sl, &size),
                        }],
                    },
                )
                .await;
            probe.save_orders(&coin, "open_orders_after_modify").await;
        }
        None => println!("\n== 3. skipped: no stop-loss is listed to move"),
    }

    // 4. A resting limit close worth just over $10, far from the mark, then
    //    cancelled.
    let far = held.away(50, true);
    let size = held.worth(11, far, true);
    if size > held.size.abs() {
        println!("\n== 4. skipped: the position is worth less than $11 at {far}");
    } else {
        let answer = probe
            .send(
                "4a. limit close worth just over $10 (GTC)",
                Action::Order {
                    orders: vec![held.order(far, &rules::wire(size), OrderKind::limit("Gtc"))],
                    grouping: Grouping::Na,
                },
            )
            .await;
        match answer["response"]["data"]["statuses"][0]["resting"]["oid"].as_u64() {
            Some(oid) => {
                probe
                    .send(
                        "4b. cancel that limit close",
                        Action::Cancel {
                            cancels: vec![CancelOne {
                                a: held.asset.number,
                                o: oid,
                            }],
                        },
                    )
                    .await;
            }
            None => println!("\n== 4b. skipped: the limit close is not resting"),
        }
    }

    // 5. Two positions, on two dexes, in one request.
    if let Some(second) = &args.second {
        let other = probe
            .held(second)
            .await
            .unwrap_or_else(|| fail(&format!("the account holds no {second} position")));
        let mut orders = Vec::new();
        for h in [&held, &other] {
            let mid = probe.mid(&h.coin).await;
            let worst = rules::slippage(mid, !h.long(), &h.asset, MARKET_SLIPPAGE_PCT);
            let size = h.worth(11, mid, true).min(h.size.abs());
            orders.push(h.order(worst, &rules::wire(size), OrderKind::limit("Ioc")));
        }
        probe
            .send(
                "5. market close of just over $10 of each, in one request",
                Action::Order {
                    orders,
                    grouping: Grouping::Na,
                },
            )
            .await;
    }

    if !args.close_position {
        println!(
            "\nThe take-profit and stop-loss are left in place. Run again with\n\
             --close-position to close the position and see whether they go with it."
        );
        return;
    }

    // 6a. All but about $5, at market.
    let Some(held) = probe.held(&coin).await else {
        fail("the position is gone already")
    };
    let mid = probe.mid(&coin).await;
    let worst = rules::slippage(mid, !held.long(), &held.asset, MARKET_SLIPPAGE_PCT);
    let keep = held.worth(5, mid, false);
    let part = held.size.abs() - keep;
    if keep.is_zero() || part * mid < Decimal::from(11) {
        println!("\n== 6a. skipped: the position is too small to leave $5 and close over $10");
    } else {
        probe
            .send(
                "6a. market close of all but about $5",
                Action::Order {
                    orders: vec![held.order(worst, &rules::wire(part), OrderKind::limit("Ioc"))],
                    grouping: Grouping::Na,
                },
            )
            .await;
    }

    // 6b. The rest, whatever it is worth.
    if let Some(rest) = probe.held(&coin).await {
        let mid = probe.mid(&coin).await;
        let worst = rules::slippage(mid, !rest.long(), &rest.asset, MARKET_SLIPPAGE_PCT);
        probe
            .send(
                "6b. market close of the whole rest of the position",
                Action::Order {
                    orders: vec![rest.order(
                        worst,
                        &rules::wire(rest.size.abs()),
                        OrderKind::limit("Ioc"),
                    )],
                    grouping: Grouping::Na,
                },
            )
            .await;
    }

    // 6c. Did the take-profit and stop-loss go with the position?
    tokio::time::sleep(Duration::from_secs(2)).await;
    match probe.held(&coin).await {
        Some(_) => println!("\nthe position is still open"),
        None => println!("\nthe position is closed"),
    }
    probe.save_orders(&coin, "open_orders_after_close").await;
}
