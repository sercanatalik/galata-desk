//! The connection to galata-watcher that the window holds while one is
//! saved in settings and an account is connected. Settings, the Watcher
//! section and the Markets chart all read it and observe it.
//!
//! It owns the core's [`WatcherFeed`], which reads the watcher every fifteen
//! seconds on the I/O runtime, and copies each snapshot in. Edits go through
//! [`WatcherLink::save`], which the feed runs one at a time.
//!
//! The headless tests give a link its snapshots and record its saves instead:
//! nothing in a test ever reaches a watcher.

use galata_desk_core::format::short_address;
use galata_desk_core::rust_decimal::Decimal;
#[cfg(not(test))]
use galata_desk_core::watcher::WatcherFeed;
use galata_desk_core::watcher::{
    Client, Direction, Level, WatcherConfig, WatcherError, WatcherSnapshot, WatcherUrl,
};
use gpui_kit::{App, AppContext, Context, Entity, Task};

#[cfg(not(test))]
use crate::Core;

/// How long without a successful check before the watcher is said to be
/// stalled: three of its one-minute checks.
pub const STALLED_MS: u64 = 3 * 60 * 1000;

pub struct WatcherLink {
    snapshot: WatcherSnapshot,
    #[cfg(not(test))]
    feed: WatcherFeed,
    #[cfg(not(test))]
    _watch: Task<()>,
    /// What a test's saves did, in order.
    #[cfg(test)]
    pub saved: Vec<WatcherConfig>,
    /// What a test's next save answers. Taken by that save.
    #[cfg(test)]
    pub next_outcome: Option<WatcherError>,
    /// The venue's last prices, as a test gives them: a market not here is
    /// not listed.
    #[cfg(test)]
    pub mids: std::collections::HashMap<String, Decimal>,
}

