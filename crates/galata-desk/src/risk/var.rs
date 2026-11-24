//! The VaR & covariance view: the measures, the matrix, each position's
//! share of VaR, and the periods side by side.

use galata_desk_core::risk::RiskPeriod;
use galata_desk_core::risk::report::{Focus, PeriodState, RiskReport};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, TestSupportExt, div, px};

use super::{Drawn, Risk, card, matrix, tag, usd, usd_signed};
use crate::theme::{MONO, Palette};
use crate::ui;

pub fn render(
    risk: &Risk,
    report: &RiskReport,
    f: &Focus,
    drawn: &mut Drawn,
    cx: &mut Context<Risk>,
) -> AnyElement {
    let conf = risk.settings.confidence.label();
    let gross = f.exposure.gross;
    let share = |v: f64| {
        if gross > 0.0 {
            format!("{:.2}% of gross", v / gross * 100.0)
        } else {
            String::new()
        }
    };
    let cards = h_flex()
        .flex_wrap()
        .gap(px(10.))
        .child(card(
            format!("Parametric VaR {conf}"),
            usd(f.parametric),
            format!("Delta-normal · {}", share(f.parametric)),
            Palette::text(),
            drawn,
        ))
        .child(card(
            format!("Cornish-Fisher VaR {conf}"),
            usd(f.cornish_fisher),
            if f.cornish_fisher_fell_back {
                "Tails too skewed for the expansion: normal quantile used".into()
            } else {
                format!(
                    "Skew {:.2} · excess kurtosis {:.2}",
                    f.skew, f.excess_kurtosis
                )
            },
            Palette::text(),
            drawn,
        ))
        .child(card(
            format!("Historical VaR {conf}"),
            usd(f.historical.var),
            format!(
                "{} returns replayed · {} in the tail",
                f.count, f.historical.count
            ),
            Palette::text(),
            drawn,
        ))
        .child(card(
            format!("Expected shortfall {conf}"),
            usd(f.historical.shortfall),
            "Mean loss at or beyond historical VaR".into(),
            Palette::text(),
            drawn,
        ));

    v_flex()
        .gap(px(16.))
        .child(cards)
        .child(
            h_flex()
                .flex_wrap()
                .items_start()
                .gap(px(16.))
                .child(matrix::render(risk, report, f, drawn, cx))
                .child(breakdown(risk, report, f, drawn)),
        )
        .child(periods(risk, report, drawn))
        .into_any_element()
}

fn breakdown(risk: &Risk, report: &RiskReport, f: &Focus, drawn: &mut Drawn) -> AnyElement {
    let largest = f
        .contributions
        .iter()
        .map(|c| c.component.abs())
        .fold(0.0, f64::max);
    drawn.var = usd(f.parametric);
    let rows = report.included.iter().zip(&f.contributions).map(|(p, c)| {
        let width = if largest > 0.0 {
            (c.component.abs() / largest) as f32
        } else {
            0.0
        };
        drawn
            .components
            .push((p.market.coin().to_string(), usd_signed(c.component)));
        let bar = |show: bool, colour| {
            div()
                .h_full()
                .w(gpui_kit::relative(if show { width } else { 0.0 }))
                .bg(colour)
        };
        h_flex()
            .h(px(30.))
            .gap(px(8.))
            .border_t_1()
            .border_color(Palette::rule())
            .child(
                h_flex()
                    .w(px(120.))
                    .gap(px(6.))
                    .flex_shrink_0()
                    .text_size(px(13.))
                    .overflow_hidden()
                    .child(p.market.coin().to_string())
                    .when(p.oracle_priced(), |d| d.child(tag("HIP-3"))),
            )
            .child(
                h_flex()
                    .flex_1()
                    .h(px(12.))
                    .child(
                        h_flex()
                            .flex_1()
                            .h_full()
                            .justify_end()
                            .border_r_1()
                            .border_color(Palette::faint())
                            .child(bar(c.component < 0.0, Palette::loss())),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .h_full()
                            .child(bar(c.component >= 0.0, Palette::gain())),
                    ),
            )
            .child(
                div()
                    .w(px(84.))
                    .text_right()
                    .font_family(MONO)
                    .text_size(px(12.5))
                    .child(usd_signed(c.component)),
            )
            .child(
                div()
                    .w(px(64.))
                    .text_right()
                    .font_family(MONO)
                    .text_size(px(12.5))
                    .text_color(Palette::muted())
                    .child(format!("{:.2}", c.marginal * 100.0)),
            )
    });
    ui::panel()
        .id("risk-breakdown")
        .test_support()
        .flex_grow(1.)
        .flex_basis(px(400.))
        .min_w(px(0.))
        .p_4()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(15.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child("VaR by position"),
        )
        .child(
            ui::label(format!(
                "Components sum to {} at {}. Negative: the position hedges the book.",
                usd(f.parametric),
                risk.settings.confidence.label()
            ))
            .text_size(px(12.)),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .text_size(px(11.5))
                .text_color(Palette::muted())
                .child(div().w(px(120.)).child("Position"))
                .child(div().flex_1().text_center().child("Contribution"))
                .child(div().w(px(84.)).text_right().child("Component"))
                .child(div().w(px(64.)).text_right().child("per $100")),
        )
        .children(rows)
        .into_any_element()
}

