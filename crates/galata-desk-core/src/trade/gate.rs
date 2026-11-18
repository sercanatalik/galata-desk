//! The one way an action leaves the app.
//!
//! Every action passes the same checks, in the same order, before the key is
//! read: no development override, mainnet trading switched on, a key saved,
//! and that key approved for the connected account. The view asks
//! [`Trader::availability`] to decide what to draw, and the action asks it
//! again: a disabled button is a courtesy, this is the guarantee.
//!
//! The key is read from the keychain for one action and dropped. It is never
//! kept here, and no error or `Debug` output carries it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rust_decimal::Decimal;

use super::assets::Asset;
use super::exchange::{self, Answer};
use super::rules::{self, Checked};
use super::sign::{self, Action, CancelOne, Grouping, Modify, OrderKind, OrderWire, Tpsl, Trigger};
use crate::api_key::ApiKey;
use crate::connect::Address;
use crate::network::Network;
use crate::secrets::{Secrets, api_key_name};
use crate::venue::{Info, Side, Transport, VenueError};

/// Why no action can be taken now.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unavailable {
    /// A development endpoint override is in force: the app is read-only.
    #[error("a development endpoint override is in force, so the app is read-only")]
    Overridden,
    #[error("trading on mainnet is switched off")]
    MainnetOff,
    #[error("no API key is saved for {}", .0.label())]
    NoKey(Network),
    /// The venue does not list the key's wallet as an API wallet of the
    /// connected account.
    #[error("the saved key cannot act for this account")]
    KeyNotApproved,
    /// The venue could not be asked about the key. Not remembered: the next
    /// action asks again.
    #[error("the key could not be checked: {0}")]
    KeyCheckFailed(String),
    /// The keychain refused, or holds something that is not a key.
    #[error("the saved key could not be read: {0}")]
    Keychain(String),
}

/// What the gate needs to know about where the app stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub network: Network,
    /// A development endpoint override is in force.
    pub overridden: bool,
    /// The user has switched on trading on mainnet.
    pub mainnet_allowed: bool,
    /// The connected account.
    pub account: Address,
}

/// An order to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// The market's asset number.
    pub asset: u32,
    pub oid: u64,
}

/// What an amended order keeps from the order it replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub side: Side,
    pub reduce_only: bool,
    /// Time in force, as the venue spells it.
    pub tif: String,
}

/// A position to close, as the view knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Holding {
    pub asset: Asset,
    /// Signed: negative is short.
    pub size: Decimal,
}

impl Holding {
    /// The order that takes `size` off this position at `price`. Always
    /// reduce-only, and always on the other side: a long is closed by
    /// selling, a short by buying. Nothing here can add to a position or
    /// turn it around.
    fn closing(&self, price: Decimal, size: Decimal, tif: &str) -> OrderWire {
        OrderWire {
            a: self.asset.number,
            b: self.size < Decimal::ZERO,
            p: rules::wire(price),
            s: rules::wire(size),
            r: true,
            t: OrderKind::limit(tif),
        }
    }
}

impl Holding {
    /// A position-wide take-profit or stop-loss at `at`, closing the whole
    /// position at market when triggered. Size `0` is how the venue takes a
    /// position-wide one: it then covers the position whatever its size. The
    /// order's own price bounds the market fill, 10% past the trigger.
    fn trigger(&self, at: Decimal, tpsl: Tpsl) -> OrderWire {
        let buy = self.size < Decimal::ZERO;
        let worst = rules::slippage(at, buy, &self.asset, rules::TRIGGER_SLIPPAGE_PCT);
        OrderWire {
            a: self.asset.number,
            b: buy,
            p: rules::wire(worst),
            s: "0".into(),
            r: true,
            t: OrderKind::Trigger(Trigger {
                is_market: true,
                trigger_px: rules::wire(at),
                tpsl,
            }),
        }
    }
}

/// What to do with one of a position's take-profit and stop-loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Leave it as it is, set or not.
    Keep,
    /// Set it at this price: move it if it is set, place it if not.
    At(Decimal),
    /// Remove it.
    Remove,
}

/// A position's take-profit and stop-loss as the venue lists them now, by
/// order id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TpslNow {
    pub tp: Option<u64>,
    pub sl: Option<u64>,
}

