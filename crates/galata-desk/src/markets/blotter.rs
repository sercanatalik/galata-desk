//! The blotter: the account's positions, open orders, trades and funding in
//! one panel, with a tab for each. Each tab mirrors the list another section
//! keeps (Portfolio's positions and orders, History's trades, Funding's
//! table) with the same rows and figures. Its buttons are those lists' own,
//! so a Close or Cancel opens the same confirmation and sends nothing until
//! it is confirmed.

use gpui_kit::component::Sizable;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, Subscription, TestSupportExt, Window, div};

use super::dock::{DockMeta, PanelKind, PanelRecord, dock_panel, market_name};
use super::links::LinkEvent;
use super::model::MarketsModel;
use crate::grid::{Col, Grid};
use galata_desk_core::market::MarketId;
use gpui_kit::component::button::Button;
use gpui_kit::component::table::TableEvent;

/// A blotter's tab.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BlotterTab {
    #[default]
    Positions,
    Orders,
    Trades,
    Funding,
}

impl BlotterTab {
    pub const ALL: [BlotterTab; 4] = [
        BlotterTab::Positions,
        BlotterTab::Orders,
        BlotterTab::Trades,
        BlotterTab::Funding,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Positions => "Positions",
            Self::Orders => "Open orders",
            Self::Trades => "Trade history",
            Self::Funding => "Funding",
        }
    }

    /// The name a saved layout keeps it under.
    pub fn name(self) -> &'static str {
        match self {
            Self::Positions => "positions",
            Self::Orders => "orders",
            Self::Trades => "trades",
            Self::Funding => "funding",
        }
    }

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.name() == name)
    }
}

pub struct BlotterPanel {
    model: Entity<MarketsModel>,
    tab: BlotterTab,
    /// Only this market's rows, as a linked panel asked.
    filter: Option<MarketId>,
    /// One list per tab, in `BlotterTab::ALL` order.
    tables: [Entity<TableState<Grid>>; 4],
    /// Its key and links in the workspace.
    dock: DockMeta,
    _subscriptions: Vec<Subscription>,
}

dock_panel!(BlotterPanel, PanelKind::Blotter);

impl BlotterPanel {
    pub fn new(
        model: Entity<MarketsModel>,
        tab: BlotterTab,
        dock: DockMeta,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tables = BlotterTab::ALL.map(|tab| {
            let grid = source(&model, tab, cx);
            cx.new(|cx| {
                TableState::new(grid, window, cx)
                    .col_movable(false)
                    .row_selectable(true)
                    .col_selectable(false)
            })
        });
        let (portfolio, history, funding) = {
            let m = model.read(cx);
            (m.portfolio.clone(), m.history.clone(), m.funding.clone())
        };
        let mut subscriptions = vec![cx.observe(&portfolio, |this, _, cx| this.refresh(cx))];
        // Selecting a row tells the linked panels its market.
        for (ix, table) in tables.iter().enumerate() {
            subscriptions.push(cx.subscribe(table, move |this, _, event: &TableEvent, cx| {
                if let TableEvent::SelectRow(row) = event {
                    this.select_row(BlotterTab::ALL[ix], *row, cx);
                }
            }));
        }
        if let Some(history) = &history {
            subscriptions.push(cx.observe(history, |this, _, cx| this.refresh(cx)));
        }
        if let Some(funding) = &funding {
            subscriptions.push(cx.observe(funding, |this, _, cx| this.refresh(cx)));
        }
        Self {
            model,
            tab,
            filter: None,
            tables,
            dock,
            _subscriptions: subscriptions,
        }
    }

    #[cfg(test)]
    pub fn tab(&self) -> BlotterTab {
        self.tab
    }

    /// Apply a linked panel's event: a filter. This never publishes.
    pub fn apply(&mut self, event: &LinkEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let LinkEvent::FilterBlotter(market) = event {
            self.set_filter(market.clone(), cx);
        }
    }

