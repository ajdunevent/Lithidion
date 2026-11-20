//! The background service on the session bus, and what the window needs to talk to it.
//!
//! Commands and events cross the bus as JSON. Each event carries a sequence number, and `Hello`
//! returns the latest state with the number it is current to, so a window that subscribes first
//! and then says hello can drop the events its snapshot already holds.

use crate::backend::{AppProgress, Command, Event};
use futures::{future, pin_mut, FutureExt};
use std::mem::Discriminant;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::{fdo, interface, object_server::SignalEmitter};

pub const BUS_NAME: &str = "net.dunevent.Lithidion.Service";
pub const PATH: &str = "/net/dunevent/Lithidion/Service";
pub const INTERFACE: &str = "net.dunevent.Lithidion.Service1";
/// Changes with every build of the commands and events, so a window never talks to a service
/// from another build.
pub const BUILD: &str = env!("LITHIDION_BUILD");

pub fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("commands and events always serialize")
}

pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> serde_json::Result<T> {
    serde_json::from_slice(bytes)
}

type Key = (Discriminant<Event>, Option<uuid::Uuid>);

/// The latest event of each kind that describes state, in the order first seen.
#[derive(Default)]
pub struct Snapshot {
    pub seq: u64,
    events: Vec<(Key, Event)>,
}

impl Snapshot {
    fn key(e: &Event) -> Option<Key> {
        let kind = std::mem::discriminant(e);
        match e {
            Event::Watches { .. }
            | Event::Link(_)
            | Event::Battery(_)
            | Event::Session(_)
            | Event::Firmware(_)
            | Event::Apps(_)
            | Event::NotificationsEnabled(_)
            | Event::NotifyApps(_)
            | Event::NotifyHistory(_)
            | Event::PhoneQuiet(_)
            | Event::Health(_)
            | Event::WatchPrefs(_)
            | Event::Profiles { .. }
            | Event::Dictation(_)
            | Event::Settings(_) => Some((kind, None)),
            Event::App(uuid, _) => Some((kind, Some(*uuid))),
            _ => None,
        }
    }

    /// Takes in an event and numbers it.
    pub fn record(&mut self, e: &Event) -> u64 {
        self.seq += 1;
        if let Some(key) = Self::key(e) {
            let finished = matches!(e, Event::App(_, AppProgress::Done | AppProgress::Failed(_)));
            match self.events.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) if !finished => slot.1 = e.clone(),
                Some(_) => self.events.retain(|(k, _)| *k != key),
                None if !finished => self.events.push((key, e.clone())),
                None => {}
            }
        }
        self.seq
    }

    pub fn events(&self) -> impl Iterator<Item = &Event> {
        self.events.iter().map(|(_, e)| e)
    }
}

struct Service {
    snapshot: Arc<Mutex<Snapshot>>,
    commands: async_channel::Sender<Command>,
    quit: async_channel::Sender<()>,
}

#[interface(name = "net.dunevent.Lithidion.Service1")]
impl Service {
    /// This service's build, the sequence number the snapshot is current to, and the snapshot.
    async fn hello(&self, build: String) -> (String, u64, Vec<Vec<u8>>) {
        if build != BUILD {
            tracing::info!(window = build, service = BUILD, "a window from another build");
        }
        let s = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        (BUILD.to_string(), s.seq, s.events().map(encode).collect())
    }

    async fn command(&self, payload: Vec<u8>) -> fdo::Result<()> {
        let command: Command = decode(&payload).map_err(|e| fdo::Error::InvalidArgs(format!("not a command: {e}")))?;
        if matches!(command, Command::Shutdown) {
            let _ = self.quit.try_send(());
            return Ok(());
        }
        self.commands.send(command).await.map_err(|_| fdo::Error::Failed("the backend has stopped".into()))
    }

    /// Stops the watch connection and exits.
    async fn quit(&self) {
        let _ = self.quit.try_send(());
    }

    #[zbus(signal)]
    async fn event(emitter: &SignalEmitter<'_>, seq: u64, payload: Vec<u8>) -> zbus::Result<()>;
}

/// Serves the backend on the session bus until asked to quit or the backend stops. Fails if
/// another service already owns the name.
pub async fn serve(commands: async_channel::Sender<Command>, events: async_channel::Receiver<Event>) -> anyhow::Result<()> {
    let snapshot = Arc::new(Mutex::new(Snapshot::default()));
    let (quit_tx, quit_rx) = async_channel::bounded(1);
    let service = Service { snapshot: snapshot.clone(), commands: commands.clone(), quit: quit_tx };
    let conn = zbus::connection::Builder::session()?.serve_at(PATH, service)?.name(BUS_NAME)?.build().await?;
    let iface = conn.object_server().interface::<_, Service>(PATH).await?;
    tracing::info!(build = BUILD, "service started");
    loop {
        let next = events.recv().fuse();
        let quit = quit_rx.recv().fuse();
        pin_mut!(next, quit);
        match future::select(next, quit).await {
            future::Either::Left((Ok(e), _)) => {
                // Numbered and sent under the lock, so a snapshot is never older than an
                // event numbered after it.
                let (seq, payload) = {
                    let mut s = snapshot.lock().unwrap_or_else(|p| p.into_inner());
                    (s.record(&e), encode(&e))
                };
                if let Err(e) = Service::event(iface.signal_emitter(), seq, payload).await {
                    tracing::warn!(error = %e, "event not sent");
                }
            }
            future::Either::Left((Err(_), _)) => break,
            future::Either::Right(_) => {
                tracing::info!("quitting");
                let _ = commands.send(Command::Shutdown).await;
                // Time for the watch connection to close cleanly.
                let drained = async { while events.recv().await.is_ok() {} };
                let _ = future::select(Box::pin(drained), Box::pin(async_io::Timer::after(Duration::from_secs(3)))).await;
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::LinkState;

    #[test]
    fn snapshot_keeps_the_latest_state() {
        let mut s = Snapshot::default();
        let app = uuid::Uuid::from_u128(5);
        s.record(&Event::Link(LinkState::NoWatch));
        s.record(&Event::Toast("hi".into()));
        s.record(&Event::Battery(Some(80)));
        s.record(&Event::App(app, AppProgress::Downloading(0.5)));
        assert_eq!(s.record(&Event::Battery(Some(70))), 5);
        let kinds: Vec<String> = s.events().map(|e| format!("{e:?}")).collect();
        assert_eq!(kinds.len(), 3);
        assert!(kinds[1].contains("Battery(Some(70))"));
        s.record(&Event::App(app, AppProgress::Done));
        assert_eq!(s.events().count(), 2);
        // Through the bus and back.
        let bytes = encode(&Event::Battery(Some(70)));
        assert!(matches!(decode::<Event>(&bytes).unwrap(), Event::Battery(Some(70))));
    }
}
