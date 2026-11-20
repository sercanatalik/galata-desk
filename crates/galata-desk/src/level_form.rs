//! The form that adds a call level: in the Watcher section, and in the
//! popover a Markets tile opens. One form, so a level gets the same checks,
//! the same default direction and the same save wherever it is added.

use galata_desk_core::format as f;
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::watcher::Direction;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, EventEmitter, SharedString, Window, div, px};

use crate::theme::Palette;
use crate::ui;
use crate::watcher_link::WatcherLink;

/// What the form tells its owner.
pub enum LevelFormEvent {
    /// The watcher saved the new level.
    Saved,
    /// The user closed the form without saving.
    Cancelled,
}

pub struct LevelForm {
    /// Ids of this form's controls start with this, so two forms can be on
    /// screen at once.
    prefix: &'static str,
    link: Entity<WatcherLink>,
    /// The market is typed, or fixed by where the form was opened.
    market: Entity<InputState>,
    fixed_market: Option<String>,
    price: Entity<InputState>,
    note: Entity<InputState>,
    /// `None` follows the last price.
    direction: Option<Direction>,
    /// The market's last price, when the opener knows it.
    last: Option<Decimal>,
    /// Why the last save didn't happen.
    message: Option<SharedString>,
    saving: bool,
    cancellable: bool,
}

impl EventEmitter<LevelFormEvent> for LevelForm {}

impl LevelForm {
    /// A form for any market, typed by the user.
    pub fn open(
        prefix: &'static str,
        link: Entity<WatcherLink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(prefix, link, None, None, false, window, cx)
    }

    /// A form for `market`, its price set to `last`, which can be cancelled.
    pub fn for_market(
        prefix: &'static str,
        link: Entity<WatcherLink>,
        market: String,
        last: Option<Decimal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(prefix, link, Some(market), last, true, window, cx)
    }

    fn build(
        prefix: &'static str,
        link: Entity<WatcherLink>,
        fixed_market: Option<String>,
        last: Option<Decimal>,
        cancellable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let market = cx.new(|cx| InputState::new(window, cx).placeholder("BTC or xyz:NVDA"));
        let price = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Price")
                .default_value(last.map(|p| p.normalize().to_string()).unwrap_or_default())
        });
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("Note (optional)"));
        Self {
            prefix,
            link,
            market,
            fixed_market,
            price,
            note,
            direction: None,
            last,
            message: None,
            saving: false,
            cancellable,
        }
    }

    fn id(&self, what: &str) -> SharedString {
        format!("{}-{what}", self.prefix).into()
    }

    fn typed_price(&self, cx: &gpui_kit::App) -> Option<Decimal> {
        self.price
            .read(cx)
            .value()
            .trim()
            .replace(',', "")
            .parse::<Decimal>()
            .ok()
    }

    /// What "Auto" will choose, when it can tell already.
    fn auto_direction(&self, cx: &gpui_kit::App) -> Option<Direction> {
        Some(Direction::from_last(self.typed_price(cx)?, self.last?))
    }

    fn choose(&mut self, direction: Option<Direction>, cx: &mut Context<Self>) {
        self.direction = direction;
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let market = match &self.fixed_market {
            Some(m) => m.clone(),
            None => self.market.read(cx).value().trim().to_string(),
        };
        if market.is_empty() {
            self.message = Some("Enter a market, such as BTC or xyz:NVDA.".into());
            cx.notify();
            return;
        }
        let Some(price) = self.typed_price(cx).filter(|p| *p > Decimal::ZERO) else {
            self.message = Some("Enter a price above zero.".into());
            cx.notify();
            return;
        };
        let note = Some(self.note.read(cx).value().trim().to_string()).filter(|n| !n.is_empty());
        let (direction, last) = (self.direction, self.last);
        let adding = self.link.update(cx, |link, cx| {
            link.add_level(market, price, direction, note, last, cx)
        });
        self.saving = true;
        self.message = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = adding.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match outcome {
                    Ok(_) => {
                        for input in [&this.market, &this.price, &this.note] {
                            input.update(cx, |i, cx| i.set_value(String::new(), window, cx));
                        }
                        this.direction = None;
                        cx.emit(LevelFormEvent::Saved);
                    }
                    Err(why) => this.message = Some(why.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The form, with its controls available only while `enabled`.
    pub fn render_form(&self, enabled: bool, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let auto = match self.auto_direction(cx) {
            Some(d) => format!("Auto ({})", d.label()),
            None => "Auto".to_string(),
        };
        let choice =
            |id: SharedString, label: String, value: Option<Direction>, cx: &mut Context<Self>| {
                let chosen = self.direction == value;
                let button = Button::new(id).small().label(label).disabled(!enabled);
                let button = if chosen {
                    button.primary()
                } else {
                    button.ghost()
                };
                button.on_click(cx.listener(move |this, _, _, cx| this.choose(value, cx)))
            };
        v_flex()
            .gap(px(6.))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(match &self.fixed_market {
                        Some(m) => div()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(m.clone())
                            .into_any_element(),
                        None => div()
                            .w(px(130.))
                            .child(Input::new(&self.market).small())
                            .into_any_element(),
                    })
                    .child(div().w(px(120.)).child(Input::new(&self.price).small()))
                    .child(choice(self.id("auto"), auto, None, cx))
                    .child(choice(
                        self.id("above"),
                        "Above".into(),
                        Some(Direction::Above),
                        cx,
                    ))
                    .child(choice(
                        self.id("below"),
                        "Below".into(),
                        Some(Direction::Below),
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(120.))
                            .child(Input::new(&self.note).small()),
                    )
                    .child(
                        Button::new(self.id("save"))
                            .small()
                            .primary()
                            .label(if self.saving {
                                "Saving\u{2026}"
                            } else {
                                "Save alert"
                            })
                            .disabled(!enabled || self.saving)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    )
                    .when(self.cancellable, |d| {
                        d.child(
                            Button::new(self.id("cancel"))
                                .small()
                                .label("Cancel")
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(LevelFormEvent::Cancelled)),
                                ),
                        )
                    }),
            )
            .when_some(self.last, |d, last| {
                d.child(ui::label(format!("Last price {}", f::price(last))))
            })
            .children(self.message.clone().map(|m| {
                div()
                    .text_size(px(12.5))
                    .text_color(Palette::loss())
                    .child(m)
            }))
            .into_any_element()
    }
}

impl gpui_kit::Render for LevelForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.link.read(cx).snapshot().editable().is_some();
        self.render_form(enabled, cx)
    }
}

#[cfg(test)]
impl LevelForm {
    pub fn type_level(
        &self,
        market: &str,
        price: &str,
        note: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.market
            .update(cx, |i, cx| i.set_value(market.to_string(), window, cx));
        self.price
            .update(cx, |i, cx| i.set_value(price.to_string(), window, cx));
        self.note
            .update(cx, |i, cx| i.set_value(note.to_string(), window, cx));
    }

    pub fn message(&self) -> Option<String> {
        self.message.as_ref().map(|m| m.to_string())
    }
}
