//! Acting on resting orders from the Open orders tab: amend one, cancel one,
//! cancel all.
//!
//! The view never signs and never sees the key. It asks a [`Trading`] whether
//! acting is possible, shows what an action would do, and sends it only when
//! the user confirms. The real `Trading` is the gate in the core, which makes
//! every check again before it reads the key.

#[cfg(any(test, feature = "shot"))]
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use galata_desk_core::trade::rules::{self, Checked, Current, Rule};
use galata_desk_core::trade::{
    Answer, Holding, Kept, Level, NewOrder, Outcome, Pricing, Standing, Target, TpslNow,
    TpslRequest, TpslStep, Trader, Unavailable,
};
use galata_desk_core::venue::ActiveAsset;
use gpui_kit::component::Disableable;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{App, FocusHandle, KeyDownEvent, Subscription};

use super::*;
use crate::grid::{Glyph, RowButton};

/// The venue's answer, some time later.
pub type Reply = Pin<Box<dyn Future<Output = Result<Answer, Unavailable>>>>;

/// A mid price, some time later: `None` when the venue lists none, or why
/// it could not be had.
pub type MidReply = Pin<Box<dyn Future<Output = Result<Option<Decimal>, String>>>>;

/// What the account can trade in a market, some time later, or why it could
/// not be had.
pub type ActiveReply = Pin<Box<dyn Future<Output = Result<ActiveAsset, String>>>>;

/// A market's asset number and size step, some time later: `None` when the
/// venue does not list it.
pub type MarketReply =
    Pin<Box<dyn Future<Output = Result<Option<galata_desk_core::trade::assets::Asset>, String>>>>;

/// What came of each request of a take-profit and stop-loss change.
pub type TpslReply = Pin<Box<dyn Future<Output = Vec<TpslStep>>>>;

/// What acts on orders and positions: the gate in the core, or a stand-in.
pub trait Trading {
    /// Whether an action could be taken now. Reads the keychain; never asks
    /// the venue.
    fn availability(&self) -> Result<(), Unavailable>;
    /// The address of the saved key's wallet, to show who signs.
    fn wallet(&self) -> Option<String>;
    fn cancel(&self, orders: Vec<Target>) -> Reply;
    fn amend(&self, order: Target, to: Checked, kept: Kept) -> Reply;
    /// The mid price of `coin` on `dex`, to price a market close.
    fn mid(&self, dex: String, coin: String) -> MidReply;
    fn close(&self, position: Holding, size: Decimal, pricing: Pricing) -> Reply;
    /// Close each position whole at market, at its worst price.
    fn close_all(&self, positions: Vec<(Holding, Decimal)>) -> Reply;
    fn set_tpsl(&self, position: Holding, now: TpslNow, tp: Level, sl: Level) -> TpslReply;
    /// Place an order from the order ticket: the one action that may open
    /// or add to a position.
    fn place(&self, order: NewOrder) -> Reply;
    /// The account's leverage and tradeable size in `coin`, the venue's
    /// name for the market (`HYPE`, `xyz:NVDA`).
    fn active_asset(&self, coin: String) -> ActiveReply;
    /// The market `coin` (the venue's name) on `dex`, as an action names it.
    fn market(&self, dex: String, coin: String) -> MarketReply;
}

/// The gate, run on the I/O runtime.
pub struct Live {
    trader: Arc<Trader<Http>>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl Live {
    pub fn new(account: &Address, cx: &App) -> Self {
        let core = Core::get(cx);
        let trader = Trader::new(
            Http::at(core.info_url()),
            Http::at(core.network.exchange_url()),
            core.secrets.clone(),
            Standing {
                network: core.network,
                overridden: core.overridden(),
                mainnet_allowed: core.allow_mainnet_trading,
                account: account.clone(),
            },
        );
        Self {
            trader: Arc::new(trader),
            runtime: core.runtime.clone(),
        }
    }

    /// Run on the I/O runtime; this side only waits for the answer.
    fn run<F>(&self, action: F) -> Reply
    where
        F: Future<Output = Result<Answer, Unavailable>> + Send + 'static,
    {
        // The task dropped mid-request: nothing is known.
        self.spawn(action, || {
            Ok(Answer::Unknown("the request was interrupted".into()))
        })
    }

    /// Run on the I/O runtime, with what to say if the task is lost.
    fn spawn<F, T>(
        &self,
        work: F,
        lost: impl FnOnce() -> T + 'static,
    ) -> Pin<Box<dyn Future<Output = T>>>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let handle = self.runtime.spawn(work);
        Box::pin(async move { handle.await.unwrap_or_else(|_| lost()) })
    }
}

impl Trading for Live {
    fn availability(&self) -> Result<(), Unavailable> {
        self.trader.availability()
    }

    fn wallet(&self) -> Option<String> {
        self.trader.wallet().map(|a| a.to_string())
    }

    fn cancel(&self, orders: Vec<Target>) -> Reply {
        let trader = self.trader.clone();
        self.run(async move { trader.cancel(&orders).await })
    }

    fn amend(&self, order: Target, to: Checked, kept: Kept) -> Reply {
        let trader = self.trader.clone();
        self.run(async move { trader.amend(order, &to, &kept).await })
    }

    fn mid(&self, dex: String, coin: String) -> MidReply {
        let trader = self.trader.clone();
        self.spawn(
            async move { trader.mid(&dex, &coin).await.map_err(|e| e.to_string()) },
            || Err("the request was interrupted".into()),
        )
    }

    fn close(&self, position: Holding, size: Decimal, pricing: Pricing) -> Reply {
        let trader = self.trader.clone();
        self.run(async move { trader.close(&position, size, pricing).await })
    }

    fn close_all(&self, positions: Vec<(Holding, Decimal)>) -> Reply {
        let trader = self.trader.clone();
        self.run(async move { trader.close_all(&positions).await })
    }

    fn place(&self, order: NewOrder) -> Reply {
        let trader = self.trader.clone();
        self.run(async move { trader.place(&order).await })
    }

    fn active_asset(&self, coin: String) -> ActiveReply {
        let trader = self.trader.clone();
        self.spawn(
            async move { trader.active_asset(&coin).await.map_err(|e| e.to_string()) },
            || Err("the request was interrupted".into()),
        )
    }

