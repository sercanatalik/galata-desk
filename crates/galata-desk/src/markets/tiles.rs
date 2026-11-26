//! The watched markets as heatmap tiles. Clicking a tile, or pressing Enter
//! or Space on it, selects its market: the panel tells its holder at once,
//! through the handler it was made with, and the holder decides what that
//! shows.

use galata_desk_core::format as f;
use galata_desk_core::market::MarketId;
use galata_desk_core::market::feed::Tile;
use galata_desk_core::market::pnl::{MarketPnl, WindowPnl};
use galata_desk_core::market::sort::SortBy;
use galata_desk_core::rust_decimal::Decimal;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, FocusHandle, KeyDownEvent, Pixels, Subscription, TestSupportExt,
    Window, anchored, deferred, div, px,
};
use std::collections::HashMap;

use super::dock::{DockMeta, PanelKind, PanelRecord, dock_panel};
use super::model::{Account, MarketsModel};
use super::{DASH, GAP, SOFT, TILE_HEIGHT, TILE_MIN, TileBounds, dex_tag, range_bar};
use crate::market_tile;
use crate::theme::{MONO, Palette};
use crate::ui;

/// What the tiles tell whoever holds them. Told synchronously, from inside
/// the click, so the selection has moved by the time the click returns.
pub enum TilesEvent {
    /// The user clicked a tile, or pressed Enter or Space on it.
    Selected(MarketId),
    /// The user asked to add a market while nothing is watched.
    AddMarket,
}

/// Which tile is outlined.
pub enum Outline {
    /// The watchlist's selection: the windowed layout, where the market panel
    /// above shows it.
    Watchlist,
    /// The panel's own: in the workspace, where it is what the panel last sent.
    #[allow(dead_code, reason = "the workspace's tiles panel uses it")]
    Own(Option<MarketId>),
}

pub struct TilesPanel {
    model: Entity<MarketsModel>,
    outline: Outline,
    /// The tile the pointer rests on. It shows that tile's remove button and
    /// nothing else: only a click selects.
    hovered: Option<MarketId>,
    /// One focus handle per tile, so the keyboard can reach each.
    focus: HashMap<MarketId, FocusHandle>,
    /// The "Alert for <market>…" form, open under that market's tile.
    tile_form: Option<(MarketId, Entity<crate::level_form::LevelForm>, Subscription)>,
    /// Where each tile was last drawn, in window coordinates, so the alert
    /// form can open above its tile when there is no room below.
    tile_bounds: TileBounds,
    /// Told of each selection, and of a request to add a market.
    on_event: TilesHandler,
    /// How wide the panel was last drawn, which decides how many columns fit.
    width: std::rc::Rc<std::cell::Cell<Option<Pixels>>>,
    /// Its key and links in the workspace.
    dock: DockMeta,
    _subscriptions: Vec<Subscription>,
}

dock_panel!(TilesPanel, PanelKind::Tiles);

/// What the holder runs for each [`TilesEvent`].
pub type TilesHandler = std::rc::Rc<dyn Fn(&TilesEvent, &mut Window, &mut gpui_kit::App)>;

impl TilesPanel {
    pub fn new(
        model: Entity<MarketsModel>,
        outline: Outline,
        on_event: TilesHandler,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![cx.observe(&model, |_, _, cx| cx.notify())];
        Self {
            model,
            outline,
            hovered: None,
            focus: HashMap::new(),
            tile_form: None,
            tile_bounds: TileBounds::default(),
            on_event,
            width: Default::default(),
            dock: DockMeta::fresh(cx),
            _subscriptions: subscriptions,
        }
    }

    /// Apply a linked panel's event: a listening tiles panel outlines the
    /// channel's market. This never publishes.
    pub fn apply(
        &mut self,
        event: &super::links::LinkEvent,
        _: &mut gpui_kit::Window,
        cx: &mut gpui_kit::Context<Self>,
    ) {
        if let super::links::LinkEvent::SetMarket(market) = event {
            self.set_outlined(Some(market.clone()), cx);
        }
    }

    /// The same panel, with a saved panel's key and links.
    pub fn with_dock(mut self, dock: DockMeta) -> Self {
        self.dock = dock;
        self
    }

