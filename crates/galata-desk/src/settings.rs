//! The settings page: which network the app shows, the API key kept for
//! each, and whether that key may act on the main network. Nothing here uses
//! a saved key; the Open orders tab does, through the gate in the core.

use galata_desk_core::api_key::ApiKey;
use galata_desk_core::connect::Address;
use galata_desk_core::network::Network;
use galata_desk_core::secrets::api_key_name;
use galata_desk_core::store::{QuickSizeRule, QuickSizes};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, ElementId, Entity, EventEmitter, Focusable, SharedString, Subscription,
    TestSupportExt, Window, div, px,
};

use crate::Core;
use crate::theme::{MONO, Palette};
use crate::ui;

/// What the page says a saved key is used for.
pub const KEY_USE: &str = "Mainnet and testnet each have their own key. A saved key is used to place orders, amend and cancel them, close positions, and set take-profit and stop-loss, each time you confirm one, and for nothing else.";

/// What the page says the mainnet switch allows.
pub const MAINNET_SWITCH_NOTE: &str = "Lets the saved mainnet key place, amend and cancel real orders, close real positions and set their take-profit and stop-loss. Testnet needs no permission. This guards against slips, not against someone else at this Mac.";

/// What the page says about quick sizes.
pub const QUICK_SIZES_NOTE: &str = "The dollar amounts the order ticket offers as Quick buttons. Each is the order's value, not its margin. Up to four, from $10. The same on mainnet and testnet. Saved as you type once every amount is valid; until then the last valid list stays in use.";

/// What the page asks of the window.
pub enum SettingsEvent {
    /// Show this network from now on.
    Network(Network),
    /// Allow, or stop allowing, order actions on the main network.
    MainnetTrading(bool),
    /// Offer these quick sizes in the order ticket. Only a list that keeps
    /// every rule is ever sent.
    QuickSizes(QuickSizes),
    /// Go back to where the user was.
    Done,
}

/// What is saved for the network in use.
enum Saved {
    Nothing,
    /// A key is saved; this is the address of the wallet it belongs to. The
    /// key itself is never kept here.
    Key(Address),
    /// Something is stored but it cannot be read as a key.
    Unreadable(String),
}

