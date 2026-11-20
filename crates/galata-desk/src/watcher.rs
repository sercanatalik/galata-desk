//! The Watcher section: whether galata-watcher is alive, and what it alerts
//! on. Everything shown comes from the window's [`WatcherLink`]; every
//! change is saved through it, with the version it was based on.
//!
//! **The desk only edits and shows.** Nothing here evaluates an alert or
//! sends one. While the watcher can't be reached, the section shows what it
//! last read, marked stale, and offers no edits.

use std::collections::BTreeSet;

use galata_desk_core::format::{self as f, short_address};
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::watcher::{
    AlertRow, Direction, WatcherConfig, WatcherError, WatcherSnapshot,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, Focusable, SharedString, Subscription, TestSupportExt, Window,
    div, px,
};

use crate::Core;
use crate::level_form::{LevelForm, LevelFormEvent};
use crate::portfolio::Portfolio;
use crate::theme::{MONO, Palette};
use crate::ui;
use crate::watcher_link::{STALLED_MS, WatcherLink, ago, now_ms};

/// What the section says about liquidation alerts.
pub const LIQUIDATION_NOTE: &str = "Liquidation and near-liquidation alerts can't be switched off. Only the distance can be tuned, from 2% to 50%.";

/// A level being edited in place.
struct Editing {
    id: String,
    price: Entity<InputState>,
    note: Entity<InputState>,
    direction: Direction,
}

pub struct WatcherSection {
    link: Entity<WatcherLink>,
    portfolio: Entity<Portfolio>,
    add: Entity<LevelForm>,
    editing: Option<Editing>,
    near_liq: Entity<InputState>,
    /// The outcome of the last edit, and whether it is a problem.
    message: Option<(SharedString, bool)>,
    _changes: [Subscription; 3],
}

impl WatcherSection {
    pub fn new(
        link: Entity<WatcherLink>,
        portfolio: Entity<Portfolio>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let add = cx.new(|cx| LevelForm::open("add-level", link.clone(), window, cx));
        let near_liq = cx.new(|cx| InputState::new(window, cx).placeholder("15"));
        let on_add = cx.subscribe(&add, |this, _, event, cx| {
            if let LevelFormEvent::Saved = event {
                this.message = Some(("Level saved. It is armed.".into(), false));
                cx.notify();
            }
        });
        let link_changes = cx.observe_in(&link, window, |this, link, window, cx| {
            this.on_snapshot(&link, window, cx);
            cx.notify();
        });
        let portfolio_changes = cx.observe(&portfolio, |_, _, cx| cx.notify());
        let mut this = Self {
            link: link.clone(),
            portfolio,
            add,
            editing: None,
            near_liq,
            message: None,
            _changes: [on_add, link_changes, portfolio_changes],
        };
        this.on_snapshot(&link, window, cx);
        this
    }

