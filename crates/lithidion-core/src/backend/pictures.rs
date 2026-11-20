//! Pictures in notifications (PROTOCOL.md section 22): a notification that carries a picture is
//! sent with its shape, and the watch then asks for the picture at the size it will show it.
//! Kept in memory for the most recent notifications only.

use super::session::Protocol;
use pebble_protocol::endpoint;
use pebble_protocol::imaging::{self, Rgba};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Pictures kept, newest last.
const KEEP: usize = 32;

static ENABLED: AtomicBool = AtomicBool::new(false);
/// The connected watch says it shows pictures. Others are never sent the picture's attribute:
/// firmware that does not know an attribute can write out of bounds verifying the item.
static SHOWN: AtomicBool = AtomicBool::new(false);
static CACHE: Mutex<VecDeque<(Uuid, Arc<Rgba>)>> = Mutex::new(VecDeque::new());

fn cache() -> std::sync::MutexGuard<'static, VecDeque<(Uuid, Arc<Rgba>)>> {
    CACHE.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if !on {
        cache().clear();
    }
}

pub fn set_watch(shows_pictures: bool) {
    SHOWN.store(shows_pictures, Ordering::Relaxed);
}

/// Keeps a notification's picture for the watch, and gives the shape to send with it; `None`
/// when there is no picture to show.
pub fn attach(item: Uuid, picture: Option<&Arc<Rgba>>) -> Option<u8> {
    let mut cache = cache();
    cache.retain(|(id, _)| *id != item);
    let picture = picture.filter(|_| ENABLED.load(Ordering::Relaxed) && SHOWN.load(Ordering::Relaxed))?;
    if cache.len() >= KEEP {
        cache.pop_front();
    }
    cache.push_back((item, picture.clone()));
    Some(imaging::aspect_sixteenths(picture.width, picture.height))
}

pub fn forget(item: Uuid) {
    cache().retain(|(id, _)| *id != item);
}

/// Answers the watch's request for an image. Album art is not served.
pub fn request(protocol: &Protocol, payload: &[u8]) {
    let Ok(Some(r)) = imaging::parse_request(payload) else { return };
    let picture = match (r.kind, r.item) {
        (imaging::TYPE_NOTIFICATION, Some(item)) if r.format == imaging::FORMAT_4BIT_PALETTE => {
            cache().iter().find(|(id, _)| *id == item).map(|(_, p)| p.clone())
        }
        (imaging::TYPE_NOTIFICATION, _) => None,
        (kind, _) => {
            let _ = protocol.send(endpoint::IMAGING, &imaging::unsupported(r.token, kind));
            return;
        }
    };
    let Some(picture) = picture else {
        let _ = protocol.send(endpoint::IMAGING, &imaging::no_image(r.token, r.kind));
        return;
    };
    let image = imaging::encode(&picture, r.width, r.height);
    tracing::debug!(width = image.width, height = image.height, colors = image.palette.len(), "notification picture");
    for chunk in imaging::response(r.token, r.kind, &image) {
        if protocol.send(endpoint::IMAGING, &chunk).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_only_when_on_and_shown() {
        let pic = Arc::new(Rgba { width: 4, height: 3, pixels: vec![0; 48] });
        let id = Uuid::new_v4();
        set_enabled(true);
        set_watch(false);
        assert_eq!(attach(id, Some(&pic)), None);
        set_watch(true);
        assert_eq!(attach(id, Some(&pic)), Some(12));
        assert_eq!(attach(id, None), None);
        assert!(cache().is_empty());
        set_enabled(false);
        assert_eq!(attach(id, Some(&pic)), None);
    }
}
