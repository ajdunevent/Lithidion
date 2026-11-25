//! The background service on the session bus, and what the window needs to talk to it.
//!
//! Commands and events cross the bus as JSON. Each event carries a sequence number, and `Hello`
//! returns the latest state with the number it is current to, so a window that subscribes first
//! and then says hello can drop the events its snapshot already holds.

use crate::backend::{AppProgress, Command, Event, LinkState};
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

/// What the desktop's list of background apps says Lithidion is doing.
#[derive(Default)]
struct Status {
    watch: Option<String>,
    link: Option<LinkState>,
    updating: bool,
}

impl Status {
    /// Takes in an event; the new message when it changes what is shown.
    fn update(&mut self, e: &Event) -> Option<String> {
        let before = self.message();
        match e {
            Event::Watches { watches, active } => {
                self.watch = watches
                    .iter()
                    .find(|w| Some(w.id) == *active)
                    .map(|w| w.nickname.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| w.name()));
            }
            Event::Link(l) => self.link = Some(l.clone()),
            Event::Firmware(u) => self.updating = u.is_busy(),
            _ => return None,
        }
        let after = self.message();
        (after != before).then_some(after)
    }

    fn message(&self) -> String {
        let watch = self.watch.as_deref().unwrap_or("Pebble");
        let m = match &self.link {
            _ if self.updating => format!("Updating {watch}"),
            None | Some(LinkState::NoWatch) => "No watch".to_string(),
            Some(LinkState::Connected) => format!("Connected to {watch}"),
            Some(LinkState::Connecting(_) | LinkState::Negotiating) => format!("Connecting to {watch}"),
            Some(LinkState::Disconnected { retry_in: Some(_), .. }) => format!("Reconnecting to {watch}"),
            Some(LinkState::Disconnected { retry_in: None, .. }) => format!("{watch} is disconnected"),
        };
        // The portal's limit.
        m.chars().take(96).collect()
    }
}

/// Shows each message in the desktop's list of background apps, skipping any overtaken by a
/// newer one. Only sandboxed apps are listed there.
async fn show_status(messages: async_channel::Receiver<String>) {
    let proxy = match ashpd::desktop::background::BackgroundProxy::new().await {
        Ok(p) => p,
        Err(e) => {
            tracing::info!(error = %e, "no background portal for a status");
            return;
        }
    };
    while let Ok(mut message) = messages.recv().await {
        while let Ok(newer) = messages.try_recv() {
            message = newer;
        }
        let options = ashpd::desktop::background::SetStatusOptions::default().set_message(&message);
        if let Err(e) = proxy.set_status(options).await {
            tracing::info!(error = %e, "background status not set");
            return;
        }
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
    let mut status = Status::default();
    let (status_tx, status_rx) = async_channel::unbounded();
    if ashpd::is_sandboxed() {
        let _ = status_tx.try_send(status.message());
        glib::spawn_future_local(show_status(status_rx));
    }
    loop {
        let next = events.recv().fuse();
        let quit = quit_rx.recv().fuse();
        pin_mut!(next, quit);
        match future::select(next, quit).await {
            future::Either::Left((Ok(e), _)) => {
                if let Some(message) = status.update(&e) {
                    let _ = status_tx.try_send(message);
                }
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
    use crate::backend::Update;

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
        let mut st = Status::default();
        assert_eq!(st.message(), "No watch");
        assert_eq!(st.update(&Event::Link(LinkState::Connected)).as_deref(), Some("Connected to Pebble"));
        assert_eq!(st.update(&Event::Toast("x".into())), None);
        assert_eq!(st.update(&Event::Firmware(Update::Installing(0.2))).as_deref(), Some("Updating Pebble"));
        assert_eq!(st.update(&Event::Firmware(Update::Installing(0.3))), None);
        assert_eq!(st.update(&Event::Firmware(Update::UpToDate)).as_deref(), Some("Connected to Pebble"));
        let gone = LinkState::Disconnected { error: None, retry_in: Some(Duration::from_secs(5)) };
        assert_eq!(st.update(&Event::Link(gone)).as_deref(), Some("Reconnecting to Pebble"));
        // Through the bus and back.
        let bytes = encode(&Event::Battery(Some(70)));
        assert!(matches!(decode::<Event>(&bytes).unwrap(), Event::Battery(Some(70))));
    }
}
