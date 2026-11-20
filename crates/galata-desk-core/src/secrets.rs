//! Where secrets are kept: the operating system's keychain.
//!
//! Nothing secret is ever written to the app's directory. Tests, and
//! platforms without a keychain, use the in-memory store.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::network::Network;

/// The keychain could not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the keychain refused: {0}")]
pub struct SecretError(pub String);

/// A place to keep named secrets.
pub trait Secrets: Send + Sync {
    /// `None` when nothing is stored under the name.
    fn get(&self, name: &str) -> Result<Option<String>, SecretError>;
    /// Stores, replacing anything already under the name.
    fn set(&self, name: &str, secret: &str) -> Result<(), SecretError>;
    /// Removing what is not there is not an error.
    fn delete(&self, name: &str) -> Result<(), SecretError>;
}

/// The name an API key is kept under: one per network.
pub fn api_key_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "api-key-mainnet",
        Network::Testnet => "api-key-testnet",
    }
}

/// The name galata-watcher's API token is kept under. One for all networks:
/// a watcher watches one network of its own.
pub const WATCHER_TOKEN_NAME: &str = "watcher-api-token";

/// Secrets held in memory for the life of the process.
#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

impl Secrets for MemorySecrets {
    fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }

    fn set(&self, name: &str, secret: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().insert(name.into(), secret.into());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

/// The macOS Keychain: generic passwords under one service name.
#[cfg(target_os = "macos")]
pub struct Keychain {
    service: String,
}

#[cfg(target_os = "macos")]
impl Keychain {
    /// The app's own entries.
    pub const SERVICE: &'static str = "dev.galata.desk";

    pub fn new() -> Self {
        Self::under(Self::SERVICE)
    }

    /// Entries under another service name, for trying the keychain out
    /// without touching the app's own.
    pub fn under(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

#[cfg(target_os = "macos")]
impl Default for Keychain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "macos")]
impl Secrets for Keychain {
    fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(&self.service, name) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| SecretError("the stored entry is not text".into())),
            Err(e) if e.code() == NOT_FOUND => Ok(None),
            Err(e) => Err(SecretError(e.to_string())),
        }
    }

    fn set(&self, name: &str, secret: &str) -> Result<(), SecretError> {
        security_framework::passwords::set_generic_password(&self.service, name, secret.as_bytes())
            .map_err(|e| SecretError(e.to_string()))
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        match security_framework::passwords::delete_generic_password(&self.service, name) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(SecretError(e.to_string())),
        }
    }
}

/// `errSecItemNotFound`.
#[cfg(target_os = "macos")]
const NOT_FOUND: i32 = -25300;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_kept_replaced_and_removed() {
        let store = MemorySecrets::default();
        assert_eq!(store.get("k").unwrap(), None);
        store.set("k", "one").unwrap();
        assert_eq!(store.get("k").unwrap().as_deref(), Some("one"));
        store.set("k", "two").unwrap();
        assert_eq!(store.get("k").unwrap().as_deref(), Some("two"));
        store.delete("k").unwrap();
        assert_eq!(store.get("k").unwrap(), None);
        // Removing what is not there is fine.
        store.delete("k").unwrap();
    }

    #[test]
    fn each_network_has_its_own_key() {
        assert_ne!(
            api_key_name(Network::Mainnet),
            api_key_name(Network::Testnet)
        );
        let store = MemorySecrets::default();
        store.set(api_key_name(Network::Testnet), "t").unwrap();
        assert_eq!(store.get(api_key_name(Network::Mainnet)).unwrap(), None);
    }
}
