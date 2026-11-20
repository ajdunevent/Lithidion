//! Turning the phone's notifications into watch notifications (PROTOCOL.md section 12).

use super::desktop::Posted;
use pebble_protocol::timeline::{self, attr, icon, Action, ActionKind, Attribute, Item, Kind, Layout};
use std::collections::HashMap;
use uuid::Uuid;

/// The watch's actions on a phone notification, by action id.
pub const ACTION_DISMISS: u8 = 0;
pub const ACTION_MUTE: u8 = 1;

/// An icon and a colour for the kind of app, from its category hint or its name.
fn look(p: &Posted) -> (u32, u32) {
    let category = p.category.as_deref().unwrap_or("");
    let name = format!("{} {}", p.app_id, p.app_name).to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if category.starts_with("call") || has(&["calls", "dialer", "telephony"]) {
        return (icon::MISSED_CALL, 0x00AA55);
    }
    if category.starts_with("im") || has(&["chatty", "chats", "messages", "sms", "signal", "telegram", "fractal", "matrix", "flare"]) {
        return (icon::GENERIC_SMS, 0x0055AA);
    }
    if category.starts_with("email") || has(&["mail", "geary", "evolution", "thunderbird"]) {
        return (icon::GENERIC_EMAIL, 0xAA5500);
    }
    if has(&["calendar", "karlender"]) {
        return (icon::CALENDAR, 0xAA0000);
    }
    if has(&["clock", "alarm"]) {
        return (icon::ALARM_CLOCK, 0x555555);
    }
    if category.starts_with("device") || category.starts_with("network") || p.urgency >= 2 {
        return (icon::GENERIC_WARNING, 0xFF5500);
    }
    (icon::NOTIFICATION_GENERIC, 0x5555AA)
}

/// The watch's copy of a phone notification, with its picture's shape when it has one to show.
pub fn item(id: Uuid, p: &Posted, silent: bool, now: i64, picture: Option<u8>) -> Item {
    let (tiny, color) = look(p);
    let title = if p.title.is_empty() { p.app_name.as_str() } else { p.title.as_str() };
    let mut attributes = vec![Attribute::text(attr::TITLE, title, timeline::TITLE_MAX)];
    if !p.body.is_empty() {
        attributes.push(Attribute::text(attr::BODY, &p.body, timeline::BODY_MAX));
    }
    if !p.title.is_empty() && p.app_name != p.title {
        attributes.push(Attribute::text(attr::SUBTITLE, &p.app_name, timeline::TITLE_MAX));
    }
    attributes.push(Attribute::text(attr::APP_NAME, &p.app_name, timeline::TITLE_MAX));
    attributes.push(Attribute::icon(attr::TINY_ICON, tiny));
    attributes.push(Attribute::color(attr::BACKGROUND_COLOR, color));
    if let Some(aspect) = picture {
        attributes.push(Attribute::u8(attr::IMAGE_ASPECT_RATIO, aspect));
    }
    let action = |id, kind, label: &str| Action { id, kind, attributes: vec![Attribute::text(attr::TITLE, label, timeline::TITLE_MAX)] };
    Item {
        id,
        parent: timeline::PHONE_NOTIFICATIONS,
        timestamp: now as i32,
        duration: 0,
        kind: Kind::Notification,
        flags: if silent { timeline::flags::SILENT } else { 0 },
        status: 0,
        layout: Layout::GenericNotification,
        attributes,
        actions: vec![
            action(ACTION_DISMISS, ActionKind::Dismiss, "Dismiss"),
            action(ACTION_MUTE, ActionKind::Generic, &format!("Mute {}", p.app_name)),
        ],
    }
}

/// What the watch has of the phone's notifications, to update or dismiss them.
#[derive(Default)]
pub struct Sent {
    by_phone: HashMap<u32, Uuid>,
    items: HashMap<Uuid, (Item, u32, String)>,
}

/// Kept for the most recent ones only; older ones are left to the watch.
const KEEP: usize = 200;

impl Sent {
    /// The watch id for a notification: the one it replaces, if the watch has that.
    pub fn id_for(&self, p: &Posted) -> Uuid {
        let old = if p.replaces != 0 { p.replaces } else { p.id };
        self.by_phone.get(&old).copied().unwrap_or_else(Uuid::new_v4)
    }

    pub fn record(&mut self, phone_id: u32, item: Item, app_id: &str) {
        if self.items.len() >= KEEP {
            self.by_phone.clear();
            self.items.clear();
        }
        self.by_phone.insert(phone_id, item.id);
        self.items.insert(item.id, (item, phone_id, app_id.to_string()));
    }

    /// The watch's copy marked dismissed, when the phone closed it.
    pub fn closed(&mut self, phone_id: u32) -> Option<Item> {
        let id = self.by_phone.remove(&phone_id)?;
        let (mut item, _, _) = self.items.remove(&id)?;
        item.status |= timeline::status::DISMISSED;
        Some(item)
    }

    /// The phone's id and the app of a watch notification.
    pub fn origin(&self, id: Uuid) -> Option<(u32, String)> {
        self.items.get(&id).map(|(_, phone, app)| (*phone, app.clone()))
    }
}

