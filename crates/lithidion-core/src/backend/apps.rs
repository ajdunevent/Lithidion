//! Apps on a watch (PROTOCOL.md section 10): adding bundles here, keeping the watch's app
//! database in step with Lithidion's list, and sending an app when the watch asks for it.

use super::firmware::{putbytes_install, putbytes_request};
use super::pbw::{self, Pbw};
use super::session::Protocol;
use crate::appstore::{self, StoreApp};
use crate::store::{App, AppKind, OnWatch, Store, WatchId};
use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use pebble_protocol::apps::{self as wire, FetchRequest, FetchResult};
use pebble_protocol::blobdb::{self, Database, Status};
use pebble_protocol::crc::stm32_crc;
use pebble_protocol::endpoint;
use pebble_protocol::hardware::AppPlatform;
use pebble_protocol::putbytes::{self, ObjectType};
use pebble_protocol::system::AppRunState;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

pub type Db = Arc<Mutex<Store>>;

pub fn lock(db: &Db) -> std::sync::MutexGuard<'_, Store> {
    db.lock().unwrap_or_else(|p| p.into_inner())
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Moves a checked bundle into place and records the app; returns it.
fn adopt(db: &Db, temp: &Path, store: Option<&StoreApp>) -> Result<App> {
    let bundle = Pbw::open(temp)?;
    if let Some(s) = store {
        if s.uuid != bundle.info.uuid {
            bail!("the store's bundle is for another app");
        }
    }
    let info = &bundle.info;
    let app = App {
        uuid: info.uuid,
        kind: if info.watchapp.watchface { AppKind::Watchface } else { AppKind::Watchapp },
        title: store.map(|s| s.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| info.title().to_string()),
        developer: store.map(|s| s.author.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| info.company_name.clone()),
        version: store.map(|s| s.version.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| info.version_label.clone()),
        hidden: info.watchapp.hidden_app,
        store: store.map(|s| (s.source.key().to_string(), s.id.clone())),
        image: store.and_then(|s| s.thumbnail().map(str::to_string)),
        added: now(),
        script: bundle.has_script(),
        configurable: info.can("configurable"),
    };
    std::fs::rename(temp, pbw::path(app.uuid)).context("the app could not be saved")?;
    let store = lock(db);
    store.save_app(&app)?;
    store.resend_app(app.uuid)?;
    Ok(app)
}

fn temp_path() -> Result<PathBuf> {
    let dir = pbw::dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir.join(format!(".{}.part", Uuid::new_v4())))
}

/// Downloads a store app's bundle; `progress` gets the fraction done.
pub async fn download(db: Db, app: StoreApp, platform: AppPlatform, mut progress: impl FnMut(f64)) -> Result<App> {
    let app = match &app.pbw {
        Some(_) => app,
        // Search results have no bundle link.
        None => appstore::app(app.source, app.id.clone(), platform).await?,
    };
    let url = app.pbw.clone().ok_or_else(|| anyhow!("the store has no download for {}", app.title))?;
    let response = crate::net::client()
        .get(&url)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .context("the download did not start")?
        .error_for_status()
        .context("the download failed")?;
    let total = response.content_length();
    let temp = temp_path()?;
    let mut file = tokio::fs::File::create(&temp).await?;
    let mut stream = response.bytes_stream();
    let mut done = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("the download was interrupted")?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        if let Some(t) = total {
            progress(done as f64 / t.max(1) as f64);
        }
    }
    file.flush().await?;
    drop(file);
    let result = tokio::task::spawn_blocking(move || {
        let r = adopt(&db, &temp, Some(&app));
        if r.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        r
    })
    .await?;
    result
}

/// Copies a bundle from a file.
pub async fn from_file(db: Db, path: PathBuf) -> Result<App> {
    tokio::task::spawn_blocking(move || {
        let temp = temp_path()?;
        std::fs::copy(&path, &temp).with_context(|| format!("cannot read {}", path.display()))?;
        let r = adopt(&db, &temp, None);
        if r.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        r
    })
    .await?
}

/// Deletes an app's bundle once no watch has it.
pub fn forget_if_unused(db: &Db, uuid: Uuid) {
    let forgotten = lock(db).forget_app_if_unused(uuid).unwrap_or(false);
    if forgotten {
        let _ = std::fs::remove_file(pbw::path(uuid));
    }
}

static TOKEN: AtomicU16 = AtomicU16::new(1);

