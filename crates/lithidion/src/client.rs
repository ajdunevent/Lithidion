//! The window's side of the service on the session bus.

use crate::backend::{Command, Event};
use anyhow::{anyhow, bail, Context, Result};
use futures::{future, StreamExt};
use lithidion_core::service::{self, BUILD, BUS_NAME, INTERFACE, PATH};
use std::cell::RefCell;
use std::time::Duration;

thread_local! {
    static PROXY: RefCell<Option<zbus::Proxy<'static>>> = const { RefCell::new(None) };
    /// The window stopped the service itself, so it is not started again.
    static QUITTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub struct Connected {
    pub snapshot: Vec<Event>,
    pub commands: async_channel::Sender<Command>,
    pub events: async_channel::Receiver<Event>,
}

type Hello = (String, u64, Vec<Vec<u8>>);

fn decode_snapshot(events: &[Vec<u8>]) -> Vec<Event> {
    events
        .iter()
        .filter_map(|b| match service::decode(b) {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::warn!(error = %e, "an event from the service could not be read");
                None
            }
        })
        .collect()
}

async fn hello(proxy: &zbus::Proxy<'static>) -> zbus::Result<Hello> {
    proxy.call("Hello", &(BUILD,)).await
}

/// Waits for the name to have no owner, or for `limit`.
async fn until_gone(conn: &zbus::Connection, limit: Duration) {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else { return };
    let name = zbus::names::BusName::try_from(BUS_NAME).expect("a valid bus name");
    let deadline = std::time::Instant::now() + limit;
    while dbus.name_has_owner(name.clone()).await.unwrap_or(false) && std::time::Instant::now() < deadline {
        glib::timeout_future(Duration::from_millis(100)).await;
    }
}

/// Says hello, starting the service if it is not running: through D-Bus activation where it is
/// installed for that, else from beside this program.
async fn start(proxy: &zbus::Proxy<'static>) -> Result<Hello> {
    match hello(proxy).await {
        Ok(h) => return Ok(h),
        Err(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
                || name.as_str() == "org.freedesktop.DBus.Error.NameHasNoOwner" => {}
        Err(e) => return Err(e).context("the service did not answer"),
    }
    let exe = std::env::current_exe().context("cannot find this program")?.with_file_name("lithidion-service");
    std::process::Command::new(&exe).spawn().with_context(|| format!("cannot start {}", exe.display()))?;
    for _ in 0..50 {
        glib::timeout_future(Duration::from_millis(100)).await;
        if let Ok(h) = hello(proxy).await {
            return Ok(h);
        }
    }
    bail!("the service did not start")
}

/// Connects to the service, replacing it if it is from another build.
pub async fn connect() -> Result<Connected> {
    let conn = zbus::Connection::session().await.context("no session bus")?;
    let proxy: zbus::Proxy<'static> = zbus::Proxy::new(&conn, BUS_NAME, PATH, INTERFACE).await?;
    // Subscribed before saying hello, so nothing between the snapshot and the stream is lost.
    let mut signals = proxy.receive_signal("Event").await?;
    let mut owners = proxy.receive_owner_changed().await?;
    let (mut build, mut seq, mut snapshot) = start(&proxy).await?;
    if build != BUILD {
        tracing::info!(service = build, window = BUILD, "replacing a service from another build");
        let _: zbus::Result<()> = proxy.call("Quit", &()).await;
        until_gone(&conn, Duration::from_secs(5)).await;
        (build, seq, snapshot) = start(&proxy).await?;
        if build != BUILD {
            return Err(anyhow!("the service is from another build ({build})"));
        }
    }
    let snapshot = decode_snapshot(&snapshot);

    let (evt_tx, evt_rx) = async_channel::unbounded();
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<Command>();
    {
        let proxy = proxy.clone();
        glib::spawn_future_local(async move {
            loop {
                match future::select(signals.next(), owners.next()).await {
                    future::Either::Left((Some(msg), _)) => {
                        let Ok((n, payload)) = msg.body().deserialize::<(u64, Vec<u8>)>() else { continue };
                        if n <= seq {
                            continue;
                        }
                        seq = n;
                        match service::decode::<Event>(&payload) {
                            Ok(e) => {
                                if evt_tx.send(e).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => tracing::warn!(error = %e, "an event from the service could not be read"),
                        }
                    }
                    // A new service (after a crash, or a quit from elsewhere): start over from
                    // its snapshot.
                    future::Either::Right((Some(Some(_)), _)) => match hello(&proxy).await {
                        Ok((_, n, events)) => {
                            seq = n;
                            for e in decode_snapshot(&events) {
                                let _ = evt_tx.send(e).await;
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "the new service did not answer"),
                    },
                    future::Either::Right((Some(None), _)) => {
                        seq = 0;
                        if QUITTING.with(|q| q.get()) {
                            continue;
                        }
                        tracing::warn!("the service stopped; starting it again");
                        glib::timeout_future(Duration::from_secs(1)).await;
                        if let Err(e) = start(&proxy).await {
                            let _ = evt_tx.send(Event::Error(format!("{e:#}"))).await;
                        }
                    }
                    _ => return,
                }
            }
        });
    }
    {
        let proxy = proxy.clone();
        glib::spawn_future_local(async move {
            while let Ok(c) = cmd_rx.recv().await {
                let r: zbus::Result<()> = proxy.call("Command", &(service::encode(&c),)).await;
                if let Err(e) = r {
                    tracing::warn!(error = %e, "command not delivered");
                }
            }
        });
    }
    PROXY.with(|p| *p.borrow_mut() = Some(proxy));
    Ok(Connected { snapshot, commands: cmd_tx, events: evt_rx })
}

/// Stops the service, waiting for it to take the request.
pub async fn quit_service() {
    QUITTING.with(|q| q.set(true));
    let proxy = PROXY.with(|p| p.borrow().clone());
    if let Some(proxy) = proxy {
        let r: zbus::Result<()> = proxy.call("Quit", &()).await;
        if let Err(e) = r {
            tracing::warn!(error = %e, "the service did not take the request to quit");
        }
    }
}
