//! The Markets panels as dock panels: their names in a saved layout, what
//! each writes into it, and how each is built again from what it wrote.
//!
//! A panel's saved state is a [`PanelRecord`]: a stable key, its kind, its
//! market and period where it has them, and its link settings. The layout
//! around it (splits, tabs, sizes) is the dock's own.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use galata_desk_core::market::{MarketId, MarketPeriod};
use gpui_kit::component::dock::{
    BasePanel, BasePanelView, DockArea, Panel, PanelBuildContext, PanelEvent, PanelInfo,
    PanelState, panel_handle, register_panel,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, Global, Render,
    WeakEntity, Window, div,
};
use serde::{Deserialize, Serialize};

use super::blotter::{BlotterPanel, BlotterTab};
use super::chart::ChartPanel;
use super::detail::DetailPanel;
use super::links::{Delivered, LinkBus, LinkEvent};
use super::model::{MarketSource, MarketsModel};
use super::position::PositionPanel;
use super::ticket::TicketPanel;
use super::tiles::{Outline, TilesPanel};

/// The names panels are saved under. Once chosen, never changed: a saved
/// layout finds its panels by them.
pub const CHART: &str = "markets.chart";
pub const TILES: &str = "markets.tiles";
pub const DETAIL: &str = "markets.detail";
pub const POSITION: &str = "markets.position";
pub const TICKET: &str = "markets.ticket";
pub const BLOTTER: &str = "markets.blotter";

/// What a panel is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelKind {
    Chart,
    Tiles,
    Detail,
    Position,
    Ticket,
    Blotter,
}

impl PanelKind {
    pub fn panel_name(self) -> &'static str {
        match self {
            Self::Chart => CHART,
            Self::Tiles => TILES,
            Self::Detail => DETAIL,
            Self::Position => POSITION,
            Self::Ticket => TICKET,
            Self::Blotter => BLOTTER,
        }
    }
}

/// A panel's part in one link channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    Off,
    Listen,
    Send,
    Both,
}

/// What a sending panel may tell its channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    SetMarket,
    TimeCursor,
    LimitPrice,
    FilterBlotter,
}

/// A market as saved: by dex and ticker, as the watchlist file has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedMarket {
    pub dex: String,
    pub ticker: String,
}

impl From<&MarketId> for SavedMarket {
    fn from(market: &MarketId) -> Self {
        Self {
            dex: market.dex().to_string(),
            ticker: market.ticker().to_string(),
        }
    }
}

impl From<&SavedMarket> for MarketId {
    fn from(saved: &SavedMarket) -> Self {
        MarketId::from_parts(&saved.dex, &saved.ticker)
    }
}

/// A period as saved: its label in lower case, `24h` for a day.
pub fn period_name(period: MarketPeriod) -> String {
    period.label().to_lowercase()
}

/// The period a saved name stands for, if it stands for one.
pub fn period_named(name: &str) -> Option<MarketPeriod> {
    MarketPeriod::ALL
        .into_iter()
        .find(|p| p.label().eq_ignore_ascii_case(name))
}

/// What a panel writes into a saved layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelRecord {
    pub key: String,
    pub kind: PanelKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market: Option<SavedMarket>,
    /// The chart's period, which sets its candle interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// The blotter's tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
    /// The panel's role in channels A, B and C.
    #[serde(default)]
    pub roles: [Role; 3],
    #[serde(default)]
    pub actions: Vec<Action>,
}

impl PanelRecord {
    /// A new panel's: a fresh key, no tab and no links.
    pub fn fresh(kind: PanelKind, market: Option<&MarketId>, period: Option<MarketPeriod>) -> Self {
        Self {
            key: new_key(),
            kind,
            market: market.map(SavedMarket::from),
            interval: period.map(period_name),
            tab: None,
            roles: [Role::Off; 3],
            actions: Vec::new(),
        }
    }

    pub fn market(&self) -> Option<MarketId> {
        self.market.as_ref().map(MarketId::from)
    }

    pub fn period(&self) -> Option<MarketPeriod> {
        self.interval.as_deref().and_then(period_named)
    }

    /// The panel state the dock saves.
    pub fn to_state(&self) -> PanelState {
        let mut state = PanelState::new(self.kind.panel_name());
        state.info = PanelInfo::Panel(serde_json::to_value(self).unwrap_or_default());
        state
    }

    /// The record a saved panel state carries, if it is one of these.
    pub fn from_state(state: &PanelState) -> Option<Self> {
        match &state.info {
            PanelInfo::Panel(value) => serde_json::from_value(value.clone()).ok(),
            _ => None,
        }
    }
}

