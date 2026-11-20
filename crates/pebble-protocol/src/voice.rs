//! Dictation (PROTOCOL.md section 21): the voice endpoint, where the watch asks for a session and
//! the phone answers with the transcription, and the audio endpoint, which carries the recording
//! as Speex frames. Little-endian throughout.

use crate::wire::{err, Reader, Result};
use uuid::Uuid;

/// What the watch wants the audio for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    /// Speech to text, for the system or an app.
    Dictation,
    /// Speech to a command (Pebble's old voice commands).
    Command,
    /// Speech to a reminder (natural-language processing).
    Nlp,
    /// Live audio for an app's companion, without transcription.
    AudioStream,
    Other(u8),
}

impl SessionType {
    fn from_byte(b: u8) -> Self {
        match b {
            0x01 => Self::Dictation,
            0x02 => Self::Command,
            0x03 => Self::Nlp,
            0x04 => Self::AudioStream,
            other => Self::Other(other),
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            Self::Dictation => 0x01,
            Self::Command => 0x02,
            Self::Nlp => 0x03,
            Self::AudioStream => 0x04,
            Self::Other(b) => b,
        }
    }
}

/// The phone's answer to a setup or a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Outcome {
    Success = 0x00,
    /// The watch shows a connection error.
    ServiceUnavailable = 0x01,
    Timeout = 0x02,
    RecognizerError = 0x03,
    /// The watch says the speech was not recognised.
    InvalidRecognizerResponse = 0x04,
    /// The watch says dictation is turned off.
    Disabled = 0x05,
    InvalidMessage = 0x06,
}

/// How the watch encodes the recording (`AudioTransferInfoSpeex`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Speex {
    pub version: String,
    pub sample_rate: u32,
    pub bit_rate: u16,
    pub bitstream_version: u8,
    /// Samples per frame.
    pub frame_size: u16,
}

/// The watch asks for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    /// Started by an app rather than the system; such sessions carry the app's UUID.
    pub app_initiated: bool,
    pub kind: SessionType,
    /// The audio endpoint session the recording will arrive on.
    pub session: u16,
    pub speex: Option<Speex>,
    pub app: Option<Uuid>,
}

const MSG_SESSION_SETUP: u8 = 0x01;
const MSG_DICTATION_RESULT: u8 = 0x02;
const ATTR_SPEEX: u8 = 0x01;
const ATTR_TRANSCRIPTION: u8 = 0x02;
const ATTR_APP_UUID: u8 = 0x03;
const FLAG_APP_INITIATED: u32 = 1;
const TRANSCRIPTION_SENTENCES: u8 = 0x01;

/// A session setup from the watch; `None` for anything else on the voice endpoint.
pub fn parse_setup(payload: &[u8]) -> Result<Option<Setup>> {
    let mut r = Reader::new(payload);
    if r.u8()? != MSG_SESSION_SETUP {
        return Ok(None);
    }
    let flags = r.u32_le()?;
    let kind = SessionType::from_byte(r.u8()?);
    let session = r.u16_le()?;
    let count = r.u8()?;
    let (mut speex, mut app) = (None, None);
    for _ in 0..count {
        let id = r.u8()?;
        let len = r.u16_le()? as usize;
        let data = r.bytes(len)?;
        let mut a = Reader::new(data);
        match id {
            ATTR_SPEEX => {
                speex = Some(Speex {
                    version: a.fixed_string(20)?,
                    sample_rate: a.u32_le()?,
                    bit_rate: a.u16_le()?,
                    bitstream_version: a.u8()?,
                    frame_size: a.u16_le()?,
                })
            }
            ATTR_APP_UUID => app = Some(a.uuid()?),
            _ => {}
        }
    }
    Ok(Some(Setup { app_initiated: flags & FLAG_APP_INITIATED != 0, kind, session, speex, app }))
}

fn flags(app_initiated: bool) -> [u8; 4] {
    (if app_initiated { FLAG_APP_INITIATED } else { 0 }).to_le_bytes()
}

