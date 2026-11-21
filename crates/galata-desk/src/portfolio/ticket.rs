//! The order ticket: the one place the app opens or adds to a position.
//!
//! Opened from the Markets panel's Buy and Sell buttons, drawn by the
//! Portfolio's dialog layer over whichever section is showing, and sent
//! through the same `Trading` and shared action state as every other action.
//! Its checks are `rules::check_order`; the figures it shows come from the
//! venue (`activeAssetData`, the mid) wherever the venue has them.

use galata_desk_core::market::MarketId;
use galata_desk_core::store::QuickSizes;
use galata_desk_core::trade::assets::Asset;
use galata_desk_core::trade::rules::{
    self, CheckedOrder, MARKET_SLIPPAGE_PCT, OrderContext, OrderInput, OrderPrice, Rule, SizeUnit,
};
use galata_desk_core::trade::{Answer, NewOrder, Outcome, Pricing, Unavailable};
use galata_desk_core::venue::{ActiveAsset, MarginMode};
use gpui_kit::component::Disableable;
use gpui_kit::component::button::ButtonCustomVariant;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{App, ElementId, Subscription};

use super::orders::{
    Acting, Doing, Report, explain, facts, field, message_line, mono, network_badge, note, refused,
    signer, unsent,
};
use super::*;

/// Something the ticket asks the venue for when it opens.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Fetch<T> {
    Loading,
    Got(T),
    Failed(String),
}

impl<T> Fetch<T> {
    fn got(&self) -> Option<&T> {
        match self {
            Fetch::Got(value) => Some(value),
            _ => None,
        }
    }
}

pub(super) struct Ticket {
    market: MarketId,
    buy: bool,
    /// At market; otherwise a resting limit order.
    at_market: bool,
    unit: SizeUnit,
    reduce_only: bool,
    with_tpsl: bool,
    size: Entity<InputState>,
    price: Entity<InputState>,
    take_profit: Entity<InputState>,
    stop_loss: Entity<InputState>,
    asset: Fetch<Asset>,
    active: Fetch<ActiveAsset>,
    mid: Fetch<Decimal>,
    /// Confirmed: the ticket now shows the venue's answer.
    sent: bool,
    _typing: Vec<Subscription>,
}

impl Ticket {
    fn unit_word(&self) -> String {
        match self.unit {
            SizeUnit::Usd => "USD".into(),
            SizeUnit::Coin => self.market.ticker().to_string(),
        }
    }

    fn label(&self) -> String {
        if self.market.dex().is_empty() {
            self.market.ticker().to_string()
        } else {
            format!("{} ({})", self.market.ticker(), self.market.dex())
        }
    }
}

/// One Quick button as the ticket draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickButton {
    /// Whole US dollars.
    pub amount: u64,
    /// The size in USD is this amount.
    pub marked: bool,
    /// Within what the venue says can be traded on this side, or not known
    /// yet.
    pub usable: bool,
}

impl QuickButton {
    /// Its name for assistive technology.
    pub fn name(&self) -> String {
        let label = QuickSizes::label(self.amount);
        if self.usable {
            format!("Size {label}")
        } else {
            format!("Size {label}, more than this account can trade")
        }
    }
}

/// A small muted word that names a row of buttons.
fn small_caption(text: &'static str) -> gpui_kit::Div {
    div()
        .mr(px(2.))
        .text_size(px(11.5))
        .text_color(Palette::muted())
        .child(text)
}

/// What the ticket's form comes to: a sendable order, or what to fix (and
/// whether that is a problem rather than something still loading).
pub(super) type TicketCheck = Result<(CheckedOrder, Pricing), (String, bool)>;

