//! PebbleOS releases from GitHub, firmware bundles, and installing one over PutBytes
//! (PROTOCOL.md section 9).

use super::session::Protocol;
use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use pebble_protocol::crc::stm32_crc;
use pebble_protocol::endpoint;
use pebble_protocol::firmware::{manifest_folders, Build, Manifest};
use pebble_protocol::putbytes::{self, ObjectType, Response};
use pebble_protocol::system::{self, FirmwareStartStatus};
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const LATEST_RELEASE: &str = "https://api.github.com/repos/coredevices/PebbleOS/releases/latest";

#[derive(serde::Serialize, Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

#[derive(serde::Serialize, Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Normal,
    Recovery,
}

pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("Lithidion/", env!("CARGO_PKG_VERSION"), " (https://dev.dunevent.net/aj/Lithidion)"))
        .connect_timeout(Duration::from_secs(20))
        .build()?)
}

pub async fn latest_release(client: &reqwest::Client) -> Result<Release> {
    let response = client
        .get(LATEST_RELEASE)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("cannot reach GitHub to look for PebbleOS")?;
    if !response.status().is_success() {
        bail!("GitHub answered {} when asked for the latest PebbleOS", response.status());
    }
    response.json().await.context("GitHub's release list could not be read")
}

/// The combined bundle (both slots) or the recovery bundle for a hardware revision, named
/// `normal_obelix_pvt_v4.47.0.pbz` or `recovery_obelix_pvt_v4.47.0.pbz`.
pub fn asset<'a>(release: &'a Release, kind: Kind, hardware_revision: &str) -> Option<&'a Asset> {
    let prefix = match kind {
        Kind::Normal => "normal",
        Kind::Recovery => "recovery",
    };
    let name = format!("{prefix}_{hardware_revision}_{}.pbz", release.tag_name);
    release.assets.iter().find(|a| a.name == name)
}

pub fn cache_dir() -> PathBuf {
    glib::user_cache_dir().join("lithidion").join("firmware")
}

/// Downloads `asset` into the cache, or reuses a complete earlier download.
pub async fn download(client: &reqwest::Client, asset: &Asset, mut progress: impl FnMut(f64)) -> Result<PathBuf> {
    let dir = cache_dir();
    tokio::fs::create_dir_all(&dir).await.with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(&asset.name);
    if tokio::fs::metadata(&path).await.is_ok_and(|m| m.len() == asset.size) {
        return Ok(path);
    }
    let part = dir.join(format!("{}.part", asset.name));
    let response = client
        .get(&asset.browser_download_url)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .context("the firmware download did not start")?
        .error_for_status()
        .context("the firmware download failed")?;
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = response.bytes_stream();
    let mut done = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("the firmware download was interrupted")?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        progress(done as f64 / asset.size.max(1) as f64);
    }
    file.flush().await?;
    if done != asset.size {
        bail!("the firmware download ended early ({done} of {} bytes)", asset.size);
    }
    tokio::fs::rename(&part, &path).await?;
    Ok(path)
}