    fn market(&self, dex: String, coin: String) -> MarketReply {
        let trader = self.trader.clone();
        self.spawn(
            async move { trader.market(&dex, &coin).await.map_err(|e| e.to_string()) },
            || Err("the request was interrupted".into()),
        )
    }

    fn set_tpsl(&self, position: Holding, now: TpslNow, tp: Level, sl: Level) -> TpslReply {
        let trader = self.trader.clone();
        self.spawn(
            async move { trader.set_tpsl(&position, now, tp, sl).await },
            // Lost mid-way: which requests went out is not known.
            || {
                vec![TpslStep {
                    kind: TpslRequest::Place,
                    levels: Vec::new(),
                    answer: Ok(Answer::Unknown("the request was interrupted".into())),
                }]
            },
        )
    }
}

/// A stand-in that sends nothing: tests and screenshots set what it says and
/// read what it was asked to do.
#[cfg(any(test, feature = "shot"))]
pub struct Staged {
    pub availability: RefCell<Result<(), Unavailable>>,
    /// Answers to give, in turn. When there are none, every order succeeds.
    pub answers: RefCell<std::collections::VecDeque<Result<Answer, Unavailable>>>,
    /// Never answer, so the in-progress state can be looked at.
    pub hold: std::cell::Cell<bool>,
    pub cancels: RefCell<Vec<Vec<Target>>>,
    pub amends: RefCell<Vec<(Target, Checked, Kept)>>,
    /// What `mid` answers.
    pub mid: RefCell<Result<Option<Decimal>, String>>,
    pub closes: RefCell<Vec<(Holding, Decimal, Pricing)>>,
    pub close_alls: RefCell<Vec<Vec<(Holding, Decimal)>>>,
    pub tpsl_changes: RefCell<Vec<(Holding, TpslNow, Level, Level)>>,
    /// Answers to TP/SL changes, in turn. When there are none, every
    /// request succeeds.
    pub tpsl_answers: RefCell<std::collections::VecDeque<Vec<TpslStep>>>,
    pub placed: RefCell<Vec<NewOrder>>,
    /// What `active_asset` answers.
    pub active: RefCell<Result<ActiveAsset, String>>,
    /// What `market` answers.
    pub market: RefCell<Result<Option<galata_desk_core::trade::assets::Asset>, String>>,
}

#[cfg(any(test, feature = "shot"))]
impl Staged {
    pub fn new(availability: Result<(), Unavailable>) -> Self {
        Self {
            availability: RefCell::new(availability),
            answers: RefCell::default(),
            hold: std::cell::Cell::new(false),
            cancels: RefCell::default(),
            amends: RefCell::default(),
            mid: RefCell::new(Ok(Some(Decimal::from(154)))),
            closes: RefCell::default(),
            close_alls: RefCell::default(),
            tpsl_changes: RefCell::default(),
            tpsl_answers: RefCell::default(),
            placed: RefCell::default(),
            active: RefCell::new(Ok(staged_active_asset())),
            market: RefCell::new(Ok(Some(galata_desk_core::trade::assets::Asset {
                number: 4,
                sz_decimals: 2,
                spot: false,
            }))),
        }
    }

    fn reply(&self, orders: usize) -> Reply {
        if self.hold.get() {
            return Box::pin(std::future::pending());
        }
        let answer = self
            .answers
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Ok(Answer::Each(vec![Outcome::DONE; orders])));
        Box::pin(async move { answer })
    }
}

/// A stand-in account: 5x cross, up to 30 of the coin either way.
#[cfg(any(test, feature = "shot"))]
pub fn staged_active_asset() -> ActiveAsset {
    serde_json::from_value(serde_json::json!({
        "leverage": { "type": "cross", "value": 5 },
        "maxTradeSzs": ["30", "30"],
        "availableToTrade": ["3737.4", "3737.4"],
        "markPx": "66.728"
    }))
    .expect("a well-formed answer")
}

#[cfg(any(test, feature = "shot"))]
impl Trading for Staged {
    fn availability(&self) -> Result<(), Unavailable> {
        self.availability.borrow().clone()
    }

    fn wallet(&self) -> Option<String> {
        Some("0x8a3f1c5e9b7d2a4f6c8e0b1d3f5a7c9e1b3dc21d".into())
    }

    fn cancel(&self, orders: Vec<Target>) -> Reply {
        let count = orders.len();
        self.cancels.borrow_mut().push(orders);
        self.reply(count)
    }

    fn amend(&self, order: Target, to: Checked, kept: Kept) -> Reply {
        self.amends.borrow_mut().push((order, to, kept));
        self.reply(1)
    }

    fn mid(&self, _dex: String, _coin: String) -> MidReply {
        if self.hold.get() {
            return Box::pin(std::future::pending());
        }
        let mid = self.mid.borrow().clone();
        Box::pin(async move { mid })
    }

    fn close(&self, position: Holding, size: Decimal, pricing: Pricing) -> Reply {
        self.closes.borrow_mut().push((position, size, pricing));
        self.reply(1)
    }

    fn close_all(&self, positions: Vec<(Holding, Decimal)>) -> Reply {
        let count = positions.len();
        self.close_alls.borrow_mut().push(positions);
        self.reply(count)
    }

    fn place(&self, order: NewOrder) -> Reply {
        let orders = 1 + order.take_profit.is_some() as usize + order.stop_loss.is_some() as usize;
        self.placed.borrow_mut().push(order);
        self.reply(orders)
    }

    fn market(&self, _dex: String, _coin: String) -> MarketReply {
        if self.hold.get() {
            return Box::pin(std::future::pending());
        }
        let market = self.market.borrow().clone();
        Box::pin(async move { market })
    }

    fn active_asset(&self, _coin: String) -> ActiveReply {
        if self.hold.get() {
            return Box::pin(std::future::pending());
        }
        let active = self.active.borrow().clone();
        Box::pin(async move { active })
    }

