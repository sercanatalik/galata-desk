//! Acting on positions from the Positions tab: close one at market or with a
//! limit order, close all at market, and set a position's take-profit and
//! stop-loss.
//!
//! As on the Open orders tab, the view never signs: it shows what an action
//! will do and hands it to a [`Trading`] when the user confirms. Every order
//! sent from here is reduce-only; the gate builds them so.

use galata_desk_core::trade::rules::{self, MARKET_SLIPPAGE_PCT};
use galata_desk_core::trade::sign::Tpsl;
use galata_desk_core::trade::{
    Answer, Holding, Level, Outcome, Pricing, TpslNow, TpslRequest, TpslStep, Unavailable,
};
#[cfg(test)]
use gpui_kit::App;
use gpui_kit::component::Disableable;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{ElementId, Subscription};

use super::orders::{
    Dialog, Doing, Report, facts, field, go_button, keep_button, listing, message_line, mono,
    network_badge, note, refused, signer, unsent,
};
use super::*;
use crate::grid::{Glyph, RowButton};

/// A mid price being fetched for a market close.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Mid {
    Loading,
    Got(Decimal),
    Failed(String),
}

/// A position's take-profit and stop-loss as the venue lists them: order id
/// and trigger price.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Levels {
    tp: Option<(u64, Decimal)>,
    sl: Option<(u64, Decimal)>,
    /// More than one of either is listed (set elsewhere); only the first is
    /// shown and changed here.
    more: bool,
}

impl Levels {
    fn now(&self) -> TpslNow {
        TpslNow {
            tp: self.tp.map(|l| l.0),
            sl: self.sl.map(|l| l.0),
        }
    }
}

pub(super) enum PositionDialog {
    Close {
        position: PositionRow,
        /// At market; otherwise with a limit order.
        market: bool,
        size: Entity<InputState>,
        price: Entity<InputState>,
        mid: Mid,
        _typing: [Subscription; 2],
    },
    /// The positions it was opened with: exactly these are closed.
    CloseAll {
        positions: Vec<PositionRow>,
        mids: Vec<Mid>,
    },
    Tpsl {
        position: PositionRow,
        levels: Levels,
        tp: Entity<InputState>,
        sl: Entity<InputState>,
        _typing: [Subscription; 2],
    },
}

impl PositionDialog {
    #[cfg(test)]
    pub(super) fn name(&self) -> &'static str {
        match self {
            PositionDialog::Close { market: true, .. } => "close-market",
            PositionDialog::Close { market: false, .. } => "close-limit",
            PositionDialog::CloseAll { .. } => "close-all",
            PositionDialog::Tpsl { .. } => "tpsl",
        }
    }
}

/// A close that passed the checks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CloseTo {
    size: Decimal,
    pricing: Pricing,
    /// The price it is valued at: the worst price, or the limit.
    at: Decimal,
}

fn holding(p: &PositionRow) -> Holding {
    Holding {
        asset: p
            .asset
            .expect("only positions in a known market reach a dialog"),
        size: p.size,
    }
}

fn key(p: &PositionRow) -> u64 {
    p.asset.map(|a| u64::from(a.number)).unwrap_or(0)
}

fn label(p: &PositionRow) -> String {
    if p.dex.is_empty() {
        p.market.clone()
    } else {
        format!("{} ({})", p.market, p.dex)
    }
}

/// The word for the closing order's side.
fn closing_side(p: &PositionRow) -> &'static str {
    if p.is_long() { "Sell" } else { "Buy" }
}

/// The worst price a market close of `p` may fill at.
fn worst(p: &PositionRow, mid: Decimal) -> Decimal {
    let asset = p.asset.expect("a known market");
    rules::slippage(mid, !p.is_long(), &asset, MARKET_SLIPPAGE_PCT)
}

/// Check the close form. The error says what to fix, and whether it is a
/// problem (rather than something still on its way, such as the price).
pub(super) fn close_check(
    p: &PositionRow,
    market: bool,
    size: &str,
    price: &str,
    mid: &Mid,
) -> Result<CloseTo, (String, bool)> {
    let asset = p.asset.expect("a known market");
    let (at, pricing) = if market {
        match mid {
            Mid::Got(mid) => {
                let worst = worst(p, *mid);
                (worst, Pricing::Market { worst })
            }
            Mid::Loading => return Err(("Reading the market price\u{2026}".into(), false)),
            Mid::Failed(why) => {
                return Err((format!("The market price could not be read: {why}."), true));
            }
        }
    } else {
        let price = rules::limit_close_price(price, &asset).map_err(|r| (r.to_string(), true))?;
        (price, Pricing::Limit { price })
    };
    let size = rules::close_size(size, p.size, &asset, at).map_err(|r| (r.to_string(), true))?;
    Ok(CloseTo { size, pricing, at })
}

/// Check the TP/SL form: a cleared field removes a level that is set; a
/// price that is not the level's now sets or moves it.
pub(super) fn tpsl_check(
    p: &PositionRow,
    levels: &Levels,
    tp: &str,
    sl: &str,
) -> Result<(Level, Level), (String, bool)> {
    let asset = p.asset.expect("a known market");
    let Some(mark) = p.mark else {
        return Err(("The position has no mark price.".into(), true));
    };
    let one = |text: &str, now: Option<(u64, Decimal)>, kind: Tpsl| -> Result<Level, String> {
        if text.trim().is_empty() {
            return Ok(if now.is_some() {
                Level::Remove
            } else {
                Level::Keep
            });
        }
        let price = rules::trigger_price(text, &asset, p.is_long(), mark, kind).map_err(|r| {
            let rule = r.to_string();
            match (r, kind) {
                // The side messages name the level already.
                (rules::Rule::WrongSide { .. }, _) => rule,
                (_, Tpsl::Tp) => format!("Take-profit: {rule}"),
                (_, Tpsl::Sl) => format!("Stop-loss: {rule}"),
            }
        })?;
        Ok(match now {
            Some((_, at)) if at == price => Level::Keep,
            _ => Level::At(price),
        })
    };
    let tp = one(tp, levels.tp, Tpsl::Tp).map_err(|e| (e, true))?;
    let sl = one(sl, levels.sl, Tpsl::Sl).map_err(|e| (e, true))?;
    if tp == Level::Keep && sl == Level::Keep {
        return Err(("Change a level to send.".into(), false));
    }
    Ok((tp, sl))
}

