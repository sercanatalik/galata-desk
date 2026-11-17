//! Which of the venue's networks the app shows.

use serde::{Deserialize, Serialize};

/// Hyperliquid runs a main network and a test network with test funds. They
/// are separate venues at separate addresses; an account's address is the
/// same on both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    #[default]
    Mainnet,
    Testnet,
}

impl Network {
    pub fn label(self) -> &'static str {
        match self {
            Network::Mainnet => "Mainnet",
            Network::Testnet => "Testnet",
        }
    }

    /// The host both of the network's addresses are on.
    pub fn host(self) -> &'static str {
        match self {
            Network::Mainnet => "api.hyperliquid.xyz",
            Network::Testnet => "api.hyperliquid-testnet.xyz",
        }
    }

    /// The info endpoint: account and market data over HTTPS.
    pub fn info_url(self) -> String {
        format!("https://{}/info", self.host())
    }

    /// The market data stream.
    pub fn ws_url(self) -> String {
        format!("wss://{}/ws", self.host())
    }

    pub fn is_testnet(self) -> bool {
        self == Network::Testnet
    }
}

/// The addresses the app talks to, and whether the network chose them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub info_url: String,
    pub ws_url: String,
    /// An address came from the development environment, not the network.
    pub overridden: bool,
}

/// The network's addresses, unless development overrides are given: those
/// win, whatever the network.
pub fn endpoints(network: Network, info: Option<String>, ws: Option<String>) -> Endpoints {
    let overridden = info.is_some() || ws.is_some();
    Endpoints {
        info_url: info.unwrap_or_else(|| network.info_url()),
        ws_url: ws.unwrap_or_else(|| network.ws_url()),
        overridden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_network_has_its_own_addresses() {
        assert_eq!(Network::Mainnet.info_url(), crate::venue::INFO_URL);
        assert_eq!(Network::Mainnet.ws_url(), crate::market::ws::WS_URL);
        assert_eq!(
            Network::Testnet.info_url(),
            "https://api.hyperliquid-testnet.xyz/info"
        );
        assert_eq!(
            Network::Testnet.ws_url(),
            "wss://api.hyperliquid-testnet.xyz/ws"
        );
        assert_eq!(Network::default(), Network::Mainnet);
    }

    #[test]
    fn the_network_chooses_the_addresses_unless_development_overrides_them() {
        let chosen = endpoints(Network::Testnet, None, None);
        assert_eq!(chosen.info_url, Network::Testnet.info_url());
        assert_eq!(chosen.ws_url, Network::Testnet.ws_url());
        assert!(!chosen.overridden);

        let overridden = endpoints(
            Network::Testnet,
            Some("http://127.0.0.1:9/info".into()),
            None,
        );
        assert_eq!(overridden.info_url, "http://127.0.0.1:9/info");
        // The other address still follows the network.
        assert_eq!(overridden.ws_url, Network::Testnet.ws_url());
        assert!(overridden.overridden);
    }
}
