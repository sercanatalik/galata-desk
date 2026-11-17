//! The settings page: which network the app shows, and the API key kept for
//! each. Nothing here uses a saved key; it is only stored.

use galata_desk_core::api_key::ApiKey;
use galata_desk_core::connect::Address;
use galata_desk_core::network::Network;
use galata_desk_core::secrets::api_key_name;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, SharedString, TestSupportExt, Window, div, px,
};

use crate::Core;
use crate::theme::{MONO, Palette};
use crate::ui;

/// What the page asks of the window.
pub enum SettingsEvent {
    /// Show this network from now on.
    Network(Network),
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
        };
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
                "Mainnet and testnet each have their own key. Trading is not enabled yet: a saved key is stored and nothing uses it.",
            ))
            .into_any_element()
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("settings-page")
            .size_full()
            .items_start()
            .justify_center()
            .p_5()
            .child(
                v_flex()
                    .w(px(680.))
                    .max_w_full()
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
                    .child(self.key_panel(cx)),
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

    /// The last message, and whether it reports a problem.
    pub fn message(&self) -> Option<(String, bool)> {
        self.message.as_ref().map(|(t, p)| (t.to_string(), *p))
    }
}
