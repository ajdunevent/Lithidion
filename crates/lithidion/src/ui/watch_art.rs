//! A drawn watch showing the phone's current time, tinted by the watch's finish.

use gtk::cairo;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

#[derive(Clone)]
pub struct WatchArt {
    pub area: gtk::DrawingArea,
    body: Rc<Cell<Finish>>,
    screen: Rc<RefCell<Screen>>,
    /// The screen's width over its height.
    ratio: Rc<Cell<f64>>,
}

/// The drawing's own units: wide enough for the widest screen with its bezel and buttons.
const WIDTH: f64 = 122.0;
const HEIGHT: f64 = 130.0;
const CENTRE: f64 = WIDTH / 2.0;
const SCREEN_H: f64 = 98.0;
const BEZEL: f64 = 10.0;

/// Screen proportions by platform: 200×228 on emery, square for the round watches (drawn as
/// squares), 144×168 elsewhere.
pub fn screen_ratio(platform: Option<pebble_protocol::hardware::AppPlatform>) -> f64 {
    use pebble_protocol::hardware::AppPlatform::*;
    match platform {
        Some(Emery) => 200.0 / 228.0,
        Some(Chalk | Gabbro) => 1.0,
        _ => 144.0 / 168.0,
    }
}

/// What the screen shows: the time, or the running app's store picture.
#[derive(Default)]
struct Screen {
    url: Option<String>,
    picture: Option<(cairo::ImageSurface, bool)>,
}

/// A texture as a cairo surface; GDK's default memory format matches cairo's ARGB32.
fn surface(texture: &gtk::gdk::Texture) -> Option<cairo::ImageSurface> {
    let (w, h) = (texture.width(), texture.height());
    let stride = cairo::Format::ARgb32.stride_for_width(w as u32).ok()?;
    let mut data = vec![0u8; stride as usize * h as usize];
    texture.download(&mut data, stride as usize);
    cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, w, h, stride).ok()
}

type Rgb = (f64, f64, f64);

/// Named finish colours. No official values exist; these are approximations.
const COLOURS: &[(&str, u32)] = &[
    ("Black", 0x232326),
    ("White", 0xf2f2ef),
    ("Red", 0xb3202e),
    ("Orange", 0xfa4a36),
    ("Pink", 0xf4a6b8),
    ("Silver", 0xc8c9cc),
    ("Gunmetal", 0x4a4c50),
    ("Fly Blue", 0x3d7fd9),
    ("Fresh Green", 0x7ccf4a),
    ("Hot Pink", 0xe8418e),
    ("Rose Gold", 0xd4a089),
    ("Gold", 0xc9a66b),
    ("Charcoal", 0x36383b),
    ("Gray", 0x8a8d91),
    ("Sorbet Green", 0xb7e08a),
    ("Turquoise", 0x3fc1c0),
    ("Blue", 0x2f5fa8),
];

fn rgb(hex: u32) -> Rgb {
    let c = |shift: u32| ((hex >> shift) & 0xff) as f64 / 255.0;
    (c(16), c(8), c(0))
}

/// How a finish is drawn: the body, the buttons when they differ, and a rainbow edge.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Finish {
    body: Rgb,
    buttons: Option<Rgb>,
    rainbow: bool,
}

/// Reads a finish name from the model table: one colour ("Fly Blue"), a body and a second colour
/// ("Black/Red", "Charcoal Red"), or a colour with a rainbow edge ("Silver Rainbow").
fn finish(name: Option<&str>) -> Finish {
    let mut rest = name.unwrap_or("").trim();
    let mut found = Vec::new();
    while !rest.is_empty() {
        let next = COLOURS.iter().filter(|(n, _)| rest.starts_with(n)).max_by_key(|(n, _)| n.len());
        match next {
            Some((n, hex)) => {
                found.push(rgb(*hex));
                rest = &rest[n.len()..];
            }
            None if rest.starts_with("Rainbow") => {
                rest = &rest["Rainbow".len()..];
            }
            None => break,
        }
        rest = rest.trim_start_matches(['/', ' ']);
    }
    Finish {
        body: found.first().copied().unwrap_or(rgb(0x232326)),
        buttons: found.get(1).copied(),
        rainbow: name.is_some_and(|n| n.contains("Rainbow")),
    }
}

