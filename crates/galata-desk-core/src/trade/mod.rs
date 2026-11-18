//! Changing the account's orders: the only part of the app that signs.
//!
//! - [`sign`] turns an action into a signature. Pure.
//! - [`assets`] names a market the way an action must.
//! - [`rules`] checks a typed price and size against the venue's rules.
//! - [`exchange`] carries a signed action to the venue and reads the answer.
//! - [`gate`] is the only caller of the two above: it checks that acting is
//!   allowed before the key is even read.

pub mod assets;
pub mod exchange;
pub mod gate;
pub mod rules;
pub mod sign;

pub use exchange::{Answer, Outcome, Traded};
pub use gate::{
    Holding, Kept, Level, Pricing, Standing, Target, TpslNow, TpslRequest, TpslStep, Trader,
    Unavailable,
};
