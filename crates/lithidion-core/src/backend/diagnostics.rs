//! Saving the watch's stored log and core dump to files (PROTOCOL.md section 23).

use super::session::Protocol;
use crate::host::Zone;
use anyhow::{anyhow, Context, Result};
use chrono::TimeZone;
use pebble_protocol::diagnostics::{self, BytesError, BytesReply, LogReply};
use pebble_protocol::endpoint;
use std::path::Path;
use std::time::Duration;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::time::timeout;

/// The longest the watch may go quiet in the middle of a transfer.
const QUIET: Duration = Duration::from_secs(5);
/// Generations asked for at most, should the watch never say there are no more.
const GENERATIONS: u8 = 64;

/// What a save came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Saved {
    Done,
    /// The watch has none.
    Nothing,
}

async fn next(rx: &mut broadcast::Receiver<pebble_protocol::frame::Frame>, wanted: u16) -> Result<Option<Vec<u8>>> {
    loop {
        match timeout(QUIET, rx.recv()).await {
            Err(_) => return Ok(None),
            Ok(Err(RecvError::Lagged(_))) => return Err(anyhow!("part of the transfer was lost")),
            Ok(Err(RecvError::Closed)) => return Err(anyhow!("the connection ended")),
            Ok(Ok(f)) if f.endpoint == wanted => return Ok(Some(f.payload)),
            Ok(Ok(_)) => {}
        }
    }
}

/// Every generation of the stored log, from 0 up, as text in the local time zone.
pub async fn logs(protocol: &Protocol, path: &Path, progress: impl Fn(usize)) -> Result<Saved> {
    let mut rx = protocol.subscribe();
    let mut out = String::new();
    let mut lines = 0;
    for generation in 0..GENERATIONS {
        let cookie = chrono::Utc::now().timestamp_subsec_nanos() ^ u32::from(generation) << 24;
        protocol.send(endpoint::LOG_DUMP, &diagnostics::log_request(generation, cookie))?;
        let mut section = format!("=== Generation {generation} ===\n");
        let more = loop {
            let Some(payload) = next(&mut rx, endpoint::LOG_DUMP).await? else {
                return Err(anyhow!("the watch stopped sending its log"));
            };
            match diagnostics::parse_log(&payload) {
                Ok(Some((c, LogReply::Line(l)))) if c == cookie => {
                    let time = Zone.timestamp_opt(l.time as i64, 0).single().map(|t| t.to_rfc3339()).unwrap_or_default();
                    section.push_str(&format!("{} {time} {}:{}> {}\n", l.level_letter(), l.file, l.line, l.message));
                    lines += 1;
                    progress(lines);
                }
                Ok(Some((c, LogReply::Done))) if c == cookie => break true,
                Ok(Some((c, LogReply::NoLogs))) if c == cookie => break false,
                _ => {}
            }
        };
        if !more {
            break;
        }
        out.push_str(&section);
    }
    if lines == 0 {
        return Ok(Saved::Nothing);
    }
    tokio::fs::write(path, out).await.with_context(|| format!("cannot write {}", path.display()))?;
    Ok(Saved::Done)
}

