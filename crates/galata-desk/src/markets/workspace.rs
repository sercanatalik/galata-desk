//! The fullscreen workspace: one dock area of Markets panels, made the first
//! time the window goes fullscreen on Markets and kept while the app runs,
//! so leaving and coming back finds the panels where they were.

use std::rc::Rc;

use galata_desk_core::market::{MarketId, MarketPeriod};
use gpui_kit::component::Placement;
use gpui_kit::component::dock::{
    DockArea, DockLayout, DockPlacement, DockSkin, InsertTarget, NodeId, PanelInfo, PanelState,
    PanelStyle,
};
use gpui_kit::{App, AppContext as _, Bounds, Entity, Pixels, Window, point, px, size};

use super::dock::{self, PanelKind, PanelRecord};
use super::links::LinkBus;
use super::model::MarketsModel;

/// What Add panel offers, in its order, by name.
pub const CATALOGUE: [(PanelKind, &str); 6] = [
    (PanelKind::Chart, "Chart"),
    (PanelKind::Tiles, "Watchlist tiles"),
    (PanelKind::Detail, "Market detail"),
    (PanelKind::Position, "Position"),
    (PanelKind::Ticket, "Order ticket"),
    (PanelKind::Blotter, "Blotter"),
];

/// The version written into a saved dock layout. A layout of another
/// version is not read.
pub const DOCK_VERSION: usize = 1;

pub struct Workspace {
    pub area: Entity<DockArea>,
    /// The dock's appearance, kept to change its settings.
    #[allow(dead_code, reason = "kept to change the dock's settings")]
    pub skin: Rc<DockSkin>,
    /// The link channels: what each shows, and which panels send and
    /// listen on it.
    pub bus: Entity<LinkBus>,
}

impl Workspace {
    /// A new workspace in the Trader layout, its channel A on the market the
    /// fixed layout shows.
    pub fn new(model: &Entity<MarketsModel>, window: &mut Window, cx: &mut App) -> Self {
        let (area, skin) = DockSkin::dock_area("markets-workspace", Some(DOCK_VERSION), window, cx);
        // Every panel has a header with its title, its channels, maximise and
        // close; there are no side docks to collapse.
        skin.set_panel_style(PanelStyle::TabBar, cx);
        // Close is the panel's own button (see `dock::close_button`).
        skin.set_close_button_visible(false, cx);
        skin.set_toggle_button_visible(false, cx);

        let (selected, other) = {
            let m = model.read(cx);
            let selected = m.selected().cloned();
            let other = m
                .watchlist
                .as_ref()
                .and_then(|w| w.markets().iter().find(|x| Some(*x) != selected.as_ref()))
                .cloned();
            (selected, other)
        };
        let bus = cx.new(|_| {
            let mut bus = LinkBus::default();
            bus.set_market(0, selected.clone());
            bus
        });
        dock::bind(&area, model, &bus, cx);
        let this = Self { area, skin, bus };
        this.apply_trader(model, other, window, cx);
        this
    }

    /// Replace the layout with Trader, its first chart on channel A's market
    /// and its second on `second`.
    pub fn apply_trader(
        &self,
        model: &Entity<MarketsModel>,
        second: Option<MarketId>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let home = dock::Home {
            model: model.clone(),
            area: self.area.downgrade(),
            bus: self.bus.clone(),
        };
        let first = self.channel_market(0, cx);
        let layout = trader(&home, first, second, window, cx);
        self.area
            .update(cx, |area, cx| area.set_center(layout, window, cx));
    }

    /// Add a new panel of `kind` beside the largest panel, splitting it
    /// along its longer side; into the empty workspace, it fills it. A
    /// panel that shows a market starts on channel A's, or the fixed
    /// layout's.
    pub fn add(
        &self,
        model: &Entity<MarketsModel>,
        kind: PanelKind,
        window: &mut Window,
        cx: &mut App,
    ) {
        let market = self
            .channel_market(0, cx)
            .or_else(|| model.read(cx).selected().cloned());
        let shows_market = !matches!(kind, PanelKind::Ticket | PanelKind::Blotter);
        let record = PanelRecord::fresh(kind, market.as_ref().filter(|_| shows_market), None);
        let home = dock::Home {
            model: model.clone(),
            area: self.area.downgrade(),
            bus: self.bus.clone(),
        };
        let largest = self.largest_group(cx);
        let panel = dock::new_panel(kind, &home, &record, window, cx);
        let id = panel.panel_id(cx);
        self.area.update(cx, |area, cx| {
            area.add_panel_view(panel, DockPlacement::Center, None, window, cx);
            if let Some((node, bounds)) = largest {
                let placement = if bounds.size.width >= bounds.size.height {
                    Placement::Right
                } else {
                    Placement::Bottom
                };
                area.move_panel(
                    id,
                    InsertTarget::Split {
                        node,
                        placement,
                        size: None,
                    },
                    window,
                    cx,
                );
            }
        });
    }

    /// The tab group with the most area on screen, and where it is.
    pub fn largest_group(&self, cx: &App) -> Option<(NodeId, Bounds<Pixels>)> {
        let area = self.area.read(cx);
        let mut groups = Vec::new();
        group_bounds(&area.dump(cx).center, area.bounds(), &mut groups);
        let tree = area.layout(DockPlacement::Center)?;
        // Each group by its first panel, found by the key it saved.
        let node_of = |key: &str| {
            tree.panels().find_map(|id| {
                let record = PanelRecord::from_state(&area.panel(id)?.dump(cx))?;
                (record.key == key)
                    .then(|| tree.find_panel_node(id))
                    .flatten()
            })
        };
        groups
            .into_iter()
            .filter_map(|(key, bounds)| Some((node_of(&key)?, bounds)))
            .max_by(|a, b| {
                let size = |b: &Bounds<Pixels>| f32::from(b.size.width) * f32::from(b.size.height);
                size(&a.1).total_cmp(&size(&b.1))
            })
    }

