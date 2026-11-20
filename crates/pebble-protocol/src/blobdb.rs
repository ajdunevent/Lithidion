//! BlobDB v1 (section 10): the phone's writes into the watch's key-value databases. Unlike the
//! rest of the protocol, its integers are little-endian.

use crate::wire::{err, Reader, Result};

/// Database ids, as on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Database {
    Pins = 0x01,
    Apps = 0x02,
    Reminders = 0x03,
    Notifications = 0x04,
    Weather = 0x05,
    CannedResponses = 0x06,
    HealthParams = 0x07,
    Contacts = 0x08,
    AppConfigs = 0x09,
    HealthStats = 0x0A,
    AppGlance = 0x0B,
    Settings = 0x0C,
}

impl Database {
    /// What a watch last filled by another phone has cleared, as the official app does: every
    /// database but Settings, which the watch owns.
    pub const CLEARED_ON_TAKEOVER: [Database; 11] = [
        Database::Pins,
        Database::Apps,
        Database::Reminders,
        Database::Notifications,
        Database::Weather,
        Database::CannedResponses,
        Database::HealthParams,
        Database::Contacts,
        Database::AppConfigs,
        Database::HealthStats,
        Database::AppGlance,
    ];
}

pub fn insert(token: u16, db: Database, key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = vec![0x01];
    out.extend_from_slice(&token.to_le_bytes());
    out.push(db as u8);
    out.push(key.len() as u8);
    out.extend_from_slice(key);
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value);
    out
}

/// Insert carrying the time of the change, for databases that resolve conflicts (settings).
pub fn insert_with_timestamp(token: u16, db: Database, timestamp: u32, key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = vec![0x0D];
    out.extend_from_slice(&token.to_le_bytes());
    out.push(db as u8);
    out.extend_from_slice(&timestamp.to_le_bytes());
    out.push(key.len() as u8);
    out.extend_from_slice(key);
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value);
    out
}

pub fn delete(token: u16, db: Database, key: &[u8]) -> Vec<u8> {
    let mut out = vec![0x04];
    out.extend_from_slice(&token.to_le_bytes());
    out.push(db as u8);
    out.push(key.len() as u8);
    out.extend_from_slice(key);
    out
}

pub fn clear(token: u16, db: Database) -> Vec<u8> {
    let mut out = vec![0x05];
    out.extend_from_slice(&token.to_le_bytes());
    out.push(db as u8);
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Success,
    GeneralFailure,
    InvalidOperation,
    InvalidDatabase,
    InvalidData,
    KeyDoesNotExist,
    DatabaseFull,
    DataStale,
    NotSupported,
    Locked,
    TryLater,
    Unknown(u8),
}

impl Status {
    fn to_byte(self) -> u8 {
        match self {
            Status::Success => 0x01,
            Status::GeneralFailure => 0x02,
            Status::InvalidOperation => 0x03,
            Status::InvalidDatabase => 0x04,
            Status::InvalidData => 0x05,
            Status::KeyDoesNotExist => 0x06,
            Status::DatabaseFull => 0x07,
            Status::DataStale => 0x08,
            Status::NotSupported => 0x09,
            Status::Locked => 0x0A,
            Status::TryLater => 0x0B,
            Status::Unknown(b) => b,
        }
    }

    fn from_byte(b: u8) -> Status {
        match b {
            0x01 => Status::Success,
            0x02 => Status::GeneralFailure,
            0x03 => Status::InvalidOperation,
            0x04 => Status::InvalidDatabase,
            0x05 => Status::InvalidData,
            0x06 => Status::KeyDoesNotExist,
            0x07 => Status::DatabaseFull,
            0x08 => Status::DataStale,
            0x09 => Status::NotSupported,
            0x0A => Status::Locked,
            0x0B => Status::TryLater,
            other => Status::Unknown(other),
        }
    }
}

/// The watch's answer: the token it was sent and a status.
pub fn parse_response(payload: &[u8]) -> Result<(u16, Status)> {
    let mut r = Reader::new(payload);
    let token = u16::from_le_bytes(r.array()?);
    let status = Status::from_byte(r.u8()?);
    if r.remaining() != 0 {
        return err("BlobDB response is longer than three bytes");
    }
    Ok((token, status))
}

/// BlobDB v2 (section 20): two-way sync, so far only for the watch's settings. Little-endian
/// like v1; responses set the top bit of the command.
pub mod v2 {
    use super::{Database, Status};
    use crate::wire::{err, Reader, Result};

    const RESPONSE: u8 = 0x80;
    const WRITE: u8 = 0x08;
    const WRITEBACK: u8 = 0x09;
    const SYNC_DONE: u8 = 0x0A;
    const VERSION: u8 = 0x0B;
    const DIRTY_ALL: u8 = 0x0C;

    pub fn version(token: u16) -> Vec<u8> {
        let mut out = vec![VERSION];
        out.extend_from_slice(&token.to_le_bytes());
        out
    }