/// What a panel keeps for the workspace: its key, its links, and the focus
/// the dock moves to it. A windowed panel has one too, unsaved.
pub struct DockMeta {
    /// The dock area the panel is in, for its own Close; none in the fixed
    /// layout.
    pub area: Option<WeakEntity<DockArea>>,
    /// The workspace's link channels, once the panel has joined them.
    pub bus: Option<Entity<LinkBus>>,
    /// What keeps it joined: its deliveries, and leaving when it closes.
    pub links: Vec<gpui_kit::Subscription>,
    pub key: String,
    pub roles: [Role; 3],
    pub actions: Vec<Action>,
    pub focus: FocusHandle,
}

impl DockMeta {
    /// A new panel's: a fresh key and no links.
    pub fn fresh(cx: &mut App) -> Self {
        Self {
            area: None,
            bus: None,
            links: Vec::new(),
            key: new_key(),
            roles: [Role::Off; 3],
            actions: Vec::new(),
            focus: cx.focus_handle(),
        }
    }

    /// A saved panel's, as it was saved.
    pub fn restored(record: &PanelRecord, area: WeakEntity<DockArea>, cx: &mut App) -> Self {
        Self {
            area: Some(area),
            bus: None,
            links: Vec::new(),
            key: record.key.clone(),
            roles: record.roles,
            actions: record.actions.clone(),
            focus: cx.focus_handle(),
        }
    }

    /// Tell the panels linked to this one about `event`. Only a user's input
    /// calls this.
    pub fn publish(&self, event: LinkEvent, cx: &mut App) {
        if let Some(bus) = self.bus.clone() {
            let key = self.key.clone();
            bus.update(cx, |b, cx| b.publish(&key, event, cx));
        }
    }

    /// The record of a panel of `kind` showing `market` over `period`.
    pub fn record(
        &self,
        kind: PanelKind,
        market: Option<&MarketId>,
        period: Option<MarketPeriod>,
    ) -> PanelRecord {
        PanelRecord {
            key: self.key.clone(),
            kind,
            market: market.map(SavedMarket::from),
            interval: period.map(period_name),
            tab: None,
            roles: self.roles,
            actions: self.actions.clone(),
        }
    }
}

/// A key no other panel has: the time and a count, in hex.
fn new_key() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_default();
    format!("{nanos:x}-{:x}", COUNT.fetch_add(1, Ordering::Relaxed))
}

/// Which model each dock area's panels are built on. The registry's builders
/// are global; the model is the window's.
#[derive(Default)]
struct Models(HashMap<EntityId, (WeakEntity<MarketsModel>, WeakEntity<LinkBus>)>);

impl Global for Models {}

/// Build `area`'s panels on `model` when a layout is loaded into it.
pub fn bind(
    area: &Entity<DockArea>,
    model: &Entity<MarketsModel>,
    bus: &Entity<LinkBus>,
    cx: &mut App,
) {
    cx.default_global::<Models>()
        .0
        .insert(area.entity_id(), (model.downgrade(), bus.downgrade()));
}

/// Teach the dock to build each Markets panel from what it saved.
pub fn register(cx: &mut App) {
    for kind in [
        PanelKind::Chart,
        PanelKind::Tiles,
        PanelKind::Detail,
        PanelKind::Position,
        PanelKind::Ticket,
        PanelKind::Blotter,
    ] {
        register_panel(cx, kind.panel_name(), move |context, window, cx| {
            build(kind, context, window, cx)
        });
    }
}

fn build(
    kind: PanelKind,
    context: PanelBuildContext,
    window: &mut Window,
    cx: &mut App,
) -> Arc<dyn BasePanelView> {
    let bound = cx
        .try_global::<Models>()
        .and_then(|models| models.0.get(&context.dock_area().entity_id()))
        .and_then(|(model, bus)| Some((model.upgrade()?, bus.upgrade()?)));
    let record = PanelRecord::from_state(context.state()).filter(|r| r.kind == kind);
    let (Some((model, bus)), Some(record)) = (bound, record) else {
        // Nothing to build it on: keep what was saved, so the next save
        // does not lose it.
        let state = context.state().clone();
        return panel_handle(cx.new(|cx| Kept::new(state, cx)));
    };
    let home = Home {
        model,
        area: context.dock_area(),
        bus,
    };
    new_panel(kind, &home, &record, window, cx)
}

/// Where a workspace panel lives: the model it shows and the dock area it is
/// in.
pub struct Home {
    pub model: Entity<MarketsModel>,
    pub area: WeakEntity<DockArea>,
    /// The workspace's link channels, which the panel joins.
    pub bus: Entity<LinkBus>,
}

