//! The account's position in one market: its size, value and PNL, its
//! liquidation price, open orders and the watcher's alerts, with the
//! close, take-profit / stop-loss and cancel actions.

use std::time::SystemTime;

use galata_desk_core::format as f;
use galata_desk_core::market::MarketId;
use galata_desk_core::rust_decimal::Decimal;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, FocusHandle, KeyDownEvent, SharedString, Subscription,
    TestSupportExt, Window, div, px,
};

use super::dock::{DockMeta, PanelKind, PanelRecord, dock_panel, market_name};
use super::model::{MarketSource, MarketsModel};
use super::section_title;
use crate::Core;
use crate::market_position::{self, Holding};
use crate::portfolio::Portfolio;
use crate::theme::{MONO, Palette};
use crate::ui;

pub struct PositionPanel {
    model: Entity<MarketsModel>,
    portfolio: Entity<Portfolio>,
    source: MarketSource,
    /// The position panel's PNL breakdown and open orders are expanded.
    /// Kept while the app runs, whatever market is selected.
    pnl_open: bool,
    orders_open: bool,
    pnl_focus: FocusHandle,
    orders_focus: FocusHandle,
    /// What the position panel last drew, for tests.
    drawn_position: std::cell::RefCell<Option<market_position::Model>>,
    /// The alerts menu: open, its price and distance fields, and what the
    /// last edit came to.
    alerts_open: bool,
    alert_price: Entity<InputState>,
    alert_near_liq: Entity<InputState>,
    alert_message: Option<(SharedString, bool)>,
    /// Its key and links in the workspace.
    dock: DockMeta,
    _subscriptions: Vec<Subscription>,
}

dock_panel!(PositionPanel, PanelKind::Position);

impl PositionPanel {
    pub fn new(
        model: Entity<MarketsModel>,
        source: MarketSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let portfolio = model.read(cx).portfolio.clone();
        let subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.observe(&portfolio, |_, _, cx| cx.notify()),
        ];
        Self {
            model,
            portfolio,
            source,
            pnl_open: false,
            orders_open: false,
            pnl_focus: cx.focus_handle(),
            orders_focus: cx.focus_handle(),
            drawn_position: std::cell::RefCell::new(None),
            alerts_open: false,
            alert_price: cx.new(|cx| InputState::new(window, cx).placeholder("Price")),
            alert_near_liq: cx.new(|cx| InputState::new(window, cx).placeholder("15")),
            alert_message: None,
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
            .record(PanelKind::Position, self.market(cx).as_ref(), None)
    }

    /// The panel's title: the market whose position it shows.
    pub fn title_text(&self, cx: &gpui_kit::App) -> String {
        match self.market(cx) {
            Some(market) => format!("Position \u{b7} {}", market_name(&market)),
            None => "Position".to_string(),
        }
    }

    /// The market the panel shows.
    pub fn market(&self, cx: &gpui_kit::App) -> Option<MarketId> {
        self.source.market(&self.model, cx)
    }

    /// Show `market`, for a panel that keeps its own. The alerts menu was the
    /// last market's, so it closes.
    #[allow(dead_code, reason = "the workspace's position panels use it")]
    pub fn set_market(&mut self, market: Option<MarketId>, cx: &mut Context<Self>) {
        if self.source.set(market) {
            self.alerts_open = false;
            cx.notify();
        }
    }

    /// The market changed under the panel: the alerts menu was the last
    /// market's, so it closes.
    pub fn market_changed(&mut self, cx: &mut Context<Self>) {
        self.close_alerts(cx);
    }

    /// The window's watcher connection, while it has one.
    fn watcher(&self, cx: &gpui_kit::App) -> Option<Entity<crate::watcher_link::WatcherLink>> {
        self.model.read(cx).watcher.clone()
    }

    /// The account's positions and per-market PNL.
    fn account(&self, cx: &gpui_kit::App) -> super::model::Account {
        self.model.read(cx).account(cx)
    }

    /// What the panel last drew.
    #[cfg(test)]
    pub fn position_panel(&self) -> Option<market_position::Model> {
        self.drawn_position.borrow().clone()
    }

    /// Type a price in the alerts menu.
    #[cfg(test)]
    pub fn set_alert_price(&self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.alert_price.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
    }

