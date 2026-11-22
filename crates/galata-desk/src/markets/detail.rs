//! What the venue says about one market now, with Buy and Sell under it.
//! Nothing of the account's: that is the position panel's.

use galata_desk_core::format as f;
use galata_desk_core::market::MarketId;
use galata_desk_core::market::feed::{Live, Tile};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, Subscription, TestSupportExt, Window, div, px};

use super::dock::{DockMeta, PanelKind, PanelRecord, dock_panel, market_name};
use super::model::{MarketSource, MarketsModel};
use super::{DETAIL_WIDTH, dex_tag, sign};
use crate::portfolio::Portfolio;
use crate::theme::{MONO, Palette};
use crate::ui;

pub struct DetailPanel {
    model: Entity<MarketsModel>,
    portfolio: Entity<Portfolio>,
    source: MarketSource,
    /// Its key and links in the workspace.
    dock: DockMeta,
    _subscriptions: Vec<Subscription>,
}

dock_panel!(DetailPanel, PanelKind::Detail);

impl DetailPanel {
    pub fn new(model: Entity<MarketsModel>, source: MarketSource, cx: &mut Context<Self>) -> Self {
        let portfolio = model.read(cx).portfolio.clone();
        let subscriptions = vec![cx.observe(&model, |_, _, cx| cx.notify())];
        Self {
            model,
            portfolio,
            source,
            dock: DockMeta::fresh(cx),
            _subscriptions: subscriptions,
        }
    }

    /// Apply a linked panel's event: a listening panel shows the channel's
    /// market. This never publishes.
    pub fn apply(
        &mut self,
        event: &super::links::LinkEvent,
        _: &mut gpui_kit::Window,
        cx: &mut gpui_kit::Context<Self>,
    ) {
        if let super::links::LinkEvent::SetMarket(market) = event {
            self.set_market(Some(market.clone()), cx);
        }
    }

    /// The same panel, with a saved panel's key and links.
    pub fn with_dock(mut self, dock: DockMeta) -> Self {
        self.dock = dock;
        self
    }

    /// What the panel saves in a layout.
    pub fn record(&self, cx: &gpui_kit::App) -> PanelRecord {
        self.dock
            .record(PanelKind::Detail, self.market(cx).as_ref(), None, cx)
    }

    /// The panel's title: the market it describes.
    pub fn title_text(&self, cx: &gpui_kit::App) -> String {
        match self.market(cx) {
            Some(market) => market_name(&market),
            None => "Market detail".to_string(),
        }
    }

    /// The market the detail describes.
    pub fn market(&self, cx: &gpui_kit::App) -> Option<MarketId> {
        self.source.market(&self.model, cx)
    }

    /// Show `market`, for a panel that keeps its own.
    #[allow(dead_code, reason = "the workspace's detail panels use it")]
    pub fn set_market(&mut self, market: Option<MarketId>, cx: &mut Context<Self>) {
        if self.source.set(market) {
            cx.notify();
        }
    }

