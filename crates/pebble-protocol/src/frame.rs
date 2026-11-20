//! Pebble Protocol frames (section 5): length `u16`, endpoint `u16`, payload.

pub const HEADER_LEN: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub endpoint: u16,
    pub payload: Vec<u8>,
}

pub fn encode(endpoint: u16, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= u16::MAX as usize, "Pebble Protocol payloads are at most 65535 bytes");
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(&endpoint.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Cuts the PPoGATT byte stream into frames; data packets split frames anywhere.
#[derive(Debug, Default)]
pub struct Reassembler {
    buffer: Vec<u8>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn next_frame(&mut self) -> Option<Frame> {
        if self.buffer.len() < HEADER_LEN {
            return None;
        }
        let len = u16::from_be_bytes([self.buffer[0], self.buffer[1]]) as usize;
        if self.buffer.len() < HEADER_LEN + len {
            return None;
        }
        let endpoint = u16::from_be_bytes([self.buffer[2], self.buffer[3]]);
        let payload = self.buffer[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.buffer.drain(..HEADER_LEN + len);
        Some(Frame { endpoint, payload })
    }

    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_across_pushes() {
        let mut stream = encode(17, &[0]);
        stream.extend(encode(16, &[1; 40]));
        let mut r = Reassembler::new();
        for chunk in stream.chunks(3) {
            r.push(chunk);
        }
        assert_eq!(r.next_frame(), Some(Frame { endpoint: 17, payload: vec![0] }));
        assert_eq!(r.next_frame(), Some(Frame { endpoint: 16, payload: vec![1; 40] }));
        assert_eq!(r.next_frame(), None);
        assert_eq!(r.buffered(), 0);
    }

    #[test]
    fn incomplete_frame_waits() {
        let stream = encode(2001, &[0, 0, 0, 0, 7]);
        let mut r = Reassembler::new();
        r.push(&stream[..6]);
        assert_eq!(r.next_frame(), None);
        r.push(&stream[6..]);
        assert_eq!(r.next_frame().unwrap().payload, vec![0, 0, 0, 0, 7]);
    }

    #[test]
    fn empty_payload() {
        let mut r = Reassembler::new();
        r.push(&encode(11, &[]));
        assert_eq!(r.next_frame(), Some(Frame { endpoint: 11, payload: vec![] }));
    }
}
