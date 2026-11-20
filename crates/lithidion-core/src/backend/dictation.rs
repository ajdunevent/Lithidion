//! Dictation (PROTOCOL.md section 21). The watch asks for a session and records; Lithidion decodes
//! the Speex frames as they arrive, transcribes the recording on this device with Parakeet
//! (through transcribe.cpp) once the watch stops, and sends the text back. The model is downloaded
//! when dictation is turned on, and loaded only for each transcription, which takes milliseconds.
//! Transcribed text is never logged.

use super::session::Protocol;
use anyhow::{bail, Context, Result};
use futures::StreamExt;
use oxideav_speex::SpeexStreamDecoder;
use pebble_protocol::endpoint;
use pebble_protocol::voice::{self, Audio, Outcome, SessionType, Setup};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

/// Parakeet TDT-CTC 110M, English, by NVIDIA (CC-BY-4.0), as converted for transcribe.cpp by
/// handy-computer. Pinned to a commit, and checked against its SHA-256.
pub const MODEL_NAME: &str = "Parakeet TDT-CTC 110M";
const MODEL_FILE: &str = "parakeet-tdt_ctc-110m-Q8_0.gguf";
const MODEL_URL: &str = "https://huggingface.co/handy-computer/parakeet-tdt_ctc-110m-gguf/resolve/\
1c3f0d5378fdd358905cc410a93ed9e64f52d170/parakeet-tdt_ctc-110m-Q8_0.gguf";
const MODEL_SIZE: u64 = 135_373_280;
const MODEL_SHA256: &str = "7dd44c74a331d788a4e5f8b16913b3feb29ced22cf5613aad0e0f6cd30516296";

/// The watch records 16 kHz audio; anything longer than this is cut off.
const RATE: usize = 16_000;
const MAX_SECONDS: usize = 60;

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Progress of a model download in progress.
static DOWNLOADING: Mutex<Option<f64>> = Mutex::new(None);

pub fn model_path() -> PathBuf {
    glib::user_data_dir().join("lithidion").join("models").join(MODEL_FILE)
}

/// The model is in place (its hash was checked when it was downloaded).
pub fn model_ready() -> bool {
    std::fs::metadata(model_path()).is_ok_and(|m| m.len() == MODEL_SIZE)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Dictation as the Preferences show it.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum State {
    Off,
    Downloading(f64),
    Ready,
    /// The model could not be fetched; dictation was turned off.
    Failed(String),
}

pub fn state() -> State {
    if let Some(p) = *DOWNLOADING.lock().unwrap_or_else(|p| p.into_inner()) {
        return State::Downloading(p);
    }
    if enabled() && model_ready() {
        State::Ready
    } else {
        State::Off
    }
}

/// Downloads the model, checks its size and hash, and moves it into place.
pub async fn download(client: &reqwest::Client, mut progress: impl FnMut(f64)) -> Result<()> {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            *DOWNLOADING.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
    }
    let _clear = Clear;
    let mut report = |p: f64| {
        *DOWNLOADING.lock().unwrap_or_else(|p| p.into_inner()) = Some(p);
        progress(p);
    };
    report(0.0);
    let path = model_path();
    let dir = path.parent().context("no folder for the model")?;
    tokio::fs::create_dir_all(dir).await.with_context(|| format!("cannot create {}", dir.display()))?;
    let part = path.with_extension("gguf.part");
    let response = client
        .get(MODEL_URL)
        .timeout(Duration::from_secs(1800))
        .send()
        .await
        .context("the speech model download did not start")?
        .error_for_status()
        .context("the speech model download failed")?;
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = response.bytes_stream();
    let mut hash = Sha256::new();
    let mut done = 0u64;
    let mut shown = -1i64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("the speech model download was interrupted")?;
        hash.update(&chunk);
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        // Whole percents, so the UI is not flooded.
        let percent = (done * 100 / MODEL_SIZE) as i64;
        if percent != shown {
            shown = percent;
            report(done as f64 / MODEL_SIZE as f64);
        }
    }
    file.flush().await?;
    drop(file);
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if done != MODEL_SIZE || digest != MODEL_SHA256 {
        let _ = tokio::fs::remove_file(&part).await;
        bail!("the speech model download was not the expected file");
    }
    tokio::fs::rename(&part, &path).await?;
    tracing::info!(model = MODEL_NAME, "speech model downloaded");
    Ok(())
}

