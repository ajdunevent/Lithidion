//! An app's screenshots: side by side when they all fit, else one at a time with faded peeks
//! of the ones before and after, and arrows. Clicking a peek moves to it.

use super::images;
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const WIDTH: i32 = 180;
const HEIGHT: i32 = 210;
const SPACING: i32 = 12;
/// A peek is at most this wide, and is left out below the smaller width.
const PEEK_MAX: i32 = 108;
const PEEK_MIN: i32 = 24;

/// A screenshot, with a spinner on a placeholder until it arrives and a broken-image icon if it
/// does not.
fn shot(url: &str, width: i32, height: i32, fit: gtk::ContentFit) -> gtk::Overlay {
    let picture = gtk::Picture::builder().width_request(width).height_request(height).content_fit(fit).build();
    let frame =
        gtk::Overlay::builder().child(&picture).overflow(gtk::Overflow::Hidden).css_classes(["screenshot", "shot-placeholder"]).build();
    let spinner = gtk::Spinner::builder().spinning(true).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
    let broken = gtk::Image::builder()
        .icon_name("lithidion-image-missing-symbolic")
        .pixel_size(32)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .tooltip_text("The screenshot could not be loaded")
        .css_classes(["dim-label"])
        .visible(false)
        .build();
    frame.add_overlay(&spinner);
    frame.add_overlay(&broken);
    let frame_weak = frame.downgrade();
    images::load_then(&picture, url, move |arrived| {
        spinner.set_spinning(false);
        spinner.set_visible(false);
        broken.set_visible(!arrived);
        if let (true, Some(f)) = (arrived, frame_weak.upgrade()) {
            f.remove_css_class("shot-placeholder");
        }
    });
    frame
}

/// A peek at a neighbouring screenshot, fading out towards the outer edge.
struct Peek {
    widget: gtk::Overlay,
    picture: gtk::Picture,
}

impl Peek {
    fn new(fade: &str) -> Peek {
        let picture = gtk::Picture::builder()
            .height_request(HEIGHT * 4 / 5)
            .content_fit(gtk::ContentFit::Cover)
            .valign(gtk::Align::Center)
            .css_classes(["screenshot"])
            .build();
        let widget = gtk::Overlay::builder()
            .child(&picture)
            .overflow(gtk::Overflow::Hidden)
            .valign(gtk::Align::Center)
            .css_classes(["screenshot", "shot-placeholder"])
            .build();
        let shade = gtk::Box::builder().css_classes([fade]).can_target(false).build();
        widget.add_overlay(&shade);
        widget.set_cursor_from_name(Some("pointer"));
        Peek { widget, picture }
    }

    fn show(&self, url: Option<&str>, width: i32) {
        match url {
            Some(u) if width >= PEEK_MIN => {
                self.picture.set_width_request(width);
                images::load(&self.picture, u);
                self.widget.set_opacity(1.0);
                self.widget.set_can_target(true);
            }
            // Kept in place, invisible, so the current screenshot stays centred.
            _ => {
                self.picture.set_width_request(width.max(0));
                self.picture.set_paintable(gtk::gdk::Paintable::NONE);
                self.widget.set_opacity(0.0);
                self.widget.set_can_target(false);
            }
        }
    }
}

