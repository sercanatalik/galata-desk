//! Small pieces every screen is built from.

use galata_desk_core::venue::VenueError;
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Div, Hsla, Pixels, SharedString, div, px};

use crate::theme::{MONO, Palette};

/// A surface resting on the window's ground.
pub fn panel() -> Div {
    v_flex()
        .bg(Palette::panel())
        .border_1()
        .border_color(Palette::line())
        .rounded(px(10.))
        .overflow_hidden()
}

/// A small label above or beside a figure.
pub fn label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.5))
        .text_color(Palette::muted())
        .child(text.into())
}

/// A figure: monospaced, so digits line up.
pub fn figure(text: impl Into<SharedString>, size: Pixels) -> Div {
    div().font_family(MONO).text_size(size).child(text.into())
}

/// A placeholder bar in the shape of a figure not yet loaded.
pub fn placeholder(width: Pixels, height: Pixels) -> impl IntoElement {
    Skeleton::new().w(width).h(height).rounded(px(4.))
}

/// One line of a key-value list: a label on the left, a figure on the right.
pub fn row(name: impl Into<SharedString>, value: impl IntoElement) -> Div {
    h_flex()
        .justify_between()
        .h(px(30.))
        .child(label(name).text_size(px(13.5)))
        .child(value)
}

/// The colour of a signed amount: gain, loss, or plain for zero.
pub fn tone_of(sign: i32) -> Hsla {
    match sign {
        s if s > 0 => Palette::gain(),
        s if s < 0 => Palette::loss(),
        _ => Palette::text(),
    }
}

/// A failure in words a person would use.
pub fn explain(error: &VenueError) -> String {
    match error {
        VenueError::RateLimited => {
            "Hyperliquid asked the app to slow down. It is waiting longer between refreshes.".into()
        }
        VenueError::Unreachable(_) => {
            "Hyperliquid could not be reached. Check the network connection.".into()
        }
        VenueError::Status(code) => format!("Hyperliquid answered with an error (HTTP {code})."),
        VenueError::Shape { query, .. } => {
            format!("Hyperliquid's answer to `{query}` was not in the expected form.")
        }
    }
}

/// An amber notice across the top of a section.
pub fn notice(text: impl Into<SharedString>) -> Div {
    h_flex()
        .px_3()
        .py_2()
        .gap_3()
        .bg(Palette::amber_ground())
        .border_1()
        .border_color(Palette::amber_line())
        .rounded(px(8.))
        .text_size(px(13.))
        .text_color(Palette::amber_text())
        .child(div().flex_1().child(text.into()))
}
