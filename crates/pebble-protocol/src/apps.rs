//! Watchfaces and watchapps (section 10): the app binary's header, the AppDB entry, app fetch,
//! and app order.

use crate::wire::{err, Reader, Result};
use uuid::Uuid;

/// What the watch needs from an app binary's header (`PBLAPP`), little-endian as the binary is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryHeader {
    pub sdk_version: (u8, u8),
    pub app_version: (u8, u8),
    pub name: String,
    pub company: String,
    pub icon: u32,
    pub flags: u32,
    pub uuid: Uuid,
}

impl BinaryHeader {
    pub const LEN: usize = 120;

    pub fn parse(bytes: &[u8]) -> Result<BinaryHeader> {
        if bytes.len() < Self::LEN {
            return err("the app binary is shorter than its header");
        }
        if &bytes[..8] != b"PBLAPP\0\0" {
            return err("not a Pebble app binary");
        }
        let text = |b: &[u8]| String::from_utf8_lossy(b.split(|&c| c == 0).next().unwrap_or_default()).into_owned();
        let le = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
        Ok(BinaryHeader {
            sdk_version: (bytes[10], bytes[11]),
            app_version: (bytes[12], bytes[13]),
            name: text(&bytes[24..56]),
            company: text(&bytes[56..88]),
            icon: le(88),
            flags: le(96),
            uuid: Uuid::from_slice(&bytes[104..120]).expect("16 bytes"),
        })
    }
}

/// An AppDB entry, keyed by the app's UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppMetadata {
    pub uuid: Uuid,
    pub flags: u32,
    pub icon: u32,
    pub app_version: (u8, u8),
    pub sdk_version: (u8, u8),
    pub name: String,
}

impl AppMetadata {
    pub const LEN: usize = 126;

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::LEN);
        out.extend_from_slice(self.uuid.as_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.icon.to_le_bytes());
        out.extend_from_slice(&[self.app_version.0, self.app_version.1, self.sdk_version.0, self.sdk_version.1]);
        // Background colour and template id, always zero from the official app.
        out.extend_from_slice(&[0, 0]);
        let mut name = [0u8; 96];
        // Leaves room for the terminator; cut at a character boundary.
        let mut end = self.name.len().min(95);
        while !self.name.is_char_boundary(end) {
            end -= 1;
        }
        name[..end].copy_from_slice(&self.name.as_bytes()[..end]);
        out.extend_from_slice(&name);
        out
    }
}

/// "2.3.0" → (2, 3); parts past 255 are capped, missing ones are 0.
pub fn version_bytes(version: &str) -> (u8, u8) {
    let mut parts = version.trim_start_matches('v').split(['.', '-', ' ']).map(|p| p.parse::<u32>().ok());
    let mut part = || parts.next().flatten().unwrap_or(0).min(255) as u8;
    (part(), part())
}

/// The watch asking for an app it does not have (opened, or just installed as a face).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchRequest {
    pub uuid: Uuid,
    /// Names this install in the PutBytes app inits that follow.
    pub app_id: u32,
}