impl Portfolio {
    /// Open the order ticket for `market`, set to buy or sell.
    pub fn open_ticket(
        &mut self,
        market: MarketId,
        buy: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.actions.pending.is_some() {
            return;
        }
        let mut typing = Vec::new();
        let mut input = |value: &str, window: &mut Window, cx: &mut Context<Self>| {
            let input = cx.new(|cx| InputState::new(window, cx));
            input.update(cx, |input, cx| {
                input.set_value(value.to_string(), window, cx)
            });
            // Typing redraws the form: its checks are made as it is drawn.
            typing.push(cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }));
            input
        };
        let size = input("", window, cx);
        let price = input("", window, cx);
        let take_profit = input("", window, cx);
        let stop_loss = input("", window, cx);
        let ticket = Ticket {
            market: market.clone(),
            buy,
            at_market: true,
            unit: SizeUnit::Usd,
            reduce_only: false,
            with_tpsl: false,
            size,
            price,
            take_profit,
            stop_loss,
            asset: Fetch::Loading,
            active: Fetch::Loading,
            mid: Fetch::Loading,
            sent: false,
            _typing: typing,
        };
        // A result from an earlier action is not this ticket's.
        self.actions.report = None;
        self.actions.ticket = Some(Box::new(ticket));
        if self.actions.ticket_docked {
            // The panel holds it: keys go to its size field.
            self.focus_ticket(window, cx);
        } else {
            // Over the screen, keys go to the dialog: Escape backs out.
            window.focus(&self.actions.focus, cx);
        }
        self.fetch_ticket_facts(market, cx);
        cx.notify();
    }

    /// Buy or Sell pressed on `market`: a docked ticket is filled and
    /// focused; without one, the ticket opens over the screen.
    pub fn trade(
        &mut self,
        market: MarketId,
        buy: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.actions.ticket_docked {
            self.fill_ticket(market, buy, window, cx);
        } else {
            self.open_ticket(market, buy, window, cx);
        }
    }

    /// Whether a ticket panel is docked.
    pub fn ticket_is_docked(&self) -> bool {
        self.actions.ticket_docked
    }

    /// Put `market` and the side in the ticket. A docked ticket that is
    /// still being filled in for this market keeps what was typed and only
    /// changes side; anything else starts a new ticket.
    fn fill_ticket(
        &mut self,
        market: MarketId,
        buy: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.ticket_mut() {
            Some(t) if t.market == market => {
                t.buy = buy;
                self.focus_ticket(window, cx);
                cx.notify();
            }
            _ => self.open_ticket(market, buy, window, cx),
        }
    }

    /// Move the keyboard to the ticket's size field.
    pub fn focus_ticket(&self, window: &mut Window, cx: &mut App) {
        if let Some(t) = &self.actions.ticket {
            t.size.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    /// Whether the keyboard is in the ticket's size field.
    #[cfg(test)]
    pub fn ticket_focused(&self, window: &Window, cx: &App) -> bool {
        self.actions.ticket.as_ref().is_some_and(|t| {
            use gpui_kit::Focusable as _;
            t.size.read(cx).focus_handle(cx).is_focused(window)
        })
    }

    /// Whether a ticket panel is docked, so the ticket is drawn there.
    pub fn set_ticket_docked(&mut self, docked: bool, cx: &mut Context<Self>) {
        if self.actions.ticket_docked != docked {
            self.actions.ticket_docked = docked;
            // A ticket left over the screen goes with its panel, and the
            // panel does not take over one that was open over the screen.
            self.close_ticket(cx);
        }
    }

    /// Close the ticket. Its form, and anything it was fetching, go with it.
    pub(super) fn close_ticket(&mut self, cx: &mut Context<Self>) {
        if self.actions.ticket.take().is_some() {
            self.actions.ticket_pricing = None;
            cx.notify();
        }
    }

    /// Cancel or Done in the ticket. Over the screen the ticket closes; in a
    /// docked panel it starts again, empty, on the same market and side.
    fn finish_ticket(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let again = self
            .actions
            .ticket
            .as_ref()
            .filter(|_| self.actions.ticket_docked)
            .map(|t| (t.market.clone(), t.buy));
        self.close_ticket(cx);
        if let Some((market, buy)) = again {
            self.open_ticket(market, buy, window, cx);
        }
    }

    /// Place the order the ticket shows. The ticket's button is the
    /// confirmation: it is enabled only while the form checks out.
    fn place_ticket(&mut self, cx: &mut Context<Self>) {
        if !self.can_act() {
            return;
        }
        if let Some(t) = self.actions.ticket.take() {
            self.confirm_ticket(t, cx);
        }
    }

    /// Ask the venue what the ticket needs: the market's number and size
    /// step, what the account can trade in it, and its mid.
    fn fetch_ticket_facts(&mut self, market: MarketId, cx: &mut Context<Self>) {
        let trading = self.actions.trading.clone();
        self.actions.ticket_pricing = Some(cx.spawn(async move |this, cx| {
            fn still<'a>(this: &'a mut Portfolio, market: &MarketId) -> Option<&'a mut Ticket> {
                match &mut this.actions.ticket {
                    Some(t) if &t.market == market => Some(t),
                    _ => None,
                }
            }
            let asset = trading
                .market(market.dex().to_string(), market.coin().to_string())
                .await;
            let asset = match asset {
                Ok(Some(asset)) => Fetch::Got(asset),
                Ok(None) => Fetch::Failed("Hyperliquid does not list this market".into()),
                Err(why) => Fetch::Failed(why),
            };
            if this
                .update(cx, |this, cx| {
                    if let Some(t) = still(this, &market) {
                        t.asset = asset;
                        cx.notify();
                    }
                })
                .is_err()
            {
                return;
            }
            let active = match trading.active_asset(market.coin().to_string()).await {
                Ok(active) => Fetch::Got(active),
                Err(why) => Fetch::Failed(why),
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(t) = still(this, &market) {
                    t.active = active;
                    cx.notify();
                }
            });
            let mid = match trading
                .mid(market.dex().to_string(), market.ticker().to_string())
                .await
            {
                Ok(Some(mid)) => Fetch::Got(mid),
                Ok(None) => Fetch::Failed("Hyperliquid lists no price for this market".into()),
                Err(why) => Fetch::Failed(why),
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(t) = still(this, &market) {
                    t.mid = mid;
                    cx.notify();
                }
            });
        }));
    }

    fn retry_ticket(&mut self, cx: &mut Context<Self>) {
        let Some(t) = &mut self.actions.ticket else {
            return;
        };
        t.asset = Fetch::Loading;
        t.active = Fetch::Loading;
        t.mid = Fetch::Loading;
        let market = t.market.clone();
        self.fetch_ticket_facts(market, cx);
        cx.notify();
    }

    fn ticket_mut(&mut self) -> Option<&mut Ticket> {
        match &mut self.actions.ticket {
            Some(t) if !t.sent => Some(t),
            _ => None,
        }
    }

    fn set_ticket(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Ticket)) {
        if let Some(t) = self.ticket_mut() {
            change(t);
            cx.notify();
        }
    }

    /// The account's position in the ticket's market, signed; zero for none.
    fn ticket_position(&self, market: &MarketId) -> Decimal {
        self.holdings()
            .and_then(|h| {
                h.positions
                    .iter()
                    .find(|p| p.dex == market.dex() && p.market == market.ticker())
            })
            .map(|p| p.size)
            .unwrap_or_default()
    }

    /// Check the ticket's form as it stands.
    pub(super) fn ticket_check(&self, t: &Ticket, cx: &App) -> TicketCheck {
        let Some(asset) = t.asset.got().copied() else {
            return Err(match &t.asset {
                Fetch::Failed(why) => (format!("The market could not be read: {why}."), true),
                _ => ("Reading the market\u{2026}".into(), false),
            });
        };
        let worst = match (&t.mid, t.at_market) {
            (Fetch::Got(mid), true) => {
                Some(rules::slippage(*mid, t.buy, &asset, MARKET_SLIPPAGE_PCT))
            }
            (Fetch::Failed(why), true) => {
                return Err((format!("The market price could not be read: {why}."), true));
            }
            (Fetch::Loading, true) => {
                return Err(("Reading the market price\u{2026}".into(), false));
            }
            (_, false) => None,
        };
        if let Fetch::Failed(why) = &t.active {
            return Err((
                format!("What this account can trade could not be read: {why}."),
                true,
            ));
        }
        let size = t.size.read(cx).value().to_string();
        let price = t.price.read(cx).value().to_string();
        let tp = t.take_profit.read(cx).value().to_string();
        let sl = t.stop_loss.read(cx).value().to_string();
        let input = OrderInput {
            buy: t.buy,
            size: &size,
            unit: t.unit,
            price: match worst {
                Some(worst) => OrderPrice::Market { worst },
                None => OrderPrice::Limit { text: &price },
            },
            reduce_only: t.reduce_only,
            take_profit: t.with_tpsl.then_some(tp.as_str()),
            stop_loss: t.with_tpsl.then_some(sl.as_str()),
        };
        let ctx = OrderContext {
            asset,
            position: self.ticket_position(&t.market),
            max_size: t.active.got().map(|a| a.max_size(t.buy)),
        };
        let checked = rules::check_order(&input, &ctx).map_err(|rule| {
            let unchanged = matches!(rule, Rule::SizeNotNumber) && size.trim().is_empty();
            (rule.to_string(), !unchanged)
        })?;
        if t.active == Fetch::Loading {
            return Err(("Reading what this account can trade\u{2026}".into(), false));
        }
        let pricing = match worst {
            Some(worst) => Pricing::Market { worst },
            None => Pricing::Limit {
                price: checked.price,
            },
        };
        Ok((checked, pricing))
    }

    /// The Quick buttons for the saved quick sizes, and, while any of them is
    /// more than can be traded on this side, the most that can (in whole
    /// dollars). Until the venue says what the account can trade, every
    /// button is usable and the order's own checks are the guard.
    pub(super) fn ticket_quick(&self, t: &Ticket, cx: &App) -> (Vec<QuickButton>, Option<u64>) {
        let size = t.size.read(cx).value().trim().to_string();
        let typed = (t.unit == SizeUnit::Usd)
            .then(|| size.parse::<Decimal>().ok())
            .flatten();
        let most = t.active.got().map(|active| {
            // The order's price: the worst price at market, the limit price
            // once it is a price, and the mark until then.
            let price = if t.at_market {
                match (&t.mid, t.asset.got()) {
                    (Fetch::Got(mid), Some(asset)) => {
                        Some(rules::slippage(*mid, t.buy, asset, MARKET_SLIPPAGE_PCT))
                    }
                    _ => None,
                }
            } else {
                t.price
                    .read(cx)
                    .value()
                    .trim()
                    .parse::<Decimal>()
                    .ok()
                    .filter(|p| p.is_sign_positive() && !p.is_zero())
            };
            active.max_size(t.buy) * price.unwrap_or(active.mark_px)
        });
        let buttons: Vec<QuickButton> = Core::get(cx)
            .quick_sizes
            .amounts()
            .iter()
            .map(|&amount| QuickButton {
                amount,
                marked: typed == Some(Decimal::from(amount)),
                usable: most.is_none_or(|most| Decimal::from(amount) <= most),
            })
            .collect();
        let most = most
            .filter(|_| buttons.iter().any(|b| !b.usable))
            .map(|most| u64::try_from(most.floor().max(Decimal::ZERO)).unwrap_or(0));
        (buttons, most)
    }

    /// Fill in a quick size: that many US dollars, in USD.
    fn pick_ticket_quick(&mut self, amount: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.ticket_mut() else {
            return;
        };
        t.unit = SizeUnit::Usd;
        let size = t.size.clone();
        size.update(cx, |input, cx| {
            input.set_value(amount.to_string(), window, cx)
        });
        cx.notify();
    }

    /// Fill in a share of the largest size the venue allows on this side.
    fn pick_ticket_share(&mut self, pct: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.ticket_mut() else {
            return;
        };
        let (Some(asset), Some(active)) = (t.asset.got().copied(), t.active.got()) else {
            return;
        };
        let coin = rules::share_of(active.max_size(t.buy), pct, &asset);
        let text = match t.unit {
            SizeUnit::Coin => rules::wire(coin),
            SizeUnit::Usd => {
                let price = match (&t.mid, t.at_market) {
                    (Fetch::Got(mid), _) => *mid,
                    _ => active.mark_px,
                };
                // Whole dollars, down, so the coin size it makes stays within
                // the share.
                rules::wire((coin * price).floor())
            }
        };
        let size = t.size.clone();
        size.update(cx, |input, cx| input.set_value(text, window, cx));
        cx.notify();
    }

    fn use_ticket_mid(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.ticket_mut() else {
            return;
        };
        if let Fetch::Got(mid) = t.mid {
            let price = t.price.clone();
            price.update(cx, |input, cx| {
                input.set_value(rules::wire(mid), window, cx)
            });
            cx.notify();
        }
    }

    /// The user confirmed the ticket: send what it shows.
    pub(super) fn confirm_ticket(&mut self, mut t: Box<Ticket>, cx: &mut Context<Self>) {
        let checked = self.ticket_check(&t, cx);
        let (Ok((order, pricing)), Some(asset)) = (checked, t.asset.got().copied()) else {
            // The button is disabled while the form is not valid.
            self.actions.ticket = Some(t);
            return;
        };
        let new_order = NewOrder {
            asset,
            buy: t.buy,
            size: order.size,
            pricing,
            reduce_only: t.reduce_only,
            take_profit: order.take_profit,
            stop_loss: order.stop_loss,
        };
        let reply = self.actions.trading.place(new_order);
        let what = OrderSummary {
            buy: t.buy,
            label: t.label(),
            ticker: t.market.ticker().to_string(),
            size: order.size,
            price: order.price,
            at_market: t.at_market,
        };
        t.sent = true;
        self.actions.ticket = Some(t);
        self.send_for(
            Vec::new(),
            vec![asset.number],
            Doing::Placing,
            reply,
            cx,
            move |answer| report_order(&what, answer),
        );
    }

    /// The ticket, over the screen.
    /// The ticket over the screen, unless a panel holds it or a
    /// confirmation is showing.
    pub(super) fn ticket_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.actions.ticket_docked || self.actions.dialog.is_some() {
            return None;
        }
        let t = self.actions.ticket.as_ref()?;
        let (title, body, buttons) = self.ticket_parts(t, cx);
        Some(self.dialog_frame("ticket-dialog", title, body, buttons, cx))
    }

    /// The ticket in its docked panel, if it is open and docked.
    pub fn ticket_inline(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.actions.ticket_docked {
            return None;
        }
        let t = self.actions.ticket.as_ref()?;
        let (title, body, buttons) = self.ticket_parts(t, cx);
        Some(
            v_flex()
                .id("ticket-inline")
                .size_full()
                .gap_3()
                .p_3()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(body)
                .child(buttons)
                .test_support()
                .into_any_element(),
        )
    }

    /// The ticket's title, form and buttons, wherever it is drawn.
    fn ticket_parts(&self, t: &Ticket, cx: &mut Context<Self>) -> (String, AnyElement, AnyElement) {
        let network = Core::get(cx).network;
        let ticker = t.market.ticker().to_string();
        let checked = self.ticket_check(t, cx);
        let unavailable = self.actions.availability.as_ref().err().cloned();
        let position = self.ticket_position(&t.market);
        let side_button =
            |id: &'static str, label: &'static str, on: bool, cx: &mut Context<Self>| {
                let buy = id == "ticket-buy";
                let button = Button::new(id).label(label).flex_1();
                // Buy in the gain colour and Sell in the loss colour, as on
                // the panel that opened the ticket.
                let button = if on {
                    let ground = if buy {
                        Palette::gain()
                    } else {
                        Palette::loss()
                    };
                    button.custom(
                        ButtonCustomVariant::new(cx)
                            .color(ground)
                            .foreground(ground)
                            .hover(ground.opacity(0.3))
                            .active(ground.opacity(0.4)),
                    )
                } else {
                    button.outline()
                };
                button.disabled(t.sent).on_click(
                    cx.listener(move |this, _, _, cx| this.set_ticket(cx, |t| t.buy = buy)),
                )
            };
        let mode_button = |id: &'static str,
                           label: &'static str,
                           on: bool,
                           cx: &mut Context<Self>| {
            let button = Button::new(id).small().label(label);
            let button = if on { button.primary() } else { button.ghost() };
            let market = id == "ticket-market";
            button.disabled(t.sent).on_click(
                cx.listener(move |this, _, _, cx| this.set_ticket(cx, |t| t.at_market = market)),
            )
        };
        let unit_button = |unit: SizeUnit, label: String, cx: &mut Context<Self>| {
            let id = match unit {
                SizeUnit::Usd => "ticket-unit-usd",
                SizeUnit::Coin => "ticket-unit-coin",
            };
            let button = Button::new(id).small().label(label);
            let button = if t.unit == unit {
                button.primary()
            } else {
                button.ghost()
            };
            button
                .on_click(cx.listener(move |this, _, _, cx| this.set_ticket(cx, |t| t.unit = unit)))
        };

        let leverage = match &t.active {
            Fetch::Got(a) => format!(
                "{}x {}",
                a.leverage.value,
                match a.leverage.mode {
                    MarginMode::Cross => "Cross",
                    MarginMode::Isolated => "Isolated",
                }
            ),
            Fetch::Loading => "\u{2026}".into(),
            Fetch::Failed(_) => "\u{2014}".into(),
        };
        let available = match &t.active {
            Fetch::Got(a) => format!("{} USDC", f::amount(a.available(t.buy).round_dp(2))),
            Fetch::Loading => "\u{2026}".into(),
            Fetch::Failed(_) => "\u{2014}".into(),
        };
        let position_text = if position.is_zero() {
            "None".to_string()
        } else {
            f::size(position, &ticker)
        };

        let (order_value, margin, fee, coin_size) = match &checked {
            Ok((order, _)) => {
                let value = order.value();
                let lev = t
                    .active
                    .got()
                    .map(|a| Decimal::from(a.leverage.value.max(1)))
                    .unwrap_or(Decimal::ONE);
                let rate = self.overview().map(|o| {
                    if t.at_market {
                        o.fees.user_cross_rate
                    } else {
                        o.fees.user_add_rate
                    }
                });
                (
                    f::usd(value),
                    if t.reduce_only {
                        "\u{2014}".to_string()
                    } else {
                        f::usd(value / lev)
                    },
                    rate.map(|r| f::usd(value * r))
                        .unwrap_or_else(|| "\u{2014}".into()),
                    Some(order.size),
                )
            }
            Err(_) => (
                "\u{2014}".into(),
                "\u{2014}".into(),
                "\u{2014}".into(),
                None,
            ),
        };

        let (quick, most) = self.ticket_quick(t, cx);
        let verb = if t.buy { "Buy" } else { "Sell" };
        let (message, problem) = match (&unavailable, &checked) {
            (Some(why), _) => (explain(why, Acting::Placing).0, true),
            (None, Ok((order, pricing))) => (
                format!(
                    "Sends: {verb} {} {ticker} {}{}{} on {}.",
                    rules::wire(order.size),
                    match pricing {
                        Pricing::Market { worst } =>
                            format!("at market, no worse than {}", rules::wire(*worst)),
                        Pricing::Limit { price } =>
                            format!("at {}, resting until it fills", rules::wire(*price)),
                    },
                    if t.reduce_only { ", reduce-only" } else { "" },
                    match (order.take_profit, order.stop_loss) {
                        (Some(tp), Some(sl)) => format!(
                            ", with a take-profit at {} and a stop-loss at {}",
                            rules::wire(tp),
                            rules::wire(sl)
                        ),
                        (Some(tp), None) => format!(", with a take-profit at {}", rules::wire(tp)),
                        (None, Some(sl)) => format!(", with a stop-loss at {}", rules::wire(sl)),
                        (None, None) => String::new(),
                    },
                    network.label()
                ),
                false,
            ),
            (None, Err((text, problem))) => (text.clone(), *problem),
        };

        // After confirming: the venue's answer, in place of the form's line.
        let pending = t.sent && self.actions.pending.is_some();
        let outcome = t.sent.then_some(self.actions.report.as_ref()).flatten();

        let estimate = |text: String| -> String {
            match (
                &checked,
                rules::limit_close_price(
                    &text,
                    &t.asset.got().copied().unwrap_or(Asset {
                        number: 0,
                        sz_decimals: 0,
                        spot: false,
                    }),
                ),
            ) {
                (Ok((order, _)), Ok(at)) => {
                    let signed = if t.buy { order.size } else { -order.size };
                    format!(
                        "about {}",
                        f::usd_signed(rules::result_at(signed, order.price, at).round_dp(2))
                    )
                }
                _ => "\u{2014}".into(),
            }
        };

        let mut body = v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(side_button("ticket-buy", "Buy / Long", t.buy, cx))
                    .child(side_button("ticket-sell", "Sell / Short", !t.buy, cx)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(mode_button("ticket-market", "Market", t.at_market, cx))
                    .child(mode_button("ticket-limit", "Limit", !t.at_market, cx)),
            )
            .child(facts(vec![
                ("Available", mono(available)),
                ("Position", mono(position_text)),
                ("Leverage", mono(leverage)),
            ]));
        if t.at_market {
            let worst = match (&t.mid, t.asset.got()) {
                (Fetch::Got(mid), Some(asset)) => format!(
                    "{} (5% from mid {})",
                    rules::wire(rules::slippage(*mid, t.buy, asset, MARKET_SLIPPAGE_PCT)),
                    rules::wire(*mid)
                ),
                (Fetch::Failed(_), _) => "\u{2014}".into(),
                _ => "\u{2026}".into(),
            };
            body = body.child(facts(vec![("Worst price", mono(worst))]));
        } else {
            body = body.child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(div().flex_1().child(field("Price (USDC)".into(), &t.price)))
                    .child(
                        Button::new("ticket-mid")
                            .small()
                            .outline()
                            .label("Mid")
                            .disabled(t.mid.got().is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.use_ticket_mid(window, cx)),
                            ),
                    ),
            );
        }
        body = body
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(
                        div()
                            .flex_1()
                            .child(field(format!("Size ({})", t.unit_word()), &t.size)),
                    )
                    .child(unit_button(SizeUnit::Usd, "USD".into(), cx))
                    .child(unit_button(SizeUnit::Coin, ticker.clone(), cx)),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(small_caption("Quick"))
                    .children(quick.into_iter().map(|q| {
                        let amount = q.amount;
                        let button =
                            Button::new(ElementId::NamedInteger("ticket-quick".into(), amount))
                                .small()
                                .font_family(MONO)
                                .label(QuickSizes::label(amount))
                                .accessibility_label(q.name());
                        let button = if q.marked {
                            button.primary()
                        } else {
                            button.outline()
                        };
                        button.disabled(!q.usable || t.sent).on_click(cx.listener(
                            move |this, _, window, cx| this.pick_ticket_quick(amount, window, cx),
                        ))
                    }))
                    .child(div().w(px(1.)).h(px(18.)).mx_1().bg(Palette::line()))
                    .child(small_caption("Of max"))
                    .children([25u32, 50, 75, 100].map(|pct| {
                        Button::new(ElementId::NamedInteger(
                            "ticket-share".into(),
                            u64::from(pct),
                        ))
                        .small()
                        .outline()
                        .label(format!("{pct}%"))
                        .disabled(t.active.got().is_none() || t.asset.got().is_none())
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.pick_ticket_share(pct, window, cx),
                        ))
                    })),
            )
            .when(
                most.is_some() || (coin_size.is_some() && t.unit == SizeUnit::Usd),
                |d| {
                    d.child(
                        h_flex()
                            .text_size(px(12.5))
                            .children(most.map(|most| {
                                let text = format!(
                                    "You can trade up to {} on this side.",
                                    QuickSizes::label(most)
                                );
                                div()
                                    .id("ticket-quick-most")
                                    .aria_label(text.clone())
                                    .text_color(Palette::amber_text())
                                    .child(text)
                            }))
                            .child(div().flex_1())
                            .children(coin_size.filter(|_| t.unit == SizeUnit::Usd).map(|size| {
                                div()
                                    .font_family(MONO)
                                    .text_color(Palette::muted())
                                    .child(format!("= {} {ticker}", rules::wire(size)))
                            })),
                    )
                },
            )
            .child(
                h_flex()
                    .gap_4()
                    .child(
                        Checkbox::new("ticket-reduce")
                            .label("Reduce only")
                            .checked(t.reduce_only)
                            .on_click(cx.listener(|this, on: &bool, _, cx| {
                                let on = *on;
                                this.set_ticket(cx, |t| t.reduce_only = on)
                            })),
                    )
                    .child(
                        Checkbox::new("ticket-tpsl")
                            .label("Take-profit / Stop-loss")
                            .checked(t.with_tpsl)
                            .on_click(cx.listener(|this, on: &bool, _, cx| {
                                let on = *on;
                                this.set_ticket(cx, |t| t.with_tpsl = on)
                            })),
                    ),
            );
        if t.with_tpsl {
            let tp_text = t.take_profit.read(cx).value().to_string();
            let sl_text = t.stop_loss.read(cx).value().to_string();
            body = body
                .child(
                    h_flex()
                        .gap_3()
                        .child(field("Take-profit price".into(), &t.take_profit))
                        .child(field("Stop-loss price".into(), &t.stop_loss)),
                )
                .child(facts(vec![
                    ("At TP", mono(estimate(tp_text))),
                    ("At SL", mono(estimate(sl_text))),
                ]));
        }
        body = body.child(facts(vec![
            ("Order value", mono(order_value)),
            ("Margin", mono(margin)),
            ("Fee", mono(fee)),
            ("Network", network_badge(network)),
            ("Signed by", signer(self.actions.wallet.as_deref())),
        ]));

        let failed = matches!(t.asset, Fetch::Failed(_))
            || matches!(t.active, Fetch::Failed(_))
            || (t.at_market && matches!(t.mid, Fetch::Failed(_)));
        body = match (pending, outcome) {
            (true, _) => body.child(message_line("ticket-message", "Sending\u{2026}".into(), false)),
            (false, Some(report)) => body
                .child(message_line("ticket-message", report.text.to_string(), report.problem))
                .children(report.more.clone().map(note)),
            (false, None) => body
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(message_line("ticket-message", message, problem)),
                        )
                        .when(unavailable.as_ref().is_some_and(|u| explain(u, Acting::Placing).2), |d| {
                            d.child(
                                Button::new("ticket-settings")
                                    .small()
                                    .label("Open Settings")
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PortfolioEvent::OpenSettings))),
                            )
                        })
                        .when(failed && unavailable.is_none(), |d| {
                            d.child(
                                Button::new("ticket-retry")
                                    .small()
                                    .label("Try again")
                                    .on_click(cx.listener(|this, _, _, cx| this.retry_ticket(cx))),
                            )
                        }),
                )
                .child(note(
                    "Nothing is sent until you confirm. This order can open or add to a position. Leverage is set on Hyperliquid.",
                )),
        };

        let buttons = if t.sent {
            h_flex()
                .gap_2()
                .justify_end()
                .child(
                    Button::new("ticket-done")
                        .primary()
                        .label("Done")
                        .disabled(pending)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.finish_ticket(window, cx)),
                        ),
                )
                .into_any_element()
        } else {
            let label = match &checked {
                Ok((order, _)) => format!("{verb} {} {ticker}", rules::wire(order.size)),
                Err(_) => format!("{verb} {ticker}"),
            };
            h_flex()
                .gap_2()
                .justify_end()
                // The ticket's own buttons: in a docked panel a confirmation
                // may be open over the screen at the same time, and these
                // must not act on it.
                .child(
                    Button::new("dialog-keep")
                        .outline()
                        .label("Cancel")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.finish_ticket(window, cx)),
                        ),
                )
                .child(
                    Button::new("dialog-confirm")
                        .primary()
                        .label(label)
                        .disabled(!(checked.is_ok() && unavailable.is_none()))
                        .on_click(cx.listener(|this, _, _, cx| this.place_ticket(cx))),
                )
                .into_any_element()
        };
        (
            format!("Place an order \u{b7} {}", t.label()),
            body.into_any_element(),
            buttons,
        )
    }
}

