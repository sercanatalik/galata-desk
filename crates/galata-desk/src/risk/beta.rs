//! The Beta & clusters view: exposure to the chosen benchmark, each
//! position in benchmark terms, and the groups that move together.

use galata_desk_core::risk::beta::LOW_R2;
use galata_desk_core::risk::cluster::{Cluster, THRESHOLDS};
use galata_desk_core::risk::report::{Focus, RiskReport};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Hsla, TestSupportExt, div, px};

use super::{Drawn, Risk, card, tag, units, usd, usd_signed};
use crate::theme::{MONO, Palette};
use crate::ui;

fn sign_colour(v: f64) -> Hsla {
    ui::tone_of(if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    })
}

pub fn render(
    risk: &Risk,
    report: &RiskReport,
    f: &Focus,
    drawn: &mut Drawn,
    cx: &mut Context<Risk>,
) -> AnyElement {
    let bench = &report.included[f.benchmark];
    let b = &bench.market;
    let e = &f.exposure;

    let pick = TabBar::new("risk-benchmark")
        .segmented()
        .xsmall()
        .selected_index(f.benchmark)
        .on_click({
            let markets: Vec<_> = report.included.iter().map(|p| p.market.clone()).collect();
            cx.listener(move |this, ix: &usize, _, cx| {
                if let Some(m) = markets.get(*ix) {
                    this.set_benchmark(m.clone(), cx);
                }
            })
        })
        .children(
            report
                .included
                .iter()
                .map(|p| p.market.ticker().to_string()),
        );
    let threshold = TabBar::new("risk-threshold")
        .segmented()
        .xsmall()
        .selected_index(
            THRESHOLDS
                .iter()
                .position(|t| (*t - risk.settings.threshold).abs() < 1e-9)
                .unwrap_or(1),
        )
        .on_click(cx.listener(|this, ix: &usize, _, cx| {
            this.settings.threshold = THRESHOLDS[*ix];
            cx.notify();
        }))
        .children(THRESHOLDS.map(|t| format!("ρ ≥ {t:.1}")));
    let choices = h_flex()
        .flex_wrap()
        .gap(px(24.))
        .child(
            v_flex()
                .gap(px(6.))
                .child(ui::label("Benchmark").text_size(px(11.5)))
                .child(pick),
        )
        .child(
            v_flex()
                .gap(px(6.))
                .child(ui::label("Cluster when").text_size(px(11.5)))
                .child(threshold),
        );

    let cards = h_flex()
        .flex_wrap()
        .gap(px(10.))
        .child(card(
            "Gross exposure".into(),
            usd(e.gross),
            "Σ |position|".into(),
            Palette::text(),
            drawn,
        ))
        .child(card(
            "Net exposure".into(),
            usd_signed(e.net),
            "Σ position".into(),
            sign_colour(e.net),
            drawn,
        ))
        .child(card(
            format!("β-adjusted gross vs {}", b.ticker()),
            usd(e.beta_gross),
            units(e.beta_gross_units, b)
                .trim_start_matches('+')
                .to_string(),
            Palette::text(),
            drawn,
        ))
        .child(card(
            format!("β-adjusted net vs {}", b.ticker()),
            usd_signed(e.beta_net),
            units(e.beta_net_units, b),
            sign_colour(e.beta_net),
            drawn,
        ))
        .child(card(
            "Hedge to β-neutral".into(),
            units(e.hedge_units(), b),
            format!(
                "{} {} of {}",
                if e.beta_net > 0.0 { "Sell" } else { "Buy" },
                usd(e.beta_net.abs()),
                b.ticker()
            ),
            Palette::amber(),
            drawn,
        ));

    v_flex()
        .gap(px(16.))
        .child(choices)
        .child(cards)
        .child(table(report, f, drawn))
        .child(clusters(risk, report, f, drawn))
        .into_any_element()
}

