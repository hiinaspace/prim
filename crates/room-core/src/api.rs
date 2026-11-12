//! The sans-IO interface of [`crate::room::Room`].
//!
//! The driver (daemon, simulator, tests) feeds [`Input`]s with the current [`Now`]
//! and drains [`Output`]s. The core never blocks, never reads a clock and never does
//! IO. `poll_timeout` says when the driver must call `handle(now, Input::Tick)` next.

use crate::frame::{AckStatus, Reason};
use crate::grants::Epoch;
use crate::ids::{Identity, Key, RoomId};
use crate::record::Kind;
use crate::staterec::{Class, Lifetime, Scope, Version};

/// Driver-assigned id of one transport connection. Unique for the driver's lifetime.
pub type ConnId = u64;
/// Driver-assigned id of one local API client session (CONTRACTS.md section 8).
pub type ClientId = u64;
/// Client-chosen id correlating a command with its reply.
pub type ReqId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lane {
    /// Highest strict priority: hello, records, presence, acks.
    Control = 0,
    /// State deltas and messages.
    State = 1,
    /// Reserved: bulk streams.
    Bulk = 2,
}

#[derive(Clone, Debug)]
pub struct Timing {
    pub presence_interval_ms: u64,
    pub presence_expiry_ms: u64,
    pub coordinator_liveness_ms: u64,
    pub rejoin_grace_ms: u64,
    /// How long to wait for a hello before closing a connection.
    pub hello_timeout_ms: u64,
    /// Redial interval for members we are not connected to.
    pub redial_ms: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            presence_interval_ms: 5_000,
            presence_expiry_ms: 15_000,
            coordinator_liveness_ms: 20_000,
            rejoin_grace_ms: 30_000,
            hello_timeout_ms: 5_000,
            redial_ms: 2_000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub identity: Identity,
    /// Persisted counter, bumped by the driver on every daemon start. It is the
    /// member-scope `term` of every record this daemon writes.
    pub member_incarnation: u32,
    /// Random per process.
    pub daemon_incarnation: u64,
    /// Seed for nonces. The daemon takes it from the OS, the simulator from its seed.
    pub entropy_seed: [u8; 32],
    pub timing: Timing,
    /// Hello capability bits this node advertises.
    pub caps: u64,
}

/// What the driver tells the core.
#[derive(Clone, Debug)]
pub enum Input {
    /// A transport connection is up. `peer` is authenticated by the transport.
    /// `outbound` is true when this node dialled.
    Connected {
        conn: ConnId,
        peer: Key,
        outbound: bool,
    },
    /// Bytes of one complete frame (length prefix included) on `lane`.
    Frame {
        conn: ConnId,
        lane: Lane,
        bytes: Vec<u8>,
    },
    Datagram {
        conn: ConnId,
        bytes: Vec<u8>,
    },
    /// The connection is gone (closed by either side, timed out, or failed).
    Disconnected {
        conn: ConnId,
    },
    /// A dial requested by `Output::Dial` failed.
    DialFailed {
        peer: Key,
    },
    Tick,
    Local {
        client: ClientId,
        cmd: LocalCommand,
    },
}

/// What the core asks the driver to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Dial {
        peer: Key,
        hints: Vec<String>,
    },
    /// One encoded frame (with length prefix) on `lane` of `conn`.
    Send {
        conn: ConnId,
        lane: Lane,
        bytes: Vec<u8>,
    },
    /// A datagram. The driver drops it if it is older than `max_age_ms` when it
    /// reaches the head of the send queue (0 = no limit).
    SendDatagram {
        conn: ConnId,
        bytes: Vec<u8>,
        max_age_ms: u32,
    },
    /// Close after flushing frames already queued on `conn`.
    Close {
        conn: ConnId,
        reason: Reason,
    },
    Local {
        client: ClientId,
        event: LocalEvent,
    },
    /// `conn` became an established connection to the member `peer`; `caps` are the
    /// hello capability bits the peer advertised. The driver keeps this as the
    /// membership view of its datagram fast path (voice never enters the core).
    Established {
        conn: ConnId,
        peer: Key,
        caps: u64,
    },
    /// `conn` is no longer an established member connection (closed, rejected,
    /// replaced). Always follows an `Established` for the same `conn`.
    Unestablished {
        conn: ConnId,
    },
    /// Persistent state changed; the driver should save [`crate::room::Room::persist`] soon.
    PersistDirty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum To {
    Member(Key),
    All,
    Coordinator,
}

