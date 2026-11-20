//! Timeline items (section 12): notifications, pins, and reminders as the watch stores them, and
//! the actions the watch asks the phone to carry out. Integers are little-endian, as in BlobDB.

use crate::wire::{err, Reader, Result};
use uuid::Uuid;

/// The parent of notifications from the phone, as the official Android app uses it.
pub const PHONE_NOTIFICATIONS: Uuid = Uuid::from_u128(0xed429c16_f674_4220_95da_454f303f15e2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Notification = 1,
    Pin = 2,
    Reminder = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Layout {
    GenericPin = 1,
    CalendarPin = 2,
    GenericReminder = 3,
    GenericNotification = 4,
    CommNotification = 5,
}

/// Header flags.
pub mod flags {
    pub const VISIBLE: u8 = 1 << 0;
    pub const FLOATING: u8 = 1 << 1;
    pub const ALL_DAY: u8 = 1 << 2;
    /// Silenced on the phone: the watch stores it without alerting when its "respect phone Do
    /// Not Disturb" preference is on.
    pub const SILENT: u8 = 1 << 6;
}

/// Header status bits.
pub mod status {
    pub const READ: u8 = 1 << 0;
    pub const DELETED: u8 = 1 << 1;
    pub const ACTIONED: u8 = 1 << 2;
    pub const DISMISSED: u8 = 1 << 4;
}

pub mod attr {
    pub const TITLE: u8 = 1;
    pub const SUBTITLE: u8 = 2;
    pub const BODY: u8 = 3;
    pub const TINY_ICON: u8 = 4;
    pub const LARGE_ICON: u8 = 6;
    pub const SENDER: u8 = 12;
    pub const BACKGROUND_COLOR: u8 = 28;
    pub const APP_NAME: u8 = 30;
    /// A picture's height over width in sixteenths; the watch then asks for the picture.
    pub const IMAGE_ASPECT_RATIO: u8 = 52;
}

/// System icons, by resource id (the official app's `TimelineIcon`).
pub mod icon {
    pub const NOTIFICATION_GENERIC: u32 = 1;
    pub const MISSED_CALL: u32 = 2;
    pub const REMINDER: u32 = 3;
    pub const ALARM_CLOCK: u32 = 13;
    pub const GENERIC_EMAIL: u32 = 19;
    pub const CALENDAR: u32 = 21;
    pub const GENERIC_WARNING: u32 = 28;
    pub const RESULT_DELETED: u32 = 43;
    pub const GENERIC_SMS: u32 = 45;
    pub const RESULT_MUTE: u32 = 46;
    pub const RESULT_SENT: u32 = 47;
    pub const RESULT_DISMISSED: u32 = 51;
    pub const GENERIC_CONFIRMATION: u32 = 55;
    pub const RESULT_FAILED: u32 = 62;
    pub const INCOMING_PHONE_CALL: u32 = 78;
    pub const SETTINGS: u32 = 83;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ActionKind {
    Generic = 0x02,
    Response = 0x03,
    Dismiss = 0x04,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub id: u8,
    pub value: Vec<u8>,
}

impl Attribute {
    /// Text cut to `max` bytes at a character boundary, ending in "…" when cut.
    pub fn text(id: u8, s: &str, max: usize) -> Attribute {
        let value = if s.len() <= max {
            s.as_bytes().to_vec()
        } else {
            let mut end = max.saturating_sub(3);
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &s[..end]).into_bytes()
        };
        Attribute { id, value }
    }

    pub fn u8(id: u8, v: u8) -> Attribute {
        Attribute { id, value: vec![v] }
    }

    pub fn u32(id: u8, v: u32) -> Attribute {
        Attribute { id, value: v.to_le_bytes().to_vec() }
    }

    /// A system icon: resource ids are offset into the system's range.
    pub fn icon(id: u8, resource: u32) -> Attribute {
        Attribute::u32(id, resource | 0x8000_0000)
    }

    /// A colour as the watch stores it: two bits each of alpha, red, green, and blue.
    pub fn color(id: u8, rgb: u32) -> Attribute {
        let c = |shift: u32| (((rgb >> shift) & 0xFF) / 85) as u8;
        Attribute { id, value: vec![0b1100_0000 | (c(16) << 4) | (c(8) << 2) | c(0)] }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.id);
        out.extend_from_slice(&(self.value.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.value);
    }
}

/// Text attribute limits, in bytes.
pub const TITLE_MAX: usize = 64;
pub const BODY_MAX: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub id: u8,
    pub kind: ActionKind,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: Uuid,
    pub parent: Uuid,
    pub timestamp: i32,
    /// Minutes.
    pub duration: u16,
    pub kind: Kind,
    pub flags: u8,
    pub status: u8,
    pub layout: Layout,
    pub attributes: Vec<Attribute>,
    pub actions: Vec<Action>,
}