fn table(report: &RiskReport, f: &Focus, drawn: &mut Drawn) -> AnyElement {
    let b = &report.included[f.benchmark].market;
    let cols: [(&str, f32); 9] = [
        ("Position", 1.5),
        ("Notional", 1.0),
        ("β", 0.6),
        ("R²", 1.0),
        ("Equiv $", 1.0),
        ("Equiv units", 1.1),
        ("Hedge", 1.1),
        ("Systematic σ", 1.0),
        ("Residual σ", 1.0),
    ];
    let total: f32 = cols.iter().map(|c| c.1).sum();
    let col = move |ix: usize| {
        div()
            .flex_grow(1.)
            .flex_basis(gpui_kit::relative(cols[ix].1 / total))
            .min_w(px(0.))
            .when(ix >= 1 && ix != 3, |d| d.text_right())
    };
    let header = h_flex()
        .gap_3()
        .h(px(30.))
        .px_2()
        .text_size(px(11.5))
        .text_color(Palette::muted())
        .children(
            cols.iter()
                .enumerate()
                .map(|(ix, (n, _))| col(ix).child(*n)),
        );
    let mono = |text: String, colour: Hsla| {
        div()
            .font_family(MONO)
            .text_size(px(13.))
            .text_color(colour)
            .child(text)
    };
    let rows = report
        .included
        .iter()
        .zip(&f.betas)
        .enumerate()
        .map(|(i, (p, r))| {
            let low = r.low_r2();
            let coin = p.market.coin().to_string();
            if low {
                drawn.low_r2.push(coin.clone());
            }
            drawn.betas.push(format!(
                "{coin}|{:.2}|{:.2}|{}",
                r.beta,
                r.r2,
                usd_signed(r.equivalent)
            ));
            let r2_colour = if low {
                Palette::amber()
            } else {
                Palette::text()
            };
            h_flex()
                .gap_3()
                .h(px(34.))
                .px_2()
                .rounded(px(4.))
                .border_t_1()
                .border_color(Palette::rule())
                .when(i == f.benchmark, |d| d.bg(Palette::raise()))
                .child(
                    col(0).child(
                        h_flex()
                            .gap(px(6.))
                            .text_size(px(13.))
                            .child(coin.clone())
                            .when(p.oracle_priced(), |d| d.child(tag("HIP-3"))),
                    ),
                )
                .child(col(1).child(mono(usd_signed(p.notional), sign_colour(p.notional))))
                .child(col(2).child(mono(format!("{:.2}", r.beta), Palette::text())))
                .child(
                    col(3).child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(6.))
                                    .rounded(px(3.))
                                    .bg(Palette::line())
                                    .child(
                                        div()
                                            .h_full()
                                            .rounded(px(3.))
                                            .w(gpui_kit::relative(r.r2.clamp(0.0, 1.0) as f32))
                                            .bg(r2_colour),
                                    ),
                            )
                            .child(mono(format!("{:.2}", r.r2), r2_colour)),
                    ),
                )
                .child(col(4).child(mono(usd_signed(r.equivalent), sign_colour(r.equivalent))))
                .child(col(5).child(mono(units(r.equivalent_units, b), Palette::text())))
                .child(col(6).child(mono(
                    if i == f.benchmark {
                        "—".into()
                    } else {
                        units(r.hedge_units(), b)
                    },
                    Palette::muted(),
                )))
                .child(col(7).child(mono(usd(r.systematic), Palette::text())))
                .child(col(8).child(mono(
                    usd(r.residual),
                    if low {
                        Palette::amber()
                    } else {
                        Palette::text()
                    },
                )))
        });
    let e = &f.exposure;
    let net = h_flex()
        .gap_3()
        .h(px(36.))
        .px_2()
        .border_t_1()
        .border_color(Palette::faint())
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .child(col(0).text_size(px(13.)).child("Net"))
        .child(col(1).child(mono(usd_signed(e.net), Palette::text())))
        .child(col(2))
        .child(col(3))
        .child(col(4).child(mono(usd_signed(e.beta_net), Palette::text())))
        .child(col(5).child(mono(units(e.beta_net_units, b), Palette::text())))
        .child(col(6).child(mono(units(e.hedge_units(), b), Palette::amber_text())))
        .child(col(7))
        .child(col(8));

    ui::panel()
        .id("risk-betas")
        .test_support()
        .p_4()
        .gap(px(4.))
        .child(
            h_flex()
                .flex_wrap()
                .gap_3()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(format!("Positions in {} terms", b.ticker())),
                )
                .child(
                    ui::label(format!(
                        "β = cov(position, {t}) ÷ var({t}) · {} · residual σ is what a {t} hedge leaves",
                        f.period.label(),
                        t = b.ticker()
                    ))
                    .text_size(px(12.)),
                ),
        )
        .child(header)
        .children(rows)
        .child(net)
        .child(
            ui::label(format!(
                "R² below {LOW_R2:.2} is amber: the position barely moves with {}, so a small equivalent does not mean small risk. Read its residual σ.",
                b.ticker()
            ))
            .text_size(px(12.)),
        )
        .into_any_element()
}

