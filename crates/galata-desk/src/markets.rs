//! The Markets section: the watched markets as a grid of heatmap tiles.
//! Read-only: it subscribes to public market data and nothing else.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Local};
use galata_desk_core::format as f;
use galata_desk_core::market::feed::{
    Connection, Listed, Listing, Live, MarketFeed, MarketSnapshot, Tile,
};
use galata_desk_core::market::watchlist::{AddError, LIMIT, Watchlist};
use galata_desk_core::market::{MarketId, MarketPeriod};
use galata_desk_core::model::PositionRow;
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::venue::Http;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::chart::LineChart;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Bounds, Context, Entity, FocusHandle, KeyDownEvent, Pixels, SharedString,
    Subscription, Task, TestSupportExt, Window, canvas, deferred, div, px,
};

use crate::Core;
use crate::market_tile::{self, ColourBy};
use crate::portfolio::Portfolio;
use crate::theme::{MONO, Palette};
use crate::ui;

/// A tile is at least this wide; the grid fits as many columns as that allows.
const TILE_MIN: f32 = 196.;
const TILE_HEIGHT: f32 = 136.;
const GAP: f32 = 8.;
/// The detail card's size, for deciding which side of its tile it opens on.
const CARD_WIDTH: f32 = 276.;
const CARD_HEIGHT: f32 = 286.;
/// How long to wait for a first connection before saying it has not come.
const PATIENCE: Duration = Duration::from_secs(5);

