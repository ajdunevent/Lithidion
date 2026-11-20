//! PebbleKit JS: the phone-side scripts of watch apps, run in JavaScriptCore.
//!
//! One script runs at a time: the one belonging to the app in the watch's foreground, as the
//! Pebble app does. Each runs on a thread of its own, with a GLib main loop for its timers;
//! `bridge.js` gives it the browser and `Pebble` APIs it expects. Network requests and
//! WebSockets go through Lithidion and only while the app is allowed network access.

mod engine;
mod socket;

use crate::store::{ProfileId, Store};
use base64::Engine as _;
use javascriptcore6 as jsc;
use pebble_protocol::appmessage::{Tuple, Value};
use serde::Deserialize;
use serde_json::{json, Map};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

pub type Db = Arc<Mutex<Store>>;

/// What a script needs to run.
pub struct Script {
    pub uuid: Uuid,
    pub title: String,
    pub code: String,
    /// Message key names and their numbers, from the app's `appinfo.json`.
    pub keys: HashMap<String, u32>,
    /// Whether the app asks for the location.
    pub location: bool,
    /// `Pebble.getActiveWatchInfo()`.
    pub watch: serde_json::Value,
    pub account_token: String,
    pub watch_token: String,
    pub profile: ProfileId,
}

/// Messages to a running script.
#[derive(Debug)]
pub enum In {
    /// From the watch app.
    Message(Vec<Tuple>),
    /// The watch's answer to a message the script sent.
    Answer {
        transaction: u8,
        ok: bool,
    },
    ShowSettings,
    /// What the settings page gave back; empty when it was closed without.
    SettingsClosed(String),
    Http(serde_json::Value),
    /// An event on one of the script's WebSockets.
    Socket(serde_json::Value),
    Location(serde_json::Value),
    Stop,
}

/// What a running script asks for.
#[derive(Debug, PartialEq)]
pub enum Out {
    Ready,
    Send {
        transaction: u8,
        tuples: Vec<Tuple>,
    },
    Notify {
        title: String,
        body: String,
    },
    /// A page to show, normally its settings.
    Open(String),
    /// A request was refused for want of network access; sent once per run.
    WantsNetwork,
}

pub struct Runner {
    pub uuid: Uuid,
    pub title: String,
    tx: async_channel::Sender<In>,
}

impl Runner {
    /// Starts `script` on its own thread.
    pub fn start(script: Script, db: Db, network: Arc<AtomicBool>, rt: tokio::runtime::Handle, out: UnboundedSender<Out>) -> Runner {
        let (tx, rx) = async_channel::unbounded();
        let (uuid, title) = (script.uuid, script.title.clone());
        let to_self = tx.clone();
        let spawned = std::thread::Builder::new().name("pkjs".into()).spawn(move || {
            let main = glib::MainContext::new();
            let _ = main.with_thread_default(|| run(script, db, network, rt, out, to_self, rx));
        });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the script could not start");
        }
        Runner { uuid, title, tx }
    }

    pub fn send(&self, m: In) {
        let _ = self.tx.send_blocking(m);
    }

    pub fn sender(&self) -> async_channel::Sender<In> {
        self.tx.clone()
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.tx.send_blocking(In::Stop);
    }
}

/// The script's side of things, on its own thread.
struct Host {
    script: Script,
    names: HashMap<u32, String>,
    db: Db,
    network: Arc<AtomicBool>,
    asked_for_network: Cell<bool>,
    rt: tokio::runtime::Handle,
    out: UnboundedSender<Out>,
    to_self: async_channel::Sender<In>,
    timers: RefCell<HashMap<u64, glib::JoinHandle<()>>>,
    /// Open WebSockets; dropping one's sender closes it.
    sockets: RefCell<HashMap<u64, tokio::sync::mpsc::UnboundedSender<socket::Command>>>,
    transaction: Cell<u8>,
    ctx: jsc::Context,
    deliver: RefCell<Option<jsc::Value>>,
}

