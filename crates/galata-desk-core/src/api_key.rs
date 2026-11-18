//! An API wallet's private key, as the user pastes it into settings.
//!
//! Hyperliquid lets an account approve a separate wallet that can place and
//! cancel orders for it and cannot withdraw. Its private key is the "API
//! key". This type exists to check one, reduce it to the address to show, and
//! hand its text to the keychain. Signing with it happens in `trade::sign`,
//! and only the gate in `trade` asks for that.

use std::fmt;

use k256::ecdsa::SigningKey;

use crate::connect::Address;

/// Why a pasted key was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ApiKeyError {
    #[error("an API key is 64 hexadecimal characters, with or without 0x in front")]
    NotAKey,
    /// Sixty-four hex digits, but not a number a key can be.
    #[error("that is 64 hexadecimal characters but not a valid key")]
    Invalid,
}

/// A checked private key. It is never printed: its `Debug` shows only the
/// address, and the key's memory is cleared when it is dropped.
pub struct ApiKey(SigningKey);

impl ApiKey {
    /// Accepts 64 hex digits, any letter case, with or without `0x`, and
    /// surrounding white space from a paste.
    pub fn parse(text: &str) -> Result<Self, ApiKeyError> {
        let text = text.trim();
        let digits = text
            .strip_prefix("0x")
            .or_else(|| text.strip_prefix("0X"))
            .unwrap_or(text);
        if digits.len() != 64 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ApiKeyError::NotAKey);
        }
        let bytes = hex::decode(digits).map_err(|_| ApiKeyError::NotAKey)?;
        SigningKey::from_slice(&bytes)
            .map(Self)
            .map_err(|_| ApiKeyError::Invalid)
    }

    /// The address of the wallet this key belongs to: what Hyperliquid shows
    /// for the API wallet.
    pub fn address(&self) -> Address {
        Address::from_key(self.0.verifying_key())
    }

    /// The key itself, for signing an action and nothing else.
    pub(crate) fn signing_key(&self) -> &SigningKey {
        &self.0
    }

    /// The key as text, for the keychain and nowhere else.
    pub fn expose(&self) -> String {
        hex::encode(self.0.to_bytes())
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ApiKey(for {})", self.address())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn a_key_gives_the_address_of_its_wallet() {
        // The key 1 and its address are a standard test vector.
        let key = ApiKey::parse(ONE).unwrap();
        assert_eq!(
            key.address().as_str(),
            "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
        );
    }

    #[test]
    fn the_prefix_case_and_pasted_white_space_do_not_matter() {
        let plain = ApiKey::parse(ONE).unwrap().address();
        assert_eq!(ApiKey::parse(&format!("0x{ONE}")).unwrap().address(), plain);
        assert_eq!(
            ApiKey::parse(&format!("  0X{ONE}\n")).unwrap().address(),
            plain
        );
        let mixed = "aB".repeat(32);
        assert_eq!(
            ApiKey::parse(&mixed).unwrap().address(),
            ApiKey::parse(&mixed.to_lowercase()).unwrap().address()
        );
    }

    #[test]
    fn anything_else_is_not_a_key() {
        for bad in [
            "",
            "0x",
            "hello",
            &ONE[1..],                  // 63 digits
            &format!("{ONE}0"),         // 65 digits
            &format!("{}g", &ONE[1..]), // not hex
            // An address is 40 digits, not 64.
            "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf",
        ] {
            assert_eq!(
                ApiKey::parse(bad).unwrap_err(),
                ApiKeyError::NotAKey,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn sixty_four_digits_that_cannot_be_a_key_are_refused() {
        // Zero, and a number past the end of the curve.
        assert_eq!(
            ApiKey::parse(&"0".repeat(64)).unwrap_err(),
            ApiKeyError::Invalid
        );
        assert_eq!(
            ApiKey::parse(&"f".repeat(64)).unwrap_err(),
            ApiKeyError::Invalid
        );
    }

    #[test]
    fn a_key_survives_the_keychain_and_is_never_printed() {
        let key = ApiKey::parse(&format!("0x{}", "ab".repeat(32))).unwrap();
        let stored = key.expose();
        assert_eq!(stored, "ab".repeat(32));
        assert_eq!(ApiKey::parse(&stored).unwrap().address(), key.address());
        let shown = format!("{key:?}");
        assert!(!shown.contains(&stored), "{shown}");
        assert!(shown.contains(key.address().as_str()));
    }
}