pub struct Markets {
    portfolio: Entity<Portfolio>,
    feed: MarketFeed,
    snapshot: MarketSnapshot,
    /// `None` until the default list can be decided, which needs the
    /// account's positions.
    watchlist: Option<Watchlist>,
    period: MarketPeriod,
    colour: ColourBy,
    /// The tile the pointer rests on.
    hovered: Option<MarketId>,
    /// One focus handle per tile, so the keyboard can reach each.
    focus: HashMap<MarketId, FocusHandle>,
    /// Where each tile was last drawn, for placing its detail card.
    bounds: Rc<RefCell<HashMap<MarketId, Bounds<Pixels>>>>,
    picker_open: bool,
    search: Entity<InputState>,
    /// Something the user should know that is not about the venue.
    warning: Option<SharedString>,
    opened_at: Instant,
    _watch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Markets {
    pub fn new(portfolio: Entity<Portfolio>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let core = Core::get(cx);
        let stored = core.store.watchlist();
        let watched: Vec<MarketId> = stored
            .as_ref()
            .map(|w| w.markets().to_vec())
            .unwrap_or_default();
        // The feed's tasks run on the I/O runtime.
        let feed = {
            let _io = core.runtime.enter();
            MarketFeed::live(
                Http::at(core.info_url.clone()),
                core.ws_url.clone(),
                MarketPeriod::default(),
                watched,
            )
        };
        let mut snapshots = feed.snapshots();
        let watch = cx.spawn(async move |this, cx| {
            while snapshots.changed().await.is_ok() {
                let snapshot = snapshots.borrow_and_update().clone();
                let applied = this.update(cx, |this, cx| {
                    this.snapshot = snapshot;
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        });

        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search markets"));
        let mut subscriptions = vec![
            // Typing in the search field redraws the list.
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        // The portfolio's positions decide the default list, and the held marks.
        subscriptions.push(cx.observe(&portfolio, |this, _, cx| {
            this.decide_default(cx);
            cx.notify();
        }));

        // Redraw once patience runs out, so "cannot reach the venue" can show.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(PATIENCE + Duration::from_millis(100))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();

        #[allow(unused_mut)]
        let mut this = Self {
            portfolio,
            feed,
            snapshot: MarketSnapshot::default(),
            watchlist: stored,
            period: MarketPeriod::default(),
            colour: ColourBy::default(),
            hovered: None,
            focus: HashMap::new(),
            bounds: Rc::default(),
            picker_open: false,
            search,
            warning: None,
            opened_at: Instant::now(),
            _watch: watch,
            _subscriptions: subscriptions,
        };
        this.decide_default(cx);
        #[cfg(feature = "shot")]
        this.preselect();
        this
    }

    /// `GALATA_DESK_SHOT_STATE=section=markets,mperiod=week,colour=funding,mopen=xyz:NVDA,picker=1`
    #[cfg(feature = "shot")]
    fn preselect(&mut self) {
        let state = std::env::var("GALATA_DESK_SHOT_STATE").unwrap_or_default();
        for (key, value) in state.split(',').filter_map(|kv| kv.split_once('=')) {
            match key {
                "mperiod" => {
                    let period = match value {
                        "hour" => MarketPeriod::Hour,
                        "week" => MarketPeriod::Week,
                        _ => MarketPeriod::Day,
                    };
                    self.period = period;
                    self.feed.set_period(period);
                }
                "colour" => {
                    self.colour = match value {
                        "volatility" => ColourBy::Volatility,
                        "funding" => ColourBy::Funding,
                        _ => ColourBy::Change,
                    }
                }
                "mopen" => self.hovered = Some(MarketId::new(value)),
                "picker" => self.picker_open = true,
                _ => {}
            }
        }
    }

    /// With no remembered watchlist, start from what the account holds, once
    /// its positions are known.
    fn decide_default(&mut self, cx: &mut Context<Self>) {
        if self.watchlist.is_some() {
            return;
        }
        let Some(list) = self
            .portfolio
            .read(cx)
            .positions()
            .map(Watchlist::default_for)
        else {
            return;
        };
        for market in list.markets() {
            self.feed.watch(market.clone());
        }
        self.watchlist = Some(list);
        self.save(cx);
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(list) = &self.watchlist
            && let Err(error) = Core::get(cx).store.save_watchlist(list)
        {
            self.warning = Some(format!("The watchlist could not be saved: {error}").into());
        }
    }

    fn add(&mut self, market: MarketId, cx: &mut Context<Self>) {
        let Some(list) = &mut self.watchlist else {
            return;
        };
        match list.add(market.clone()) {
            Ok(()) => {
                self.feed.watch(market);
                self.save(cx);
            }
            // The list shows both states; choosing such a row adds nothing.
            Err(AddError::AlreadyWatched | AddError::Full) => {}
        }
        cx.notify();
    }

    fn remove(&mut self, market: &MarketId, cx: &mut Context<Self>) {
        let Some(list) = &mut self.watchlist else {
            return;
        };
        if list.remove(market) {
            self.feed.unwatch(market.clone());
            self.focus.remove(market);
            self.bounds.borrow_mut().remove(market);
            if self.hovered.as_ref() == Some(market) {
                self.hovered = None;
            }
            self.save(cx);
        }
        cx.notify();
    }

    fn set_period(&mut self, period: MarketPeriod, cx: &mut Context<Self>) {
        self.period = period;
        self.feed.set_period(period);
        cx.notify();
    }

    fn toggle_picker(&mut self, cx: &mut Context<Self>) {
        self.picker_open = !self.picker_open;
        if self.picker_open {
            // The list may have failed to load, or grown old.
            self.feed.refresh_listing();
        }
        cx.notify();
    }

    /// The account's position in a market, if it holds one.
    fn position(&self, market: &MarketId, cx: &Context<Self>) -> Option<PositionRow> {
        self.portfolio
            .read(cx)
            .positions()?
            .iter()
            .find(|p| p.dex == market.dex() && p.market == market.ticker())
            .cloned()
    }

    /// Whether the feed has failed to connect for long enough to say so.
    fn unreachable(&self) -> bool {
        self.snapshot.connection == Connection::Connecting && self.opened_at.elapsed() >= PATIENCE
    }

    /// The Markets section's part of the title bar: whether it is live.
    pub fn status(&mut self, _: &mut Context<Self>) -> AnyElement {
        let (text, colour) = match self.snapshot.connection {
            Connection::Live => ("Live".to_string(), Palette::gain()),
            Connection::Down { .. } => ("Not live".to_string(), Palette::amber()),
            Connection::Connecting if self.unreachable() => {
                ("Not connected".to_string(), Palette::amber())
            }
            Connection::Connecting => ("Connecting\u{2026}".to_string(), Palette::faint()),
        };
        h_flex()
            .gap_2()
            .child(div().size(px(7.)).rounded_full().bg(colour))
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .text_color(Palette::muted())
                    .child(text),
            )
            .into_any_element()
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.watchlist.as_ref().map(|w| w.markets().len());
        let period = TabBar::new("mperiod")
            .segmented()
            .xsmall()
            .selected_index(
                MarketPeriod::ALL
                    .iter()
                    .position(|p| *p == self.period)
                    .unwrap_or(1),
            )
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.set_period(MarketPeriod::ALL[*ix], cx);
            }))
            .children(MarketPeriod::ALL.map(MarketPeriod::label));
        let colour = TabBar::new("colour")
            .segmented()
            .xsmall()
            .selected_index(
                ColourBy::ALL
                    .iter()
                    .position(|c| *c == self.colour)
                    .unwrap_or(0),
            )
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.colour = ColourBy::ALL[*ix];
                cx.notify();
            }))
            .children(ColourBy::ALL.map(ColourBy::label));

        h_flex()
            .flex_wrap()
            .gap_x_6()
            .gap_y_2()
            .items_center()
            .child(
                h_flex()
                    .gap_3()
                    .items_baseline()
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Markets"),
                    )
                    .children(count.map(|n| ui::label(format!("{n} watched")).text_size(px(13.)))),
            )
            .child(div().flex_1())
            .child(h_flex().gap_2().child(ui::label("Period")).child(period))
            .child(h_flex().gap_2().child(ui::label("Colour by")).child(colour))
            .child(
                div()
                    .relative()
                    .child(
                        Button::new("add-market")
                            .small()
                            .label("+ Add market")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(cx))),
                    )
                    .when(self.picker_open, |d| {
                        d.child(deferred(self.picker(cx)).with_priority(2))
                    }),
            )
            .into_any_element()
    }

    fn legend(&self) -> AnyElement {
        let (low, steps, high) = market_tile::legend(self.colour, self.period);
        h_flex()
            .flex_wrap()
            .gap_x_5()
            .gap_y_1()
            .text_size(px(12.))
            .text_color(Palette::muted())
            .child(
                h_flex()
                    .gap_2()
                    .child(low)
                    .child(
                        h_flex().gap(px(2.)).children(
                            steps
                                .into_iter()
                                .map(|c| div().w(px(26.)).h(px(10.)).rounded(px(2.)).bg(c)),
                        ),
                    )
                    .child(high),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().size(px(7.)).rounded_full().bg(Palette::amber()))
                    .child("You hold a position"),
            )
            .child("Bar: where the last price sits between the period's low and high")
            .into_any_element()
    }

    /// The add-market list, opened from the add button.
    fn picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let full = self.watchlist.as_ref().is_some_and(Watchlist::is_full);
        let query = self.search.read(cx).value().trim().to_lowercase();

        let body: AnyElement = match &self.snapshot.listing {
            Listing::Loading => div()
                .p_4()
                .text_color(Palette::muted())
                .child("Loading the list of markets\u{2026}")
                .into_any_element(),
            Listing::Failed => v_flex()
                .p_4()
                .gap_3()
                .child(
                    div()
                        .text_color(Palette::amber_text())
                        .child("The list of markets could not be loaded."),
                )
                .child(
                    h_flex().child(
                        Button::new("retry-listing")
                            .small()
                            .label("Retry")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.feed.refresh_listing();
                                cx.notify();
                            })),
                    ),
                )
                .into_any_element(),
            Listing::Ready(listed) => {
                let matches: Vec<(usize, &Listed)> = listed
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.market.coin().to_lowercase().contains(&query))
                    .collect();
                if matches.is_empty() {
                    div()
                        .p_4()
                        .text_color(Palette::muted())
                        .child("No market matches.")
                        .into_any_element()
                } else {
                    let mut rows: Vec<AnyElement> = Vec::with_capacity(matches.len() + 12);
                    let mut dex: Option<&str> = None;
                    for (ix, listed) in matches {
                        // A heading where each dex begins.
                        if dex != Some(listed.market.dex()) {
                            dex = Some(listed.market.dex());
                            let name = if listed.market.dex().is_empty() {
                                "Main dex".to_string()
                            } else {
                                listed.market.dex().to_string()
                            };
                            rows.push(
                                div()
                                    .px_3()
                                    .pt_2()
                                    .pb_1()
                                    .text_size(px(11.))
                                    .text_color(Palette::faint())
                                    .child(name)
                                    .into_any_element(),
                            );
                        }
                        let market = listed.market.clone();
                        let watched = self.watchlist.as_ref().is_some_and(|w| w.contains(&market));
                        let blocked = watched || full;
                        rows.push(
                            h_flex()
                                .id(("pick", ix))
                                .px_3()
                                .h(px(30.))
                                .gap_2()
                                .text_size(px(13.))
                                .when(!blocked, |d| {
                                    d.cursor_pointer()
                                        .hover(|s| s.bg(Palette::raise()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.add(market.clone(), cx);
                                        }))
                                })
                                .when(blocked, |d| d.text_color(Palette::faint()))
                                .child(
                                    div()
                                        .font_family(MONO)
                                        .child(listed.market.ticker().to_string()),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(Palette::faint())
                                        .child(if watched {
                                            "Watched".to_string()
                                        } else {
                                            format!("up to {}x", listed.max_leverage)
                                        }),
                                )
                                .test_support()
                                .into_any_element(),
                        );
                    }
                    v_flex()
                        .id("pick-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .pb_2()
                        .children(rows)
                        .into_any_element()
                }
            }
        };

        ui::panel()
            .id("picker")
            .occlude()
            .absolute()
            .top(px(36.))
            .right_0()
            .w(px(320.))
            .h(px(440.))
            .border_color(Palette::amber_line())
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.picker_open = false;
                cx.notify();
            }))
            .child(
                div()
                    .p_2()
                    .child(Input::new(&self.search).small().cleanable(true)),
            )
            .when(full, |d| {
                d.child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_size(px(12.5))
                        .text_color(Palette::amber_text())
                        .child(format!(
                            "The watchlist is full ({LIMIT} markets). Remove one to add another."
                        )),
                )
            })
            .child(body)
            .into_any_element()
    }

    /// One market's tile, and its detail card when it is pointed at or focused.
    fn tile(
        &mut self,
        ix: usize,
        tile: &Tile,
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
        let open = focused || self.hovered.as_ref() == Some(&market);
        let position = self.position(&market, cx);

        let header = h_flex()
            .gap(px(6.))
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(market.ticker().to_string()),
            )
            .when(!market.dex().is_empty(), |d| {
                d.child(
                    div()
                        .px(px(5.))
                        .rounded(px(4.))
                        .bg(Palette::ground().opacity(0.45))
                        .font_family(MONO)
                        .text_size(px(10.5))
                        .child(market.dex().to_string()),
                )
            })
            .when(position.is_some(), |d| {
                d.child(div().size(px(7.)).rounded_full().bg(Palette::amber()))
            })
            .child(div().flex_1());

        let soft = gpui_kit::rgb(0xd8d3c8);
        let body: AnyElement = if tile.unavailable {
            v_flex()
                .flex_1()
                .justify_between()
                .child(header)
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(Palette::muted())
                        .child("Unavailable: the venue no longer lists this market."),
                )
                .child(h_flex().child({
                    let market = market.clone();
                    Button::new(("remove", ix))
                        .xsmall()
                        .label("Remove")
                        .on_click(cx.listener(move |this, _, _, cx| this.remove(&market, cx)))
                }))
                .into_any_element()
        } else {
            let pending = |w: f32, h: f32| ui::placeholder(px(w), px(h)).into_any_element();
            let stats = tile.stats.as_ref();
            v_flex()
                .flex_1()
                .gap_2()
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
                        // The remove action shows while the tile is pointed
                        // at or focused, at the end of the header.
                        .when(open, |d| d.child(remove_button(ix, &market, cx))),
                )
                .child(match &tile.live {
                    Some(live) => ui::figure(f::price(live.last), px(21.)).into_any_element(),
                    None => pending(110., 24.),
                })
                .child(match stats {
                    Some(s) => v_flex()
                        .gap(px(4.))
                        .child(
                            div()
                                .relative()
                                .h(px(4.))
                                .rounded(px(2.))
                                .bg(Palette::ground().opacity(0.5))
                                .child(
                                    div()
                                        .absolute()
                                        .top(px(-3.))
                                        .left(gpui_kit::relative(s.marker as f32))
                                        .ml(px(-1.))
                                        .w(px(2.))
                                        .h(px(10.))
                                        .rounded(px(1.))
                                        .bg(Palette::text()),
                                ),
                        )
                        .child(
                            h_flex()
                                .justify_between()
                                .font_family(MONO)
                                .text_size(px(11.))
                                .text_color(soft)
                                .child(f::price(s.low))
                                .child(f::price(s.high)),
                        )
                        .into_any_element(),
                    None => pending(170., 22.),
                })
                .child(
                    h_flex()
                        .justify_between()
                        .text_size(px(11.5))
                        .text_color(soft)
                        .child("Volatility")
                        .child(match stats {
                            Some(s) => div()
                                .font_family(MONO)
                                .child(f::volatility(s.volatility))
                                .into_any_element(),
                            None => pending(34., 12.),
                        }),
                )
                .into_any_element()
        };

        // Where the card opens: below and left-aligned, unless that would
        // leave the window, by where the tile was last drawn.
        let viewport = window.viewport_size();
        let drawn = self.bounds.borrow().get(&market).copied();
        let above = drawn.is_some_and(|b| b.bottom() + px(CARD_HEIGHT + 6.) > viewport.height);
        let right = drawn.is_some_and(|b| b.left() + px(CARD_WIDTH) > viewport.width);

        let record = {
            let bounds = self.bounds.clone();
            let market = market.clone();
            canvas(
                move |drawn, _, _| {
                    bounds.borrow_mut().insert(market.clone(), drawn);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        };

        let hover_market = market.clone();
        let key_market = market.clone();
        div()
            .id(("tile", ix))
            .relative()
            .h(px(TILE_HEIGHT))
            .track_focus(&handle)
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered = Some(hover_market.clone());
                } else if this.hovered.as_ref() == Some(&hover_market) {
                    this.hovered = None;
                }
                cx.notify();
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                // A focused tile is removed with Delete or Backspace.
                if matches!(event.keystroke.key.as_str(), "backspace" | "delete") {
                    this.remove(&key_market, cx);
                }
            }))
            .child(record)
            .child(
                v_flex()
                    .size_full()
                    .px_3()
                    .pt(px(11.))
                    .pb(px(10.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if open {
                        Palette::amber()
                    } else {
                        Palette::text().opacity(0.08)
                    })
                    .bg(market_tile::background(self.colour, self.period, tile))
                    .child(body),
            )
            .when(open && !tile.unavailable, |d| {
                let card = self.card(tile, position.as_ref());
                d.child(
                    deferred(
                        div()
                            .absolute()
                            .w(px(CARD_WIDTH))
                            .when(above, |d| d.bottom(px(TILE_HEIGHT + 6.)))
                            .when(!above, |d| d.top(px(TILE_HEIGHT + 6.)))
                            .when(right, |d| d.right_0())
                            .when(!right, |d| d.left_0())
                            .child(card),
                    )
                    .with_priority(1),
                )
            })
            .test_support()
            .into_any_element()
    }

    /// The detail card for one market. It takes no pointer events: moving on
    /// to the next tile must not be blocked by the card of the last.
    fn card(&self, tile: &Tile, position: Option<&PositionRow>) -> AnyElement {
        let market = &tile.market;
        let row = |name: &'static str, value: String, tone: gpui_kit::Hsla| {
            h_flex()
                .justify_between()
                .text_size(px(12.5))
                .child(div().text_color(Palette::muted()).child(name))
                .child(div().font_family(MONO).text_color(tone).child(value))
        };
        let plain = Palette::text();
        let dash = "\u{2014}".to_string();
        let live: Option<&Live> = tile.live.as_ref();
        let funding_tone = live.map(|l| ui::tone_of(sign(l.funding))).unwrap_or(plain);

        let name = if market.dex().is_empty() {
            market.ticker().to_string()
        } else {
            format!("{} \u{b7} {}", market.ticker(), market.dex())
        };

        ui::panel()
            .w_full()
            .p_3()
            .gap(px(6.))
            .border_color(Palette::amber_line())
            .shadow_lg()
            .child(
                h_flex()
                    .justify_between()
                    .items_baseline()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .children(tile.max_leverage.map(|l| {
                        div()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(Palette::muted())
                            .child(format!("up to {l}x"))
                    })),
            )
            .child(
                div().h(px(44.)).w_full().children(
                    tile.stats
                        .as_ref()
                        .filter(|s| s.spark.len() >= 2)
                        .map(|s| sparkline(&s.spark)),
                ),
            )
            .child(row(
                "Mark",
                live.map(|l| f::price(l.mark)).unwrap_or(dash.clone()),
                plain,
            ))
            .child(row(
                "Oracle",
                live.map(|l| f::price(l.oracle)).unwrap_or(dash.clone()),
                plain,
            ))
            .child(row(
                "Premium",
                live.map(|l| f::premium(l.premium)).unwrap_or(dash.clone()),
                plain,
            ))
            .child(row(
                "Funding (1h)",
                live.map(|l| f::funding_hourly(l.funding))
                    .unwrap_or(dash.clone()),
                funding_tone,
            ))
            .child(row(
                "Funding (yearly)",
                live.map(|l| f::funding_yearly(l.funding))
                    .unwrap_or(dash.clone()),
                funding_tone,
            ))
            .child(row(
                "24h volume",
                live.map(|l| f::usd_abbrev(l.volume_usd))
                    .unwrap_or(dash.clone()),
                plain,
            ))
            .child(row(
                "Open interest",
                live.map(|l| f::usd_abbrev(l.open_interest_usd))
                    .unwrap_or(dash),
                plain,
            ))
            .children(position.map(|p| {
                h_flex()
                    .justify_between()
                    .mt_1()
                    .pt_2()
                    .border_t_1()
                    .border_color(Palette::line())
                    .text_size(px(12.5))
                    .child(
                        div()
                            .text_color(Palette::amber_text())
                            .child("Your position"),
                    )
                    .child(div().font_family(MONO).child(signed_size(p)))
            }))
            .into_any_element()
    }

    fn notices(&self) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if let Some(warning) = &self.warning {
            out.push(ui::notice(warning.clone()).into_any_element());
        }
        match self.snapshot.connection {
            Connection::Down { since } => out.push(
                ui::notice(format!(
                    "Not live since {}. The figures below are the last received; reconnecting.",
                    clock(since)
                ))
                .into_any_element(),
            ),
            Connection::Connecting if self.unreachable() => out.push(
                ui::notice("Hyperliquid's market feed cannot be reached. The app keeps trying.")
                    .into_any_element(),
            ),
            _ => {}
        }
        out
    }
}