    /// Keep the distance field in step with the watcher while the user isn't
    /// changing it.
    fn on_snapshot(
        &mut self,
        link: &Entity<WatcherLink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pct) = link
            .read(cx)
            .snapshot()
            .config
            .config()
            .map(|c| c.near_liquidation_pct)
        else {
            return;
        };
        let text = pct.normalize().to_string();
        let focused = self.near_liq.read(cx).focus_handle(cx).is_focused(window);
        if !focused && self.near_liq.read(cx).value() != text.as_str() {
            self.near_liq
                .update(cx, |i, cx| i.set_value(text, window, cx));
        }
    }

    fn snapshot<'a>(&self, cx: &'a gpui_kit::App) -> &'a WatcherSnapshot {
        self.link.read(cx).snapshot()
    }

    /// Save `edit` and say how it went.
    fn save(
        &mut self,
        edit: impl FnOnce(&mut WatcherConfig) + Send + 'static,
        done: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.snapshot(cx).editable().is_none() {
            return;
        }
        let saving = self.link.update(cx, |link, cx| link.save(edit, cx));
        cx.spawn(async move |this, cx| {
            let outcome = saving.await;
            let _ = this.update(cx, |this, cx| {
                this.message = Some(match outcome {
                    Ok(_) => (done.into(), false),
                    Err(e) => (outcome_message(&e).into(), true),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.save(
            move |c| c.levels.retain(|l| l.id.as_deref() != Some(id.as_str())),
            "Level deleted.",
            cx,
        );
    }

    fn rearm(&mut self, id: String, cx: &mut Context<Self>) {
        self.save(
            move |c| {
                if let Some(level) = c
                    .levels
                    .iter_mut()
                    .find(|l| l.id.as_deref() == Some(id.as_str()))
                {
                    level.rearm();
                }
            },
            "Level re-armed.",
            cx,
        );
    }

    fn start_edit(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(level) = self
            .snapshot(cx)
            .config
            .config()
            .and_then(|c| {
                c.levels
                    .iter()
                    .find(|l| l.id.as_deref() == Some(id.as_str()))
            })
            .cloned()
        else {
            return;
        };
        let price = cx.new(|cx| {
            InputState::new(window, cx).default_value(level.price.normalize().to_string())
        });
        let note = cx.new(|cx| {
            InputState::new(window, cx).default_value(level.note.clone().unwrap_or_default())
        });
        self.editing = Some(Editing {
            id,
            price,
            note,
            direction: level.direction,
        });
        cx.notify();
    }

    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = &self.editing else { return };
        let Some(price) = editing
            .price
            .read(cx)
            .value()
            .trim()
            .replace(',', "")
            .parse::<Decimal>()
            .ok()
            .filter(|p| *p > Decimal::ZERO)
        else {
            self.message = Some(("Enter a price above zero. Nothing was saved.".into(), true));
            cx.notify();
            return;
        };
        let note = Some(editing.note.read(cx).value().trim().to_string()).filter(|n| !n.is_empty());
        let (id, direction) = (editing.id.clone(), editing.direction);
        self.editing = None;
        self.save(
            move |c| {
                if let Some(level) = c
                    .levels
                    .iter_mut()
                    .find(|l| l.id.as_deref() == Some(id.as_str()))
                {
                    level.price = price;
                    level.direction = direction;
                    level.note = note;
                }
            },
            "Level saved.",
            cx,
        );
    }

    fn set_stop_alerts(&mut self, ticker: String, on: bool, cx: &mut Context<Self>) {
        let done = if on {
            "Stop alerts on."
        } else {
            "Stop alerts off."
        };
        self.save(
            move |c| {
                c.stop_alerts.insert(ticker, on);
            },
            done,
            cx,
        );
    }

    fn save_near_liq(&mut self, cx: &mut Context<Self>) {
        let text = self
            .near_liq
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
            self.message = Some((
                "The distance is from 2% to 50%. Nothing was saved.".into(),
                true,
            ));
            cx.notify();
            return;
        };
        self.save(move |c| c.near_liquidation_pct = pct, "Distance saved.", cx);
    }

    /// Tickers with a position or a stop alert setting, by the watcher's
    /// name for them (`BTC`, `xyz:NVDA`).
    fn stop_tickers(&self, cx: &gpui_kit::App) -> Vec<(String, Option<bool>)> {
        let config = self.snapshot(cx).config.config();
        let mut tickers: BTreeSet<String> = self
            .portfolio
            .read(cx)
            .positions()
            .unwrap_or_default()
            .iter()
            .map(|p| {
                if p.dex.is_empty() {
                    p.market.clone()
                } else {
                    format!("{}:{}", p.dex, p.market)
                }
            })
            .collect();
        if let Some(c) = config {
            tickers.extend(c.stop_alerts.keys().cloned());
        }
        tickers
            .into_iter()
            .map(|t| {
                let setting = config.and_then(|c| c.stop_alerts.get(&t).copied());
                (t, setting)
            })
            .collect()
    }

    /// The account the watcher watches, when it is not the connected one.
    fn other_account(&self, cx: &gpui_kit::App) -> Option<String> {
        let config = self.snapshot(cx).config.config()?;
        let account = Core::get(cx).store.account()?;
        (!config
            .address
            .eq_ignore_ascii_case(account.address.as_str()))
        .then(|| config.address.clone())
    }

    /// The health line: a short state and whether it needs attention.
    fn health_state(&self, now: u64, cx: &gpui_kit::App) -> (String, bool) {
        match self.snapshot(cx).health.as_ref().and_then(|h| h.last_ok_ms) {
            None => ("The watcher has not checked the account yet.".into(), true),
            Some(t) if now.saturating_sub(t) > STALLED_MS => (
                format!(
                    "The watcher has not checked the account since {}.",
                    ago(now, t)
                ),
                true,
            ),
            Some(_) => ("Checking normally".into(), false),
        }
    }

    /// The stale banner, while the watcher can't be read.
    fn stale_banner(&self, now: u64, cx: &gpui_kit::App) -> Option<String> {
        let s = self.snapshot(cx);
        let problem = s.problem?;
        Some(match s.reached_ms {
            Some(t) => format!(
                "{} Showing what was read {}. Edits are off until it answers.",
                problem.message(),
                ago(now, t)
            ),
            None => format!("{} Nothing has been read from it yet.", problem.message()),
        })
    }

    fn health_panel(&self, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let s = self.snapshot(cx);
        let (state, attention) = self.health_state(now, cx);
        let config = s.config.config();
        let other = self.other_account(cx);
        let kv = |name: &str, value: String| {
            v_flex()
                .gap(px(2.))
                .child(ui::label(name.to_uppercase()))
                .child(div().font_family(MONO).text_size(px(12.5)).child(value))
        };
        let health = s.health.clone();
        ui::panel()
            .px_5()
            .py_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_6()
                    .flex_wrap()
                    .child(
                        div()
                            .id("watcher-health")
                            .px_2()
                            .rounded(px(999.))
                            .text_size(px(12.5))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(if attention { Palette::amber() } else { Palette::gain() })
                            .bg(if attention { Palette::amber() } else { Palette::gain() }.opacity(0.12))
                            .child(state)
                            .test_support(),
                    )
                    .child(kv(
                        "Last check",
                        health
                            .as_ref()
                            .and_then(|h| h.last_ok_ms)
                            .map(|t| ago(now, t))
                            .unwrap_or_else(|| "never".into()),
                    ))
                    .child(kv(
                        "Watching",
                        config
                            .map(|c| format!("{} \u{b7} {}", short_address(&c.address), c.network.label().to_lowercase()))
                            .unwrap_or_else(|| "not configured".into()),
                    ))
                    .child(kv(
                        "Config",
                        config.map(|c| format!("version {}", c.version)).unwrap_or_else(|| "\u{2014}".into()),
                    ))
                    .child(kv(
                        "Last error",
                        match health.as_ref().and_then(|h| h.last_error.clone()) {
                            Some(e) => match health.as_ref().and_then(|h| h.last_error_ms) {
                                Some(t) => format!("{e} ({})", ago(now, t)),
                                None => e,
                            },
                            None => "none".into(),
                        },
                    )),
            )
            .when_some(other, |d, address| {
                d.child(ui::notice(format!(
                    "The watcher watches {}, not the connected account. Settings has \u{201c}Watch this account\u{201d}.",
                    short_address(&address)
                )))
            })
            .into_any_element()
    }

    fn levels_panel(&self, editable: bool, cx: &mut Context<Self>) -> AnyElement {
        let levels = self
            .snapshot(cx)
            .config
            .config()
            .map(|c| c.levels.clone())
            .unwrap_or_default();
        let rows = levels.into_iter().enumerate().map(|(ix, level)| {
            let id = level.id.clone().unwrap_or_default();
            let editing = self.editing.as_ref().filter(|e| e.id == id);
            let state = match level.fired() {
                Some(t) => div()
                    .text_color(Palette::muted())
                    .child(format!("fired {}", clock(t))),
                None => div().text_color(Palette::amber()).child("armed"),
            };
            let row = h_flex()
                .id(("level-row", ix))
                .gap_3()
                .py(px(6.))
                .border_b_1()
                .border_color(Palette::line())
                .child(
                    div()
                        .w(px(110.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(level.market.clone()),
                );
            let row = match editing {
                Some(e) => {
                    let dir = |d: Direction, cx: &mut Context<Self>| {
                        let b = Button::new(SharedString::from(format!("edit-{}", d.label())))
                            .small()
                            .label(d.label());
                        let b = if e.direction == d {
                            b.primary()
                        } else {
                            b.ghost()
                        };
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(e) = &mut this.editing {
                                e.direction = d;
                            }
                            cx.notify();
                        }))
                    };
                    row.child(div().w(px(110.)).child(Input::new(&e.price).small()))
                        .child(dir(Direction::Above, cx))
                        .child(dir(Direction::Below, cx))
                        .child(div().flex_1().child(Input::new(&e.note).small()))
                        .child(
                            Button::new("edit-save")
                                .small()
                                .primary()
                                .label("Save")
                                .disabled(!editable)
                                .on_click(cx.listener(|this, _, _, cx| this.save_edit(cx))),
                        )
                        .child(Button::new("edit-cancel").small().label("Cancel").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.editing = None;
                                cx.notify();
                            }),
                        ))
                }
                None => {
                    let (id_edit, id_delete, id_rearm) = (id.clone(), id.clone(), id.clone());
                    let fired = level.fired().is_some();
                    row.child(
                        div()
                            .w(px(110.))
                            .font_family(MONO)
                            .child(f::price(level.price)),
                    )
                    .child(div().w(px(60.)).child(level.direction.label()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(Palette::muted())
                            .child(level.note.clone().unwrap_or_default()),
                    )
                    .child(div().w(px(110.)).text_size(px(12.5)).child(state))
                    .when(fired, |d| {
                        d.child(
                            Button::new(("level-rearm", ix))
                                .small()
                                .label("Re-arm")
                                .disabled(!editable)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.rearm(id_rearm.clone(), cx)
                                })),
                        )
                    })
                    .child(
                        Button::new(("level-edit", ix))
                            .small()
                            .ghost()
                            .label("Edit")
                            .disabled(!editable)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_edit(id_edit.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new(("level-delete", ix))
                            .small()
                            .ghost()
                            .label("Delete")
                            .disabled(!editable)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete(id_delete.clone(), cx)
                            })),
                    )
                }
            };
            row.into_any_element()
        });
        ui::panel()
            .px_5()
            .py_4()
            .gap_2()
            .child(heading("CALL LEVELS"))
            .child(ui::label(
                "Each fires once when the price crosses it, judged on the minute's high and low, then waits to be re-armed. Armed levels are drawn on the Markets chart.",
            ))
            .children(rows)
            .child(div().pt_2().child(self.add.clone()))
            .into_any_element()
    }

    fn stops_panel(&self, editable: bool, cx: &mut Context<Self>) -> AnyElement {
        let rows = self
            .stop_tickers(cx)
            .into_iter()
            .enumerate()
            .map(|(ix, (ticker, setting))| {
                let on = setting.unwrap_or(true);
                let words = match setting {
                    None => "on (default)",
                    Some(true) => "on",
                    Some(false) => "off",
                };
                let t = ticker.clone();
                h_flex()
                    .gap_3()
                    .py(px(4.))
                    .child(div().flex_1().child(ticker.clone()))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(Palette::muted())
                            .child(words),
                    )
                    .child(
                        Switch::new(("stop", ix))
                            .checked(on)
                            .disabled(!editable)
                            .accessibility_label(format!("Stop alerts for {ticker}"))
                            .on_click(cx.listener(move |this, on: &bool, _, cx| {
                                this.set_stop_alerts(t.clone(), *on, cx)
                            })),
                    )
            });
        ui::panel()
            .px_5()
            .py_4()
            .gap_2()
            .child(heading("STOP ALERTS"))
            .child(ui::label(
                "Take-profit and stop-loss fills, by ticker. A ticker with no setting is on.",
            ))
            .children(rows)
            .into_any_element()
    }

    fn liquidation_panel(&self, editable: bool, cx: &mut Context<Self>) -> AnyElement {
        ui::panel()
            .px_5()
            .py_4()
            .gap_2()
            .child(heading("NEAR LIQUIDATION"))
            .child(
                h_flex()
                    .gap_3()
                    .child("Alert within")
                    .child(div().w(px(70.)).child(Input::new(&self.near_liq).small()))
                    .child("%")
                    .child(
                        Button::new("near-liq-save")
                            .small()
                            .label("Save")
                            .disabled(!editable)
                            .on_click(cx.listener(|this, _, _, cx| this.save_near_liq(cx))),
                    ),
            )
            .child(ui::label(LIQUIDATION_NOTE))
            .into_any_element()
    }

    fn history_panel(&self, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let alerts = self.snapshot(cx).alerts.clone();
        let empty = alerts.is_empty();
        let rows = alerts
            .into_iter()
            .take(50)
            .enumerate()
            .map(|(ix, a)| alert_row(ix, &a, now));
        ui::panel()
            .px_5()
            .py_4()
            .gap_1()
            .child(heading("ALERT HISTORY"))
            .when(empty, |d| d.child(ui::label("Nothing sent yet.")))
            .children(rows)
            .into_any_element()
    }
}

