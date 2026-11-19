//! Keeping the History section's venue-backed tabs current: orders, funding
//! payments and the ledger. Trades are the fills the Portfolio feed already
//! walks, so nothing here asks for them.
//!
//! A tab is loaded the first time it is shown and when the period widens
//! past what it holds, and refreshed every minute only while it is shown.
//! A refresh that fails keeps the rows and records why.

use std::time::{Duration, SystemTime};

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::loader::{PageSpec, Paged, Timed, walk_with};
use super::orders;
use crate::feed::Section;
use crate::market::history::Budget;
use crate::venue::{HistoricalOrder, Info, LedgerRow, Transport, UserFundingRow, VenueError};

/// How often a shown tab is refreshed.
pub const REFRESH: Duration = Duration::from_secs(60);

/// The section's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Tab {
    #[default]
    Trades,
    Orders,
    Funding,
    Transfers,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Trades, Tab::Orders, Tab::Funding, Tab::Transfers];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Trades => "Trades",
            Tab::Orders => "Orders",
            Tab::Funding => "Funding payments",
            Tab::Transfers => "Deposits and withdrawals",
        }
    }
}

/// The orders held, with from when they are shown when the venue's answer
/// was full.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrdersHeld {
    pub rows: Vec<HistoricalOrder>,
    pub shown_from: Option<u64>,
}

/// What the History section shows of the venue, as last fetched.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HistorySnapshot {
    pub orders: Section<OrdersHeld>,
    pub funding: Section<Paged<UserFundingRow>>,
    pub ledger: Section<Paged<LedgerRow>>,
}

#[derive(Debug)]
enum Command {
    /// The section shows `tab`, over a period from `since_ms` (`0` for
    /// All); `None` when the section is not shown.
    Show {
        tab: Option<Tab>,
        since_ms: u64,
    },
    RefreshNow,
}

/// The History section's loader, running on the I/O runtime.
pub struct HistoryFeed {
    snapshots: watch::Receiver<HistorySnapshot>,
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

impl HistoryFeed {
    pub fn start<T: Transport + Clone>(transport: T, user: impl Into<String>) -> Self {
        let (tx, snapshots) = watch::channel(HistorySnapshot::default());
        let (commands, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(Info::new(transport), user.into(), tx, rx));
        HistoryFeed {
            snapshots,
            commands,
            task,
        }
    }

    pub fn snapshots(&self) -> watch::Receiver<HistorySnapshot> {
        self.snapshots.clone()
    }

    /// Say which tab is shown, and from when the period starts. `None`
    /// when the section is hidden: nothing is refreshed then.
    pub fn show(&self, tab: Option<Tab>, since_ms: u64) {
        let _ = self.commands.send(Command::Show { tab, since_ms });
    }