impl WatcherLink {
    /// Connect to the watcher at `url` with `token`, and start reading it.
    #[cfg_attr(test, allow(unused_variables))]
    pub fn start(url: WatcherUrl, token: String, cx: &mut App) -> Entity<Self> {
        cx.new(|cx: &mut Context<Self>| {
            #[cfg(not(test))]
            {
                let feed = {
                    let core = Core::get(cx);
                    let _io = core.runtime.enter();
                    WatcherFeed::start(Client::new(url.clone(), token))
                };
                let mut snapshots = feed.snapshots();
                let _watch = cx.spawn(async move |this, cx| {
                    while snapshots.changed().await.is_ok() {
                        let snapshot = snapshots.borrow_and_update().clone();
                        if this
                            .update(cx, |this, cx| this.set_snapshot(snapshot, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                Self {
                    snapshot: WatcherSnapshot::default(),
                    feed,
                    _watch,
                }
            }
            #[cfg(test)]
            {
                let _ = (Client::new(url, token), &cx);
                Self {
                    snapshot: WatcherSnapshot::default(),
                    saved: Vec::new(),
                    next_outcome: None,
                    mids: Default::default(),
                }
            }
        })
    }

    pub fn snapshot(&self) -> &WatcherSnapshot {
        &self.snapshot
    }

    pub fn set_snapshot(&mut self, snapshot: WatcherSnapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        cx.notify();
    }

    /// Apply `edit` to the latest configuration and save it with that
    /// configuration's version. The task resolves with the configuration as
    /// saved, or why it was not.
    pub fn save(
        &mut self,
        edit: impl FnOnce(&mut WatcherConfig) + Send + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Result<WatcherConfig, WatcherError>> {
        #[cfg(not(test))]
        {
            let saving = self.feed.save(edit);
            cx.background_spawn(saving)
        }
        #[cfg(test)]
        {
            let outcome = match (
                self.snapshot.config.config().cloned(),
                self.next_outcome.take(),
            ) {
                (None, _) => Err(WatcherError::NoConfig),
                (Some(_), Some(e)) => Err(e),
                (Some(mut config), None) => {
                    edit(&mut config);
                    config.version += 1;
                    for level in &mut config.levels {
                        if level.id.is_none() {
                            level.id = Some(format!(
                                "{}-{}-{}",
                                level.market,
                                level.direction.label(),
                                level.price
                            ));
                            level.state = Some(galata_desk_core::watcher::LevelState::Armed);
                        }
                    }
                    self.saved.push(config.clone());
                    self.snapshot.config =
                        galata_desk_core::watcher::ConfigState::Read(config.clone());
                    cx.notify();
                    Ok(config)
                }
            };
            Task::ready(outcome)
        }
    }
}

impl WatcherLink {
    /// Add an armed level for `market` (`BTC`, `xyz:NVDA`) at `price`. Its
    /// direction is `direction` when chosen, or else follows the market's
    /// last price: `last` when the caller knows it, or the venue's mid. A
    /// market the venue doesn't list is refused before anything is saved.
    /// The task resolves with what to tell the user when it was not saved.
    pub fn add_level(
        &mut self,
        market: String,
        price: Decimal,
        direction: Option<Direction>,
        note: Option<String>,
        last: Option<Decimal>,
        cx: &mut Context<Self>,
    ) -> Task<Result<WatcherConfig, String>> {
        let lookup: Task<Result<Option<Decimal>, String>> = match last {
            Some(last) => Task::ready(Ok(Some(last))),
            None => self.last_price(&market, cx),
        };
        cx.spawn(async move |this, cx| {
            let last = match lookup.await? {
                Some(last) => last,
                None => {
                    return Err(format!(
                        "{market} is not listed on the venue, so no level was saved."
                    ));
                }
            };
            let direction = direction.unwrap_or_else(|| Direction::from_last(price, last));
            let level = Level::new(market, price, direction, note);
            let saving = this
                .update(cx, |link, cx| link.save(move |c| c.levels.push(level), cx))
                .map_err(|_| "The watcher connection closed.".to_string())?;
            saving.await.map_err(|e| e.to_string())
        })
    }

    /// The venue's mid price of `market`, or `None` when it isn't listed.
    #[cfg_attr(test, allow(unused_variables))]
    fn last_price(
        &self,
        market: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Option<Decimal>, String>> {
        #[cfg(test)]
        {
            Task::ready(Ok(self.mids.get(market).copied()))
        }
        #[cfg(not(test))]
        {
            let id = galata_desk_core::market::MarketId::new(market);
            let core = Core::get(cx);
            let (runtime, url) = (core.runtime.clone(), core.info_url());
            let asking = runtime.spawn(async move {
                let info =
                    galata_desk_core::venue::Info::new(galata_desk_core::venue::Http::at(url));
                info.mid(id.dex(), id.ticker()).await
            });
            let market = market.to_string();
            cx.background_spawn(async move {
                match asking.await {
                    Ok(Ok(mid)) => Ok(mid),
                    Ok(Err(e)) => Err(format!(
                        "{market}'s price could not be read: {}",
                        crate::ui::explain(&e)
                    )),
                    Err(_) => Err(format!("{market}'s price could not be read.")),
                }
            })
        }
    }
}

/// "40 s ago", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(now_ms: u64, then_ms: u64) -> String {
    let s = now_ms.saturating_sub(then_ms) / 1000;
    match s {
        0..60 => format!("{s} s ago"),
        60..3_600 => format!("{} min ago", s / 60),
        3_600..86_400 => format!("{} h ago", s / 3_600),
        _ => match s / 86_400 {
            1 => "1 day ago".to_string(),
            d => format!("{d} days ago"),
        },
    }
}

/// Now, in milliseconds since the epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// What "Test connection" says about the answers it got.
pub fn describe_test(
    url: &WatcherUrl,
    answer: Result<
        (
            galata_desk_core::watcher::Health,
            Result<WatcherConfig, WatcherError>,
        ),
        WatcherError,
    >,
    now_ms: u64,
) -> (String, bool) {
    match answer {
        Err(WatcherError::Unreachable) => (format!("Nothing answered at {url}. Is the watcher running?"), true),
        Err(e) => (e.to_string(), true),
        Ok((_, Err(WatcherError::NoConfig))) => (
            "Connected, but the watcher has no configuration yet. Write its first one, with its Telegram chat, as galata-watcher's README shows.".into(),
            false,
        ),
        Ok((_, Err(e))) => (e.to_string(), true),
        Ok((health, Ok(config))) => {
            let checked = health
                .last_ok_ms
                .map(|t| format!("last checked {}", ago(now_ms, t)))
                .unwrap_or_else(|| "it has not checked yet".into());
            (
                format!(
                    "Connected. Watching {} on {}, configuration version {}, {checked}.",
                    short_address(&config.address),
                    config.network.label().to_lowercase(),
                    config.version,
                ),
                false,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_reads_naturally() {
        let now = 10_000_000_000;
        assert_eq!(ago(now, now - 40_000), "40 s ago");
        assert_eq!(ago(now, now - 5 * 60_000), "5 min ago");
        assert_eq!(ago(now, now - 3 * 3_600_000), "3 h ago");
        assert_eq!(ago(now, now - 86_400_000), "1 day ago");
        assert_eq!(ago(now, now + 5_000), "0 s ago");
    }
}
