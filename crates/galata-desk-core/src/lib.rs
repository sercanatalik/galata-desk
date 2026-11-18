//! Everything galata-desk does that needs no window: reading an account from
//! the venue, the portfolio's arithmetic, and the wallet connection bridge.
//!
//! This crate links no UI toolkit, so all of it is tested with `cargo test`.

pub mod api_key;
pub mod connect;
pub mod drawdown;
pub mod feed;
pub mod format;
pub mod funding;
pub mod market;
pub mod model;
pub mod network;
pub mod secrets;
pub mod store;
pub mod trade;
pub mod venue;
pub use rust_decimal;
