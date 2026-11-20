//! Field encodings (section 5): big-endian integers, fixed strings, and pstrings.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

pub type Result<T> = std::result::Result<T, DecodeError>;

pub(crate) fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(DecodeError(message.into()))
}

pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return err(format!("needed {n} bytes at offset {}, {} left", self.pos, self.remaining()));
        }
        let out = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("slice has N bytes"))
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub fn u16_le(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub fn u32_le(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// A string padded to `n` bytes; `00` or `FF` ends it.
    pub fn fixed_string(&mut self, n: usize) -> Result<String> {
        let raw = self.bytes(n)?;
        let end = raw.iter().position(|&b| b == 0 || b == 0xFF).unwrap_or(n);
        Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
    }

    pub fn pstring(&mut self) -> Result<String> {
        let len = self.u8()? as usize;
        Ok(String::from_utf8_lossy(self.bytes(len)?).into_owned())
    }

    pub fn uuid(&mut self) -> Result<uuid::Uuid> {
        Ok(uuid::Uuid::from_bytes(self.array()?))
    }
}

/// Appends a pstring, cut to 255 bytes on a character boundary.
pub fn put_pstring(out: &mut Vec<u8>, s: &str) {
    let mut end = s.len().min(255);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    out.push(end as u8);
    out.extend_from_slice(&s.as_bytes()[..end]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_string_stops_at_padding_or_erased_flash() {
        let mut r = Reader::new(b"v4.35.0\0\0\0ab\xff\xff");
        assert_eq!(r.fixed_string(10).unwrap(), "v4.35.0");
        assert_eq!(r.fixed_string(4).unwrap(), "ab");
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn short_input_is_an_error() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert!(r.u32().is_err());
    }

    #[test]
    fn pstring_round_trip_and_truncation() {
        let mut out = Vec::new();
        put_pstring(&mut out, "America/Chicago");
        assert_eq!(out[0], 15);
        assert_eq!(Reader::new(&out).pstring().unwrap(), "America/Chicago");

        let long = "é".repeat(200);
        let mut out = Vec::new();
        put_pstring(&mut out, &long);
        assert_eq!(out[0], 254);
    }
}