    fn set_tpsl(&self, position: Holding, now: TpslNow, tp: Level, sl: Level) -> TpslReply {
        self.tpsl_changes.borrow_mut().push((position, now, tp, sl));
        if self.hold.get() {
            return Box::pin(std::future::pending());
        }
        let steps = self
            .tpsl_answers
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| {
                // Every request a real gate would send, each done.
                let mut steps = Vec::new();
                let mut each = |kind, levels: Vec<_>| {
                    if !levels.is_empty() {
                        let n = levels.len();
                        steps.push(TpslStep {
                            kind,
                            levels,
                            answer: Ok(Answer::Each(vec![Outcome::DONE; n])),
                        });
                    }
                };
                use galata_desk_core::trade::sign::Tpsl;
                let pairs = [(Tpsl::Tp, now.tp, tp), (Tpsl::Sl, now.sl, sl)];
                let pick = |want: fn(Level, Option<u64>) -> bool| {
                    pairs
                        .iter()
                        .filter(|(_, oid, level)| want(*level, *oid))
                        .map(|(kind, _, _)| *kind)
                        .collect::<Vec<_>>()
                };
                each(
                    TpslRequest::Move,
                    pick(|l, o| matches!(l, Level::At(_)) && o.is_some()),
                );
                each(
                    TpslRequest::Place,
                    pick(|l, o| matches!(l, Level::At(_)) && o.is_none()),
                );
                each(
                    TpslRequest::Remove,
                    pick(|l, o| l == Level::Remove && o.is_some()),
                );
                steps
            });
        Box::pin(async move { steps })
    }
}

/// What is being asked of the user before anything is sent.
pub(super) enum Dialog {
    Cancel(OrderRow),
    /// The orders it was opened with: exactly these are cancelled.
    CancelAll(Vec<OrderRow>),
    Amend {
        order: OrderRow,
        price: Entity<InputState>,
        size: Entity<InputState>,
        _typing: [Subscription; 2],
    },
    /// A close, Close all, or a take-profit and stop-loss change.
    Position(super::positions::PositionDialog),
    /// The order ticket.
    Ticket(Box<super::ticket::Ticket>),
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Doing {
    Cancelling,
    Amending,
    Closing,
    SettingTpsl,
    Placing,
}

/// An action that is with the venue: the orders and the positions (by
/// asset number) it is about.
pub(super) struct Pending {
    pub(super) oids: Vec<u64>,
    pub(super) assets: Vec<u32>,
    pub(super) doing: Doing,
}

/// What the last action came to.
pub(super) struct Report {
    pub(super) text: SharedString,
    /// What was not done, when only part was.
    pub(super) more: Option<SharedString>,
    pub(super) problem: bool,
}

pub struct Actions {
    pub(super) trading: Rc<dyn Trading>,
    /// A stand-in was put in place: keep it when settings change.
    staged: bool,
    pub(super) availability: Result<(), Unavailable>,
    pub(super) wallet: Option<String>,
    pub(super) dialog: Option<Dialog>,
    pub(super) pending: Option<Pending>,
    pub(super) report: Option<Report>,
    pub(super) task: Option<Task<()>>,
    /// Fetching the mid prices a market close needs.
    pub(super) pricing: Option<Task<()>>,
    focus: FocusHandle,
    /// How many times the orders were asked for again after an action.
    pub(super) refreshes: usize,
    /// A screenshot run's state to show once the orders have loaded.
    #[cfg(feature = "shot")]
    shot: Option<String>,
    /// A screenshot run's Positions tab state, shown once positions load.
    #[cfg(feature = "shot")]
    pub(super) position_shot: Option<String>,
    /// A screenshot run's order ticket state, shown once Markets has a price.
    #[cfg(feature = "shot")]
    pub(super) ticket_shot: Option<String>,
}

impl Actions {
    pub fn new(account: &Address, cx: &mut Context<Portfolio>) -> Self {
        let trading: Rc<dyn Trading> = Rc::new(Live::new(account, cx));
        let availability = trading.availability();
        let wallet = availability.is_ok().then(|| trading.wallet()).flatten();
        Self {
            trading,
            staged: false,
            availability,
            wallet,
            dialog: None,
            pending: None,
            report: None,
            task: None,
            pricing: None,
            focus: cx.focus_handle(),
            refreshes: 0,
            #[cfg(feature = "shot")]
            shot: None,
            #[cfg(feature = "shot")]
            position_shot: None,
            #[cfg(feature = "shot")]
            ticket_shot: None,
        }
    }
}

pub(super) fn side_word(side: Side) -> &'static str {
    match side {
        Side::Buy => "Buy",
        Side::Sell => "Sell",
    }
}

fn market_label(o: &OrderRow) -> String {
    if o.dex.is_empty() {
        o.market.clone()
    } else {
        format!("{} ({})", o.market, o.dex)
    }
}

/// `Buy 12 NVDA at 154.20`.
fn describe(o: &OrderRow) -> String {
    format!(
        "{} {} {} at {}",
        side_word(o.side),
        f::amount(o.remaining),
        market_label(o),
        f::price(o.price)
    )
}

/// `0x8a3f…c21d`.
pub(super) fn short(address: &str) -> String {
    if address.len() > 12 {
        format!("{}\u{2026}{}", &address[..6], &address[address.len() - 4..])
    } else {
        address.to_string()
    }
}

/// What a tab's actions are, in the words of its notice.
#[derive(Clone, Copy)]
pub(super) enum Acting {
    Orders,
    Positions,
    Placing,
}

impl Acting {
    fn verb(self) -> &'static str {
        match self {
            Acting::Orders => "amend or cancel orders",
            Acting::Positions => "close positions or set take-profit and stop-loss",
            Acting::Placing => "place orders",
        }
    }
}