/// The answer to a setup. An audio-stream setup also names its session.
pub fn setup_result(setup: &Setup, outcome: Outcome) -> Vec<u8> {
    let mut out = vec![MSG_SESSION_SETUP];
    out.extend_from_slice(&flags(setup.app_initiated));
    out.push(setup.kind.to_byte());
    out.push(outcome as u8);
    if setup.kind == SessionType::AudioStream {
        out.extend_from_slice(&setup.session.to_le_bytes());
    }
    out
}

fn attribute(out: &mut Vec<u8>, id: u8, data: &[u8]) {
    out.push(id);
    out.extend_from_slice(&(data.len().min(u16::MAX as usize) as u16).to_le_bytes());
    out.extend_from_slice(data);
}

/// The words of one sentence as the watch expects them: no empty words and no control
/// characters, which the watch rejects.
pub fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect()
}

/// The session's result. A successful one carries the text as one sentence of words; the watch
/// joins them with spaces. An app-initiated session names its app, or the watch drops the result.
pub fn dictation_result(setup: &Setup, outcome: Outcome, text: &str) -> Vec<u8> {
    let mut out = vec![MSG_DICTATION_RESULT];
    out.extend_from_slice(&flags(setup.app_initiated));
    out.extend_from_slice(&setup.session.to_le_bytes());
    out.push(outcome as u8);
    let words = if outcome == Outcome::Success { words(text) } else { Vec::new() };
    let app = setup.app.filter(|_| setup.app_initiated);
    out.push(u8::from(!words.is_empty()) + u8::from(app.is_some()));
    if !words.is_empty() {
        let mut t = vec![TRANSCRIPTION_SENTENCES, 1];
        t.extend_from_slice(&(words.len().min(u16::MAX as usize) as u16).to_le_bytes());
        for w in words.iter().take(u16::MAX as usize) {
            // Confidence 0: not available.
            t.push(0);
            t.extend_from_slice(&(w.len().min(u16::MAX as usize) as u16).to_le_bytes());
            t.extend_from_slice(w.as_bytes());
        }
        attribute(&mut out, ATTR_TRANSCRIPTION, &t);
    }
    if let Some(u) = app {
        attribute(&mut out, ATTR_APP_UUID, u.as_bytes());
    }
    out
}

/// What arrives on the audio endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audio {
    /// Speex frames, each a complete packet for the decoder.
    Data { session: u16, frames: Vec<Vec<u8>> },
    /// The watch stopped recording.
    Stop { session: u16 },
}

const AUDIO_DATA: u8 = 0x02;
const AUDIO_STOP: u8 = 0x03;

/// A message on the audio endpoint. Each frame is preceded by its length in one byte.
pub fn parse_audio(payload: &[u8]) -> Result<Audio> {
    let mut r = Reader::new(payload);
    let id = r.u8()?;
    let session = r.u16_le()?;
    match id {
        AUDIO_DATA => {
            let count = r.u8()?;
            let mut frames = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let len = r.u8()? as usize;
                frames.push(r.bytes(len)?.to_vec());
            }
            Ok(Audio::Data { session, frames })
        }
        AUDIO_STOP => Ok(Audio::Stop { session }),
        other => err(format!("unknown audio message {other:#04x}")),
    }
}

