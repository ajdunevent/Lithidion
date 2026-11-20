//! The watch's stored log (endpoint 2002) and core dump (endpoint 9000), for bug reports
//! (PROTOCOL.md section 23).

use crate::wire::{Reader, Result};

const LOG_REQUEST: u8 = 0x10;
const LOG_LINE: u8 = 0x80;
const LOG_DONE: u8 = 0x81;
const LOG_NONE: u8 = 0x82;

/// Asks for one generation of the stored log. The cookie comes back in each reply as it was
/// sent: the watch copies it without converting it.
pub fn log_request(generation: u8, cookie: u32) -> Vec<u8> {
    let mut out = vec![LOG_REQUEST, generation];
    out.extend_from_slice(&cookie.to_le_bytes());
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// UTC seconds.
    pub time: u32,
    pub level: u8,
    pub line: u16,
    pub file: String,
    pub message: String,
}

impl LogLine {
    /// The level as one letter, as the official app writes it.
    pub fn level_letter(&self) -> char {
        match self.level {
            0 => '*',
            1 => 'E',
            50 => 'W',
            100 => 'I',
            200 => 'D',
            255 => 'V',
            _ => '?',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogReply {
    Line(LogLine),
    /// The generation is complete.
    Done,
    /// There is no such generation; sent only to phones advertising infinite log dumping.
    NoLogs,
}

/// A reply and its cookie; `None` for anything else on the endpoint.
pub fn parse_log(payload: &[u8]) -> Result<Option<(u32, LogReply)>> {
    let mut r = Reader::new(payload);
    let command = r.u8()?;
    let cookie = r.u32_le()?;
    let reply = match command {
        LOG_LINE => {
            let time = r.u32()?;
            let level = r.u8()?;
            let length = r.u8()? as usize;
            let line = r.u16()?;
            let file = text(r.bytes(16)?);
            let message = text(r.bytes(length.min(r.remaining()))?);
            LogReply::Line(LogLine { time, level, line, file, message })
        }
        LOG_DONE => LogReply::Done,
        LOG_NONE => LogReply::NoLogs,
        _ => return Ok(None),
    };
    Ok(Some((cookie, reply)))
}

/// Text up to its first NUL.
fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim_end().to_string()
}

const GET_CORE_DUMP: u8 = 0x00;
const INFO: u8 = 0x01;
const DATA: u8 = 0x02;

/// Asks for the latest core dump, read or not.
pub fn core_dump_request(transaction: u8) -> Vec<u8> {
    vec![GET_CORE_DUMP, transaction]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytesError {
    Malformed,
    InProgress,
    /// There is no core dump.
    Missing,
    Corrupt,
    Other(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BytesReply {
    /// The size to expect, or why nothing follows.
    Info {
        transaction: u8,
        size: std::result::Result<u32, BytesError>,
    },
    Data {
        transaction: u8,
        offset: u32,
        data: Vec<u8>,
    },
}

/// Big-endian; `None` for anything else on the endpoint.
pub fn parse_bytes(payload: &[u8]) -> Result<Option<BytesReply>> {
    let mut r = Reader::new(payload);
    let command = r.u8()?;
    let transaction = r.u8()?;
    Ok(Some(match command {
        INFO => {
            let error = r.u8()?;
            let size = r.u32()?;
            let size = match error {
                0 => Ok(size),
                1 => Err(BytesError::Malformed),
                2 => Err(BytesError::InProgress),
                3 => Err(BytesError::Missing),
                4 => Err(BytesError::Corrupt),
                e => Err(BytesError::Other(e)),
            };
            BytesReply::Info { transaction, size }
        }
        DATA => {
            let offset = r.u32()?;
            BytesReply::Data { transaction, offset, data: r.bytes(r.remaining())?.to_vec() }
        }
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_line() {
        assert_eq!(log_request(3, 0x0403_0201), [0x10, 3, 1, 2, 3, 4]);
        let mut p = vec![0x80, 1, 2, 3, 4, 0x69, 0x1b, 0x9b, 0x40, 100, 5, 0x01, 0x2c];
        p.extend_from_slice(b"comm_session.c\0\0");
        p.extend_from_slice(b"hello");
        let (cookie, reply) = parse_log(&p).unwrap().unwrap();
        assert_eq!(cookie, 0x0403_0201);
        let LogReply::Line(l) = reply else { panic!("a line") };
        assert_eq!((l.time, l.level_letter(), l.line), (0x691b_9b40, 'I', 300));
        assert_eq!((l.file.as_str(), l.message.as_str()), ("comm_session.c", "hello"));
        assert_eq!(parse_log(&[0x82, 1, 2, 3, 4]).unwrap(), Some((0x0403_0201, LogReply::NoLogs)));
        assert_eq!(parse_log(&[0x81, 1, 2, 3, 4]).unwrap(), Some((0x0403_0201, LogReply::Done)));
    }

    #[test]
    fn core_dump() {
        assert_eq!(core_dump_request(7), [0, 7]);
        assert_eq!(parse_bytes(&[1, 7, 0, 0, 1, 0, 0]).unwrap(), Some(BytesReply::Info { transaction: 7, size: Ok(65536) }));
        assert_eq!(parse_bytes(&[1, 7, 3, 0, 0, 0, 0]).unwrap(), Some(BytesReply::Info { transaction: 7, size: Err(BytesError::Missing) }));
        assert_eq!(
            parse_bytes(&[2, 7, 0, 0, 0, 16, 9, 8]).unwrap(),
            Some(BytesReply::Data { transaction: 7, offset: 16, data: vec![9, 8] })
        );
    }
}