/// Why acting is not possible, as a heading and a sentence, and whether
/// Settings is where to fix it.
pub(super) fn explain(unavailable: &Unavailable, acting: Acting) -> (String, String, bool) {
    let verb = acting.verb();
    match unavailable {
        Unavailable::NoKey(network) => (
            format!("An API key is needed to {verb}"),
            format!(
                "No key is saved for {}. Save the private key of an API wallet in Settings.",
                network.label()
            ),
            true,
        ),
        Unavailable::KeyNotApproved => (
            "The saved key cannot act for this account".into(),
            "Hyperliquid does not list it as an approved API wallet for the connected account. Nothing was sent.".into(),
            true,
        ),
        Unavailable::MainnetOff => (
            "Trading on mainnet is switched off".into(),
            format!("Turn on \u{201c}Allow trading on mainnet\u{201d} in Settings to {verb} here."),
            true,
        ),
        Unavailable::Overridden => (
            match acting {
                Acting::Orders => "Order actions are off".into(),
                Acting::Positions => "Position actions are off".into(),
                Acting::Placing => "Placing orders is off".into(),
            },
            "The venue's addresses are set by the development environment, so the app is read-only.".into(),
            false,
        ),
        Unavailable::Keychain(why) => (
            "The saved API key could not be read".into(),
            format!("The keychain said: {why}. Remove the key in Settings and save it again."),
            true,
        ),
        Unavailable::KeyCheckFailed(why) => (
            "The key could not be checked".into(),
            format!("{why}. Nothing was sent; try again."),
            false,
        ),
    }
}

impl Portfolio {
    /// Look again at whether orders can be acted on: after a key is saved or
    /// removed, or the mainnet switch is flipped.
    pub fn reconsider_trading(&mut self, cx: &mut Context<Self>) {
        if !self.actions.staged {
            self.actions.trading = Rc::new(Live::new(&self.address, cx));
        }
        self.reread_availability(cx);
    }

    pub(super) fn reread_availability(&mut self, cx: &mut Context<Self>) {
        let a = &mut self.actions;
        a.availability = a.trading.availability();
        a.wallet = a.availability.is_ok().then(|| a.trading.wallet()).flatten();
        self.rebuild_orders(cx);
        cx.notify();
    }

    pub(super) fn can_act(&self) -> bool {
        self.actions.availability.is_ok() && self.actions.pending.is_none()
    }

    /// The Amend and Cancel buttons of one order's row.
    pub(super) fn order_buttons(&self, o: &OrderRow) -> Cell {
        let doing = self
            .actions
            .pending
            .as_ref()
            .filter(|p| p.oids.contains(&o.oid))
            .map(|p| p.doing);
        // A market the venue's list does not name cannot be acted on.
        let known = o.asset.is_some();
        let off = !self.can_act() || !known;
        let name = |action: &str, busy: &str, mine: bool| -> SharedString {
            if mine {
                busy.to_string().into()
            } else if !known {
                format!("{action}: this market's details are not loaded yet").into()
            } else {
                action.to_string().into()
            }
        };
        let mut buttons = Vec::new();
        if o.amendable {
            let mine = doing == Some(Doing::Amending);
            buttons.push(RowButton {
                id: "amend-order",
                key: o.oid,
                glyph: Glyph::Pencil,
                name: name("Amend order", "Amending\u{2026}", mine),
                disabled: off,
                busy: mine,
            });
        }
        let mine = doing == Some(Doing::Cancelling);
        buttons.push(RowButton {
            id: "cancel-order",
            key: o.oid,
            glyph: Glyph::Cross,
            name: name("Cancel order", "Cancelling\u{2026}", mine),
            disabled: off,
            busy: mine,
        });
        Cell::buttons(buttons)
    }