/// One BlobDB command; a watch still starting up answers "try later", so that is retried.
pub(super) async fn blob(protocol: &Protocol, payload: impl Fn(u16) -> Vec<u8>) -> Result<Status> {
    let mut tries = 0;
    loop {
        let token = TOKEN.fetch_add(1, Ordering::Relaxed).max(1);
        let reply = protocol
            .request(
                endpoint::BLOBDB_V1,
                &payload(token),
                endpoint::BLOBDB_V1,
                |p| blobdb::parse_response(p).is_ok_and(|(t, _)| t == token),
                Duration::from_secs(15),
            )
            .await?;
        let status = blobdb::parse_response(&reply).map_err(|e| anyhow!("{e}"))?.1;
        tries += 1;
        if matches!(status, Status::TryLater | Status::Locked) && tries < 10 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        return Ok(status);
    }
}

/// What a sync changed, for the UI.
#[derive(Debug, Default)]
pub struct Synced {
    pub changed: bool,
    /// Apps the watch could not take: UUID, title, and why.
    pub failed: Vec<(Uuid, String, String)>,
}

/// Brings the watch's app database in line with Lithidion's list. A watch last filled by
/// another phone (`takeover`) has its databases cleared first, as the official app does.
pub async fn sync(protocol: &Protocol, db: &Db, watch: WatchId, platform: AppPlatform, takeover: bool) -> Result<Synced> {
    let mut out = Synced::default();
    if takeover {
        tracing::info!("clearing the watch's databases");
        for d in Database::CLEARED_ON_TAKEOVER {
            match blob(protocol, |t| blobdb::clear(t, d)).await {
                Ok(Status::Success) => {}
                Ok(s) => tracing::debug!(database = ?d, status = ?s, "not cleared"),
                Err(e) => return Err(e.context("clearing the watch's databases failed")),
            }
        }
        let store = lock(db);
        store.reset_watch_apps(watch)?;
        store.forget_sent_items(watch)?;
        store.set_databases_cleared(watch, true)?;
        out.changed = true;
    }
    let list = lock(db).watch_apps(watch)?;
    let mut order_changed = takeover;
    for item in &list {
        let uuid = item.app.uuid;
        match item.state {
            OnWatch::Inserted => {}
            OnWatch::ToDelete => {
                match blob(protocol, |t| blobdb::delete(t, Database::Apps, uuid.as_bytes())).await? {
                    Status::Success | Status::KeyDoesNotExist => {}
                    s => {
                        out.failed.push((uuid, item.app.title.clone(), format!("{s:?}")));
                        continue;
                    }
                }
                lock(db).drop_from_watch(watch, uuid)?;
                forget_if_unused(db, uuid);
                out.changed = true;
            }
            OnWatch::ToInsert => {
                let path = pbw::path(uuid);
                let meta = tokio::task::spawn_blocking(move || Pbw::open(&path)?.metadata(platform)).await?;
                let meta = match meta {
                    Ok(m) => m,
                    Err(e) => {
                        out.failed.push((uuid, item.app.title.clone(), format!("{e:#}")));
                        continue;
                    }
                };
                match blob(protocol, |t| blobdb::insert(t, Database::Apps, uuid.as_bytes(), &meta.encode())).await? {
                    Status::Success => {
                        lock(db).set_on_watch(watch, uuid, OnWatch::Inserted)?;
                        out.changed = true;
                        order_changed = true;
                    }
                    Status::DatabaseFull => out.failed.push((uuid, item.app.title.clone(), "the watch is full".to_string())),
                    s => out.failed.push((uuid, item.app.title.clone(), format!("the watch refused it ({s:?})"))),
                }
            }
        }
    }
    if order_changed {
        send_order(protocol, db, watch).await;
    }
    Ok(out)
}

/// Sends Lithidion's order: watchapps, then faces.
pub async fn send_order(protocol: &Protocol, db: &Db, watch: WatchId) {
    let list = lock(db).watch_apps(watch).unwrap_or_default();
    let on_watch = list.iter().filter(|a| a.state == OnWatch::Inserted);
    let apps = on_watch.clone().filter(|a| a.app.kind == AppKind::Watchapp).map(|a| a.app.uuid);
    let faces = on_watch.filter(|a| a.app.kind == AppKind::Watchface).map(|a| a.app.uuid);
    let order: Vec<Uuid> = apps.chain(faces).collect();
    if order.is_empty() {
        return;
    }
    let reply =
        protocol.request(endpoint::APP_REORDER, &wire::reorder(&order), endpoint::APP_REORDER, |_| true, Duration::from_secs(10)).await;
    match reply {
        Ok(p) if p.first() == Some(&1) => {}
        Ok(p) => tracing::info!(status = ?p.first(), "the watch did not take the app order"),
        Err(e) => tracing::info!(error = %e, "no answer to the app order"),
    }
}

pub fn launch(protocol: &Protocol, uuid: Uuid) -> Result<()> {
    protocol.send(endpoint::APP_RUN_STATE, &AppRunState::Started(uuid).encode())
}

