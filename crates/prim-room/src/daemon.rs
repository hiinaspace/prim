//! Glue: state dir + identity + transport + `Room` + driver + local socket.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use room_core::api::{Config, Timing};
use room_core::frame::caps;
use room_core::ids::{Identity, Key, Now, RoomId};
use room_core::room::{Persisted, Room};
use room_core::token::InviteToken;
use tokio::task::JoinHandle;

use crate::driver::{Driver, DriverConfig, DriverHandle};
use crate::local;
use crate::state::{self, StateDir};
use crate::transport::Transport;

pub enum Start {
    /// Found a new room; `meta` is the genesis metadata (a petname hint).
    Found { meta: Vec<u8> },
    /// Join with an invite token (text form).
    Join(String),
    /// Resume the room in the state dir.
    Resume,
}

pub struct DaemonOpts {
    /// Explicit state dir. Otherwise `$XDG_STATE_HOME/prim/rooms/<room-hex>/`.
    pub state_dir: Option<PathBuf>,
    /// Room to resume when `state_dir` is not given.
    pub resume_room: Option<RoomId>,
    /// Local API socket. `None` runs without one (tests drive the handle directly).
    pub socket: Option<PathBuf>,
    pub timing: Timing,
    pub persist_debounce: Duration,
}

impl Default for DaemonOpts {
    fn default() -> Self {
        Self {
            state_dir: None,
            resume_room: None,
            socket: None,
            timing: Timing::default(),
            persist_debounce: Duration::from_millis(500),
        }
    }
}

pub struct Daemon {
    pub handle: DriverHandle,
    pub key: Key,
    /// `RoomId::ZERO` never occurs: for a join it comes from the token.
    pub room: RoomId,
    pub state_dir: PathBuf,
    pub socket: Option<PathBuf>,
    pub incarnation: u32,
    task: JoinHandle<()>,
    api_task: Option<JoinHandle<()>>,
}

fn now() -> Now {
    use std::time::{SystemTime, UNIX_EPOCH};
    // The driver's monotonic clock starts at 0 right after this.
    Now {
        mono_us: 0,
        unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    }
}

impl Daemon {
    /// Start a daemon. `make_transport` gets the member identity (its public key must
    /// be the transport's endpoint id) and binds the network side.
    pub async fn start<T, F, Fut>(
        opts: DaemonOpts,
        start: Start,
        make_transport: F,
    ) -> Result<Daemon>
    where
        T: Transport,
        F: FnOnce(Identity) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        // Parse the token first: it names the room, hence the default state dir.
        let token = match &start {
            Start::Join(t) => {
                Some(InviteToken::decode(t).map_err(|e| anyhow::anyhow!("bad invite token: {e}"))?)
            }
            _ => None,
        };
        let (mut dir, relocate) = match (&opts.state_dir, &start) {
            (Some(p), _) => (StateDir::open(p)?, false),
            (None, Start::Found { .. }) => (StateDir::open(state::staging_dir()?)?, true),
            (None, Start::Join(_)) => (
                StateDir::open(state::room_dir(&token.as_ref().unwrap().room))?,
                false,
            ),
            (None, Start::Resume) => match opts.resume_room {
                Some(r) => (StateDir::open(state::room_dir(&r))?, false),
                None => bail!("resume needs a state dir or a room id"),
            },
        };
        let identity = dir.load_or_create_identity().context("identity.key")?;
        let key = identity.key();
        // Bump before the Room exists: it is the term of every record we write.
        let incarnation = dir.bump_incarnation().context("incarnation")?;
        let transport = make_transport(identity.clone()).await?;
        if transport.local_key() != key {
            bail!("transport key does not match the member identity");
        }
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| anyhow::anyhow!("entropy: {e}"))?;
        let mut daemon_incarnation = [0u8; 8];
        getrandom::fill(&mut daemon_incarnation).map_err(|e| anyhow::anyhow!("entropy: {e}"))?;
        let config = Config {
            identity,
            member_incarnation: incarnation,
            daemon_incarnation: u64::from_le_bytes(daemon_incarnation),
            entropy_seed: seed,
            timing: opts.timing.clone(),
            caps: caps::R1,
        };
        let stored = dir.read_room().context("room.bin")?;
        let had_stored = stored.is_some();
        let room = match start {
            Start::Found { meta } => {
                if stored.is_some() {
                    bail!("{} already holds a room; use resume", dir.path.display());
                }
                Room::found(config, now(), &meta)
            }
            Start::Join(_) => {
                if stored.is_some() {
                    bail!("{} already holds a room; use resume", dir.path.display());
                }
                Room::join(config, token.unwrap(), now())
            }
            Start::Resume => {
                let bytes = stored.context("nothing to resume: no room.bin in the state dir")?;
                let p = Persisted::decode(&bytes).map_err(|e| anyhow::anyhow!("room.bin: {e}"))?;
                Room::resume(config, p, now()).map_err(|e| anyhow::anyhow!("room.bin: {e}"))?
            }
        };
        let room_id = room.room_id();
        if relocate {
            let to = state::room_dir(&room_id);
            dir.relocate(to)?;
        }
        if !had_stored && !is_join_pending(&room) {
            // A newly founded room is saved at once: a crash right after must not lose it.
            state::atomic_write(&dir.room_path(), &room.persist().encode(), 0o600)?;
        }
        let (handle, task) = Driver::spawn(
            room,
            transport,
            DriverConfig {
                persist_path: Some(dir.room_path()),
                persist_debounce: opts.persist_debounce,
                ..Default::default()
            },
        );
        let api_task = match &opts.socket {
            Some(path) => {
                let listener = local::bind(path)
                    .await
                    .with_context(|| path.display().to_string())?;
                Some(tokio::spawn(local::serve(listener, handle.clone())))
            }
            None => None,
        };
        Ok(Daemon {
            handle,
            key,
            room: room_id,
            state_dir: dir.path.clone(),
            socket: opts.socket,
            incarnation,
            task,
            api_task,
        })
    }

    /// Resolves when the driver has stopped.
    pub async fn wait(&mut self) {
        let _ = (&mut self.task).await;
    }

    pub async fn shutdown(mut self) {
        self.handle.shutdown().await;
        let _ = (&mut self.task).await;
        if let Some(t) = self.api_task.take() {
            t.abort();
        }
        if let Some(p) = &self.socket {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A joiner has no genesis yet, so there is nothing to persist until the first records
/// arrive (the driver saves on `PersistDirty`).
fn is_join_pending(room: &Room) -> bool {
    room.grants().is_none()
}