    /// A row's button was pressed.
    pub(super) fn press(
        &mut self,
        id: &str,
        oid: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_act() {
            return;
        }
        let Some(order) = self
            .snapshot
            .orders
            .data
            .as_ref()
            .and_then(|orders| orders.iter().find(|o| o.oid == oid))
            // An order in a market the venue's list does not name cannot be
            // acted on, however the press arrived.
            .filter(|o| o.asset.is_some())
            .cloned()
        else {
            return;
        };
        match id {
            "cancel-order" => self.open(Dialog::Cancel(order), window, cx),
            "amend-order" if order.amendable => {
                let field = |value: String, window: &mut Window, cx: &mut Context<Self>| {
                    let input = cx.new(|cx| InputState::new(window, cx));
                    input.update(cx, |input, cx| input.set_value(value, window, cx));
                    // Typing redraws the form: its checks are made as it is drawn.
                    let typing = cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            cx.notify();
                        }
                    });
                    (input, typing)
                };
                let (price, typing_price) = field(rules::wire(order.price), window, cx);
                let (size, typing_size) = field(rules::wire(order.remaining), window, cx);
                self.open(
                    Dialog::Amend {
                        order,
                        price,
                        size,
                        _typing: [typing_price, typing_size],
                    },
                    window,
                    cx,
                );
            }
            _ => {}
        }
    }

    fn ask_cancel_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_act() {
            return;
        }
        let orders = self.cancellable();
        if !orders.is_empty() {
            self.open(Dialog::CancelAll(orders), window, cx);
        }
    }

    /// The listed orders that can be acted on.
    fn cancellable(&self) -> Vec<OrderRow> {
        self.snapshot
            .orders
            .data
            .iter()
            .flatten()
            .filter(|o| o.asset.is_some())
            .cloned()
            .collect()
    }

    pub(super) fn open(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        self.actions.dialog = Some(dialog);
        // Keys go to the dialog: Escape backs out.
        window.focus(&self.actions.focus, cx);
        cx.notify();
    }

    pub(super) fn close_dialog(&mut self, cx: &mut Context<Self>) {
        self.actions.dialog = None;
        self.actions.pricing = None;
        cx.notify();
    }

    /// The amend form's values, checked against the venue's rules.
    fn amend_check(order: &OrderRow, price: &str, size: &str) -> Result<Checked, Rule> {
        let asset = order
            .asset
            .expect("Amend is offered only for a known market");
        rules::check(
            price,
            size,
            &asset,
            &Current {
                price: order.price,
                size: order.remaining,
            },
        )
    }

    /// The user confirmed: send what the dialog shows.
    pub(super) fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.can_act() {
            return;
        }
        let Some(dialog) = self.actions.dialog.take() else {
            return;
        };
        let target = |o: &OrderRow| Target {
            asset: o.asset.expect("only known markets reach a dialog").number,
            oid: o.oid,
        };
        match dialog {
            Dialog::Position(dialog) => self.confirm_position(dialog, cx),
            Dialog::Ticket(ticket) => self.confirm_ticket(ticket, cx),
            Dialog::Cancel(order) => {
                let reply = self.actions.trading.cancel(vec![target(&order)]);
                self.send(
                    vec![order.oid],
                    Doing::Cancelling,
                    reply,
                    cx,
                    move |answer| report_cancel(&order, answer),
                );
            }
            Dialog::CancelAll(orders) => {
                let reply = self
                    .actions
                    .trading
                    .cancel(orders.iter().map(target).collect());
                let oids = orders.iter().map(|o| o.oid).collect();
                self.send(oids, Doing::Cancelling, reply, cx, move |answer| {
                    report_cancel_all(&orders, answer)
                });
            }
            Dialog::Amend {
                order,
                price,
                size,
                _typing,
            } => {
                let checked =
                    Self::amend_check(&order, &price.read(cx).value(), &size.read(cx).value());
                let Ok(to) = checked else {
                    // The button is disabled while the form is not valid;
                    // should this be reached anyway, the form stays open.
                    self.actions.dialog = Some(Dialog::Amend {
                        order,
                        price,
                        size,
                        _typing,
                    });
                    return;
                };
                let kept = Kept {
                    side: order.side,
                    reduce_only: order.reduce_only,
                    tif: order.tif.clone().expect("an amendable order has one"),
                };
                let reply = self.actions.trading.amend(target(&order), to.clone(), kept);
                self.send(vec![order.oid], Doing::Amending, reply, cx, move |answer| {
                    report_amend(&order, &to, answer)
                });
            }
        }
    }

    fn send(
        &mut self,
        oids: Vec<u64>,
        doing: Doing,
        reply: Reply,
        cx: &mut Context<Self>,
        report: impl FnOnce(Result<Answer, Unavailable>) -> Report + 'static,
    ) {
        self.send_for(oids, Vec::new(), doing, reply, cx, report);
    }

    /// Mark the orders and positions an action is about as in progress,
    /// wait for the venue, report what it said, and reload both lists.
    pub(super) fn send_for<T: 'static>(
        &mut self,
        oids: Vec<u64>,
        assets: Vec<u32>,
        doing: Doing,
        reply: Pin<Box<dyn Future<Output = T>>>,
        cx: &mut Context<Self>,
        report: impl FnOnce(T) -> Report + 'static,
    ) {
        self.actions.pending = Some(Pending {
            oids,
            assets,
            doing,
        });
        self.actions.report = None;
        self.rebuild_orders(cx);
        cx.notify();
        self.actions.task = Some(cx.spawn(async move |this, cx| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.actions.pending = None;
                this.actions.report = Some(report(answer));
                // Whatever happened, the lists are asked for again now:
                // after a silence it is the only way to know.
                this.feed.refresh_after_action();
                this.actions.refreshes += 1;
                // A key found not to be this account's is known from here on.
                this.reread_availability(cx);
            });
        }));
    }

    /// Why orders or positions cannot be acted on, across the top of the tab.
    pub(super) fn trading_notice(
        &self,
        acting: Acting,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let unavailable = self.actions.availability.as_ref().err()?;
        let (title, text, settings) = explain(unavailable, acting);
        Some(
            h_flex()
                .id("orders-notice")
                .mx_3()
                .mt_2()
                .px_3()
                .py_2()
                .gap_3()
                .bg(Palette::amber_ground())
                .border_1()
                .border_color(Palette::amber_line())
                .rounded(px(8.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(Palette::amber_text())
                                .child(title),
                        )
                        .child(ui::label(text)),
                )
                .when(settings, |d| {
                    d.child(
                        Button::new("open-settings")
                            .small()
                            .label("Open Settings")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(PortfolioEvent::OpenSettings)),
                            ),
                    )
                })
                .test_support()
                .into_any_element(),
        )
    }

    /// What the last action came to.
    pub(super) fn orders_report(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let report = self.actions.report.as_ref()?;
        Some(
            h_flex()
                .id("orders-report")
                .mx_3()
                .mt_2()
                .px_3()
                .py_2()
                .gap_3()
                .border_1()
                .border_color(if report.problem {
                    Palette::amber_line()
                } else {
                    Palette::line()
                })
                .rounded(px(8.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .text_size(px(13.))
                        .child(
                            div()
                                .text_color(if report.problem {
                                    Palette::amber_text()
                                } else {
                                    Palette::text()
                                })
                                .child(report.text.clone()),
                        )
                        .children(report.more.clone().map(|more| {
                            div()
                                .text_size(px(12.5))
                                .text_color(Palette::amber_text())
                                .child(more)
                        })),
                )
                .child(
                    Button::new("orders-report-dismiss")
                        .small()
                        .ghost()
                        .label("Dismiss")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.actions.report = None;
                            cx.notify();
                        })),
                )
                .test_support()
                .into_any_element(),
        )
    }

    /// The Cancel all button, at the end of the tab bar.
    pub(super) fn cancel_all_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self
            .actions
            .pending
            .as_ref()
            .is_some_and(|p| p.doing == Doing::Cancelling && p.oids.len() > 1);
        Button::new("cancel-all")
            .ghost()
            .small()
            .icon(ui::list_cross_icon())
            .tooltip("Cancel all open orders")
            .accessibility_label("Cancel all open orders")
            .disabled(!self.can_act() || self.cancellable().is_empty())
            .loading(busy && !cx.reduce_motion())
            .on_click(cx.listener(|this, _, window, cx| this.ask_cancel_all(window, cx)))
            .into_any_element()
    }

    /// The confirmation or the amend form, over the screen.
    pub(super) fn order_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.actions.dialog.as_ref()?;
        let network = Core::get(cx).network;
        let signer = || signer(self.actions.wallet.as_deref());
        let order_line = |o: &OrderRow| {
            let mut words = format!(
                "{} {} at {}",
                f::amount(o.remaining),
                market_label(o),
                f::price(o.price)
            );
            words.push_str(&format!(" \u{b7} {}", o.kind));
            if let Some(trigger) = &o.trigger {
                words.push_str(&format!(" \u{b7} {trigger}"));
            }
            h_flex()
                .flex_wrap()
                .gap_2()
                .child(
                    div()
                        .text_color(ui::tone_of(if o.side == Side::Buy { 1 } else { -1 }))
                        .child(side_word(o.side)),
                )
                .child(words)
        };

        let (id, title, body, buttons): (&'static str, String, AnyElement, AnyElement) = match dialog {
            Dialog::Position(dialog) => return self.position_dialog(dialog, cx),
            Dialog::Ticket(ticket) => return self.ticket_dialog(ticket, cx),
            Dialog::Cancel(order) => (
                "cancel-dialog",
                "Cancel this order?".into(),
                v_flex()
                    .gap_3()
                    .child(facts(vec![
                        ("Order", order_line(order).into_any_element()),
                        ("Network", network_badge(network)),
                        ("Signed by", signer()),
                    ]))
                    .child(note(
                        "Nothing is sent until you confirm. If the order fills first, Hyperliquid will refuse the cancel and the app will say so.",
                    ))
                    .into_any_element(),
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(keep_button("Keep order", cx))
                    .child(go_button("Cancel order".into(), true, cx))
                    .into_any_element(),
            ),
            Dialog::CancelAll(orders) => (
                "cancel-all-dialog",
                if orders.len() == 1 {
                    "Cancel the 1 open order?".into()
                } else {
                    format!("Cancel all {} open orders?", orders.len())
                },
                v_flex()
                    .gap_3()
                    .child(listing(
                        "cancel-all-list",
                        orders.iter().map(|o| order_line(o).into_any_element()).collect(),
                    ))
                    .child(facts(vec![
                        ("Network", network_badge(network)),
                        ("Signed by", signer()),
                    ]))
                    .child(note(
                        "These are the orders listed now, in every market. Any that fill first are refused one by one; the rest are still cancelled.",
                    ))
                    .into_any_element(),
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(keep_button("Keep orders", cx))
                    .child(go_button(
                        if orders.len() == 1 {
                            "Cancel 1 order".into()
                        } else {
                            format!("Cancel {} orders", orders.len())
                        },
                        true,
                        cx,
                    ))
                    .into_any_element(),
            ),
            Dialog::Amend {
                order, price, size, ..
            } => {
                let checked =
                    Self::amend_check(order, &price.read(cx).value(), &size.read(cx).value());
                let (message, problem) = match &checked {
                    Ok(to) => (
                        format!(
                            "Sends: {} {} {} at {} on {}.",
                            side_word(order.side),
                            to.size_text,
                            market_label(order),
                            to.price_text,
                            network.label()
                        ),
                        false,
                    ),
                    Err(Rule::Unchanged) => (Rule::Unchanged.to_string(), false),
                    Err(rule) => (rule.to_string(), true),
                };
                let value = match &checked {
                    Ok(to) => f::usd(to.value()),
                    Err(_) => "\u{2014}".into(),
                };
                (
                    "amend-dialog",
                    "Amend order".into(),
                    v_flex()
                        .gap_3()
                        .child(facts(vec![("Now", order_line(order).into_any_element())]))
                        .child(
                            h_flex()
                                .gap_3()
                                .child(field("Price".into(), price))
                                .child(field(format!("Size ({})", order.market), size)),
                        )
                        .child(facts(vec![
                            ("New value", mono(value)),
                            ("Network", network_badge(network)),
                            ("Signed by", signer()),
                        ]))
                        .child(message_line("amend-message", message, problem))
                        .child(note(
                            "Side and order type stay the same. Hyperliquid replaces the order, so it joins the back of the queue at its price.",
                        ))
                        .into_any_element(),
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(keep_button("Keep as it is", cx))
                        .child(go_button("Amend order".into(), checked.is_ok(), cx))
                        .into_any_element(),
                )
            }
        };
        Some(self.dialog_frame(id, title, body, buttons, cx))
    }

    /// A dialog's panel over a dimmed screen. Escape backs out.
    pub(super) fn dialog_frame(
        &self,
        id: &'static str,
        title: String,
        body: AnyElement,
        buttons: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("order-dialog-scrim")
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_kit::hsla(0., 0., 0.03, 0.62))
            .child(
                ui::panel()
                    .id(id)
                    .w(px(470.))
                    .max_w_full()
                    .px_5()
                    .py_4()
                    .gap_3()
                    .shadow_lg()
                    .track_focus(&self.actions.focus)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" {
                            this.close_dialog(cx);
                        }
                    }))
                    .child(
                        div()
                            .text_size(px(17.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(body)
                    .child(buttons)
                    .test_support(),
            )
            .into_any_element()
    }

    /// `orders=confirm|amend|amend-bad|all|pending|done|refused|partial|unknown|nokey|wrongkey|locked`
    /// in `GALATA_DESK_SHOT_STATE`: show that state of the tab. A screenshot
    /// run's trading is a stand-in, so nothing can be sent.
    #[cfg(feature = "shot")]
    pub(super) fn stage_for_shot(&mut self, state: &str) {
        let availability = match state {
            "nokey" => Err(Unavailable::NoKey(
                galata_desk_core::network::Network::Testnet,
            )),
            "wrongkey" => Err(Unavailable::KeyNotApproved),
            "locked" => Err(Unavailable::MainnetOff),
            "override" => Err(Unavailable::Overridden),
            _ => Ok(()),
        };
        self.use_trading(Rc::new(Staged::new(availability)));
        self.actions.shot = Some(state.to_string());
    }

    /// Once the orders are in, put the staged state on screen.
    #[cfg(feature = "shot")]
    pub(super) fn show_shot_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(orders) = self.snapshot.orders.data.clone() else {
            return;
        };
        let Some(state) = self.actions.shot.take() else {
            return;
        };
        let Some(first) = orders.iter().find(|o| o.amendable && o.asset.is_some()) else {
            return;
        };
        match state.as_str() {
            "confirm" => self.press("cancel-order", first.oid, window, cx),
            "amend" => self.press("amend-order", first.oid, window, cx),
            "amend-bad" => {
                self.press("amend-order", first.oid, window, cx);
                if let Some(Dialog::Amend { size, .. }) = &self.actions.dialog {
                    size.update(cx, |input, cx| {
                        input.set_value("0.000000001".to_string(), window, cx)
                    });
                }
            }
            "all" => self.ask_cancel_all(window, cx),
            "pending" => {
                self.actions.pending = Some(Pending {
                    oids: vec![first.oid],
                    assets: Vec::new(),
                    doing: Doing::Cancelling,
                });
            }
            "done" => {
                self.actions.report =
                    Some(report_cancel(first, Ok(Answer::Each(vec![Outcome::DONE]))));
            }
            "refused" => {
                self.actions.report = Some(report_cancel(
                    first,
                    Ok(Answer::Each(vec![Outcome::Refused(
                        "Order was never placed, already canceled, or filled.".into(),
                    )])),
                ));
            }
            "partial" => {
                let some: Vec<OrderRow> = orders.iter().take(3).cloned().collect();
                let mut outcomes = vec![Outcome::DONE; some.len()];
                if let Some(last) = outcomes.last_mut() {
                    *last = Outcome::Refused(
                        "Order was never placed, already canceled, or filled.".into(),
                    );
                }
                self.actions.report = Some(report_cancel_all(&some, Ok(Answer::Each(outcomes))));
            }
            "unknown" => {
                self.actions.report = Some(report_cancel(
                    first,
                    Ok(Answer::Unknown("the request timed out".into())),
                ));
            }
            _ => {}
        }
        self.rebuild_orders(cx);
        cx.notify();
    }

    /// Put a stand-in in place of the gate.
    #[cfg(any(test, feature = "shot"))]
    pub fn use_trading(&mut self, trading: Rc<dyn Trading>) {
        self.actions.trading = trading;
        self.actions.staged = true;
        self.actions.availability = self.actions.trading.availability();
        self.actions.wallet = self.actions.trading.wallet();
    }
}