/// Every build in a `.pbz`, with its bytes.
pub fn read_bundle(path: &Path) -> Result<Vec<Build>> {
    let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("the file is not a firmware bundle (.pbz)")?;
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    let mut read = |name: &str| -> Result<Vec<u8>> {
        let mut entry = zip.by_name(name).with_context(|| format!("{name} is missing from the bundle"))?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let mut builds = Vec::new();
    for folder in manifest_folders(&names).map_err(|e| anyhow!(e))? {
        let manifest = Manifest::parse(&read(&format!("{folder}manifest.json"))?).map_err(|e| anyhow!(e))?;
        let firmware = read(&format!("{folder}{}", manifest.firmware.name))?;
        let resources = match &manifest.resources {
            Some(r) => Some(read(&format!("{folder}{}", r.name))?),
            None => None,
        };
        builds.push(Build { manifest, folder, firmware, resources });
    }
    Ok(builds)
}

pub(super) async fn putbytes_request(protocol: &Protocol, payload: &[u8], cookie: Option<u32>, limit: Duration) -> Result<u32> {
    let reply = protocol.request(endpoint::PUT_BYTES, payload, endpoint::PUT_BYTES, |p| Response::parse(p).is_ok(), limit).await?;
    let response = Response::parse(&reply)?;
    if !response.ack {
        if let Some(c) = cookie {
            let _ = protocol.send(endpoint::PUT_BYTES, &putbytes::abort(c));
        }
        bail!("the watch refused the transfer");
    }
    if cookie.is_some_and(|c| c != response.cookie) {
        bail!("the watch answered for another transfer");
    }
    Ok(response.cookie)
}

/// Install's answer echoes its cookie only on recent PebbleOS: older builds, recovery firmware
/// 4.9.142 among them, clear the transfer before answering and send another cookie (zero), so
/// only the ack counts here, as in the official app.
pub(super) async fn putbytes_install(protocol: &Protocol, cookie: u32) -> Result<()> {
    let reply = protocol
        .request(
            endpoint::PUT_BYTES,
            &putbytes::install(cookie),
            endpoint::PUT_BYTES,
            |p| Response::parse(p).is_ok(),
            Duration::from_secs(20),
        )
        .await?;
    let response = Response::parse(&reply)?;
    if !response.ack {
        bail!("the watch refused to install the update");
    }
    if response.cookie != cookie {
        tracing::debug!(sent = cookie, answered = response.cookie, "install answered with another cookie");
    }
    Ok(())
}

/// Sends one object and commits it; returns its cookie, for the install that follows.
async fn send_object(protocol: &Protocol, object: ObjectType, bytes: &[u8], crc: u32, mut sent: impl FnMut(usize)) -> Result<u32> {
    // Init can wait for the watch to erase flash.
    let cookie =
        putbytes_request(protocol, &putbytes::init(bytes.len() as u32, object, 0, "", None), None, Duration::from_secs(35)).await?;
    for chunk in bytes.chunks(putbytes::CHUNK) {
        putbytes_request(protocol, &putbytes::put(cookie, chunk), Some(cookie), Duration::from_secs(20)).await?;
        sent(chunk.len());
    }
    putbytes_request(protocol, &putbytes::commit(cookie, crc), Some(cookie), Duration::from_secs(20)).await?;
    Ok(cookie)
}

/// Installs a checked build: firmware, then resources, then both installs, then "complete",
/// after which the watch restarts.
pub async fn install(protocol: &Protocol, build: &Build, mut progress: impl FnMut(f64)) -> Result<()> {
    let total = build.manifest.total_size();
    let start = protocol
        .request(
            endpoint::SYSTEM_MESSAGE,
            &system::firmware_update_start(0, total as u32),
            endpoint::SYSTEM_MESSAGE,
            system::is_firmware_start_response,
            Duration::from_secs(30),
        )
        .await?;
    match system::parse_firmware_start_response(&start)? {
        FirmwareStartStatus::Started => {}
        other => bail!("the watch did not start the update ({other:?})"),
    }
    let mut done = 0u64;
    let mut sent = |n: usize| {
        done += n as u64;
        progress(done as f64 / total.max(1) as f64);
    };
    let object = if build.manifest.is_recovery() { ObjectType::Recovery } else { ObjectType::Firmware };
    let firmware_crc = stm32_crc(&build.firmware);
    let firmware = send_object(protocol, object, &build.firmware, firmware_crc, &mut sent).await.context("sending the firmware failed")?;
    let resources = match &build.resources {
        Some(bytes) => Some(
            send_object(protocol, ObjectType::SystemResources, bytes, stm32_crc(bytes), &mut sent)
                .await
                .context("sending the system resources failed")?,
        ),
        None => None,
    };
    // Both are installed only once both have arrived.
    putbytes_install(protocol, firmware).await?;
    if let Some(r) = resources {
        putbytes_install(protocol, r).await?;
    }
    protocol.send(endpoint::SYSTEM_MESSAGE, &system::FIRMWARE_UPDATE_COMPLETE)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pebble_protocol::firmware::{check, route, Route, Running};

    /// Runs against a real bundle: `LITHIDION_PBZ=/path/normal_obelix_pvt_v4.47.0.pbz cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_bundle() {
        let path = std::env::var("LITHIDION_PBZ").expect("LITHIDION_PBZ names a .pbz");
        let builds = read_bundle(Path::new(&path)).unwrap();
        let hw = builds[0].manifest.firmware.hwrev.clone();
        let running = Running { recovery: true, dual_slot: false, slot: None, tag: "v4.9.142".into() };
        let Some(Route::Direct(i)) = route(&builds, &running) else { panic!("no direct route from recovery") };
        check(&builds[i], &hw, running.target_slot()).unwrap();
        for b in &builds {
            check(b, &hw, b.manifest.firmware.slot).unwrap();
        }
    }

    /// A watch that takes a firmware update: answers the start, acks PutBytes, and checks
    /// each object's CRC on commit.
    #[tokio::test]
    async fn install_sends_both_objects() {
        use pebble_protocol::firmware::{ManifestFirmware, ManifestResources};
        use pebble_protocol::frame::{Frame, Reassembler};
        let firmware: Vec<u8> = (0..5003u32).map(|i| (i * 7) as u8).collect();
        let resources: Vec<u8> = (0..2001u32).map(|i| (i * 13) as u8).collect();
        let build = Build {
            manifest: Manifest {
                firmware: ManifestFirmware {
                    name: "pebbleos.bin".into(),
                    kind: "normal".into(),
                    timestamp: 0,
                    commit: String::new(),
                    hwrev: "obelix_pvt".into(),
                    size: firmware.len() as u64,
                    crc: stm32_crc(&firmware) as u64,
                    version_tag: Some("v4.47.0".into()),
                    slot: Some(0),
                },
                resources: Some(ManifestResources {
                    name: "system_resources.pbpack".into(),
                    size: resources.len() as u64,
                    crc: stm32_crc(&resources) as u64,
                }),
            },
            folder: "slot0/".into(),
            firmware: firmware.clone(),
            resources: Some(resources.clone()),
        };
        let (protocol, mut out, watch_tx) = Protocol::loopback();
        let watch = tokio::spawn(async move {
            let mut reassembler = Reassembler::new();
            let mut objects: Vec<(u8, Vec<u8>, bool, bool)> = Vec::new(); // type, bytes, committed, installed
            let mut log = Vec::new();
            let reply = |endpoint, payload: Vec<u8>| {
                let _ = watch_tx.send(Frame { endpoint, payload });
            };
            while let Some(bytes) = out.recv().await {
                reassembler.push(&bytes);
                while let Some(f) = reassembler.next_frame() {
                    let p = &f.payload;
                    match (f.endpoint, p[0]) {
                        (endpoint::SYSTEM_MESSAGE, 0x00) if p[1] == 0x01 => {
                            log.push("start");
                            reply(endpoint::SYSTEM_MESSAGE, vec![0x00, 0x0A, 0x01]);
                        }
                        (endpoint::SYSTEM_MESSAGE, 0x00) if p[1] == 0x02 => {
                            log.push("complete");
                            return (objects, log);
                        }
                        (endpoint::PUT_BYTES, 0x01) => {
                            objects.push((p[5], Vec::new(), false, false));
                            reply(endpoint::PUT_BYTES, [&[1u8][..], &(objects.len() as u32 + 100).to_be_bytes()].concat());
                        }
                        (endpoint::PUT_BYTES, op) => {
                            let cookie = u32::from_be_bytes(p[1..5].try_into().unwrap());
                            let o = &mut objects[(cookie - 101) as usize];
                            match op {
                                0x02 => o.1.extend_from_slice(&p[9..]),
                                0x03 => {
                                    assert_eq!(u32::from_be_bytes(p[5..9].try_into().unwrap()), stm32_crc(&o.1));
                                    o.2 = true;
                                }
                                0x05 => {
                                    assert!(o.2, "installed before commit");
                                    o.3 = true;
                                    log.push("install");
                                    // As recovery firmware 4.9.142 does.
                                    reply(endpoint::PUT_BYTES, vec![1, 0, 0, 0, 0]);
                                    continue;
                                }
                                _ => panic!("unexpected PutBytes {op}"),
                            }
                            reply(endpoint::PUT_BYTES, [&[1u8][..], &cookie.to_be_bytes()].concat());
                        }
                        _ => panic!("unexpected frame on {}", f.endpoint),
                    }
                }
            }
            panic!("the phone stopped early");
        });
        let mut fractions = Vec::new();
        install(&protocol, &build, |f| fractions.push(f)).await.unwrap();
        let (objects, log) = watch.await.unwrap();
        assert_eq!(log, ["start", "install", "install", "complete"]);
        assert_eq!(objects.len(), 2);
        assert_eq!((objects[0].0, &objects[0].1), (ObjectType::Firmware as u8, &firmware));
        assert_eq!((objects[1].0, &objects[1].1), (ObjectType::SystemResources as u8, &resources));
        assert!(objects.iter().all(|o| o.2 && o.3));
        assert_eq!(fractions.last(), Some(&1.0));
    }

    #[test]
    fn asset_names() {
        let release = Release {
            tag_name: "v4.47.0".into(),
            assets: ["normal_obelix_pvt_v4.47.0.pbz", "normal_obelix_pvt_v4.47.0_slot0.pbz", "recovery_obelix_pvt_v4.47.0.pbz"]
                .iter()
                .map(|n| Asset { name: n.to_string(), browser_download_url: format!("https://x/{n}"), size: 1 })
                .collect(),
        };
        assert_eq!(asset(&release, Kind::Normal, "obelix_pvt").unwrap().name, "normal_obelix_pvt_v4.47.0.pbz");
        assert_eq!(asset(&release, Kind::Recovery, "obelix_pvt").unwrap().name, "recovery_obelix_pvt_v4.47.0.pbz");
        assert!(asset(&release, Kind::Normal, "asterix").is_none());
    }
}
