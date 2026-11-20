//! PutBytes (endpoint 48879, section 9): moving a firmware image, resource pack, or app binary to
//! the watch. Init, then puts of at most `CHUNK` bytes, then commit with the object's CRC, then
//! install; each step waits for an ack carrying the session cookie.

use crate::wire::{err, Reader, Result};

pub const CHUNK: usize = 2000;
pub const RESUME_MAGIC: u32 = 0xBE43_54EF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Firmware = 0x01,
    Recovery = 0x02,
    SystemResources = 0x03,
    AppResources = 0x04,
    AppExecutable = 0x05,
    File = 0x06,
    Worker = 0x07,
}

/// Init for firmware and resources. `size` is the bytes this session sends.
pub fn init(size: u32, object: ObjectType, bank: u8, filename: &str, resume_offset: Option<u32>) -> Vec<u8> {
    let mut out = vec![0x01];
    out.extend_from_slice(&size.to_be_bytes());
    out.push(object as u8);
    out.push(bank);
    if !filename.is_empty() {
        out.extend_from_slice(filename.as_bytes());
        out.push(0);
    }
    if let Some(offset) = resume_offset {
        out.extend_from_slice(&RESUME_MAGIC.to_be_bytes());
        out.extend_from_slice(&offset.to_be_bytes());
    }
    out
}

/// Init for an app's binary or resources: the type has its top bit set and the app's install
/// id follows.
pub fn app_init(size: u32, object: ObjectType, app_id: u32) -> Vec<u8> {
    let mut out = vec![0x01];
    out.extend_from_slice(&size.to_be_bytes());
    out.push(object as u8 | 0x80);
    out.extend_from_slice(&app_id.to_be_bytes());
    out
}

pub fn put(cookie: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + data.len());
    out.push(0x02);
    out.extend_from_slice(&cookie.to_be_bytes());
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
    out
}

pub fn commit(cookie: u32, crc: u32) -> Vec<u8> {
    let mut out = vec![0x03];
    out.extend_from_slice(&cookie.to_be_bytes());
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

pub fn abort(cookie: u32) -> Vec<u8> {
    let mut out = vec![0x04];
    out.extend_from_slice(&cookie.to_be_bytes());
    out
}

pub fn install(cookie: u32) -> Vec<u8> {
    let mut out = vec![0x05];
    out.extend_from_slice(&cookie.to_be_bytes());
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response {
    pub ack: bool,
    pub cookie: u32,
}

impl Response {
    pub fn parse(payload: &[u8]) -> Result<Response> {
        let mut r = Reader::new(payload);
        let ack = match r.u8()? {
            0x01 => true,
            0x02 => false,
            other => return err(format!("unknown PutBytes result {other:#04x}")),
        };
        Ok(Response { ack, cookie: r.u32()? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        assert_eq!(init(0x0102, ObjectType::Firmware, 0, "", None), vec![1, 0, 0, 1, 2, 1, 0]);
        let resumed = init(10, ObjectType::SystemResources, 0, "", Some(5));
        assert_eq!(&resumed[7..11], &RESUME_MAGIC.to_be_bytes());
        assert_eq!(&resumed[11..], &[0, 0, 0, 5]);
        assert_eq!(init(1, ObjectType::File, 0, "a", None)[7..], [b'a', 0]);
        assert_eq!(app_init(1, ObjectType::AppExecutable, 7)[5], 0x85);
        assert_eq!(put(9, &[7, 7]), vec![2, 0, 0, 0, 9, 0, 0, 0, 2, 7, 7]);
        assert_eq!(commit(9, 0xAABBCCDD), vec![3, 0, 0, 0, 9, 0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(install(9), vec![5, 0, 0, 0, 9]);
        assert_eq!(Response::parse(&[1, 0, 0, 0, 42]).unwrap(), Response { ack: true, cookie: 42 });
        assert!(!Response::parse(&[2, 0, 0, 0, 0]).unwrap().ack);
    }
}