/// Notifications that arrived while no watch was connected, for a watch that comes back soon:
/// a connection that dropped for a moment, or a walk out of range. Older ones are no longer news.
#[derive(Default)]
pub struct Missed {
    items: Vec<MissedItem>,
}

pub struct MissedItem {
    pub posted: Posted,
    pub time: i64,
    /// It would have alerted (the app is On, not just Always).
    pub alert: bool,
    /// Its notification history entry.
    pub log: Option<i64>,
}

/// How long a missed notification stays worth sending.
pub const MISSED_FOR: i64 = 10 * 60;
const MISSED_MAX: usize = 50;

impl Missed {
    pub fn push(&mut self, item: MissedItem) {
        // One that replaces an earlier one takes its place.
        let replaced = item.posted.replaces;
        if replaced != 0 {
            self.items.retain(|m| m.posted.id != replaced);
        }
        self.items.push(item);
        if self.items.len() > MISSED_MAX {
            self.items.remove(0);
        }
    }

    /// The desktop closed it, so it is no longer worth sending.
    pub fn closed(&mut self, phone_id: u32) {
        self.items.retain(|m| m.posted.id != phone_id);
    }

    /// Those still recent at `now`, oldest first; the queue is emptied.
    pub fn take(&mut self, now: i64) -> Vec<MissedItem> {
        let mut out: Vec<MissedItem> = self.items.drain(..).filter(|m| now - m.time <= MISSED_FOR).collect();
        out.sort_by_key(|m| m.time);
        out
    }
}

/// The watch pref that makes it deliver silent notifications without alerting.
pub const RESPECT_SILENCE_KEY: &[u8] = b"notifRespectPhoneSilence\0";

/// The icon a desktop entry names, if the entry is visible here.
pub fn desktop_icon(app_id: &str) -> Option<String> {
    use gio::prelude::*;
    let id = format!("{app_id}.desktop");
    let info = gio::AppInfo::all().into_iter().find(|a| a.id().as_deref() == Some(id.as_str()))?;
    let text = info.icon()?.to_string()?.to_string();
    // A themed icon with fallbacks reads ". GThemedIcon name fallback…".
    Some(text.strip_prefix(". GThemedIcon ").and_then(|t| t.split(' ').next()).unwrap_or(&text).to_string())
}

/// An icon name is kept as it is; an image file is scaled to 64 px and copied to the cache, as
/// apps often pass files that do not last.
pub fn cache_icon(app_id: &str, source: &str) -> Option<String> {
    let path = source.strip_prefix("file://").unwrap_or(source);
    if !path.starts_with('/') {
        return Some(source.to_string());
    }
    let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(path, 64, 64, true).ok()?;
    let dir = glib::user_cache_dir().join("lithidion").join("icons");
    std::fs::create_dir_all(&dir).ok()?;
    let name: String = app_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let out = dir.join(format!("{name}.png"));
    pixbuf.savev(&out, "png", &[]).ok()?;
    Some(out.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posted(id: u32, replaces: u32) -> Posted {
        Posted {
            id,
            replaces,
            app_id: "sm.puri.Chatty".into(),
            app_name: "Chats".into(),
            title: "Al".into(),
            body: "hey".into(),
            urgency: 1,
            category: Some("im.received".into()),
            icon: None,
            picture: None,
        }
    }

    #[test]
    fn missed_keep_only_recent_open_ones() {
        let mut m = Missed::default();
        let at =
            |id: u32, replaces: u32, time: i64| MissedItem { posted: Posted { replaces, ..posted(id, 0) }, time, alert: true, log: None };
        m.push(at(1, 0, 1000));
        m.push(at(2, 0, 1500));
        m.push(at(3, 0, 1800));
        m.push(at(4, 3, 1900)); // replaces 3
        m.closed(2);
        let ids: Vec<u32> = m.take(1000 + MISSED_FOR + 1).iter().map(|x| x.posted.id).collect();
        // 1 is too old, 2 was closed, 3 was replaced by 4.
        assert_eq!(ids, vec![4]);
        assert!(m.take(2000).is_empty());
    }

    #[test]
    fn items_and_replacement() {
        let mut sent = Sent::default();
        let p = posted(5, 0);
        let id = sent.id_for(&p);
        let it = item(id, &p, true, 1000, None);
        assert_eq!(it.flags, timeline::flags::SILENT);
        assert!(!it.attributes.iter().any(|a| a.id == attr::IMAGE_ASPECT_RATIO));
        assert!(item(id, &p, true, 1000, Some(12)).attributes.contains(&Attribute::u8(attr::IMAGE_ASPECT_RATIO, 12)));
        assert!(it.attributes.contains(&Attribute::icon(attr::TINY_ICON, icon::GENERIC_SMS)));
        sent.record(5, it, &p.app_id);
        // An update replacing 5 keeps the watch id.
        assert_eq!(sent.id_for(&posted(6, 5)), id);
        assert_eq!(sent.origin(id), Some((5, "sm.puri.Chatty".to_string())));
        let closed = sent.closed(5).unwrap();
        assert_eq!(closed.status, timeline::status::DISMISSED);
        assert!(sent.closed(5).is_none());
    }
}