/// One request of a TP/SL change, and what came of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TpslStep {
    pub kind: TpslRequest,
    /// The levels the request was about, in the order of its outcomes.
    pub levels: Vec<Tpsl>,
    pub answer: Result<Answer, Unavailable>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpslRequest {
    Move,
    Place,
    Remove,
}

/// How a close is priced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pricing {
    /// Fill at once at this price or better, or not at all.
    Market { worst: Decimal },
    /// Rest on the book at this price.
    Limit { price: Decimal },
}

pub struct Trader<T> {
    info: Info<T>,
    exchange: T,
    secrets: Arc<dyn Secrets>,
    standing: Standing,
    /// Whether a key's wallet may act for the account, by wallet address.
    /// Only answers the venue gave are kept.
    approved: Mutex<HashMap<String, bool>>,
    last_nonce: AtomicU64,
}

impl<T: Transport + Clone> Trader<T> {
    /// `info` asks the venue questions; `exchange` carries actions.
    pub fn new(info: T, exchange: T, secrets: Arc<dyn Secrets>, standing: Standing) -> Self {
        Self {
            info: Info::new(info),
            exchange,
            secrets,
            standing,
            approved: Mutex::default(),
            last_nonce: AtomicU64::new(0),
        }
    }

    pub fn standing(&self) -> &Standing {
        &self.standing
    }

    /// The saved key for the network in use.
    fn key(&self) -> Result<ApiKey, Unavailable> {
        let network = self.standing.network;
        let stored = self
            .secrets
            .get(api_key_name(network))
            .map_err(|e| Unavailable::Keychain(e.to_string()))?
            .ok_or(Unavailable::NoKey(network))?;
        ApiKey::parse(&stored).map_err(|_| Unavailable::Keychain("it is not a key".into()))
    }

    /// The wallet the saved key belongs to, to show who would sign. Reads
    /// the keychain; `None` when there is no readable key.
    pub fn wallet(&self) -> Option<Address> {
        self.key().ok().map(|key| key.address())
    }

    /// Whether an action could be taken now, without asking the venue.
    /// Reads the keychain.
    pub fn availability(&self) -> Result<(), Unavailable> {
        if self.standing.overridden {
            return Err(Unavailable::Overridden);
        }
        if self.standing.network == Network::Mainnet && !self.standing.mainnet_allowed {
            return Err(Unavailable::MainnetOff);
        }
        let key = self.key()?;
        match self.approved.lock().unwrap().get(key.address().as_str()) {
            Some(false) => Err(Unavailable::KeyNotApproved),
            _ => Ok(()),
        }
    }

    /// The key, once every check has passed, the venue's included.
    async fn cleared(&self) -> Result<ApiKey, Unavailable> {
        self.availability()?;
        let key = self.key()?;
        let wallet = key.address();
        let known = self.approved.lock().unwrap().get(wallet.as_str()).copied();
        let approved = match known {
            Some(approved) => approved,
            None => {
                let role = self
                    .info
                    .user_role(wallet.as_str())
                    .await
                    .map_err(|e| Unavailable::KeyCheckFailed(e.to_string()))?;
                let acts_for = role.data.and_then(|d| d.user).unwrap_or_default();
                let approved = role.role == "agent"
                    && acts_for.eq_ignore_ascii_case(self.standing.account.as_str());
                self.approved
                    .lock()
                    .unwrap()
                    .insert(wallet.as_str().to_string(), approved);
                approved
            }
        };
        if approved {
            Ok(key)
        } else {
            Err(Unavailable::KeyNotApproved)
        }
    }