    /// What the panel saves in a layout: the tile it outlines.
    pub fn record(&self, cx: &gpui_kit::App) -> PanelRecord {
        let outlined = match &self.outline {
            Outline::Own(market) => market.clone(),
            Outline::Watchlist => None,
        };
        self.dock
            .record(PanelKind::Tiles, outlined.as_ref(), None, cx)
    }

    /// The panel's title.
    pub fn title_text(&self, _: &gpui_kit::App) -> String {
        "Watchlist".to_string()
    }

    /// The market whose tile is outlined.
    pub fn outlined(&self, cx: &gpui_kit::App) -> Option<MarketId> {
        match &self.outline {
            Outline::Watchlist => self.model.read(cx).selected().cloned(),
            Outline::Own(market) => market.clone(),
        }
    }

    /// Outline `market`'s tile, for a panel that keeps its own outline.
    #[allow(dead_code, reason = "the workspace's tiles panel uses it")]
    pub fn set_outlined(&mut self, market: Option<MarketId>, cx: &mut Context<Self>) {
        if let Outline::Own(own) = &mut self.outline {
            *own = market;
            cx.notify();
        }
    }

    /// Only a click, or Enter or Space on a focused tile, comes here;
    /// pointing at a tile does not.
    /// Select `market`'s tile, as a click on it does, for tests.
    #[cfg(test)]
    pub fn select_for_test(
        &mut self,
        market: &MarketId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(market, window, cx);
    }

    fn select(&mut self, market: &MarketId, window: &mut Window, cx: &mut Context<Self>) {
        if let Outline::Own(own) = &mut self.outline {
            *own = Some(market.clone());
        }
        (self.on_event)(&TilesEvent::Selected(market.clone()), window, cx);
        // In the workspace, the channels it sends on show the market, and
        // linked blotters may show only its rows.
        self.dock
            .publish(super::links::LinkEvent::SetMarket(market.clone()), cx);
        self.dock.publish(
            super::links::LinkEvent::FilterBlotter(Some(market.clone())),
            cx,
        );
        cx.notify();
    }

    fn remove(&mut self, market: &MarketId, cx: &mut Context<Self>) {
        if self.model.update(cx, |m, cx| m.remove(market, cx)) {
            self.focus.remove(market);
            if self.hovered.as_ref() == Some(market) {
                self.hovered = None;
            }
            if let Outline::Own(own) = &mut self.outline
                && own.as_ref() == Some(market)
            {
                *own = None;
            }
        }
        cx.notify();
    }

    /// Open the "Alert for <market>…" form under `market`'s tile, its price
    /// set to `last`. It closes when the level is saved or the user cancels.
    pub fn open_tile_form(
        &mut self,
        market: MarketId,
        last: Option<Decimal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = self.model.read(cx).watcher.clone() else {
            return;
        };
        let coin = market.coin().to_string();
        let form = cx.new(|cx| {
            crate::level_form::LevelForm::for_market("tile-alert", link, coin, last, window, cx)
        });
        let closing = cx.subscribe(
            &form,
            |this, _, _: &crate::level_form::LevelFormEvent, cx| {
                this.tile_form = None;
                cx.notify();
            },
        );
        form.update(cx, |f, cx| f.focus_price(window, cx));
        self.tile_form = Some((market, form, closing));
        cx.notify();
    }

    /// Close the tile's alert form without saving.
    pub fn close_tile_form(&mut self, cx: &mut Context<Self>) {
        if self.tile_form.take().is_some() {
            cx.notify();
        }
    }

    /// The tile the pointer rests on, which shows its remove button.
    #[cfg(test)]
    pub fn hovered_market(&self) -> Option<String> {
        self.hovered.as_ref().map(|m| m.to_string())
    }

    /// Put the keyboard focus on a market's tile.
    #[cfg(test)]
    pub fn focus_market(&self, market: &str, window: &mut Window, cx: &mut Context<Self>) {
        let handle = &self.focus[&MarketId::new(market)];
        window.focus(handle, cx);
        cx.notify();
    }

    /// The "Alert for <market>…" form, while it is open.
    #[cfg(test)]
    pub fn tile_form(&self) -> Option<Entity<crate::level_form::LevelForm>> {
        self.tile_form.as_ref().map(|(_, f, _)| f.clone())
    }

