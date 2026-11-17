//! The ownership challenge and its verification.
//!
//! The wallet signs the challenge with `personal_sign` (EIP-191). Verifying
//! is recovering the signer from the signature and comparing addresses; there
//! is nothing to look up and no chain to ask.

use std::fmt;

use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use sha3::{Digest, Keccak256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// An account address: `0x` and forty lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Address(String);

impl Address {
    /// Accepts any letter case; `None` unless it is `0x` and forty hex digits.
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text
            .strip_prefix("0x")
            .or_else(|| text.strip_prefix("0X"))?;
        (hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| Self(format!("0x{}", hex.to_ascii_lowercase())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_key(key: &VerifyingKey) -> Self {
        let point = key.to_encoded_point(false);
        // An address is the last 20 bytes of the hash of the public key,
        // without the 0x04 tag that marks it uncompressed.
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        Self(format!("0x{}", hex::encode(&hash[12..])))
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a proof was not accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProofError {
    #[error("that is not an account address")]
    BadAddress,
    #[error("that is not a signature")]
    BadSignature,
    /// The signature is valid, but for another address or another challenge.
    #[error("the signature does not prove ownership of that address")]
    Mismatch,
}

/// One connection attempt's challenge. Its nonce is random and lives only as
/// long as the attempt, so a signature made for an earlier attempt proves
/// nothing to this one.
#[derive(Debug, Clone)]
pub struct Challenge {
    nonce: String,
    issued: String,
}

impl Default for Challenge {
    fn default() -> Self {
        Self::new()
    }
}

impl Challenge {
    pub fn new() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the operating system provides randomness");
        let issued = OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .expect("zero is a valid nanosecond")
            .format(&Rfc3339)
            .expect("a UTC time formats as RFC 3339");
        Self {
            nonce: hex::encode(bytes),
            issued,
        }
    }

    /// The text the wallet shows and signs for `address`.
    pub fn message(&self, address: &Address) -> String {
        format!(
            "galata-desk wants to show the Hyperliquid portfolio of this account.\n\
             \n\
             Signing proves you own the address. It grants no permission to trade or move funds.\n\
             \n\
             Address: {address}\n\
             Nonce: {}\n\
             Issued: {}",
            self.nonce, self.issued
        )
    }

    /// Check that `signature` (65 bytes, hex) is `address` signing this
    /// challenge, and return the proven address.
    pub fn verify(&self, address: &str, signature: &str) -> Result<Address, ProofError> {
        let address = Address::parse(address).ok_or(ProofError::BadAddress)?;
        let signer = recover(self.message(&address).as_bytes(), signature)?;
        if signer == address {
            Ok(address)
        } else {
            Err(ProofError::Mismatch)
        }
    }
}

/// The address that signed `message` with `personal_sign`.
fn recover(message: &[u8], signature: &str) -> Result<Address, ProofError> {
    let bytes = hex::decode(signature.strip_prefix("0x").unwrap_or(signature))
        .map_err(|_| ProofError::BadSignature)?;
    if bytes.len() != 65 {
        return Err(ProofError::BadSignature);
    }
    let mut signature =
        Signature::from_slice(&bytes[..64]).map_err(|_| ProofError::BadSignature)?;
    // Wallets send v as 27 or 28; some send 0 or 1.
    let v = match bytes[64] {
        27 | 28 => bytes[64] - 27,
        v @ (0 | 1) => v,
        _ => return Err(ProofError::BadSignature),
    };
    let mut recovery = RecoveryId::from_byte(v).ok_or(ProofError::BadSignature)?;
    // A signature with a high `s` is the same signature mirrored; recovery
    // wants the low form, and mirroring flips which point it recovers.
    if let Some(low) = signature.normalize_s() {
        signature = low;
        recovery = RecoveryId::from_byte(recovery.to_byte() ^ 1).expect("0 or 1");
    }

    let mut hasher = Keccak256::new();
    hasher.update(format!("\x19Ethereum Signed Message:\n{}", message.len()));
    hasher.update(message);
    let digest = hasher.finalize();

    let key = VerifyingKey::recover_from_prehash(&digest, &signature, recovery)
        .map_err(|_| ProofError::Mismatch)?;
    Ok(Address::from_key(&key))
}

/// Sign as a wallet would, for tests in this crate and its integration tests.
#[doc(hidden)]
pub mod testing {
    use super::*;
    use k256::ecdsa::SigningKey;

    /// A wallet with a fixed key.
    pub struct Wallet(SigningKey);

    impl Wallet {
        pub fn new(seed: u8) -> Self {
            Self(SigningKey::from_slice(&[seed.max(1); 32]).expect("a non-zero key"))
        }

        pub fn address(&self) -> Address {
            Address::from_key(self.0.verifying_key())
        }

        /// `personal_sign`: 65 bytes, hex, `v` as 27 or 28.
        pub fn sign(&self, message: &str) -> String {
            let mut hasher = Keccak256::new();
            hasher.update(format!("\x19Ethereum Signed Message:\n{}", message.len()));
            hasher.update(message.as_bytes());
            let (signature, recovery) = self
                .0
                .sign_prehash_recoverable(&hasher.finalize())
                .expect("signing a 32-byte digest succeeds");
            let mut bytes = signature.to_bytes().to_vec();
            bytes.push(27 + recovery.to_byte());
            format!("0x{}", hex::encode(bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Wallet;
    use super::*;

    #[test]
    fn a_known_key_has_its_known_address() {
        // The well-known test key 0x01 repeated is not standard, so pin the
        // derivation with the canonical vector instead: private key 1.
        let mut one = [0u8; 32];
        one[31] = 1;
        let key = k256::ecdsa::SigningKey::from_slice(&one).unwrap();
        assert_eq!(
            Address::from_key(key.verifying_key()).as_str(),
            "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
        );
    }

    #[test]
    fn addresses_parse_in_any_case_and_nothing_else_does() {
        let a = Address::parse("0xDFC24B077BC1425AD1DEA75BCB6F8158E10DF303").unwrap();
        assert_eq!(a.as_str(), "0xdfc24b077bc1425ad1dea75bcb6f8158e10df303");
        assert!(Address::parse("dfc24b077bc1425ad1dea75bcb6f8158e10df303").is_none());
        assert!(Address::parse("0x1234").is_none());
        assert!(Address::parse("0xzzc24b077bc1425ad1dea75bcb6f8158e10df303").is_none());
        assert!(Address::parse("").is_none());
    }

    #[test]
    fn a_valid_proof_is_accepted() {
        let wallet = Wallet::new(7);
        let challenge = Challenge::new();
        let signature = wallet.sign(&challenge.message(&wallet.address()));
        assert_eq!(
            challenge.verify(wallet.address().as_str(), &signature),
            Ok(wallet.address())
        );
    }

    #[test]
    fn the_address_may_arrive_in_checksum_case() {
        let wallet = Wallet::new(7);
        let challenge = Challenge::new();
        let signature = wallet.sign(&challenge.message(&wallet.address()));
        let shouting = wallet.address().as_str().replace("0x", "0X").to_uppercase();
        assert_eq!(
            challenge.verify(&shouting, &signature),
            Ok(wallet.address())
        );
    }

    #[test]
    fn a_signature_from_another_address_is_a_mismatch() {
        let owner = Wallet::new(7);
        let other = Wallet::new(9);
        let challenge = Challenge::new();
        // The other wallet signs the text naming the owner's address.
        let signature = other.sign(&challenge.message(&owner.address()));
        assert_eq!(
            challenge.verify(owner.address().as_str(), &signature),
            Err(ProofError::Mismatch)
        );
    }

    #[test]
    fn a_proof_for_an_earlier_challenge_is_a_mismatch() {
        let wallet = Wallet::new(7);
        let earlier = Challenge::new();
        let current = Challenge::new();
        let replayed = wallet.sign(&earlier.message(&wallet.address()));
        assert_eq!(
            current.verify(wallet.address().as_str(), &replayed),
            Err(ProofError::Mismatch)
        );
    }

    #[test]
    fn v_may_be_zero_or_one() {
        let wallet = Wallet::new(7);
        let challenge = Challenge::new();
        let signature = wallet.sign(&challenge.message(&wallet.address()));
        let mut bytes = hex::decode(&signature[2..]).unwrap();
        bytes[64] -= 27;
        let signature = hex::encode(bytes);
        assert_eq!(
            challenge.verify(wallet.address().as_str(), &signature),
            Ok(wallet.address())
        );
    }

    #[test]
    fn garbage_is_not_a_signature() {
        let wallet = Wallet::new(7);
        let challenge = Challenge::new();
        let address = wallet.address();
        assert_eq!(
            challenge.verify(address.as_str(), "0x1234"),
            Err(ProofError::BadSignature)
        );
        assert_eq!(
            challenge.verify(address.as_str(), "not hex"),
            Err(ProofError::BadSignature)
        );
        assert_eq!(
            challenge.verify("0x12", "0x1234"),
            Err(ProofError::BadAddress)
        );
    }

    #[test]
    fn the_challenge_says_what_signing_does_not_grant() {
        let wallet = Wallet::new(7);
        let message = Challenge::new().message(&wallet.address());
        assert!(message.contains("grants no permission to trade or move funds"));
        assert!(message.contains(wallet.address().as_str()));
        assert!(message.contains("Nonce: "));
    }

    #[test]
    fn every_challenge_has_its_own_nonce() {
        let wallet = Wallet::new(7);
        let a = Challenge::new().message(&wallet.address());
        let b = Challenge::new().message(&wallet.address());
        assert_ne!(a, b);
    }
}