    /// The time in milliseconds, and never a number used before: the venue
    /// refuses a nonce it has seen.
    fn nonce(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut next = now;
        let _ = self
            .last_nonce
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |last| {
                next = now.max(last + 1);
                Some(next)
            });
        next
    }

    async fn act(&self, action: Action) -> Result<Answer, Unavailable> {
        let key = self.cleared().await?;
        let nonce = self.nonce();
        let signature = sign::sign(&key, &action, nonce, self.standing.network);
        // The key has done its work.
        drop(key);
        Ok(exchange::send(&self.exchange, &action, nonce, &signature).await)
    }

    /// The mid price of `coin` (without a dex prefix) on `dex`, to price a
    /// market close. A public question: no guard applies.
    pub async fn mid(&self, dex: &str, coin: &str) -> Result<Option<Decimal>, VenueError> {
        self.info.mid(dex, coin).await
    }

    /// Take `size` off a position, at market or at a limit.
    pub async fn close(
        &self,
        position: &Holding,
        size: Decimal,
        pricing: Pricing,
    ) -> Result<Answer, Unavailable> {
        let order = match pricing {
            Pricing::Market { worst } => position.closing(worst, size, "Ioc"),
            Pricing::Limit { price } => position.closing(price, size, "Gtc"),
        };
        self.act(Action::Order {
            orders: vec![order],
            grouping: Grouping::Na,
        })
        .await
    }

    /// Close each position whole, at market, in one request. The answer has
    /// one outcome per position, in the same order.
    pub async fn close_all(&self, positions: &[(Holding, Decimal)]) -> Result<Answer, Unavailable> {
        self.act(Action::Order {
            orders: positions
                .iter()
                .map(|(p, worst)| p.closing(*worst, p.size.abs(), "Ioc"))
                .collect(),
            grouping: Grouping::Na,
        })
        .await
    }

    /// Set, move or remove a position's take-profit and stop-loss.
    ///
    /// Up to three requests, in this order: move the levels that exist,
    /// place the ones that do not, remove the ones cleared. A level that
    /// exists is never cancelled to be placed again, so the position is
    /// never left without it while it moves. Each request is reported; a
    /// later one is sent even when an earlier one was refused, since each
    /// concerns other levels. Nothing is sent when nothing changes.
    pub async fn set_tpsl(
        &self,
        position: &Holding,
        now: TpslNow,
        tp: Level,
        sl: Level,
    ) -> Vec<TpslStep> {
        let levels = [(Tpsl::Tp, now.tp, tp), (Tpsl::Sl, now.sl, sl)];
        let mut moves = Vec::new();
        let mut places = Vec::new();
        let mut removes = Vec::new();
        for (kind, oid, level) in levels {
            match (level, oid) {
                (Level::At(price), Some(oid)) => moves.push((kind, oid, price)),
                (Level::At(price), None) => places.push((kind, price)),
                (Level::Remove, Some(oid)) => removes.push((kind, oid)),
                (Level::Remove, None) | (Level::Keep, _) => {}
            }
        }
        let mut steps = Vec::new();
        if !moves.is_empty() {
            let action = Action::BatchModify {
                modifies: moves
                    .iter()
                    .map(|(kind, oid, price)| Modify {
                        oid: *oid,
                        order: position.trigger(*price, *kind),
                    })
                    .collect(),
            };
            steps.push(TpslStep {
                kind: TpslRequest::Move,
                levels: moves.iter().map(|m| m.0).collect(),
                answer: self.act(action).await,
            });
        }
        if !places.is_empty() {
            let action = Action::Order {
                orders: places
                    .iter()
                    .map(|(kind, price)| position.trigger(*price, *kind))
                    .collect(),
                grouping: Grouping::PositionTpsl,
            };
            steps.push(TpslStep {
                kind: TpslRequest::Place,
                levels: places.iter().map(|p| p.0).collect(),
                answer: self.act(action).await,
            });
        }
        if !removes.is_empty() {
            let action = Action::Cancel {
                cancels: removes
                    .iter()
                    .map(|(_, oid)| CancelOne {
                        a: position.asset.number,
                        o: *oid,
                    })
                    .collect(),
            };
            steps.push(TpslStep {
                kind: TpslRequest::Remove,
                levels: removes.iter().map(|r| r.0).collect(),
                answer: self.act(action).await,
            });
        }
        steps
    }

    /// Cancel these orders, in one request. The answer has one outcome per
    /// order, in the same order.
    pub async fn cancel(&self, orders: &[Target]) -> Result<Answer, Unavailable> {
        self.act(Action::Cancel {
            cancels: orders
                .iter()
                .map(|t| CancelOne {
                    a: t.asset,
                    o: t.oid,
                })
                .collect(),
        })
        .await
    }

    /// Replace an order with one at a checked price and size, keeping its
    /// side, reduce-only setting and time in force.
    pub async fn amend(
        &self,
        order: Target,
        to: &Checked,
        kept: &Kept,
    ) -> Result<Answer, Unavailable> {
        self.act(Action::BatchModify {
            modifies: vec![Modify {
                oid: order.oid,
                order: OrderWire {
                    a: order.asset,
                    b: kept.side == Side::Buy,
                    p: to.price_text.clone(),
                    s: to.size_text.clone(),
                    r: kept.reduce_only,
                    t: OrderKind::limit(kept.tif.clone()),
                },
            }],
        })
        .await
    }
}
