//! Strict fixed-width little-endian encoding (CONTRACTS.md section 0).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Short,
    Trailing,
    Magic,
    Version(u8),
    Kind(u8),
    Field(&'static str),
    TooLarge(&'static str),
    Signature,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for DecodeError {}

#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn new() -> Self {
        Self(Vec::new())
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn raw(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
    /// `bytes16`; panics if `v` exceeds u16 (callers enforce caps first).
    pub fn bytes16(&mut self, v: &[u8]) -> &mut Self {
        self.u16(u16::try_from(v.len()).expect("bytes16 overflow"));
        self.raw(v)
    }
    pub fn bytes8(&mut self, v: &[u8]) -> &mut Self {
        self.u8(u8::try_from(v.len()).expect("bytes8 overflow"));
        self.raw(v)
    }
    pub fn bytes32(&mut self, v: &[u8]) -> &mut Self {
        self.u32(u32::try_from(v.len()).expect("bytes32 overflow"));
        self.raw(v)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn finish(self) -> Vec<u8> {
        self.0
    }
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub fn pos(&self) -> usize {
        self.pos
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < n {
            return Err(DecodeError::Short);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    pub fn bytes16(&mut self, cap: usize, what: &'static str) -> Result<&'a [u8], DecodeError> {
        let n = self.u16()? as usize;
        if n > cap {
            return Err(DecodeError::TooLarge(what));
        }
        self.take(n)
    }
    pub fn bytes8(&mut self, cap: usize, what: &'static str) -> Result<&'a [u8], DecodeError> {
        let n = self.u8()? as usize;
        if n > cap {
            return Err(DecodeError::TooLarge(what));
        }
        self.take(n)
    }
    pub fn bytes32(&mut self, cap: usize, what: &'static str) -> Result<&'a [u8], DecodeError> {
        let n = self.u32()? as usize;
        if n > cap {
            return Err(DecodeError::TooLarge(what));
        }
        self.take(n)
    }
    pub fn end(&self) -> Result<(), DecodeError> {
        if self.remaining() != 0 {
            return Err(DecodeError::Trailing);
        }
        Ok(())
    }
}

/// BLAKE3 `derive_key(ctx, bytes)`.
pub fn h(ctx: &str, bytes: &[u8]) -> [u8; 32] {
    blake3::derive_key(ctx, bytes)
}

pub fn hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}