fn run(
    script: Script,
    db: Db,
    network: Arc<AtomicBool>,
    rt: tokio::runtime::Handle,
    out: UnboundedSender<Out>,
    to_self: async_channel::Sender<In>,
    rx: async_channel::Receiver<In>,
) {
    let names = script.keys.iter().map(|(k, v)| (*v, k.clone())).collect();
    let host = Rc::new(Host {
        script,
        names,
        db,
        network,
        asked_for_network: Cell::new(false),
        rt,
        out,
        to_self,
        timers: RefCell::default(),
        sockets: RefCell::default(),
        transaction: Cell::new(0),
        ctx: jsc::Context::new(),
        deliver: RefCell::default(),
    });
    let weak = Rc::downgrade(&host);
    let native = engine::function(&host.ctx, "__native", move |args| {
        let host = weak.upgrade()?;
        let op = args.first().map(|a| a.to_str().to_string()).unwrap_or_default();
        let arg = args.get(1).map(|a| a.to_str().to_string()).unwrap_or_default();
        let reply = host.native(&op, &arg);
        Some(jsc::Value::new_string(&host.ctx, Some(&reply.map(|v| v.to_string()).unwrap_or_default())))
    });
    host.ctx.set_value("__native", &native);
    engine::run(&host.ctx, include_str!("bridge.js"), "lithidion:bridge.js");
    *host.deliver.borrow_mut() = host.ctx.value("__deliver");
    tracing::info!(app = %host.script.title, "script started");
    engine::run(&host.ctx, &host.script.code, "pebble-js-app.js");
    host.deliver("ready", None);
    let _ = host.out.send(Out::Ready);

    let main = glib::MainLoop::new(Some(&glib::MainContext::ref_thread_default()), false);
    let (h, m) = (Rc::downgrade(&host), main.clone());
    glib::MainContext::ref_thread_default().spawn_local(async move {
        while let Ok(message) = rx.recv().await {
            let Some(host) = h.upgrade() else { break };
            if !host.receive(message) {
                break;
            }
        }
        m.quit();
    });
    main.run();
    for (_, t) in host.timers.borrow_mut().drain() {
        t.abort();
    }
    host.sockets.borrow_mut().clear();
    *host.deliver.borrow_mut() = None;
    tracing::info!(app = %host.script.title, "script stopped");
}

#[derive(Deserialize)]
struct Timer {
    id: u64,
    ms: u64,
    repeat: bool,
}

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: u64,
    method: String,
    url: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    timeout: u64,
    #[serde(default)]
    binary: bool,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    base64: Option<String>,
}

#[derive(Deserialize)]
struct SocketRequest {
    id: u64,
    url: String,
    #[serde(default)]
    protocols: Vec<String>,
}

impl Host {
    fn deliver(&self, kind: &str, data: Option<serde_json::Value>) {
        let Some(f) = self.deliver.borrow().clone() else { return };
        let mut args = vec![jsc::Value::new_string(&self.ctx, Some(kind))];
        if let Some(d) = data {
            args.push(jsc::Value::new_string(&self.ctx, Some(&d.to_string())));
        }
        let _ = f.function_callv(&args);
        if let Some(e) = self.ctx.exception() {
            tracing::warn!(app = %self.script.title, kind, message = %e.message().unwrap_or_default(), "script error");
            self.ctx.clear_exception();
        }
    }

    /// Handles a message; false to stop.
    fn receive(self: &Rc<Self>, m: In) -> bool {
        match m {
            In::Message(tuples) => self.deliver("appmessage", Some(self.payload(&tuples))),
            In::Answer { transaction, ok } => self.deliver(if ok { "ack" } else { "nack" }, Some(json!(transaction))),
            In::ShowSettings => self.deliver("showConfiguration", None),
            In::SettingsClosed(response) => self.deliver("webviewclosed", Some(json!(response))),
            In::Http(reply) => self.deliver("http", Some(reply)),
            In::Socket(event) => {
                if event["kind"] == "close" {
                    if let Some(id) = event["id"].as_u64() {
                        self.sockets.borrow_mut().remove(&id);
                    }
                }
                self.deliver("socket", Some(event))
            }
            In::Location(reply) => self.deliver("location", Some(reply)),
            In::Stop => return false,
        }
        true
    }

