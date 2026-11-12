//! The daemon's state directory: `identity.key`, `incarnation`, `room.bin`.
//!
//! Default location `$XDG_STATE_HOME/prim/rooms/<room-hex>/` (or `~/.local/state/...`).
//! These are small and worth keeping, so no `CACHEDIR.TAG` here.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use room_core::ids::{Identity, RoomId};

/// Write `bytes` to `path` atomically (temp file in the same directory, fsync, rename).
pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Root of all rooms' state dirs.
pub fn rooms_root() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("prim").join("rooms")
}

pub fn room_dir(room: &RoomId) -> PathBuf {
    rooms_root().join(room.hex())
}

/// A fresh staging dir under [`rooms_root`] for a room whose id isn't known yet
/// (found: the id depends on the identity created here).
pub fn staging_dir() -> std::io::Result<PathBuf> {
    let mut r = [0u8; 6];
    getrandom::fill(&mut r).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(rooms_root().join(format!(".pending-{}", data_encoding::HEXLOWER.encode(&r))))
}

#[derive(Debug)]
pub struct StateDir {
    pub path: PathBuf,
}

impl StateDir {
    /// Open (creating, mode 0700) a state dir.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        fs::create_dir_all(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(Self { path })
    }

    pub fn identity_path(&self) -> PathBuf {
        self.path.join("identity.key")
    }
    pub fn incarnation_path(&self) -> PathBuf {
        self.path.join("incarnation")
    }
    pub fn room_path(&self) -> PathBuf {
        self.path.join("room.bin")
    }

    /// The member identity, created on first use. `identity.key` is 0600 and holds the
    /// 32-byte Ed25519 secret (it is also the iroh secret key).
    pub fn load_or_create_identity(&self) -> std::io::Result<Identity> {
        let p = self.identity_path();
        match fs::read(&p) {
            Ok(b) => {
                let secret: [u8; 32] = b.as_slice().try_into().map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "identity.key is not 32 bytes",
                    )
                })?;
                Ok(Identity::from_secret(secret))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut secret = [0u8; 32];
                getrandom::fill(&mut secret).map_err(|e| std::io::Error::other(e.to_string()))?;
                atomic_write(&p, &secret, 0o600)?;
                Ok(Identity::from_secret(secret))
            }
            Err(e) => Err(e),
        }
    }

    /// Increment the persisted counter and return the new value (first start: 1).
    /// Must be called before the `Room` is built.
    pub fn bump_incarnation(&self) -> std::io::Result<u32> {
        let p = self.incarnation_path();
        let prev = match fs::read(&p) {
            Ok(b) if b.len() == 4 => u32::from_le_bytes(b.try_into().unwrap()),
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "incarnation is not 4 bytes",
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };
        let next = prev
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("incarnation overflow"))?;
        atomic_write(&p, &next.to_le_bytes(), 0o600)?;
        Ok(next)
    }

    pub fn read_room(&self) -> std::io::Result<Option<Vec<u8>>> {
        match fs::read(self.room_path()) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Move a staging dir to its final name. Fails if `to` exists.
    pub fn relocate(&mut self, to: PathBuf) -> std::io::Result<()> {
        if to.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} already exists", to.display()),
            ));
        }
        fs::rename(&self.path, &to)?;
        self.path = to;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_incarnation_and_modes() {
        let d = tempfile::tempdir().unwrap();
        let s = StateDir::open(d.path().join("r")).unwrap();
        let a = s.load_or_create_identity().unwrap();
        let b = s.load_or_create_identity().unwrap();
        assert_eq!(a.key(), b.key());
        let mode = fs::metadata(s.identity_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(s.bump_incarnation().unwrap(), 1);
        assert_eq!(s.bump_incarnation().unwrap(), 2);
        assert_eq!(s.bump_incarnation().unwrap(), 3);
        assert!(s.read_room().unwrap().is_none());
        atomic_write(&s.room_path(), b"x", 0o600).unwrap();
        assert_eq!(s.read_room().unwrap().unwrap(), b"x");
    }

    #[test]
    fn relocate_refuses_existing() {
        let d = tempfile::tempdir().unwrap();
        let mut s = StateDir::open(d.path().join("a")).unwrap();
        fs::create_dir(d.path().join("b")).unwrap();
        assert!(s.relocate(d.path().join("b")).is_err());
        s.relocate(d.path().join("c")).unwrap();
        assert!(d.path().join("c").is_dir());
    }
}
