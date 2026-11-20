//! A Pebble Protocol session on an open link: the PPoGATT pump, request helpers, the session
//! start (PROTOCOL.md section 6), and the messages the watch sends unprompted.

use super::link::Link;
use anyhow::{anyhow, Result};
use pebble_protocol::capabilities::{Capabilities, Capability};
use pebble_protocol::frame::{encode, Frame, Reassembler};
use pebble_protocol::ppog::{Action, CloseReason, Session};
use pebble_protocol::system::{self, AppRunState, OsType, PingPong, WatchVersion};
use pebble_protocol::{endpoint, endpoint::name};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{sleep_until, timeout, Instant};
use uuid::Uuid;

/// What Lithidion implements (PROTOCOL.md section 7).
pub fn phone_capabilities() -> Capabilities {
    // Dictation is always offered, as the official app does: with it off, apps show the watch's
    // own "turned off" message rather than one about the connection.
    // Imaging is offered for notification pictures; album art is refused when asked for.
    Capabilities::from_list(&[
        Capability::AppRunState,
        Capability::InfiniteLogDump,
        Capability::ExtendedMusic,
        Capability::AppDictation,
        Capability::WeatherApp,
        Capability::ImageFetch,
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opened {
    pub version: u8,
    pub tx_window: u8,
    pub rx_window: u8,
}

/// Sends frames and waits for replies; cheap to clone.
#[derive(Clone)]
pub struct Protocol {
    out: mpsc::UnboundedSender<Vec<u8>>,
    frames: broadcast::Sender<Frame>,
}

impl Protocol {
    pub fn send(&self, endpoint: u16, payload: &[u8]) -> Result<()> {
        tracing::debug!(endpoint = name(endpoint), len = payload.len(), "tx");
        self.out.send(encode(endpoint, payload)).map_err(|_| anyhow!("the session has ended"))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Frame> {
        self.frames.subscribe()
    }

    /// Sends `payload` and returns the first frame on `reply` that `matches` accepts.
    pub async fn request(
        &self,
        endpoint: u16,
        payload: &[u8],
        reply: u16,
        matches: impl Fn(&[u8]) -> bool,
        limit: Duration,
    ) -> Result<Vec<u8>> {
        let mut rx = self.subscribe();
        self.send(endpoint, payload)?;
        wait_for(&mut rx, reply, matches, limit).await
    }
}

#[cfg(test)]
impl Protocol {
    /// A protocol with no link: what it sends comes out of the receiver as encoded frames, and
    /// frames sent on the sender arrive as if from the watch.
    pub fn loopback() -> (Protocol, mpsc::UnboundedReceiver<Vec<u8>>, broadcast::Sender<Frame>) {
        let (out, rx) = mpsc::unbounded_channel();
        let (frames, _) = broadcast::channel(64);
        (Protocol { out, frames: frames.clone() }, rx, frames)
    }
}

pub async fn wait_for(
    rx: &mut broadcast::Receiver<Frame>,
    endpoint: u16,
    matches: impl Fn(&[u8]) -> bool,
    limit: Duration,
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + limit;
    loop {
        let frame = match timeout(deadline.saturating_duration_since(Instant::now()), rx.recv()).await {
            Err(_) => return Err(anyhow!("no answer from the watch ({})", name(endpoint))),
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!(n, "frames dropped while waiting");
                continue;
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => return Err(anyhow!("the session has ended")),
            Ok(Ok(f)) => f,
        };
        if frame.endpoint == endpoint && matches(&frame.payload) {
            return Ok(frame.payload);
        }
    }
}

/// Creates the protocol handle and the pump that drives PPoGATT over `link`. The pump ends with
/// the reason the session closed.
pub fn start(link: Link) -> (Protocol, oneshot::Receiver<Opened>, impl std::future::Future<Output = Result<()>>) {
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (frames, _) = broadcast::channel(256);
    let (opened_tx, opened_rx) = oneshot::channel();
    let protocol = Protocol { out: out_tx, frames: frames.clone() };
    (protocol, opened_rx, pump(link, out_rx, frames, opened_tx))
}

async fn pump(
    mut link: Link,
    mut out: mpsc::UnboundedReceiver<Vec<u8>>,
    frames: broadcast::Sender<Frame>,
    opened: oneshot::Sender<Opened>,
) -> Result<()> {
    let (mut session, first) = Session::new(link.transport.role(), Instant::now().into_std());
    let mut opened = Some(opened);
    let mut reassembler = Reassembler::new();
    let mut mtu_check = tokio::time::interval(Duration::from_secs(15));
    let mut pending = first;
    loop {
        for action in pending.drain(..) {
            match action {
                Action::Write(bytes) => link.write(&bytes).await?,
                Action::Deliver(bytes) => {
                    reassembler.push(&bytes);
                    while let Some(frame) = reassembler.next_frame() {
                        tracing::debug!(endpoint = name(frame.endpoint), len = frame.payload.len(), "rx");
                        let _ = frames.send(frame);
                    }
                }
                Action::Opened { version, tx_window, rx_window } => {
                    session.set_mtu(link.mtu().await);
                    tracing::info!(version, tx_window, rx_window, mtu = session.mtu(), "PPoGATT open");
                    if let Some(tx) = opened.take() {
                        let _ = tx.send(Opened { version, tx_window, rx_window });
                    }
                }
                Action::Closed(reason) => {
                    return Err(anyhow!(match reason {
                        CloseReason::OpenTimeout => "the watch did not start the PPoGATT session",
                        CloseReason::AckTimeout => "the watch stopped acknowledging data",
                        CloseReason::PeerReset => "the watch reset the session",
                    }));
                }
            }
        }
        let deadline = session.next_deadline().map(Instant::from_std);
        let now = || Instant::now().into_std();
        tokio::select! {
            packet = link.inbound.recv() => match packet {
                Some(p) => pending = session.on_packet(&p, now()),
                None => return Err(anyhow!("the watch's data stream ended")),
            },
            frame = out.recv() => match frame {
                Some(f) => pending = session.send(&f, now()),
                None => return Ok(()),
            },
            _ = async { sleep_until(deadline.expect("guarded")).await }, if deadline.is_some() => pending = session.on_tick(now()),
            _ = mtu_check.tick() => {
                if session.is_open() {
                    session.set_mtu(link.mtu().await);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct WatchInfo {
    pub version: WatchVersion,
    pub model: Option<i32>,
    pub running_app: Option<Uuid>,
}

pub fn time_message() -> Vec<u8> {
    let now = chrono::Utc::now();
    let unix = ((now.timestamp_millis() + 500) / 1000) as u32;
    let offset = crate::host::Zone::now().offset().local_minus_utc() / 60;
    system::set_utc(unix, offset as i16, &zone_matching(&time_zone(), now, offset))
}

/// The watch takes the zone by name from its own database and ignores the offset, so a name that
/// disagrees with the offset (`UTC` while the clock is in Chicago) sets the wrong time. Such a
/// name is left out, and the watch uses the offset.
fn zone_matching(name: &str, now: chrono::DateTime<chrono::Utc>, offset_minutes: i32) -> String {
    use chrono::Offset;
    match name.parse::<chrono_tz::Tz>() {
        Ok(tz) if now.with_timezone(&tz).offset().fix().local_minus_utc() / 60 == offset_minutes => name.to_string(),
        _ => {
            tracing::warn!(zone = name, offset_minutes, "the zone's name does not match the clock; sending the offset alone");
            String::new()
        }
    }
}

/// The zone's name: timedated's (see host.rs), else `TZ`, else the system's.
pub fn time_zone() -> String {
    if let Some(name) = crate::host::zone_name() {
        return name;
    }
    match std::env::var("TZ") {
        Ok(tz) if !tz.trim_start_matches(':').is_empty() => tz.trim_start_matches(':').to_string(),
        _ => iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string()),
    }
}

/// The session start: phone version, watch version, model, time, and running app.
pub async fn negotiate(protocol: &Protocol, early: &mut broadcast::Receiver<Frame>) -> Result<WatchInfo> {
    wait_for(early, endpoint::PHONE_VERSION, system::is_phone_version_request, Duration::from_secs(20)).await?;
    protocol.send(endpoint::PHONE_VERSION, &system::phone_version_response(OsType::Android, &phone_capabilities()))?;
    // Answered on the watch's main task, which can be busy right after a boot.
    let payload = protocol
        .request(
            endpoint::WATCH_VERSION,
            &system::WATCH_VERSION_REQUEST,
            endpoint::WATCH_VERSION,
            |p| p.first() == Some(&1),
            Duration::from_secs(30),
        )
        .await?;
    let version = WatchVersion::parse(&payload)?;
    tracing::info!(firmware = %version.running.tag, recovery = %version.recovery.tag, board = %version.board, unfaithful = ?version.unfaithful, "watch version");
    // Recovery firmware 4.9.142 answered after 12 s.
    let model = match protocol
        .request(
            endpoint::FACTORY_REGISTRY,
            &system::factory_request("mfg_color"),
            endpoint::FACTORY_REGISTRY,
            |_| true,
            Duration::from_secs(30),
        )
        .await
    {
        Ok(p) => system::parse_factory_response(&p).ok().flatten().and_then(|v| system::model_number(&v)),
        Err(e) => {
            tracing::info!(error = %e, "model not read");
            None
        }
    };
    protocol.send(endpoint::TIME, &time_message())?;
    let mut running_app = None;
    if !version.running.is_recovery() {
        let started = protocol
            .request(
                endpoint::APP_RUN_STATE,
                &AppRunState::Request.encode(),
                endpoint::APP_RUN_STATE,
                |p| matches!(AppRunState::parse(p), Ok(AppRunState::Started(_))),
                Duration::from_secs(10),
            )
            .await;
        if let Ok(p) = started {
            if let Ok(AppRunState::Started(u)) = AppRunState::parse(&p) {
                running_app = Some(u);
            }
        }
    }
    Ok(WatchInfo { version, model, running_app })
}

pub enum Unprompted {
    RunningApp(Option<Uuid>),
    Nothing,
}

/// Answers what the watch sends on its own.
pub fn handle(protocol: &Protocol, frame: &Frame) -> Unprompted {
    match frame.endpoint {
        endpoint::PING => {
            if let Ok(PingPong::Ping(cookie)) = system::parse_ping(&frame.payload) {
                let _ = protocol.send(endpoint::PING, &system::pong(cookie));
            }
        }
        endpoint::TIME if system::is_get_utc_request(&frame.payload) => {
            let _ = protocol.send(endpoint::TIME, &time_message());
        }
        endpoint::PHONE_VERSION if system::is_phone_version_request(&frame.payload) => {
            let _ = protocol.send(endpoint::PHONE_VERSION, &system::phone_version_response(OsType::Android, &phone_capabilities()));
        }
        endpoint::APP_RUN_STATE => match AppRunState::parse(&frame.payload) {
            Ok(AppRunState::Started(u)) => return Unprompted::RunningApp(Some(u)),
            Ok(AppRunState::Stopped(_)) => return Unprompted::RunningApp(None),
            _ => {}
        },
        // Answers to requests, taken by whoever is waiting for them.
        endpoint::PUT_BYTES | endpoint::SYSTEM_MESSAGE | endpoint::BLOBDB_V1 | endpoint::APP_REORDER => {}
        _ => tracing::debug!(endpoint = frame.endpoint, "unhandled frame"),
    }
    Unprompted::Nothing
}

/// Round trip of a ping.
pub async fn ping(protocol: &Protocol) -> Result<Duration> {
    let cookie = (chrono::Utc::now().timestamp_subsec_nanos()) ^ 0x5A5A_5A5A;
    let start = Instant::now();
    protocol
        .request(
            endpoint::PING,
            &system::ping(cookie),
            endpoint::PING,
            |p| system::parse_ping(p) == Ok(PingPong::Pong(cookie)),
            Duration::from_secs(15),
        )
        .await?;
    Ok(start.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_name_must_match_the_offset() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-15T15:00:00Z").unwrap().with_timezone(&chrono::Utc);
        assert_eq!(zone_matching("America/Chicago", now, -360), "America/Chicago");
        assert_eq!(zone_matching("UTC", now, -360), "");
        assert_eq!(zone_matching("Not/AZone", now, 0), "");
        assert_eq!(zone_matching("UTC", now, 0), "UTC");
    }
}
