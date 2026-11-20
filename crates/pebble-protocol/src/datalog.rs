//! Data logging (section 17): the watch opens sessions and sends batches of fixed-size items,
//! which the phone acknowledges. Little-endian. PebbleOS's health data arrives this way.

use crate::wire::{err, Reader, Result};
use uuid::Uuid;

/// Tags the system (nil UUID) uses for health data.
pub mod tag {
    pub const STEPS: u32 = 81;
    pub const SLEEP: u32 = 83;
    pub const OVERLAY: u32 = 84;
    pub const HEART_RATE: u32 = 85;
}

/// What the watch sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Open {
        session: u8,
        app: Uuid,
        timestamp: u32,
        tag: u32,
        item_type: u8,
        item_size: u16,
    },
    Data {
        session: u8,
        items_left: u32,
        crc: u32,
        payload: Vec<u8>,
    },
    Close {
        session: u8,
    },
    /// The watch did not see an ACK in time; it keeps the batch and sends it again.
    Timeout {
        session: u8,
    },
    SendEnabled(bool),
}

pub fn parse(payload: &[u8]) -> Result<Incoming> {
    let mut r = Reader::new(payload);
    Ok(match r.u8()? {
        0x01 => Incoming::Open {
            session: r.u8()?,
            app: r.uuid()?,
            timestamp: r.u32_le()?,
            tag: r.u32_le()?,
            item_type: r.u8()?,
            item_size: r.u16_le()?,
        },
        0x02 => {
            let session = r.u8()?;
            let items_left = r.u32_le()?;
            let crc = r.u32_le()?;
            let payload = r.bytes(r.remaining())?.to_vec();
            Incoming::Data { session, items_left, crc, payload }
        }
        0x03 => Incoming::Close { session: r.u8()? },
        0x07 => Incoming::Timeout { session: r.u8()? },
        0x0A => Incoming::SendEnabled(r.u8()? != 0),
        c => return err(format!("unknown data logging command {c:#04x}")),
    })
}

/// The sessions the phone already knows; an empty list makes the watch open its sessions again.
pub fn report_open_sessions(sessions: &[u8]) -> Vec<u8> {
    let mut out = vec![0x84];
    out.extend_from_slice(sessions);
    out
}

pub fn ack(session: u8) -> Vec<u8> {
    vec![0x85, session]
}

pub fn nack(session: u8) -> Vec<u8> {
    vec![0x86, session]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_and_data() {
        let mut open = vec![0x01, 7];
        open.extend_from_slice(&[0; 16]);
        open.extend_from_slice(&100u32.to_le_bytes());
        open.extend_from_slice(&81u32.to_le_bytes());
        open.push(0);
        open.extend_from_slice(&36u16.to_le_bytes());
        assert_eq!(
            parse(&open).unwrap(),
            Incoming::Open { session: 7, app: Uuid::nil(), timestamp: 100, tag: tag::STEPS, item_type: 0, item_size: 36 }
        );
        let mut data = vec![0x02, 7];
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&9u32.to_le_bytes());
        data.extend_from_slice(&[1, 2]);
        assert_eq!(parse(&data).unwrap(), Incoming::Data { session: 7, items_left: 3, crc: 9, payload: vec![1, 2] });
        assert_eq!(parse(&[0x03, 7]).unwrap(), Incoming::Close { session: 7 });
        assert!(parse(&[0x55]).is_err());
        assert_eq!(ack(7), [0x85, 7]);
        assert_eq!(report_open_sessions(&[]), [0x84]);
    }
}