    /// The market a link channel shows.
    pub fn channel_market(&self, channel: usize, cx: &App) -> Option<MarketId> {
        self.bus.read(cx).market(channel).cloned()
    }

    /// Whether the layout is locked: no panel dragged, resized, closed or
    /// added.
    pub fn is_locked(&self, cx: &App) -> bool {
        self.area.read(cx).is_locked()
    }

    pub fn set_locked(&self, locked: bool, window: &mut Window, cx: &mut App) {
        self.area
            .update(cx, |area, cx| area.set_locked(locked, window, cx));
    }

    /// Where the dividers between panels are, relative to the dock area: a
    /// strip along each group edge that another group shares. The dock's
    /// lock does not stop resizing, so a locked workspace covers these.
    pub fn divider_strips(&self, cx: &App) -> Vec<Bounds<Pixels>> {
        const STRIP: f32 = 10.;
        let area = self.area.read(cx);
        let whole = area.bounds();
        let mut groups = Vec::new();
        group_bounds(&area.dump(cx).center, whole, &mut groups);
        let mut strips = Vec::new();
        for (_, b) in groups {
            let (x, y) = (b.origin.x - whole.origin.x, b.origin.y - whole.origin.y);
            if b.right() < whole.right() - px(1.) {
                strips.push(Bounds::new(
                    point(x + b.size.width - px(STRIP / 2.), y),
                    size(px(STRIP), b.size.height),
                ));
            }
            if b.bottom() < whole.bottom() - px(1.) {
                strips.push(Bounds::new(
                    point(x, y + b.size.height - px(STRIP / 2.)),
                    size(b.size.width, px(STRIP)),
                ));
            }
        }
        strips
    }

    /// Whether the last panel was closed.
    pub fn is_empty(&self, cx: &App) -> bool {
        self.area
            .read(cx)
            .layout(DockPlacement::Center)
            .is_none_or(|tree| tree.panels().next().is_none())
    }
}

/// A new panel of `kind` on `market`, wrapped in its own tab group.
fn group(
    home: &dock::Home,
    kind: PanelKind,
    market: Option<MarketId>,
    period: Option<MarketPeriod>,
    window: &mut Window,
    cx: &mut App,
) -> DockLayout {
    let record = PanelRecord::fresh(kind, market.as_ref(), period);
    let panel = dock::new_panel(kind, home, &record, window, cx);
    DockLayout::tabs().panel_view(panel, cx)
}

/// Trader: the tiles over the blotter on the left, two charts over the
/// position in the middle, and the detail over the order ticket on the
/// right.
pub fn trader(
    home: &dock::Home,
    first: Option<MarketId>,
    second: Option<MarketId>,
    window: &mut Window,
    cx: &mut App,
) -> DockLayout {
    let second = second.or_else(|| first.clone());
    let g = |kind, market: Option<MarketId>, period, window: &mut Window, cx: &mut App| {
        group(home, kind, market, period, window, cx)
    };
    let left = DockLayout::v_split()
        .child(g(PanelKind::Tiles, first.clone(), None, window, cx), None)
        .child(g(PanelKind::Blotter, None, None, window, cx), None);
    let charts = DockLayout::h_split()
        .child(g(PanelKind::Chart, first.clone(), None, window, cx), None)
        .child(g(PanelKind::Chart, second, None, window, cx), None);
    let middle = DockLayout::v_split().child(charts, None).child(
        g(PanelKind::Position, first.clone(), None, window, cx),
        None,
    );
    let right = DockLayout::v_split()
        .child(g(PanelKind::Detail, first, None, window, cx), None)
        .child(g(PanelKind::Ticket, None, None, window, cx), None);
    DockLayout::h_split()
        .child(left, None)
        .child(middle, None)
        .child(right, None)
}

/// Where each tab group of a saved layout sits within `bounds`, by its first
/// panel's key: the splits' sizes, shared out across the space they have.
fn group_bounds(
    state: &PanelState,
    bounds: Bounds<Pixels>,
    out: &mut Vec<(String, Bounds<Pixels>)>,
) {
    match &state.info {
        PanelInfo::Tabs { .. } => {
            if let Some(key) = state.children.iter().find_map(PanelRecord::from_state) {
                out.push((key.key, bounds));
            }
        }
        PanelInfo::Stack { sizes, axis } => {
            let across = *axis == 0;
            let extent = f32::from(if across {
                bounds.size.width
            } else {
                bounds.size.height
            });
            let total: f32 = sizes.iter().map(|s| f32::from(*s)).sum::<f32>().max(1.);
            let mut at = 0.;
            for (child, share) in state.children.iter().zip(sizes) {
                let length = extent * f32::from(*share) / total;
                let part = if across {
                    Bounds::new(
                        point(bounds.origin.x + px(at), bounds.origin.y),
                        size(px(length), bounds.size.height),
                    )
                } else {
                    Bounds::new(
                        point(bounds.origin.x, bounds.origin.y + px(at)),
                        size(bounds.size.width, px(length)),
                    )
                };
                group_bounds(child, part, out);
                at += length;
            }
        }
        PanelInfo::Panel(_) => {}
    }
}