    /// A message from the watch as the script sees it: under each key's name, and its number.
    fn payload(&self, tuples: &[Tuple]) -> serde_json::Value {
        let mut out = Map::new();
        for (key, value) in tuples {
            let v = match value {
                Value::Text(t) => json!(t),
                Value::Unsigned(n) => json!(n),
                Value::Signed(n) => json!(n),
                Value::Bytes(b) => json!(b),
            };
            if let Some(name) = self.names.get(key) {
                out.insert(name.clone(), v.clone());
            }
            out.insert(key.to_string(), v);
        }
        serde_json::Value::Object(out)
    }

    fn native(self: &Rc<Self>, op: &str, arg: &str) -> Option<serde_json::Value> {
        let parsed = || serde_json::from_str::<serde_json::Value>(arg).unwrap_or(serde_json::Value::Null);
        let text = || parsed().as_str().map(str::to_string).unwrap_or_default();
        let storage = |f: &dyn Fn(&Store) -> anyhow::Result<Option<serde_json::Value>>| -> Option<serde_json::Value> {
            let store = self.db.lock().unwrap_or_else(|e| e.into_inner());
            f(&store).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "script storage failed");
                None
            })
        };
        let (profile, app) = (self.script.profile, self.script.uuid);
        match op {
            "log" => {
                let v = parsed();
                let (level, line) = (v["level"].as_str().unwrap_or("log"), v["text"].as_str().unwrap_or(""));
                match level {
                    "error" => tracing::warn!(app = %self.script.title, "{line}"),
                    "debug" => tracing::debug!(app = %self.script.title, "{line}"),
                    _ => tracing::info!(app = %self.script.title, "{line}"),
                }
                None
            }
            "send" => {
                let tuples = to_tuples(&parsed(), &self.script.keys);
                let t = self.transaction.get();
                self.transaction.set(t.wrapping_add(1));
                if self.out.send(Out::Send { transaction: t, tuples }).is_err() {
                    return Some(json!(-1));
                }
                Some(json!(t))
            }
            "notify" => {
                let v = parsed();
                let _ = self.out.send(Out::Notify {
                    title: v["title"].as_str().unwrap_or_default().to_string(),
                    body: v["body"].as_str().unwrap_or_default().to_string(),
                });
                None
            }
            "open" => {
                let _ = self.out.send(Out::Open(text()));
                None
            }
            "token" => Some(json!(if text() == "watch" { &self.script.watch_token } else { &self.script.account_token })),
            "watch" => Some(self.script.watch.clone()),
            "get" => storage(&|s| Ok(s.script_value(profile, app, &text())?.map(|v| json!(v)))),
            "set" => {
                let v = parsed();
                storage(&|s| {
                    s.set_script_value(profile, app, v["key"].as_str().unwrap_or_default(), v["value"].as_str().unwrap_or_default())?;
                    Ok(None)
                })
            }
            "remove" => storage(&|s| {
                s.remove_script_value(profile, app, &text())?;
                Ok(None)
            }),
            "clear" => storage(&|s| {
                s.clear_script_values(profile, app)?;
                Ok(None)
            }),
            "keys" => storage(&|s| Ok(Some(json!(s.script_keys(profile, app)?)))).or(Some(json!([]))),
            "timer" => {
                let Ok(t) = serde_json::from_str::<Timer>(arg) else { return None };
                self.add_timer(t);
                None
            }
            "cancel" => {
                if let Some(t) = parsed().as_u64().and_then(|id| self.timers.borrow_mut().remove(&id)) {
                    t.abort();
                }
                None
            }
            "http" => {
                let Ok(r) = serde_json::from_str::<Request>(arg) else {
                    return Some(json!({ "error": "the request could not be read" }));
                };
                self.http(r)
            }
            "socket" => {
                let Ok(s) = serde_json::from_str::<SocketRequest>(arg) else {
                    return Some(json!({ "error": "the socket could not be read" }));
                };
                self.open_socket(s)
            }
            "socketSend" => {
                let v = parsed();
                let command = match (v["text"].as_str(), v["base64"].as_str()) {
                    (_, Some(b)) => socket::Command::Binary(base64::engine::general_purpose::STANDARD.decode(b).unwrap_or_default()),
                    (t, None) => socket::Command::Text(t.unwrap_or_default().to_string()),
                };
                if let Some(tx) = v["id"].as_u64().and_then(|id| self.sockets.borrow().get(&id).cloned()) {
                    let _ = tx.send(command);
                }
                None
            }
            "socketClose" => {
                let v = parsed();
                let code = v["code"].as_u64().map(|c| c as u16);
                let reason = v["reason"].as_str().unwrap_or_default().to_string();
                if let Some(tx) = v["id"].as_u64().and_then(|id| self.sockets.borrow().get(&id).cloned()) {
                    let _ = tx.send(socket::Command::Close(code, reason));
                }
                None
            }
            "location" => {
                let v = parsed();
                self.locate(v["id"].as_u64().unwrap_or(0), v["timeout"].as_u64().unwrap_or(0));
                None
            }
            other => {
                tracing::debug!(op = other, "unknown script call");
                None
            }
        }
    }

    /// Timers run on the script thread's own main context.
    fn add_timer(self: &Rc<Self>, t: Timer) {
        let weak: Weak<Host> = Rc::downgrade(self);
        let handle = glib::MainContext::ref_thread_default().spawn_local(async move {
            loop {
                glib::timeout_future(Duration::from_millis(t.ms)).await;
                let Some(host) = weak.upgrade() else { return };
                if !t.repeat {
                    host.timers.borrow_mut().remove(&t.id);
                }
                host.deliver("timer", Some(json!(t.id)));
                if !t.repeat || !host.timers.borrow().contains_key(&t.id) {
                    return;
                }
            }
        });
        if let Some(old) = self.timers.borrow_mut().insert(t.id, handle) {
            old.abort();
        }
    }

    /// Answers at once for a synchronous request, else later through `deliver('http')`.
    fn http(self: &Rc<Self>, r: Request) -> Option<serde_json::Value> {
        let sync = r.id == 0;
        let refused = if !self.network.load(Ordering::Relaxed) {
            if !self.asked_for_network.replace(true) {
                let _ = self.out.send(Out::WantsNetwork);
            }
            Some("network access is off for this app")
        } else if !(r.url.starts_with("http://") || r.url.starts_with("https://")) {
            Some("only http and https are supported")
        } else {
            None
        };
        if let Some(why) = refused {
            tracing::info!(app = %self.script.title, url = %r.url, why, "request refused");
            let reply = json!({ "id": r.id, "error": why });
            if sync {
                return Some(reply);
            }
            let _ = self.to_self.send_blocking(In::Http(reply));
            return None;
        }
        if sync {
            return Some(self.rt.block_on(fetch(r)));
        }
        let tx = self.to_self.clone();
        self.rt.spawn(async move {
            let reply = fetch(r).await;
            let _ = tx.send(In::Http(reply)).await;
        });
        None
    }

    /// Starts a WebSocket and gives its URL as parsed. A URL the constructor should throw on
    /// gives an error at once; any later failure arrives as the socket's error and close events.
    fn open_socket(self: &Rc<Self>, s: SocketRequest) -> Option<serde_json::Value> {
        let url = match socket::parse(&s.url) {
            Ok(u) => u.to_string(),
            Err(why) => return Some(json!({ "error": why })),
        };
        if !self.network.load(Ordering::Relaxed) {
            if !self.asked_for_network.replace(true) {
                let _ = self.out.send(Out::WantsNetwork);
            }
            let why = "network access is off for this app";
            tracing::info!(app = %self.script.title, why, "socket refused");
            let _ = self.to_self.send_blocking(In::Socket(json!({ "id": s.id, "kind": "error", "message": why })));
            let _ = self.to_self.send_blocking(In::Socket(json!({ "id": s.id, "kind": "close", "code": 1006, "reason": "", "clean": false })));
            return Some(json!({ "url": url }));
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.sockets.borrow_mut().insert(s.id, tx);
        self.rt.spawn(socket::session(s.id, url.clone(), s.protocols, rx, self.to_self.clone(), self.script.title.clone()));
        Some(json!({ "url": url }))
    }

    fn locate(&self, id: u64, timeout_ms: u64) {
        let tx = self.to_self.clone();
        if !self.script.location {
            let _ = tx.send_blocking(In::Location(
                json!({ "id": id, "error": { "code": 1, "message": "the app does not ask for the location" } }),
            ));
            return;
        }
        let limit = Duration::from_millis(if timeout_ms == 0 { 60_000 } else { timeout_ms });
        self.rt.spawn(async move {
            let reply = match tokio::time::timeout(limit, crate::backend::weather::locate(true)).await {
                Ok(Ok(p)) => json!({
                    "id": id,
                    "coords": { "latitude": p.latitude, "longitude": p.longitude, "accuracy": p.accuracy,
                                "altitude": null, "altitudeAccuracy": null, "heading": null, "speed": null },
                    "timestamp": chrono::Utc::now().timestamp_millis(),
                }),
                Ok(Err(e)) => json!({ "id": id, "error": { "code": 2, "message": format!("{e:#}") } }),
                Err(_) => json!({ "id": id, "error": { "code": 3, "message": "the location did not arrive in time" } }),
            };
            let _ = tx.send(In::Location(reply)).await;
        });
    }
}