/// What a sent order was, to say what came of it.
pub(super) struct OrderSummary {
    buy: bool,
    label: String,
    ticker: String,
    size: Decimal,
    price: Decimal,
    at_market: bool,
}

/// An IOC that met no order within its price is refused in these words.
fn unmatched(why: &str) -> bool {
    why.contains("could not immediately match")
}

pub(super) fn report_order(o: &OrderSummary, answer: Result<Answer, Unavailable>) -> Report {
    let (did, side) = if o.buy {
        ("Bought", "Buy")
    } else {
        ("Sold", "Sell")
    };
    match answer {
        Err(e) => unsent(&e),
        Ok(Answer::Unknown(why)) => Report {
            text: format!(
                "No answer from Hyperliquid ({why}). The order may or may not have gone through; positions and orders are being reloaded."
            )
            .into(),
            more: None,
            problem: true,
        },
        Ok(Answer::Refused(why)) => refused("order", &why),
        Ok(Answer::Each(outcomes)) => {
            // The entry's outcome comes first; its take-profit and stop-loss
            // follow.
            let triggers: Vec<String> = outcomes
                .iter()
                .skip(1)
                .filter_map(|o| match o {
                    Outcome::Refused(why) => Some(format!("A take-profit or stop-loss was not placed: {why}")),
                    _ => None,
                })
                .collect();
            let more_triggers = (!triggers.is_empty()).then(|| triggers.join(" "));
            match outcomes.first() {
                Some(Outcome::Done {
                    filled: Some(traded),
                    resting,
                }) => {
                    let avg = traded
                        .avg_price
                        .map(|a| format!(" at an average of {}", rules::wire(a)))
                        .unwrap_or_default();
                    let filled = traded.size.unwrap_or(o.size);
                    let (text, rest) = if filled < o.size {
                        (
                            format!("{did} {} of {} {}{avg}.", rules::wire(filled), rules::wire(o.size), o.label),
                            Some(if *resting || !o.at_market {
                                "The rest rests at its price, listed in Open orders.".to_string()
                            } else {
                                format!(
                                    "The rest did not fill within the price limit of {}; nothing is left resting.",
                                    rules::wire(o.price)
                                )
                            }),
                        )
                    } else {
                        (format!("{did} {} {}{avg}.", rules::wire(filled), o.label), None)
                    };
                    let more = [rest, more_triggers].into_iter().flatten().collect::<Vec<_>>();
                    Report {
                        text: text.into(),
                        more: (!more.is_empty()).then(|| more.join(" ").into()),
                        problem: !triggers.is_empty(),
                    }
                }
                Some(Outcome::Done { resting: true, .. }) => Report {
                    text: format!(
                        "Order placed: {side} {} {} at {}. It is resting and listed in Open orders.",
                        rules::wire(o.size),
                        o.ticker,
                        rules::wire(o.price)
                    )
                    .into(),
                    more: more_triggers.map(Into::into),
                    problem: !triggers.is_empty(),
                },
                Some(Outcome::Done { .. }) => Report {
                    text: format!("Order sent: {side} {} {}.", rules::wire(o.size), o.ticker).into(),
                    more: more_triggers.map(Into::into),
                    problem: !triggers.is_empty(),
                },
                Some(Outcome::Refused(why)) if o.at_market && unmatched(why) => Report {
                    text: format!(
                        "Not filled: nothing on the book within the price limit of {}.",
                        rules::wire(o.price)
                    )
                    .into(),
                    more: Some(format!("Hyperliquid said: {why}").into()),
                    problem: true,
                },
                Some(Outcome::Refused(why)) => refused("order", why),
                None => Report {
                    text: "Hyperliquid's answer named no order. The orders are being reloaded.".into(),
                    more: None,
                    problem: true,
                },
            }
        }
    }
}

