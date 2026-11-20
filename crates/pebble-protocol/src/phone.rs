//! Phone calls on the watch (section 13): the phone reports a call's progress, the watch answers
//! or hangs up. The cookie names the call; strings are length-prefixed.

use crate::wire::{put_pstring, Reader, Result};

fn message(command: u8, cookie: u32) -> Vec<u8> {
    let mut out = vec![command];
    out.extend_from_slice(&cookie.to_be_bytes());
    out
}

fn short(s: &str) -> &str {
    let mut end = s.len().min(31);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// An incoming call: the watch shows the caller with Answer and Decline.
pub fn incoming(cookie: u32, number: &str, name: &str) -> Vec<u8> {
    let mut out = message(0x04, cookie);
    put_pstring(&mut out, short(number));
    put_pstring(&mut out, short(name));
    out
}

pub fn missed(cookie: u32, number: &str, name: &str) -> Vec<u8> {
    let mut out = message(0x06, cookie);
    put_pstring(&mut out, short(number));
    put_pstring(&mut out, short(name));
    out
}

/// The call was answered (on either side).
pub fn start(cookie: u32) -> Vec<u8> {
    message(0x08, cookie)
}

/// The call is over.
pub fn end(cookie: u32) -> Vec<u8> {
    message(0x09, cookie)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Answer(u32),
    Hangup(u32),
}

/// What the watch asks for; `None` for anything else on the endpoint.
pub fn parse(payload: &[u8]) -> Result<Option<Request>> {
    let mut r = Reader::new(payload);
    let command = r.u8()?;
    let cookie = r.u32()?;
    Ok(match command {
        0x01 => Some(Request::Answer(cookie)),
        0x02 => Some(Request::Hangup(cookie)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        assert_eq!(incoming(7, "555", "Al"), vec![4, 0, 0, 0, 7, 3, b'5', b'5', b'5', 2, b'A', b'l']);
        assert_eq!(end(1), vec![9, 0, 0, 0, 1]);
        assert_eq!(parse(&[1, 0, 0, 0, 7]).unwrap(), Some(Request::Answer(7)));
        assert_eq!(parse(&[2, 0, 0, 0, 7]).unwrap(), Some(Request::Hangup(7)));
        assert_eq!(parse(&[3, 0, 0, 0, 7]).unwrap(), None);
        assert_eq!(incoming(1, &"9".repeat(40), "").len(), 5 + 32 + 1);
    }
}
