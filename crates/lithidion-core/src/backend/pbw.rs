//! App bundles (`.pbw`, PROTOCOL.md section 10): `appinfo.json` at the root, and a folder per
//! platform with a `manifest.json` and the files it names. Aplite builds of old bundles sit at
//! the root instead.

use anyhow::{anyhow, bail, Context, Result};
use pebble_protocol::apps::{version_bytes, AppMetadata, BinaryHeader};
use pebble_protocol::crc::stm32_crc;
use pebble_protocol::hardware::AppPlatform;
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub uuid: uuid::Uuid,
    pub short_name: String,
    #[serde(default)]
    pub long_name: String,
    #[serde(default)]
    pub company_name: String,
    #[serde(default)]
    pub version_label: String,
    #[serde(default)]
    pub watchapp: Watchapp,
    /// Message key names and their numbers, for the app's script.
    #[serde(default)]
    pub app_keys: std::collections::HashMap<String, u32>,
    /// What the app asks for: "configurable", "location", …
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Watchapp {
    #[serde(default)]
    pub watchface: bool,
    #[serde(default)]
    pub hidden_app: bool,
}

impl AppInfo {
    pub fn can(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    pub fn title(&self) -> &str {
        if self.long_name.trim().is_empty() {
            &self.short_name
        } else {
            &self.long_name
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Blob {
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub crc: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub application: Blob,
    #[serde(default)]
    pub resources: Option<Blob>,
    #[serde(default)]
    pub worker: Option<Blob>,
}

/// The parts of one platform's build, read for sending.
pub struct Build {
    pub binary: Vec<u8>,
    pub resources: Option<Vec<u8>>,
    pub worker: Option<Vec<u8>>,
}

pub struct Pbw {
    path: PathBuf,
    pub info: AppInfo,
    /// The platforms with a build, and where its files sit ("" or "emery/").
    pub builds: Vec<(AppPlatform, String)>,
}

const PLATFORMS: [AppPlatform; 7] = [
    AppPlatform::Aplite,
    AppPlatform::Basalt,
    AppPlatform::Chalk,
    AppPlatform::Diorite,
    AppPlatform::Emery,
    AppPlatform::Flint,
    AppPlatform::Gabbro,
];

const SCRIPT: &str = "pebble-js-app.js";

fn read_entry(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<Vec<u8>> {
    let mut entry = zip.by_name(name).with_context(|| format!("{name} is missing from the app"))?;
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<std::fs::File>> {
    let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    zip::ZipArchive::new(file).context("the file is not a Pebble app (.pbw)")
}

impl Pbw {
    pub fn open(path: &Path) -> Result<Pbw> {
        let mut zip = open_zip(path)?;
        let info: AppInfo = serde_json::from_slice(&read_entry(&mut zip, "appinfo.json")?).context("appinfo.json cannot be read")?;
        let names: Vec<String> = zip.file_names().map(str::to_string).collect();
        let mut builds = Vec::new();
        for p in PLATFORMS {
            let folder = format!("{}/", p.codename());
            if names.iter().any(|n| *n == format!("{folder}manifest.json")) {
                builds.push((p, folder));
            } else if p == AppPlatform::Aplite && names.iter().any(|n| n == "manifest.json") {
                builds.push((p, String::new()));
            }
        }
        if builds.is_empty() {
            bail!("the app has no build for any watch");
        }
        Ok(Pbw { path: path.to_path_buf(), info, builds })
    }

    /// The app's phone-side script (PebbleKit JS), when it has one.
    pub fn script(&self) -> Result<Option<String>> {
        let mut zip = open_zip(&self.path)?;
        if zip.index_for_name(SCRIPT).is_none() {
            return Ok(None);
        }
        let bytes = read_entry(&mut zip, SCRIPT)?;
        Ok(Some(String::from_utf8(bytes).context("the app's script is not UTF-8")?))
    }

    pub fn has_script(&self) -> bool {
        open_zip(&self.path).is_ok_and(|z| z.index_for_name(SCRIPT).is_some())
    }

    /// The best build for a watch of `platform`.
    pub fn build_for(&self, platform: AppPlatform) -> Option<&(AppPlatform, String)> {
        platform.compatible().iter().find_map(|p| self.builds.iter().find(|(b, _)| b == p))
    }

    fn manifest(zip: &mut zip::ZipArchive<std::fs::File>, folder: &str) -> Result<Manifest> {
        serde_json::from_slice(&read_entry(zip, &format!("{folder}manifest.json"))?).context("the app's manifest cannot be read")
    }

    /// The watch's app database entry for this app.
    pub fn metadata(&self, platform: AppPlatform) -> Result<AppMetadata> {
        let (_, folder) = self.build_for(platform).ok_or_else(|| anyhow!("{} has no build for this watch", self.info.title()))?;
        let mut zip = open_zip(&self.path)?;
        let manifest = Self::manifest(&mut zip, folder)?;
        let binary = read_entry(&mut zip, &format!("{folder}{}", manifest.application.name))?;
        let header = BinaryHeader::parse(&binary).map_err(|e| anyhow!("{e}"))?;
        Ok(AppMetadata {
            uuid: self.info.uuid,
            flags: header.flags,
            icon: header.icon,
            app_version: version_bytes(&self.info.version_label),
            sdk_version: header.sdk_version,
            name: self.info.title().to_string(),
        })
    }

    /// The files of the best build, checked against the manifest's sizes and CRCs.
    pub fn read_build(&self, platform: AppPlatform) -> Result<Build> {
        let (_, folder) = self.build_for(platform).ok_or_else(|| anyhow!("{} has no build for this watch", self.info.title()))?;
        let mut zip = open_zip(&self.path)?;
        let manifest = Self::manifest(&mut zip, folder)?;
        let mut read = |blob: &Blob| -> Result<Vec<u8>> {
            let bytes = read_entry(&mut zip, &format!("{folder}{}", blob.name))?;
            if bytes.len() as u64 != blob.size || blob.crc.is_some_and(|c| c != stm32_crc(&bytes) as u64) {
                bail!("{} does not match the app's manifest", blob.name);
            }
            Ok(bytes)
        };
        Ok(Build {
            binary: read(&manifest.application)?,
            resources: manifest.resources.as_ref().map(&mut read).transpose()?,
            worker: manifest.worker.as_ref().map(&mut read).transpose()?,
        })
    }
}

/// Where bundles are kept: `apps/<uuid>.pbw` in the data folder.
pub fn dir() -> PathBuf {
    glib::user_data_dir().join("lithidion").join("apps")
}

pub fn path(uuid: uuid::Uuid) -> PathBuf {
    dir().join(format!("{uuid}.pbw"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `LITHIDION_PBW=/path/app.pbw cargo test -- --ignored`, with an emery build in it.
    #[test]
    #[ignore]
    fn real_pbw() {
        let path = std::env::var("LITHIDION_PBW").expect("LITHIDION_PBW names a .pbw");
        let pbw = Pbw::open(Path::new(&path)).unwrap();
        let m = pbw.metadata(AppPlatform::Emery).unwrap();
        assert_eq!(m.uuid, pbw.info.uuid);
        assert_eq!(m.encode().len(), AppMetadata::LEN);
        let build = pbw.read_build(AppPlatform::Emery).unwrap();
        assert!(!build.binary.is_empty());
        // Emery runs basalt builds when there is no emery one.
        assert!(pbw.build_for(AppPlatform::Emery).is_some());
    }
}
