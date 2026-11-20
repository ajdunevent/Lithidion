//! Music control (section 15): what is playing, for the watch's Music app, and the watch's
//! buttons. Strings are length-prefixed; integers little-endian.

use crate::wire::{put_pstring, Reader, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    PlayPause,
    Pause,
    Play,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
    /// The watch asks for the current track.
    GetCurrent,
}

pub fn parse(payload: &[u8]) -> Result<Option<Button>> {
    let mut r = Reader::new(payload);
    Ok(match r.u8()? {
        0x01 => Some(Button::PlayPause),
        0x02 => Some(Button::Pause),
        0x03 => Some(Button::Play),
        0x04 => Some(Button::Next),
        0x05 => Some(Button::Previous),
        0x06 => Some(Button::VolumeUp),
        0x07 => Some(Button::VolumeDown),
        0x08 => Some(Button::GetCurrent),
        _ => None,
    })
}

/// The track: artist, album, title, and its length in milliseconds when known.
pub fn track(artist: &str, album: &str, title: &str, length_ms: Option<u32>) -> Vec<u8> {
    let mut out = vec![0x10];
    put_pstring(&mut out, artist);
    put_pstring(&mut out, album);
    put_pstring(&mut out, title);
    if let Some(l) = length_ms {
        out.extend_from_slice(&l.to_le_bytes());
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Paused = 0,
    Playing = 1,
    Unknown = 4,
}

/// Playing or paused, the position in milliseconds, and the rate in percent.
pub fn play_state(state: State, position_ms: u32, rate_percent: u32) -> Vec<u8> {
    let mut out = vec![0x11, state as u8];
    out.extend_from_slice(&position_ms.to_le_bytes());
    out.extend_from_slice(&rate_percent.to_le_bytes());
    // Shuffle and repeat unknown, next and previous change track.
    out.extend_from_slice(&[0, 0, 0]);
    out
}

pub fn volume(percent: u8) -> Vec<u8> {
    vec![0x12, percent.min(100)]
}

/// The player: an id and a name.
pub fn player(id: &str, name: &str) -> Vec<u8> {
    let mut out = vec![0x13];
    put_pstring(&mut out, id);
    put_pstring(&mut out, name);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        assert_eq!(track("A", "", "T", Some(1000)), vec![0x10, 1, b'A', 0, 1, b'T', 0xE8, 3, 0, 0]);
        assert_eq!(play_state(State::Playing, 2, 100), vec![0x11, 1, 2, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0]);
        assert_eq!(volume(150), vec![0x12, 100]);
        assert_eq!(parse(&[6]).unwrap(), Some(Button::VolumeUp));
        assert_eq!(parse(&[0x10]).unwrap(), None);
    }
}