impl Item {
    pub fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        for a in &self.attributes {
            a.encode(&mut payload);
        }
        for action in &self.actions {
            payload.push(action.id);
            payload.push(action.kind as u8);
            payload.push(action.attributes.len() as u8);
            for a in &action.attributes {
                a.encode(&mut payload);
            }
        }
        let mut out = Vec::with_capacity(46 + payload.len());
        out.extend_from_slice(self.id.as_bytes());
        out.extend_from_slice(self.parent.as_bytes());
        out.extend_from_slice(&self.timestamp.to_le_bytes());
        out.extend_from_slice(&self.duration.to_le_bytes());
        out.extend_from_slice(&[self.kind as u8, self.flags, self.status, self.layout as u8]);
        out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        out.push(self.attributes.len() as u8);
        out.push(self.actions.len() as u8);
        out.extend_from_slice(&payload);
        out
    }
}

/// The watch asking the phone to carry out one of an item's actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invoke {
    pub item: Uuid,
    pub action: u8,
}

pub fn parse_invoke(payload: &[u8]) -> Result<Invoke> {
    let mut r = Reader::new(payload);
    if r.u8()? != 0x02 {
        return err("not an action invocation");
    }
    let item = r.uuid()?;
    let action = r.u8()?;
    Ok(Invoke { item, action })
}

/// What the watch shows after an action: an icon and a word ("Dismissed").
pub fn action_response(item: Uuid, ok: bool, icon: u32, text: &str) -> Vec<u8> {
    let mut out = vec![0x11];
    out.extend_from_slice(item.as_bytes());
    out.push(if ok { 0 } else { 1 });
    out.push(2);
    Attribute::icon(attr::LARGE_ICON, icon).encode(&mut out);
    Attribute::text(attr::SUBTITLE, text, TITLE_MAX).encode(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_layout() {
        let item = Item {
            id: Uuid::from_bytes([1; 16]),
            parent: PHONE_NOTIFICATIONS,
            timestamp: 0x0102_0304,
            duration: 0,
            kind: Kind::Notification,
            flags: flags::SILENT,
            status: 0,
            layout: Layout::GenericNotification,
            attributes: vec![Attribute::text(attr::TITLE, "Hi", TITLE_MAX)],
            actions: vec![Action {
                id: 0,
                kind: ActionKind::Dismiss,
                attributes: vec![Attribute::text(attr::TITLE, "Dismiss", TITLE_MAX)],
            }],
        };
        let b = item.encode();
        assert_eq!(&b[16..32], PHONE_NOTIFICATIONS.as_bytes());
        assert_eq!(&b[32..42], &[4, 3, 2, 1, 0, 0, 1, 0x40, 0, 4]);
        // Payload: title (3 + 2) and one action (3 + 3 + 7).
        assert_eq!(&b[42..46], &[18, 0, 1, 1]);
        assert_eq!(b.len(), 46 + 18);
        assert_eq!(&b[46..51], &[1, 2, 0, b'H', b'i']);
        assert_eq!(&b[51..54], &[0, 4, 1]);
    }

    #[test]
    fn attributes() {
        assert_eq!(Attribute::color(0, 0xFF5500).value, vec![0b1111_0100]);
        assert_eq!(Attribute::icon(4, 45).value, (45u32 | 0x8000_0000).to_le_bytes().to_vec());
        let long = Attribute::text(1, &"é".repeat(40), 64);
        assert!(long.value.len() <= 64);
        assert!(String::from_utf8(long.value).unwrap().ends_with('…'));
        assert_eq!(Attribute::text(1, "short", 64).value, b"short");
    }

    #[test]
    fn actions() {
        let u = Uuid::from_bytes([9; 16]);
        let mut p = vec![2];
        p.extend_from_slice(u.as_bytes());
        p.extend_from_slice(&[1, 0]);
        assert_eq!(parse_invoke(&p).unwrap(), Invoke { item: u, action: 1 });
        let r = action_response(u, true, icon::RESULT_DISMISSED, "Dismissed");
        assert_eq!(&r[..1], &[0x11]);
        assert_eq!(&r[17..19], &[0, 2]);
    }
}