/// Relative luminance of an sRGB colour, 0 to 1.
fn luminance(r: f64, g: f64, b: f64) -> f64 {
    let lin = |c: f64| if c <= 0.039_28 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

impl WatchArt {
    /// `width` in pixels; the height follows the watch's proportions.
    pub fn new(width: i32, with_time: bool) -> WatchArt {
        let height = (width as f64 * HEIGHT / WIDTH).round() as i32;
        let area = gtk::DrawingArea::builder().content_width(width).content_height(height).valign(gtk::Align::Center).build();
        area.set_accessible_role(gtk::AccessibleRole::Img);
        let body = Rc::new(Cell::new(finish(None)));
        let b = body.clone();
        let screen: Rc<RefCell<Screen>> = Rc::default();
        let sc = screen.clone();
        let ratio = Rc::new(Cell::new(144.0 / 168.0));
        let ra = ratio.clone();
        area.set_draw_func(move |area, cr, w, h| {
            let s = (w as f64 / WIDTH).min(h as f64 / HEIGHT);
            cr.scale(s, s);
            // The screen keeps the watch's own proportions, with the body around it.
            let sw = SCREEN_H * ra.get();
            let (sx, bw) = (CENTRE - sw / 2.0, sw + 2.0 * BEZEL);
            let bx = CENTRE - bw / 2.0;
            let Finish { body: (r, g, bl), buttons, rainbow } = b.get();
            // A soft halo in the text colour when the body is close to the window's colour, as a
            // black watch is on a dark window.
            let fg = area.color();
            let fg_lum = luminance(fg.red() as f64, fg.green() as f64, fg.blue() as f64);
            let window_lum = if fg_lum > 0.5 { 0.02 } else { 0.95 };
            if (luminance(r, g, bl) - window_lum).abs() < 0.3 {
                for (width, alpha) in [(9.0, 0.035), (6.0, 0.05), (3.5, 0.07), (1.5, 0.12)] {
                    cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, alpha);
                    cr.set_line_width(width);
                    rounded(cr, bx, 4.0, bw, 122.0, 22.0);
                    let _ = cr.stroke();
                }
            }
            // Buttons in the finish's second colour; otherwise lighter on a dark window, where the
            // usual dark grey disappears.
            if let Some((r, g, b)) = buttons {
                cr.set_source_rgb(r, g, b);
            } else if window_lum < 0.5 {
                cr.set_source_rgb(0.45, 0.44, 0.48);
            } else {
                cr.set_source_rgb(0.24, 0.22, 0.27);
            }
            // Select sits level with the screen's centre, and Back level with Up.
            let (left, right) = (bx - 4.0, bx + bw - 1.0);
            for (x, y) in [(left, 32.5), (right, 32.5), (right, 56.5), (right, 80.5)] {
                rounded(cr, x, y, 5.0, 17.0, 2.0);
            }
            let _ = cr.fill();
            cr.set_source_rgb(r, g, bl);
            rounded(cr, bx, 4.0, bw, 122.0, 22.0);
            let _ = cr.fill();
            if rainbow {
                let gradient = cairo::LinearGradient::new(bx, 4.0, bx + bw, 126.0);
                for (k, hex) in [0xe53935u32, 0xfb8c00, 0xfdd835, 0x43a047, 0x1e88e5, 0x8e24aa].iter().enumerate() {
                    let (r, g, b) = rgb(*hex);
                    gradient.add_color_stop_rgb(k as f64 / 5.0, r, g, b);
                }
                let _ = cr.set_source(&gradient);
                cr.set_line_width(3.0);
                rounded(cr, bx + 1.5, 5.5, bw - 3.0, 119.0, 20.5);
                let _ = cr.stroke();
            }
            cr.set_source_rgb(0.96, 0.96, 0.96);
            rounded(cr, sx, 16.0, sw, SCREEN_H, 6.0);
            let _ = cr.fill();
            if let Some((picture, face)) = sc.borrow().picture.as_ref() {
                // A face's screenshot fills the screen; an app's icon sits in its middle.
                let (pw, ph) = (picture.width() as f64, picture.height() as f64);
                let (scale, (x, y)) = if *face {
                    let k = (sw / pw).max(SCREEN_H / ph);
                    (k, (CENTRE - pw * k / 2.0, 65.0 - ph * k / 2.0))
                } else {
                    let k = 44.0 / pw.max(ph);
                    (k, (CENTRE - pw * k / 2.0, 65.0 - ph * k / 2.0))
                };
                let _ = cr.save();
                rounded(cr, sx, 16.0, sw, SCREEN_H, 6.0);
                cr.clip();
                cr.translate(x, y);
                cr.scale(scale, scale);
                let _ = cr.set_source_surface(picture, 0.0, 0.0);
                cr.source().set_filter(cairo::Filter::Good);
                let _ = cr.paint();
                let _ = cr.restore();
                return;
            }
            if with_time {
                let now = crate::host::Zone::now();
                cr.set_source_rgb(0.14, 0.12, 0.19);
                cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
                cr.set_font_size(24.0);
                let time = super::locale::watch_time(now.timestamp());
                if let Ok(ext) = cr.text_extents(&time) {
                    cr.move_to(CENTRE - ext.width() / 2.0 - ext.x_bearing(), 66.0);
                    let _ = cr.show_text(&time);
                }
                cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
                cr.set_font_size(10.0);
                cr.set_source_rgb(0.37, 0.36, 0.39);
                let date = super::locale::day_and_month(now.timestamp());
                if let Ok(ext) = cr.text_extents(&date) {
                    cr.move_to(CENTRE - ext.width() / 2.0 - ext.x_bearing(), 84.0);
                    let _ = cr.show_text(&date);
                }
            }
        });
        {
            // The halo follows the light or dark style.
            let weak = area.downgrade();
            adw::StyleManager::default().connect_dark_notify(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.queue_draw();
                }
            });
        }
        if with_time {
            let weak = area.downgrade();
            glib::timeout_add_seconds_local(15, move || match weak.upgrade() {
                Some(a) => {
                    a.queue_draw();
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            });
        }
        WatchArt { area, body, screen, ratio }
    }

    /// Shows the running app's store picture (`face` for a watchface's screenshot), or the time
    /// with `None`.
    pub fn set_screen(&self, picture: Option<(&str, bool)>) {
        let url = picture.map(|(u, _)| u.to_string());
        if self.screen.borrow().url == url {
            return;
        }
        *self.screen.borrow_mut() = Screen { url: url.clone(), picture: None };
        self.area.queue_draw();
        let (Some(url), Some((_, face))) = (url, picture) else { return };
        let (screen, area) = (Rc::downgrade(&self.screen), self.area.downgrade());
        super::images::texture(&url.clone(), move |texture| {
            let (Some(screen), Some(area)) = (screen.upgrade(), area.upgrade()) else { return };
            let mut s = screen.borrow_mut();
            if s.url.as_deref() == Some(url.as_str()) {
                s.picture = texture.as_ref().and_then(surface).map(|p| (p, face));
                area.queue_draw();
            }
        });
    }

    pub fn set_ratio(&self, ratio: f64) {
        if (self.ratio.get() - ratio).abs() > f64::EPSILON {
            self.ratio.set(ratio);
            self.area.queue_draw();
        }
    }

    pub fn set_finish(&self, finish_name: Option<&str>, label: &str) {
        self.body.set(finish(finish_name));
        self.area.update_property(&[gtk::accessible::Property::Label(label)]);
        self.area.queue_draw();
    }
}