fn clusters(risk: &Risk, report: &RiskReport, f: &Focus, drawn: &mut Drawn) -> AnyElement {
    let b = &report.included[f.benchmark].market;
    let card = |name: String, c: &Cluster, drawn: &mut Drawn| {
        let coins: Vec<String> = c
            .members
            .iter()
            .map(|&i| report.included[i].market.coin().to_string())
            .collect();
        drawn.clusters.push(coins.join("+"));
        let figure = |label: &'static str, value: String| {
            v_flex()
                .flex_1()
                .gap(px(2.))
                .child(ui::label(label).text_size(px(11.5)))
                .child(ui::figure(value, px(15.)))
        };
        ui::panel()
            .flex_grow(1.)
            .flex_basis(px(320.))
            .min_w(px(280.))
            .p_4()
            .gap(px(10.))
            .child(
                h_flex()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(div().flex_1())
                    .child(ui::label(match c.mean_correlation {
                        Some(r) => format!("mean ρ {r:.2}"),
                        None => format!("ρ < {:.1} with the rest", risk.settings.threshold),
                    })),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .children(c.members.iter().map(|&i| {
                        let p = &report.included[i];
                        div()
                            .px(px(8.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .bg(Palette::raise())
                            .border_1()
                            .border_color(Palette::line())
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(sign_colour(p.notional))
                            .child(format!("{} {}", p.market.ticker(), usd_signed(p.notional)))
                    })),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(figure("Gross", usd(c.gross)))
                    .child(figure("Net", usd_signed(c.net)))
                    .child(figure("β-adj net", usd_signed(c.beta_net))),
            )
            .child(ui::label(format!(
                "β-adjusted net ≈ {}",
                units(
                    c.beta_net / report.included[f.benchmark].mark.max(f64::MIN_POSITIVE),
                    b
                )
            )))
    };
    let mut cards: Vec<AnyElement> = f
        .clusters
        .iter()
        .enumerate()
        .map(|(ix, c)| {
            let name = format!(
                "Cluster {} · {} positions",
                (b'A' + ix as u8) as char,
                c.members.len()
            );
            card(name, c, drawn).into_any_element()
        })
        .collect();
    if let Some(rest) = &f.uncorrelated {
        cards.push(
            card(
                format!("Uncorrelated · {}", rest.members.len()),
                rest,
                drawn,
            )
            .into_any_element(),
        );
    }
    v_flex()
        .id("risk-clusters")
        .test_support()
        .gap(px(10.))
        .child(
            h_flex()
                .flex_wrap()
                .gap_3()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child("Correlated clusters"),
                )
                .child(
                    ui::label(format!(
                        "Positions linked by ρ ≥ {:.1} over {} move as one. Gross is how big that one position is.",
                        risk.settings.threshold,
                        f.period.label()
                    ))
                    .text_size(px(12.)),
                ),
        )
        .child(h_flex().flex_wrap().gap(px(10.)).children(cards))
        .into_any_element()
}
