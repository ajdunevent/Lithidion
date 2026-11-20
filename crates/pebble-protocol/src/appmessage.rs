//! App messages (endpoint 48, PROTOCOL.md section 19): dictionaries passed between a watch app
//! and its phone-side script. Little-endian.
//!
//! A push is `01`, a transaction id, the app's UUID, a tuple count, then the tuples; the other
//! side answers `FF` (ACK) or `7F` (NACK) with the same transaction id. Each tuple is a `u32` key,
//! a type, a `u16` length, then the value.

use crate::wire::{err, Reader, Result};
use uuid::Uuid;

pub const PUSH: u8 = 0x01;
pub const ACK: u8 = 0xFF;
pub const NACK: u8 = 0x7F;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Bytes(Vec<u8>),
    /// Sent with a trailing NUL, which is not kept here.
    Text(String),
    Unsigned(u32),
    Signed(i32),
}

const BYTES: u8 = 0;
const TEXT: u8 = 1;
const UNSIGNED: u8 = 2;
const SIGNED: u8 = 3;

pub type Tuple = (u32, Value);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Push { transaction: u8, uuid: Uuid, tuples: Vec<Tuple> },
    Ack(u8),
    Nack(u8),
}

fn number(b: &[u8], signed: bool) -> Result<Value> {
    Ok(match (b.len(), signed) {
        (1, false) => Value::Unsigned(b[0] as u32),
        (1, true) => Value::Signed(b[0] as i8 as i32),
        (2, false) => Value::Unsigned(u16::from_le_bytes([b[0], b[1]]) as u32),
        (2, true) => Value::Signed(i16::from_le_bytes([b[0], b[1]]) as i32),
        (4, false) => Value::Unsigned(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        (4, true) => Value::Signed(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        (n, _) => return err(format!("a {n}-byte number")),
    })
}

pub fn parse(payload: &[u8]) -> Result<Message> {
    let mut r = Reader::new(payload);
    let command = r.u8()?;
    let transaction = r.u8()?;
    match command {
        ACK => return Ok(Message::Ack(transaction)),
        NACK => return Ok(Message::Nack(transaction)),
        PUSH => {}
        other => return err(format!("app message command {other:#04x}")),
    }
    let uuid = Uuid::from_bytes(r.array()?);
    let count = r.u8()?;
    let mut tuples = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let key = r.u32_le()?;
        let kind = r.u8()?;
        let len = r.u16_le()? as usize;
        let value = r.bytes(len)?;
        tuples.push((
            key,
            match kind {
                BYTES => Value::Bytes(value.to_vec()),
                TEXT => {
                    let end = value.iter().position(|b| *b == 0).unwrap_or(value.len());
                    Value::Text(String::from_utf8_lossy(&value[..end]).into_owned())
                }
                UNSIGNED => number(value, false)?,
                SIGNED => number(value, true)?,
                other => return err(format!("tuple type {other}")),
            },
        ));
    }
    Ok(Message::Push { transaction, uuid, tuples })
}

/// A push for `uuid`. Numbers go as four bytes, which is what watch apps read them as.
pub fn push(transaction: u8, uuid: Uuid, tuples: &[Tuple]) -> Vec<u8> {
    let mut out = vec![PUSH, transaction];
    out.extend_from_slice(uuid.as_bytes());
    out.push(tuples.len() as u8);
    for (key, value) in tuples {
        let (kind, bytes) = match value {
            Value::Bytes(b) => (BYTES, b.clone()),
            Value::Text(t) => (TEXT, t.bytes().chain([0]).collect()),
            Value::Unsigned(n) => (UNSIGNED, n.to_le_bytes().to_vec()),
            Value::Signed(n) => (SIGNED, n.to_le_bytes().to_vec()),
        };
        out.extend_from_slice(&key.to_le_bytes());
        out.push(kind);
        out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    out
}

pub fn ack(transaction: u8) -> Vec<u8> {
    vec![ACK, transaction]
}

pub fn nack(transaction: u8) -> Vec<u8> {
    vec![NACK, transaction]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_round_trip() {
        let uuid = Uuid::parse_str("133215f0-cf20-4c05-997b-3c9be5a64e5b").unwrap();
        let tuples =
            vec![(0, Value::Text("Sunny".into())), (1, Value::Signed(-7)), (2, Value::Unsigned(300)), (10, Value::Bytes(vec![1, 2, 3]))];
        let bytes = push(9, uuid, &tuples);
        assert_eq!(&bytes[..2], &[1, 9]);
        assert_eq!(bytes[18], 4);
        // Key 0, text, six bytes with the NUL.
        assert_eq!(&bytes[19..26], &[0, 0, 0, 0, 1, 6, 0]);
        assert_eq!(parse(&bytes).unwrap(), Message::Push { transaction: 9, uuid, tuples });
    }

    #[test]
    fn short_numbers_and_answers() {
        let uuid = Uuid::nil();
        let mut bytes = vec![1, 3];
        bytes.extend_from_slice(uuid.as_bytes());
        bytes.push(2);
        bytes.extend_from_slice(&[5, 0, 0, 0, 3, 1, 0, 0xFF]);
        bytes.extend_from_slice(&[6, 0, 0, 0, 2, 2, 0, 0x34, 0x12]);
        let Message::Push { tuples, .. } = parse(&bytes).unwrap() else { panic!() };
        assert_eq!(tuples, vec![(5, Value::Signed(-1)), (6, Value::Unsigned(0x1234))]);
        assert_eq!(parse(&ack(4)).unwrap(), Message::Ack(4));
        assert_eq!(parse(&nack(4)).unwrap(), Message::Nack(4));
        assert!(parse(&bytes[..30]).is_err());
    }
}