fn heading(text: &'static str) -> gpui_kit::Div {
    div()
        .text_size(px(12.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(Palette::muted())
        .child(text)
}

fn alert_row(ix: usize, a: &AlertRow, _now: u64) -> AnyElement {
    let tag = |text: &'static str, colour: gpui_kit::Hsla| {
        div()
            .px(px(6.))
            .rounded(px(4.))
            .text_size(px(11.))
            .font_family(MONO)
            .text_color(colour)
            .bg(colour.opacity(0.12))
            .child(text)
    };
    h_flex()
        .id(("alert", ix))
        .gap_3()
        .py(px(5.))
        .border_b_1()
        .border_color(Palette::line())
        .child(
            div()
                .w(px(90.))
                .flex_shrink_0()
                .font_family(MONO)
                .text_size(px(11.5))
                .text_color(Palette::muted())
                .child(clock(a.event_ms)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.5))
                .child(a.text.clone()),
        )
        .when(a.late, |d| {
            d.child(
                tag("late", Palette::amber())
                    .id(("alert-late", ix))
                    .test_support(),
            )
        })
        .child(if a.delivered {
            tag("delivered", Palette::gain())
                .id(("alert-delivered", ix))
                .test_support()
        } else {
            tag("not delivered", Palette::loss())
                .id(("alert-undelivered", ix))
                .test_support()
        })
        .test_support()
        .into_any_element()
}