    /// One market's tile. Clicking it, or pressing Enter or Space while it
    /// has focus, selects its market.
    fn tile(
        &mut self,
        ix: usize,
        tile: &Tile,
        account: &Account,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let market = tile.market.clone();
        let handle = self
            .focus
            .entry(market.clone())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        let focused = handle.is_focused(window);
        let selected = self.outlined(cx).as_ref() == Some(&market);
        let (sort_now, colour_now, period_now, headline, can_call) = {
            let m = self.model.read(cx);
            (
                m.sort,
                m.colour,
                m.period,
                m.about
                    .get(&market)
                    .and_then(|a| a.headline())
                    .map(str::to_string),
                m.calls_in(&market, cx).1,
            )
        };
        // Pointing at a tile, or focusing it, shows its remove button.
        let reachable = focused || self.hovered.as_ref() == Some(&market);
        let position = account.position(&market);
        let held = position.flatten().is_some();
        let by_funding = sort_now == SortBy::Funding;

        let header = h_flex()
            .gap(px(6.))
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(market.ticker().to_string()),
            )
            .when(!market.dex().is_empty(), |d| d.child(dex_tag(market.dex())))
            .when(held, |d| {
                d.child(div().size(px(7.)).rounded_full().bg(Palette::amber()))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(11.))
                    .text_color(gpui_kit::rgb(SOFT))
                    .children(headline.clone()),
            );