/// Asks the watch to stop recording.
pub fn stop_transfer(session: u16) -> Vec<u8> {
    let mut out = vec![AUDIO_STOP];
    out.extend_from_slice(&session.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speex_attr() -> Vec<u8> {
        let mut v = b"1.2.1".to_vec();
        v.resize(20, 0);
        v.extend_from_slice(&16000u32.to_le_bytes());
        v.extend_from_slice(&9800u16.to_le_bytes());
        v.push(4);
        v.extend_from_slice(&320u16.to_le_bytes());
        v
    }

    fn setup_bytes(app: Option<Uuid>) -> Vec<u8> {
        let mut m = vec![0x01];
        m.extend_from_slice(&(app.is_some() as u32).to_le_bytes());
        m.push(0x01);
        m.extend_from_slice(&7u16.to_le_bytes());
        m.push(1 + app.is_some() as u8);
        attribute(&mut m, 0x01, &speex_attr());
        if let Some(u) = app {
            attribute(&mut m, 0x03, u.as_bytes());
        }
        m
    }

    #[test]
    fn setup_from_the_watch() {
        let app = Uuid::parse_str("0b7b4b5c-1111-2222-3333-444455556666").unwrap();
        let s = parse_setup(&setup_bytes(Some(app))).unwrap().unwrap();
        assert!(s.app_initiated);
        assert_eq!(s.kind, SessionType::Dictation);
        assert_eq!(s.session, 7);
        assert_eq!(s.app, Some(app));
        assert_eq!(
            s.speex,
            Some(Speex { version: "1.2.1".into(), sample_rate: 16000, bit_rate: 9800, bitstream_version: 4, frame_size: 320 })
        );
        let system = parse_setup(&setup_bytes(None)).unwrap().unwrap();
        assert!(!system.app_initiated && system.app.is_none());
        assert_eq!(parse_setup(&[0x02, 0, 0, 0, 0]).unwrap(), None);
    }

    #[test]
    fn answers() {
        let app = Uuid::parse_str("0b7b4b5c-1111-2222-3333-444455556666").unwrap();
        let s = parse_setup(&setup_bytes(Some(app))).unwrap().unwrap();
        // SessionSetupResultMsg: id, flags (app initiated), type, result.
        assert_eq!(setup_result(&s, Outcome::Success), vec![0x01, 1, 0, 0, 0, 0x01, 0x00]);
        let stream = Setup { kind: SessionType::AudioStream, ..s.clone() };
        assert_eq!(setup_result(&stream, Outcome::ServiceUnavailable), vec![0x01, 1, 0, 0, 0, 0x04, 0x01, 7, 0]);

        let r = dictation_result(&s, Outcome::Success, " Turn on\tthe lights. ");
        // VoiceSessionResultMsg: id, flags, session, result, two attributes.
        assert_eq!(&r[..9], &[0x02, 1, 0, 0, 0, 7, 0, 0x00, 2]);
        let mut t = vec![0x01, 1, 4, 0];
        for w in ["Turn", "on", "the", "lights."] {
            t.push(0);
            t.extend_from_slice(&(w.len() as u16).to_le_bytes());
            t.extend_from_slice(w.as_bytes());
        }
        let mut expected = r[..9].to_vec();
        attribute(&mut expected, 0x02, &t);
        attribute(&mut expected, 0x03, app.as_bytes());
        assert_eq!(r, expected);

        // A failure carries only the app.
        let f = dictation_result(&s, Outcome::InvalidRecognizerResponse, "");
        assert_eq!(&f[..9], &[0x02, 1, 0, 0, 0, 7, 0, 0x04, 1]);
        // A system session carries nothing but the text.
        let system = parse_setup(&setup_bytes(None)).unwrap().unwrap();
        assert_eq!(&dictation_result(&system, Outcome::Success, "hi")[..9], &[0x02, 0, 0, 0, 0, 7, 0, 0x00, 1]);
    }

    #[test]
    fn audio_frames() {
        // DataTransferMsg as the watch sends it: one frame, its length first.
        let msg = [0x02, 7, 0, 1, 3, 0xAA, 0xBB, 0xCC];
        assert_eq!(parse_audio(&msg).unwrap(), Audio::Data { session: 7, frames: vec![vec![0xAA, 0xBB, 0xCC]] });
        assert_eq!(parse_audio(&[0x03, 7, 0]).unwrap(), Audio::Stop { session: 7 });
        assert!(parse_audio(&[0x02, 7, 0, 1, 9, 1]).is_err());
        assert_eq!(stop_transfer(7), vec![0x03, 7, 0]);
    }

    #[test]
    fn words_the_watch_accepts() {
        assert_eq!(words("a\u{0}b  c\n"), vec!["ab", "c"]);
        assert!(words("  ").is_empty());
    }
}
