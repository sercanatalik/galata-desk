//! The covariance or correlation matrix as a heatmap.

use galata_desk_core::risk::report::{Focus, RiskReport};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Hsla, TestSupportExt, div, px};

use super::{Drawn, MatrixView, Risk};
use crate::theme::{MONO, Palette};
use crate::ui;

/// The strongest tint a cell takes, so its figure keeps its contrast.
const MAX_TINT: f32 = 0.5;

fn tint(value: f64, scale: f64) -> Hsla {
    let strength = if scale > 0.0 {
        (value.abs() / scale).min(1.0)
    } else {
        0.0
    } as f32;
    let base = if value >= 0.0 {
        Palette::gain()
    } else {
        Palette::loss()
    };
    base.opacity(0.06 + MAX_TINT * strength)
}

pub fn render(
    risk: &Risk,
    report: &RiskReport,
    f: &Focus,
    drawn: &mut Drawn,
    cx: &mut Context<Risk>,
) -> AnyElement {
    let n = report.included.len();
    let correlation = risk.matrix == MatrixView::Correlation;
    let source = if correlation {
        &f.correlation
    } else {
        &f.covariance
    };
    let scale = if correlation {
        1.0
    } else {
        (0..n)
            .flat_map(|i| (0..n).map(move |j| (i, j)))
            .map(|(i, j)| source.get(i, j).abs())
            .fold(0.0, f64::max)
    };
    let note = format!(
        "{} · {} candles, {} returns, scaled to 1 day · {}{}",
        f.period.label(),
        f.period.interval().as_str(),
        f.count,
        risk.settings.estimator.label(),
        if correlation { "" } else { " · ×10⁻⁴" }
    );
    drawn.matrix_note = note.clone();

    let label_w = px(96.);
    let head = h_flex()
        .gap(px(2.))
        .child(div().w(label_w).flex_shrink_0())
        .children(report.included.iter().map(|p| {
            div()
                .flex_1()
                .min_w(px(52.))
                .text_size(px(11.5))
                .text_color(Palette::muted())
                .text_center()
                .overflow_hidden()
                .child(p.market.ticker().to_string())
        }));
    let rows = report.included.iter().enumerate().map(|(i, p)| {
        h_flex()
            .gap(px(2.))
            .child(
                div()
                    .w(label_w)
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .text_color(Palette::muted())
                    .overflow_hidden()
                    .child(p.market.coin().to_string()),
            )
            .children((0..n).map(|j| {
                let v = source.get(i, j);
                let text = if correlation {
                    format!("{v:.2}")
                } else {
                    format!("{:.2}", v * 1e4)
                };
                if i == j {
                    drawn.diagonal.push(text.clone());
                }
                div()
                    .flex_1()
                    .min_w(px(52.))
                    .h(px(34.))
                    .rounded(px(3.))
                    .bg(tint(v, scale))
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .text_color(Palette::text())
                    .child(text)
            }))
    });
    let switch = TabBar::new("risk-matrix")
        .segmented()
        .xsmall()
        .selected_index(if correlation { 0 } else { 1 })
        .on_click(cx.listener(|this, ix: &usize, _, cx| {
            this.matrix = if *ix == 0 {
                MatrixView::Correlation
            } else {
                MatrixView::Covariance
            };
            cx.notify();
        }))
        .children(["Correlation", "Covariance"]);
    let swatch = |colour: Hsla, text: &'static str| {
        h_flex()
            .gap(px(6.))
            .child(div().size(px(11.)).rounded(px(2.)).bg(colour))
            .child(ui::label(text).text_size(px(12.)))
    };
    let legend = h_flex()
        .flex_wrap()
        .gap(px(16.))
        .child(swatch(Palette::gain().opacity(0.06 + MAX_TINT), "Moves together"))
        .child(swatch(Palette::loss().opacity(0.06 + MAX_TINT), "Moves against"))
        .when(report.included.iter().any(|p| p.oracle_priced()), |d| {
            d.child(
                h_flex()
                    .gap(px(6.))
                    .child(super::tag("HIP-3"))
                    .child(ui::label(
                        "Oracle-priced: its price stalls while its own market is closed, so its correlations lean toward 0.",
                    ).text_size(px(12.))),
            )
        });

    ui::panel()
        .id("risk-matrix-panel")
        .test_support()
        .flex_grow(1.)
        .flex_basis(px(640.))
        .min_w(px(0.))
        .p_4()
        .gap(px(12.))
        .child(
            h_flex()
                .flex_wrap()
                .gap_3()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(if correlation {
                            "Correlation"
                        } else {
                            "Covariance"
                        }),
                )
                .child(ui::label(note).text_size(px(12.)))
                .child(div().flex_1())
                .child(switch),
        )
        .child(v_flex().gap(px(2.)).child(head).children(rows))
        .child(legend)
        .into_any_element()
}