/// Converts what the script gave `sendAppMessage`: keys by name or number; text, numbers,
/// booleans, and byte arrays. Anything else is left out.
pub fn to_tuples(data: &serde_json::Value, keys: &HashMap<String, u32>) -> Vec<Tuple> {
    let Some(map) = data.as_object() else { return Vec::new() };
    let mut out = Vec::new();
    for (name, v) in map {
        let Some(key) = keys.get(name).copied().or_else(|| name.parse().ok()) else {
            tracing::info!(key = %name, "a message key the app does not define");
            continue;
        };
        let value = match v {
            serde_json::Value::String(s) => Value::Text(s.clone()),
            serde_json::Value::Bool(b) => Value::Signed(*b as i32),
            serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
                (Some(i), _) if i > i32::MAX as i64 && i <= u32::MAX as i64 => Value::Unsigned(i as u32),
                (Some(i), _) => Value::Signed(i as i32),
                (None, Some(f)) => Value::Signed(f as i32),
                _ => continue,
            },
            serde_json::Value::Array(items) => Value::Bytes(items.iter().map(|x| x.as_i64().unwrap_or(0) as u8).collect()),
            _ => continue,
        };
        out.push((key, value));
    }
    out.sort_by_key(|(k, _)| *k);
    out
}

/// Bodies larger than this are cut.
const MAX_BODY: usize = 8 << 20;