/// The remove action on a tile.
fn remove_button(ix: usize, market: &MarketId, cx: &mut Context<Markets>) -> Button {
    let market = market.clone();
    Button::new(("remove", ix))
        .ghost()
        .xsmall()
        .label("\u{d7}")
        .accessibility_label(format!("Remove {}", market.ticker()))
        .tooltip("Remove from the watchlist")
        .on_click(cx.listener(move |this, _, _, cx| this.remove(&market, cx)))
}

impl Render for Markets {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let padding = 12.;
        let width = f32::from(window.viewport_size().width) - 2. * padding;
        let columns = (((width + GAP) / (TILE_MIN + GAP)).floor() as u16).max(1);

        let body: AnyElement = match &self.watchlist {
            // The default list needs the account's positions.
            None => h_flex()
                .size_full()
                .justify_center()
                .text_color(Palette::muted())
                .child("Waiting for the account's positions to choose the markets to watch\u{2026}")
                .into_any_element(),
            Some(list) if list.is_empty() => v_flex()
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
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.picker_open {
                                this.toggle_picker(cx);
                            }
                        })),
                )
                .into_any_element(),
            Some(list) => {
                // A tile for every watched market, at once: one the feed has
                // not reported yet is all placeholders.
                let tiles: Vec<Tile> = list
                    .markets()
                    .iter()
                    .map(|market| {
                        self.snapshot
                            .tiles
                            .iter()
                            .find(|t| &t.market == market)
                            .cloned()
                            .unwrap_or(Tile {
                                market: market.clone(),
                                live: None,
                                stats: None,
                                max_leverage: None,
                                unavailable: false,
                            })
                    })
                    .collect();
                let mut grid = div().grid().grid_cols(columns).gap(px(GAP));
                for (ix, tile) in tiles.iter().enumerate() {
                    grid = grid.child(self.tile(ix, tile, window, cx));
                }
                div()
                    .id("tiles")
                    .size_full()
                    .overflow_y_scroll()
                    .child(grid)
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .p(px(padding))
            .gap_3()
            .child(self.toolbar(cx))
            .child(self.legend())
            .children(self.notices())
            .child(div().flex_1().min_h_0().child(body))
    }
}

