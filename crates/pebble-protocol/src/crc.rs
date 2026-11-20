//! The CRC the watch checks transfers with (section 9): the STM32 hardware CRC, over 32-bit
//! little-endian words, polynomial 0x04C11DB7, start 0xFFFFFFFF, no final inversion. A trailing
//! partial word is zero-padded at the top: bytes `a b c` count as the word `0x00abc` read LE from
//! `c b a 0`.

#[derive(Debug, Clone)]
pub struct Stm32Crc {
    value: u32,
    pending: Vec<u8>,
}

impl Default for Stm32Crc {
    fn default() -> Self {
        Stm32Crc { value: 0xFFFF_FFFF, pending: Vec::new() }
    }
}

impl Stm32Crc {
    pub fn new() -> Self {
        Self::default()
    }

    fn word(&mut self, w: u32) {
        let mut v = self.value ^ w;
        for _ in 0..32 {
            v = if v & 0x8000_0000 != 0 { (v << 1) ^ 0x04C1_1DB7 } else { v << 1 };
        }
        self.value = v;
    }

    pub fn update(&mut self, mut bytes: &[u8]) {
        if !self.pending.is_empty() {
            let need = 4 - self.pending.len();
            let take = need.min(bytes.len());
            self.pending.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.pending.len() < 4 {
                return;
            }
            let w = u32::from_le_bytes([self.pending[0], self.pending[1], self.pending[2], self.pending[3]]);
            self.pending.clear();
            self.word(w);
        }
        let mut chunks = bytes.chunks_exact(4);
        for c in &mut chunks {
            self.word(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
        }
        self.pending.extend_from_slice(chunks.remainder());
    }

    pub fn finish(mut self) -> u32 {
        if !self.pending.is_empty() {
            let mut padded = vec![0u8; 4 - self.pending.len()];
            padded.extend_from_slice(&self.pending);
            padded.reverse();
            let w = u32::from_le_bytes([padded[0], padded[1], padded[2], padded[3]]);
            self.word(w);
        }
        self.value
    }
}

pub fn stm32_crc(bytes: &[u8]) -> u32 {
    let mut c = Stm32Crc::new();
    c.update(bytes);
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_words_match_the_stm32_reference() {
        // The STM32 reference value for one word 0x12345678 (application note AN4187).
        assert_eq!(stm32_crc(&0x1234_5678u32.to_le_bytes()), 0xDF8A_8A2B);
        assert_eq!(stm32_crc(&[]), 0xFFFF_FFFF);
    }

    #[test]
    fn split_updates_match_one_update() {
        let data: Vec<u8> = (0..=250u8).collect();
        let mut c = Stm32Crc::new();
        for chunk in data.chunks(7) {
            c.update(chunk);
        }
        assert_eq!(c.finish(), stm32_crc(&data));
    }

    #[test]
    fn trailing_bytes_are_padded_at_the_top() {
        let mut c = Stm32Crc::new();
        c.word(0x0001_0203);
        assert_eq!(stm32_crc(&[1, 2, 3]), c.finish());
    }
}
