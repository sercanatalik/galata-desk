//! Linking: three channels, A, B and C, that carry what one panel does to the
//! panels listening. A panel sends on a channel when its role there is Send
//! or Both, and only the actions it has switched on. A panel applies what is
//! delivered when its role there is Listen or Both and it did not send it.
//!
//! The bus knows panels only by their keys. It holds no reference to the
//! portfolio or any order path: nothing it carries can trade.

use std::collections::{HashMap, HashSet};

use galata_desk_core::market::MarketId;
use galata_desk_core::rust_decimal::Decimal;
use gpui_kit::{Context, EventEmitter};

use super::dock::{Action, Role};

/// Something one panel tells the panels linked to it.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkEvent {
    /// Show this market.
    SetMarket(MarketId),
    /// The pointer is over this time on a chart, or has left it.
    TimeCursor(Option<u64>),
    /// A price picked on a chart of this market, for the ticket's limit
    /// price.
    LimitPrice(MarketId, Decimal),
    /// Show only this market's rows, or every row.
    FilterBlotter(Option<MarketId>),
}

impl LinkEvent {
    /// The action a panel must have switched on to send this.
    pub fn action(&self) -> Action {
        match self {
            Self::SetMarket(_) => Action::SetMarket,
            Self::TimeCursor(_) => Action::TimeCursor,
            Self::LimitPrice(..) => Action::LimitPrice,
            Self::FilterBlotter(_) => Action::FilterBlotter,
        }
    }
}

/// An event on its way along one channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivered {
    /// 0 for A, 1 for B, 2 for C.
    pub channel: usize,
    /// The key of the panel that sent it.
    pub from: String,
    pub event: LinkEvent,
}

#[derive(Default)]
pub struct LinkBus {
    /// The market each channel last showed.
    markets: [Option<MarketId>; 3],
    /// Each panel's role on each channel. A panel not here is on none.
    roles: HashMap<String, [Role; 3]>,
    /// The actions each panel sends.
    actions: HashMap<String, HashSet<Action>>,
    /// How many events panels have published, for tests.
    #[cfg(test)]
    pub published: usize,
}

impl EventEmitter<Delivered> for LinkBus {}

impl LinkBus {
    /// The market `channel` shows.
    pub fn market(&self, channel: usize) -> Option<&MarketId> {
        self.markets.get(channel)?.as_ref()
    }

    /// Show `market` on `channel`, as when a workspace starts on it.
    pub fn set_market(&mut self, channel: usize, market: Option<MarketId>) {
        if let Some(slot) = self.markets.get_mut(channel) {
            *slot = market;
        }
    }

    pub fn roles(&self, panel: &str) -> [Role; 3] {
        self.roles.get(panel).copied().unwrap_or_default()
    }

    pub fn set_roles(&mut self, panel: &str, roles: [Role; 3]) {
        self.roles.insert(panel.to_string(), roles);
    }

    #[allow(dead_code, reason = "the links view (task 4.6) lists them")]
    pub fn actions(&self, panel: &str) -> Vec<Action> {
        let mut actions: Vec<Action> = self
            .actions
            .get(panel)
            .map(|a| a.iter().copied().collect())
            .unwrap_or_default();
        actions.sort_by_key(|a| *a as u8);
        actions
    }

    #[allow(dead_code, reason = "the links view (task 4.6) switches them")]
    pub fn set_action(&mut self, panel: &str, action: Action, on: bool) {
        let actions = self.actions.entry(panel.to_string()).or_default();
        if on {
            actions.insert(action);
        } else {
            actions.remove(&action);
        }
    }

    /// Take a panel's links as it saved them.
    pub fn join(&mut self, panel: &str, roles: [Role; 3], actions: &[Action]) {
        self.set_roles(panel, roles);
        self.actions
            .insert(panel.to_string(), actions.iter().copied().collect());
    }

    /// Forget a closed panel's links.
    pub fn leave(&mut self, panel: &str) {
        self.roles.remove(panel);
        self.actions.remove(panel);
    }

    /// Where `event` from `from` goes: one delivery for each channel the
    /// panel sends on, if it has the event's action switched on. A market
    /// sent becomes the channel's market.
    pub fn route(&mut self, from: &str, event: LinkEvent) -> Vec<Delivered> {
        let sends = self
            .actions
            .get(from)
            .is_some_and(|a| a.contains(&event.action()));
        if !sends {
            return Vec::new();
        }
        let roles = self.roles(from);
        let mut out = Vec::new();
        for (channel, role) in roles.iter().enumerate() {
            if !matches!(role, Role::Send | Role::Both) {
                continue;
            }
            if let LinkEvent::SetMarket(market) = &event {
                self.markets[channel] = Some(market.clone());
            }
            out.push(Delivered {
                channel,
                from: from.to_string(),
                event: event.clone(),
            });
        }
        out
    }

