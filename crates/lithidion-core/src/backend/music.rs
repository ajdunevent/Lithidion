//! Music (PROTOCOL.md section 15): the phone's players through MPRIS, and its volume through
//! PipeWire's PulseAudio interface.

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use zbus::zvariant::OwnedValue;
use zbus::{Connection, MatchRule, MessageStream};

const MPRIS: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NowPlaying {
    /// The bus name's last part ("mpv", "org.gnome.Lollypop").
    pub player: String,
    /// The player's own name ("Lollypop").
    pub identity: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub length_ms: Option<u32>,
    pub playing: bool,
    pub position_ms: u32,
}

#[derive(Debug, Default)]
struct Player {
    identity: String,
    status: String,
    metadata: HashMap<String, OwnedValue>,
    /// When it last started playing or appeared; the most recent present one is followed.
    last_active: u64,
}

fn text(v: Option<&OwnedValue>) -> String {
    let Some(v) = v else { return String::new() };
    if let Ok(s) = String::try_from(v.clone()) {
        return s;
    }
    // xesam:artist is a list.
    Vec::<String>::try_from(v.clone()).map(|l| l.join(", ")).unwrap_or_default()
}

fn now_playing(name: &str, p: &Player, position_us: Option<i64>) -> NowPlaying {
    let length = p
        .metadata
        .get("mpris:length")
        .and_then(|v| i64::try_from(v.clone()).ok().or_else(|| u64::try_from(v.clone()).ok().map(|u| u as i64)));
    NowPlaying {
        player: name.trim_start_matches(MPRIS).to_string(),
        identity: if p.identity.is_empty() { name.trim_start_matches(MPRIS).to_string() } else { p.identity.clone() },
        title: text(p.metadata.get("xesam:title")),
        artist: text(p.metadata.get("xesam:artist")),
        album: text(p.metadata.get("xesam:album")),
        length_ms: length.filter(|l| *l > 0).map(|l| (l / 1000).min(u32::MAX as i64) as u32),
        playing: p.status == "Playing",
        position_ms: position_us.map_or(0, |u| (u.max(0) / 1000).min(u32::MAX as i64) as u32),
    }
}

async fn read_player(bus: &Connection, name: &str) -> Result<Player> {
    let root = zbus::Proxy::new(bus, name.to_string(), PATH, "org.mpris.MediaPlayer2").await?;
    let player = zbus::Proxy::new(bus, name.to_string(), PATH, PLAYER).await?;
    Ok(Player {
        identity: root.get_property::<String>("Identity").await.unwrap_or_default(),
        status: player.get_property::<String>("PlaybackStatus").await.unwrap_or_default(),
        metadata: player.get_property::<HashMap<String, OwnedValue>>("Metadata").await.unwrap_or_default(),
        last_active: 0,
    })
}

async fn position(bus: &Connection, name: &str) -> Option<i64> {
    let player = zbus::Proxy::new(bus, name.to_string(), PATH, PLAYER).await.ok()?;
    player.get_property::<i64>("Position").await.ok()
}