    /// Type a distance in the alerts menu.
    #[cfg(test)]
    pub fn set_alert_near_liq(&self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.alert_near_liq.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
    }

    /// Put the keyboard on the PNL breakdown's header.
    #[cfg(test)]
    pub fn focus_pnl_header(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.pnl_focus, cx);
    }

    /// Whether the alerts menu can edit the watcher, or why not: it must be
    /// connected, answering, configured, and watching this network.
    fn alerts_editable(&self, cx: &gpui_kit::App) -> Result<(), &'static str> {
        let Some(link) = self.watcher(cx) else {
            return Err("needs a watcher connection");
        };
        let snapshot = link.read(cx).snapshot();
        let Some(config) = snapshot.config.config() else {
            return Err("the watcher has no configuration yet");
        };
        if config.network != Core::get(cx).network {
            return Err("the watcher watches another network");
        }
        if snapshot.editable().is_none() {
            return Err("the watcher cannot be reached");
        }
        Ok(())
    }

    /// The watcher's levels in `market`, armed first: each as its direction,
    /// price and state.
    fn levels_in(&self, market: &MarketId, cx: &gpui_kit::App) -> Vec<(String, bool)> {
        let Some(link) = self.watcher(cx) else {
            return Vec::new();
        };
        let Some(config) = link.read(cx).snapshot().config.config() else {
            return Vec::new();
        };
        let mut levels: Vec<(String, bool)> = config
            .levels
            .iter()
            .filter(|l| l.market == market.coin())
            .map(|l| {
                let armed = l.is_armed();
                (
                    format!(
                        "{} {} \u{b7} {}",
                        match l.direction {
                            galata_desk_core::watcher::Direction::Above => "Above",
                            galata_desk_core::watcher::Direction::Below => "Below",
                        },
                        f::price(l.price),
                        if armed { "armed" } else { "fired" }
                    ),
                    armed,
                )
            })
            .collect();
        levels.sort_by_key(|(_, armed)| !armed);
        levels
    }

    fn toggle_alerts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.alerts_open = !self.alerts_open;
        self.alert_message = None;
        if self.alerts_open {
            let pct = self.watcher(cx).and_then(|l| {
                l.read(cx)
                    .snapshot()
                    .config
                    .config()
                    .map(|c| c.near_liquidation_pct.normalize().to_string())
            });
            if let Some(pct) = pct {
                self.alert_near_liq
                    .update(cx, |input, cx| input.set_value(pct, window, cx));
            }
            self.alert_price
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.notify();
    }

    fn close_alerts(&mut self, cx: &mut Context<Self>) {
        if self.alerts_open {
            self.alerts_open = false;
            cx.notify();
        }
    }

    /// Add an armed level in `market` at the price typed, its direction from
    /// `last`.
    fn add_alert(&mut self, market: &MarketId, last: Option<Decimal>, cx: &mut Context<Self>) {
        if self.alerts_editable(cx).is_err() {
            return;
        }
        let Some(link) = self.watcher(cx) else {
            return;
        };
        let typed = self.alert_price.read(cx).value().trim().replace(',', "");
        let Some(price) = typed.parse::<Decimal>().ok().filter(|p| *p > Decimal::ZERO) else {
            self.alert_message =
                Some(("Enter a price above zero. Nothing was saved.".into(), true));
            cx.notify();
            return;
        };
        let coin = market.coin().to_string();
        let adding = link.update(cx, |link, cx| {
            link.add_level(coin, price, None, None, last, cx)
        });
        cx.spawn(async move |this, cx| {
            let outcome = adding.await;
            let _ = this.update(cx, |this, cx| {
                this.alert_message = Some(match outcome {
                    Ok(_) => ("Alert saved.".into(), false),
                    Err(why) => (why.into(), true),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Save an every-market setting through the watcher, with the version it
    /// was read at.
    fn save_alert_setting(
        &mut self,
        edit: impl FnOnce(&mut galata_desk_core::watcher::WatcherConfig) + Send + 'static,
        done: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.alerts_editable(cx).is_err() {
            return;
        }
        let Some(link) = self.watcher(cx) else {
            return;
        };
        let saving = link.update(cx, |link, cx| link.save(edit, cx));
        cx.spawn(async move |this, cx| {
            let outcome = saving.await;
            let _ = this.update(cx, |this, cx| {
                this.alert_message = Some(match outcome {
                    Ok(_) => (done.into(), false),
                    Err(e) => (crate::watcher::outcome_message(&e).into(), true),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn save_alert_near_liq(&mut self, cx: &mut Context<Self>) {
        let text = self
            .alert_near_liq
            .read(cx)
            .value()
            .trim()
            .trim_end_matches('%')
            .to_string();
        let Some(pct) = text
            .parse::<Decimal>()
            .ok()
            .filter(|p| *p >= Decimal::TWO && *p <= Decimal::new(50, 0))
        else {
            self.alert_message = Some((
                "The distance is from 2% to 50%. Nothing was saved.".into(),
                true,
            ));
            cx.notify();
            return;
        };
        self.save_alert_setting(move |c| c.near_liquidation_pct = pct, "Distance saved.", cx);
    }

    /// The bell in the position panel's header, with the market's armed
    /// levels counted on it.
    fn alerts_bell(&self, market: &MarketId, cx: &mut Context<Self>) -> AnyElement {
        let ticker = market.ticker();
        let (armed, _) = self.model.read(cx).calls_in(market, cx);
        let editable = self.alerts_editable(cx);
        let name = match editable {
            Ok(()) => format!("Alerts for {ticker}"),
            Err(why) => format!("Alerts for {ticker}: {why}"),
        };
        div()
            .relative()
            .child(
                Button::new("position-alerts")
                    .ghost()
                    .small()
                    .icon(Icon::new(IconName::Bell))
                    .accessibility_label(name)
                    .disabled(editable.is_err())
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_alerts(window, cx))),
            )
            .when(!armed.is_empty(), |d| {
                d.child(
                    div()
                        .id("alerts-count")
                        .absolute()
                        .top(px(-3.))
                        .right(px(-3.))
                        .min_w(px(14.))
                        .h(px(14.))
                        .px(px(3.))
                        .rounded_full()
                        .bg(Palette::gain())
                        .text_size(px(10.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(Palette::ground())
                        .flex()
                        .items_center()
                        .justify_center()
                        .aria_label(format!("{} armed", armed.len()))
                        .child(armed.len().to_string())
                        .test_support(),
                )
            })
            .into_any_element()
    }

    /// The alerts menu over the position panel: the market's levels, a way
    /// to add one, and the settings that apply to every market.
    fn alerts_menu(
        &self,
        market: &MarketId,
        last: Option<Decimal>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ticker = market.ticker().to_string();
        let levels = self.levels_in(market, cx);
        let config = self
            .watcher(cx)
            .and_then(|l| l.read(cx).snapshot().config.config().cloned());
        let stop_on = config.as_ref().is_none_or(|c| c.stop_alerts);
        let add_market = market.clone();
        v_flex()
            .id("alerts-menu")
            .absolute()
            .top(px(40.))
            .right(px(10.))
            .w(px(280.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .bg(Palette::panel())
            .border_1()
            .border_color(Palette::line())
            .shadow_lg()
            .occlude()
            .text_size(px(12.5))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_alerts(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close_alerts(cx);
                }
            }))
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(format!("Alerts for {ticker}")),
            )
            .child(section_title("Price alerts"))
            .children(if levels.is_empty() {
                vec![
                    div()
                        .text_color(Palette::muted())
                        .child(format!("No price alerts for {ticker}"))
                        .into_any_element(),
                ]
            } else {
                levels
                    .into_iter()
                    .enumerate()
                    .map(|(i, (text, armed))| {
                        div()
                            .id(("alert-level", i))
                            .font_family(MONO)
                            .text_color(if armed {
                                Palette::text()
                            } else {
                                Palette::muted()
                            })
                            .aria_label(text.clone())
                            .child(text)
                            .test_support()
                            .into_any_element()
                    })
                    .collect()
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_color(Palette::muted()).child("Price crosses"))
                    .child(div().flex_1().child(Input::new(&self.alert_price).small()))
                    .child(
                        Button::new("alert-add")
                            .small()
                            .label("Add")
                            .accessibility_label(format!("Add a price alert for {ticker}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let m = add_market.clone();
                                this.add_alert(&m, last, cx)
                            })),
                    ),
            )
            .child(div().h(px(1.)).bg(Palette::rule()))
            .child(section_title("Every market"))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child("Take-profit and stop-loss fills"))
                    .child(
                        gpui_kit::component::switch::Switch::new("alert-stop-alerts")
                            .checked(stop_on)
                            .accessibility_label("Take-profit and stop-loss alerts, every market")
                            .on_click(cx.listener(|this, on: &bool, _, cx| {
                                let on = *on;
                                this.save_alert_setting(
                                    move |c| c.stop_alerts = on,
                                    if on {
                                        "Stop alerts on."
                                    } else {
                                        "Stop alerts off."
                                    },
                                    cx,
                                )
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child("Near liquidation, within"))
                    .child(
                        div()
                            .w(px(56.))
                            .child(Input::new(&self.alert_near_liq).small()),
                    )
                    .child("%")
                    .child(
                        Button::new("alert-near-liq-save")
                            .small()
                            .label("Save")
                            .accessibility_label("Save the near-liquidation distance")
                            .on_click(cx.listener(|this, _, _, cx| this.save_alert_near_liq(cx))),
                    ),
            )
            .child(ui::label(
                "These two apply to every market. Liquidation alerts can't be switched off.",
            ))
            .children(self.alert_message.as_ref().map(|(text, problem)| {
                div()
                    .id("alert-message")
                    .text_color(if *problem {
                        Palette::amber_text()
                    } else {
                        Palette::muted()
                    })
                    .aria_label(text.clone())
                    .child(text.clone())
                    .test_support()
            }))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(Palette::muted())
                            .child("Sent by your watcher, to Telegram"),
                    )
                    .child(
                        Button::new("alerts-done")
                            .small()
                            .label("Done")
                            .on_click(cx.listener(|this, _, _, cx| this.close_alerts(cx))),
                    ),
            )
            .test_support()
            .into_any_element()
    }

    fn toggle_pnl(&mut self, cx: &mut Context<Self>) {
        self.pnl_open = !self.pnl_open;
        cx.notify();
    }

    fn toggle_orders(&mut self, cx: &mut Context<Self>) {
        self.orders_open = !self.orders_open;
        cx.notify();
    }

    /// A position-panel section's header line: a button that expands or
    /// collapses the section, and tells assistive technology which it is.
    #[allow(clippy::too_many_arguments)]
    fn section_header(
        &self,
        id: &'static str,
        focus: &FocusHandle,
        title: &'static str,
        open: bool,
        right: AnyElement,
        toggle: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(id)
            .track_focus(focus)
            .cursor_pointer()
            .min_h(px(28.))
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(Palette::rule())
            .role(gpui_kit::Role::Button)
            .aria_expanded(open)
            .aria_label(title)
            .on_click(cx.listener(move |this, _, _, cx| toggle(this, cx)))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .small()
                .text_color(Palette::muted()),
            )
            .child(section_title(title))
            .child(div().flex_1())
            .child(right)
            .test_support()
            .into_any_element()
    }

    /// The panel's right side: the account's position in the selected
    /// market, its PNL, its last trade and its resting orders, and the
    /// actions for them.
    fn position_column(
        &self,
        market: &MarketId,
        last: Option<Decimal>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let account = self.account(cx);
        let pnl = account.pnl.as_ref().map(|all| all.get(market).copied());
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let (model, can_act, blocked, cancellable) = {
            let portfolio = self.portfolio.read(cx);
            (
                market_position::build(
                    market,
                    account.positions.as_deref(),
                    pnl,
                    portfolio.fills(),
                    portfolio.orders(),
                    now_ms,
                ),
                portfolio.can_act_now(),
                portfolio.actions_blocked(true),
                !portfolio.market_orders(market).is_empty(),
            )
        };
        *self.drawn_position.borrow_mut() = Some(model.clone());
        let ticker = market.ticker().to_string();
        let pending = |w: f32, h: f32| ui::placeholder(px(w), px(h)).into_any_element();
        let mono = |text: String, tone: gpui_kit::Hsla| {
            div()
                .font_family(MONO)
                .text_color(tone)
                .child(text)
                .into_any_element()
        };

        // The header: the side and leverage, or that there is none.
        let badge = |text: String, tone: gpui_kit::Hsla| {
            div()
                .id("position-side")
                .px(px(7.))
                .py(px(2.))
                .rounded(px(4.))
                .text_size(px(11.5))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .bg(tone.opacity(0.14))
                .text_color(tone)
                .aria_label(text.clone())
                .child(text)
                .test_support()
                .into_any_element()
        };
        let header = h_flex()
            .px_4()
            .pt(px(8.))
            .pb(px(4.))
            .gap_2()
            .items_center()
            .child(div().size(px(7.)).rounded_full().bg(Palette::amber()))
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Your position"),
            )
            .child(div().flex_1())
            .child(match &model.holding {
                Holding::Loading => pending(84., 18.),
                Holding::Flat => badge("Flat".into(), Palette::muted()),
                Holding::Held(h) => badge(
                    h.side.clone(),
                    if h.long {
                        Palette::gain()
                    } else {
                        Palette::loss()
                    },
                ),
            })
            .child(self.alerts_bell(market, cx));
        let menu = self.alerts_open.then(|| self.alerts_menu(market, last, cx));

        let mut body = v_flex().px_4().pb_2().gap(px(4.));

        // The position: size and unrealised PNL, then its details.
        match &model.holding {
            Holding::Loading => {
                body = body.child(
                    h_flex()
                        .gap_2()
                        .child(pending(126., 58.))
                        .child(pending(126., 58.)),
                );
            }
            Holding::Flat => {
                body = body.child(
                    div()
                        .id("position-flat")
                        .p_2()
                        .rounded(px(6.))
                        .bg(Palette::raise())
                        .text_size(px(12.))
                        .text_color(Palette::muted())
                        .child(format!(
                            "No open {ticker} position. Realised PNL, the last trade and resting orders still show below."
                        ))
                        .test_support(),
                );
            }
            Holding::Held(h) => {
                let card = |id: &'static str,
                            label: &'static str,
                            big: String,
                            small: String,
                            tone: gpui_kit::Hsla,
                            small_tone: gpui_kit::Hsla| {
                    v_flex()
                        .id(id)
                        .flex_1()
                        .min_w_0()
                        .px_2()
                        .py(px(4.))
                        .rounded(px(6.))
                        .bg(Palette::raise())
                        .gap(px(1.))
                        .aria_label(format!("{label} {big}, {small}"))
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(14.5))
                                .text_color(tone)
                                .whitespace_nowrap()
                                .child(big),
                        )
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(11.5))
                                .text_color(small_tone)
                                .whitespace_nowrap()
                                .child(small),
                        )
                        .test_support()
                };
                let pnl_tone = ui::tone_of(h.pnl_sign);
                body = body
                    .child(
                        h_flex()
                            .gap_2()
                            .child(card(
                                "position-size",
                                "size",
                                h.size.clone(),
                                h.value.clone(),
                                Palette::text(),
                                Palette::muted(),
                            ))
                            .child(card(
                                "position-pnl",
                                "unrealised",
                                h.pnl.clone(),
                                format!("{} ROE", h.roe),
                                pnl_tone,
                                pnl_tone,
                            )),
                    )
                    .child(v_flex().children(h.rows.iter().enumerate().map(
                        |(i, (name, value, tone))| {
                            h_flex()
                                .id(("position-row", i))
                                .justify_between()
                                .py(px(2.))
                                .border_t_1()
                                .border_color(Palette::rule())
                                .text_size(px(12.5))
                                .aria_label(format!("{name} {value}"))
                                .child(div().text_color(Palette::muted()).child(*name))
                                .child(mono(value.clone(), ui::tone_of(*tone)))
                                .test_support()
                        },
                    )));
            }
        }

        // PNL: one line with the day's total, the four windows when open.
        let day = match model.day_total() {
            Some((text, s)) => h_flex()
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(Palette::muted())
                .child("24h")
                .child(mono(text, ui::tone_of(s)))
                .into_any_element(),
            None => pending(70., 13.),
        };
        body = body.child(self.section_header(
            "pnl-header",
            &self.pnl_focus,
            "PNL breakdown",
            self.pnl_open,
            day,
            Self::toggle_pnl,
            cx,
        ));
        if self.pnl_open {
            let cell = |text: String, tone: gpui_kit::Hsla| {
                div()
                    .w(px(78.))
                    .flex_shrink_0()
                    .text_right()
                    .font_family(MONO)
                    .text_color(tone)
                    .child(text)
            };
            let heading = h_flex()
                .text_size(px(11.5))
                .text_color(Palette::muted())
                .child(div().flex_1().child("Period"))
                .child(cell("Realised".into(), Palette::muted()))
                .child(cell("Total".into(), Palette::muted()));
            body = body.child(match &model.pnl {
                None => pending(240., 60.),
                Some(rows) => v_flex()
                    .id("pnl-rows")
                    .child(heading)
                    .children(rows.iter().enumerate().map(|(i, r)| {
                        h_flex()
                            .id(("pnl-row", i))
                            .py(px(4.))
                            .border_t_1()
                            .border_color(Palette::rule())
                            .text_size(px(12.5))
                            .aria_label(format!(
                                "{}: realised {}, total {}",
                                r.label, r.realised, r.total
                            ))
                            .child(div().flex_1().child(r.label))
                            .child(cell(r.realised.clone(), ui::tone_of(r.realised_sign)))
                            .child(cell(r.total.clone(), ui::tone_of(r.total_sign)))
                            .test_support()
                    }))
                    .test_support()
                    .into_any_element(),
            });
        }

        // The last trade.
        body = body.child(
            v_flex()
                .pt(px(5.))
                .gap(px(2.))
                .border_t_1()
                .border_color(Palette::rule())
                .child(section_title("Last trade"))
                .child(match &model.trade {
                    None => pending(240., 14.),
                    Some(None) => div()
                        .id("last-trade")
                        .text_size(px(12.))
                        .text_color(Palette::muted())
                        .aria_label("No recent trade")
                        .child("No recent trade")
                        .test_support()
                        .into_any_element(),
                    Some(Some(trade)) => {
                        let side = if trade.buy { "Buy" } else { "Sell" };
                        h_flex()
                            .id("last-trade")
                            .gap(px(5.))
                            .text_size(px(12.))
                            .aria_label(format!(
                                "{} {side} {} at {}, fee {}",
                                trade.time, trade.size, trade.price, trade.fee
                            ))
                            .child(mono(trade.time.clone(), Palette::muted()))
                            .child(
                                div()
                                    .text_color(ui::tone_of(if trade.buy { 1 } else { -1 }))
                                    .child(side),
                            )
                            .child(mono(
                                format!("{} @ {}", trade.size, trade.price),
                                Palette::text(),
                            ))
                            .child(mono(format!("\u{b7} {}", trade.fee), Palette::muted()))
                            .test_support()
                            .into_any_element()
                    }
                }),
        );

        // Open orders: a count and what kinds, the list when open.
        let count = model.orders.as_ref().map(Vec::len);
        let summary = model.orders_summary();
        let orders_right = match count {
            None => pending(60., 13.),
            Some(n) => h_flex()
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(Palette::muted())
                .child(
                    div()
                        .id("orders-count")
                        .px(px(6.))
                        .rounded(px(8.))
                        .bg(Palette::raise())
                        .font_family(MONO)
                        .text_color(Palette::text())
                        .aria_label(format!("{n} open orders"))
                        .child(n.to_string())
                        .test_support(),
                )
                .child(
                    div()
                        .id("orders-summary")
                        .aria_label(summary.clone())
                        .child(summary)
                        .test_support(),
                )
                .into_any_element(),
        };
        body = body.child(self.section_header(
            "orders-header",
            &self.orders_focus,
            "Open orders",
            self.orders_open,
            orders_right,
            Self::toggle_orders,
            cx,
        ));
        if self.orders_open {
            body = body.child(match &model.orders {
                None => pending(240., 40.),
                Some(lines) if lines.is_empty() => div()
                    .id("orders-none")
                    .text_size(px(12.5))
                    .text_color(Palette::muted())
                    .child(format!("No open {ticker} orders"))
                    .test_support()
                    .into_any_element(),
                Some(lines) => {
                    let m = market.clone();
                    let mut list = v_flex().id("orders-list").child(
                        h_flex().justify_end().child(
                            Button::new("cancel-market-orders")
                                .ghost()
                                .xsmall()
                                .label("Cancel all")
                                .accessibility_label(format!("Cancel all {ticker} orders"))
                                .disabled(!can_act || !cancellable)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let m = m.clone();
                                    this.portfolio
                                        .update(cx, |p, cx| p.open_cancel_market(&m, window, cx));
                                })),
                        ),
                    );
                    for o in lines {
                        let oid = o.oid;
                        let side = if o.buy { "Buy" } else { "Sell" };
                        list = list.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .border_t_1()
                                .border_color(Palette::rule())
                                .text_size(px(12.5))
                                .child(
                                    h_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap(px(5.))
                                        .child(
                                            div()
                                                .text_color(ui::tone_of(if o.buy { 1 } else { -1 }))
                                                .child(side),
                                        )
                                        .child(
                                            div()
                                                .text_color(Palette::muted())
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .child(o.kind.clone()),
                                        ),
                                )
                                .child(mono(o.size.clone(), Palette::text()))
                                .child(
                                    div()
                                        .w(px(70.))
                                        .text_right()
                                        .child(mono(o.price.clone(), Palette::text())),
                                )
                                .child(
                                    Button::new(("cancel-market-order", oid as usize))
                                        .ghost()
                                        .xsmall()
                                        .icon(Icon::new(IconName::Close))
                                        .accessibility_label(format!(
                                            "Cancel {side} {} order in {ticker}",
                                            o.kind
                                        ))
                                        .disabled(!can_act || !o.cancellable)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.portfolio
                                                .update(cx, |p, cx| p.open_cancel(oid, window, cx));
                                        })),
                                ),
                        );
                    }
                    list.test_support().into_any_element()
                }
            });
        }

        // Why the actions cannot be used, when they cannot.
        let has_orders = count.is_some_and(|n| n > 0);
        let held = matches!(model.holding, Holding::Held(_));
        let guard = blocked
            .filter(|_| held || has_orders)
            .map(|(heading, sentence)| {
                div()
                    .id("position-guard")
                    .px_4()
                    .pt_1()
                    .text_size(px(11.5))
                    .text_color(Palette::amber_text())
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .aria_label(format!("{heading}. {sentence}"))
                    .tooltip(move |window, cx| Tooltip::new(sentence.clone()).build(window, cx))
                    .child(heading)
                    .test_support()
            });

        // The position's actions.
        let actions = held.then(|| {
            let (tpsl_market, close_market) = (market.clone(), market.clone());
            let loss = Palette::loss();
            h_flex()
                .px_4()
                .pt(px(6.))
                .pb(px(8.))
                .gap_2()
                .border_t_1()
                .border_color(Palette::rule())
                .child(
                    Button::new("position-tpsl")
                        .flex_1()
                        .small()
                        .outline()
                        .label("Set TP / SL")
                        .accessibility_label(format!("Set TP / SL for {ticker}"))
                        .disabled(!can_act)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let m = tpsl_market.clone();
                            this.portfolio
                                .update(cx, |p, cx| p.open_tpsl(&m, window, cx));
                        })),
                )
                .child(
                    Button::new("position-close")
                        .flex_1()
                        .small()
                        .label("Close position")
                        .custom(
                            ButtonCustomVariant::new(cx)
                                .color(loss)
                                .foreground(loss)
                                .hover(loss.opacity(0.3))
                                .active(loss.opacity(0.3)),
                        )
                        .accessibility_label(format!("Close {ticker} position"))
                        .disabled(!can_act)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let m = close_market.clone();
                            this.portfolio
                                .update(cx, |p, cx| p.open_close(&m, window, cx));
                        })),
                )
        });

        v_flex()
            .id("position-panel")
            .relative()
            .w(px(market_position::WIDTH))
            .h_full()
            .flex_shrink_0()
            .border_l_1()
            .border_color(Palette::rule())
            .child(header)
            .child(
                div()
                    .id("position-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .children(guard)
            .children(actions)
            .children(menu)
            .test_support()
            .into_any_element()
    }
}

impl Render for PositionPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(market) = self.market(cx) else {
            return div().into_any_element();
        };
        let last = self
            .model
            .read(cx)
            .tile_of(&market)
            .live
            .as_ref()
            .map(|l| l.last);
        self.position_column(&market, last, cx)
    }
}
