//! The realtime socket from the client side: a `SOCK_SEQPACKET` connection carrying
//! one `RtMsg` per packet, and the lease handshake.

use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::time::Duration;

use room_core::ids::Key;
use room_core::rt::{refuse, Role, RtMsg, RT_MAX_MESSAGE, RT_PROTO};

use crate::clock::mono_us;

pub struct RtConn {
    fd: OwnedFd,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    /// The socket buffer is full; the message was not sent (the age bound applies).
    Full,
    Closed,
}

impl RtConn {
    pub fn from_fd(fd: OwnedFd) -> RtConn {
        RtConn { fd }
    }

    /// `$XDG_RUNTIME_DIR/prim/rt.sock`, next to the daemon's `room.sock`.
    pub fn default_path() -> std::path::PathBuf {
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            Some(d) => std::path::PathBuf::from(d).join("prim").join("rt.sock"),
            None => {
                use std::os::unix::fs::MetadataExt;
                let uid = std::fs::metadata("/proc/self")
                    .map(|m| m.uid().to_string())
                    .unwrap_or_else(|_| "0".into());
                std::path::PathBuf::from(format!("/tmp/prim-{uid}/rt.sock"))
            }
        }
    }

    pub fn connect(path: &Path) -> io::Result<RtConn> {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        // SAFETY: plain libc calls; the fd is wrapped immediately so it cannot leak.
        unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = OwnedFd::from_raw_fd(fd);
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
            if bytes.len() >= addr.sun_path.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "socket path too long",
                ));
            }
            for (i, b) in bytes.iter().enumerate() {
                addr.sun_path[i] = *b as libc::c_char;
            }
            let len = std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1;
            if libc::connect(
                fd.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                len as libc::socklen_t,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            // The client-to-daemon bound is a documented 64 KiB, not the system default.
            let buf: libc::c_int = 64 * 1024;
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &buf as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            Ok(RtConn { fd })
        }
    }

    /// A connected pair (tests: one end plays the daemon).
    pub fn pair() -> io::Result<(RtConn, RtConn)> {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` has room for the two descriptors socketpair writes.
        unsafe {
            if libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok((
                RtConn::from_fd(OwnedFd::from_raw_fd(fds[0])),
                RtConn::from_fd(OwnedFd::from_raw_fd(fds[1])),
            ))
        }
    }

    /// Half-close both directions so the peer sees the end and local senders fail.
    pub fn shutdown(&self) {
        // SAFETY: shutdown on an fd we own; errors (already closed) are irrelevant.
        unsafe {
            libc::shutdown(self.fd.as_raw_fd(), libc::SHUT_RDWR);
        }
    }

    pub fn send(&self, m: &RtMsg) -> SendOutcome {
        let b = m.encode();
        // SAFETY: `b` is valid for `b.len()` bytes for the duration of the call.
        let r = unsafe {
            libc::send(
                self.fd.as_raw_fd(),
                b.as_ptr() as *const libc::c_void,
                b.len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if r >= 0 {
            return SendOutcome::Sent;
        }
        match io::Error::last_os_error().kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => SendOutcome::Full,
            _ => SendOutcome::Closed,
        }
    }

    /// Wait up to `timeout` for one message. `Ok(None)` is a timeout; a closed peer is
    /// an error.
    pub fn recv(&self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let mut pfd = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let n = unsafe {
            libc::poll(
                &mut pfd,
                1,
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(e)
            };
        }
        if n == 0 {
            return Ok(None);
        }
        let mut buf = vec![0u8; RT_MAX_MESSAGE + 64];
        // SAFETY: `buf` is valid for its length.
        let r = unsafe {
            libc::recv(
                self.fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if r < 0 {
            let e = io::Error::last_os_error();
            return if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                Ok(None)
            } else {
                Err(e)
            };
        }
        if r == 0 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "realtime socket closed",
            ));
        }
        buf.truncate(r as usize);
        Ok(Some(buf))
    }
}

/// The daemon's answer to a `Hello`.
#[derive(Clone, Copy, Debug)]
pub struct Lease {
    pub self_key: Key,
    pub lease_ttl_ms: u32,
    pub session: u64,
}

#[derive(Debug)]
pub enum HandshakeError {
    Refused(u8),
    Revoked(u8),
    Io(io::Error),
    Timeout,
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HandshakeError::Refused(r) if *r == refuse::BUSY => {
                write!(
                    f,
                    "lease refused: another client holds this role (use --takeover)"
                )
            }
            HandshakeError::Refused(r) => write!(f, "lease refused (reason {r})"),
            HandshakeError::Revoked(r) => {
                write!(f, "lease revoked during the handshake (reason {r})")
            }
            HandshakeError::Io(e) => write!(f, "realtime socket: {e}"),
            HandshakeError::Timeout => write!(f, "no Welcome from the daemon within 5 s"),
        }
    }
}

impl std::error::Error for HandshakeError {}

pub fn handshake(
    conn: &RtConn,
    role: Role,
    takeover: bool,
    incarnation: u32,
    flags: u8,
) -> Result<Lease, HandshakeError> {
    let hello = RtMsg::Hello {
        proto: RT_PROTO,
        role,
        takeover,
        incarnation,
        flags,
    };
    if conn.send(&hello) != SendOutcome::Sent {
        return Err(HandshakeError::Io(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "send Hello",
        )));
    }
    let deadline = mono_us() + 5_000_000;
    loop {
        let left = deadline.saturating_sub(mono_us());
        if left == 0 {
            return Err(HandshakeError::Timeout);
        }
        match conn.recv(Duration::from_micros(left)) {
            Ok(None) => continue,
            Ok(Some(b)) => match RtMsg::decode(&b) {
                Ok(RtMsg::Welcome {
                    self_key,
                    lease_ttl_ms,
                    session,
                    ..
                }) => {
                    return Ok(Lease {
                        self_key,
                        lease_ttl_ms,
                        session,
                    })
                }
                Ok(RtMsg::Refused { reason }) => return Err(HandshakeError::Refused(reason)),
                Ok(RtMsg::Revoked { reason }) => return Err(HandshakeError::Revoked(reason)),
                _ => continue,
            },
            Err(e) => return Err(HandshakeError::Io(e)),
        }
    }
}

/// The client's connection to the daemon, replaceable while the process runs: after a lost
/// lease the reader connects again and swaps the new socket in. While the link is down,
/// sends report `Full` (the caller counts a drop) and nothing is queued.
pub struct Link {
    /// The current connection and its generation. A send that fails on generation N may
    /// only mark the link down while N is still current: an error on a stale socket
    /// (a sender that took the old connection just before the swap) must not take down
    /// the new one.
    cur: std::sync::Mutex<(std::sync::Arc<RtConn>, u64)>,
    up: std::sync::atomic::AtomicBool,
}

impl Link {
    pub fn new(conn: RtConn) -> Link {
        Link {
            cur: std::sync::Mutex::new((std::sync::Arc::new(conn), 0)),
            up: std::sync::atomic::AtomicBool::new(true),
        }
    }

    pub fn get(&self) -> std::sync::Arc<RtConn> {
        self.cur.lock().unwrap().0.clone()
    }

    /// The current connection's generation (tests, stats).
    pub fn generation(&self) -> u64 {
        self.cur.lock().unwrap().1
    }

    /// Swap in a freshly handshaken connection and mark the link up. The old socket is shut
    /// down so the daemon sees it close at once and any straggling sender fails fast.
    pub fn replace(&self, conn: RtConn) {
        let old = {
            let mut g = self.cur.lock().unwrap();
            let old = std::mem::replace(&mut g.0, std::sync::Arc::new(conn));
            g.1 += 1;
            old
        };
        old.shutdown();
        self.set_up(true);
    }

    pub fn is_up(&self) -> bool {
        self.up.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn set_up(&self, up: bool) {
        self.up.store(up, std::sync::atomic::Ordering::SeqCst);
    }

    /// Mark the link down only if `gen` is still the current generation.
    fn down_if_current(&self, gen: u64) {
        let g = self.cur.lock().unwrap();
        if g.1 == gen {
            self.set_up(false);
        }
    }

    /// Send on the current connection. A closed socket marks the link down (so the reader
    /// reconnects), but only if it is still the connection that failed.
    pub fn send(&self, m: &RtMsg) -> SendOutcome {
        if !self.is_up() {
            return SendOutcome::Full;
        }
        let (conn, gen) = {
            let g = self.cur.lock().unwrap();
            (g.0.clone(), g.1)
        };
        self.send_on(&conn, gen, m)
    }

    /// `send` on a connection obtained earlier (`snapshot`), as a slow sender holding the
    /// pre-swap socket would.
    pub fn snapshot(&self) -> (std::sync::Arc<RtConn>, u64) {
        let g = self.cur.lock().unwrap();
        (g.0.clone(), g.1)
    }

    pub fn send_on(&self, conn: &RtConn, gen: u64, m: &RtMsg) -> SendOutcome {
        let r = conn.send(m);
        if r == SendOutcome::Closed {
            self.down_if_current(gen);
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sender that took the old socket just before the swap fails on it afterwards. That
    /// failure belongs to the old connection: the new link stays up (otherwise the reader
    /// would reconnect again and hit BUSY against its own live session).
    #[test]
    fn a_stale_send_error_does_not_take_down_the_new_link() {
        let (a, _a_peer) = RtConn::pair().unwrap();
        let (b, _b_peer) = RtConn::pair().unwrap();
        let link = Link::new(a);
        let (stale, gen) = link.snapshot();
        link.replace(b);
        assert_eq!(link.generation(), gen + 1);
        assert_eq!(
            link.send_on(&stale, gen, &RtMsg::Keepalive),
            SendOutcome::Closed,
            "the replaced socket was shut down"
        );
        assert!(link.is_up(), "stale failure must not mark the new link down");
        assert_eq!(link.send(&RtMsg::Keepalive), SendOutcome::Sent);
        // A failure on the current connection still does.
        let (cur, g) = link.snapshot();
        cur.shutdown();
        assert_eq!(link.send_on(&cur, g, &RtMsg::Keepalive), SendOutcome::Closed);
        assert!(!link.is_up());
    }
}