/// A panel of `kind` from `record`, on `model`, wrapped for the dock.
pub fn new_panel(
    kind: PanelKind,
    home: &Home,
    record: &PanelRecord,
    window: &mut Window,
    cx: &mut App,
) -> Arc<dyn BasePanelView> {
    let model = home.model.clone();
    let meta = DockMeta::restored(record, home.area.clone(), cx);
    let market = record.market();
    let bus = &home.bus;
    match kind {
        PanelKind::Chart => {
            let period = record.period();
            let panel = cx.new(|cx| ChartPanel::docked(model, market, period, meta, cx));
            linked(panel, bus, window, cx)
        }
        PanelKind::Tiles => {
            let panel = cx.new(|cx| {
                TilesPanel::new(
                    model,
                    Outline::Own(market),
                    std::rc::Rc::new(|_, _, _| {}),
                    cx,
                )
                .with_dock(meta)
            });
            linked(panel, bus, window, cx)
        }
        PanelKind::Detail => {
            let panel =
                cx.new(|cx| DetailPanel::new(model, MarketSource::Own(market), cx).with_dock(meta));
            linked(panel, bus, window, cx)
        }
        PanelKind::Position => {
            let panel = cx.new(|cx| {
                PositionPanel::new(model, MarketSource::Own(market), window, cx).with_dock(meta)
            });
            linked(panel, bus, window, cx)
        }
        PanelKind::Ticket => {
            let portfolio = model.read(cx).portfolio.clone();
            let panel = cx.new(|cx| TicketPanel::new(portfolio, meta, cx));
            linked(panel, bus, window, cx)
        }
        PanelKind::Blotter => {
            let tab = record
                .tab
                .as_deref()
                .and_then(BlotterTab::named)
                .unwrap_or_default();
            let panel = cx.new(|cx| BlotterPanel::new(model, tab, meta, window, cx));
            linked(panel, bus, window, cx)
        }
    }
}

/// A panel that takes part in linking: it has a key and links, and applies
/// what is delivered to it.
pub trait Linked: Panel {
    fn dock(&self) -> &DockMeta;
    fn dock_mut(&mut self) -> &mut DockMeta;
    /// Apply a delivered event. This never publishes: only a user's input
    /// does, so nothing echoes.
    fn apply_link(&mut self, event: &LinkEvent, window: &mut Window, cx: &mut Context<Self>);
}

/// Join `panel` to `bus` with its links, have it apply what it is delivered,
/// and leave the bus when it closes. Returns it wrapped for the dock.
fn linked<P: Linked>(
    panel: Entity<P>,
    bus: &Entity<LinkBus>,
    window: &mut Window,
    cx: &mut App,
) -> Arc<dyn BasePanelView> {
    let (key, roles, actions) = {
        let d = panel.read(cx).dock();
        (d.key.clone(), d.roles, d.actions.clone())
    };
    bus.update(cx, |b, _| b.join(&key, roles, &actions));
    panel.update(cx, |p, cx| {
        let delivered = cx.subscribe_in(
            bus,
            window,
            |this: &mut P, bus, delivered: &Delivered, window, cx| {
                if bus.read(cx).applies(&this.dock().key, delivered) {
                    this.apply_link(&delivered.event, window, cx);
                }
            },
        );
        let leaving = {
            let bus = bus.downgrade();
            cx.on_release(move |this: &mut P, cx| {
                let key = this.dock().key.clone();
                let _ = bus.update(cx, |b, _| b.leave(&key));
            })
        };
        let d = p.dock_mut();
        d.bus = Some(bus.clone());
        d.links = vec![delivered, leaving];
    });
    panel_handle(panel)
}

/// A saved panel that could not be built: it draws nothing and saves what it
/// was given.
struct Kept {
    state: PanelState,
    focus: FocusHandle,
}

impl Kept {
    fn new(state: PanelState, cx: &mut Context<Self>) -> Self {
        Self {
            state,
            focus: cx.focus_handle(),
        }
    }
}

impl Render for Kept {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Focusable for Kept {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PanelEvent> for Kept {}

impl BasePanel for Kept {
    fn panel_name(&self) -> &'static str {
        "markets.kept"
    }