async fn fetch(r: Request) -> serde_json::Value {
    let id = r.id;
    let result = async {
        let method = reqwest::Method::from_bytes(r.method.as_bytes())?;
        let mut req = crate::net::client().request(method, &r.url);
        for (k, v) in &r.headers {
            req = req.header(k, v);
        }
        if r.timeout > 0 {
            req = req.timeout(Duration::from_millis(r.timeout));
        }
        if let Some(b) = &r.base64 {
            req = req.body(base64::engine::general_purpose::STANDARD.decode(b)?);
        } else if let Some(t) = r.text {
            req = req.body(t);
        }
        let reply = req.send().await?;
        let status = reply.status();
        let url = reply.url().to_string();
        let headers: Vec<(String, String)> =
            reply.headers().iter().map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
        let mut body = reply.bytes().await?.to_vec();
        body.truncate(MAX_BODY);
        anyhow::Ok(json!({
            "id": id,
            "status": status.as_u16(),
            "statusText": status.canonical_reason().unwrap_or(""),
            "url": url,
            "headers": headers,
            "text": String::from_utf8_lossy(&body),
            "base64": if r.binary { Some(base64::engine::general_purpose::STANDARD.encode(&body)) } else { None },
        }))
    }
    .await;
    result.unwrap_or_else(|e| {
        let timed_out = e.downcast_ref::<reqwest::Error>().is_some_and(|e| e.is_timeout());
        json!({ "id": id, "error": format!("{e:#}"), "timedOut": timed_out })
    })
}

