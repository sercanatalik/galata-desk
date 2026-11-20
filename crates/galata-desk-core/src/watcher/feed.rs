//! Keeping the watcher's health, configuration and alerts current, and
//! saving edits one at a time.
//!
//! **Stale, not empty.** When a read fails, the snapshot keeps the last
//! health, configuration and alerts it had, says what went wrong, and keeps
//! the time of the last good read, so a view can show them marked as stale.
//!
//! **One save at a time, never retried.** An edit is applied to the latest
//! configuration and sent with its version. A refusal because the
//! configuration changed elsewhere is not retried against the newer one: the
//! feed reads again, and the user sees the newer configuration and decides.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::client::{Client, WatcherError};
use super::types::{AlertRow, Health, WatcherConfig};

/// How often the watcher is read.
pub const EVERY: Duration = Duration::from_secs(15);

/// How many of the latest alerts a snapshot keeps.
pub const ALERTS_KEPT: usize = 200;

/// Why the last read failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    Unreachable,
    TokenRefused,
    /// Some other answer, by its status.
    Unexpected(u16),
}

impl Problem {
    fn of(e: &WatcherError) -> Self {
        match e {
            WatcherError::Unreachable => Self::Unreachable,
            WatcherError::TokenRefused => Self::TokenRefused,
            WatcherError::Unexpected { status } => Self::Unexpected(*status),
            // Not answers to a read.
            WatcherError::NoConfig
            | WatcherError::Changed { .. }
            | WatcherError::Refused { .. } => Self::Unexpected(0),
        }
    }

    pub fn message(self) -> String {
        match self {
            Self::Unreachable => WatcherError::Unreachable.to_string(),
            Self::TokenRefused => WatcherError::TokenRefused.to_string(),
            Self::Unexpected(status) => WatcherError::Unexpected { status }.to_string(),
        }
    }
}

/// What the watcher's configuration is known to be.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ConfigState {
    /// Not read yet.
    #[default]
    Unknown,
    /// The watcher has never been configured.
    Missing,
    Read(WatcherConfig),
}

impl ConfigState {
    pub fn config(&self) -> Option<&WatcherConfig> {
        match self {
            Self::Read(c) => Some(c),
            _ => None,
        }
    }
}

/// The watcher as last read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WatcherSnapshot {
    /// When everything was last read successfully, milliseconds since the
    /// epoch. `None` before the first success.
    pub reached_ms: Option<u64>,
    /// Why the latest read failed. `None` when it succeeded, and before the
    /// first read.
    pub problem: Option<Problem>,
    pub health: Option<Health>,
    pub config: ConfigState,
    /// Newest first.
    pub alerts: Vec<AlertRow>,
}

impl WatcherSnapshot {
    /// Edits are offered only while the watcher answers and is configured.
    pub fn editable(&self) -> Option<&WatcherConfig> {
        if self.problem.is_some() {
            return None;
        }
        self.config.config()
    }
}

type Edit = Box<dyn FnOnce(&mut WatcherConfig) + Send>;

enum Command {
    RefreshNow,
    Save(Edit, oneshot::Sender<Result<WatcherConfig, WatcherError>>),
}

/// A running reader of one watcher. Dropping it stops the reading.
pub struct WatcherFeed {
    snapshots: watch::Receiver<WatcherSnapshot>,
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

impl WatcherFeed {
    /// Start reading the watcher every [`EVERY`] on the current Tokio
    /// runtime, the first time at once.
    pub fn start(client: Client) -> Self {
        Self::start_every(client, EVERY)
    }

    /// The same, at another interval (for tests).
    pub fn start_every(client: Client, every: Duration) -> Self {
        let (tx, snapshots) = watch::channel(WatcherSnapshot::default());
        let (commands, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(client, every, tx, rx));
        Self {
            snapshots,
            commands,
            task,
        }
    }

    /// A receiver that always holds the latest snapshot.
    pub fn snapshots(&self) -> watch::Receiver<WatcherSnapshot> {
        self.snapshots.clone()
    }

    /// Read everything again at once.
    pub fn refresh_now(&self) {
        let _ = self.commands.send(Command::RefreshNow);
    }

    /// Apply `edit` to the latest configuration and save it with that
    /// configuration's version. Resolves with the configuration as saved, or
    /// why it wasn't: [`WatcherError::Changed`] when it changed elsewhere,
    /// [`WatcherError::Refused`] with the watcher's problems,
    /// [`WatcherError::NoConfig`] when there is no configuration to edit.
    /// Saves run one at a time, in the order asked. The future needs no
    /// runtime of its own to be awaited.
    pub fn save(
        &self,
        edit: impl FnOnce(&mut WatcherConfig) + Send + 'static,
    ) -> impl Future<Output = Result<WatcherConfig, WatcherError>> + Send + 'static {
        let (reply, answer) = oneshot::channel();
        let sent = self
            .commands
            .send(Command::Save(Box::new(edit), reply))
            .is_ok();
        async move {
            if !sent {
                return Err(WatcherError::Unreachable);
            }
            answer.await.unwrap_or(Err(WatcherError::Unreachable))
        }
    }
}

impl Drop for WatcherFeed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Read health, configuration and alerts into `snapshot`, keeping what it
/// had when a read fails.
async fn read(client: &Client, snapshot: &mut WatcherSnapshot) {
    let result = async {
        let health = client.health().await?;
        let config = match client.config().await {
            Ok(c) => ConfigState::Read(c),
            Err(WatcherError::NoConfig) => ConfigState::Missing,
            Err(e) => return Err(e),
        };
        let mut alerts = client.alerts(0).await?;
        alerts.truncate(ALERTS_KEPT);
        Ok((health, config, alerts))
    }
    .await;
    match result {
        Ok((health, config, alerts)) => {
            snapshot.health = Some(health);
            snapshot.config = config;
            snapshot.alerts = alerts;
            snapshot.problem = None;
            snapshot.reached_ms = Some(now_ms());
        }
        Err(e) => snapshot.problem = Some(Problem::of(&e)),
    }
}

async fn run(
    client: Client,
    every: Duration,
    tx: watch::Sender<WatcherSnapshot>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let mut snapshot = WatcherSnapshot::default();
    let mut ticks = tokio::time::interval(every);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let command = tokio::select! {
            _ = ticks.tick() => None,
            c = commands.recv() => match c {
                Some(c) => Some(c),
                None => return,
            },
        };
        match command {
            None | Some(Command::RefreshNow) => {}
            Some(Command::Save(edit, reply)) => {
                let outcome = match snapshot.config.config().cloned() {
                    None => Err(WatcherError::NoConfig),
                    Some(mut config) => {
                        edit(&mut config);
                        client.put_config(&config).await
                    }
                };
                if let Ok(saved) = &outcome {
                    // Shown at once; the read below brings the rest.
                    snapshot.config = ConfigState::Read(saved.clone());
                }
                // Reply after the reread, so a caller that then looks at the
                // snapshot sees the result or the newer configuration.
                read(&client, &mut snapshot).await;
                tx.send_replace(snapshot.clone());
                let _ = reply.send(outcome);
                ticks.reset();
                continue;
            }
        }
        read(&client, &mut snapshot).await;
        tx.send_replace(snapshot.clone());
    }
}