    /// Send `event` from the panel `from`. Only a user's input publishes; a
    /// panel applying a delivery never does, so nothing echoes.
    pub fn publish(&mut self, from: &str, event: LinkEvent, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.published += 1;
        }
        for delivered in self.route(from, event) {
            cx.emit(delivered);
        }
    }

    /// Whether `panel` applies `delivered`: it listens on that channel and
    /// did not send it.
    pub fn applies(&self, panel: &str, delivered: &Delivered) -> bool {
        delivered.from != panel
            && matches!(
                self.roles(panel).get(delivered.channel),
                Some(Role::Listen | Role::Both)
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn btc() -> MarketId {
        MarketId::new("BTC")
    }

    /// Tiles sending on A, a chart listening on A, a chart listening on B,
    /// and a chart that sends and listens on A.
    fn bus() -> LinkBus {
        let mut bus = LinkBus::default();
        bus.join(
            "tiles",
            [Role::Send, Role::Off, Role::Off],
            &[Action::SetMarket],
        );
        bus.join("chart-a", [Role::Listen, Role::Off, Role::Off], &[]);
        bus.join("chart-b", [Role::Off, Role::Listen, Role::Off], &[]);
        bus.join(
            "chart-ab",
            [Role::Both, Role::Off, Role::Off],
            &[Action::SetMarket],
        );
        bus
    }

    fn receivers(bus: &LinkBus, delivered: &[Delivered]) -> Vec<&'static str> {
        ["tiles", "chart-a", "chart-b", "chart-ab"]
            .into_iter()
            .filter(|p| delivered.iter().any(|d| bus.applies(p, d)))
            .collect()
    }

    #[test]
    fn a_market_reaches_only_the_listeners_on_its_channel() {
        let mut bus = bus();
        let delivered = bus.route("tiles", LinkEvent::SetMarket(btc()));
        assert_eq!(
            delivered,
            [Delivered {
                channel: 0,
                from: "tiles".into(),
                event: LinkEvent::SetMarket(btc()),
            }]
        );
        assert_eq!(receivers(&bus, &delivered), ["chart-a", "chart-ab"]);
        assert_eq!(bus.market(0), Some(&btc()));
        assert_eq!(bus.market(1), None);
    }

    #[test]
    fn the_sender_does_not_receive_its_own_event() {
        let mut bus = bus();
        let delivered = bus.route("chart-ab", LinkEvent::SetMarket(btc()));
        assert_eq!(delivered.len(), 1);
        assert_eq!(receivers(&bus, &delivered), ["chart-a"]);
    }

    #[test]
    fn an_action_switched_off_is_not_sent() {
        let mut bus = bus();
        bus.set_action("tiles", Action::SetMarket, false);
        assert!(bus.route("tiles", LinkEvent::SetMarket(btc())).is_empty());
        assert_eq!(bus.market(0), None, "the channel keeps its market");
        // An action never switched on is not sent either.
        assert!(
            bus.route("tiles", LinkEvent::TimeCursor(Some(1)))
                .is_empty()
        );
        // A listener alone sends nothing.
        assert!(bus.route("chart-a", LinkEvent::SetMarket(btc())).is_empty());
    }

    #[test]
    fn a_panel_on_two_channels_sends_on_both() {
        let mut bus = bus();
        bus.set_roles("tiles", [Role::Send, Role::Send, Role::Off]);
        let delivered = bus.route("tiles", LinkEvent::SetMarket(btc()));
        assert_eq!(
            delivered.iter().map(|d| d.channel).collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(
            receivers(&bus, &delivered),
            ["chart-a", "chart-b", "chart-ab"]
        );
    }

    #[test]
    fn a_closed_panel_neither_sends_nor_receives() {
        let mut bus = bus();
        bus.leave("chart-a");
        let delivered = bus.route("tiles", LinkEvent::SetMarket(btc()));
        assert_eq!(receivers(&bus, &delivered), ["chart-ab"]);
        bus.leave("tiles");
        assert!(bus.route("tiles", LinkEvent::SetMarket(btc())).is_empty());
    }
}