fn sign(value: Decimal) -> i32 {
    if value > Decimal::ZERO {
        1
    } else if value < Decimal::ZERO {
        -1
    } else {
        0
    }
}

/// A position's size with its sign always shown: `+123.467 NVDA`.
fn signed_size(position: &PositionRow) -> String {
    let size = f::size(position.size, &position.market);
    if position.size > Decimal::ZERO {
        format!("+{size}")
    } else {
        size
    }
}

/// A time of day on this machine's clock: `04:21:07`.
fn clock(at: SystemTime) -> String {
    DateTime::<Local>::from(at).format("%H:%M:%S").to_string()
}

/// The period's prices as a bare line: no axes, no grid, no tooltip.
fn sparkline(points: &[f64]) -> impl IntoElement + use<> {
    let low = points.iter().copied().fold(f64::INFINITY, f64::min);
    let high = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let air = ((high - low) * 0.1).max(high.abs() * 1e-6).max(1e-9);
    let data: Vec<(SharedString, f64)> = points
        .iter()
        .enumerate()
        .map(|(i, v)| (SharedString::from(i.to_string()), *v))
        .collect();
    LineChart::new(data)
        .id("spark")
        .appear(false)
        .interactive(false)
        .x(|p: &(SharedString, f64)| p.0.clone())
        .y(|p: &(SharedString, f64)| p.1)
        .linear()
        .stroke(Palette::amber())
        .x_axis(false)
        .y_axis(false)
        .grid(false)
        .y_domain(low - air, high + air)
}

