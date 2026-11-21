//! The fullscreen workspace: one dock area of Markets panels, made the first
//! time the window goes fullscreen on Markets and kept while the app runs,
//! so leaving and coming back finds the panels where they were.

use std::rc::Rc;

use galata_desk_core::market::{MarketId, MarketPeriod};
use gpui_kit::component::dock::{DockArea, DockLayout, DockPlacement, DockSkin};
use gpui_kit::{App, Entity, Window};

use super::dock::{self, PanelKind, PanelRecord};
use super::model::MarketsModel;
use super::ticket::TicketPanel;

/// The version written into a saved dock layout. A layout of another
/// version is not read.
pub const DOCK_VERSION: usize = 1;

pub struct Workspace {
    pub area: Entity<DockArea>,
    /// The dock's appearance, kept to change its settings.
    #[allow(dead_code, reason = "the panel header (task 3.6) is styled through it")]
    pub skin: Rc<DockSkin>,
    /// Each link channel's market: A, B and C.
    pub channels: [Option<MarketId>; 3],
}

impl Workspace {
    /// A new workspace in the Trader layout, its channel A on the market the
    /// fixed layout shows.
    pub fn new(model: &Entity<MarketsModel>, window: &mut Window, cx: &mut App) -> Self {
        let (area, skin) = DockSkin::dock_area("markets-workspace", Some(DOCK_VERSION), window, cx);
        dock::bind(&area, model, cx);
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
        let layout = trader(model, selected.clone(), other, window, cx);
        area.update(cx, |area, cx| area.set_center(layout, window, cx));
        Self {
            area,
            skin,
            channels: [selected, None, None],
        }
    }

    /// Whether an order ticket panel is in the workspace.
    pub fn has_ticket(&self, cx: &App) -> bool {
        let area = self.area.read(cx);
        area.layout(DockPlacement::Center)
            .map(|tree| {
                tree.panels().any(|id| {
                    area.panel(id)
                        .is_some_and(|p| p.view().downcast::<TicketPanel>().is_ok())
                })
            })
            .unwrap_or(false)
    }
}

/// A new panel of `kind` on `market`, wrapped in its own tab group.
fn group(
    model: &Entity<MarketsModel>,
    kind: PanelKind,
    market: Option<MarketId>,
    period: Option<MarketPeriod>,
    window: &mut Window,
    cx: &mut App,
) -> DockLayout {
    let record = PanelRecord::fresh(kind, market.as_ref(), period);
    let panel = dock::new_panel(kind, model.clone(), &record, window, cx);
    DockLayout::tabs().panel_view(panel, cx)
}

/// Trader: the tiles over the blotter on the left, two charts over the
/// position in the middle, and the detail over the order ticket on the
/// right.
pub fn trader(
    model: &Entity<MarketsModel>,
    first: Option<MarketId>,
    second: Option<MarketId>,
    window: &mut Window,
    cx: &mut App,
) -> DockLayout {
    let second = second.or_else(|| first.clone());
    let g = |kind, market: Option<MarketId>, period, window: &mut Window, cx: &mut App| {
        group(model, kind, market, period, window, cx)
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