/// Rows of a name and its value, in a dialog.
pub(super) fn facts(rows: Vec<(&'static str, AnyElement)>) -> AnyElement {
    v_flex()
        .gap(px(7.))
        .children(rows.into_iter().map(|(name, value)| {
            h_flex()
                .gap_3()
                .items_start()
                .child(
                    div()
                        .w(px(92.))
                        .flex_shrink_0()
                        .text_color(Palette::muted())
                        .child(name),
                )
                .child(div().flex_1().min_w_0().child(value))
        }))
        .into_any_element()
}

/// The network, as a badge as wide as its word.
pub(super) fn network_badge(network: galata_desk_core::network::Network) -> AnyElement {
    h_flex()
        .child(
            div()
                .px(px(8.))
                .rounded(px(5.))
                .text_size(px(11.5))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .when(network.is_testnet(), |d| {
                    d.bg(Palette::amber_ground()).text_color(Palette::amber())
                })
                .when(!network.is_testnet(), |d| {
                    d.bg(Palette::raise()).text_color(Palette::text())
                })
                .child(network.label().to_uppercase()),
        )
        .into_any_element()
}

/// Who signs: the API wallet, by its short address.
pub(super) fn signer(wallet: Option<&str>) -> AnyElement {
    h_flex()
        .gap_2()
        .child("API wallet")
        .children(wallet.map(|w| div().font_family(MONO).text_size(px(12.5)).child(short(w))))
        .into_any_element()
}

/// A muted sentence under a dialog's facts.
pub(super) fn note(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_size(px(12.5))
        .text_color(Palette::muted())
        .child(text.into())
        .into_any_element()
}

/// A figure in the monospaced face.
pub(super) fn mono(text: impl Into<SharedString>) -> AnyElement {
    div()
        .font_family(MONO)
        .text_size(px(12.5))
        .child(text.into())
        .into_any_element()
}

/// What a form says about its values: what will be sent, or what to fix.
pub(super) fn message_line(id: &'static str, text: String, problem: bool) -> AnyElement {
    div()
        .id(id)
        .text_size(px(12.5))
        .text_color(if problem {
            Palette::amber_text()
        } else {
            Palette::text()
        })
        .child(text)
        .test_support()
        .into_any_element()
}

/// A labelled input.
pub(super) fn field(label: String, input: &Entity<InputState>) -> AnyElement {
    v_flex()
        .flex_1()
        .gap(px(4.))
        .child(ui::label(label))
        .child(Input::new(input))
        .into_any_element()
}

/// A bordered, scrolling list of lines.
pub(super) fn listing(id: &'static str, lines: Vec<AnyElement>) -> AnyElement {
    v_flex()
        .id(id)
        .max_h(px(220.))
        .overflow_y_scroll()
        .border_1()
        .border_color(Palette::rule())
        .rounded(px(8.))
        .children(lines.into_iter().enumerate().map(|(i, line)| {
            div()
                .px_3()
                .py(px(7.))
                .when(i > 0, |d| d.border_t_1().border_color(Palette::rule()))
                .child(line)
        }))
        .into_any_element()
}

/// The safe choice: close the dialog, send nothing.
pub(super) fn keep_button(label: &'static str, cx: &mut Context<Portfolio>) -> Button {
    Button::new("dialog-keep")
        .outline()
        .label(label)
        .on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx)))
}