/// A time as the user's clock shows it: `03:12` today, `19 Nov 03:12`
/// before.
pub fn clock(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    let Some(at) = Local.timestamp_millis_opt(ms as i64).single() else {
        return String::new();
    };
    if at.date_naive() == Local::now().date_naive() {
        at.format("%H:%M").to_string()
    } else {
        at.format("%-d %b %H:%M").to_string()
    }
}

/// What to tell the user when the watcher didn't save an edit.
pub fn outcome_message(e: &WatcherError) -> String {
    match e {
        WatcherError::Changed { .. } => {
            "Not saved: the configuration changed elsewhere. The newer one is shown; make the change again if you still want it.".into()
        }
        WatcherError::Refused { problems } => format!("Not saved. The watcher found: {}", problems.join("; ")),
        other => format!("Not saved. {other}"),
    }
}

impl gpui_kit::Render for WatcherSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = now_ms();
        let editable = self.snapshot(cx).editable().is_some();
        let banner = self.stale_banner(now, cx);
        div()
            .id("watcher-section")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .p_5()
                    .gap_4()
                    .when_some(banner, |d, text| d.child(ui::notice(text)))
                    .children(self.message.clone().map(|(text, problem)| {
                        div()
                            .id("watcher-message")
                            .text_size(px(12.5))
                            .text_color(if problem {
                                Palette::loss()
                            } else {
                                Palette::muted()
                            })
                            .child(text)
                    }))
                    .child(self.health_panel(now, cx))
                    .child(self.levels_panel(editable, cx))
                    .child(
                        h_flex()
                            .gap_4()
                            .items_start()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.stops_panel(editable, cx)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.liquidation_panel(editable, cx)),
                            ),
                    )
                    .child(self.history_panel(now, cx)),
            )
            .test_support()
    }
}

