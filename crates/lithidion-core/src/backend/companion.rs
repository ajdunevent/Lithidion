//! Apps' phone-side scripts (PebbleKit JS) during a connection: the one for the app in the
//! watch's foreground runs, and app messages pass between it and the watch.

use super::apps::{lock, Db};
use super::pbw::Pbw;
use crate::pkjs::{In, Out, Runner, Script};
use crate::store::ProfileId;
use pebble_protocol::appmessage::{self, Message};
use pebble_protocol::endpoint;
use pebble_protocol::system::WatchVersion;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

/// What scripts are told about the watch.
pub fn watch_info(version: &WatchVersion, model: Option<i32>, platform: Option<&str>) -> serde_json::Value {
    let tag = &version.running.tag;
    let v = version.running.version();
    let suffix = tag.split_once('-').map(|(_, s)| s).unwrap_or("");
    serde_json::json!({
        "platform": platform.unwrap_or("unknown"),
        "model": model.map(pebble_protocol::hardware::js_model).unwrap_or("unknown_unknown"),
        "language": version.language.clone().unwrap_or_else(|| "en_US".to_string()),
        "firmware": {
            "major": v.map(|v| v.major).unwrap_or(0),
            "minor": v.map(|v| v.minor).unwrap_or(0),
            "patch": v.map(|v| v.patch).unwrap_or(0),
            "suffix": suffix,
        },
    })
}

pub struct Companion {
    pub runner: Runner,
    pub out: UnboundedReceiver<Out>,
    pub network: Arc<AtomicBool>,
    /// Set when the settings page was asked for; the script's next page opens then.
    pub settings_asked: Option<tokio::time::Instant>,
}

/// Starts `app`'s script, if it has one.
pub fn start(db: &Db, app: Uuid, profile: ProfileId, watch: &serde_json::Value, serial: &str) -> Option<Companion> {
    let pbw = Pbw::open(&super::pbw::path(app)).ok()?;
    let code = match pbw.script() {
        Ok(Some(code)) => code,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(%app, error = format!("{e:#}"), "the app's script cannot be read");
            return None;
        }
    };
    let (allowed, seed) = {
        let s = lock(db);
        (crate::settings::network_allowed(&s, profile, app), crate::settings::account_seed(&s, profile).unwrap_or_default())
    };
    let network = Arc::new(AtomicBool::new(allowed));
    let script = Script {
        uuid: app,
        title: pbw.info.title().to_string(),
        code,
        keys: pbw.info.app_keys.clone(),
        location: pbw.info.can("location"),
        watch: watch.clone(),
        account_token: crate::pkjs::token(&seed, app),
        watch_token: crate::pkjs::token(serial, app),
        profile,
    };
    let (tx, out) = tokio::sync::mpsc::unbounded_channel();
    let runner = Runner::start(script, db.clone(), network.clone(), tokio::runtime::Handle::current(), tx);
    Some(Companion { runner, out, network, settings_asked: None })
}

impl Companion {
    pub fn set_network(&self, allowed: bool) {
        self.network.store(allowed, Ordering::Relaxed);
    }

    /// Sends a message from the script and tells it how the watch answered.
    pub fn send(&self, protocol: &super::session::Protocol, transaction: u8, tuples: Vec<appmessage::Tuple>) {
        let (protocol, tx, app) = (protocol.clone(), self.runner.sender(), self.runner.uuid);
        tokio::spawn(async move {
            let answer = protocol
                .request(
                    endpoint::APP_MESSAGE,
                    &appmessage::push(transaction, app, &tuples),
                    endpoint::APP_MESSAGE,
                    |p| matches!(appmessage::parse(p), Ok(Message::Ack(t) | Message::Nack(t)) if t == transaction),
                    Duration::from_secs(10),
                )
                .await;
            let ok = matches!(answer.as_deref().map(appmessage::parse), Ok(Ok(Message::Ack(_))));
            if !ok {
                tracing::info!(%app, transaction, "the watch did not take the script's message");
            }
            let _ = tx.send(In::Answer { transaction, ok }).await;
        });
    }
}