#[cfg(test)]
impl Markets {
    /// Replace what the feed has reported, for tests that have no venue.
    pub fn set_snapshot(&mut self, snapshot: MarketSnapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        cx.notify();
    }

    pub fn watched(&self) -> Vec<String> {
        self.watchlist
            .as_ref()
            .map(|w| w.markets().iter().map(|m| m.to_string()).collect())
            .unwrap_or_default()
    }

    pub fn selections(&self) -> String {
        format!(
            "period={} colour={}",
            self.period.label(),
            self.colour.label()
        )
    }

    pub fn open_market(&self, window: &Window) -> Option<String> {
        self.focus
            .iter()
            .find(|(_, handle)| handle.is_focused(window))
            .map(|(market, _)| market.to_string())
            .or_else(|| self.hovered.as_ref().map(|m| m.to_string()))
    }

    pub fn picker_is_open(&self) -> bool {
        self.picker_open
    }

    /// Put the keyboard focus on a market's tile.
    pub fn focus_market(&self, market: &str, window: &mut Window, cx: &mut Context<Self>) {
        let handle = &self.focus[&MarketId::new(market)];
        window.focus(handle, cx);
        cx.notify();
    }

    /// Type into the add-market search field.
    pub fn set_search(&self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
        cx.notify();
    }
}