#[cfg(test)]
impl WatcherSection {
    pub fn add_form(&self) -> Entity<LevelForm> {
        self.add.clone()
    }

    pub fn message(&self) -> Option<(String, bool)> {
        self.message.as_ref().map(|(t, p)| (t.to_string(), *p))
    }

    pub fn health_text(&self, cx: &gpui_kit::App) -> (String, bool) {
        self.health_state(now_ms(), cx)
    }

    pub fn notice_other_account(&self, cx: &gpui_kit::App) -> Option<String> {
        self.other_account(cx)
    }

    pub fn banner(&self, cx: &gpui_kit::App) -> Option<String> {
        self.stale_banner(now_ms(), cx)
    }

    pub fn stop_rows(&self, cx: &gpui_kit::App) -> Vec<(String, Option<bool>)> {
        self.stop_tickers(cx)
    }

    pub fn set_near_liq_text(&self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.near_liq
            .update(cx, |i, cx| i.set_value(text.to_string(), window, cx));
    }

    pub fn set_edit_text(
        &self,
        price: &str,
        note: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(e) = &self.editing {
            e.price
                .update(cx, |i, cx| i.set_value(price.to_string(), window, cx));
            e.note
                .update(cx, |i, cx| i.set_value(note.to_string(), window, cx));
        }
    }
}