pub async fn remove_model() {
    let path = model_path();
    let _ = tokio::fs::remove_file(path.with_extension("gguf.part")).await;
    let _ = tokio::fs::remove_file(path).await;
}

/// The recording of the session in progress.
struct Active {
    setup: Setup,
    decoder: SpeexStreamDecoder,
    pcm: Vec<i16>,
    bad_frames: u32,
    started: Instant,
}

/// A connection's dictation: at most one session at a time, as on the watch.
#[derive(Default)]
pub struct Dictation {
    active: Option<Active>,
}

impl Dictation {
    /// A message on the voice endpoint.
    pub fn voice(&mut self, protocol: &Protocol, payload: &[u8]) {
        let setup = match voice::parse_setup(payload) {
            Ok(Some(s)) => s,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(error = %e, "voice message not understood");
                return;
            }
        };
        // The watch runs one session; a new one replaces whatever was left.
        self.active = None;
        let answer = |outcome: Outcome| {
            let _ = protocol.send(endpoint::VOICE_CONTROL, &voice::setup_result(&setup, outcome));
        };
        if setup.kind != SessionType::Dictation {
            tracing::info!(kind = ?setup.kind, "voice session refused: only dictation is supported");
            answer(if setup.kind == SessionType::AudioStream { Outcome::ServiceUnavailable } else { Outcome::Disabled });
            return;
        }
        if !enabled() || !model_ready() {
            tracing::info!(app = ?setup.app, "dictation refused: turned off");
            answer(Outcome::Disabled);
            return;
        }
        let decoder = match &setup.speex {
            Some(s) if s.sample_rate as usize == RATE => SpeexStreamDecoder::for_sample_rate(s.sample_rate).ok(),
            _ => None,
        };
        let Some(decoder) = decoder else {
            tracing::info!(speex = ?setup.speex, "dictation refused: audio format not supported");
            answer(Outcome::InvalidMessage);
            return;
        };
        answer(Outcome::Success);
        tracing::info!(app = ?setup.app, session = setup.session, "dictation started");
        self.active = Some(Active { setup, decoder, pcm: Vec::new(), bad_frames: 0, started: Instant::now() });
    }

    /// A message on the audio endpoint.
    pub fn audio(&mut self, protocol: &Protocol, payload: &[u8]) {
        let message = match voice::parse_audio(payload) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(error = %e, "audio message not understood");
                return;
            }
        };
        let Some(active) = self.active.as_mut() else { return };
        match message {
            Audio::Data { session, frames } if session == active.setup.session => {
                for frame in frames {
                    match active.decoder.decode_packet_pcm_i16(&frame) {
                        Ok(pcm) => active.pcm.extend_from_slice(&pcm),
                        Err(_) => active.bad_frames += 1,
                    }
                }
                if active.pcm.len() >= RATE * MAX_SECONDS {
                    let _ = protocol.send(endpoint::AUDIO_STREAM, &voice::stop_transfer(session));
                    self.finish(protocol);
                }
            }
            Audio::Stop { session } if session == active.setup.session => self.finish(protocol),
            _ => {}
        }
    }

    /// The recording is complete: transcribe it away from the connection and send the result.
    fn finish(&mut self, protocol: &Protocol) {
        let Some(active) = self.active.take() else { return };
        let protocol = protocol.clone();
        tokio::spawn(async move {
            let Active { setup, pcm, bad_frames, started, .. } = active;
            let seconds = pcm.len() as f64 / RATE as f64;
            let begun = Instant::now();
            let text = tokio::task::spawn_blocking(move || transcribe(&pcm)).await;
            let (outcome, text) = match text {
                Ok(Ok(t)) if !voice::words(&t).is_empty() => (Outcome::Success, t),
                Ok(Ok(_)) => (Outcome::InvalidRecognizerResponse, String::new()),
                Ok(Err(e)) => {
                    tracing::warn!(error = format!("{e:#}"), "transcription failed");
                    (Outcome::RecognizerError, String::new())
                }
                Err(e) => {
                    tracing::warn!(error = %e, "transcription stopped");
                    (Outcome::RecognizerError, String::new())
                }
            };
            tracing::info!(
                session = setup.session,
                ?outcome,
                words = voice::words(&text).len(),
                audio_s = format!("{seconds:.1}"),
                transcribed_s = format!("{:.2}", begun.elapsed().as_secs_f64()),
                total_s = format!("{:.1}", started.elapsed().as_secs_f64()),
                bad_frames,
                "dictation finished"
            );
            let _ = protocol.send(endpoint::VOICE_CONTROL, &voice::dictation_result(&setup, outcome, &text));
        });
    }
}

