//! The transport seam. **Frozen at the end of round L0**: later rounds only add
//! implementations (a Windows named pipe), never change these two traits.
//!
//! A transport yields a connection to the daemon's local API. A connection is a blocking
//! byte stream that can be duplicated, so one thread can read while others write, and
//! shut down from any thread to unblock the reader.

use std::io::{Read, Write};

/// One live connection to the daemon.
pub trait Conn: Read + Write + Send {
    /// A second handle to the same stream (the worker reads from one, callers write to
    /// the other).
    fn try_clone_box(&self) -> std::io::Result<Box<dyn Conn>>;
    /// Close both directions; a blocked `read` on any handle returns.
    fn shutdown(&self);
}

/// A way to reach the daemon. Cheap to call repeatedly: the client reconnects with it.
pub trait Transport: Send + Sync + 'static {
    fn connect(&self) -> std::io::Result<Box<dyn Conn>>;
    /// For logs ("unix:/run/user/1000/prim/room.sock").
    fn describe(&self) -> String;
}

#[cfg(unix)]
pub use unix::UnixTransport;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    pub struct UnixTransport {
        pub path: PathBuf,
    }

    impl UnixTransport {
        pub fn new(path: impl Into<PathBuf>) -> Self {
            Self { path: path.into() }
        }
    }

    struct UnixConn(UnixStream);

    impl Read for UnixConn {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(b)
        }
    }
    impl Write for UnixConn {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.write(b)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }
    impl Conn for UnixConn {
        fn try_clone_box(&self) -> std::io::Result<Box<dyn Conn>> {
            Ok(Box::new(UnixConn(self.0.try_clone()?)))
        }
        fn shutdown(&self) {
            let _ = self.0.shutdown(std::net::Shutdown::Both);
        }
    }

    impl Transport for UnixTransport {
        fn connect(&self) -> std::io::Result<Box<dyn Conn>> {
            Ok(Box::new(UnixConn(UnixStream::connect(&self.path)?)))
        }
        fn describe(&self) -> String {
            format!("unix:{}", self.path.display())
        }
    }
}