    /// Buy / Long and Sell / Short for the selected market: each opens the
    /// order ticket on that side. Trading's guards are explained in the
    /// ticket, so these are not disabled by them.
    fn trade_buttons(&self, market: &MarketId, cx: &mut Context<Self>) -> AnyElement {
        let side = |id: &'static str, label: &'static str, buy: bool, cx: &mut Context<Self>| {
            // The button's colour paints as a tint, with its words in the
            // full colour: blue for buying, orange for selling.
            let ground = if buy {
                Palette::gain()
            } else {
                Palette::loss()
            };
            let hover = ground.opacity(0.3);
            let market = market.clone();
            let name = format!("{} {}", if buy { "Buy" } else { "Sell" }, market.ticker());
            Button::new(id)
                .flex_1()
                .label(label)
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(ground)
                        .foreground(ground)
                        .hover(hover)
                        .active(hover),
                )
                .accessibility_label(name)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let market = market.clone();
                    this.portfolio
                        .update(cx, |portfolio, cx| portfolio.trade(market, buy, window, cx));
                }))
        };
        h_flex()
            .mt_auto()
            .pt_2()
            .gap_2()
            .child(side("trade-buy", "Buy / Long", true, cx))
            .child(side("trade-sell", "Sell / Short", false, cx))
            .into_any_element()
    }

    /// The panel's left side: everything the venue says about the market now,
    /// with the Buy and Sell buttons under it. Nothing of the account's: that
    /// is the position column's.
    fn detail(&self, tile: &Tile, trade: Option<AnyElement>, cx: &gpui_kit::App) -> AnyElement {
        let market = &tile.market;
        let model = self.model.read(cx);
        let pending = |w: f32, h: f32| ui::placeholder(px(w), px(h)).into_any_element();
        let row = |name: &str, value: AnyElement| {
            h_flex()
                .justify_between()
                .text_size(px(12.5))
                .child(div().text_color(Palette::muted()).child(name.to_string()))
                .child(value)
        };
        let figure = |text: String, tone: gpui_kit::Hsla| {
            div()
                .font_family(MONO)
                .text_color(tone)
                .child(text)
                .into_any_element()
        };
        let plain = Palette::text();
        let live: Option<&Live> = tile.live.as_ref();
        let stats = tile.stats.as_ref();
        // A live figure, or a placeholder until the stream has spoken.
        let from_live = |show: &dyn Fn(&Live) -> (String, gpui_kit::Hsla)| match live {
            Some(l) => {
                let (text, tone) = show(l);
                figure(text, tone)
            }
            None => pending(70., 13.),
        };
        // A figure that needs history, or a placeholder until it has loaded.
        let from_stats =
            |show: &dyn Fn(&galata_desk_core::market::candles::Stats) -> String| match stats {
                Some(s) => figure(show(s), plain),
                None => pending(70., 13.),
            };
        let funding_tone = |l: &Live| ui::tone_of(sign(l.funding));

        // Docked, it fills its place, down to its windowed width; the dock
        // draws the dividers. Windowed, it is a column beside the chart.
        let docked = self.dock.area.is_some();
        v_flex()
            .id("market-detail")
            .map(|d| {
                if docked {
                    d.w_full().min_w(px(DETAIL_WIDTH))
                } else {
                    d.w(px(DETAIL_WIDTH))
                        .flex_shrink_0()
                        .border_r_1()
                        .border_color(Palette::rule())
                }
            })
            .h_full()
            .px_4()
            .py_3()
            .gap(px(3.))
            .child(
                h_flex()
                    .gap(px(7.))
                    .child(
                        div()
                            .text_size(px(17.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(market.ticker().to_string()),
                    )
                    .when(!market.dex().is_empty(), |d| d.child(dex_tag(market.dex())))
                    .child(model.about.button(market))
                    .child(div().flex_1())
                    .children(tile.max_leverage.map(|l| {
                        div()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(Palette::muted())
                            .child(format!("up to {l}x"))
                    })),
            )
            .children(model.about.get(market).and_then(|a| {
                let line = [a.name.clone(), a.category.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" \u{b7} ");
                (!line.is_empty()).then(|| {
                    div()
                        .id("market-about-line")
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(Palette::muted())
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .aria_label(line.clone())
                        .child(line)
                        .test_support()
                })
            }))
            .child(
                h_flex()
                    .gap_3()
                    .items_baseline()
                    .pb_1()
                    .child(match live {
                        Some(l) => ui::figure(f::price(l.last), px(26.)).into_any_element(),
                        None => pending(130., 28.),
                    })
                    .child(match stats {
                        Some(s) => div()
                            .font_family(MONO)
                            .text_size(px(13.5))
                            .text_color(ui::tone_of(if s.change > 0.0 {
                                1
                            } else if s.change < 0.0 {
                                -1
                            } else {
                                0
                            }))
                            .child(f::pct_signed_f(s.change, 2))
                            .into_any_element(),
                        None => pending(56., 14.),
                    }),
            )
            .child(row("Mark", from_live(&|l| (f::price(l.mark), plain))))
            .child(row("Oracle", from_live(&|l| (f::price(l.oracle), plain))))
            .child(row(
                "Premium",
                from_live(&|l| (f::premium(l.premium), plain)),
            ))
            .child(row(
                "Funding (1h)",
                from_live(&|l| (f::funding_hourly(l.funding), funding_tone(l))),
            ))
            .child(row(
                "Funding (yearly)",
                from_live(&|l| (f::funding_yearly(l.funding), funding_tone(l))),
            ))
            .child(row(
                "24h volume",
                from_live(&|l| (f::usd_abbrev(l.volume_usd), plain)),
            ))
            .child(row(
                "Open interest",
                from_live(&|l| (f::usd_abbrev(l.open_interest_usd), plain)),
            ))
            .child(row(
                &format!("{} low / high", model.period.label()),
                from_stats(&|s| format!("{} / {}", f::price(s.low), f::price(s.high))),
            ))
            .child(row(
                "Volatility",
                from_stats(&|s| f::volatility(s.volatility)),
            ))
            .children(trade)
            .test_support()
            .into_any_element()
    }
}

impl Render for DetailPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(market) = self.market(cx) else {
            return div().into_any_element();
        };
        let tile = self.model.read(cx).tile_of(&market);
        let trade = (!tile.unavailable).then(|| self.trade_buttons(&market, cx));
        self.detail(&tile, trade, cx)
    }
}
