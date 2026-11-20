//! Messages used to start and keep a session (section 6): phone and watch version, model, time,
//! app run state, ping, and reset.

use crate::capabilities::Capabilities;
use crate::hardware::HardwarePlatform;
use crate::wire::{err, put_pstring, Reader, Result};
use uuid::Uuid;

/// OS type in the phone version response. The watch's music endpoint only works with Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsType {
    Android = 2,
    Linux = 4,
}

pub fn is_phone_version_request(payload: &[u8]) -> bool {
    payload.first() == Some(&0x00)
}

pub fn phone_version_response(os: OsType, capabilities: &Capabilities) -> Vec<u8> {
    let mut out = vec![0x01];
    out.extend_from_slice(&u32::MAX.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(os as u32).to_be_bytes());
    out.extend_from_slice(&[2, 4, 4, 2]);
    out.extend_from_slice(&capabilities.0);
    out
}

pub const WATCH_VERSION_REQUEST: [u8; 1] = [0x00];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareInfo {
    pub timestamp: u32,
    pub tag: String,
    pub git_hash: String,
    pub flags: u8,
    pub hardware_platform: u8,
    pub metadata_version: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Parses `v4.35.0`, `4.35`, or `v4.35.0-rc1`; the suffix is dropped.
pub fn parse_version(tag: &str) -> Option<Version> {
    let core = tag.trim().trim_start_matches('v').split('-').next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().map(|p| p.parse().ok()).unwrap_or(Some(0))?;
    Some(Version { major, minor, patch })
}

impl FirmwareInfo {
    fn parse(r: &mut Reader) -> Result<FirmwareInfo> {
        Ok(FirmwareInfo {
            timestamp: r.u32()?,
            tag: r.fixed_string(32)?,
            git_hash: r.fixed_string(8)?,
            flags: r.u8()?,
            hardware_platform: r.u8()?,
            metadata_version: r.u8()?,
        })
    }

    pub fn is_recovery(&self) -> bool {
        self.flags & 0x01 != 0
    }

    pub fn is_dual_slot(&self) -> bool {
        self.flags & 0x04 != 0
    }

    pub fn is_slot0(&self) -> bool {
        self.flags & 0x08 != 0
    }

    /// The running slot, for dual-slot firmware.
    pub fn slot(&self) -> Option<u8> {
        self.is_dual_slot().then_some(if self.is_slot0() { 0 } else { 1 })
    }

    pub fn version(&self) -> Option<Version> {
        parse_version(&self.tag)
    }

    pub fn is_present(&self) -> bool {
        !self.tag.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchVersion {
    pub running: FirmwareInfo,
    pub recovery: FirmwareInfo,
    pub bootloader_timestamp: u32,
    pub board: String,
    pub serial: String,
    pub bt_address: [u8; 6],
    pub resource_crc: u32,
    pub resource_timestamp: u32,
    pub language: Option<String>,
    pub language_version: Option<u16>,
    pub capabilities: Capabilities,
    pub unfaithful: Option<bool>,
    pub health_insights_version: Option<u16>,
    pub javascript_version: Option<u16>,
}

impl WatchVersion {
    pub fn parse(payload: &[u8]) -> Result<WatchVersion> {
        let mut r = Reader::new(payload);
        if r.u8()? != 0x01 {
            return err("not a watch version response");
        }
        let running = FirmwareInfo::parse(&mut r)?;
        let recovery = FirmwareInfo::parse(&mut r)?;
        let bootloader_timestamp = r.u32()?;
        let board = r.fixed_string(9)?;
        let serial = r.fixed_string(12)?;
        let bt_address = r.array()?;
        let resource_crc = r.u32()?;
        let resource_timestamp = r.u32()?;
        let (mut language, mut language_version, mut capabilities) = (None, None, Capabilities::default());
        if r.remaining() >= 16 {
            language = Some(r.fixed_string(6)?);
            language_version = Some(r.u16()?);
            capabilities = Capabilities(r.array()?);
        }
        let unfaithful = if r.remaining() >= 1 { Some(r.u8()? != 0) } else { None };
        let health_insights_version = if r.remaining() >= 2 { Some(r.u16()?) } else { None };
        let javascript_version = if r.remaining() >= 2 { Some(r.u16()?) } else { None };
        Ok(WatchVersion {
            running,
            recovery,
            bootloader_timestamp,
            board,
            serial,
            bt_address,
            resource_crc,
            resource_timestamp,
            language,
            language_version,
            capabilities,
            unfaithful,
            health_insights_version,
            javascript_version,
        })
    }

    pub fn hardware(&self) -> HardwarePlatform {
        HardwarePlatform::from_number(self.running.hardware_platform)
    }
}

pub fn factory_request(key: &str) -> Vec<u8> {
    let mut out = vec![0x00];
    put_pstring(&mut out, key);
    out
}

/// The value for a factory registry key; `None` when the watch has none.
pub fn parse_factory_response(payload: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut r = Reader::new(payload);
    match r.u8()? {
        0x01 => {
            let len = r.u8()? as usize;
            Ok(Some(r.bytes(len)?.to_vec()))
        }
        0xFF => Ok(None),
        other => err(format!("unknown factory registry message {other:#04x}")),
    }
}

/// The model number from the `mfg_color` value.
pub fn model_number(value: &[u8]) -> Option<i32> {
    Reader::new(value).i32().ok()
}

pub fn set_utc(unix_seconds: u32, utc_offset_minutes: i16, time_zone: &str) -> Vec<u8> {
    let mut out = vec![0x03];
    out.extend_from_slice(&unix_seconds.to_be_bytes());
    out.extend_from_slice(&utc_offset_minutes.to_be_bytes());
    put_pstring(&mut out, time_zone);
    out
}

pub fn is_get_utc_request(payload: &[u8]) -> bool {
    payload.first() == Some(&0x04)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRunState {
    Started(Uuid),
    Stopped(Uuid),
    Request,
}

impl AppRunState {
    pub fn parse(payload: &[u8]) -> Result<AppRunState> {
        let mut r = Reader::new(payload);
        match r.u8()? {
            0x01 => Ok(AppRunState::Started(r.uuid()?)),
            0x02 => Ok(AppRunState::Stopped(r.uuid()?)),
            0x03 => Ok(AppRunState::Request),
            other => err(format!("unknown app run state message {other:#04x}")),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            AppRunState::Started(u) => [&[0x01][..], u.as_bytes()].concat(),
            AppRunState::Stopped(u) => [&[0x02][..], u.as_bytes()].concat(),
            AppRunState::Request => vec![0x03],
        }
    }
}

pub fn ping(cookie: u32) -> Vec<u8> {
    let mut out = vec![0x00];
    out.extend_from_slice(&cookie.to_be_bytes());
    out
}

pub fn pong(cookie: u32) -> Vec<u8> {
    let mut out = vec![0x01];
    out.extend_from_slice(&cookie.to_be_bytes());
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingPong {
    Ping(u32),
    Pong(u32),
}

pub fn parse_ping(payload: &[u8]) -> Result<PingPong> {
    let mut r = Reader::new(payload);
    match r.u8()? {
        0x00 => Ok(PingPong::Ping(r.u32()?)),
        0x01 => Ok(PingPong::Pong(r.u32()?)),
        other => err(format!("unknown ping message {other:#04x}")),
    }
}

/// System messages (endpoint 18) start with `00` and their type. The firmware update start
/// carries little-endian counts.
pub fn firmware_update_start(already_sent: u32, to_send: u32) -> Vec<u8> {
    let mut out = vec![0x00, 0x01];
    out.extend_from_slice(&already_sent.to_le_bytes());
    out.extend_from_slice(&to_send.to_le_bytes());
    out
}

pub const FIRMWARE_UPDATE_COMPLETE: [u8; 2] = [0x00, 0x02];
pub const FIRMWARE_UPDATE_FAILED: [u8; 2] = [0x00, 0x03];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareStartStatus {
    Stopped,
    Started,
    Cancelled,
    Other(u8),
}

pub fn is_firmware_start_response(payload: &[u8]) -> bool {
    payload.len() >= 3 && payload[0] == 0x00 && payload[1] == 0x0A
}

pub fn parse_firmware_start_response(payload: &[u8]) -> Result<FirmwareStartStatus> {
    if !is_firmware_start_response(payload) {
        return err("not a firmware update start response");
    }
    Ok(match payload[2] {
        0 => FirmwareStartStatus::Stopped,
        1 => FirmwareStartStatus::Started,
        2 => FirmwareStartStatus::Cancelled,
        other => FirmwareStartStatus::Other(other),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reset {
    Reboot = 0x00,
    CoreDump = 0x01,
    FactoryReset = 0xFE,
    IntoRecovery = 0xFF,
}

pub fn reset(kind: Reset) -> Vec<u8> {
    vec![kind as u8]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::Capability;

    fn firmware(tag: &str, flags: u8, platform: u8) -> Vec<u8> {
        let mut out = 1_700_000_000u32.to_be_bytes().to_vec();
        let mut t = tag.as_bytes().to_vec();
        t.resize(32, 0);
        out.extend(t);
        out.extend(b"abcdef12");
        out.extend([flags, platform, 1]);
        out
    }

    fn watch_version(unfaithful: Option<u8>) -> Vec<u8> {
        let mut p = vec![0x01];
        p.extend(firmware("v4.35.0", 0b1100, 18));
        p.extend(firmware("v4.35.0", 0b0001, 18));
        p.extend(7u32.to_be_bytes());
        p.extend(b"obelix\0\0\0");
        p.extend(b"C111A24B0123");
        p.extend([0x24, 0x7B, 0xD4, 0x0A, 0xA3, 0x24]);
        p.extend(1u32.to_be_bytes());
        p.extend(2u32.to_be_bytes());
        p.extend(b"en_US\0");
        p.extend(3u16.to_be_bytes());
        p.extend(Capabilities::from_list(&[Capability::AppRunState, Capability::WeatherApp]).0);
        p.extend(unfaithful);
        p
    }

    #[test]
    fn unfaithful_sits_at_offset_150() {
        let p = watch_version(Some(1));
        assert_eq!(p.len(), 151);
        assert_eq!(p[150], 1);
        let v = WatchVersion::parse(&p).unwrap();
        assert_eq!(v.unfaithful, Some(true));
        assert_eq!(v.running.version(), Some(Version { major: 4, minor: 35, patch: 0 }));
        assert_eq!(v.running.slot(), Some(0));
        assert!(v.recovery.is_recovery());
        assert_eq!(v.board, "obelix");
        assert_eq!(v.serial, "C111A24B0123");
        assert_eq!(v.language.as_deref(), Some("en_US"));
        assert!(v.capabilities.contains(Capability::WeatherApp));
        assert_eq!(v.hardware().revision, "obelix_pvt");
    }

    #[test]
    fn optional_tail_may_be_missing() {
        let v = WatchVersion::parse(&watch_version(None)).unwrap();
        assert_eq!(v.unfaithful, None);
        let short = &watch_version(None)[..134];
        let v = WatchVersion::parse(short).unwrap();
        assert_eq!(v.language, None);
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("v4.35.0-rc1"), Some(Version { major: 4, minor: 35, patch: 0 }));
        assert_eq!(parse_version("3.12"), Some(Version { major: 3, minor: 12, patch: 0 }));
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn phone_version_layout() {
        let p = phone_version_response(OsType::Android, &Capabilities::from_list(&[Capability::AppRunState]));
        assert_eq!(p.len(), 25);
        assert_eq!(&p[..5], &[1, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(&p[9..13], &[0, 0, 0, 2]);
        assert_eq!(&p[13..17], &[2, 4, 4, 2]);
        assert_eq!(p[17], 1);
    }

    #[test]
    fn set_utc_layout() {
        let p = set_utc(0x01020304, -360, "America/Chicago");
        assert_eq!(&p[..7], &[3, 1, 2, 3, 4, 0xFE, 0x98]);
        assert_eq!(p[7], 15);
    }

    #[test]
    fn factory_registry() {
        assert_eq!(factory_request("mfg_color"), [&[0u8, 9][..], b"mfg_color"].concat());
        let value = parse_factory_response(&[1, 4, 0, 0, 0, 37]).unwrap().unwrap();
        assert_eq!(model_number(&value), Some(37));
        assert_eq!(parse_factory_response(&[0xFF]).unwrap(), None);
    }

    #[test]
    fn app_run_state() {
        let u = Uuid::from_u128(0x0102);
        assert_eq!(AppRunState::parse(&AppRunState::Started(u).encode()).unwrap(), AppRunState::Started(u));
        assert_eq!(AppRunState::Request.encode(), vec![3]);
    }

    #[test]
    fn firmware_messages() {
        assert_eq!(firmware_update_start(0, 0x0102_0304), vec![0, 1, 0, 0, 0, 0, 4, 3, 2, 1]);
        assert_eq!(parse_firmware_start_response(&[0, 0x0A, 1]).unwrap(), FirmwareStartStatus::Started);
        assert!(parse_firmware_start_response(&[0, 0x02]).is_err());
    }

    #[test]
    fn ping_pong() {
        assert_eq!(parse_ping(&ping(7)).unwrap(), PingPong::Ping(7));
        assert_eq!(parse_ping(&pong(9)).unwrap(), PingPong::Pong(9));
    }
}