/// The Pebble app's token salt: tokens match it, so a server that keeps settings by watch token
/// finds them.
const TOKEN_SALT: &str = "MMIxeUT[G9/U#(7V67O^EuADSw,{$C;B}`>|-nlrQCs|t|k=P_!*LETm,RKc,BG*'";

/// An app's token from `seed`: the watch's serial for the watch token, the profile's own seed for
/// the account token. Apps not from a store use their UUID, as the Pebble app does.
pub fn token(seed: &str, app: Uuid) -> String {
    use md5::Digest;
    let digest = md5::Md5::digest(format!("{seed}{}{TOKEN_SALT}", app.to_string().to_uppercase()));
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuples_from_script_values() {
        let keys = HashMap::from([("Temp".to_string(), 10), ("City".to_string(), 11)]);
        let v = json!({ "Temp": 21.7, "City": "Bentonville", "5": true, "Big": 1, "6": [1, 2, 300], "7": 3_000_000_000u64, "8": null });
        assert_eq!(
            to_tuples(&v, &keys),
            vec![
                (5, Value::Signed(1)),
                (6, Value::Bytes(vec![1, 2, 44])),
                (7, Value::Unsigned(3_000_000_000)),
                (10, Value::Signed(21)),
                (11, Value::Text("Bentonville".into())),
            ]
        );
    }

    #[test]
    fn token_is_stable() {
        let app = Uuid::parse_str("133215f0-cf20-4c05-997b-3c9be5a64e5b").unwrap();
        let t = token("Q123456789AB", app);
        assert_eq!(t.len(), 32);
        assert_eq!(t, token("Q123456789AB", app));
        assert_ne!(t, token("Q123456789AC", app));
    }

    fn store() -> Db {
        Arc::new(Mutex::new(Store::in_memory()))
    }

    /// Runs a script against a stand-in watch: what it sends, and what it gets back.
    #[test]
    fn script_round_trip() {
        let _ = tracing_subscriber::fmt().with_env_filter("lithidion=debug").with_test_writer().try_init();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (out_tx, mut out) = tokio::sync::mpsc::unbounded_channel();
        let code = r#"
            Pebble.addEventListener('ready', function () {
                localStorage.setItem('n', '1');
                localStorage.last = 'x';
                setTimeout(function () {
                    Pebble.sendAppMessage({ City: 'Here', 3: localStorage.getItem('n') + localStorage.last },
                        function (e) { Pebble.sendAppMessage({ 4: e.data.transactionId }); });
                }, 10);
            });
            Pebble.addEventListener('appmessage', function (e) {
                var r = new XMLHttpRequest();
                r.onerror = function () { Pebble.sendAppMessage({ 5: e.payload.Temp + e.payload['10'] }); };
                r.open('GET', 'https://example.com/');
                r.send();
            });
            Pebble.addEventListener('showConfiguration', function () { Pebble.openURL('data:text/html,<p>x</p>'); });
            Pebble.addEventListener('webviewclosed', function (e) { Pebble.showSimpleNotificationOnPebble('Saved', e.response); });
        "#;
        let script = Script {
            uuid: Uuid::nil(),
            title: "Test".into(),
            code: code.into(),
            keys: HashMap::from([("Temp".to_string(), 10), ("City".to_string(), 11)]),
            location: false,
            watch: json!({ "platform": "emery" }),
            account_token: String::new(),
            watch_token: String::new(),
            profile: 1,
        };
        let runner = Runner::start(script, store(), Arc::new(AtomicBool::new(false)), rt.handle().clone(), out_tx);
        let next = |out: &mut tokio::sync::mpsc::UnboundedReceiver<Out>| {
            rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), out.recv()).await.unwrap().unwrap() })
        };
        assert_eq!(next(&mut out), Out::Ready);
        assert_eq!(
            next(&mut out),
            Out::Send { transaction: 0, tuples: vec![(3, Value::Text("1x".into())), (11, Value::Text("Here".into()))] }
        );
        runner.send(In::Answer { transaction: 0, ok: true });
        assert_eq!(next(&mut out), Out::Send { transaction: 1, tuples: vec![(4, Value::Signed(0))] });
        // Without network access the request fails, and the app is said to want it.
        runner.send(In::Message(vec![(10, Value::Signed(4))]));
        assert_eq!(next(&mut out), Out::WantsNetwork);
        assert_eq!(next(&mut out), Out::Send { transaction: 2, tuples: vec![(5, Value::Signed(8))] });
        runner.send(In::ShowSettings);
        assert_eq!(next(&mut out), Out::Open("data:text/html,<p>x</p>".into()));
        runner.send(In::SettingsClosed("{\"a\":1}".into()));
        assert_eq!(next(&mut out), Out::Notify { title: "Saved".into(), body: "{\"a\":1}".into() });
        drop(runner);
    }

    #[test]
    fn timers_and_refusals() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (out_tx, mut out) = tokio::sync::mpsc::unbounded_channel();
        let code = r#"
            var n = 0;
            var gone = setTimeout(function () { n += 100; }, 5);
            clearTimeout(gone);
            var i = setInterval(function () {
                if (++n === 3) {
                    clearInterval(i);
                    var r = new XMLHttpRequest();
                    r.open('GET', 'ftp://example.com/', false);
                    r.send();
                    navigator.geolocation.getCurrentPosition(function () {}, function (e) {
                        Pebble.sendAppMessage({ 1: n, 2: r.status, 3: e.code, 4: btoa(atob('aGk=')), 5: typeof fetch });
                    });
                }
            }, 5);
        "#;
        let script = Script {
            uuid: Uuid::nil(),
            title: "Timers".into(),
            code: code.into(),
            keys: HashMap::new(),
            location: false,
            watch: json!({}),
            account_token: String::new(),
            watch_token: String::new(),
            profile: 1,
        };
        let _runner = Runner::start(script, store(), Arc::new(AtomicBool::new(true)), rt.handle().clone(), out_tx);
        let mut next = || rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), out.recv()).await.unwrap().unwrap() });
        assert_eq!(next(), Out::Ready);
        assert_eq!(
            next(),
            Out::Send {
                transaction: 0,
                tuples: vec![
                    (1, Value::Signed(3)),
                    (2, Value::Signed(0)),
                    (3, Value::Signed(1)),
                    (4, Value::Text("aGk=".into())),
                    (5, Value::Text("function".into())),
                ]
            }
        );
    }

    /// A socket to a local echo server: open, a text and a binary echo, and a clean close; then
    /// `ws://` to a public address, which is refused, and a scheme the constructor throws on.
    #[test]
    fn sockets() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let port = listener.local_addr().unwrap().port();
        rt.spawn(async move {
            use futures::{SinkExt, StreamExt};
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            while let Some(Ok(m)) = ws.next().await {
                if m.is_text() || m.is_binary() {
                    ws.send(m).await.unwrap();
                }
            }
        });
        let (out_tx, mut out) = tokio::sync::mpsc::unbounded_channel();
        let code = format!(
            r#"
            var seen = [];
            var s = new WebSocket('ws://127.0.0.1:{port}/');
            s.binaryType = 'arraybuffer';
            s.onopen = function () {{ s.send('hello'); s.send(new Uint8Array([7, 8])); }};
            s.onmessage = function (e) {{
                seen.push(typeof e.data === 'string' ? e.data : new Uint8Array(e.data).join('+'));
                if (seen.length === 2) s.close(1000, 'done');
            }};
            s.addEventListener('close', function (e) {{
                Pebble.sendAppMessage({{ 1: seen.join(','), 2: e.code, 3: e.wasClean, 4: s.readyState }});
                var far = new WebSocket('ws://8.8.8.8/');
                var failed = false;
                far.onerror = function () {{ failed = true; }};
                far.onclose = function (e) {{
                    var threw = '';
                    try {{ new WebSocket('ftp://example.com/'); }} catch (x) {{ threw = x.name; }}
                    Pebble.sendAppMessage({{ 5: failed, 6: e.code, 7: threw }});
                }};
            }});
        "#
        );
        let script = Script {
            uuid: Uuid::nil(),
            title: "Sockets".into(),
            code,
            keys: HashMap::new(),
            location: false,
            watch: json!({}),
            account_token: String::new(),
            watch_token: String::new(),
            profile: 1,
        };
        let _runner = Runner::start(script, store(), Arc::new(AtomicBool::new(true)), rt.handle().clone(), out_tx);
        let mut next = || rt.block_on(async { tokio::time::timeout(Duration::from_secs(10), out.recv()).await.unwrap().unwrap() });
        assert_eq!(next(), Out::Ready);
        assert_eq!(
            next(),
            Out::Send {
                transaction: 0,
                tuples: vec![
                    (1, Value::Text("hello,7+8".into())),
                    (2, Value::Signed(1000)),
                    (3, Value::Signed(1)),
                    (4, Value::Signed(3)),
                ]
            }
        );
        assert_eq!(
            next(),
            Out::Send {
                transaction: 1,
                tuples: vec![(5, Value::Signed(1)), (6, Value::Signed(1006)), (7, Value::Text("SyntaxError".into()))]
            }
        );
    }

    /// Runs a real app's script: `PKJS_PBW=app.pbw cargo test -- --ignored --nocapture real_script`.
    #[test]
    #[ignore]
    fn real_script() {
        let _ = tracing_subscriber::fmt().with_env_filter("lithidion=debug").with_test_writer().try_init();
        let path = std::path::PathBuf::from(std::env::var("PKJS_PBW").expect("PKJS_PBW"));
        let pbw = crate::backend::pbw_for_tests(&path);
        let rt = tokio::runtime::Runtime::new().unwrap();
        crate::net::set_runtime(rt.handle().clone());
        let (out_tx, mut out) = tokio::sync::mpsc::unbounded_channel();
        let script = Script {
            uuid: pbw.0,
            title: pbw.1,
            code: pbw.2,
            keys: pbw.3,
            location: false,
            watch: json!({ "platform": "basalt", "model": "pebble_time_black", "language": "en_US",
                           "firmware": { "major": 4, "minor": 9, "patch": 150, "suffix": "" } }),
            account_token: "a".into(),
            watch_token: "w".into(),
            profile: 1,
        };
        let runner = Runner::start(script, store(), Arc::new(AtomicBool::new(true)), rt.handle().clone(), out_tx);
        let mut next = |secs| rt.block_on(async { tokio::time::timeout(Duration::from_secs(secs), out.recv()).await.ok().flatten() });
        let drain = |runner: &Runner, next: &mut dyn FnMut(u64) -> Option<Out>| {
            while let Some(o) = next(3) {
                match &o {
                    Out::Send { transaction, .. } => runner.send(In::Answer { transaction: *transaction, ok: true }),
                    Out::Open(url) => eprintln!("OPEN {}", &url[..url.len().min(160)]),
                    _ => {}
                }
                eprintln!("OUT {}", format!("{o:?}").chars().take(300).collect::<String>());
            }
        };
        drain(&runner, &mut next);
        runner.send(In::ShowSettings);
        drain(&runner, &mut next);
        runner.send(In::SettingsClosed(std::env::var("PKJS_RESPONSE").unwrap_or_default()));
        drain(&runner, &mut next);
    }
}