/// The latest core dump, as the watch stored it.
pub async fn core_dump(protocol: &Protocol, path: &Path, progress: impl Fn(f64)) -> Result<Saved> {
    let mut rx = protocol.subscribe();
    let transaction = chrono::Utc::now().timestamp_subsec_nanos() as u8;
    protocol.send(endpoint::GET_BYTES, &diagnostics::core_dump_request(transaction))?;
    let mut data = Vec::new();
    let mut size = None;
    loop {
        let Some(payload) = next(&mut rx, endpoint::GET_BYTES).await? else {
            return Err(anyhow!("the watch stopped sending its core dump"));
        };
        match (diagnostics::parse_bytes(&payload), size) {
            (Ok(Some(BytesReply::Info { transaction: t, size: s })), None) if t == transaction => match s {
                Ok(0) | Err(BytesError::Missing) => return Ok(Saved::Nothing),
                Ok(n) => {
                    size = Some(n);
                    data.reserve(n as usize);
                }
                Err(BytesError::InProgress) => return Err(anyhow!("the watch is already sending one")),
                Err(BytesError::Corrupt) => return Err(anyhow!("the watch's core dump is corrupt")),
                Err(e) => return Err(anyhow!("the watch refused: {e:?}")),
            },
            (Ok(Some(BytesReply::Data { transaction: t, offset, data: chunk })), Some(n)) if t == transaction => {
                if offset as usize != data.len() || chunk.is_empty() || data.len() + chunk.len() > n as usize {
                    return Err(anyhow!("the core dump arrived out of order"));
                }
                data.extend_from_slice(&chunk);
                progress(data.len() as f64 / n as f64);
                if data.len() == n as usize {
                    break;
                }
            }
            _ => {}
        }
    }
    tokio::fs::write(path, data).await.with_context(|| format!("cannot write {}", path.display()))?;
    Ok(Saved::Done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pebble_protocol::frame::Frame;

    #[tokio::test]
    async fn logs_until_there_are_none() {
        let (protocol, mut sent, frames) = Protocol::loopback();
        let path = std::env::temp_dir().join(format!("lithidion-log-{}.txt", std::process::id()));
        let watch = tokio::spawn(async move {
            for generation in 0..3u8 {
                let frame = sent.recv().await.unwrap();
                // Frame header: length and endpoint, both two bytes.
                let request = &frame[4..];
                assert_eq!(&request[..2], &[0x10, generation]);
                let cookie = &request[2..6];
                let reply = |command: u8, rest: &[u8]| {
                    let mut p = vec![command];
                    p.extend_from_slice(cookie);
                    p.extend_from_slice(rest);
                    frames.send(Frame { endpoint: endpoint::LOG_DUMP, payload: p }).unwrap();
                };
                if generation == 2 {
                    reply(0x82, &[]);
                    break;
                }
                let mut line = vec![0x69, 0x1b, 0x9b, 0x40, 1, 4, 0, 7];
                line.extend_from_slice(b"main.c\0\0\0\0\0\0\0\0\0\0");
                line.extend_from_slice(b"oops");
                reply(0x80, &line);
                reply(0x81, &[]);
            }
        });
        let count = std::cell::Cell::new(0);
        assert_eq!(logs(&protocol, &path, |n| count.set(n)).await.unwrap(), Saved::Done);
        watch.await.unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(count.get(), 2);
        assert!(text.starts_with("=== Generation 0 ===\nE "), "{text}");
        assert!(text.contains(" main.c:7> oops\n=== Generation 1 ===\n"), "{text}");
        assert!(!text.contains("Generation 2"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn core_dump_in_order() {
        let (protocol, mut sent, frames) = Protocol::loopback();
        let path = std::env::temp_dir().join(format!("lithidion-core-{}.bin", std::process::id()));
        let watch = tokio::spawn(async move {
            let frame = sent.recv().await.unwrap();
            let t = frame[5];
            let send = |p: Vec<u8>| frames.send(Frame { endpoint: endpoint::GET_BYTES, payload: p }).unwrap();
            send(vec![1, t, 0, 0, 0, 0, 5]);
            send(vec![2, t, 0, 0, 0, 0, 1, 2, 3]);
            send(vec![2, t, 0, 0, 0, 3, 4, 5]);
        });
        assert_eq!(core_dump(&protocol, &path, |_| {}).await.unwrap(), Saved::Done);
        watch.await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3, 4, 5]);

        let watch = {
            let (protocol2, mut sent, frames) = Protocol::loopback();
            let task = tokio::spawn(async move {
                let frame = sent.recv().await.unwrap();
                frames.send(Frame { endpoint: endpoint::GET_BYTES, payload: vec![1, frame[5], 3, 0, 0, 0, 0] }).unwrap();
            });
            assert_eq!(core_dump(&protocol2, &path, |_| {}).await.unwrap(), Saved::Nothing);
            task
        };
        watch.await.unwrap();
        let _ = std::fs::remove_file(&path);
    }
}