    /// Asks the watch to send every record of `db` again; only Settings supports it.
    pub fn dirty_all(token: u16, db: Database) -> Vec<u8> {
        let mut out = vec![DIRTY_ALL];
        out.extend_from_slice(&token.to_le_bytes());
        out.push(db as u8);
        out
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Incoming {
        Version {
            token: u16,
            status: Status,
            version: u8,
        },
        DirtyAll {
            token: u16,
            status: Status,
        },
        /// A record the watch changed (`writeback` while answering a sync); `timestamp` is when.
        Write {
            token: u16,
            db: u8,
            timestamp: u32,
            key: Vec<u8>,
            value: Vec<u8>,
            writeback: bool,
        },
        SyncDone {
            token: u16,
            db: u8,
        },
        /// Anything else, by command.
        Other(u8),
    }

    pub fn parse(payload: &[u8]) -> Result<Incoming> {
        let mut r = Reader::new(payload);
        let cmd = r.u8()?;
        Ok(match cmd {
            c if c == VERSION | RESPONSE => {
                let token = r.u16_le()?;
                let status = Status::from_byte(r.u8()?);
                let version = if r.remaining() > 0 { r.u8()? } else { 0 };
                Incoming::Version { token, status, version }
            }
            c if c == DIRTY_ALL | RESPONSE => Incoming::DirtyAll { token: r.u16_le()?, status: Status::from_byte(r.u8()?) },
            WRITE | WRITEBACK => {
                let token = r.u16_le()?;
                let db = r.u8()?;
                let timestamp = r.u32_le()?;
                let key_len = r.u8()? as usize;
                let key = r.bytes(key_len)?.to_vec();
                let value_len = r.u16_le()? as usize;
                let value = r.bytes(value_len)?.to_vec();
                Incoming::Write { token, db, timestamp, key, value, writeback: cmd == WRITEBACK }
            }
            SYNC_DONE => Incoming::SyncDone { token: r.u16_le()?, db: r.u8()? },
            other if payload.len() < 3 => return err(format!("BlobDB v2 command {other:#04x} is too short")),
            other => Incoming::Other(other),
        })
    }

    /// The answer to a watch's write, writeback, or sync done.
    pub fn answer(incoming: &Incoming, status: Status) -> Option<Vec<u8>> {
        let (cmd, token) = match incoming {
            Incoming::Write { token, writeback: false, .. } => (WRITE, *token),
            Incoming::Write { token, writeback: true, .. } => (WRITEBACK, *token),
            Incoming::SyncDone { token, .. } => (SYNC_DONE, *token),
            _ => return None,
        };
        let mut out = vec![cmd | RESPONSE];
        out.extend_from_slice(&token.to_le_bytes());
        out.push(status.to_byte());
        Some(out)
    }

    /// Whether `payload` answers the request with `token`.
    pub fn answers(payload: &[u8], token: u16) -> bool {
        payload.len() >= 3 && payload[0] & RESPONSE != 0 && u16::from_le_bytes([payload[1], payload[2]]) == token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_layouts() {
        use v2::*;
        assert_eq!(version(0x0102), vec![0x0B, 2, 1]);
        assert_eq!(dirty_all(3, Database::Settings), vec![0x0C, 3, 0, 12]);
        assert_eq!(parse(&[0x8B, 3, 0, 1, 1]).unwrap(), Incoming::Version { token: 3, status: Status::Success, version: 1 });
        assert_eq!(parse(&[0x8C, 3, 0, 1]).unwrap(), Incoming::DirtyAll { token: 3, status: Status::Success });
        let w = [0x09, 5, 0, 12, 0x10, 0, 0, 0, 3, b'a', b'b', 0, 1, 0, 7];
        let write = parse(&w).unwrap();
        assert_eq!(write, Incoming::Write { token: 5, db: 12, timestamp: 16, key: b"ab\0".to_vec(), value: vec![7], writeback: true });
        assert_eq!(answer(&write, Status::DataStale), Some(vec![0x89, 5, 0, 8]));
        let done = parse(&[0x0A, 6, 0, 12]).unwrap();
        assert_eq!(answer(&done, Status::Success), Some(vec![0x8A, 6, 0, 1]));
        assert!(answers(&[0x8B, 3, 0, 1, 1], 3));
        assert!(!answers(&[0x0B, 3, 0], 3));
    }

    #[test]
    fn layouts() {
        assert_eq!(insert(0x0102, Database::Apps, &[9, 9], &[7]), vec![1, 2, 1, 2, 2, 9, 9, 1, 0, 7]);
        assert_eq!(delete(0x0102, Database::Apps, &[9]), vec![4, 2, 1, 2, 1, 9]);
        assert_eq!(clear(5, Database::Pins), vec![5, 5, 0, 1]);
        assert_eq!(insert_with_timestamp(1, Database::Settings, 2, b"k", &[1]), vec![0x0D, 1, 0, 12, 2, 0, 0, 0, 1, b'k', 1, 0, 1]);
        assert_eq!(parse_response(&[2, 1, 1]).unwrap(), (0x0102, Status::Success));
        assert_eq!(parse_response(&[0, 0, 7]).unwrap().1, Status::DatabaseFull);
        assert!(parse_response(&[0, 0]).is_err());
    }
}