    pub fn set_tab(&mut self, tab: BlotterTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.refresh(cx);
        }
    }

    /// The row drawn at `row` on `tab` was selected: tell the linked panels
    /// its market, and to show only its rows.
    pub fn select_row(&mut self, tab: BlotterTab, row: usize, cx: &mut Context<Self>) {
        let ix = BlotterTab::ALL.iter().position(|t| *t == tab).unwrap_or(0);
        let Some(market) = self.tables[ix].read(cx).delegate().market_at(row).cloned() else {
            return;
        };
        self.dock.publish(LinkEvent::SetMarket(market.clone()), cx);
        self.dock
            .publish(LinkEvent::FilterBlotter(Some(market)), cx);
    }

    /// Show only `market`'s rows, or every row.
    fn set_filter(&mut self, market: Option<MarketId>, cx: &mut Context<Self>) {
        if self.filter != market {
            self.filter = market;
            self.refresh(cx);
        }
    }

    /// The market the rows are filtered to, for tests.
    #[cfg(test)]
    pub fn filter(&self) -> Option<&MarketId> {
        self.filter.as_ref()
    }

    /// Copy each list's rows again from the section that keeps it.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        for (ix, tab) in BlotterTab::ALL.into_iter().enumerate() {
            let grid = source(&self.model, tab, cx);
            let only = self.filter.clone();
            self.tables[ix].update(cx, |t, cx| {
                t.delegate_mut().copy_rows_from(&grid, only.as_ref());
                t.refresh(cx);
            });
        }
        cx.notify();
    }

    /// The list a tab shows, for tests.
    #[cfg(test)]
    pub fn table(&self, tab: BlotterTab) -> Entity<TableState<Grid>> {
        let ix = BlotterTab::ALL.iter().position(|t| *t == tab).unwrap_or(0);
        self.tables[ix].clone()
    }

    /// What the blotter saves in a layout: its tab.
    pub fn record(&self, cx: &App) -> PanelRecord {
        let mut record = self.dock.record(PanelKind::Blotter, None, None, cx);
        record.tab = Some(self.tab.name().to_string());
        record
    }

    pub fn title_text(&self, _: &App) -> String {
        "Blotter".to_string()
    }
}

/// A copy of the list `tab` mirrors, as its section has it now. A section
/// not there yet gives an empty list.
fn source(model: &Entity<MarketsModel>, tab: BlotterTab, cx: &mut App) -> Grid {
    let (portfolio, history, funding) = {
        let m = model.read(cx);
        (m.portfolio.clone(), m.history.clone(), m.funding.clone())
    };
    match tab {
        BlotterTab::Positions => portfolio.read(cx).positions_grid(cx).mirror(),
        BlotterTab::Orders => portfolio.read(cx).orders_grid(cx).mirror(),
        BlotterTab::Trades => match history {
            Some(history) => history.read(cx).trades_grid(cx).mirror(),
            None => Grid::new(vec![Col::text("Trades", 200.)], "No trades"),
        },
        BlotterTab::Funding => match funding {
            Some(funding) => funding.update(cx, |f, cx| f.blotter_grid(cx)),
            None => Grid::new(vec![Col::text("Funding", 200.)], "No funding"),
        },
    }
}

impl Render for BlotterPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ix = BlotterTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);
        v_flex()
            .id("blotter")
            .size_full()
            .track_focus(&self.dock.focus)
            .child(
                h_flex()
                    .p_2()
                    .gap_3()
                    .child(
                        TabBar::new("blotter-tabs")
                            .segmented()
                            .xsmall()
                            .selected_index(ix)
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.set_tab(BlotterTab::ALL[*ix], cx)
                            }))
                            .children(BlotterTab::ALL.map(BlotterTab::label)),
                    )
                    .children(self.filter.clone().map(|market| {
                        // The filter a linked panel set; pressing it shows every row.
                        Button::new("blotter-filter")
                            .xsmall()
                            .outline()
                            .label(format!("Showing {} \u{d7}", market_name(&market)))
                            .on_click(cx.listener(|this, _, _, cx| this.set_filter(None, cx)))
                    })),
            )
            .child(
                div().flex_1().min_h_0().child(
                    DataTable::new(&self.tables[ix])
                        .stripe(false)
                        .bordered(false),
                ),
            )
            .test_support()
    }
}
