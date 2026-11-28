//! galata-watcher, as the desk sees it: where it is, its configuration and
//! alert history, and a feed that keeps them current.
//!
//! The desk only reads and edits. Every alert is decided and sent by the
//! watcher itself. Nothing here touches the venue.

mod client;
mod feed;
mod types;
mod url;

pub use self::client::{Client, WatcherError};
pub use self::feed::{ALERTS_KEPT, ConfigState, EVERY, Problem, WatcherFeed, WatcherSnapshot};

pub use self::types::{Accounts, AlertRow, Direction, Health, Level, LevelState, ListedAccount, WatchedAccount, WatcherConfig};
pub use self::url::{WatcherUrl, WatcherUrlError};
