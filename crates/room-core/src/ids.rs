//! Identities and time (CONTRACTS.md section 8).

use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

macro_rules! id32 {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
        pub struct $name(pub [u8; 32]);
        impl $name {
            pub const ZERO: Self = Self([0; 32]);
            pub fn is_zero(&self) -> bool {
                self.0 == [0; 32]
            }
            pub fn hex(&self) -> String {
                crate::wire::hex(&self.0)
            }
            pub fn short(&self) -> String {
                crate::wire::hex(&self.0[..4])
            }
            pub fn from_hex(s: &str) -> Option<Self> {
                let v = data_encoding::HEXLOWER_PERMISSIVE
                    .decode(s.as_bytes())
                    .ok()?;
                Some(Self(v.try_into().ok()?))
            }
        }
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.short())
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.hex())
            }
        }
    };
}

id32!(Key);
id32!(RoomId);
id32!(RecordId);

impl Key {
    pub fn verifying(&self) -> Option<VerifyingKey> {
        VerifyingKey::from_bytes(&self.0).ok()
    }
    /// Strict Ed25519 verification of `sig` over `msg`.
    pub fn verify(&self, msg: &[u8], sig: &[u8; 64]) -> bool {
        match self.verifying() {
            Some(vk) => vk.verify_strict(msg, &Signature::from_bytes(sig)).is_ok(),
            None => false,
        }
    }
}

/// A signing identity. Wraps an Ed25519 secret; the public half is the member key
/// (and the iroh `EndpointId` in the daemon).
#[derive(Clone)]
pub struct Identity {
    sk: SigningKey,
}

impl Identity {
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self {
            sk: SigningKey::from_bytes(&secret),
        }
    }
    pub fn secret(&self) -> [u8; 32] {
        self.sk.to_bytes()
    }
    pub fn key(&self) -> Key {
        Key(self.sk.verifying_key().to_bytes())
    }
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.sk.sign(msg).to_bytes()
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Identity({:?})", self.key())
    }
}

/// Time is an input to the core. `mono_us` is the local monotonic clock (only ever
/// compared with itself); `unix_ms` is the local wall clock (used for `issued_ms`
/// fields and expiry checks at admission).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Now {
    pub mono_us: u64,
    pub unix_ms: u64,
}

impl Now {
    pub fn plus_ms(self, ms: u64) -> Self {
        Self {
            mono_us: self.mono_us + ms * 1000,
            unix_ms: self.unix_ms + ms,
        }
    }
}

/// Deterministic byte source (BLAKE3 in counter mode). The daemon seeds it from the
/// OS; the simulator from the run seed. Not used for long-term keys in the daemon.
#[derive(Clone)]
pub struct Entropy {
    seed: [u8; 32],
    counter: u64,
}

impl Entropy {
    pub fn new(seed: [u8; 32]) -> Self {
        Self { seed, counter: 0 }
    }
    pub fn fill(&mut self, out: &mut [u8]) {
        let mut h = blake3::Hasher::new_keyed(&self.seed);
        h.update(&self.counter.to_le_bytes());
        self.counter += 1;
        h.finalize_xof().fill(out);
    }
    pub fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut b = [0; N];
        self.fill(&mut b);
        b
    }
    pub fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.bytes())
    }
}