pub(super) fn periods(risk: &Risk, report: &RiskReport, drawn: &mut Drawn) -> AnyElement {
    let cols: [(&str, f32); 8] = [
        ("Lookback", 0.8),
        ("Candle", 0.6),
        ("Returns", 0.6),
        ("σ per candle", 1.0),
        ("To 1 day", 0.7),
        ("VaR 95% · 1D", 1.0),
        ("VaR 99% · 1D", 1.0),
        ("CF VaR 99% · 1D", 1.0),
    ];
    let cell = |ix: usize, text: String, colour| {
        div()
            .flex_grow(1.)
            .flex_basis(gpui_kit::relative(cols[ix].1 / 6.7))
            .min_w(px(0.))
            .when(ix >= 2, |d| d.text_right())
            .font_family(MONO)
            .text_size(px(13.))
            .text_color(colour)
            .child(text)
    };
    let header = h_flex()
        .gap_3()
        .h(px(30.))
        .text_size(px(11.5))
        .text_color(Palette::muted())
        .children(cols.iter().enumerate().map(|(ix, (name, w))| {
            div()
                .flex_grow(1.)
                .flex_basis(gpui_kit::relative(w / 6.7))
                .min_w(px(0.))
                .when(ix >= 2, |d| d.text_right())
                .child(*name)
        }));
    let rows = report.periods.iter().map(|row| {
        let p: RiskPeriod = row.period;
        let focused = p == risk.settings.focus;
        let scale = if !p.scaled() {
            ("native".to_string(), Palette::faint())
        } else if p.per_day() > 1.0 {
            (format!("×√{:.0}", p.per_day()), Palette::amber())
        } else {
            (format!("÷√{:.0}", 1.0 / p.per_day()), Palette::amber())
        };
        let mut texts = vec![p.label().to_string(), p.interval().as_str().to_string()];
        let figures: Vec<String> = match &row.state {
            PeriodState::Ready(fig) => vec![
                fig.count.to_string(),
                usd(fig.sigma_candle),
                scale.0.clone(),
                usd(fig.var95),
                usd(fig.var99),
                usd(fig.cornish_fisher99),
            ],
            PeriodState::Loading => vec!["…".into(); 6],
            PeriodState::TooFew { count } => {
                let mut v = vec![count.to_string()];
                v.extend([
                    "too few".to_string(),
                    "".into(),
                    "".into(),
                    "".into(),
                    "".into(),
                ]);
                v
            }
        };
        texts.extend(figures);
        drawn.periods.push(texts.join("|"));
        h_flex()
            .gap_3()
            .h(px(32.))
            .px_2()
            .rounded(px(4.))
            .border_t_1()
            .border_color(Palette::rule())
            .when(focused, |d| d.bg(Palette::raise()))
            .children(texts.into_iter().enumerate().map(|(ix, t)| {
                let colour = match ix {
                    0 if focused => Palette::amber_text(),
                    1 => Palette::muted(),
                    4 => scale.1,
                    _ => Palette::text(),
                };
                cell(ix, t, colour)
            }))
    });
    ui::panel()
        .id("risk-periods")
        .test_support()
        .p_4()
        .gap(px(6.))
        .child(
            h_flex()
                .flex_wrap()
                .gap_3()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child("Periods compared"),
                )
                .child(
                    ui::label(
                        "Each lookback uses the candles Markets loads for it. Figures from candles other than daily are scaled to 1 day by √time, which understates jumps.",
                    )
                    .text_size(px(12.)),
                ),
        )
        .child(header)
        .children(rows)
        .when(report.periods.is_empty(), |d| {
            d.child(ui::label("Tick at least one period above to compare."))
        })
        .into_any_element()
}