impl FetchRequest {
    pub fn parse(payload: &[u8]) -> Result<FetchRequest> {
        let mut r = Reader::new(payload);
        if r.u8()? != 0x01 {
            return err("not an app fetch request");
        }
        let uuid = r.uuid()?;
        let app_id = r.u32_le()?;
        Ok(FetchRequest { uuid, app_id })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FetchResult {
    Starting = 0x01,
    Busy = 0x02,
    InvalidUuid = 0x03,
    NoData = 0x04,
}

pub fn fetch_response(result: FetchResult) -> Vec<u8> {
    vec![0x01, result as u8]
}

/// The order of the watch's apps and faces; ones not listed keep their place after these.
pub fn reorder(uuids: &[Uuid]) -> Vec<u8> {
    let mut out = vec![0x01, uuids.len().min(255) as u8];
    for u in uuids.iter().take(255) {
        out.extend_from_slice(u.as_bytes());
    }
    out
}

/// The apps built into PebbleOS, by UUID. The nil UUID is the system itself: the launcher,
/// settings, and anything else built in without one of its own.
pub const SYSTEM_APPS: [(&str, &str); 26] = [
    ("00000000-0000-0000-0000-000000000000", "System"),
    ("07e0d9cb-8957-4bf7-9d42-35bf47caadfe", "Settings"),
    ("1f03293d-47af-4f28-b960-f2b02a6dd757", "Music"),
    ("b2cae818-10f8-46df-ad2b-98ad2254a3c1", "Notifications"),
    ("67a32d95-ef69-46d4-a0b9-854cc62f97f9", "Alarms"),
    ("61b22bc8-1e29-460d-a236-3fe409a439ff", "Weather"),
    ("36d8c6ed-4c83-4fa1-a9e2-8f12dc941f8c", "Health"),
    ("fef82c82-7176-4e22-88de-35a3fc18d43f", "Workout"),
    ("18e443ce-38fd-47c8-84d5-6d0c775fbe55", "Watchfaces"),
    ("6c6c6fc2-1912-4d25-8396-3547d1dfac5b", "Calendar"),
    ("0863fc6a-66c5-4f62-ab8a-82ed00a98b5d", "Send Text"),
    ("42a07217-5491-4267-904a-d02a156752b6", "Reminders"),
    ("af760190-bfc0-11e4-bb52-0800200c9a66", "Missed Calls"),
    ("426ccd53-b380-4d83-8d06-9893de3477ce", "Timeline"),
    ("79c76b48-6111-4e80-8deb-3119eebef33e", "Timeline (Upcoming)"),
    ("daae3686-bff6-4ba5-921b-262f847bb6e8", "Timeline (Past)"),
    ("8f3c8686-31a1-4f5f-91f5-01600c9bdc59", "TicToc"),
    ("3af858c3-16cb-4561-91e7-f1ad2df8725f", "Kickstart"),
    ("2220d805-cf9a-4e12-92b9-5ca778aff6bb", "Quiet Time"),
    ("d0f12e6c-97eb-2287-a2f5-115dfaa1d168", "Backlight"),
    ("d4f7be63-97e6-4952-b265-dd4bce11c155", "Motion Backlight"),
    ("88c28c12-7f81-42db-aaa6-14ccef6f27e5", "Airplane Mode"),
    ("5cc5a4e8-150d-4ec3-9afc-965e3fdec3c3", "Touch"),
    ("d9c0d758-54bd-45b1-99ac-2a3a889350c9", "Clear Notifications"),
    ("de6da17f-1a10-4725-adbb-1efc22e43f04", "Quick Launch (Nothing)"),
    ("ed429c16-f674-4220-95da-454f303f15e2", "Phone Notifications"),
];

pub fn system_app_name(uuid: Uuid) -> Option<&'static str> {
    SYSTEM_APPS.iter().find(|(u, _)| u.parse::<Uuid>().ok() == Some(uuid)).map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_apps() {
        assert!(SYSTEM_APPS.iter().all(|(u, _)| u.parse::<Uuid>().is_ok()));
        assert_eq!(system_app_name(Uuid::nil()), Some("System"));
        assert_eq!(system_app_name("36D8C6ED-4C83-4FA1-A9E2-8F12DC941F8C".parse().unwrap()), Some("Health"));
        assert_eq!(system_app_name(Uuid::from_u128(1)), None);
    }

    fn header() -> Vec<u8> {
        let mut b = vec![0u8; 120];
        b[..8].copy_from_slice(b"PBLAPP\0\0");
        b[8..14].copy_from_slice(&[16, 1, 5, 110, 2, 3]);
        b[24..37].copy_from_slice(b"NBA Watchface");
        b[56..68].copy_from_slice(b"jcontreras29");
        b[96..100].copy_from_slice(&1353u32.to_le_bytes());
        b[104..120].copy_from_slice(&Uuid::parse_str("6e16dae9-de6a-4541-b383-df4333562cd8").unwrap().into_bytes());
        b
    }

    #[test]
    fn binary_header() {
        let h = BinaryHeader::parse(&header()).unwrap();
        assert_eq!((h.sdk_version, h.app_version, h.flags, h.icon), ((5, 110), (2, 3), 1353, 0));
        assert_eq!((h.name.as_str(), h.company.as_str()), ("NBA Watchface", "jcontreras29"));
        assert_eq!(h.uuid.to_string(), "6e16dae9-de6a-4541-b383-df4333562cd8");
        assert!(BinaryHeader::parse(&header()[..100]).is_err());
    }

    #[test]
    fn metadata_layout() {
        let m = AppMetadata {
            uuid: Uuid::from_bytes([7; 16]),
            flags: 1353,
            icon: 2,
            app_version: (2, 3),
            sdk_version: (5, 110),
            name: "é".repeat(60),
        };
        let b = m.encode();
        assert_eq!(b.len(), AppMetadata::LEN);
        assert_eq!(&b[16..30], &[0x49, 0x05, 0, 0, 2, 0, 0, 0, 2, 3, 5, 110, 0, 0]);
        // 47 two-byte characters fit before the terminator.
        assert_eq!(b[30 + 94], 0);
        assert_eq!(String::from_utf8(b[30..30 + 94].to_vec()).unwrap(), "é".repeat(47));
    }

    #[test]
    fn fetch_and_order() {
        let u = Uuid::from_bytes([3; 16]);
        let mut p = vec![1];
        p.extend_from_slice(u.as_bytes());
        p.extend_from_slice(&0x0102_0304u32.to_le_bytes());
        assert_eq!(FetchRequest::parse(&p).unwrap(), FetchRequest { uuid: u, app_id: 0x0102_0304 });
        assert_eq!(fetch_response(FetchResult::NoData), vec![1, 4]);
        assert_eq!(reorder(&[u]).len(), 18);
        assert_eq!(version_bytes("2.3.0"), (2, 3));
        assert_eq!(version_bytes("1"), (1, 0));
        assert_eq!(version_bytes("v3.400"), (3, 255));
    }
}