        let soft = gpui_kit::rgb(SOFT);
        let mut name = market.ticker().to_string();
        if let Some(h) = &headline {
            name.push_str(&format!(", {h}"));
        }
        let body: AnyElement = if tile.unavailable {
            v_flex()
                .flex_1()
                .justify_between()
                .child(header)
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(Palette::muted())
                        .child("Unavailable"),
                )
                .child(h_flex().child(remove_labelled(ix, &market, cx)))
                .into_any_element()
        } else {
            let pending = |w: f32, h: f32| ui::placeholder(px(w), px(h)).into_any_element();
            let stats = tile.stats.as_ref();
            let last = tile.live.as_ref().map(|l| l.last);
            let placement = stats.zip(last).map(|(s, last)| s.placement(last));
            let pnl = account.pnl(&market);
            let position_text = position.map(|p| match p {
                Some(p) => f::usd_k_signed(p.signed_value()),
                None => DASH.to_string(),
            });
            let window_text = |w: fn(&MarketPnl) -> Option<WindowPnl>| {
                pnl.as_ref().map(|p| match w(p) {
                    Some(w) if w.partial => format!("{}*", f::pnl_rounded(w.usd)),
                    Some(w) => f::pnl_rounded(w.usd),
                    None => DASH.to_string(),
                })
            };
            let d30 = window_text(|p| p.d30);
            let ytd = window_text(|p| p.ytd);
            let all = window_text(|p| p.all);

            // What a screen reader says for the tile.
            if let Some(live) = &tile.live {
                name.push_str(&format!(", last {}", f::price(live.last)));
            }
            if let Some(s) = stats {
                name.push_str(&format!(", {}", f::pct_signed_f(s.change, 2)));
            }
            if let Some(z) = placement.and_then(|p| p.z) {
                name.push_str(&format!(
                    ", {} standard deviations from the period average",
                    f::sigma(z).trim_end_matches('\u{3c3}')
                ));
            }
            let spoken = |t: &Option<String>| match t.as_deref() {
                Some(DASH) => "none".to_string(),
                Some(t) => t.trim_end_matches('*').to_string(),
                None => "loading".to_string(),
            };
            name.push_str(&format!(
                ", position {}, 30-day PNL {}, year-to-date PNL {}, all-time PNL {}",
                spoken(&position_text),
                spoken(&d30),
                spoken(&ytd),
                spoken(&all),
            ));

            let cell = |id: &'static str, label: &'static str, value: Option<String>| {
                h_flex()
                    .justify_between()
                    .gap_1()
                    .child(div().text_size(px(10.)).text_color(soft).child(label))
                    .child(match value {
                        Some(text) => div()
                            .id((id, ix))
                            .aria_label(text.clone())
                            .font_family(MONO)
                            .text_size(px(11.))
                            .when(text == DASH, |d| d.text_color(soft))
                            .child(text)
                            .test_support()
                            .into_any_element(),
                        None => pending(40., 11.),
                    })
            };

            v_flex()
                .flex_1()
                .justify_between()
                .child(
                    header
                        .child(match stats {
                            Some(s) => div()
                                .font_family(MONO)
                                .text_size(px(12.5))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(f::pct_signed_f(s.change, 2))
                                .into_any_element(),
                            None => pending(52., 14.),
                        })
                        .when(reachable, |d| d.child(remove_button(ix, &market, cx))),
                )
                .child(
                    h_flex()
                        .items_baseline()
                        .gap(px(6.))
                        .child(match &tile.live {
                            Some(live) => {
                                ui::figure(f::price(live.last), px(19.)).into_any_element()
                            }
                            None => pending(110., 22.),
                        })
                        .child(div().flex_1())
                        // Sorted by funding, the tile shows what it is sorted by.
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(soft)
                                .child(if by_funding { "Funding (1h)" } else { "Vol" }),
                        )
                        .child(match (by_funding, &tile.live, stats) {
                            (true, Some(live), _) => div()
                                .id(("tile-vol", ix))
                                .aria_label(f::funding_hourly(live.funding))
                                .font_family(MONO)
                                .text_size(px(10.5))
                                .text_color(soft)
                                .child(f::funding_hourly(live.funding))
                                .test_support()
                                .into_any_element(),
                            (false, _, Some(s)) => div()
                                .id(("tile-vol", ix))
                                .aria_label(f::volatility(s.volatility))
                                .font_family(MONO)
                                .text_size(px(10.5))
                                .text_color(soft)
                                .child(f::volatility(s.volatility))
                                .test_support()
                                .into_any_element(),
                            _ => pending(30., 11.),
                        }),
                )
                .child(match placement {
                    Some(at) => range_bar(ix, &at),
                    None => pending(TILE_MIN - 40., 10.),
                })
                .child(
                    div()
                        .grid()
                        .grid_cols(2)
                        .gap_x_3()
                        .gap_y(px(2.))
                        .pt(px(5.))
                        .border_t_1()
                        .border_color(Palette::text().opacity(0.13))
                        .child(cell("tile-pos", "Position", position_text.clone()))
                        .child(cell("tile-30d", "30D PNL", d30.clone()))
                        .child(cell("tile-ytd", "YTD PNL", ytd.clone()))
                        .child(cell("tile-all", "All-time", all.clone())),
                )
                .into_any_element()
        };

        let hover_market = market.clone();
        let click_market = market.clone();
        let key_market = market.clone();
        let bounds_store = self.tile_bounds.clone();
        let bounds_market = market.clone();
        let tile_element = div()
            .id(("tile", ix))
            .relative()
            .child(
                gpui_kit::canvas(
                    move |b, _, _| {
                        bounds_store.borrow_mut().insert(bounds_market.clone(), b);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .h(px(TILE_HEIGHT))
            .cursor_pointer()
            .track_focus(&handle)
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered = Some(hover_market.clone());
                } else if this.hovered.as_ref() == Some(&hover_market) {
                    this.hovered = None;
                }
                cx.notify();
            }))
            .aria_label(name)
            .on_click(
                cx.listener(move |this, _, window, cx| this.select(&click_market, window, cx)),
            )
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "enter" | "space" => this.select(&key_market, window, cx),
                    "backspace" | "delete" => this.remove(&key_market, cx),
                    _ => {}
                }
            }))
            .child(
                v_flex()
                    .size_full()
                    .px_3()
                    .pt(px(10.))
                    .pb(px(9.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if selected {
                        Palette::amber()
                    } else if focused {
                        Palette::amber_line()
                    } else {
                        Palette::text().opacity(0.08)
                    })
                    .bg(market_tile::background(colour_now, period_now, tile))
                    .child(body),
            )
            // The "Alert for <market>…" form, over what lies below the tile.
            .children(
                self.tile_form
                    .as_ref()
                    .filter(|(m, _, _)| *m == market)
                    .map(|(_, form, _)| {
                        // Under the tile, or above it when the window has
                        // no room below; kept inside the window's edges.
                        let target = self
                            .tile_bounds
                            .borrow()
                            .get(&market)
                            .copied()
                            .unwrap_or_default();
                        deferred(
                            anchored()
                                .attach(target, gpui_kit::Anchor::BottomLeft)
                                .anchor(gpui_kit::Anchor::TopLeft)
                                .offset(gpui_kit::point(px(0.), px(6.)))
                                .snap_to_window_with_margin(px(8.))
                                .child(
                                    div()
                                        .id("tile-alert-form")
                                        .w(px(460.))
                                        .p_3()
                                        .rounded(px(8.))
                                        .border_1()
                                        .border_color(Palette::amber())
                                        .bg(Palette::panel())
                                        .shadow_lg()
                                        .child(form.clone())
                                        // Esc, or a click anywhere else, closes it.
                                        .capture_key_down(cx.listener(
                                            |this, e: &gpui_kit::KeyDownEvent, _, cx| {
                                                if e.keystroke.key == "escape" {
                                                    this.close_tile_form(cx);
                                                    cx.stop_propagation();
                                                }
                                            },
                                        ))
                                        .on_mouse_down_out(
                                            cx.listener(|this, _, _, cx| this.close_tile_form(cx)),
                                        )
                                        .test_support(),
                                ),
                        )
                        .with_priority(2)
                    }),
            )
            .test_support();
        // Right-click: "Alert for <market>…", while the watcher can take one.
        if !can_call {
            return tile_element.into_any_element();
        }
        let markets = cx.entity().downgrade();
        let last = tile.live.as_ref().map(|l| l.last);
        let label = format!("Alert for {}\u{2026}", market.ticker());
        tile_element
            .context_menu(move |menu, _, _| {
                let (markets, market, label) = (markets.clone(), market.clone(), label.clone());
                menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let _ = markets.update(cx, |m, cx| {
                        m.open_tile_form(market.clone(), last, window, cx)
                    });
                }))
            })
            .into_any_element()
    }
}