#[cfg(feature = "shot")]
impl Portfolio {
    /// `ticket=buy|sell-limit|tpsl|bad|nokey|filled|resting|refused` in
    /// `GALATA_DESK_SHOT_STATE`: open the ticket for `market` at `mid` in
    /// that state. The trading is a stand-in, so nothing can be sent.
    pub fn stage_ticket_shot(
        &mut self,
        market: MarketId,
        mid: Decimal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.actions.ticket_shot.take() else {
            return;
        };
        let availability = match state.as_str() {
            "nokey" => Err(Unavailable::NoKey(
                galata_desk_core::network::Network::Testnet,
            )),
            _ => Ok(()),
        };
        let staged = super::orders::Staged::new(availability);
        *staged.mid.borrow_mut() = Ok(Some(mid));
        let mut active = super::orders::staged_active_asset();
        active.mark_px = mid;
        // Up to about $18,700 either way, as at 5x on 3,737 USDC.
        let max = (Decimal::from(18_687) / mid).round_dp(2);
        active.max_trade_szs = [max, max];
        *staged.active.borrow_mut() = Ok(active);
        self.use_trading(std::rc::Rc::new(staged));
        let buy = !state.starts_with("sell");
        self.open_ticket(market.clone(), buy, window, cx);
        let up = |pct: i64| (mid * (Decimal::ONE + Decimal::new(pct, 2))).round_dp(2);
        let set = |input: &Entity<InputState>,
                   text: String,
                   window: &mut Window,
                   cx: &mut Context<Self>| {
            input.update(cx, |i, cx| i.set_value(text, window, cx));
        };
        let Some(t) = &mut self.actions.ticket else {
            return;
        };
        let (size, price, tp, sl) = (
            t.size.clone(),
            t.price.clone(),
            t.take_profit.clone(),
            t.stop_loss.clone(),
        );
        match state.as_str() {
            "sell-limit" => {
                t.at_market = false;
                t.unit = SizeUnit::Coin;
                set(&size, "2".into(), window, cx);
                set(&price, rules::wire(up(2)), window, cx);
            }
            "tpsl" => {
                t.with_tpsl = true;
                set(&size, "250".into(), window, cx);
                set(&tp, rules::wire(up(8)), window, cx);
                set(&sl, rules::wire(up(-5)), window, cx);
            }
            "bad" => set(&size, "5".into(), window, cx),
            "filled" | "resting" | "refused" => {
                let at_market = state != "resting";
                t.at_market = at_market;
                t.sent = true;
                let what = OrderSummary {
                    buy: true,
                    label: t.label(),
                    ticker: market.ticker().to_string(),
                    // What the form's $250 comes to at the order's price.
                    size: (Decimal::from(250) / if at_market { up(5) } else { up(-1) })
                        .round_dp_with_strategy(
                            2,
                            galata_desk_core::rust_decimal::RoundingStrategy::ToZero,
                        ),
                    price: if at_market { up(5) } else { up(-1) },
                    at_market,
                };
                let outcome = match state.as_str() {
                    "filled" => Outcome::Done {
                        filled: Some(galata_desk_core::trade::Traded {
                            size: Some(what.size),
                            avg_price: Some(mid),
                        }),
                        resting: false,
                    },
                    "resting" => Outcome::Done {
                        filled: None,
                        resting: true,
                    },
                    _ => Outcome::Refused(
                        "Order could not immediately match against any resting orders.".into(),
                    ),
                };
                set(&size, "250".into(), window, cx);
                self.actions.report = Some(report_order(&what, Ok(Answer::Each(vec![outcome]))));
            }
            _ => set(&size, "250".into(), window, cx),
        }
        cx.notify();
    }
}