#[cfg(test)]
mod tests {
    use super::{finish, rgb};

    /// The screen showing a face's screenshot and an app's icon, saved to `/tmp/art-screens.png`:
    /// `xvfb-run cargo test -- --ignored art_screens`.
    #[test]
    #[ignore]
    fn art_screens() {
        use gtk::prelude::*;
        adw::init().unwrap();
        // Stand-ins: a 144×168 face with a frame, and a round 48×48 icon.
        let picture = |w: i32, h: i32, round: bool| {
            let s = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h).unwrap();
            let cr = gtk::cairo::Context::new(&s).unwrap();
            if round {
                cr.arc(w as f64 / 2.0, h as f64 / 2.0, w as f64 / 2.0, 0.0, 6.3);
                cr.set_source_rgb(0.9, 0.3, 0.2);
                let _ = cr.fill();
            } else {
                cr.set_source_rgb(0.1, 0.3, 0.6);
                let _ = cr.paint();
                cr.set_source_rgb(1.0, 0.8, 0.2);
                cr.set_line_width(6.0);
                cr.rectangle(3.0, 3.0, w as f64 - 6.0, h as f64 - 6.0);
                let _ = cr.stroke();
            }
            drop(cr);
            s
        };
        let row = gtk::Box::builder().spacing(12).margin_start(12).margin_end(12).margin_top(12).margin_bottom(12).build();
        use pebble_protocol::hardware::AppPlatform::{Basalt, Emery};
        for (face, p, platform) in
            [(true, picture(144, 168, false), Basalt), (true, picture(200, 228, false), Emery), (false, picture(48, 48, true), Emery)]
        {
            let art = super::WatchArt::new(96, true);
            art.set_finish(Some("Black/Gray"), "Watch");
            art.set_ratio(super::screen_ratio(Some(platform)));
            art.screen.borrow_mut().picture = Some((p, face));
            row.append(&art.area);
            std::mem::forget(art);
        }
        let window = adw::Window::builder().content(&row).build();
        window.present();
        let ctx = glib::MainContext::default();
        let end = std::time::Instant::now() + std::time::Duration::from_millis(600);
        while std::time::Instant::now() < end {
            ctx.iteration(false);
        }
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
        let texture = window.native().unwrap().renderer().unwrap().render_texture(snapshot.to_node().unwrap(), None);
        texture.save_to_png("/tmp/art-screens.png").unwrap();
    }

    #[test]
    fn every_finish_has_its_colours() {
        for model in 1..=43 {
            let Some((_, name)) = pebble_protocol::hardware::model(model) else { continue };
            let f = finish(Some(name));
            let colours = name.split(['/', ' ']).filter(|w| *w != "Rainbow").count();
            // Two-word colour names count as one.
            let single = ["Fly Blue", "Fresh Green", "Hot Pink", "Rose Gold", "Sorbet Green"].iter().any(|n| name.contains(n));
            assert_eq!(f.buttons.is_some(), colours > 1 && !single, "{name}");
        }
        assert_eq!(finish(Some("Black/Red")).buttons, Some(rgb(0xb3202e)));
        assert_eq!(finish(Some("Rose Gold")).body, rgb(0xd4a089));
        assert!(finish(Some("Black Rainbow")).rainbow);
        assert_eq!(finish(Some("Charcoal Red")).body, rgb(0x36383b));
    }

    /// Renders black, red, and silver watches in the light and dark styles to a PNG in the temp
    /// folder: `xvfb-run cargo test -- --ignored watch_art_layout`.
    #[test]
    #[ignore]
    fn watch_art_layout() {
        use adw::prelude::*;
        adw::init().unwrap();
        for (dark, name) in [(false, "art-light.png"), (true, "art-dark.png")] {
            adw::StyleManager::default().set_color_scheme(if dark { adw::ColorScheme::ForceDark } else { adw::ColorScheme::ForceLight });
            let row = gtk::Box::builder().spacing(12).margin_start(12).margin_end(12).margin_top(12).margin_bottom(12).build();
            for finish in ["Black/Gray", "Black/Red", "Silver/Blue", "Fly Blue", "Rose Gold", "Black Rainbow", "White Turquoise"] {
                let art = super::WatchArt::new(70, true);
                art.set_finish(Some(finish), finish);
                row.append(&art.area);
            }
            let window = adw::Window::builder().content(&row).build();
            window.present();
            let ctx = glib::MainContext::default();
            let end = std::time::Instant::now() + std::time::Duration::from_millis(800);
            while std::time::Instant::now() < end {
                ctx.iteration(false);
            }
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
            let texture = window.native().unwrap().renderer().unwrap().render_texture(snapshot.to_node().unwrap(), None);
            texture.save_to_png(std::env::temp_dir().join(name)).unwrap();
            window.close();
        }
    }
}