async fn send_part(protocol: &Protocol, app_id: u32, object: ObjectType, bytes: &[u8], sent: &mut impl FnMut(usize)) -> Result<()> {
    let init = putbytes::app_init(bytes.len() as u32, object, app_id);
    let cookie = putbytes_request(protocol, &init, None, Duration::from_secs(20)).await?;
    for chunk in bytes.chunks(putbytes::CHUNK) {
        putbytes_request(protocol, &putbytes::put(cookie, chunk), Some(cookie), Duration::from_secs(20)).await?;
        sent(chunk.len());
    }
    putbytes_request(protocol, &putbytes::commit(cookie, stm32_crc(bytes)), Some(cookie), Duration::from_secs(20)).await?;
    putbytes_install(protocol, cookie).await
}

/// Answers the watch's request for an app: the binary, its resources, and its worker.
pub async fn send_app(
    protocol: &Protocol,
    db: &Db,
    watch: WatchId,
    platform: AppPlatform,
    request: FetchRequest,
    mut progress: impl FnMut(f64),
) -> Result<()> {
    let known = lock(db).watch_apps(watch)?.into_iter().any(|a| a.app.uuid == request.uuid && a.state != OnWatch::ToDelete);
    if !known {
        protocol.send(endpoint::APP_FETCH, &wire::fetch_response(FetchResult::InvalidUuid))?;
        bail!("the watch asked for an app it should not have ({})", request.uuid);
    }
    let path = pbw::path(request.uuid);
    let build = tokio::task::spawn_blocking(move || Pbw::open(&path)?.read_build(platform)).await?;
    let build = match build {
        Ok(b) => b,
        Err(e) => {
            protocol.send(endpoint::APP_FETCH, &wire::fetch_response(FetchResult::NoData))?;
            return Err(e);
        }
    };
    protocol.send(endpoint::APP_FETCH, &wire::fetch_response(FetchResult::Starting))?;
    let total = build.binary.len() + build.resources.as_ref().map_or(0, Vec::len) + build.worker.as_ref().map_or(0, Vec::len);
    let mut done = 0usize;
    let mut sent = |n: usize| {
        done += n;
        progress(done as f64 / total.max(1) as f64);
    };
    send_part(protocol, request.app_id, ObjectType::AppExecutable, &build.binary, &mut sent).await.context("sending the app failed")?;
    if let Some(r) = &build.resources {
        send_part(protocol, request.app_id, ObjectType::AppResources, r, &mut sent).await.context("sending the app's resources failed")?;
    }
    if let Some(w) = &build.worker {
        send_part(protocol, request.app_id, ObjectType::Worker, w, &mut sent).await.context("sending the app's worker failed")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::App;
    use pebble_protocol::frame::{Frame, Reassembler};
    use std::io::Write;

    const UUID: &str = "6e16dae9-de6a-4541-b383-df4333562cd8";

    fn binary() -> Vec<u8> {
        let mut b = vec![0u8; 620];
        b[..8].copy_from_slice(b"PBLAPP\0\0");
        b[8..14].copy_from_slice(&[16, 1, 5, 110, 2, 3]);
        b[96..100].copy_from_slice(&1353u32.to_le_bytes());
        b[104..120].copy_from_slice(UUID.parse::<Uuid>().unwrap().as_bytes());
        b
    }

    fn write_pbw(path: &Path, resources: &[u8]) {
        let bin = binary();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        let info = serde_json::json!({ "uuid": UUID, "shortName": "NBA", "versionLabel": "2.3.0", "companyName": "jc", "watchapp": { "watchface": true } });
        zip.start_file("appinfo.json", opts).unwrap();
        zip.write_all(info.to_string().as_bytes()).unwrap();
        let manifest = serde_json::json!({
            "application": { "name": "pebble-app.bin", "size": bin.len(), "crc": stm32_crc(&bin) },
            "resources": { "name": "app_resources.pbpack", "size": resources.len(), "crc": stm32_crc(resources) },
        });
        zip.start_file("basalt/manifest.json", opts).unwrap();
        zip.write_all(manifest.to_string().as_bytes()).unwrap();
        zip.start_file("basalt/pebble-app.bin", opts).unwrap();
        zip.write_all(&bin).unwrap();
        zip.start_file("basalt/app_resources.pbpack", opts).unwrap();
        zip.write_all(resources).unwrap();
        zip.finish().unwrap();
    }

    /// A watch that takes BlobDB commands, app order, and an app fetch's PutBytes; it logs them.
    fn fake_watch(
        mut out: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
        tx: tokio::sync::broadcast::Sender<Frame>,
    ) -> tokio::task::JoinHandle<Vec<String>> {
        tokio::spawn(async move {
            let mut r = Reassembler::new();
            let mut log = Vec::new();
            let mut received: Vec<u8> = Vec::new();
            let reply = |endpoint, payload: Vec<u8>| {
                let _ = tx.send(Frame { endpoint, payload });
            };
            while let Some(bytes) = out.recv().await {
                r.push(&bytes);
                while let Some(f) = r.next_frame() {
                    let p = f.payload.clone();
                    match f.endpoint {
                        endpoint::BLOBDB_V1 => {
                            let what = match p[0] {
                                0x01 => format!("insert {} {}", p[3], p.len() - 4 - 1 - 16 - 2),
                                0x04 => format!("delete {}", p[3]),
                                0x05 => format!("clear {}", p[3]),
                                _ => "?".into(),
                            };
                            log.push(what);
                            reply(endpoint::BLOBDB_V1, vec![p[1], p[2], 1]);
                        }
                        endpoint::APP_REORDER => {
                            log.push(format!("order {}", p[1]));
                            reply(endpoint::APP_REORDER, vec![1]);
                        }
                        endpoint::APP_FETCH => log.push(format!("fetch {}", p[1])),
                        endpoint::PUT_BYTES => {
                            match p[0] {
                                0x01 => {
                                    log.push(format!("init {:#04x} id {}", p[5], u32::from_be_bytes(p[6..10].try_into().unwrap())));
                                    received.clear();
                                }
                                0x02 => received.extend_from_slice(&p[9..]),
                                0x03 => {
                                    assert_eq!(u32::from_be_bytes(p[5..9].try_into().unwrap()), stm32_crc(&received));
                                    log.push(format!("commit {}", received.len()));
                                }
                                0x05 => log.push("install".into()),
                                _ => {}
                            }
                            reply(endpoint::PUT_BYTES, vec![1, 0, 0, 0, 9]);
                        }
                        e => panic!("unexpected endpoint {e}"),
                    }
                    if log.last().is_some_and(|l| l == "done") {
                        return log;
                    }
                }
            }
            log
        })
    }

    #[tokio::test]
    async fn sync_and_fetch() {
        let dir = std::env::temp_dir().join(format!("lithidion-test-{}", Uuid::new_v4()));
        std::env::set_var("XDG_DATA_HOME", &dir);
        std::fs::create_dir_all(pbw::dir()).unwrap();
        let uuid: Uuid = UUID.parse().unwrap();
        let resources = vec![5u8; 2500];
        write_pbw(&pbw::path(uuid), &resources);

        let db: Db = Arc::new(Mutex::new(Store::in_memory()));
        let watch = {
            let s = lock(&db);
            let w = s.add_watch("24:A3:0A:D4:7B:24", 0).unwrap();
            s.save_app(&App {
                uuid,
                kind: AppKind::Watchface,
                title: "NBA".into(),
                developer: "jc".into(),
                version: "2.3.0".into(),
                hidden: false,
                store: None,
                image: None,
                added: 0,
                script: false,
                configurable: false,
            })
            .unwrap();
            s.add_to_watch(w, uuid).unwrap();
            w
        };
        let (protocol, out, tx) = Protocol::loopback();
        let fake = fake_watch(out, tx.clone());

        let synced = sync(&protocol, &db, watch, AppPlatform::Emery, true).await.unwrap();
        assert!(synced.changed && synced.failed.is_empty(), "{:?}", synced.failed);
        assert!(lock(&db).databases_cleared(watch).unwrap());
        assert_eq!(lock(&db).watch_apps(watch).unwrap()[0].state, OnWatch::Inserted);

        let request = FetchRequest { uuid, app_id: 7 };
        send_app(&protocol, &db, watch, AppPlatform::Emery, request, |_| {}).await.unwrap();

        lock(&db).set_on_watch(watch, uuid, OnWatch::ToDelete).unwrap();
        sync(&protocol, &db, watch, AppPlatform::Emery, false).await.unwrap();
        assert!(lock(&db).watch_apps(watch).unwrap().is_empty());
        assert!(!pbw::path(uuid).exists(), "the unused bundle is deleted");

        drop(protocol);
        drop(tx);
        let log = fake.await.unwrap();
        let mut expected: Vec<String> = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11].iter().map(|d| format!("clear {d}")).collect();
        expected.extend(
            [
                "insert 2 126",
                "order 1",
                "fetch 1",
                "init 0x85 id 7",
                "commit 620",
                "install",
                "init 0x84 id 7",
                "commit 2500",
                "install",
                "delete 2",
            ]
            .map(String::from),
        );
        assert_eq!(log, expected);
        let _ = std::fs::remove_dir_all(dir);
    }
}