/// Send what the dialog shows.
pub(super) fn go_button(label: String, enabled: bool, cx: &mut Context<Portfolio>) -> Button {
    Button::new("dialog-confirm")
        .primary()
        .label(label)
        .disabled(!enabled)
        .on_click(cx.listener(|this, _, _, cx| this.confirm(cx)))
}

pub(super) fn unsent(unavailable: &Unavailable) -> Report {
    Report {
        text: format!("Nothing was sent: {unavailable}.").into(),
        more: None,
        problem: true,
    }
}

pub(super) fn silence(what: &str, why: &str) -> Report {
    Report {
        text: format!(
            "No answer from Hyperliquid ({why}). The {what} may or may not have gone through; the orders below are being reloaded."
        )
        .into(),
        more: None,
        problem: true,
    }
}

pub(super) fn refused(what: &str, why: &str) -> Report {
    Report {
        text: format!("Hyperliquid refused the {what}: {why}").into(),
        more: None,
        problem: true,
    }
}

fn report_cancel(order: &OrderRow, answer: Result<Answer, Unavailable>) -> Report {
    match answer {
        Err(e) => unsent(&e),
        Ok(Answer::Unknown(why)) => silence("cancel", &why),
        Ok(Answer::Refused(why)) => refused("cancel", &why),
        Ok(Answer::Each(outcomes)) => match outcomes.first() {
            Some(Outcome::Done { .. }) => Report {
                text: format!("Cancelled: {}.", describe(order)).into(),
                more: None,
                problem: false,
            },
            Some(Outcome::Refused(why)) => refused("cancel", why),
            None => silence("cancel", "its answer named no order"),
        },
    }
}

