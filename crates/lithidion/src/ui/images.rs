//! Store images, fetched once per run and kept in memory.

use crate::net;
use adw::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::Semaphore;

thread_local! {
    static CACHE: RefCell<HashMap<String, Option<gtk::gdk::Texture>>> = RefCell::default();
}

/// At most a few downloads at once, so a long list does not swamp the connection.
fn permits() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(6))
}

async fn fetch(url: String) -> anyhow::Result<Vec<u8>> {
    let _permit = permits().acquire().await?;
    let response = net::client().get(&url).timeout(Duration::from_secs(30)).send().await?.error_for_status()?;
    Ok(response.bytes().await?.to_vec())
}

/// Shows the image at `url` in `picture` once it has arrived; a picture reused for another
/// image meanwhile is left alone.
pub fn load(picture: &gtk::Picture, url: &str) {
    load_then(picture, url, |_| {});
}

/// As [`load`], then calls `done` with whether the image arrived.
pub fn load_then(picture: &gtk::Picture, url: &str, done: impl Fn(bool) + 'static) {
    picture.set_widget_name(url);
    if let Some(found) = CACHE.with(|c| c.borrow().get(url).cloned()) {
        picture.set_paintable(found.as_ref());
        done(found.is_some());
        return;
    }
    picture.set_paintable(gtk::gdk::Paintable::NONE);
    let (picture, url) = (picture.downgrade(), url.to_string());
    glib::spawn_future_local(async move {
        let bytes = net::run(fetch(url.clone())).await;
        let texture = bytes.ok().and_then(|b| gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(b)).ok());
        CACHE.with(|c| c.borrow_mut().insert(url.clone(), texture.clone()));
        if let Some(p) = picture.upgrade() {
            if p.widget_name() == url {
                p.set_paintable(texture.as_ref());
                done(texture.is_some());
            }
        }
    });
}

/// The image at `url`, from memory or fetched once; `done` gets `None` when it cannot be had.
pub fn texture(url: &str, done: impl FnOnce(Option<gtk::gdk::Texture>) + 'static) {
    if let Some(found) = CACHE.with(|c| c.borrow().get(url).cloned()) {
        done(found);
        return;
    }
    let url = url.to_string();
    glib::spawn_future_local(async move {
        let bytes = net::run(fetch(url.clone())).await;
        let texture = bytes.ok().and_then(|b| gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(b)).ok());
        CACHE.with(|c| c.borrow_mut().insert(url, texture.clone()));
        done(texture);
    });
}

/// A thumbnail of `size` pixels: the image when there is one, else `icon`.
pub fn thumbnail(url: Option<&str>, size: i32, icon: &str) -> gtk::Widget {
    let frame = gtk::Overlay::builder()
        .width_request(size)
        .height_request(size)
        .valign(gtk::Align::Center)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["app-thumb"])
        .build();
    let fallback = gtk::Image::builder().icon_name(icon).pixel_size(size / 2).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
    frame.set_child(Some(&fallback));
    if let Some(url) = url {
        let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Contain).width_request(size).height_request(size).build();
        frame.add_overlay(&picture);
        load(&picture, url);
        let fb = fallback.clone();
        picture.connect_paintable_notify(move |p| fb.set_visible(p.paintable().is_none()));
    }
    frame.upcast()
}