pub fn view(urls: Vec<String>) -> gtk::Widget {
    let urls = Rc::new(urls);
    let n = urls.len() as i32;

    // All at once.
    let row = gtk::Box::builder().spacing(SPACING).halign(gtk::Align::Center).build();
    for u in urls.iter() {
        row.append(&shot(u, WIDTH, HEIGHT, gtk::ContentFit::Contain));
    }

    // One at a time.
    let current = gtk::Stack::builder().transition_type(gtk::StackTransitionType::SlideLeftRight).hhomogeneous(true).build();
    for (i, u) in urls.iter().enumerate() {
        current.add_named(&shot(u, WIDTH, HEIGHT, gtk::ContentFit::Contain), Some(&i.to_string()));
    }
    let before = Peek::new("fade-start");
    let after = Peek::new("fade-end");
    let (before_widget, after_widget) = (before.widget.clone(), after.widget.clone());
    let strip = gtk::Box::builder().spacing(SPACING).halign(gtk::Align::Center).build();
    strip.append(&before.widget);
    strip.append(&current);
    strip.append(&after.widget);
    let arrow = |icon: &str, tip: &str, align: gtk::Align| {
        gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tip)
            .halign(align)
            .valign(gtk::Align::Center)
            .margin_start(4)
            .margin_end(4)
            .css_classes(["circular", "osd"])
            .build()
    };
    let previous = arrow("lithidion-go-previous-symbolic", "Previous", gtk::Align::Start);
    let next = arrow("lithidion-go-next-symbolic", "Next", gtk::Align::End);
    let frame = gtk::Overlay::builder().child(&strip).halign(gtk::Align::Center).build();
    frame.add_overlay(&previous);
    frame.add_overlay(&next);
    let position = gtk::Label::builder().css_classes(["caption", "dim-label"]).build();
    let single = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    single.append(&frame);
    single.append(&position);

    let modes = gtk::Stack::builder().hhomogeneous(false).vhomogeneous(false).interpolate_size(true).build();
    modes.add_named(&row, Some("row"));
    modes.add_named(&single, Some("single"));

    let index = Rc::new(Cell::new(0usize));
    let peek_width = Rc::new(Cell::new(PEEK_MAX));
    let show = {
        let (urls, index, peek_width) = (urls.clone(), index.clone(), peek_width.clone());
        let (current, previous, next, position) = (current.clone(), previous.clone(), next.clone(), position.clone());
        let peeks = Rc::new((before, after));
        Rc::new(move |i: usize| {
            index.set(i);
            current.set_visible_child_name(&i.to_string());
            let w = peek_width.get();
            peeks.0.show(i.checked_sub(1).and_then(|j| urls.get(j)).map(String::as_str), w);
            peeks.1.show(urls.get(i + 1).map(String::as_str), w);
            previous.set_visible(i > 0);
            next.set_visible(i + 1 < urls.len());
            position.set_label(&format!("{} of {}", i + 1, urls.len()));
        })
    };
    {
        let (show, index) = (show.clone(), index.clone());
        previous.connect_clicked(move |_| show(index.get().saturating_sub(1)));
    }
    {
        let (show, index, last) = (show.clone(), index.clone(), urls.len().saturating_sub(1));
        next.connect_clicked(move |_| show((index.get() + 1).min(last)));
    }
    for (widget, step) in [(&before_widget, -1i64), (&after_widget, 1)] {
        let click = gtk::GestureClick::new();
        let (show, index, last) = (show.clone(), index.clone(), urls.len().saturating_sub(1) as i64);
        click.connect_released(move |_, _, _, _| show((index.get() as i64 + step).clamp(0, last) as usize));
        widget.add_controller(click);
    }

    // The width decides the mode; a zero-height area reports it.
    let ruler = gtk::DrawingArea::builder().hexpand(true).height_request(0).build();
    let mode = Rc::new(RefCell::new(String::new()));
    {
        let (modes, show, index, peek_width, mode) = (modes.clone(), show.clone(), index.clone(), peek_width.clone(), mode.clone());
        ruler.connect_resize(move |_, width, _| {
            let all = n * WIDTH + (n - 1).max(0) * SPACING;
            let wanted = if width >= all { "row" } else { "single" };
            let peek = ((width - WIDTH - 2 * SPACING) / 2).clamp(0, PEEK_MAX);
            let changed = *mode.borrow() != wanted || peek != peek_width.get();
            if !changed {
                return;
            }
            *mode.borrow_mut() = wanted.to_string();
            peek_width.set(peek);
            // Not during the allocation that reported the width.
            let (modes, show, index) = (modes.clone(), show.clone(), index.get());
            glib::idle_add_local_once(move || {
                modes.set_visible_child_name(wanted);
                show(index);
            });
        });
    }
    show(0);
    // Asks for no width of its own, so the ruler sees the width there is, not the row's.
    let holder = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&modes)
        .build();
    let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    outer.append(&ruler);
    outer.append(&holder);
    outer.upcast()
}

#[cfg(test)]
mod tests {
    /// Renders the viewer at a phone's width and a desktop's to PNGs in the temp folder:
    /// `xvfb-run cargo test -- --ignored screenshot_layout --test-threads=1`.
    #[test]
    #[ignore]
    fn screenshot_layout() {
        use adw::prelude::*;
        let rt = tokio::runtime::Runtime::new().unwrap();
        crate::net::set_runtime(rt.handle().clone());
        adw::init().unwrap();
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("style.css"));
        gtk::style_context_add_provider_for_display(&gtk::gdk::Display::default().unwrap(), &provider, 800);
        let urls: Vec<String> = ["6xhPvoSTpKxReqdERV0O", "VNXKnW3aSfTaB55MjYLs", "missing", "VNXKnW3aSfTaB55MjYLs"]
            .iter()
            .map(|id| format!("https://assets2.rebble.io/exact/144x168/{id}"))
            .collect();
        for (width, step) in [(360, 1), (360, 0), (900, 0)] {
            let view = super::view(urls.clone());
            let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).margin_start(12).margin_end(12).build();
            column.append(&view);
            let window = adw::Window::builder().default_width(width).default_height(320).content(&column).build();
            window.present();
            let ctx = glib::MainContext::default();
            let end = std::time::Instant::now() + std::time::Duration::from_secs(4);
            let mut clicked = false;
            while std::time::Instant::now() < end {
                ctx.iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(10));
                if step == 1 && !clicked && std::time::Instant::now() + std::time::Duration::from_secs(2) > end {
                    // Press "Next" once.
                    let mut stack = vec![view.clone()];
                    while let Some(x) = stack.pop() {
                        if let Some(b) = x.downcast_ref::<gtk::Button>() {
                            if b.tooltip_text().as_deref() == Some("Next") {
                                b.emit_clicked();
                                clicked = true;
                            }
                        }
                        let mut c = x.first_child();
                        while let Some(ch) = c {
                            c = ch.next_sibling();
                            stack.push(ch);
                        }
                    }
                }
            }
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let snapshot = gtk::Snapshot::new();
            let (w, h) = (window.width(), window.height());
            paintable.snapshot(&snapshot, w as f64, h as f64);
            let node = snapshot.to_node().unwrap();
            let texture = window.native().unwrap().renderer().unwrap().render_texture(node, None);
            let path = std::env::temp_dir().join(format!("shots-{width}-{step}.png"));
            texture.save_to_png(&path).unwrap();
            eprintln!("{}", path.display());
            window.close();
        }
    }
}