impl Portfolio {
    /// The listed positions that can be acted on.
    fn closable(&self) -> Vec<PositionRow> {
        self.holdings()
            .map(|h| {
                h.positions
                    .iter()
                    .filter(|p| p.asset.is_some() && !p.size.is_zero())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn position(&self, key: u64) -> Option<PositionRow> {
        self.closable().into_iter().find(|p| self::key(p) == key)
    }

    /// The position's take-profit and stop-loss, from the open orders.
    pub(super) fn levels(&self, p: &PositionRow) -> Levels {
        let mut levels = Levels::default();
        for o in self.snapshot.orders.data.iter().flatten() {
            if o.dex != p.dex || o.market != p.market {
                continue;
            }
            let (Some(kind), Some(at)) = (o.tpsl, o.trigger_px) else {
                continue;
            };
            let slot = match kind {
                Tpsl::Tp => &mut levels.tp,
                Tpsl::Sl => &mut levels.sl,
            };
            if slot.is_some() {
                levels.more = true;
            } else {
                *slot = Some((o.oid, at));
            }
        }
        levels
    }

    /// The TP/SL cell: `160 / --`, and its edit button.
    pub(super) fn tpsl_cell(&self, p: &PositionRow) -> Cell {
        let levels = self.levels(p);
        let show = |l: Option<(u64, Decimal)>| {
            l.map(|(_, at)| f::price_like(at, p.entry))
                .unwrap_or_else(|| "--".into())
        };
        let busy = self.pending_on(p) == Some(Doing::SettingTpsl);
        Cell::text(format!("{} / {}", show(levels.tp), show(levels.sl))).with_buttons(vec![
            RowButton {
                id: "edit-tpsl",
                key: key(p),
                glyph: Glyph::Pencil,
                name: self.control_name(p, "Edit take-profit and stop-loss", busy),
                disabled: !self.can_act() || p.asset.is_none(),
                busy,
            },
        ])
    }

    fn pending_on(&self, p: &PositionRow) -> Option<Doing> {
        let number = p.asset.map(|a| a.number)?;
        self.actions
            .pending
            .as_ref()
            .filter(|pending| pending.assets.contains(&number))
            .map(|pending| pending.doing)
    }

    fn control_name(&self, p: &PositionRow, action: &str, busy: bool) -> SharedString {
        if busy {
            "Sending\u{2026}".into()
        } else if p.asset.is_none() {
            format!("{action}: this market's details are still loading").into()
        } else {
            action.to_string().into()
        }
    }

    /// The Limit and Market buttons of a position's row.
    pub(super) fn position_buttons(&self, p: &PositionRow) -> Cell {
        let busy = self.pending_on(p) == Some(Doing::Closing);
        let off = !self.can_act() || p.asset.is_none();
        Cell::buttons(vec![
            RowButton {
                id: "close-limit",
                key: key(p),
                glyph: Glyph::Word("Limit"),
                name: self.control_name(p, "Close at limit", false),
                disabled: off,
                busy: false,
            },
            RowButton {
                id: "close-market",
                key: key(p),
                glyph: Glyph::Word("Market"),
                name: self.control_name(p, "Close at market", busy),
                disabled: off,
                busy,
            },
        ])
    }

    /// Draw the positions table's rows again: the positions changed, their
    /// take-profits and stop-losses did, or what can be done with them did.
    pub(super) fn rebuild_positions(&mut self, cx: &mut Context<Self>) {
        let Some(holdings) = self.holdings() else {
            return;
        };
        let rows = holdings
            .positions
            .iter()
            .map(|p| position_row(p, self.position_buttons(p), self.tpsl_cell(p)))
            .collect();
        let keys = holdings.positions.iter().map(position_keys).collect();
        self.positions.update(cx, |t, cx| {
            t.delegate_mut().set_keyed_rows(rows, keys);
            t.refresh(cx);
        });
    }

    /// A position row's button was pressed.
    pub(super) fn press_position(
        &mut self,
        id: &str,
        key: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_act() {
            return;
        }
        let Some(position) = self.position(key) else {
            return;
        };
        let input = |value: String, window: &mut Window, cx: &mut Context<Self>| {
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
        match id {
            "close-limit" | "close-market" => {
                let market = id == "close-market";
                let (size, typing_size) = input(rules::wire(position.size.abs()), window, cx);
                let mark = position.mark.map(rules::wire).unwrap_or_default();
                let (price, typing_price) = input(mark, window, cx);
                let dialog = PositionDialog::Close {
                    position: position.clone(),
                    market,
                    size,
                    price,
                    mid: Mid::Loading,
                    _typing: [typing_size, typing_price],
                };
                self.open(Dialog::Position(dialog), window, cx);
                if market {
                    self.fetch_mids(vec![position], cx);
                }
            }
            "edit-tpsl" => {
                let levels = self.levels(&position);
                let shown =
                    |l: Option<(u64, Decimal)>| l.map(|l| rules::wire(l.1)).unwrap_or_default();
                let (tp, typing_tp) = input(shown(levels.tp), window, cx);
                let (sl, typing_sl) = input(shown(levels.sl), window, cx);
                let dialog = PositionDialog::Tpsl {
                    position,
                    levels,
                    tp,
                    sl,
                    _typing: [typing_tp, typing_sl],
                };
                self.open(Dialog::Position(dialog), window, cx);
            }
            _ => {}
        }
    }

    pub(super) fn ask_close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_act() {
            return;
        }
        let positions = self.closable();
        if positions.is_empty() {
            return;
        }
        let mids = vec![Mid::Loading; positions.len()];
        self.open(
            Dialog::Position(PositionDialog::CloseAll {
                positions: positions.clone(),
                mids,
            }),
            window,
            cx,
        );
        self.fetch_mids(positions, cx);
    }

    /// Fetch the mid of each position's market for the open dialog, one
    /// after another. A dialog closed or replaced meanwhile drops the task.
    fn fetch_mids(&mut self, positions: Vec<PositionRow>, cx: &mut Context<Self>) {
        let trading = self.actions.trading.clone();
        self.actions.pricing = Some(cx.spawn(async move |this, cx| {
            for (i, p) in positions.iter().enumerate() {
                let mid = match trading.mid(p.dex.clone(), p.market.clone()).await {
                    Ok(Some(mid)) => Mid::Got(mid),
                    Ok(None) => Mid::Failed("Hyperliquid lists no price for this market".into()),
                    Err(why) => Mid::Failed(why),
                };
                let still = this.update(cx, |this, cx| {
                    let Some(Dialog::Position(dialog)) = &mut this.actions.dialog else {
                        return false;
                    };
                    match dialog {
                        PositionDialog::Close { mid: slot, .. } => *slot = mid.clone(),
                        PositionDialog::CloseAll { mids, .. } => {
                            if let Some(slot) = mids.get_mut(i) {
                                *slot = mid.clone();
                            }
                        }
                        PositionDialog::Tpsl { .. } => return false,
                    }
                    cx.notify();
                    true
                });
                if !matches!(still, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// Ask for the market prices again after a failure.
    fn retry_mids(&mut self, cx: &mut Context<Self>) {
        let positions = match &mut self.actions.dialog {
            Some(Dialog::Position(PositionDialog::Close { position, mid, .. })) => {
                *mid = Mid::Loading;
                vec![position.clone()]
            }
            Some(Dialog::Position(PositionDialog::CloseAll { positions, mids })) => {
                mids.iter_mut().for_each(|m| *m = Mid::Loading);
                positions.clone()
            }
            _ => return,
        };
        self.fetch_mids(positions, cx);
        cx.notify();
    }

    /// Put a share of the position in the close form's size.
    pub(super) fn pick_share(&mut self, pct: u32, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Dialog::Position(PositionDialog::Close { position, size, .. })) =
            &self.actions.dialog
        {
            let asset = position.asset.expect("a known market");
            let share = rules::wire(rules::share_of(position.size, pct, &asset));
            size.update(cx, |input, cx| input.set_value(share, window, cx));
            cx.notify();
        }
    }

    /// The user confirmed a position dialog: send what it shows.
    pub(super) fn confirm_position(&mut self, dialog: PositionDialog, cx: &mut Context<Self>) {
        match dialog {
            PositionDialog::Close {
                position,
                market,
                size,
                price,
                mid,
                _typing,
            } => {
                let checked = close_check(
                    &position,
                    market,
                    &size.read(cx).value(),
                    &price.read(cx).value(),
                    &mid,
                );
                let Ok(to) = checked else {
                    // The button is disabled while the form is not valid;
                    // should this be reached anyway, the form stays open.
                    self.actions.dialog = Some(Dialog::Position(PositionDialog::Close {
                        position,
                        market,
                        size,
                        price,
                        mid,
                        _typing,
                    }));
                    return;
                };
                let reply = self
                    .actions
                    .trading
                    .close(holding(&position), to.size, to.pricing);
                let asset = holding(&position).asset.number;
                self.send_for(
                    Vec::new(),
                    vec![asset],
                    Doing::Closing,
                    reply,
                    cx,
                    move |answer| report_close(&position, &to, answer),
                );
            }
            PositionDialog::CloseAll { positions, mids } => {
                let worsts: Option<Vec<Decimal>> = positions
                    .iter()
                    .zip(&mids)
                    .map(|(p, m)| match m {
                        Mid::Got(mid) => Some(worst(p, *mid)),
                        _ => None,
                    })
                    .collect();
                let Some(worsts) = worsts else {
                    self.actions.dialog = Some(Dialog::Position(PositionDialog::CloseAll {
                        positions,
                        mids,
                    }));
                    return;
                };
                let closes = positions
                    .iter()
                    .zip(&worsts)
                    .map(|(p, w)| (holding(p), *w))
                    .collect();
                let reply = self.actions.trading.close_all(closes);
                let assets = positions.iter().map(|p| holding(p).asset.number).collect();
                self.send_for(
                    Vec::new(),
                    assets,
                    Doing::Closing,
                    reply,
                    cx,
                    move |answer| report_close_all(&positions, answer),
                );
            }
            PositionDialog::Tpsl {
                position,
                levels,
                tp,
                sl,
                _typing,
            } => {
                let checked = tpsl_check(
                    &position,
                    &levels,
                    &tp.read(cx).value(),
                    &sl.read(cx).value(),
                );
                let Ok((tp_level, sl_level)) = checked else {
                    self.actions.dialog = Some(Dialog::Position(PositionDialog::Tpsl {
                        position,
                        levels,
                        tp,
                        sl,
                        _typing,
                    }));
                    return;
                };
                let reply = self.actions.trading.set_tpsl(
                    holding(&position),
                    levels.now(),
                    tp_level,
                    sl_level,
                );
                let asset = holding(&position).asset.number;
                self.send_for(
                    Vec::new(),
                    vec![asset],
                    Doing::SettingTpsl,
                    reply,
                    cx,
                    move |steps| report_tpsl(&position, tp_level, sl_level, steps),
                );
            }
        }
    }

    /// The Close all button, at the end of the tab bar.
    pub(super) fn close_all_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self
            .actions
            .pending
            .as_ref()
            .is_some_and(|p| p.doing == Doing::Closing && p.assets.len() > 1);
        Button::new("close-all")
            .ghost()
            .small()
            .label("Close all")
            .tooltip("Close all positions at market")
            .accessibility_label("Close all positions at market")
            .disabled(!self.can_act() || self.closable().is_empty())
            .loading(busy && !cx.reduce_motion())
            .on_click(cx.listener(|this, _, window, cx| this.ask_close_all(window, cx)))
            .into_any_element()
    }

    /// A position dialog, over the screen.
    pub(super) fn position_dialog(
        &self,
        dialog: &PositionDialog,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let network = Core::get(cx).network;
        let wallet = self.actions.wallet.as_deref();
        let retry = |cx: &mut Context<Self>| {
            Button::new("mid-retry")
                .small()
                .label("Try again")
                .on_click(cx.listener(|this, _, _, cx| this.retry_mids(cx)))
        };
        let position_line = |p: &PositionRow| {
            h_flex()
                .flex_wrap()
                .gap_2()
                .child(
                    div()
                        .text_color(ui::tone_of(if p.is_long() { 1 } else { -1 }))
                        .child(if p.is_long() { "Long" } else { "Short" }),
                )
                .child(format!(
                    "{} {} \u{b7} mark {}",
                    f::amount(p.size.abs()),
                    label(p),
                    p.mark
                        .map(|m| f::price_like(m, p.entry))
                        .unwrap_or_else(|| "\u{2014}".into())
                ))
                .into_any_element()
        };

        let (id, title, body, buttons): (&'static str, String, AnyElement, AnyElement) =
            match dialog {
                PositionDialog::Close {
                    position,
                    market,
                    size,
                    price,
                    mid,
                    ..
                } => {
                    let checked = close_check(
                        position,
                        *market,
                        &size.read(cx).value(),
                        &price.read(cx).value(),
                        mid,
                    );
                    let side = closing_side(position);
                    let (message, problem) = match &checked {
                        Ok(to) => (
                            match to.pricing {
                                Pricing::Market { worst } => format!(
                                    "Sends: {side} {} {} at market, no worse than {}, on {}.",
                                    rules::wire(to.size),
                                    label(position),
                                    rules::wire(worst),
                                    network.label()
                                ),
                                Pricing::Limit { price } => format!(
                                    "Sends: {side} {} {} at {} on {}.",
                                    rules::wire(to.size),
                                    label(position),
                                    rules::wire(price),
                                    network.label()
                                ),
                            },
                            false,
                        ),
                        Err((text, problem)) => (text.clone(), *problem),
                    };
                    let leaves = match &checked {
                        Ok(to) if to.size == position.size.abs() => {
                            "Closes the whole position.".into()
                        }
                        Ok(to) => format!(
                            "Leaves: {} {}",
                            if position.is_long() { "long" } else { "short" },
                            f::size(position.size.abs() - to.size, &position.market)
                        ),
                        Err(_) => "\u{2014}".into(),
                    };
                    let mut rows = vec![("Position", position_line(position))];
                    if *market {
                        let worst_text = match mid {
                            Mid::Got(m) => rules::wire(worst(position, *m)),
                            Mid::Loading => "\u{2026}".into(),
                            Mid::Failed(_) => "\u{2014}".into(),
                        };
                        rows.push(("Worst price", mono(worst_text)));
                    }
                    rows.push(("After", mono(leaves)));
                    rows.push(("Network", network_badge(network)));
                    rows.push(("Signed by", signer(wallet)));
                    let shares = h_flex().gap_1().children([25u32, 50, 75, 100].map(|pct| {
                        Button::new(ElementId::NamedInteger("share".into(), u64::from(pct)))
                            .small()
                            .outline()
                            .label(format!("{pct}%"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_share(pct, window, cx)
                            }))
                    }));
                    let through = !*market
                        && rules::limit_close_price(
                            &price.read(cx).value(),
                            &position.asset.expect("known"),
                        )
                        .ok()
                        .zip(position.mark)
                        .is_some_and(|(p, mark)| rules::fills_at_once(p, mark, position.is_long()));
                    let explain = if *market {
                        format!(
                            "Fills at once at the best prices on the book, {} the worst price, or not at all. Nothing is left resting. The order is reduce-only.",
                            if position.is_long() {
                                "at or above"
                            } else {
                                "at or below"
                            }
                        )
                    } else if through {
                        "This price is already through the market: the order will fill at once."
                            .into()
                    } else {
                        "The order rests on the book at this price and is listed in Open orders until it fills. It is reduce-only.".into()
                    };
                    let inputs = if *market {
                        h_flex()
                            .gap_3()
                            .items_end()
                            .child(field(format!("Size ({})", position.market), size))
                            .child(shares)
                            .into_any_element()
                    } else {
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(field("Limit price".into(), price))
                                    .child(field(format!("Size ({})", position.market), size)),
                            )
                            .child(shares)
                            .into_any_element()
                    };
                    (
                        if *market {
                            "close-market-dialog"
                        } else {
                            "close-limit-dialog"
                        },
                        if *market {
                            "Close at market".into()
                        } else {
                            "Close with a limit order".into()
                        },
                        v_flex()
                            .gap_3()
                            .child(inputs)
                            .child(facts(rows))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(message_line(
                                        "close-message",
                                        message,
                                        problem,
                                    )))
                                    .when(matches!(mid, Mid::Failed(_)) && *market, |d| {
                                        d.child(retry(cx))
                                    }),
                            )
                            .child(note(explain))
                            .into_any_element(),
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(keep_button("Keep position", cx))
                            .child(go_button(
                                if *market {
                                    "Close at market".into()
                                } else {
                                    "Place limit close".into()
                                },
                                checked.is_ok(),
                                cx,
                            ))
                            .into_any_element(),
                    )
                }
                PositionDialog::CloseAll { positions, mids } => {
                    let ready = mids.iter().all(|m| matches!(m, Mid::Got(_)));
                    let failed = mids.iter().any(|m| matches!(m, Mid::Failed(_)));
                    let lines = positions
                        .iter()
                        .zip(mids)
                        .map(|(p, m)| {
                            let worst_text = match m {
                                Mid::Got(mid) => {
                                    format!("no worse than {}", rules::wire(worst(p, *mid)))
                                }
                                Mid::Loading => "reading the price\u{2026}".into(),
                                Mid::Failed(why) => format!("price not read: {why}"),
                            };
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    div()
                                        .text_color(ui::tone_of(if p.is_long() { -1 } else { 1 }))
                                        .child(closing_side(p)),
                                )
                                .child(format!(
                                    "{} {} \u{b7} {worst_text}",
                                    f::amount(p.size.abs()),
                                    label(p)
                                ))
                                .into_any_element()
                        })
                        .collect();
                    let count = positions.len();
                    (
                    "close-all-dialog",
                    if count == 1 {
                        "Close the 1 position at market?".into()
                    } else {
                        format!("Close all {count} positions at market?")
                    },
                    v_flex()
                        .gap_3()
                        .child(listing("close-all-list", lines))
                        .child(facts(vec![
                            ("Network", network_badge(network)),
                            ("Signed by", signer(wallet)),
                        ]))
                        .when(failed, |d| {
                            d.child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(message_line(
                                        "close-all-message",
                                        "Some market prices could not be read, so nothing can be sent yet.".into(),
                                        true,
                                    )))
                                    .child(retry(cx)),
                            )
                        })
                        .child(note(
                            "Each position is closed whole, in one request, filling at once within its worst price or not at all. Any that cannot fill are named afterwards; the rest are still closed. Every order is reduce-only.",
                        ))
                        .into_any_element(),
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(keep_button("Keep positions", cx))
                        .child(go_button(
                            if count == 1 {
                                "Close 1 position".into()
                            } else {
                                format!("Close {count} positions")
                            },
                            ready,
                            cx,
                        ))
                        .into_any_element(),
                )
                }
                PositionDialog::Tpsl {
                    position,
                    levels,
                    tp,
                    sl,
                    ..
                } => {
                    let (tp_text, sl_text) = (tp.read(cx).value(), sl.read(cx).value());
                    let checked = tpsl_check(position, levels, &tp_text, &sl_text);
                    let (message, problem) = match &checked {
                        Ok((tp_level, sl_level)) => (
                            format!(
                                "Sends: {} on {}.",
                                describe_change(*tp_level, *sl_level),
                                network.label()
                            ),
                            false,
                        ),
                        Err((text, problem)) => (text.clone(), *problem),
                    };
                    let estimate = |text: &str| -> String {
                        let price = rules::limit_close_price(text, &position.asset.expect("known"));
                        match price {
                            Ok(price) => format!(
                                "about {}",
                                f::usd_signed(
                                    rules::result_at(position.size, position.entry, price)
                                        .round_dp(2)
                                )
                            ),
                            Err(_) => "\u{2014}".into(),
                        }
                    };
                    let mut rows = vec![
                        ("Position", position_line(position)),
                        ("Entry", mono(f::price(position.entry))),
                        ("At TP", mono(estimate(&tp_text))),
                        ("At SL", mono(estimate(&sl_text))),
                        ("Network", network_badge(network)),
                        ("Signed by", signer(wallet)),
                    ];
                    if levels.more {
                        rows.push((
                        "Note",
                        note("More than one take-profit or stop-loss is set for this position. The first of each is shown and changed here; the others are in Open orders."),
                    ));
                    }
                    (
                    "tpsl-dialog",
                    "Take-profit and stop-loss".into(),
                    v_flex()
                        .gap_3()
                        .child(
                            h_flex()
                                .gap_3()
                                .child(field("Take-profit price".into(), tp))
                                .child(field("Stop-loss price".into(), sl)),
                        )
                        .child(facts(rows))
                        .child(message_line("tpsl-message", message, problem))
                        .child(note(
                            "Each covers the whole position, whatever its size when it triggers, and closes it at market. Clear a price to remove that level. A level that is set is moved in place, never removed and placed again.",
                        ))
                        .into_any_element(),
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(keep_button("Keep as it is", cx))
                        .child(go_button("Save".into(), checked.is_ok(), cx))
                        .into_any_element(),
                )
                }
            };
        Some(self.dialog_frame(id, title, body, buttons, cx))
    }
}

/// `take-profit set at 160, stop-loss removed`.
fn describe_change(tp: Level, sl: Level) -> String {
    let one = |name: &str, level: Level| match level {
        Level::Keep => None,
        Level::At(price) => Some(format!("{name} at {}", rules::wire(price))),
        Level::Remove => Some(format!("{name} removed")),
    };
    [one("take-profit", tp), one("stop-loss", sl)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
}

/// No answer: whether the action took effect is not known.
fn silent(what: &str, why: &str) -> Report {
    Report {
        text: format!(
            "No answer from Hyperliquid ({why}). The {what} may or may not have gone through; positions and orders are being reloaded."
        )
        .into(),
        more: None,
        problem: true,
    }
}

/// An IOC that met no order within its price is refused in these words.
fn unmatched(why: &str) -> bool {
    why.contains("could not immediately match")
}

pub(super) fn report_close(
    p: &PositionRow,
    to: &CloseTo,
    answer: Result<Answer, Unavailable>,
) -> Report {
    let market = matches!(to.pricing, Pricing::Market { .. });
    match answer {
        Err(e) => unsent(&e),
        Ok(Answer::Unknown(why)) => silent("close", &why),
        Ok(Answer::Refused(why)) => refused("close", &why),
        Ok(Answer::Each(outcomes)) => match outcomes.first() {
            Some(Outcome::Done {
                filled: Some(traded),
                ..
            }) => {
                let avg = traded
                    .avg_price
                    .map(|a| format!(" at an average of {}", rules::wire(a)))
                    .unwrap_or_default();
                match traded.size {
                    Some(filled) if filled < to.size => Report {
                        text: format!(
                            "Closed {} of {} {}{avg}.",
                            rules::wire(filled),
                            rules::wire(to.size),
                            label(p)
                        )
                        .into(),
                        more: Some(
                            format!(
                                "The rest did not fill within the price limit of {}; nothing is left resting.",
                                rules::wire(to.at)
                            )
                            .into(),
                        ),
                        problem: false,
                    },
                    _ => Report {
                        text: format!("Closed {} {}{avg}.", rules::wire(to.size), label(p)).into(),
                        more: None,
                        problem: false,
                    },
                }
            }
            Some(Outcome::Done { resting: true, .. }) => Report {
                text: format!(
                    "Limit close placed: {} {} {} at {}. It is listed in Open orders.",
                    closing_side(p),
                    rules::wire(to.size),
                    label(p),
                    rules::wire(to.at)
                )
                .into(),
                more: None,
                problem: false,
            },
            Some(Outcome::Done { .. }) => Report {
                text: format!("Close sent: {} {}.", rules::wire(to.size), label(p)).into(),
                more: None,
                problem: false,
            },
            Some(Outcome::Refused(why)) if market && unmatched(why) => Report {
                text: format!(
                    "Not closed: nothing on the book could fill within the price limit of {}.",
                    rules::wire(to.at)
                )
                .into(),
                more: Some(format!("Hyperliquid said: {why}").into()),
                problem: true,
            },
            Some(Outcome::Refused(why)) => refused("close", why),
            None => silent("close", "its answer named no order"),
        },
    }
}

pub(super) fn report_close_all(
    positions: &[PositionRow],
    answer: Result<Answer, Unavailable>,
) -> Report {
    match answer {
        Err(e) => unsent(&e),
        Ok(Answer::Unknown(why)) => silent("closes", &why),
        Ok(Answer::Refused(why)) => refused("closes", &why),
        Ok(Answer::Each(outcomes)) => {
            let mut closed = 0;
            let mut notes = Vec::new();
            for (i, p) in positions.iter().enumerate() {
                match outcomes.get(i) {
                    Some(Outcome::Done {
                        filled: Some(traded),
                        ..
                    }) => {
                        closed += 1;
                        if let Some(filled) = traded.size.filter(|f| *f < p.size.abs()) {
                            notes.push(format!(
                                "{} closed in part ({} of {})",
                                label(p),
                                rules::wire(filled),
                                rules::wire(p.size.abs())
                            ));
                        }
                    }
                    Some(Outcome::Done { .. }) => closed += 1,
                    Some(Outcome::Refused(why)) if unmatched(why) => notes.push(format!(
                        "{} not closed (nothing filled within its price limit)",
                        label(p)
                    )),
                    Some(Outcome::Refused(why)) => {
                        notes.push(format!("{} not closed ({why})", label(p)))
                    }
                    None => notes.push(format!("{} not closed (no answer for it)", label(p))),
                }
            }
            let count = positions.len();
            Report {
                text: format!(
                    "Closed {closed} of {count} position{}.",
                    if count == 1 { "" } else { "s" }
                )
                .into(),
                more: (!notes.is_empty()).then(|| notes.join("; ").into()),
                problem: closed < count,
            }
        }
    }
}

pub(super) fn report_tpsl(p: &PositionRow, tp: Level, sl: Level, steps: Vec<TpslStep>) -> Report {
    let name = |kind: Tpsl| match kind {
        Tpsl::Tp => "take-profit",
        Tpsl::Sl => "stop-loss",
    };
    let price = |kind: Tpsl| match (kind, tp, sl) {
        (Tpsl::Tp, Level::At(at), _) | (Tpsl::Sl, _, Level::At(at)) => rules::wire(at),
        _ => String::new(),
    };
    let mut done = Vec::new();
    let mut not_done = Vec::new();
    let mut silent_parts = Vec::new();
    for step in &steps {
        for (i, kind) in step.levels.iter().enumerate() {
            let what = match step.kind {
                TpslRequest::Move => format!("{} moved to {}", name(*kind), price(*kind)),
                TpslRequest::Place => format!("{} set at {}", name(*kind), price(*kind)),
                TpslRequest::Remove => format!("{} removed", name(*kind)),
            };
            match &step.answer {
                Ok(Answer::Each(outcomes)) => match outcomes.get(i) {
                    Some(Outcome::Done { .. }) => done.push(what),
                    Some(Outcome::Refused(why)) => not_done.push(format!("{what} ({why})")),
                    None => not_done.push(format!("{what} (no answer for it)")),
                },
                Ok(Answer::Refused(why)) => not_done.push(format!("{what} ({why})")),
                Ok(Answer::Unknown(why)) => silent_parts.push(format!("{what} ({why})")),
                Err(e) => not_done.push(format!("{what} ({e})")),
            }
        }
    }
    // Lost before any request was known to be sent.
    if steps.iter().all(|s| s.levels.is_empty())
        && let Some(Ok(Answer::Unknown(why))) = steps.first().map(|s| &s.answer)
    {
        return silent("change", why);
    }
    let first_upper = |s: String| {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            None => s,
        }
    };
    let mut more = Vec::new();
    if !not_done.is_empty() {
        more.push(format!("Not done: {}.", not_done.join("; ")));
    }
    if !silent_parts.is_empty() {
        more.push(format!(
            "No answer for: {}. It may or may not have gone through; the orders are being reloaded.",
            silent_parts.join("; ")
        ));
    }
    let text = if done.is_empty() {
        format!("Nothing changed on {}.", label(p))
    } else {
        format!("{} on {}.", first_upper(done.join(", ")), label(p))
    };
    Report {
        text: text.into(),
        more: (!more.is_empty()).then(|| more.join(" ").into()),
        problem: !not_done.is_empty() || !silent_parts.is_empty(),
    }
}

#[cfg(feature = "shot")]
impl Portfolio {
    /// `positions=market|limit|limit-bad|all|tpsl|tpsl-bad|pending|done|partial|unfilled|all-partial|tpsl-partial|unknown|nokey|wrongkey|locked|override`
    /// in `GALATA_DESK_SHOT_STATE`: once the positions are in, show that
    /// state of the Positions tab. The trading is a stand-in whose mid is
    /// the first position's mark, so nothing can be sent.
    pub(super) fn show_position_shot_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(first) = self.closable().into_iter().next() else {
            return;
        };
        // TP/SL states read the levels from the orders: wait for them too.
        if self.snapshot.orders.data.is_none() {
            return;
        }
        let Some(state) = self.actions.position_shot.take() else {
            return;
        };
        let availability = match state.as_str() {
            "nokey" => Err(Unavailable::NoKey(
                galata_desk_core::network::Network::Testnet,
            )),
            "wrongkey" => Err(Unavailable::KeyNotApproved),
            "locked" => Err(Unavailable::MainnetOff),
            "override" => Err(Unavailable::Overridden),
            _ => Ok(()),
        };
        let staged = super::orders::Staged::new(availability);
        *staged.mid.borrow_mut() = Ok(first.mark);
        self.use_trading(std::rc::Rc::new(staged));
        let mid = first.mark.unwrap_or(first.entry);
        let worst = worst(&first, mid);
        let whole = first.size.abs();
        let part = rules::share_of(first.size, 50, &first.asset.expect("known"));
        let report = |to: CloseTo, outcome: Outcome| {
            report_close(&first, &to, Ok(Answer::Each(vec![outcome])))
        };
        let at_market = |size| CloseTo {
            size,
            pricing: Pricing::Market { worst },
            at: worst,
        };
        let filled = |size: Decimal| Outcome::Done {
            filled: Some(galata_desk_core::trade::Traded {
                size: Some(size),
                avg_price: Some(mid),
            }),
            resting: false,
        };
        let key = self::key(&first);
        match state.as_str() {
            "market" => self.press_position("close-market", key, window, cx),
            "limit" => self.press_position("close-limit", key, window, cx),
            "limit-bad" => {
                self.press_position("close-limit", key, window, cx);
                if let Some(Dialog::Position(PositionDialog::Close { size, .. })) =
                    &self.actions.dialog
                {
                    let tiny = Decimal::new(1, first.asset.expect("known").sz_decimals);
                    size.update(cx, |input, cx| {
                        input.set_value(rules::wire(tiny), window, cx)
                    });
                }
            }
            "all" => self.ask_close_all(window, cx),
            "tpsl" => self.press_position("edit-tpsl", key, window, cx),
            "tpsl-bad" => {
                self.press_position("edit-tpsl", key, window, cx);
                if let Some(Dialog::Position(PositionDialog::Tpsl { sl, .. })) =
                    &self.actions.dialog
                {
                    // A stop on the wrong side of the mark.
                    let wrong =
                        rules::slippage(mid, first.is_long(), &first.asset.expect("known"), 5);
                    sl.update(cx, |input, cx| {
                        input.set_value(rules::wire(wrong), window, cx)
                    });
                }
            }
            "pending" => {
                self.actions.pending = Some(super::orders::Pending {
                    oids: Vec::new(),
                    assets: vec![first.asset.expect("known").number],
                    doing: Doing::Closing,
                });
            }
            "done" => self.actions.report = Some(report(at_market(whole), filled(whole))),
            "partial" => {
                self.actions.report = Some(report(at_market(whole), filled(part)));
            }
            "unfilled" => {
                self.actions.report = Some(report(
                    at_market(whole),
                    Outcome::Refused(
                        "Order could not immediately match against any resting orders.".into(),
                    ),
                ));
            }
            "all-partial" => {
                let all = self.closable();
                let mut outcomes = vec![filled(whole); all.len()];
                if let Some(last) = outcomes.last_mut().filter(|_| all.len() > 1) {
                    *last = Outcome::Refused(
                        "Order could not immediately match against any resting orders.".into(),
                    );
                }
                self.actions.report = Some(report_close_all(&all, Ok(Answer::Each(outcomes))));
            }
            "tpsl-partial" => {
                let tp = rules::slippage(mid, !first.is_long(), &first.asset.expect("known"), 10);
                let sl = rules::slippage(mid, first.is_long(), &first.asset.expect("known"), 10);
                self.actions.report = Some(report_tpsl(
                    &first,
                    Level::At(tp),
                    Level::At(sl),
                    vec![
                        TpslStep {
                            kind: TpslRequest::Move,
                            levels: vec![Tpsl::Sl],
                            answer: Ok(Answer::Refused(
                                "Cannot modify canceled or filled order".into(),
                            )),
                        },
                        TpslStep {
                            kind: TpslRequest::Place,
                            levels: vec![Tpsl::Tp],
                            answer: Ok(Answer::Each(vec![Outcome::DONE])),
                        },
                    ],
                ));
            }
            "unknown" => {
                self.actions.report = Some(report_close(
                    &first,
                    &at_market(whole),
                    Ok(Answer::Unknown("the request timed out".into())),
                ));
            }
            _ => {}
        }
        self.rebuild_orders(cx);
        cx.notify();
    }
}