    /// Refresh the shown tab at once.
    pub fn refresh_now(&self) {
        let _ = self.commands.send(Command::RefreshNow);
    }
}

impl Drop for HistoryFeed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

fn record<T>(section: &mut Section<T>, result: Result<T, VenueError>) {
    match result {
        Ok(data) => {
            section.data = Some(data);
            section.fetched_at = Some(SystemTime::now());
            section.error = None;
        }
        // The last rows stay; the failure is recorded beside them.
        Err(e) => section.error = Some(e),
    }
}

/// Load what `tab` lacks to cover from `since_ms`, or with `top_up` what is
/// newer than its newest row.
#[allow(clippy::too_many_arguments)]
async fn load<T: Transport + Clone>(
    info: &Info<T>,
    user: &str,
    snapshot: &mut HistorySnapshot,
    tab: Tab,
    since_ms: u64,
    top_up: bool,
    budget: &mut Budget,
    publish: &impl Fn(&HistorySnapshot),
) {
    match tab {
        Tab::Trades => {}
        Tab::Orders => {
            if !top_up && snapshot.orders.data.is_some() {
                return;
            }
            let first = snapshot.orders.data.is_none();
            let result = info.historical_orders(user).await.map(|answer| {
                let mut held = snapshot.orders.data.clone().unwrap_or_default();
                // A full first answer cannot be paged back: it is as far as
                // orders go. Later answers add newer rows to those held, so
                // they never reach less far.
                if first {
                    held.shown_from = orders::shown_from(answer.len(), &answer);
                }
                orders::merge(&mut held.rows, answer);
                held
            });
            record(&mut snapshot.orders, result);
        }
        Tab::Funding => {
            let held = snapshot.funding.data.clone().unwrap_or_default();
            let ask = |s, e| info.user_funding(user, s, e);
            // Rows are shown page by page while a long history loads.
            let mut shown = snapshot.clone();
            let progress = |partial: Paged<UserFundingRow>| {
                shown.funding.data = Some(partial);
                publish(&shown);
            };
            let result = paged(
                held,
                since_ms,
                top_up,
                PageSpec::FUNDING,
                budget,
                ask,
                progress,
            )
            .await;
            record(&mut snapshot.funding, result);
        }
        Tab::Transfers => {
            let held = snapshot.ledger.data.clone().unwrap_or_default();
            let ask = |s, e| info.user_ledger(user, s, e);
            let mut shown = snapshot.clone();
            let progress = |partial: Paged<LedgerRow>| {
                shown.ledger.data = Some(partial);
                publish(&shown);
            };
            let result = paged(
                held,
                since_ms,
                top_up,
                PageSpec::LEDGER,
                budget,
                ask,
                progress,
            )
            .await;
            record(&mut snapshot.ledger, result);
        }
    }
}

/// Load what `held` lacks, telling `progress` the rows so far after each
/// page of a long load: their `stopped_at` is how far loading has reached.
async fn paged<R, F, Fut>(
    mut held: Paged<R>,
    since_ms: u64,
    top_up: bool,
    spec: PageSpec,
    budget: &mut Budget,
    mut ask: F,
    mut progress: impl FnMut(Paged<R>),
) -> Result<Paged<R>, VenueError>
where
    R: Timed,
    F: FnMut(u64, Option<u64>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<R>, VenueError>>,
{
    let (start, end, widening) = match held.gap(since_ms) {
        Some((start, end)) => (start, end, true),
        None if top_up => (held.newest_ms().unwrap_or(since_ms), None, false),
        None => return Ok(held),
    };
    let on_page = |rows: &[R]| {
        let mut partial = held.clone();
        partial.merge(rows.to_vec());
        if widening {
            partial.from_ms = Some(start);
        }
        partial.stopped_at = rows.last().map(Timed::time_ms);
        progress(partial);
    };
    let (rows, stopped) = walk_with(&mut ask, start, end, spec, budget, on_page).await?;
    held.merge(rows);
    if widening {
        held.from_ms = Some(start);
    }
    if end.is_none() {
        held.stopped_at = stopped;
    }
    Ok(held)
}

async fn run<T: Transport + Clone>(
    info: Info<T>,
    user: String,
    tx: watch::Sender<HistorySnapshot>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let mut snapshot = HistorySnapshot::default();
    let mut budget = Budget::for_account_history();
    let mut shown: Option<Tab> = None;
    let mut since_ms = 0;
    let mut due = Instant::now() + REFRESH;
    let publish = |snapshot: &HistorySnapshot| {
        tx.send_if_modified(|current| {
            let changed = current != snapshot;
            if changed {
                *current = snapshot.clone();
            }
            changed
        });
    };
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return };
                match command {
                    Command::Show { tab, since_ms: since } => {
                        shown = tab;
                        since_ms = since;
                        if let Some(tab) = tab {
                            // A first show, or a wider period, loads now.
                            load(&info, &user, &mut snapshot, tab, since_ms, false, &mut budget, &publish).await;
                            publish(&snapshot);
                            due = Instant::now() + REFRESH;
                        }
                    }
                    Command::RefreshNow => {
                        if let Some(tab) = shown {
                            load(&info, &user, &mut snapshot, tab, since_ms, true, &mut budget, &publish).await;
                            publish(&snapshot);
                            due = Instant::now() + REFRESH;
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(due), if shown.is_some() => {
                if let Some(tab) = shown {
                    load(&info, &user, &mut snapshot, tab, since_ms, true, &mut budget, &publish).await;
                    publish(&snapshot);
                }
                due = Instant::now() + REFRESH;
            }
        }
    }
}

/// The time now, in milliseconds, for callers that set the period.
pub fn now() -> u64 {
    now_ms()
}
