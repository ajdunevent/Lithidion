//! What the phone itself says over D-Bus (PROTOCOL.md section 12): its notifications, its quiet
//! state (feedbackd's profile and GNOME's Do Not Disturb), and calls through ModemManager.

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use pebble_protocol::imaging::Rgba;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use zbus::message::Type;
use zbus::zvariant::OwnedValue;
use zbus::{Connection, MatchRule, MessageStream};

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";

#[derive(Debug, Clone, PartialEq)]
pub struct Posted {
    /// The id the notification server gave it.
    pub id: u32,
    /// The id it replaces, or 0.
    pub replaces: u32,
    /// The app's desktop entry when it gives one, else its name.
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
    pub category: Option<String>,
    /// The app's icon as the notification names it: an icon name, a path, or a `file://` URI.
    pub icon: Option<String>,
    /// The picture the notification carries in its hints, scaled down to what a watch can show.
    pub picture: Option<Arc<Rgba>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Ringing,
    Active,
    Ended,
    /// Dialling, held and the like: nothing for the watch.
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallUpdate {
    /// The call's ModemManager object path.
    pub path: String,
    pub number: String,
    pub incoming: bool,
    pub state: CallState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PhoneEvent {
    Posted(Posted),
    Closed(u32),
    /// The phone is silent or in Do Not Disturb.
    Quiet(bool),
    Call(CallUpdate),
    /// The device's time zone changed.
    TimeZone(String),
}

/// Follows the desktop's clock format (12- or 24-hour) through the settings portal.
pub async fn clock_format() -> Result<()> {
    use ashpd::desktop::settings::Settings;
    let settings = Settings::new().await?;
    let mut changes = settings.receive_setting_changed_with_args::<String>(crate::host::CLOCK_NAMESPACE, crate::host::CLOCK_KEY).await?;
    if let Ok(v) = settings.read::<String>(crate::host::CLOCK_NAMESPACE, crate::host::CLOCK_KEY).await {
        crate::host::set_clock_format(&v);
    }
    while let Some(v) = changes.next().await {
        match v {
            Ok(v) => {
                tracing::info!(clock = %v, "clock format changed");
                crate::host::set_clock_format(&v);
                super::watch_prefs::wake();
            }
            Err(e) => tracing::debug!(error = %e, "clock format not read"),
        }
    }
    Ok(())
}

/// Follows timedated's `Timezone`: sends the zone at the start and whenever it changes.
pub async fn time_zone(tx: UnboundedSender<PhoneEvent>) -> Result<()> {
    const NAME: &str = "org.freedesktop.timedate1";
    let bus = Connection::system().await.context("no system bus")?;
    let props = zbus::fdo::PropertiesProxy::builder(&bus).destination(NAME)?.path("/org/freedesktop/timedate1")?.build().await?;
    let mut changes = props.receive_properties_changed().await?;
    let read = || async {
        let v = props.get(zbus::names::InterfaceName::from_static_str_unchecked(NAME), "Timezone").await?;
        Ok::<_, anyhow::Error>(String::try_from(v)?)
    };
    tx.send(PhoneEvent::TimeZone(read().await?)).map_err(|_| anyhow!("stopped"))?;
    while let Some(signal) = changes.next().await {
        if signal.args().is_ok_and(|a| a.interface_name.as_str() == NAME) {
            if let Ok(zone) = read().await {
                tx.send(PhoneEvent::TimeZone(zone)).map_err(|_| anyhow!("stopped"))?;
            }
        }
    }
    Ok(())
}

/// Keeps `f` running, starting it again a while after it fails.
pub async fn keep<F, Fut>(what: &'static str, f: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    loop {
        match f().await {
            Ok(()) => tracing::info!(what, "stopped"),
            Err(e) => tracing::warn!(what, error = format!("{e:#}"), "not available"),
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// Removes markup the notification spec allows in bodies (`<b>`, `<a href…>`…) and decodes the
/// few entities it uses.
pub fn plain(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_tag = false;
    for c in body.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&").trim().to_string()
}

type NotifyArgs = (String, u32, String, String, String, Vec<String>, HashMap<String, OwnedValue>, i32);

fn posted(args: NotifyArgs, id: u32) -> Posted {
    let (app_name, replaces, icon, summary, body, _actions, hints, _timeout) = args;
    let hint_str = |k: &str| hints.get(k).and_then(|v| String::try_from(v.clone()).ok()).filter(|s| !s.is_empty());
    let urgency = hints.get("urgency").and_then(|v| u8::try_from(v.clone()).ok()).unwrap_or(1);
    let app_id = hint_str("desktop-entry").unwrap_or_else(|| app_name.clone());
    Posted {
        id,
        replaces,
        app_name: if app_name.is_empty() { app_id.clone() } else { app_name },
        app_id,
        title: summary.trim().to_string(),
        body: plain(&body),
        urgency,
        category: hint_str("category"),
        icon: Some(icon).filter(|i| !i.is_empty()),
        picture: ["image-data", "image_data", "icon_data"].iter().find_map(|k| hints.get(*k)).and_then(picture).map(Arc::new),
    }
}

/// The longest side kept of a notification's picture: wider than any watch shows it.
const PICTURE_MAX: u32 = 260;

/// A picture from the `image-data` hint: width, height, row stride, alpha, bits per sample,
/// channels, and the pixels.
fn picture(v: &OwnedValue) -> Option<Rgba> {
    let (w, h, stride, _alpha, bits, channels, data) = <(i32, i32, i32, bool, i32, i32, Vec<u8>)>::try_from(v.try_clone().ok()?).ok()?;
    let (w, h, stride, channels) = (u32::try_from(w).ok()?, u32::try_from(h).ok()?, stride as usize, channels as usize);
    if bits != 8 || !(channels == 3 || channels == 4) || w == 0 || h == 0 || w > 8192 || h > 8192 {
        return None;
    }
    if stride < w as usize * channels || data.len() < stride * (h as usize - 1) + w as usize * channels {
        return None;
    }
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        for p in data[y * stride..].chunks_exact(channels).take(w as usize) {
            pixels.extend_from_slice(&[p[0], p[1], p[2], if channels == 4 { p[3] } else { 255 }]);
        }
    }
    Some(Rgba { width: w, height: h, pixels }.bounded(PICTURE_MAX))
}

/// Watches every notification on the session bus: the bus is asked to copy `Notify` calls,
/// their replies (which carry the id), and `NotificationClosed` to a monitoring connection.
pub async fn notifications(tx: UnboundedSender<PhoneEvent>) -> Result<()> {
    let bus = Connection::session().await.context("no session bus")?;
    let dbus = zbus::fdo::DBusProxy::new(&bus).await?;
    let mut owner_changes = dbus.receive_name_owner_changed_with_args(&[(0, NOTIFICATIONS)]).await?;
    loop {
        let owner = match dbus.get_name_owner(NOTIFICATIONS.try_into()?).await {
            Ok(o) => o.to_string(),
            Err(_) => {
                tracing::info!("no notification server yet");
                owner_changes.next().await;
                continue;
            }
        };
        tracing::info!(%owner, "watching notifications");
        let monitor = zbus::connection::Builder::session()?.build().await?;
        let rules = [
            MatchRule::builder().msg_type(Type::MethodCall).interface(NOTIFICATIONS)?.member("Notify")?.build(),
            MatchRule::builder().msg_type(Type::MethodReturn).sender(owner.as_str())?.build(),
            MatchRule::builder().msg_type(Type::Signal).interface(NOTIFICATIONS)?.member("NotificationClosed")?.build(),
        ];
        zbus::fdo::MonitoringProxy::new(&monitor)
            .await?
            .become_monitor(&rules, 0)
            .await
            .context("the bus refused to let Lithidion watch notifications")?;
        let mut stream = MessageStream::from(&monitor);
        // Notify calls waiting for their reply, by caller and serial.
        let mut waiting: HashMap<(String, u32), NotifyArgs> = HashMap::new();
        loop {
            let msg = tokio::select! {
                m = stream.next() => match m {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return Err(e.into()),
                    None => return Err(anyhow!("the monitor connection closed")),
                },
                _ = owner_changes.next() => break,
            };
            let header = msg.header();
            match msg.message_type() {
                Type::MethodCall => {
                    let (Some(sender), serial) = (header.sender(), header.primary().serial_num().get()) else { continue };
                    match msg.body().deserialize::<NotifyArgs>() {
                        Ok(args) => {
                            waiting.insert((sender.to_string(), serial), args);
                            if waiting.len() > 64 {
                                waiting.clear();
                            }
                        }
                        Err(e) => tracing::debug!(error = %e, "Notify call not read"),
                    }
                }
                Type::MethodReturn => {
                    let (Some(dest), Some(reply)) = (header.destination(), header.reply_serial()) else { continue };
                    if let Some(args) = waiting.remove(&(dest.to_string(), reply.get())) {
                        if let Ok(id) = msg.body().deserialize::<u32>() {
                            let _ = tx.send(PhoneEvent::Posted(posted(args, id)));
                        }
                    }
                }
                Type::Signal => {
                    if let Ok((id, _reason)) = msg.body().deserialize::<(u32, u32)>() {
                        let _ = tx.send(PhoneEvent::Closed(id));
                    }
                }
                Type::Error => {}
            }
        }
        tracing::info!("the notification server changed");
    }
}

/// Closes a notification on the phone, as dismissing it there would.
pub async fn close_notification(id: u32) -> Result<()> {
    let bus = Connection::session().await?;
    bus.call_method(Some(NOTIFICATIONS), "/org/freedesktop/Notifications", Some(NOTIFICATIONS), "CloseNotification", &(id,)).await?;
    Ok(())
}

/// Reports whether the phone is quiet: feedbackd's profile is "silent", or GNOME's notification
/// banners are off (Phosh's Do Not Disturb). Either source may be missing.
pub async fn quiet(tx: UnboundedSender<PhoneEvent>) -> Result<()> {
    use ashpd::desktop::settings::Settings;
    let bus = Connection::session().await?;
    let feedback = zbus::Proxy::new(&bus, "org.sigxcpu.Feedback", "/org/sigxcpu/Feedback", "org.sigxcpu.Feedback").await?;
    let mut profile_changes = feedback.receive_property_changed::<String>("Profile").await;
    let mut silent = feedback.get_property::<String>("Profile").await.map(|p| p == "silent").unwrap_or(false);

    const NS: &str = "org.gnome.desktop.notifications";
    let settings = Settings::new().await.ok();
    let mut dnd = match &settings {
        Some(s) => s.read::<bool>(NS, "show-banners").await.map(|b| !b).unwrap_or_else(|e| {
            tracing::info!(error = %e, "GNOME's Do Not Disturb cannot be read");
            false
        }),
        None => false,
    };
    let banner_changes = match &settings {
        Some(s) => s.receive_setting_changed_with_args::<bool>(NS, "show-banners").await.ok(),
        None => None,
    };
    let mut banner_changes = match banner_changes {
        Some(s) => s.boxed(),
        None => futures::stream::pending().boxed(),
    };
    tracing::info!(silent, dnd, "phone quiet state");
    let mut last = silent || dnd;
    let _ = tx.send(PhoneEvent::Quiet(last));
    loop {
        tokio::select! {
            Some(change) = profile_changes.next() => {
                silent = change.get().await.map(|p| p == "silent").unwrap_or(false);
            }
            Some(show) = banner_changes.next() => {
                dnd = !show.unwrap_or(true);
            }
            else => return Ok(()),
        }
        if (silent || dnd) != last {
            last = silent || dnd;
            tracing::info!(silent, dnd, "phone quiet state");
            let _ = tx.send(PhoneEvent::Quiet(last));
        }
    }
}

const MM: &str = "org.freedesktop.ModemManager1";
const MM_CALL: &str = "org.freedesktop.ModemManager1.Call";

fn call_state(mm: i32) -> CallState {
    match mm {
        3 | 6 => CallState::Ringing, // ringing-in, waiting
        4 => CallState::Active,
        7 => CallState::Ended,
        _ => CallState::Other,
    }
}

/// Follows calls through ModemManager's voice interface, which FuriOS provides through
/// ofono2mm whatever the phone app.
pub async fn calls(tx: UnboundedSender<PhoneEvent>) -> Result<()> {
    let bus = Connection::system().await.context("no system bus")?;
    let added_rule =
        MatchRule::builder().msg_type(Type::Signal).interface("org.freedesktop.ModemManager1.Modem.Voice")?.member("CallAdded")?.build();
    let state_rule = MatchRule::builder().msg_type(Type::Signal).interface(MM_CALL)?.member("StateChanged")?.build();
    let mut added = MessageStream::for_match_rule(added_rule, &bus, None).await?;
    let mut changed = MessageStream::for_match_rule(state_rule, &bus, None).await?;
    let mut known: HashMap<String, (String, bool)> = HashMap::new();
    tracing::info!("watching calls");
    loop {
        tokio::select! {
            Some(Ok(msg)) = added.next() => {
                let Ok(path) = msg.body().deserialize::<zbus::zvariant::OwnedObjectPath>() else { continue };
                let call = zbus::Proxy::new(&bus, MM, path.clone(), MM_CALL).await?;
                let number = call.get_property::<String>("Number").await.unwrap_or_default();
                let incoming = call.get_property::<i32>("Direction").await.map(|d| d == 1).unwrap_or(false);
                let state = call.get_property::<i32>("State").await.unwrap_or(0);
                known.insert(path.to_string(), (number.clone(), incoming));
                let _ = tx.send(PhoneEvent::Call(CallUpdate { path: path.to_string(), number, incoming, state: call_state(state) }));
            }
            Some(Ok(msg)) = changed.next() => {
                let Some(path) = msg.header().path().map(|p| p.to_string()) else { continue };
                let Ok((_old, new, _reason)) = msg.body().deserialize::<(i32, i32, u32)>() else { continue };
                let (number, incoming) = match known.get(&path) {
                    Some(k) => k.clone(),
                    None => {
                        let call = zbus::Proxy::new(&bus, MM, path.clone(), MM_CALL).await?;
                        let n = call.get_property::<String>("Number").await.unwrap_or_default();
                        let i = call.get_property::<i32>("Direction").await.map(|d| d == 1).unwrap_or(false);
                        known.insert(path.clone(), (n.clone(), i));
                        (n, i)
                    }
                };
                let state = call_state(new);
                if state == CallState::Ended {
                    known.remove(&path);
                }
                let _ = tx.send(PhoneEvent::Call(CallUpdate { path, number, incoming, state }));
            }
            else => return Ok(()),
        }
    }
}

/// Answers or hangs up a call.
pub async fn call_action(path: String, answer: bool) -> Result<()> {
    let bus = Connection::system().await?;
    let method = if answer { "Accept" } else { "Hangup" };
    bus.call_method(Some(MM), path.as_str(), Some(MM_CALL), method, &()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup() {
        assert_eq!(plain("<b>Hi</b> &amp; <a href=\"x\">there</a> 1 &lt; 2"), "Hi & there 1 < 2");
        assert_eq!(plain("  plain  "), "plain");
    }

    #[test]
    fn notify_arguments() {
        let mut hints = HashMap::new();
        hints.insert("desktop-entry".to_string(), OwnedValue::try_from(zbus::zvariant::Value::from("sm.puri.Chatty")).unwrap());
        hints.insert("urgency".to_string(), OwnedValue::from(2u8));
        hints.insert(
            "image-data".to_string(),
            OwnedValue::try_from(zbus::zvariant::Value::from((3i32, 2i32, 12i32, false, 8i32, 3i32, vec![200u8; 24]))).unwrap(),
        );
        let p = posted(("Chats".into(), 0, String::new(), " Al ".into(), "<i>hey</i>".into(), vec![], hints, -1), 9);
        let pic = p.picture.as_deref().expect("picture read");
        assert_eq!((pic.width, pic.height, pic.pixels.len()), (3, 2, 24));
        assert_eq!(&pic.pixels[..4], &[200, 200, 200, 255]);
        assert_eq!(
            (p.id, p.app_id.as_str(), p.app_name.as_str(), p.title.as_str(), p.body.as_str(), p.urgency),
            (9, "sm.puri.Chatty", "Chats", "Al", "hey", 2)
        );
    }

    /// A stand-in feedbackd on a private session bus: `dbus-run-session -- cargo test -- --ignored
    /// feedbackd`. Without a settings portal there, GNOME's state counts as not quiet.
    #[tokio::test]
    #[ignore]
    async fn feedbackd_silent_profile() {
        struct Feedback {
            profile: String,
        }
        #[zbus::interface(name = "org.sigxcpu.Feedback")]
        impl Feedback {
            #[zbus(property)]
            fn profile(&self) -> String {
                self.profile.clone()
            }
        }
        let server = zbus::connection::Builder::session()
            .unwrap()
            .name("org.sigxcpu.Feedback")
            .unwrap()
            .serve_at("/org/sigxcpu/Feedback", Feedback { profile: "full".into() })
            .unwrap()
            .build()
            .await
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(quiet(tx));
        let first = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
        assert_eq!(first, PhoneEvent::Quiet(false));
        let iface = server.object_server().interface::<_, Feedback>("/org/sigxcpu/Feedback").await.unwrap();
        iface.get_mut().await.profile = "silent".into();
        iface.get().await.profile_changed(iface.signal_emitter()).await.unwrap();
        let second = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert_eq!(second, PhoneEvent::Quiet(true));
    }

    /// A stand-in timedated, with the session bus standing in for the system bus:
    /// `dbus-run-session -- sh -c 'DBUS_SYSTEM_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS cargo test -- --ignored timedated'`.
    #[tokio::test]
    #[ignore]
    async fn timedated_zone_changes() {
        struct TimeDate {
            zone: String,
        }
        #[zbus::interface(name = "org.freedesktop.timedate1")]
        impl TimeDate {
            #[zbus(property)]
            fn timezone(&self) -> String {
                self.zone.clone()
            }
        }
        let server = zbus::connection::Builder::system()
            .unwrap()
            .name("org.freedesktop.timedate1")
            .unwrap()
            .serve_at("/org/freedesktop/timedate1", TimeDate { zone: "America/Chicago".into() })
            .unwrap()
            .build()
            .await
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(time_zone(tx));
        let first = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
        assert_eq!(first, PhoneEvent::TimeZone("America/Chicago".into()));
        let iface = server.object_server().interface::<_, TimeDate>("/org/freedesktop/timedate1").await.unwrap();
        iface.get_mut().await.zone = "Asia/Tokyo".into();
        iface.get().await.timezone_changed(iface.signal_emitter()).await.unwrap();
        let second = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert_eq!(second, PhoneEvent::TimeZone("Asia/Tokyo".into()));
    }

    /// Needs a session bus of its own: `dbus-run-session -- cargo test -- --ignored monitor`.
    #[tokio::test]
    #[ignore]
    async fn monitor_sees_notifications() {
        // A stand-in notification server.
        struct Server;
        #[zbus::interface(name = "org.freedesktop.Notifications")]
        impl Server {
            #[allow(clippy::too_many_arguments)]
            fn notify(
                &self,
                _a: String,
                _r: u32,
                _i: String,
                _s: String,
                _b: String,
                _ac: Vec<String>,
                _h: HashMap<String, OwnedValue>,
                _t: i32,
            ) -> u32 {
                42
            }
        }
        let server = zbus::connection::Builder::session()
            .unwrap()
            .name(NOTIFICATIONS)
            .unwrap()
            .serve_at("/org/freedesktop/Notifications", Server)
            .unwrap()
            .build()
            .await
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(notifications(tx));
        tokio::time::sleep(Duration::from_millis(500)).await;
        let client = Connection::session().await.unwrap();
        let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
        let reply = client
            .call_method(
                Some(NOTIFICATIONS),
                "/org/freedesktop/Notifications",
                Some(NOTIFICATIONS),
                "Notify",
                &("Chats", 0u32, "", "Al", "hey", Vec::<&str>::new(), hints, -1i32),
            )
            .await
            .unwrap();
        assert_eq!(reply.body().deserialize::<u32>().unwrap(), 42);
        server
            .emit_signal(None::<()>, "/org/freedesktop/Notifications", NOTIFICATIONS, "NotificationClosed", &(42u32, 2u32))
            .await
            .unwrap();
        let first = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
        let PhoneEvent::Posted(p) = first else { panic!("{first:?}") };
        assert_eq!((p.id, p.title.as_str()), (42, "Al"));
        let second = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
        assert_eq!(second, PhoneEvent::Closed(42));
    }
}