#[cfg(test)]
impl Portfolio {
    /// Each listed position's buttons, as `(key, name, disabled, busy)`.
    pub fn position_controls(&self) -> Vec<(u64, String, bool, bool)> {
        self.holdings()
            .iter()
            .flat_map(|h| h.positions.iter())
            .flat_map(|p| {
                let mut buttons = self.position_buttons(p).row_buttons().to_vec();
                buttons.extend(self.tpsl_cell(p).row_buttons().to_vec());
                buttons
                    .into_iter()
                    .map(|b| (b.key, b.name.to_string(), b.disabled, b.busy))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Each listed position's TP/SL cell text.
    pub fn tpsl_cells(&self) -> Vec<String> {
        self.holdings()
            .iter()
            .flat_map(|h| h.positions.iter())
            .map(|p| {
                let levels = self.levels(p);
                let show = |l: Option<(u64, Decimal)>| {
                    l.map(|(_, at)| f::price_like(at, p.entry))
                        .unwrap_or_else(|| "--".into())
                };
                format!("{} / {}", show(levels.tp), show(levels.sl))
            })
            .collect()
    }

    /// The asset numbers an action is in progress for.
    pub fn positions_pending(&self) -> Vec<u32> {
        self.actions
            .pending
            .as_ref()
            .map(|p| p.assets.clone())
            .unwrap_or_default()
    }

    pub fn can_close_all(&self) -> bool {
        self.can_act() && !self.closable().is_empty()
    }

    pub fn positions_unavailable(&self) -> Option<String> {
        self.actions
            .availability
            .as_ref()
            .err()
            .map(|e| super::orders::explain(e, super::orders::Acting::Positions).0)
    }

    pub fn press_position_button(
        &mut self,
        id: &str,
        key: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.press_position(id, key, window, cx);
    }

    pub fn press_close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_close_all(window, cx);
    }

    pub fn press_share(&mut self, pct: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_share(pct, window, cx);
    }

    /// Type into the close form: its size, and its limit price.
    pub fn type_close(
        &mut self,
        size: &str,
        price: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Dialog::Position(PositionDialog::Close {
            size: s, price: p, ..
        })) = &self.actions.dialog
        {
            s.update(cx, |input, cx| {
                input.set_value(size.to_string(), window, cx)
            });
            p.update(cx, |input, cx| {
                input.set_value(price.to_string(), window, cx)
            });
        }
        cx.notify();
    }

    /// Type into the TP/SL form.
    pub fn type_tpsl(&mut self, tp: &str, sl: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Dialog::Position(PositionDialog::Tpsl { tp: t, sl: s, .. })) =
            &self.actions.dialog
        {
            t.update(cx, |input, cx| input.set_value(tp.to_string(), window, cx));
            s.update(cx, |input, cx| input.set_value(sl.to_string(), window, cx));
        }
        cx.notify();
    }