pub struct Settings {
    key: Entity<InputState>,
    saved: Saved,
    /// The outcome of the last save or removal, and whether it is a problem.
    message: Option<(SharedString, bool)>,
    /// One field per quick size, in the order the user arranged them.
    quick: Vec<Entity<InputState>>,
    _quick_typing: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for Settings {}

impl Settings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("0x\u{2026}")
        });
        let mut this = Self {
            key,
            saved: Saved::Nothing,
            message: None,
            quick: Vec::new(),
            _quick_typing: Vec::new(),
        };
        let amounts = Core::get(cx).quick_sizes.amounts().to_vec();
        this.set_quick_fields(&amounts, window, cx);
        // A screenshot run can show the saved state: `key=1` stores a
        // well-known test key in that run's in-memory secrets.
        #[cfg(feature = "shot")]
        if std::env::var("GALATA_DESK_SHOT_STATE")
            .unwrap_or_default()
            .split(',')
            .any(|kv| kv == "key=1")
        {
            let core = Core::get(cx);
            let _ = core
                .secrets
                .set(api_key_name(core.network), &format!("{:064x}", 1));
        }
        this.reload(cx);
        this
    }

    /// Read what is saved for the network in use. The keychain is asked here
    /// and after a change, never while drawing.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let core = Core::get(cx);
        self.saved = match core.secrets.get(api_key_name(core.network)) {
            Ok(None) => Saved::Nothing,
            Ok(Some(text)) => match ApiKey::parse(&text) {
                Ok(key) => Saved::Key(key.address()),
                Err(_) => Saved::Unreadable("what is stored is not an API key".into()),
            },
            Err(e) => Saved::Unreadable(e.to_string()),
        };
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.key.read(cx).value();
        let outcome = match ApiKey::parse(&text) {
            Err(e) => Err(format!("Not saved: {e}.")),
            Ok(key) => {
                let core = Core::get(cx);
                core.secrets
                    .set(api_key_name(core.network), &key.expose())
                    .map(|()| key.address())
                    .map_err(|e| format!("Not saved: {e}."))
            }
        };
        match outcome {
            Ok(address) => {
                // The field is cleared whatever happens next: the key is in
                // the keychain now and should be nowhere else.
                self.key
                    .update(cx, |input, cx| input.set_value(String::new(), window, cx));
                self.saved = Saved::Key(address);
                self.message = Some(("Saved to the keychain.".into(), false));
            }
            Err(problem) => self.message = Some((problem.into(), true)),
        }
        cx.notify();
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let core = Core::get(cx);
        match core.secrets.delete(api_key_name(core.network)) {
            Ok(()) => {
                self.saved = Saved::Nothing;
                self.message = Some(("Removed from the keychain.".into(), false));
            }
            Err(e) => self.message = Some((format!("Not removed: {e}.").into(), true)),
        }
        cx.notify();
    }

    /// Replace the quick size fields with one per amount.
    fn set_quick_fields(&mut self, amounts: &[u64], window: &mut Window, cx: &mut Context<Self>) {
        self.quick.clear();
        self._quick_typing.clear();
        for amount in amounts {
            self.push_quick_field(&amount.to_string(), window, cx);
        }
    }

    fn push_quick_field(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("10000"));
        input.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
        self._quick_typing
            .push(cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.quick_changed(cx);
                }
            }));
        self.quick.push(input);
    }

    fn quick_texts(&self, cx: &gpui_kit::App) -> Vec<String> {
        self.quick
            .iter()
            .map(|input| input.read(cx).value().to_string())
            .collect()
    }

    fn quick_check(&self, cx: &gpui_kit::App) -> Result<QuickSizes, Vec<Option<QuickSizeRule>>> {
        let texts = self.quick_texts(cx);
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        QuickSizes::check(&texts)
    }

    /// The fields changed: when they make a valid list that differs from the
    /// one in use, ask for it to be used and saved. Otherwise the last valid
    /// list stays in use, and the fields say what is wrong.
    fn quick_changed(&mut self, cx: &mut Context<Self>) {
        if let Ok(list) = self.quick_check(cx)
            && list != Core::get(cx).quick_sizes
        {
            cx.emit(SettingsEvent::QuickSizes(list));
        }
        cx.notify();
    }

    fn add_quick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick.len() >= QuickSizes::MOST {
            return;
        }
        self.push_quick_field("", window, cx);
        if let Some(input) = self.quick.last() {
            let focus = input.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
        self.quick_changed(cx);
    }

    fn remove_quick(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.quick.len() <= 1 || ix >= self.quick.len() {
            return;
        }
        self.quick.remove(ix);
        drop(self._quick_typing.remove(ix));
        self.quick_changed(cx);
    }

    fn reset_quick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_quick_fields(&QuickSizes::DEFAULT, window, cx);
        self.quick_changed(cx);
    }

    fn quick_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let problems = match self.quick_check(cx) {
            Ok(_) => vec![None; self.quick.len()],
            Err(problems) => problems,
        };
        let texts = self.quick_texts(cx);
        let only = self.quick.len() == 1;
        let in_use = Core::get(cx).quick_sizes.clone();
        let is_default = texts.iter().map(String::as_str).eq(QuickSizes::DEFAULT
            .map(|a| a.to_string())
            .iter()
            .map(String::as_str));
        let fields = self.quick.iter().enumerate().map(|(ix, input)| {
            // An empty field is still being written: no reason yet.
            let problem = problems
                .get(ix)
                .copied()
                .flatten()
                .filter(|_| !texts[ix].trim().is_empty());
            let name = format!("Quick size {}", ix + 1);
            v_flex()
                .id(ElementId::NamedInteger(
                    "quick-size-field".into(),
                    ix as u64,
                ))
                .w(px(132.))
                .gap(px(4.))
                .child(
                    Input::new(input)
                        .aria_label(name.clone())
                        .prefix(
                            div()
                                .font_family(MONO)
                                .text_color(Palette::muted())
                                .child("$"),
                        )
                        .suffix(
                            Button::new(("quick-size-remove", ix))
                                .ghost()
                                .xsmall()
                                .label("\u{d7}")
                                .accessibility_label(format!("Remove {name}"))
                                .disabled(only)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.remove_quick(ix, cx)),
                                ),
                        ),
                )
                .child(
                    div()
                        .id(("quick-size-problem", ix))
                        .min_h(px(15.))
                        .text_size(px(11.5))
                        .text_color(Palette::loss())
                        .when_some(problem, |d, rule| {
                            d.aria_label(rule.message()).child(rule.message())
                        }),
                )
        });
        ui::panel()
            .px_5()
            .py_4()
            .gap_3()
            .child(Self::heading("QUICK ORDER SIZES"))
            .child(ui::label(QUICK_SIZES_NOTE))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(px(10.))
                    .items_start()
                    .children(fields)
                    .when(self.quick.len() < QuickSizes::MOST, |row| {
                        row.child(
                            Button::new("quick-size-add")
                                .outline()
                                .label("Add a size")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.add_quick(window, cx)),
                                ),
                        )
                    }),
            )
            .child(
                h_flex()
                    .id("quick-size-preview")
                    .gap(px(6.))
                    .pt_3()
                    .border_t_1()
                    .border_color(Palette::line())
                    .child(
                        div()
                            .mr(px(6.))
                            .text_size(px(12.5))
                            .text_color(Palette::muted())
                            .child("In the ticket"),
                    )
                    .children(in_use.amounts().iter().map(|amount| {
                        div()
                            .id(("quick-size-preview", *amount))
                            .px(px(10.))
                            .py(px(4.))
                            .border_1()
                            .border_color(Palette::line())
                            .rounded(px(7.))
                            .font_family(MONO)
                            .text_size(px(12.5))
                            .aria_label(QuickSizes::label(*amount))
                            .child(QuickSizes::label(*amount))
                    }))
                    .child(div().flex_1())
                    .child(
                        Button::new("quick-size-reset")
                            .small()
                            .outline()
                            .label("Reset to $5k, $10k, $25k")
                            .disabled(is_default)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.reset_quick(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn heading(text: impl Into<SharedString>) -> gpui_kit::Div {
        div()
            .text_size(px(12.))
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .text_color(Palette::muted())
            .child(text.into())
    }

    fn network_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let core = Core::get(cx);
        let network = core.network;
        let overridden = core.overridden();
        let host: String = core
            .info_url()
            .split("//")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or_default()
            .to_string();
        ui::panel()
            .px_5()
            .py_4()
            .gap_3()
            .child(Self::heading("NETWORK"))
            .child(
                h_flex()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            // Let the explanation wrap; it must never push
                            // the switch out of the panel.
                            .min_w_0()
                            .gap(px(3.))
                            .child(
                                div()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child("Use testnet"),
                            )
                            .child(ui::label(
                                "Hyperliquid's test network, with test funds. Everything the app shows comes from it while this is on.",
                            )),
                    )
                    .child(div().flex_shrink_0().child(
                        Switch::new("testnet")
                            .checked(network.is_testnet())
                            .accessibility_label("Use testnet")
                            .on_click(cx.listener(|_, on: &bool, _, cx| {
                                cx.emit(SettingsEvent::Network(if *on {
                                    Network::Testnet
                                } else {
                                    Network::Mainnet
                                }));
                            })),
                    )),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_size(px(12.5))
                    .text_color(Palette::muted())
                    .child("Now showing")
                    .child(div().text_color(Palette::text()).child(network.label()))
                    .child("\u{b7}")
                    .child(div().font_family(MONO).child(host)),
            )
            .when(overridden, |d| {
                d.child(ui::notice(
                    "The venue's addresses are set by the development environment and are used whatever this switch says.",
                ))
            })
            .into_any_element()
    }

    fn trading_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let allowed = Core::get(cx).allow_mainnet_trading;
        ui::panel()
            .px_5()
            .py_4()
            .gap_3()
            .child(Self::heading("TRADING"))
            .child(
                h_flex()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(3.))
                            .child(
                                div()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child("Allow trading on mainnet"),
                            )
                            .child(ui::label(MAINNET_SWITCH_NOTE)),
                    )
                    .child(
                        div().flex_shrink_0().child(
                            Switch::new("mainnet-trading")
                                .checked(allowed)
                                .accessibility_label("Allow trading on mainnet")
                                .on_click(cx.listener(|_, on: &bool, _, cx| {
                                    cx.emit(SettingsEvent::MainnetTrading(*on));
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn key_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let network = Core::get(cx).network;
        let body: AnyElement = match &self.saved {
            Saved::Key(address) => h_flex()
                .gap_3()
                .px_3()
                .py(px(10.))
                .border_1()
                .border_color(Palette::line())
                .rounded(px(8.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(3.))
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .child("Saved \u{b7} API wallet")
                                .child(div().font_family(MONO).child(address.to_string())),
                        )
                        .child(ui::label(
                            "Check this address against the one Hyperliquid shows for your API wallet.",
                        )),
                )
                .child(
                    Button::new("remove-key")
                        .small()
                        .label("Remove")
                        .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                )
                .into_any_element(),
            Saved::Unreadable(why) => h_flex()
                .gap_3()
                .child(div().flex_1().child(ui::notice(format!(
                    "The saved key cannot be read: {why}. Remove it and save it again."
                ))))
                .child(
                    Button::new("remove-key")
                        .small()
                        .label("Remove")
                        .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                )
                .into_any_element(),
            Saved::Nothing => v_flex()
                .gap(px(4.))
                .child(ui::label("Private key (64 hexadecimal characters)"))
                .child(
                    h_flex()
                        .gap_3()
                        .child(div().flex_1().child(Input::new(&self.key)))
                        .child(
                            Button::new("save-key")
                                .primary()
                                .label("Save")
                                .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                        ),
                )
                .into_any_element(),
        };
        ui::panel()
            .px_5()
            .py_4()
            .gap_3()
            .child(Self::heading(format!(
                "API KEY FOR {}",
                network.label().to_uppercase()
            )))
            .child(ui::label(
                "The private key of an API wallet you created on Hyperliquid. It can place and cancel orders for your account and cannot withdraw. It is kept in the macOS Keychain, never in a file, and is not shown again after saving.",
            ))
            .child(body)
            .children(self.message.clone().map(|(text, problem)| {
                div()
                    .text_size(px(12.5))
                    .text_color(if problem {
                        Palette::loss()
                    } else {
                        Palette::muted()
                    })
                    .child(text)
            }))
            .child(ui::label(
                KEY_USE,
            ))
            .into_any_element()
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The page scrolls when the window is shorter than its sections.
        div()
            .id("settings-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                h_flex().w_full().justify_center().p_5().child(
                    v_flex()
                        .w(px(680.))
                        .max_w_full()
                        .flex_shrink_0()
                        .gap_4()
                        .child(
                            h_flex()
                                .child(
                                    div()
                                        .text_size(px(20.))
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child("Settings"),
                                )
                                .child(div().flex_1())
                                .child(
                                    Button::new("settings-done").small().label("Done").on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Done)),
                                    ),
                                ),
                        )
                        .child(self.network_panel(cx))
                        .child(self.key_panel(cx))
                        .child(self.trading_panel(cx))
                        .child(self.quick_panel(cx)),
                ),
            )
            .test_support()
    }
}

