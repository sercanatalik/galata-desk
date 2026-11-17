//! Learning which account to show: a wallet in the user's browser proves it
//! owns an address, and a short-lived local server carries that proof back.
//!
//! No key ever reaches this process. The wallet signs a plain-text challenge;
//! this crate checks the signature and keeps only the address.

mod bridge;
mod proof;

pub use bridge::{Attempt, AttemptEvent, BridgeError, EXPIRY};
pub use proof::{Address, Challenge, ProofError};

#[doc(hidden)]
pub use proof::testing as proof_testing;
