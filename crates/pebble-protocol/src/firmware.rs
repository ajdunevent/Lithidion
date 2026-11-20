//! Firmware bundles (`.pbz`) and how one is installed (section 9): manifests, the safety checks,
//! and which slot's build goes where. Reading the zip is left to the caller.

use crate::crc::stm32_crc;
use crate::system::{parse_version, FirmwareInfo, Version};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ManifestFirmware {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub timestamp: u64,
    #[serde(default)]
    pub commit: String,
    pub hwrev: String,
    pub size: u64,
    pub crc: u64,
    #[serde(rename = "versionTag", default)]
    pub version_tag: Option<String>,
    #[serde(default)]
    pub slot: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ManifestResources {
    pub name: String,
    pub size: u64,
    pub crc: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub firmware: ManifestFirmware,
    #[serde(default)]
    pub resources: Option<ManifestResources>,
}

impl Manifest {
    pub fn parse(json: &[u8]) -> Result<Manifest, String> {
        serde_json::from_slice(json).map_err(|e| format!("manifest.json: {e}"))
    }

    pub fn is_recovery(&self) -> bool {
        self.firmware.kind == "recovery"
    }

    pub fn version(&self) -> Option<Version> {
        self.firmware.version_tag.as_deref().and_then(parse_version)
    }

    pub fn total_size(&self) -> u64 {
        self.firmware.size + self.resources.as_ref().map_or(0, |r| r.size)
    }
}

/// One installable build from a bundle: a manifest, its folder in the zip ("" or "slot0/"),
/// and the bytes it names.
#[derive(Debug, Clone)]
pub struct Build {
    pub manifest: Manifest,
    pub folder: String,
    pub firmware: Vec<u8>,
    pub resources: Option<Vec<u8>>,
}

/// Where the manifests of a bundle sit: one at the root, or one each under `slot0/` and
/// `slot1/`.
pub fn manifest_folders(names: &[String]) -> Result<Vec<String>, String> {
    if names.iter().any(|n| n == "manifest.json") {
        return Ok(vec![String::new()]);
    }
    if names.iter().any(|n| n == "slot0/manifest.json") && names.iter().any(|n| n == "slot1/manifest.json") {
        return Ok(vec!["slot0/".to_string(), "slot1/".to_string()]);
    }
    Err("the bundle has no manifest.json".to_string())
}

/// The safety checks of the official app, plus the CRCs of the bytes against the manifest.
pub fn check(build: &Build, hardware_revision: &str, target_slot: Option<u8>) -> Result<(), String> {
    let f = &build.manifest.firmware;
    let recovery = build.manifest.is_recovery();
    if f.kind != "normal" && !recovery {
        return Err(format!("unknown firmware type \"{}\"", f.kind));
    }
    if f.size == 0 || f.crc == 0 {
        return Err("the manifest gives no firmware size or CRC".to_string());
    }
    if f.hwrev != hardware_revision {
        return Err(format!("this firmware is for {}, and the watch is {hardware_revision}", f.hwrev));
    }
    if let (Some(target), false) = (target_slot, recovery) {
        if f.slot != Some(target) {
            return Err(format!("this build is for slot {:?}, and the watch needs slot {target}", f.slot));
        }
    }
    if build.firmware.len() as u64 != f.size || stm32_crc(&build.firmware) as u64 != f.crc {
        return Err("the firmware does not match its manifest".to_string());
    }
    match (&build.manifest.resources, &build.resources) {
        (Some(r), Some(bytes)) => {
            if r.size == 0 || bytes.len() as u64 != r.size || stm32_crc(bytes) as u64 != r.crc {
                return Err("the resources do not match their manifest".to_string());
            }
        }
        (Some(_), None) => return Err("the resources named in the manifest are missing".to_string()),
        _ => {}
    }
    Ok(())
}

/// What the watch runs now, for choosing a route.
#[derive(Debug, Clone)]
pub struct Running {
    pub recovery: bool,
    pub dual_slot: bool,
    pub slot: Option<u8>,
    pub tag: String,
}

impl Running {
    pub fn from_info(info: &FirmwareInfo) -> Running {
        Running { recovery: info.is_recovery(), dual_slot: info.is_dual_slot(), slot: info.slot(), tag: info.tag.clone() }
    }

    /// The slot an update goes to: the one not running.
    pub fn target_slot(&self) -> Option<u8> {
        match (self.recovery, self.slot) {
            (false, Some(0)) => Some(1),
            (false, Some(1)) => Some(0),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Install build `index` now.
    Direct(usize),
    /// Restart into recovery firmware, which then installs build `index` (always slot 0).
    ViaRecovery(usize),
}

/// A release tag as the bootloader sees one: `v4.47.0`, `4.47`, `v4.47.0-rc1`, `-beta2`.
pub fn is_release(tag: &str) -> bool {
    let t = tag.trim_start_matches('v');
    let (core, suffix) = match t.split_once('-') {
        Some((c, s)) => (c, Some(s)),
        None => (t, None),
    };
    let numbers_ok =
        !core.is_empty() && core.split('.').count() <= 3 && core.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let suffix_ok = suffix.is_none_or(|s| {
        let digits = s.strip_prefix("beta").or_else(|| s.strip_prefix("rc"));
        digits.is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
    });
    numbers_ok && suffix_ok
}

/// Of two valid slots the bootloader boots the higher: any development build over any release,
/// releases by version.
fn boots_after_direct_install(running: &str, target: &str) -> bool {
    if !is_release(target) {
        return true;
    }
    if !is_release(running) {
        return false;
    }
    match (parse_version(running), parse_version(target)) {
        (Some(r), Some(t)) => t >= r,
        _ => false,
    }
}

/// How the builds in a bundle install on a watch running `running`; `None` when none fits.
pub fn route(builds: &[Build], running: &Running) -> Option<Route> {
    let slot0 = builds.iter().position(|b| b.manifest.firmware.slot == Some(0));
    if builds.first().is_some_and(|b| b.manifest.is_recovery()) {
        return Some(Route::Direct(0));
    }
    if running.recovery {
        return slot0.or_else(|| (builds.len() == 1 && builds[0].manifest.firmware.slot.is_none()).then_some(0)).map(Route::Direct);
    }
    let Some(target) = running.target_slot() else {
        return (builds.len() == 1).then_some(Route::Direct(0));
    };
    let other = builds.iter().position(|b| b.manifest.firmware.slot == Some(target));
    match other {
        Some(i)
            if !running.dual_slot
                || boots_after_direct_install(&running.tag, builds[i].manifest.firmware.version_tag.as_deref().unwrap_or("")) =>
        {
            Some(Route::Direct(i))
        }
        _ => slot0.map(Route::ViaRecovery),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(slot: Option<u8>, tag: &str, kind: &str) -> Build {
        let firmware = vec![1, 2, 3, 4, 5, 6];
        let resources = vec![9, 9, 9];
        Build {
            manifest: Manifest {
                firmware: ManifestFirmware {
                    name: "pebbleos.bin".into(),
                    kind: kind.into(),
                    timestamp: 1,
                    commit: String::new(),
                    hwrev: "obelix_pvt".into(),
                    size: firmware.len() as u64,
                    crc: stm32_crc(&firmware) as u64,
                    version_tag: Some(tag.into()),
                    slot,
                },
                resources: Some(ManifestResources { name: "r".into(), size: 3, crc: stm32_crc(&resources) as u64 }),
            },
            folder: String::new(),
            firmware,
            resources: Some(resources),
        }
    }

    fn running(recovery: bool, slot: Option<u8>, tag: &str) -> Running {
        Running { recovery, dual_slot: slot.is_some(), slot, tag: tag.into() }
    }

    #[test]
    fn parses_a_real_manifest() {
        let json = br#"{"manifestVersion":2,"generatedAt":1794602615,"generatedBy":"","debug":{},
            "firmware":{"name":"pebbleos.bin","type":"normal","timestamp":1794599791,"commit":"f4926c952",
            "hwrev":"obelix_pvt","size":1899976,"crc":764780087,"versionTag":"v4.47.0","slot":0},
            "resources":{"name":"system_resources.pbpack","timestamp":1794599791,"size":900386,"crc":3247032109},
            "type":"firmware"}"#;
        let m = Manifest::parse(json).unwrap();
        assert_eq!(m.firmware.slot, Some(0));
        assert_eq!(m.version(), parse_version("4.47.0"));
        assert_eq!(m.total_size(), 1899976 + 900386);
    }

    #[test]
    fn folders() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(manifest_folders(&names(&["manifest.json", "x.bin"])).unwrap(), vec![""]);
        assert_eq!(manifest_folders(&names(&["slot0/manifest.json", "slot1/manifest.json"])).unwrap(), vec!["slot0/", "slot1/"]);
        assert!(manifest_folders(&names(&["slot0/manifest.json"])).is_err());
    }

    #[test]
    fn checks() {
        let b = build(Some(1), "v4.47.0", "normal");
        assert!(check(&b, "obelix_pvt", Some(1)).is_ok());
        assert!(check(&b, "asterix", Some(1)).unwrap_err().contains("asterix"));
        assert!(check(&b, "obelix_pvt", Some(0)).is_err());
        let mut bad = b.clone();
        bad.firmware[0] = 0;
        assert!(check(&bad, "obelix_pvt", Some(1)).is_err());
        assert!(check(&build(None, "v4.47.0", "recovery"), "obelix_pvt", Some(0)).is_ok());
    }

    #[test]
    fn routes() {
        let pair = [build(Some(0), "v4.47.0", "normal"), build(Some(1), "v4.47.0", "normal")];
        assert_eq!(route(&pair, &running(true, None, "v4.9.142")), Some(Route::Direct(0)));
        assert_eq!(route(&pair, &running(false, Some(0), "v4.46.0")), Some(Route::Direct(1)));
        assert_eq!(route(&pair, &running(false, Some(1), "v4.46.0")), Some(Route::Direct(0)));
        assert_eq!(route(&pair, &running(false, Some(0), "v4.48.0")), Some(Route::ViaRecovery(0)), "a downgrade goes through recovery");
        assert_eq!(route(&pair, &running(false, Some(0), "v4.48.0-dev3")), Some(Route::ViaRecovery(0)));
        assert_eq!(route(&[build(None, "v4.0.0", "recovery")], &running(false, Some(0), "v4.46.0")), Some(Route::Direct(0)));
    }

    #[test]
    fn release_tags() {
        for t in ["v4.47.0", "4.47", "v4.47.0-rc1", "v4.47.0-beta12"] {
            assert!(is_release(t), "{t}");
        }
        for t in ["v4.47.0-dev3", "v4.47.0-12-gabc", "", "vX"] {
            assert!(!is_release(t), "{t}");
        }
    }
}
