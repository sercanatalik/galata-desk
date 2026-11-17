//! The Funding section: what each open position pays or earns in funding at
//! current rates, and has since it opened. It has no state of its own: it is
//! drawn from what the Portfolio view already holds.

use chrono::{DateTime, Local};
use galata_desk_core::feed::Section;
use galata_desk_core::format as f;
use galata_desk_core::funding::{self, FundingRates, FundingRow};
use galata_desk_core::model::PositionRow;
use galata_desk_core::rust_decimal::Decimal;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Hsla, TestSupportExt, div, px};

use crate::theme::{MONO, Palette};
use crate::ui;

/// The table's columns: name, relative width, and whether it is a figure.
const COLUMNS: [(&str, f32); 8] = [
    ("Market", 1.3),
    ("Size", 1.3),
    ("Position value", 1.1),
    ("Rate (1h)", 0.9),
    ("Yearly", 0.8),
    ("Per hour", 1.0),
    ("Per day", 1.0),
    ("Since open", 1.1),
];

fn tone(value: Decimal) -> Hsla {
    ui::tone_of(if value > Decimal::ZERO {
        1
    } else if value < Decimal::ZERO {
        -1
    } else {
        0
    })
}

/// `positions` is `None` until they have loaded.
pub fn render(positions: Option<&[PositionRow]>, rates: &Section<FundingRates>) -> AnyElement {
    let empty = FundingRates::new();
    let known = rates.data.as_ref();
    let rows = positions.map(|p| funding::rows(p, known.unwrap_or(&empty)));
    let summary = rows.as_deref().map(funding::summary);
    // Flows need both the positions and the rates.
    let flows_ready = rows.is_some() && known.is_some();

    let as_of = match (positions, rates.fetched_at) {
        (Some(p), Some(at)) => format!(
            "{} positions \u{b7} rates as of {}",
            p.len(),
            DateTime::<Local>::from(at).format("%H:%M:%S")
        ),
        (Some(p), None) => format!("{} positions", p.len()),
        _ => String::new(),
    };

    let pending = |w: f32, h: f32| ui::placeholder(px(w), px(h)).into_any_element();
    let card = |name: &'static str, value: AnyElement, note: String| {
        ui::panel()
            .flex_1()
            .min_w(px(180.))
            .px_4()
            .py_3()
            .gap(px(6.))
            .child(ui::label(name))
            .child(value)
            .child(ui::label(note).text_size(px(12.)))
    };
    let money = |value: Decimal| {
        ui::figure(f::usd_signed(value), px(21.))
            .text_color(tone(value))
            .into_any_element()
    };

    let left_out = summary
        .as_ref()
        .filter(|s| flows_ready && s.without_rate > 0)
        .map(|s| {
            format!(
                "{} position{} without a rate left out",
                s.without_rate,
                if s.without_rate == 1 { "" } else { "s" }
            )
        });
    let cards = h_flex()
        .flex_wrap()
        .gap(px(10.))
        .child(card(
            "Net per hour",
            match &summary {
                Some(s) if flows_ready => money(s.per_hour),
                _ => pending(120., 24.),
            },
            left_out.clone().unwrap_or("At current rates".into()),
        ))
        .child(card(
            "Net per day",
            match &summary {
                Some(s) if flows_ready => money(s.per_day()),
                _ => pending(120., 24.),
            },
            "If rates and sizes hold".into(),
        ))
        .child(card(
            "Since positions opened",
            match &summary {
                Some(s) => money(s.since_open),
                None => pending(120., 24.),
            },
            "Open positions only".into(),
        ))
        .child(card(
            "Positions",
            match &summary {
                Some(s) if flows_ready => ui::figure(
                    format!("{} earn \u{b7} {} pay", s.earning, s.paying),
                    px(21.),
                )
                .into_any_element(),
                _ => pending(140., 24.),
            },
            "By the current rate".into(),
        ));

    let cell = |ix: usize, content: AnyElement| {
        let (_, width) = COLUMNS[ix];
        div()
            .flex_basis(px(0.))
            // Relative widths, as flex factors.
            .flex_grow(width)
            .flex_shrink(1.)
            .when(ix > 0, |d| d.flex().justify_end())
            .child(content)
    };
    let header = h_flex()
        .px_4()
        .h(px(34.))
        .gap_3()
        .border_b_1()
        .border_color(Palette::line())
        .text_size(px(12.5))
        .text_color(Palette::muted())
        .children(
            COLUMNS
                .iter()
                .enumerate()
                .map(|(ix, (name, _))| cell(ix, div().child(*name).into_any_element())),
        );

    let figure = |text: String, colour: Hsla| {
        div()
            .font_family(MONO)
            .text_color(colour)
            .child(text)
            .into_any_element()
    };
    let plain = Palette::text();
    let dash = || figure("\u{2014}".into(), Palette::muted());
    let line = |row: &FundingRow| {
        let side = if row.size >= Decimal::ZERO {
            Palette::gain()
        } else {
            Palette::loss()
        };
        // A rate or flow: the figure, a placeholder while rates load, or a
        // dash when the venue's rates do not include this market.
        let from_rate = |value: Option<(String, Hsla)>| match value {
            Some((text, colour)) => figure(text, colour),
            None if known.is_none() => pending(56., 12.),
            None => dash(),
        };
        let name = h_flex()
            .gap_2()
            .font_family(MONO)
            .child(row.market.clone())
            .when(!row.dex.is_empty(), |d| {
                d.child(div().text_color(Palette::muted()).child(row.dex.clone()))
            })
            .child(
                div()
                    .text_color(Palette::muted())
                    .child(format!("{}x", row.leverage)),
            )
            .into_any_element();
        h_flex()
            .px_4()
            .h(px(34.))
            .gap_3()
            .border_b_1()
            .border_color(Palette::rule())
            .text_size(px(12.5))
            .child(cell(0, name))
            .child(cell(1, figure(f::size(row.size, &row.market), side)))
            .child(cell(2, figure(f::usd(row.value), plain)))
            .child(cell(
                3,
                from_rate(row.rate.map(|r| (f::funding_hourly(r), plain))),
            ))
            .child(cell(
                4,
                from_rate(row.rate.map(|r| (f::funding_yearly(r), plain))),
            ))
            .child(cell(
                5,
                from_rate(row.per_hour.map(|v| (f::usd_signed(v), tone(v)))),
            ))
            .child(cell(
                6,
                from_rate(row.per_day().map(|v| (f::usd_signed(v), tone(v)))),
            ))
            .child(cell(
                7,
                figure(f::usd_signed(row.since_open), tone(row.since_open)),
            ))
    };

    let body: AnyElement = match &rows {
        None => v_flex()
            .p_4()
            .gap_3()
            .children((0..6).map(|_| ui::placeholder(px(9999.), px(14.))))
            .into_any_element(),
        Some(rows) if rows.is_empty() => h_flex()
            .size_full()
            .justify_center()
            .text_color(Palette::muted())
            .child("No open positions, so no funding to pay or earn.")
            .into_any_element(),
        Some(rows) => div()
            .id("funding-rows")
            .size_full()
            .overflow_y_scroll()
            .children(rows.iter().map(line))
            .test_support()
            .into_any_element(),
    };

    v_flex()
        .id("funding")
        .size_full()
        .p_3()
        .gap_3()
        .child(
            h_flex()
                .gap_3()
                .items_baseline()
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child("Funding"),
                )
                .child(ui::label(as_of).text_size(px(13.))),
        )
        .when(rates.is_stale(), |d| {
            d.child(ui::notice(
                "The funding rates could not be refreshed. The rates and flows below are the last received; retrying.",
            ))
        })
        .when(rates.data.is_none() && rates.error.is_some(), |d| {
            d.child(ui::notice(
                "The funding rates could not be loaded. Position sizes and funding since open are shown; retrying.",
            ))
        })
        .child(cards)
        .child(
            ui::panel()
                .flex_1()
                .min_h_0()
                .child(header)
                .child(div().flex_1().min_h_0().child(body)),
        )
        .child(
            ui::label(
                "A plus is funding you receive, a minus funding you pay. Per hour and per day assume the current rate and position size hold. Largest flows first.",
            ),
        )
        .test_support()
        .into_any_element()
}
