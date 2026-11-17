//! The connect screen: shown until an account is connected.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{ClipboardItem, Context, SharedString, Task, div, px};

use crate::desk::Desk;
use crate::theme::{MONO, Palette};
use crate::ui;

/// Where a connection attempt stands.
pub enum ConnectState {
    /// Nothing in progress. `notice` says how the last attempt ended, if badly.
    Idle { notice: Option<SharedString> },
    /// The local server is starting.
    Starting { _task: Task<()> },
    /// The browser page is open and the app is waiting for the wallet.
    Waiting {
        url: SharedString,
        /// A proof arrived and was refused; the page may still try again.
        rejected: Option<SharedString>,
        /// Holds the attempt. Dropping it cancels the attempt and stops the server.
        _task: Task<()>,
    },
}

pub fn render(state: &ConnectState, cx: &mut Context<Desk>) -> impl IntoElement {
    let body = match state {
        ConnectState::Idle { notice } => v_flex()
            .gap_5()
            .children(notice.clone().map(ui::notice))
            .child(
                Button::new("connect")
                    .primary()
                    .large()
                    .label("Connect wallet")
                    .on_click(cx.listener(|this, _, window, cx| this.start_connect(window, cx))),
            )
            .child(
                v_flex()
                    .gap_1()
                    .text_size(px(13.))
                    .text_color(Palette::muted())
                    .child("Opens a page in your browser. Use Rabby or MetaMask there, or scan a QR code with a wallet on your phone.")
                    .child("You sign one plain-text message. No key leaves your wallet, and nothing here can trade or move funds."),
            ),
        ConnectState::Starting { .. } => v_flex()
            .gap_4()
            .child(div().text_color(Palette::muted()).child("Opening your browser\u{2026}")),
        ConnectState::Waiting { url, rejected, .. } => {
            let copy_url = url.clone();
            v_flex()
                .gap_5()
                .children(rejected.clone().map(ui::notice))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_size(px(17.)).font_weight(gpui_kit::FontWeight::MEDIUM).child("Waiting for your wallet"))
                        .child(
                            div()
                                .text_color(Palette::muted())
                                .child("Finish in the browser page that just opened: pick a wallet and sign the message. This attempt expires after five minutes."),
                        ),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(ui::label("If the browser did not open, open this address yourself"))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .px_3()
                                        .py_2()
                                        .bg(Palette::raise())
                                        .rounded(px(6.))
                                        .font_family(MONO)
                                        .text_size(px(12.))
                                        .text_color(Palette::muted())
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(url.clone()),
                                )
                                .child(Button::new("copy-url").small().label("Copy").on_click(
                                    move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(copy_url.to_string()));
                                    },
                                )),
                        ),
                )
                .child(
                    h_flex().child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_connect(cx))),
                    ),
                )
        }
    };

    h_flex().size_full().justify_center().items_center().child(
        v_flex()
            .w(px(460.))
            .gap_6()
            .pb(px(60.))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(28.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Your Hyperliquid portfolio"),
                    )
                    .child(
                        div()
                            .text_color(Palette::muted())
                            .child("Connect the wallet that owns the account. galata-desk shows what it is worth, how it has performed and what it holds. Read-only."),
                    ),
            )
            .child(body),
    )
}