impl Render for TilesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // As many columns as fit the panel; before it has been measured, as
        // many as fit the window, less the section's padding.
        let width = self
            .width
            .get()
            .map(f32::from)
            .unwrap_or_else(|| f32::from(window.viewport_size().width) - 24.);
        let columns = (((width + GAP) / (TILE_MIN + GAP)).floor() as u16).max(1);
        let measure = {
            let width = self.width.clone();
            gpui_kit::canvas(
                move |b, _, _| width.set(Some(b.size.width)),
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };

        let watched = self.model.read(cx).watched_tiles();
        let body: AnyElement = match watched {
            // The default list needs the account's positions.
            None => h_flex()
                .size_full()
                .justify_center()
                .text_color(Palette::muted())
                .child("Waiting for the account's positions to choose the markets to watch\u{2026}")
                .into_any_element(),
            Some(tiles) if tiles.is_empty() => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_color(Palette::muted())
                        .child("Nothing is watched."),
                )
                .child(
                    Button::new("add-first")
                        .primary()
                        .label("Add a market")
                        .on_click(cx.listener(|this, _, window, cx| {
                            (this.on_event)(&TilesEvent::AddMarket, window, cx)
                        })),
                )
                .into_any_element(),
            // A tile for every watched market, at once: one the feed has not
            // reported yet is all placeholders.
            Some(tiles) => {
                let account = self.model.read(cx).account(cx);
                let mut grid = div().grid().grid_cols(columns).gap(px(GAP));
                // Drawn in the sorted order; a tile's id stays its place in the
                // watchlist, so focus and Remove follow the market.
                let order = self.model.update(cx, |m, _| m.settled_order(&tiles));
                for ix in order {
                    grid = grid.child(self.tile(ix, &tiles[ix], &account, window, cx));
                }
                div()
                    .id("tiles")
                    .size_full()
                    .overflow_y_scroll()
                    .child(grid)
                    .test_support()
                    .into_any_element()
            }
        };
        div().relative().size_full().child(measure).child(body)
    }
}

/// The remove action on an unavailable tile: always shown, and labelled.
fn remove_labelled(ix: usize, market: &MarketId, cx: &mut Context<TilesPanel>) -> Button {
    let market = market.clone();
    Button::new(("remove", ix))
        .xsmall()
        .label("Remove")
        .on_click(cx.listener(move |this, _, _, cx| this.remove(&market, cx)))
}

/// The remove action on a tile.
fn remove_button(ix: usize, market: &MarketId, cx: &mut Context<TilesPanel>) -> Button {
    let market = market.clone();
    Button::new(("remove", ix))
        .ghost()
        .xsmall()
        .label("\u{d7}")
        .accessibility_label(format!("Remove {}", market.ticker()))
        .tooltip("Remove from the watchlist")
        .on_click(cx.listener(move |this, _, _, cx| this.remove(&market, cx)))
}