#[cfg(test)]
impl Portfolio {
    /// Whether the ticket is open, and on which side and type.
    pub fn ticket_open(&self) -> Option<(String, bool, bool)> {
        self.actions
            .ticket
            .as_ref()
            .map(|t| (t.market.coin().to_string(), t.buy, t.at_market))
    }

    /// What the ticket says now, and whether it can be sent.
    pub fn ticket_state(&self, cx: &App) -> Option<(String, bool)> {
        let Some(t) = &self.actions.ticket else {
            return None;
        };
        if let Some(why) = self.actions.availability.as_ref().err() {
            return Some((explain(why, Acting::Placing).0, false));
        }
        Some(match self.ticket_check(t, cx) {
            Ok((order, pricing)) => (
                format!(
                    "{} {} at {}",
                    if t.buy { "buy" } else { "sell" },
                    rules::wire(order.size),
                    match pricing {
                        Pricing::Market { worst } => rules::wire(worst),
                        Pricing::Limit { price } => rules::wire(price),
                    }
                ),
                true,
            ),
            Err((text, _)) => (text, false),
        })
    }

    /// The ticket's size unit.
    pub fn ticket_unit(&self) -> Option<SizeUnit> {
        self.actions.ticket.as_ref().map(|t| t.unit)
    }

    /// Fill in the ticket, as typing does.
    pub fn type_ticket(
        &mut self,
        size: &str,
        price: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(t) = &self.actions.ticket {
            let (s, p) = (t.size.clone(), t.price.clone());
            s.update(cx, |input, cx| {
                input.set_value(size.to_string(), window, cx)
            });
            p.update(cx, |input, cx| {
                input.set_value(price.to_string(), window, cx)
            });
        }
        cx.notify();
    }