fn report_cancel_all(orders: &[OrderRow], answer: Result<Answer, Unavailable>) -> Report {
    match answer {
        Err(e) => unsent(&e),
        Ok(Answer::Unknown(why)) => silence("cancels", &why),
        Ok(Answer::Refused(why)) => refused("cancels", &why),
        Ok(Answer::Each(outcomes)) => {
            let done = outcomes
                .iter()
                .filter(|o| matches!(o, Outcome::Done { .. }))
                .count();
            // An order the answer says nothing about is not known cancelled.
            let not_done: Vec<String> = orders
                .iter()
                .enumerate()
                .filter_map(|(i, order)| match outcomes.get(i) {
                    Some(Outcome::Done { .. }) => None,
                    Some(Outcome::Refused(why)) => Some(format!("{} ({why})", describe(order))),
                    None => Some(format!("{} (no answer for it)", describe(order))),
                })
                .collect();
            Report {
                text: format!(
                    "Cancelled {done} of {} order{}.",
                    orders.len(),
                    if orders.len() == 1 { "" } else { "s" }
                )
                .into(),
                more: (!not_done.is_empty())
                    .then(|| format!("Not cancelled: {}", not_done.join("; ")).into()),
                problem: !not_done.is_empty(),
            }
        }
    }
}

fn report_amend(order: &OrderRow, to: &Checked, answer: Result<Answer, Unavailable>) -> Report {
    let now = format!(
        "{} {} {} at {}",
        side_word(order.side),
        to.size_text,
        market_label(order),
        to.price_text
    );
    match answer {
        Err(e) => unsent(&e),
        Ok(Answer::Unknown(why)) => silence("amend", &why),
        Ok(Answer::Refused(why)) => refused("amend", &why),
        Ok(Answer::Each(outcomes)) => match outcomes.first() {
            Some(Outcome::Done { filled: None, .. }) => Report {
                text: format!("Amended: now {now}.").into(),
                more: None,
                problem: false,
            },
            Some(Outcome::Done {
                filled: Some(_), ..
            }) => Report {
                text: format!("Amended, and it filled at once: {now}.").into(),
                more: None,
                problem: false,
            },
            Some(Outcome::Refused(why)) => refused("amend", why),
            None => silence("amend", "its answer named no order"),
        },
    }
}

#[cfg(test)]
impl Portfolio {
    /// What the tab says about acting on orders, for tests: the notice's
    /// heading, if there is one.
    pub fn orders_unavailable(&self) -> Option<String> {
        self.actions
            .availability
            .as_ref()
            .err()
            .map(|e| explain(e, Acting::Orders).0)
    }

    /// The last action's report: its text, what was not done, and whether it
    /// is a problem.
    pub fn orders_reported(&self) -> Option<(String, Option<String>, bool)> {
        self.actions.report.as_ref().map(|r| {
            (
                r.text.to_string(),
                r.more.as_ref().map(|m| m.to_string()),
                r.problem,
            )
        })
    }

    /// Which dialog is open: `cancel`, `cancel-all` or `amend`.
    pub fn open_dialog(&self) -> Option<&'static str> {
        self.actions.dialog.as_ref().map(|d| match d {
            Dialog::Cancel(_) => "cancel",
            Dialog::CancelAll(_) => "cancel-all",
            Dialog::Amend { .. } => "amend",
            Dialog::Position(d) => d.name(),
            Dialog::Ticket(_) => "ticket",
        })
    }

    /// The order ids an action is in progress for.
    pub fn orders_pending(&self) -> Vec<u64> {
        self.actions
            .pending
            .as_ref()
            .map(|p| p.oids.clone())
            .unwrap_or_default()
    }

    pub fn orders_refreshes(&self) -> usize {
        self.actions.refreshes
    }

    /// Each listed order's buttons, as `(oid, name, disabled, busy)`.
    pub fn order_controls(&self) -> Vec<(u64, String, bool, bool)> {
        self.snapshot
            .orders
            .data
            .iter()
            .flatten()
            .flat_map(|o| {
                self.order_buttons(o)
                    .row_buttons()
                    .iter()
                    .map(|b| (b.key, b.name.to_string(), b.disabled, b.busy))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Whether Cancel all can be pressed.
    pub fn can_cancel_all(&self) -> bool {
        self.can_act() && !self.cancellable().is_empty()
    }

    /// Press a row's button, as a click on it does.
    pub fn press_order(&mut self, id: &str, oid: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.press(id, oid, window, cx);
    }

    pub fn press_cancel_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_cancel_all(window, cx);
    }

    /// Type into the amend form.
    pub fn type_amend(
        &mut self,
        price: &str,
        size: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Dialog::Amend {
            price: p, size: s, ..
        }) = &self.actions.dialog
        {
            p.update(cx, |input, cx| {
                input.set_value(price.to_string(), window, cx)
            });
            s.update(cx, |input, cx| {
                input.set_value(size.to_string(), window, cx)
            });
        }
        cx.notify();
    }

    /// What the amend form says about its values, and whether it can be sent.
    pub fn amend_state(&self, cx: &App) -> Option<(String, bool)> {
        match &self.actions.dialog {
            Some(Dialog::Amend {
                order, price, size, ..
            }) => Some(
                match Self::amend_check(order, &price.read(cx).value(), &size.read(cx).value()) {
                    Ok(to) => (format!("{} at {}", to.size_text, to.price_text), true),
                    Err(rule) => (rule.to_string(), false),
                },
            ),
            _ => None,
        }
    }
}