/// Transcribes 16 kHz mono audio with the downloaded model. Blocking.
pub fn transcribe(pcm: &[i16]) -> Result<String> {
    static QUIET: std::sync::Once = std::sync::Once::new();
    QUIET.call_once(transcribe_cpp::disable_logging);
    if pcm.is_empty() {
        return Ok(String::new());
    }
    let model = transcribe_cpp::Model::load(model_path()).context("the speech model cannot be loaded")?;
    let mut session = model.session()?;
    let samples: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
    let result = session.run(&samples, &transcribe_cpp::RunOptions::default())?;
    Ok(result.text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sample encoded as the watch encodes it (Speex wideband at 9.8 kbit/s, quality 3), sent
    /// through a session frame by frame, decoded, and transcribed. Needs the model at
    /// LITHIDION_TEST_MODEL and a 16 kHz mono WAV at LITHIDION_TEST_WAV.
    #[test]
    #[ignore]
    fn watch_audio_round_trip() {
        let model = std::env::var("LITHIDION_TEST_MODEL").expect("LITHIDION_TEST_MODEL");
        let wav = std::env::var("LITHIDION_TEST_WAV").expect("LITHIDION_TEST_WAV");
        let bytes = std::fs::read(wav).unwrap();
        // A plain 44-byte header, 16-bit samples.
        let pcm: Vec<i16> = bytes[44..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        let mut encoder = oxideav_speex::SpeexStreamEncoder::new(16_000, 3).unwrap();
        assert_eq!(encoder.nominal_bitrate_bps(), Some(9_800));
        let mut decoder = SpeexStreamDecoder::for_sample_rate(16_000).unwrap();
        let mut decoded = Vec::new();
        let mut bytes_sent = 0;
        for frame in pcm.chunks_exact(encoder.frame_samples()) {
            let packet = encoder.encode_frame(frame).unwrap();
            bytes_sent += packet.len();
            // Through the audio endpoint's framing.
            let mut msg = vec![0x02, 7, 0, 1, packet.len() as u8];
            msg.extend_from_slice(&packet);
            let Audio::Data { frames, .. } = voice::parse_audio(&msg).unwrap() else { panic!() };
            decoded.extend(decoder.decode_packet_pcm_i16(&frames[0]).unwrap());
        }
        let seconds = pcm.len() as f64 / 16_000.0;
        eprintln!("{:.1} s, {:.1} kbit/s", seconds, bytes_sent as f64 * 8.0 / seconds / 1000.0);
        let original = transcribe_with(&model, &pcm);
        let through_speex = transcribe_with(&model, &decoded);
        eprintln!("original:      {original}\nthrough Speex: {through_speex}");
        let (a, b) = (voice::words(&original.to_lowercase()), voice::words(&through_speex.to_lowercase()));
        let same = a.iter().filter(|w| b.contains(w)).count();
        assert!(same * 10 >= a.len() * 8, "too many words lost through Speex");
    }

    fn setup_message(kind: u8, session: u16, app: Option<uuid::Uuid>) -> Vec<u8> {
        let mut m = vec![0x01];
        m.extend_from_slice(&(app.is_some() as u32).to_le_bytes());
        m.push(kind);
        m.extend_from_slice(&session.to_le_bytes());
        m.push(1 + app.is_some() as u8);
        let mut speex = b"1.2.1".to_vec();
        speex.resize(20, 0);
        speex.extend_from_slice(&16000u32.to_le_bytes());
        speex.extend_from_slice(&9800u16.to_le_bytes());
        speex.push(4);
        speex.extend_from_slice(&320u16.to_le_bytes());
        m.push(0x01);
        m.extend_from_slice(&(speex.len() as u16).to_le_bytes());
        m.extend_from_slice(&speex);
        if let Some(u) = app {
            m.push(0x03);
            m.extend_from_slice(&16u16.to_le_bytes());
            m.extend_from_slice(u.as_bytes());
        }
        m
    }

    /// What Lithidion sent: endpoint and payload of each frame.
    fn sent(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) -> Vec<(u16, Vec<u8>)> {
        let mut out = Vec::new();
        while let Ok(f) = rx.try_recv() {
            out.push((u16::from_be_bytes([f[2], f[3]]), f[4..].to_vec()));
        }
        out
    }

    #[test]
    fn refusals() {
        let (protocol, mut rx, _) = Protocol::loopback();
        let mut d = Dictation::default();
        set_enabled(false);
        d.voice(&protocol, &setup_message(0x01, 3, None));
        // Turned off: Disabled, so the watch says so rather than blaming the connection.
        assert_eq!(sent(&mut rx), vec![(endpoint::VOICE_CONTROL, vec![0x01, 0, 0, 0, 0, 0x01, 0x05])]);
        assert!(d.active.is_none());
        d.voice(&protocol, &setup_message(0x04, 3, None));
        assert_eq!(sent(&mut rx), vec![(endpoint::VOICE_CONTROL, vec![0x01, 0, 0, 0, 0, 0x04, 0x01, 3, 0])]);
        // Audio with no session in progress is ignored.
        d.audio(&protocol, &[0x03, 3, 0]);
        assert!(sent(&mut rx).is_empty());
    }

    /// A whole session: setup from an app, the recording as the watch sends it, Stop, and the
    /// result. Needs the model at LITHIDION_TEST_MODEL and a 16 kHz mono WAV at
    /// LITHIDION_TEST_WAV; run alone, as it points the data folder at a temporary one.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn session_from_setup_to_result() {
        let model = std::env::var("LITHIDION_TEST_MODEL").expect("LITHIDION_TEST_MODEL");
        let wav = std::fs::read(std::env::var("LITHIDION_TEST_WAV").expect("LITHIDION_TEST_WAV")).unwrap();
        let data = std::env::temp_dir().join(format!("lithidion-dictation-{}", std::process::id()));
        std::env::set_var("XDG_DATA_HOME", &data);
        std::fs::create_dir_all(model_path().parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&model, model_path()).unwrap();
        assert!(model_ready());
        set_enabled(true);

        let (protocol, mut rx, _) = Protocol::loopback();
        let mut d = Dictation::default();
        let app = uuid::Uuid::parse_str("0b7b4b5c-1111-2222-3333-444455556666").unwrap();
        d.voice(&protocol, &setup_message(0x01, 9, Some(app)));
        assert_eq!(sent(&mut rx), vec![(endpoint::VOICE_CONTROL, vec![0x01, 1, 0, 0, 0, 0x01, 0x00])]);

        let pcm: Vec<i16> = wav[44..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        let mut encoder = oxideav_speex::SpeexStreamEncoder::new(16_000, 3).unwrap();
        for frame in pcm.chunks_exact(encoder.frame_samples()) {
            let packet = encoder.encode_frame(frame).unwrap();
            let mut msg = vec![0x02, 9, 0, 1, packet.len() as u8];
            msg.extend_from_slice(&packet);
            d.audio(&protocol, &msg);
        }
        d.audio(&protocol, &[0x03, 9, 0]);
        let frame = tokio::time::timeout(Duration::from_secs(60), rx.recv()).await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&data);
        assert_eq!(u16::from_be_bytes([frame[2], frame[3]]), endpoint::VOICE_CONTROL);
        let result = &frame[4..];
        // Dictation Result, app-initiated, session 9, success, two attributes.
        assert_eq!(&result[..9], &[0x02, 1, 0, 0, 0, 9, 0, 0x00, 2]);
        let text = String::from_utf8_lossy(result).to_lowercase();
        eprintln!("{text}");
        assert!(text.contains("country"));
    }

    fn transcribe_with(model: &str, pcm: &[i16]) -> String {
        transcribe_cpp::disable_logging();
        let model = transcribe_cpp::Model::load(model).unwrap();
        let samples: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        model.session().unwrap().run(&samples, &transcribe_cpp::RunOptions::default()).unwrap().text
    }
}