    pub fn type_ticket_tpsl(
        &mut self,
        tp: &str,
        sl: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(t) = &self.actions.ticket {
            let (a, b) = (t.take_profit.clone(), t.stop_loss.clone());
            a.update(cx, |input, cx| input.set_value(tp.to_string(), window, cx));
            b.update(cx, |input, cx| input.set_value(sl.to_string(), window, cx));
        }
        cx.notify();
    }

    pub fn set_ticket_for_test(
        &mut self,
        buy: Option<bool>,
        at_market: Option<bool>,
        unit: Option<SizeUnit>,
        reduce_only: Option<bool>,
        with_tpsl: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        self.set_ticket(cx, |t| {
            if let Some(v) = buy {
                t.buy = v;
            }
            if let Some(v) = at_market {
                t.at_market = v;
            }
            if let Some(v) = unit {
                t.unit = v;
            }
            if let Some(v) = reduce_only {
                t.reduce_only = v;
            }
            if let Some(v) = with_tpsl {
                t.with_tpsl = v;
            }
        });
    }

    pub fn ticket_share(&mut self, pct: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_ticket_share(pct, window, cx);
    }

    /// The ticket's Quick buttons, and the most that can be traded while
    /// any of them is out of reach.
    pub fn ticket_quick_buttons(&self, cx: &App) -> Option<(Vec<QuickButton>, Option<u64>)> {
        self.actions
            .ticket
            .as_ref()
            .map(|t| self.ticket_quick(t, cx))
    }

    /// The ticket's size field, as typed.
    pub fn ticket_size_text(&self, cx: &App) -> Option<String> {
        self.actions
            .ticket
            .as_ref()
            .map(|t| t.size.read(cx).value().to_string())
    }

    /// The ticket's result line, once it has one.
    pub fn ticket_outcome(&self) -> Option<(String, Option<String>, bool)> {
        match &self.actions.ticket {
            Some(t) if t.sent => self.actions.report.as_ref().map(|r| {
                (
                    r.text.to_string(),
                    r.more.as_ref().map(|m| m.to_string()),
                    r.problem,
                )
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
impl Portfolio {
    /// Close whatever dialog is open, as its Keep or Done button does.
    pub fn dismiss_dialog(&mut self, cx: &mut Context<Self>) {
        self.close_dialog(cx);
    }
}
