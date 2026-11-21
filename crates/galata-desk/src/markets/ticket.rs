//! The order ticket as a workspace panel. The ticket stays the Portfolio's,
//! where orders are checked, confirmed and sent; the panel only gives it a
//! place on screen. While it is docked, Buy and Sell fill it instead of
//! opening it over the screen.

use gpui_kit::component::v_flex;
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Subscription, TestSupportExt, Window, div, px};

use super::dock::{DockMeta, PanelKind, PanelRecord, dock_panel};
use crate::portfolio::Portfolio;
use crate::theme::Palette;

pub struct TicketPanel {
    portfolio: Entity<Portfolio>,
    /// Its key and links in the workspace.
    dock: DockMeta,
    _subscriptions: Vec<Subscription>,
}

dock_panel!(TicketPanel, PanelKind::Ticket);

impl TicketPanel {
    pub fn new(portfolio: Entity<Portfolio>, dock: DockMeta, cx: &mut Context<Self>) -> Self {
        portfolio.update(cx, |p, cx| p.ticket_panel_added(cx));
        let subscriptions = vec![
            cx.observe(&portfolio, |_, _, cx| cx.notify()),
            // Closed, the ticket goes back over the screen.
            cx.on_release(|this: &mut Self, cx| {
                this.portfolio
                    .update(cx, |p, cx| p.ticket_panel_removed(cx));
            }),
        ];
        Self {
            portfolio,
            dock,
            _subscriptions: subscriptions,
        }
    }

    /// Apply a linked panel's event: the market, or a limit price picked on
    /// a chart. Either only fills in the ticket, as typing would: nothing is
    /// sent and no confirmation opens. This never publishes.
    pub fn apply(
        &mut self,
        event: &super::links::LinkEvent,
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::Context<Self>,
    ) {
        use super::links::LinkEvent;
        match event {
            LinkEvent::SetMarket(market) => self
                .portfolio
                .update(cx, |p, cx| p.show_ticket_market(market.clone(), window, cx)),
            LinkEvent::LimitPrice(market, price) => self.portfolio.update(cx, |p, cx| {
                p.fill_ticket_limit(market.clone(), *price, window, cx)
            }),
            LinkEvent::TimeCursor(_) | LinkEvent::FilterBlotter(_) => {}
        }
    }

    /// What the panel saves in a layout: nothing but its key and links. The
    /// ticket's market is whatever it was last filled with.
    pub fn record(&self, cx: &gpui_kit::App) -> PanelRecord {
        self.dock.record(PanelKind::Ticket, None, None, cx)
    }

    pub fn title_text(&self, _: &gpui_kit::App) -> String {
        "Order ticket".to_string()
    }
}

impl Render for TicketPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ticket = self.portfolio.update(cx, |p, cx| p.ticket_inline(cx));
        v_flex()
            .id("ticket-panel")
            .size_full()
            .track_focus(&self.dock.focus)
            .child(ticket.unwrap_or_else(|| {
                div()
                    .p_3()
                    .text_size(px(12.5))
                    .text_color(Palette::muted())
                    .child("Press Buy or Sell on a market to fill in the ticket.")
                    .into_any_element()
            }))
            .test_support()
    }
}