/// Follows the players; sends what the most recently active one still open is playing.
pub async fn players(tx: UnboundedSender<Option<NowPlaying>>) -> Result<()> {
    let bus = Connection::session().await?;
    let dbus = zbus::fdo::DBusProxy::new(&bus).await?;
    let mut tick = 0u64;
    let mut players: HashMap<String, Player> = HashMap::new();
    // Unique name → well-known name, for signals.
    let mut owners: HashMap<String, String> = HashMap::new();
    for name in dbus.list_names().await? {
        let name = name.to_string();
        if name.starts_with(MPRIS) {
            if let Ok(owner) = dbus.get_name_owner(name.as_str().try_into()?).await {
                owners.insert(owner.to_string(), name.clone());
            }
            if let Ok(mut p) = read_player(&bus, &name).await {
                tick += 1;
                p.last_active = if p.status == "Playing" { tick + 1000 } else { tick };
                players.insert(name, p);
            }
        }
    }
    let changes_rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .path(PATH)?
        .build();
    let mut changes = MessageStream::for_match_rule(changes_rule, &bus, None).await?;
    let mut owner_changes = dbus.receive_name_owner_changed().await?;
    // Nothing sent yet, so the first state always goes out.
    let mut last_sent: Option<Option<NowPlaying>> = None;
    loop {
        // What to send now.
        let active = players.iter().max_by_key(|(_, p)| p.last_active).map(|(n, _)| n.clone());
        let now = match &active {
            Some(name) => {
                let pos = position(&bus, name).await;
                Some(now_playing(name, &players[name], pos))
            }
            None => None,
        };
        if last_sent.as_ref() != Some(&now) {
            last_sent = Some(now.clone());
            let _ = tx.send(now);
        }
        tokio::select! {
            Some(change) = owner_changes.next() => {
                let Ok(args) = change.args() else { continue };
                let name = args.name().to_string();
                if !name.starts_with(MPRIS) {
                    continue;
                }
                owners.retain(|_, n| *n != name);
                match args.new_owner().as_ref() {
                    Some(owner) => {
                        owners.insert(owner.to_string(), name.clone());
                        // Give the player a moment to publish its properties.
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        if let Ok(mut p) = read_player(&bus, &name).await {
                            tick += 1;
                            p.last_active = tick + 1000;
                            players.insert(name, p);
                        }
                    }
                    None => {
                        players.remove(&name);
                    }
                }
            }
            Some(Ok(msg)) = changes.next() => {
                let Some(name) = msg.header().sender().and_then(|s| owners.get(s.as_str())).cloned() else { continue };
                let Ok((iface, changed, _)) = msg.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>() else { continue };
                if iface != PLAYER {
                    continue;
                }
                let Some(p) = players.get_mut(&name) else { continue };
                if let Some(status) = changed.get("PlaybackStatus").and_then(|v| String::try_from(v.clone()).ok()) {
                    if status == "Playing" && p.status != "Playing" {
                        tick += 1;
                        p.last_active = tick + 1000;
                    }
                    p.status = status;
                }
                if let Some(m) = changed.get("Metadata").and_then(|v| HashMap::<String, OwnedValue>::try_from(v.clone()).ok()) {
                    p.metadata = m;
                }
            }
            // Positions move on their own; refresh now and then while playing.
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                last_sent = None;
            }
            else => return Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PlayPause,
    Pause,
    Play,
    Next,
    Previous,
}

/// Sends a button press to a player.
pub async fn act(player: &str, action: Action) -> Result<()> {
    let bus = Connection::session().await?;
    let method = match action {
        Action::PlayPause => "PlayPause",
        Action::Pause => "Pause",
        Action::Play => "Play",
        Action::Next => "Next",
        Action::Previous => "Previous",
    };
    bus.call_method(Some(format!("{MPRIS}{player}").as_str()), PATH, Some(PLAYER), method, &())
        .await
        .with_context(|| format!("{method} failed"))?;
    Ok(())
}

/// Changes the default output's volume by `step` (a fraction, -0.05 for 5 % down); returns the
/// new volume in percent.
pub fn change_volume(step: f32) -> Result<u8> {
    use pulseaudio::protocol::{self, Command};
    use std::ffi::CString;
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;

    let path = pulseaudio::socket_path_from_env().ok_or_else(|| anyhow!("no PulseAudio or PipeWire socket"))?;
    let mut sock = BufReader::new(UnixStream::connect(path).context("the sound server cannot be reached")?);
    sock.get_ref().set_read_timeout(Some(Duration::from_secs(5)))?;
    let cookie = pulseaudio::cookie_path_from_env().and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
    let auth = protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie };
    protocol::write_command_message(sock.get_mut(), 0, &Command::Auth(auth), protocol::MAX_VERSION)?;
    let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION)?;
    let version = protocol::MAX_VERSION.min(reply.version);
    let mut props = protocol::Props::new();
    props.set(protocol::Prop::ApplicationName, CString::new("Lithidion").expect("no NUL"));
    protocol::write_command_message(sock.get_mut(), 1, &Command::SetClientName(props), version)?;
    let _ = protocol::read_reply_message::<protocol::SetClientNameReply>(&mut sock, version)?;
    protocol::write_command_message(sock.get_mut(), 2, &Command::GetServerInfo, version)?;
    let (_, server) = protocol::read_reply_message::<protocol::ServerInfo>(&mut sock, version)?;
    let Some(sink) = server.default_sink_name else { bail!("there is no default output") };
    protocol::write_command_message(
        sock.get_mut(),
        3,
        &Command::GetSinkInfo(protocol::GetSinkInfo { index: None, name: Some(sink.clone()) }),
        version,
    )?;
    let (_, info) = protocol::read_reply_message::<protocol::SinkInfo>(&mut sock, version)?;
    let channels = info.cvolume.channels();
    let current = channels.iter().map(|v| v.to_linear()).fold(0.0f32, f32::max);
    let target = (current + step).clamp(0.0, 1.0);
    let mut volume = protocol::ChannelVolume::empty();
    for _ in 0..channels.len().max(1) {
        volume.push(protocol::Volume::from_linear(target));
    }
    let params = protocol::SetDeviceVolumeParams { device_index: None, device_name: Some(sink), volume };
    protocol::write_command_message(sock.get_mut(), 4, &Command::SetSinkVolume(params), version)?;
    protocol::read_ack_message(&mut sock)?;
    Ok((target * 100.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata() {
        let mut meta = HashMap::new();
        meta.insert("xesam:title".to_string(), OwnedValue::try_from(zbus::zvariant::Value::from("Song")).unwrap());
        meta.insert("xesam:artist".to_string(), OwnedValue::try_from(zbus::zvariant::Value::from(vec!["A", "B"])).unwrap());
        meta.insert("mpris:length".to_string(), OwnedValue::from(180_000_000i64));
        let p = Player { identity: "Lollypop".into(), status: "Playing".into(), metadata: meta, last_active: 1 };
        let n = now_playing("org.mpris.MediaPlayer2.Lollypop", &p, Some(5_000_000));
        assert_eq!(
            (n.title.as_str(), n.artist.as_str(), n.length_ms, n.playing, n.position_ms),
            ("Song", "A, B", Some(180_000), true, 5_000)
        );
        assert_eq!(n.player, "Lollypop");
    }

    /// A stand-in player on a private session bus: `dbus-run-session -- cargo test -- --ignored
    /// mpris`.
    #[tokio::test]
    #[ignore]
    async fn mpris_player_followed() {
        struct Root;
        #[zbus::interface(name = "org.mpris.MediaPlayer2")]
        impl Root {
            #[zbus(property)]
            fn identity(&self) -> String {
                "Fake".into()
            }
        }
        struct Fake {
            status: String,
        }
        #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
        impl Fake {
            #[zbus(property)]
            fn playback_status(&self) -> String {
                self.status.clone()
            }
            #[zbus(property)]
            fn metadata(&self) -> HashMap<String, OwnedValue> {
                let mut m = HashMap::new();
                m.insert("xesam:title".to_string(), OwnedValue::try_from(zbus::zvariant::Value::from("Track")).unwrap());
                m
            }
            #[zbus(property)]
            fn position(&self) -> i64 {
                0
            }
            fn play_pause(&mut self) {}
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(players(tx));
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert_eq!(first, None);
        let conn = zbus::connection::Builder::session()
            .unwrap()
            .name("org.mpris.MediaPlayer2.fake")
            .unwrap()
            .serve_at(PATH, Root)
            .unwrap()
            .serve_at(PATH, Fake { status: "Paused".into() })
            .unwrap()
            .build()
            .await
            .unwrap();
        let seen = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap().unwrap();
        assert_eq!((seen.player.as_str(), seen.identity.as_str(), seen.title.as_str(), seen.playing), ("fake", "Fake", "Track", false));
        let iface = conn.object_server().interface::<_, Fake>(PATH).await.unwrap();
        iface.get_mut().await.status = "Playing".into();
        iface.get().await.playback_status_changed(iface.signal_emitter()).await.unwrap();
        let playing = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap().unwrap();
        assert!(playing.playing);
        act("fake", Action::PlayPause).await.unwrap();
        conn.close().await.unwrap();
        let gone = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert_eq!(gone, None);
    }
}