    /// What the close form says, its size field, and whether it can be sent.
    pub fn close_state(&self, cx: &App) -> Option<(String, String, bool)> {
        match &self.actions.dialog {
            Some(Dialog::Position(PositionDialog::Close {
                position,
                market,
                size,
                price,
                mid,
                ..
            })) => {
                let size_text = size.read(cx).value().to_string();
                let checked =
                    close_check(position, *market, &size_text, &price.read(cx).value(), mid);
                Some(match checked {
                    Ok(to) => (
                        format!("{} at {}", rules::wire(to.size), rules::wire(to.at)),
                        size_text,
                        true,
                    ),
                    Err((text, _)) => (text, size_text, false),
                })
            }
            _ => None,
        }
    }

    /// Whether the Close all confirmation can be sent.
    pub fn close_all_ready(&self) -> Option<bool> {
        match &self.actions.dialog {
            Some(Dialog::Position(PositionDialog::CloseAll { mids, .. })) => {
                Some(mids.iter().all(|m| matches!(m, Mid::Got(_))))
            }
            _ => None,
        }
    }

    /// What the TP/SL form says, and whether it can be sent.
    pub fn tpsl_state(&self, cx: &App) -> Option<(String, bool)> {
        match &self.actions.dialog {
            Some(Dialog::Position(PositionDialog::Tpsl {
                position,
                levels,
                tp,
                sl,
                ..
            })) => Some(
                match tpsl_check(position, levels, &tp.read(cx).value(), &sl.read(cx).value()) {
                    Ok((tp, sl)) => (describe_change(tp, sl), true),
                    Err((text, _)) => (text, false),
                },
            ),
            _ => None,
        }
    }
}
