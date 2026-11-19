//! What each watched market is: its company, kind, listing, deployer and
//! oracle, and where the deployer documents it. Loaded once per session:
//! none of it changes while the app is open.

use std::collections::{HashMap, HashSet};

use galata_desk_core::format as f;
use galata_desk_core::market::MarketId;
use galata_desk_core::market::about::{self, About};
use galata_desk_core::venue::DexInfo;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, div, px};

use crate::theme::{MONO, Palette};

#[derive(Default)]
pub struct AboutBook {
    about: HashMap<MarketId, About>,
    /// Asked for and not failed: not asked again.
    pub asked: HashSet<MarketId>,
    /// Every builder dex by short name, once loaded.
    dexes: Option<HashMap<String, DexInfo>>,
    pub dexes_asked: bool,
    /// A screenshot run opens the selected market's card.
    pub open_at_start: bool,
}

impl AboutBook {
    pub fn get(&self, market: &MarketId) -> Option<&About> {
        self.about.get(market)
    }

    pub fn put(&mut self, market: MarketId, about: About) {
        self.about.insert(market, about);
    }

    pub fn put_dexes(&mut self, dexes: Vec<DexInfo>) {
        self.dexes = Some(dexes.into_iter().map(|d| (d.name.clone(), d)).collect());
    }

    fn dex(&self, dex: &str) -> Option<&DexInfo> {
        self.dexes.as_ref()?.get(dex)
    }

    /// The info button beside the market's name, and the card it opens.
    pub fn button(&self, market: &MarketId) -> AnyElement {
        let card = Card::of(market, self.get(market), self.dex(market.dex()));
        Popover::new(("market-about", hash(market)))
            .anchor(gpui_kit::Anchor::TopLeft)
            .default_open(self.open_at_start)
            .trigger(
                Button::new(("market-about-button", hash(market)))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Info))
                    .tooltip("About this market"),
            )
            .content(move |_, _, _| card.render())
            .into_any_element()
    }
}

fn hash(market: &MarketId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    market.coin().hash(&mut h);
    h.finish()
}

/// Everything the card shows, copied out so the card can be drawn again
/// without the view.
#[derive(Clone)]
struct Card {
    title: String,
    kind: String,
    description: Option<String>,
    facts: Vec<(&'static str, String)>,
    links: Vec<about::DocLink>,
}

impl Card {
    fn of(market: &MarketId, about: Option<&About>, dex: Option<&DexInfo>) -> Self {
        let coin = market.coin();
        let title = about
            .and_then(|a| a.name.clone())
            .unwrap_or_else(|| market.ticker().to_string());
        let kind = [
            about.and_then(|a| a.category.clone()),
            about.and_then(|a| a.listing.clone()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" \u{b7} ");

        let mut facts: Vec<(&'static str, String)> = Vec::new();
        if let Some(display) = about.and_then(|a| a.display_name.clone()) {
            facts.push(("Shown on the venue as", display));
        }
        if market.dex().is_empty() {
            facts.push(("Deployed by", "Hyperliquid (main dex)".into()));
            facts.push(("Oracle", "Set by the validators".into()));
        } else if let Some(d) = dex {
            let who = if d.full_name.is_empty() {
                d.name.clone()
            } else if d.full_name.eq_ignore_ascii_case(&d.name) {
                d.full_name.clone()
            } else {
                format!("{} ({})", d.full_name, d.name)
            };
            facts.push(("Deployed by", who));
            if let Some(deployer) = &d.deployer {
                facts.push(("Deployer address", f::short_address(deployer)));
            }
            let setters = d.oracle_setters();
            facts.push((
                "Oracle set by",
                if setters.is_empty() {
                    "The deployer".into()
                } else {
                    setters
                        .iter()
                        .map(|s| f::short_address(s))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            ));
            if let Some(cap) = d.oi_cap(coin) {
                facts.push(("Open interest cap", f::usd_abbrev(cap)));
            }
            if let Some(m) = d.funding_multiplier(coin) {
                facts.push(("Funding multiplier", format!("{}\u{d7}", m.normalize())));
            }
        }
        let pre_ipo = about.and_then(|a| a.category.as_deref()) == Some("Pre-IPO");
        Card {
            title,
            kind,
            description: about.and_then(|a| a.description.clone()),
            facts,
            links: about::docs(market.dex(), pre_ipo),
        }
    }

    fn render(&self) -> AnyElement {
        v_flex()
            .w(px(360.))
            .gap_2()
            .text_size(px(12.5))
            .child(
                v_flex()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(self.title.clone()),
                    )
                    .when(!self.kind.is_empty(), |d| {
                        d.child(div().text_color(Palette::muted()).child(self.kind.clone()))
                    }),
            )
            .children(self.description.clone().map(|d| {
                div()
                    .text_color(Palette::muted())
                    .line_height(px(17.))
                    .child(d)
            }))
            .child(
                v_flex()
                    .gap(px(3.))
                    .pt_2()
                    .border_t_1()
                    .border_color(Palette::line())
                    .children(self.facts.iter().map(|(name, value)| {
                        h_flex()
                            .justify_between()
                            .gap_3()
                            .child(div().text_color(Palette::muted()).child(*name))
                            .child(
                                div()
                                    .font_family(MONO)
                                    .text_size(px(12.))
                                    .text_right()
                                    .child(value.clone()),
                            )
                    })),
            )
            .when(!self.links.is_empty(), |d| {
                d.child(
                    v_flex()
                        .pt_2()
                        .border_t_1()
                        .border_color(Palette::line())
                        .items_start()
                        .children(self.links.iter().enumerate().map(|(ix, l)| {
                            let url = l.url;
                            Button::new(("market-doc", ix))
                                .ghost()
                                .small()
                                .icon(Icon::new(IconName::ExternalLink))
                                .label(l.label)
                                .tooltip(url)
                                .on_click(move |_, _, cx| cx.open_url(url))
                        })),
                )
            })
            .into_any_element()
    }
}