#[derive(Clone, Debug)]
pub enum LocalCommand {
    Attach,
    Detach,
    Subscribe {
        req: ReqId,
        sub: u32,
        pattern: String,
    },
    Unsubscribe {
        sub: u32,
    },
    Write {
        req: ReqId,
        scope: Scope,
        name: String,
        class: Class,
        lifetime: Lifetime,
        value: Vec<u8>,
    },
    RegisterChannel {
        req: ReqId,
        channel: u16,
    },
    UnregisterChannel {
        channel: u16,
    },
    Send {
        req: ReqId,
        to: To,
        channel: u16,
        reliable: bool,
        payload: Vec<u8>,
    },
    SendDatagram {
        to: To,
        channel: u16,
        payload: Vec<u8>,
        max_age_ms: u32,
    },
    Status {
        req: ReqId,
    },
    /// Mint an invite token for the current open generation.
    CreateInvite {
        req: ReqId,
        expiry_ms: u64,
        relays: Vec<String>,
    },
    Grant {
        req: ReqId,
        kind: Kind,
        subject: Key,
        expiry_ms: u64,
    },
    Revoke {
        req: ReqId,
        subject: Key,
    },
    Retire {
        req: ReqId,
        subject: Key,
        keep_all: bool,
    },
    Close {
        req: ReqId,
    },
    Reopen {
        req: ReqId,
    },
    Leave {
        req: ReqId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    NotAttached,
    NoRoom,
    BadPattern,
    BadName,
    TooLarge,
    OverBudget,
    NotCoordinator,
    NoCoordinator,
    NotAuthorized,
    Offline,
    Closed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Ok,
    Error(ErrorCode),
    /// `Send` accepted; a `MessageResult` per target follows.
    Sending {
        msg_id: u64,
        targets: Vec<Key>,
    },
    Status(Box<Status>),
    Invite {
        token: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// The subscription restarted (after attach, reconnect or partition heal);
    /// a full snapshot of matching keys follows.
    Reset,
    Value {
        path: String,
        writer: Key,
        version: Version,
        class: Class,
        value: Vec<u8>,
    },
    Removed {
        path: String,
    },
    /// End of the snapshot that followed a `Reset`.
    SnapshotDone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberEvent {
    Joined,
    Online,
    Offline,
    /// Presence expired without an explicit offline.
    Unresponsive,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordinatorEvent {
    Changed {
        epoch: Epoch,
        holder: Key,
    },
    /// The holder signed offline or left.
    Left {
        epoch: Epoch,
        holder: Key,
    },
    /// The holder's presence is older than `coordinator_liveness_ms`.
    Unresponsive {
        epoch: Epoch,
        holder: Key,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum LocalEvent {
    Reply {
        req: ReqId,
        reply: Reply,
    },
    /// Subscription `sub` has coalesced updates; call `Room::drain(client, sub, max)`.
    Ready {
        sub: u32,
    },
    Message {
        from: Key,
        channel: u16,
        payload: Vec<u8>,
    },
    Datagram {
        from: Key,
        channel: u16,
        payload: Vec<u8>,
        age_ms: u32,
    },
    MessageResult {
        msg_id: u64,
        to: Key,
        status: MessageStatus,
    },
    Member {
        key: Key,
        event: MemberEvent,
    },
    Coordinator(CoordinatorEvent),
    RoomClosed {
        generation: u32,
    },
    RoomOpened {
        generation: u32,
    },
    /// This node was removed from the room.
    Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageStatus {
    Acked(AckStatus),
    /// The connection went away before the ack (delivery unknown).
    Lost,
    Offline,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MemberInfo {
    pub key: Key,
    pub keyholder: bool,
    pub inviter: bool,
    pub connected: bool,
    pub presence: Option<crate::staterec::Presence>,
    pub fresh: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub room: Option<RoomId>,
    pub me: Key,
    pub lifecycle: crate::grants::Lifecycle,
    pub coordinator: Option<(Epoch, Key)>,
    pub members: Vec<MemberInfo>,
    pub records: usize,
    pub durable_bytes_room: usize,
    pub durable_bytes_mine: usize,
}