#[cfg(test)]
impl Settings {
    /// Type into the key field.
    pub fn set_key_text(&self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.key.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
    }

    pub fn key_text(&self, cx: &gpui_kit::App) -> String {
        self.key.read(cx).value().to_string()
    }

    /// The address shown for the saved key, if one is saved.
    pub fn saved_address(&self) -> Option<String> {
        match &self.saved {
            Saved::Key(address) => Some(address.to_string()),
            _ => None,
        }
    }

    /// Type into the quick size field at `ix`.
    pub fn set_quick_text(
        &self,
        ix: usize,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.quick[ix].update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
    }

    /// Type into the quick size field at `ix`, as the keyboard does: the
    /// list is checked and, when valid, used.
    pub fn type_quick(
        &mut self,
        ix: usize,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_quick_text(ix, text, window, cx);
        self.quick_changed(cx);
    }

    /// What the quick size fields hold.
    pub fn quick_field_texts(&self, cx: &gpui_kit::App) -> Vec<String> {
        self.quick_texts(cx)
    }

    /// The reason under each quick size field, if any.
    pub fn quick_problems(&self, cx: &gpui_kit::App) -> Vec<Option<&'static str>> {
        let texts = self.quick_texts(cx);
        match self.quick_check(cx) {
            Ok(_) => vec![None; texts.len()],
            Err(problems) => problems
                .into_iter()
                .zip(&texts)
                .map(|(p, t)| {
                    p.filter(|_| !t.trim().is_empty())
                        .map(QuickSizeRule::message)
                })
                .collect(),
        }
    }

    /// The last message, and whether it reports a problem.
    pub fn message(&self) -> Option<(String, bool)> {
        self.message.as_ref().map(|(t, p)| (t.to_string(), *p))
    }
}