    fn dump(&self, _: &App) -> PanelState {
        self.state.clone()
    }
}

impl Panel for Kept {}

/// The dock's half of each Markets panel, the same for all four: the name it
/// saves under and what it saves come from the panel; everything else is
/// the dock's default.
macro_rules! dock_panel {
    ($panel:ty, $kind:expr) => {
        impl gpui_kit::Focusable for $panel {
            fn focus_handle(&self, _: &gpui_kit::App) -> gpui_kit::FocusHandle {
                self.dock.focus.clone()
            }
        }

        impl gpui_kit::EventEmitter<gpui_kit::component::dock::PanelEvent> for $panel {}

        impl gpui_kit::component::dock::BasePanel for $panel {
            fn panel_name(&self) -> &'static str {
                $kind.panel_name()
            }

            fn dump(&self, cx: &gpui_kit::App) -> gpui_kit::component::dock::PanelState {
                self.record(cx).to_state()
            }
        }

        impl $crate::markets::dock::Linked for $panel {
            fn dock(&self) -> &$crate::markets::dock::DockMeta {
                &self.dock
            }

            fn dock_mut(&mut self) -> &mut $crate::markets::dock::DockMeta {
                &mut self.dock
            }

            fn apply_link(
                &mut self,
                event: &$crate::markets::links::LinkEvent,
                window: &mut gpui_kit::Window,
                cx: &mut gpui_kit::Context<Self>,
            ) {
                self.apply(event, window, cx)
            }
        }

        impl gpui_kit::component::dock::Panel for $panel {
            fn title(
                &mut self,
                _: &mut gpui_kit::Window,
                cx: &mut gpui_kit::Context<Self>,
            ) -> impl gpui_kit::IntoElement {
                $crate::markets::dock::panel_title(self.title_text(cx))
            }

            fn title_suffix(
                &mut self,
                _: &mut gpui_kit::Window,
                _: &mut gpui_kit::Context<Self>,
            ) -> Option<impl gpui_kit::IntoElement> {
                $crate::markets::dock::channel_marks(&self.dock.roles)
            }

            /// Close, in the header. The dock's own refuses to close the
            /// last panel; this one does not, and an empty workspace offers
            /// Add panel and the presets.
            fn toolbar_buttons(
                &mut self,
                _: &mut gpui_kit::Window,
                cx: &mut gpui_kit::Context<Self>,
            ) -> Option<Vec<gpui_kit::component::button::Button>> {
                $crate::markets::dock::close_button(self.dock.area.clone(), cx.entity(), cx)
                    .map(|button| vec![button])
            }

            /// Maximise is a button in the header, beside close.
            fn zoom_control(
                &self,
                _: &gpui_kit::App,
            ) -> Option<gpui_kit::component::dock::PanelControl> {
                Some(gpui_kit::component::dock::PanelControl::Toolbar)
            }

            /// Panels draw their own spacing, as in the fixed layout.
            fn inner_padding(&self, _: &gpui_kit::App) -> bool {
                false
            }
        }
    };
}
pub(super) use dock_panel;

/// A panel's Close: it leaves `area`. None outside a dock area, or while
/// the layout is locked.
pub fn close_button<P: BasePanel>(
    area: Option<WeakEntity<DockArea>>,
    panel: Entity<P>,
    cx: &App,
) -> Option<gpui_kit::component::button::Button> {
    use gpui_kit::component::IconName;
    use gpui_kit::component::button::{Button, ButtonVariants as _};
    let area = area?;
    // A locked layout closes nothing.
    if area.upgrade().is_some_and(|a| a.read(cx).is_locked()) {
        return None;
    }
    Some(
        Button::new("panel-close")
            .icon(IconName::Close)
            .ghost()
            .tooltip("Close")
            .on_click(move |_, window, cx| {
                if let Some(area) = area.upgrade() {
                    area.update(cx, |a, cx| a.remove_panel(panel.clone(), window, cx));
                }
            }),
    )
}

/// A panel's title in its header.
pub fn panel_title(text: String) -> gpui_kit::AnyElement {
    use gpui_kit::TestSupportExt as _;
    div()
        .id("panel-title")
        .text_size(gpui_kit::px(12.5))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .child(text)
        .test_support()
        .into_any_element()
}

/// Each link channel's letter and colour: A amber, B blue, C violet.
pub const CHANNELS: [(&str, u32); 3] = [("A", 0xf0a73a), ("B", 0x5fa8f5), ("C", 0xb48cf0)];

/// The marks of the channels a panel takes part in, for its header; none
/// for a panel that is not linked.
pub fn channel_marks(roles: &[Role; 3]) -> Option<gpui_kit::AnyElement> {
    use gpui_kit::TestSupportExt as _;
    if roles.iter().all(|r| *r == Role::Off) {
        return None;
    }
    let marks = roles
        .iter()
        .enumerate()
        .filter(|(_, role)| **role != Role::Off)
        .map(|(ix, _)| {
            let (letter, colour) = CHANNELS[ix];
            let colour = gpui_kit::rgb(colour);
            div()
                .id(("channel-mark", ix))
                .size(gpui_kit::px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(gpui_kit::px(3.))
                .border_1()
                .border_color(colour)
                .text_size(gpui_kit::px(10.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(colour)
                .child(letter)
                .test_support()
        });
    Some(
        gpui_kit::component::h_flex()
            .gap_1()
            .children(marks)
            .into_any_element(),
    )
}

/// How a panel names a market in its title.
pub fn market_name(market: &MarketId) -> String {
    if market.dex().is_empty() {
        market.ticker().to_string()
    } else {
        format!("{} ({})", market.ticker(), market.dex())
    }
}
