//! The Health tab: today, last night, heart rate, the week, and recent activities for the active
//! watch, with its wearer's profile and the watch's health settings a page away.

use super::state::{ago, State};
use crate::backend::health::{Day, DayDetail, Night, Range, SleepBlock, View};
use crate::backend::{Command, Event};
use crate::host::Zone as Local;
use crate::settings::{HealthSettings, Interval};
use adw::prelude::*;
use chrono::TimeZone;
use pebble_protocol::health::{Session, SessionKind};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

/// "12,345".
fn grouped(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "7 h 12 min", "45 min".
fn duration(seconds: u32) -> String {
    let minutes = (seconds + 30) / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

fn distance(meters: u32, imperial: bool) -> String {
    if imperial {
        format!("{:.1} mi", meters as f64 / 1609.344)
    } else {
        format!("{:.1} km", meters as f64 / 1000.0)
    }
}

fn clock(unix: i64) -> String {
    super::locale::time(unix)
}

fn day_name(date: chrono::NaiveDate) -> String {
    let today = Local::now().date_naive();
    match (today - date).num_days() {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        _ => super::locale::weekday(date),
    }
}

fn kind_name(k: SessionKind) -> &'static str {
    match k {
        SessionKind::Walk => "Walk",
        SessionKind::Run => "Run",
        SessionKind::Workout => "Workout",
        _ => "Sleep",
    }
}

fn kind_icon(k: SessionKind) -> &'static str {
    match k {
        SessionKind::Run => "lithidion-run-symbolic",
        SessionKind::Workout => "lithidion-workout-symbolic",
        _ => "lithidion-walk-symbolic",
    }
}

fn value_row(title: &str) -> (adw::ActionRow, gtk::Label) {
    let row = adw::ActionRow::builder().title(title).build();
    let value = gtk::Label::builder().css_classes(["dim-label", "numeric"]).build();
    row.add_suffix(&value);
    (row, value)
}

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

fn foreground(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, alpha: f64) {
    let color = area.color();
    cr.set_source_rgba(color.red() as f64, color.green() as f64, color.blue() as f64, alpha * color.alpha() as f64);
}

/// Bars, a value above each when there is room for one, and labels under them: one a bar, or
/// one for each group of bars.
struct Chart {
    widget: gtk::Box,
    area: gtk::DrawingArea,
    top: gtk::Box,
    bottom: gtk::Box,
    bars: Rc<RefCell<Vec<f64>>>,
    highlight: Rc<Cell<Option<usize>>>,
}

impl Chart {
    fn new(height: i32) -> Chart {
        let row = || gtk::Box::builder().homogeneous(true).build();
        let (top, bottom) = (row(), row());
        let bars: Rc<RefCell<Vec<f64>>> = Rc::default();
        let highlight: Rc<Cell<Option<usize>>> = Rc::default();
        let area = gtk::DrawingArea::builder().content_height(height).hexpand(true).build();
        {
            let (bars, highlight) = (bars.clone(), highlight.clone());
            area.set_draw_func(move |area, cr, width, height| {
                let bars = bars.borrow();
                let slot = width as f64 / bars.len().max(1) as f64;
                let bar = (slot * if bars.len() > 12 { 0.65 } else { 0.5 }).min(28.0);
                for (i, &v) in bars.iter().enumerate() {
                    foreground(area, cr, if highlight.get() == Some(i) { 0.85 } else { 0.4 });
                    let h = (v * height as f64).max(if v > 0.0 { 3.0 } else { 1.0 });
                    let x = slot * i as f64 + (slot - bar) / 2.0;
                    let r = (bar / 2.0).min(6.0).min(h / 2.0);
                    let y = height as f64 - h;
                    // Rounded at the top only.
                    cr.new_sub_path();
                    cr.arc(x + bar - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
                    cr.line_to(x + bar, height as f64);
                    cr.line_to(x, height as f64);
                    cr.arc(x + r, y + r, r, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
                    cr.close_path();
                    let _ = cr.fill();
                }
            });
        }
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).margin_top(12).margin_bottom(12).build();
        widget.append(&top);
        widget.append(&area);
        widget.append(&bottom);
        Chart { widget, area, top, bottom, bars, highlight }
    }

    /// `bars` from 0 to 1. `values`: one a bar, or none. `labels`: text and tooltip.
    fn set(&self, bars: Vec<f64>, values: &[String], labels: &[(String, String)], highlight: Option<usize>) {
        clear(&self.top);
        clear(&self.bottom);
        self.top.set_visible(!values.is_empty());
        for v in values {
            self.top.append(&gtk::Label::builder().label(v).css_classes(["caption", "dim-label", "numeric"]).build());
        }
        let per_bar = labels.len() == bars.len();
        for (i, (text, tip)) in labels.iter().enumerate() {
            let strong = per_bar && highlight == Some(i);
            let label = gtk::Label::builder()
                .label(text)
                .css_classes(if strong { vec!["caption-heading"] } else { vec!["caption", "dim-label"] })
                .xalign(if per_bar { 0.5 } else { 0.0 })
                .build();
            if !tip.is_empty() {
                label.set_tooltip_text(Some(tip));
            }
            self.bottom.append(&label);
        }
        *self.bars.borrow_mut() = bars;
        self.highlight.set(highlight);
        self.area.queue_draw();
    }

    fn connect_bar(&self, f: impl Fn(usize) + 'static) {
        let click = gtk::GestureClick::new();
        let (bars, area) = (self.bars.clone(), self.area.clone());
        click.connect_released(move |_, _, x, _| {
            let n = bars.borrow().len();
            if n > 0 && area.width() > 0 {
                f(((x / area.width() as f64 * n as f64) as usize).min(n - 1));
            }
        });
        self.area.add_controller(click);
        self.area.set_cursor_from_name(Some("pointer"));
    }
}

/// Readings across a day, joined where they are close together.
struct Line {
    area: gtk::DrawingArea,
    /// Points from 0 to 1 both ways.
    points: Rc<RefCell<Vec<(f64, f64)>>>,
}

/// Readings further apart are not joined.
const LINE_GAP: f64 = 30.0 / 1440.0;

impl Line {
    fn new() -> Line {
        let points: Rc<RefCell<Vec<(f64, f64)>>> = Rc::default();
        let area = gtk::DrawingArea::builder().content_height(72).hexpand(true).build();
        {
            let points = points.clone();
            area.set_draw_func(move |area, cr, width, height| {
                let points = points.borrow();
                let (w, h) = (width as f64, height as f64 - 4.0);
                let at = |&(x, y): &(f64, f64)| (x * w, 2.0 + (1.0 - y) * h);
                foreground(area, cr, 0.85);
                cr.set_line_width(2.0);
                cr.set_line_join(gtk::cairo::LineJoin::Round);
                cr.set_line_cap(gtk::cairo::LineCap::Round);
                let mut i = 0;
                while i < points.len() {
                    let mut j = i + 1;
                    while j < points.len() && points[j].0 - points[j - 1].0 <= LINE_GAP {
                        j += 1;
                    }
                    let (x, y) = at(&points[i]);
                    if j - i == 1 {
                        cr.arc(x, y, 2.0, 0.0, std::f64::consts::TAU);
                        let _ = cr.fill();
                    } else {
                        cr.move_to(x, y);
                        for p in &points[i + 1..j] {
                            let (x, y) = at(p);
                            cr.line_to(x, y);
                        }
                        let _ = cr.stroke();
                    }
                    i = j;
                }
            });
        }
        Line { area, points }
    }

    /// Readings over a span of time, scaled between their lowest and highest.
    fn set(&self, readings: &[(i64, u8)], (from, to): (i64, i64)) {
        let lo = readings.iter().map(|r| r.1).min().unwrap_or(0) as f64 - 2.0;
        let hi = readings.iter().map(|r| r.1).max().unwrap_or(0) as f64 + 2.0;
        let span = (to - from).max(1) as f64;
        *self.points.borrow_mut() = readings.iter().map(|&(t, v)| ((t - from) as f64 / span, (v as f64 - lo) / (hi - lo))).collect();
        self.area.queue_draw();
    }
}

/// Sleep blocks, and the span they are drawn across.
type Blocks = Rc<RefCell<(Vec<SleepBlock>, (i64, i64))>>;

/// Light and deep sleep from a night's start to its end.
struct SleepBar {
    widget: gtk::Box,
    area: gtk::DrawingArea,
    start: gtk::Label,
    end: gtk::Label,
    blocks: Blocks,
}

impl SleepBar {
    fn new() -> SleepBar {
        let blocks: Blocks = Rc::default();
        let area = gtk::DrawingArea::builder().content_height(28).hexpand(true).build();
        {
            let blocks = blocks.clone();
            area.set_draw_func(move |area, cr, width, height| {
                let (blocks, (from, to)) = &*blocks.borrow();
                let span = (to - from).max(1) as f64;
                for b in blocks {
                    foreground(area, cr, if b.deep { 0.85 } else { 0.3 });
                    let x = (b.start - from) as f64 / span * width as f64;
                    let w = ((b.end - b.start) as f64 / span * width as f64).max(1.0);
                    cr.rectangle(x, 0.0, w, height as f64);
                    let _ = cr.fill();
                }
            });
        }
        let label = |x: f32| gtk::Label::builder().xalign(x).hexpand(true).css_classes(["caption", "dim-label"]).build();
        let (start, end) = (label(0.0), label(1.0));
        let times = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        times.append(&start);
        times.append(&end);
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).margin_top(12).margin_bottom(12).build();
        widget.append(&area);
        widget.append(&times);
        SleepBar { widget, area, start, end, blocks }
    }

    fn set(&self, night: &Night, blocks: &[SleepBlock]) {
        let span = (night.start(), night.end());
        self.start.set_label(&clock(span.0));
        self.end.set_label(&clock(span.1));
        *self.blocks.borrow_mut() = (blocks.to_vec(), span);
        self.area.queue_draw();
    }
}

/// Labels under a day's charts: midnight, 6, noon, and 18, each under the first of its six hours.
fn hour_labels(bounds: (i64, i64)) -> Vec<(String, String)> {
    (0..4).map(|q| (clock(bounds.0 + q * 6 * 3600), String::new())).collect()
}

fn hour_axis(bounds: (i64, i64)) -> gtk::Box {
    let b = gtk::Box::builder().homogeneous(true).build();
    for (text, _) in hour_labels(bounds) {
        b.append(&gtk::Label::builder().label(text).xalign(0.0).css_classes(["caption", "dim-label"]).build());
    }
    b
}

/// A card with a heading, and a figure at the heading's end.
fn card(title: &str, content: &impl IsA<gtk::Widget>) -> (gtk::Box, gtk::Label) {
    let figure = gtk::Label::builder().hexpand(true).xalign(1.0).css_classes(["caption", "dim-label", "numeric"]).build();
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    heading.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["heading"]).build());
    heading.append(&figure);
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).margin_start(12).margin_end(12).margin_top(10).build();
    b.append(&heading);
    b.append(content);
    (b, figure)
}

/// The first letter, as the charts label a day or a month.
fn initial(name: &str) -> String {
    name.chars().next().map(String::from).unwrap_or_default()
}

fn short(n: u32) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

pub struct HealthPage {
    pub widget: gtk::Stack,
    state: Rc<State>,
    nav: OnceCell<adw::NavigationView>,
    steps: gtk::Label,
    updated: gtk::Label,
    distance: gtk::Label,
    active: gtk::Label,
    calories: gtk::Label,
    sleep: adw::ActionRow,
    sleep_value: gtk::Label,
    heart: adw::PreferencesGroup,
    heart_latest: adw::ActionRow,
    heart_latest_value: gtk::Label,
    heart_range: gtk::Label,
    heart_resting: gtk::Label,
    range: Cell<Range>,
    /// The bars shown, for the day a tapped bar is.
    shown: RefCell<Vec<Day>>,
    steps_chart: Chart,
    steps_average: gtk::Label,
    sleep_chart: Chart,
    sleep_average: gtk::Label,
    activities: adw::PreferencesGroup,
    activity_rows: RefCell<Vec<adw::ActionRow>>,
    off: adw::ActionRow,
}

impl HealthPage {
    pub fn new(state: &Rc<State>) -> Rc<HealthPage> {
        let empty = adw::StatusPage::builder().icon_name("lithidion-health-symbolic").title("No Profile").vexpand(true).build();

        let steps = gtk::Label::builder().css_classes(["title-1", "numeric"]).build();
        let steps_caption = gtk::Label::builder().label("steps today").css_classes(["dim-label"]).build();
        let updated = gtk::Label::builder().css_classes(["caption", "dim-label"]).margin_top(6).build();
        let hero = gtk::Box::builder().orientation(gtk::Orientation::Vertical).margin_top(12).build();
        hero.append(&steps);
        hero.append(&steps_caption);
        hero.append(&updated);

        let off = adw::ActionRow::builder().title("Activity Tracking Is Off").visible(false).build();
        off.add_prefix(&gtk::Image::from_icon_name("lithidion-info-symbolic"));

        let today = adw::PreferencesGroup::builder().title("Today").build();
        let refresh = gtk::Button::builder()
            .icon_name("arrow-counterclockwise-symbolic")
            .tooltip_text("Get the Latest From the Watch")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        today.set_header_suffix(Some(&refresh));
        let (distance_row, distance) = value_row("Distance");
        let (active_row, active) = value_row("Active Time");
        let (calories_row, calories) = value_row("Calories");
        let details = adw::ActionRow::builder().title("Details").activatable(true).build();
        details.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
        today.add(&off);
        today.add(&distance_row);
        today.add(&active_row);
        today.add(&calories_row);
        today.add(&details);

        let sleep_group = adw::PreferencesGroup::builder().title("Sleep").build();
        let (sleep, sleep_value) = value_row("Last Night");
        sleep.set_activatable(true);
        sleep.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
        sleep_group.add(&sleep);

        let heart = adw::PreferencesGroup::builder().title("Heart Rate").build();
        let (heart_latest, heart_latest_value) = value_row("Latest");
        let (range_row, heart_range) = value_row("Today");
        let (resting_row, heart_resting) = value_row("Resting");
        heart.add(&heart_latest);
        heart.add(&range_row);
        heart.add(&resting_row);

        let week = adw::PreferencesGroup::builder().title("Trends").build();
        let ranges = gtk::Box::builder().css_classes(["linked"]).valign(gtk::Align::Center).build();
        let toggles: Vec<(Range, gtk::ToggleButton)> = [(Range::Week, "Week"), (Range::Month, "Month"), (Range::Year, "Year")]
            .into_iter()
            .map(|(r, label)| (r, gtk::ToggleButton::builder().label(label).css_classes(["small"]).active(r == Range::Week).build()))
            .collect();
        for (_, t) in &toggles {
            if let Some((_, first)) = toggles.first().filter(|(_, f)| f != t) {
                t.set_group(Some(first));
            }
            ranges.append(t);
        }
        week.set_header_suffix(Some(&ranges));
        let steps_chart = Chart::new(96);
        let sleep_chart = Chart::new(96);
        let charts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["card"]).build();
        let (steps_card, steps_average) = card("Steps", &steps_chart.widget);
        let (sleep_card, sleep_average) = card("Sleep", &sleep_chart.widget);
        charts.append(&steps_card);
        charts.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        charts.append(&sleep_card);
        week.add(&charts);

        let activities = adw::PreferencesGroup::builder().title("Activities").build();

        let settings_group = adw::PreferencesGroup::new();
        let settings_row = adw::ActionRow::builder().title("Health Settings").activatable(true).build();
        settings_row.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
        settings_group.add(&settings_row);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        for w in [hero.upcast_ref::<gtk::Widget>(), today.upcast_ref(), sleep_group.upcast_ref(), heart.upcast_ref()] {
            column.append(w);
        }
        column.append(&week);
        column.append(&activities);
        column.append(&settings_group);
        let clamp = adw::Clamp::builder().maximum_size(640).child(&column).build();
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();
        let widget = gtk::Stack::new();
        widget.add_named(&empty, Some("empty"));
        widget.add_named(&scroller, Some("health"));

        let page = Rc::new(HealthPage {
            widget,
            state: state.clone(),
            nav: OnceCell::new(),
            steps,
            updated,
            distance,
            active,
            calories,
            sleep,
            sleep_value,
            heart,
            heart_latest,
            heart_latest_value,
            heart_range,
            heart_resting,
            range: Cell::new(Range::Week),
            shown: RefCell::default(),
            steps_chart,
            steps_average,
            sleep_chart,
            sleep_average,
            activities,
            activity_rows: RefCell::default(),
            off,
        });
        {
            let state = state.clone();
            refresh.connect_clicked(move |_| state.send(Command::RefreshHealth));
        }
        {
            let (state, weak) = (state.clone(), Rc::downgrade(&page));
            settings_row.connect_activated(move |_| {
                let Some(page) = weak.upgrade() else { return };
                if let Some(nav) = page.nav.get() {
                    nav.push(&settings_page(&state));
                }
            });
        }
        for (range, toggle) in toggles {
            let weak = Rc::downgrade(&page);
            toggle.connect_toggled(move |t| {
                if let (true, Some(page)) = (t.is_active(), weak.upgrade()) {
                    page.range.set(range);
                    page.refresh_trend(page.state.health.borrow().as_deref());
                }
            });
        }
        for chart in [&page.steps_chart, &page.sleep_chart] {
            let weak = Rc::downgrade(&page);
            chart.connect_bar(move |i| {
                let Some(page) = weak.upgrade() else { return };
                let date = page.shown.borrow().get(i).map(|d| d.date);
                if let (Some(date), false) = (date, page.range.get() == Range::Year) {
                    page.open_day(date);
                }
            });
        }
        for row in [&details, &page.sleep] {
            let weak = Rc::downgrade(&page);
            row.connect_activated(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.open_day(Local::now().date_naive());
                }
            });
        }
        {
            // Holds the page: nothing else keeps it once the window is built.
            let page = page.clone();
            state.listen(move |e| {
                match e {
                    Event::Health(view) => page.show(view.as_deref()),
                    Event::HealthTrend(t) if t.range == page.range.get() && t.range != Range::Week => page.show_trend(t.range, &t.bars),
                    _ => {}
                }
                true
            });
        }
        page.show(state.health.borrow().as_deref());
        page
    }

    pub fn set_nav(&self, nav: &adw::NavigationView) {
        let _ = self.nav.set(nav.clone());
    }

    fn open_day(&self, date: chrono::NaiveDate) {
        if let Some(nav) = self.nav.get() {
            nav.push(&day_page(&self.state, date));
        }
    }

    /// The week comes with the summary; a month or a year is asked for.
    fn refresh_trend(&self, view: Option<&View>) {
        match (self.range.get(), view) {
            (Range::Week, Some(v)) => self.show_trend(Range::Week, &v.week),
            (Range::Week, None) => {}
            (range, _) => self.state.send(Command::HealthTrend(range)),
        }
    }

    fn show_trend(&self, range: Range, bars: &[Day]) {
        let last = bars.len().checked_sub(1);
        let labels: Vec<(String, String)> = match range {
            Range::Week => bars.iter().map(|d| (initial(&super::locale::weekday_short(d.date)), day_name(d.date))).collect(),
            Range::Month => bars.chunks(6).map(|c| (super::locale::month_day(c[0].date), String::new())).collect(),
            Range::Year => bars.iter().map(|d| (initial(&super::locale::month_short(d.date)), super::locale::month_year(d.date))).collect(),
        };
        // Each chart, its average, its value, and the value as a bar and as the average show it.
        type Series<'a> = (&'a Chart, &'a gtk::Label, fn(&Day) -> u32, fn(u32) -> String, fn(u32) -> String);
        let charts: [Series; 2] = [
            (&self.steps_chart, &self.steps_average, |d| d.steps, short, grouped),
            (&self.sleep_chart, &self.sleep_average, |d| d.sleep, |s| format!("{:.1}h", s as f64 / 3600.0), duration),
        ];
        for (chart, average, value, text, long) in charts {
            let max = bars.iter().map(value).max().unwrap_or(0).max(1) as f64;
            let values: Vec<String> = if range == Range::Week {
                bars.iter().map(|d| if value(d) == 0 { String::new() } else { text(value(d)) }).collect()
            } else {
                Vec::new()
            };
            chart.set(bars.iter().map(|d| value(d) as f64 / max).collect(), &values, &labels, last);
            let recorded: Vec<u32> = bars.iter().map(value).filter(|&v| v > 0).collect();
            let mean = recorded.iter().sum::<u32>().checked_div(recorded.len() as u32);
            average.set_label(&mean.map(|m| format!("Average {}", long(m))).unwrap_or_default());
        }
        *self.shown.borrow_mut() = bars.to_vec();
    }

    fn show(&self, view: Option<&View>) {
        let Some(v) = view else {
            self.widget.set_visible_child_name("empty");
            return;
        };
        self.widget.set_visible_child_name("health");
        let imperial = crate::units::imperial();
        self.steps.set_label(&grouped(v.today.steps));
        self.updated.set_label(&match v.span {
            Some((_, last)) => format!("Updated {}", ago(last + 60)),
            None => String::new(),
        });
        self.off.set_visible(!v.settings.tracking);
        self.distance.set_label(&distance(v.today.distance_m, imperial));
        self.active.set_label(&duration(v.today.active_minutes * 60));
        let total = v.today.active_kcal + v.today.resting_kcal;
        self.calories.set_label(&format!("{} kcal", grouped(total)));
        self.calories.set_tooltip_text(Some(&format!(
            "{} active, {} resting",
            grouped(v.today.active_kcal),
            grouped(v.today.resting_kcal)
        )));

        match &v.night {
            Some(n) => {
                self.sleep_value.set_label(&duration(n.total));
                let deep = if n.deep > 0 { format!(" · {} deep", duration(n.deep)) } else { String::new() };
                self.sleep.set_subtitle(&format!("{} – {}{deep}", clock(n.start()), clock(n.end())));
            }
            None => {
                self.sleep_value.set_label("");
                self.sleep.set_subtitle("No sleep recorded");
            }
        }

        self.heart.set_visible(v.has_heart_rate);
        let hr = &v.heart_rate;
        match hr.latest {
            Some((at, bpm)) => {
                self.heart_latest_value.set_label(&format!("{bpm} bpm"));
                self.heart_latest.set_subtitle(&ago(at));
            }
            None => {
                self.heart_latest_value.set_label("");
                self.heart_latest.set_subtitle("No readings today");
            }
        }
        self.heart_range.set_label(&match (hr.low, hr.high) {
            (Some(l), Some(h)) if l != h => format!("{l}–{h} bpm"),
            (Some(l), _) => format!("{l} bpm"),
            _ => String::new(),
        });
        self.heart_resting.set_label(&hr.resting.map(|r| format!("{r} bpm")).unwrap_or_default());

        self.refresh_trend(Some(v));

        for row in self.activity_rows.borrow_mut().drain(..) {
            self.activities.remove(&row);
        }
        let mut rows = Vec::new();
        if v.activities.is_empty() {
            rows.push(adw::ActionRow::builder().title("None this week").build());
        }
        for s in v.activities.iter().take(20) {
            rows.push(activity_row(s, imperial));
        }
        for row in &rows {
            self.activities.add(row);
        }
        *self.activity_rows.borrow_mut() = rows;
    }
}

fn activity_row(s: &Session, imperial: bool) -> adw::ActionRow {
    let start = s.start as i64;
    let date = Local.timestamp_opt(start, 0).single().map(|t| t.date_naive()).unwrap_or_default();
    let mut detail = vec![format!("{}, {}", day_name(date), clock(start)), duration(s.seconds)];
    if s.steps > 0 {
        detail.push(format!("{} steps", grouped(s.steps as u32)));
    }
    let row = adw::ActionRow::builder().title(kind_name(s.kind)).subtitle(detail.join(" · ")).build();
    row.add_prefix(&gtk::Image::from_icon_name(kind_icon(s.kind)));
    if s.distance_m > 0 {
        row.add_suffix(&gtk::Label::builder().label(distance(s.distance_m as u32, imperial)).css_classes(["dim-label", "numeric"]).build());
    }
    row
}

/// "Today", "Yesterday", a weekday within the week, else the date.
fn day_title(date: chrono::NaiveDate) -> String {
    match (Local::now().date_naive() - date).num_days() {
        0..=6 => day_name(date),
        _ => super::locale::day_and_month(crate::backend::health::midnight(date) + 12 * 3600),
    }
}

/// A range of readings: "54–121".
fn reading_range(readings: &[(i64, u8)]) -> String {
    let lo = readings.iter().map(|r| r.1).min().unwrap_or(0);
    let hi = readings.iter().map(|r| r.1).max().unwrap_or(0);
    if lo == hi {
        lo.to_string()
    } else {
        format!("{lo}–{hi}")
    }
}

/// One day hour by hour: steps, sleep the night before, heart rate, and blood oxygen.
fn day_page(state: &Rc<State>, date: chrono::NaiveDate) -> adw::NavigationPage {
    let steps = gtk::Label::builder().css_classes(["title-1", "numeric"]).build();
    let hero = gtk::Box::builder().orientation(gtk::Orientation::Vertical).margin_top(12).build();
    hero.append(&steps);
    hero.append(&gtk::Label::builder().label("steps").css_classes(["dim-label"]).build());

    let activity = adw::PreferencesGroup::new();
    let hourly = Chart::new(72);
    let (hourly_card, _) = card("Steps", &hourly.widget);
    let hourly_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["card"]).build();
    hourly_box.append(&hourly_card);
    let (distance_row, distance_value) = value_row("Distance");
    let (active_row, active_value) = value_row("Active Time");
    let (calories_row, calories_value) = value_row("Calories");
    let rows = adw::PreferencesGroup::new();
    rows.add(&distance_row);
    rows.add(&active_row);
    rows.add(&calories_row);
    let activity_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    activity_box.append(&hourly_box);
    activity_box.append(&rows);
    activity.add(&activity_box);

    let sleep = adw::PreferencesGroup::new();
    let bar = SleepBar::new();
    let (sleep_card, sleep_figure) = card("Sleep", &bar.widget);
    let sleep_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["card"]).build();
    sleep_box.append(&sleep_card);
    sleep.add(&sleep_box);

    let line_card = |title: &str| {
        let line = Line::new();
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).margin_top(12).margin_bottom(12).build();
        content.append(&line.area);
        let axis = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.append(&axis);
        let (inner, figure) = card(title, &content);
        let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["card"]).build();
        outer.append(&inner);
        let group = adw::PreferencesGroup::new();
        group.add(&outer);
        (group, line, axis, figure)
    };
    let (heart, heart_line, heart_axis, heart_figure) = line_card("Heart Rate");
    let (spo2, spo2_line, spo2_axis, spo2_figure) = line_card("Blood Oxygen");

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    for w in [hero.upcast_ref::<gtk::Widget>(), activity.upcast_ref(), sleep.upcast_ref(), heart.upcast_ref(), spo2.upcast_ref()] {
        column.append(w);
    }
    for g in [&sleep, &heart, &spo2] {
        g.set_visible(false);
    }
    let clamp = adw::Clamp::builder().maximum_size(640).child(&column).build();
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    let page = adw::NavigationPage::builder().title(day_title(date)).child(&toolbar).build();

    let show = move |d: &DayDetail| {
        let imperial = crate::units::imperial();
        steps.set_label(&grouped(d.totals.steps));
        let max = d.hourly.iter().copied().max().unwrap_or(0).max(1) as f64;
        let now = Local::now();
        let current = (d.date == now.date_naive()).then(|| chrono::Timelike::hour(&now) as usize);
        hourly.set(d.hourly.iter().map(|&h| h as f64 / max).collect(), &[], &hour_labels(d.bounds), current);
        distance_value.set_label(&distance(d.totals.distance_m, imperial));
        active_value.set_label(&duration(d.totals.active_minutes * 60));
        calories_value.set_label(&format!("{} kcal", grouped(d.totals.active_kcal + d.totals.resting_kcal)));

        sleep.set_visible(d.night.is_some());
        if let Some(n) = &d.night {
            bar.set(n, &d.sleep);
            let deep = if n.deep > 0 { format!(" · {} deep", duration(n.deep)) } else { String::new() };
            sleep_figure.set_label(&format!("{}{deep}", duration(n.total)));
        }
        for (group, line, axis, figure, readings, unit) in [
            (&heart, &heart_line, &heart_axis, &heart_figure, &d.heart_rate, " bpm"),
            (&spo2, &spo2_line, &spo2_axis, &spo2_figure, &d.spo2, "%"),
        ] {
            group.set_visible(!readings.is_empty());
            line.set(readings, d.bounds);
            figure.set_label(&format!("{}{unit}", reading_range(readings)));
            clear(axis);
            let labels = hour_axis(d.bounds);
            labels.set_hexpand(true);
            axis.append(&labels);
        }
    };
    {
        let (alive, state2) = (page.downgrade(), state.clone());
        state.listen(move |e| {
            if alive.upgrade().is_none() {
                return false;
            }
            match e {
                Event::HealthDay(d) if d.date == date => show(d),
                // New data from the watch.
                Event::Health(Some(_)) if date == Local::now().date_naive() => state2.send(Command::HealthDay(date)),
                _ => {}
            }
            true
        });
    }
    state.send(Command::HealthDay(date));
    page
}

const INTERVALS: [(Interval, &str); 4] =
    [(Interval::Five, "5 minutes"), (Interval::Ten, "10 minutes"), (Interval::Thirty, "30 minutes"), (Interval::Hour, "Hour")];

/// How far back the history is kept, for "Delete Older Than".
const KEEP: [(&str, &str, i64); 4] =
    [("month", "1 Month", 31), ("three", "3 Months", 92), ("six", "6 Months", 183), ("year", "1 Year", 366)];

fn settings_page(state: &Rc<State>) -> adw::NavigationPage {
    let view = state.health.borrow().clone();
    let current = Rc::new(RefCell::new(view.as_ref().map(|v| v.settings.clone()).unwrap_or_default()));
    let has_heart_rate = view.as_ref().is_some_and(|v| v.has_heart_rate);
    let five = view.as_ref().is_some_and(|v| v.five_minutes);
    let settable = view.as_ref().is_none_or(|v| v.settable);
    let save: Rc<dyn Fn()> = {
        let (state, current) = (state.clone(), current.clone());
        Rc::new(move || state.send(Command::SetHealth(current.borrow().clone())))
    };
    let h = current.borrow().clone();

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();

    if !settable {
        let note = adw::PreferencesGroup::new();
        let row = adw::ActionRow::builder().title("Not Supported by This Firmware").build();
        row.add_prefix(&gtk::Image::from_icon_name("lithidion-info-symbolic"));
        note.add(&row);
        column.append(&note);
    }

    let switch = |title: &str, on: bool, set: fn(&mut HealthSettings, bool)| {
        let row = adw::SwitchRow::builder().title(title).active(on).sensitive(settable).build();
        let (current, save) = (current.clone(), save.clone());
        row.connect_active_notify(move |r| {
            set(&mut current.borrow_mut(), r.is_active());
            save();
        });
        row
    };

    let tracking = adw::PreferencesGroup::builder().title("Tracking").build();
    tracking.add(&switch("Activity Tracking", h.tracking, |h, on| h.tracking = on));
    tracking.add(&switch("Activity Insights", h.activity_insights, |h, on| h.activity_insights = on));
    tracking.add(&switch("Sleep Insights", h.sleep_insights, |h, on| h.sleep_insights = on));
    column.append(&tracking);

    if has_heart_rate {
        let group = adw::PreferencesGroup::builder().title("Heart Rate").build();
        let monitor = switch("Heart Rate Monitoring", h.heart_rate, |h, on| h.heart_rate = on);
        let offered: Vec<(Interval, &str)> = INTERVALS.iter().copied().filter(|(i, _)| five || *i != Interval::Five).collect();
        let names: Vec<&str> = offered.iter().map(|(_, n)| *n).collect();
        let every = adw::ComboRow::builder().title("Measure Every").model(&gtk::StringList::new(&names)).sensitive(settable).build();
        let selected = offered.iter().position(|(i, _)| *i == h.heart_rate_interval).unwrap_or(if five { 1 } else { 0 });
        every.set_selected(selected as u32);
        {
            let (current, save) = (current.clone(), save.clone());
            every.connect_selected_notify(move |r| {
                if let Some((i, _)) = offered.get(r.selected() as usize) {
                    current.borrow_mut().heart_rate_interval = *i;
                    save();
                }
            });
        }
        let during = switch("During Walks and Runs", h.heart_rate_during_activities, |h, on| h.heart_rate_during_activities = on);
        monitor.bind_property("active", &every, "sensitive").sync_create().transform_to(move |_, on: bool| Some(on && settable)).build();
        monitor.bind_property("active", &during, "sensitive").sync_create().transform_to(move |_, on: bool| Some(on && settable)).build();
        group.add(&monitor);
        group.add(&every);
        group.add(&during);
        column.append(&group);
    }

    let history = adw::PreferencesGroup::builder().title("History").build();
    let kept = adw::ActionRow::builder().title("Kept Since").build();
    let kept_value = gtk::Label::builder().css_classes(["dim-label"]).build();
    kept.add_suffix(&kept_value);
    let older = adw::ActionRow::builder().title("Delete Older History…").activatable(true).build();
    let all = adw::ActionRow::builder().title("Delete All History…").activatable(true).css_classes(["error"]).build();
    history.add(&kept);
    history.add(&older);
    history.add(&all);
    column.append(&history);
    let show_span = {
        let kept_value = kept_value.clone();
        move |v: Option<&View>| {
            let text =
                v.and_then(|v| v.span).map(|(first, _)| super::locale::short_date(first)).unwrap_or_else(|| "Nothing yet".to_string());
            kept_value.set_label(&text);
        }
    };
    show_span(view.as_deref());

    let clamp = adw::Clamp::builder().maximum_size(640).child(&column).build();
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    let page = adw::NavigationPage::builder().title("Health Settings").child(&toolbar).build();
    {
        let alive = page.downgrade();
        state.listen(move |e| {
            if alive.upgrade().is_none() {
                return false;
            }
            if let Event::Health(v) = e {
                show_span(v.as_deref());
            }
            true
        });
    }
    {
        let (state, page2) = (state.clone(), page.clone());
        older.connect_activated(move |_| {
            let alert = adw::AlertDialog::new(Some("Delete Older History?"), None);
            alert.add_response("cancel", "Cancel");
            for (id, label, _) in KEEP {
                alert.add_response(id, &format!("Older Than {label}"));
                alert.set_response_appearance(id, adw::ResponseAppearance::Destructive);
            }
            alert.set_default_response(Some("cancel"));
            alert.set_close_response("cancel");
            let state = state.clone();
            alert.connect_response(None, move |_, r| {
                if let Some((_, _, days)) = KEEP.iter().find(|(id, _, _)| *id == r) {
                    let cutoff = chrono::Utc::now().timestamp() - days * 86_400;
                    state.send(Command::ClearHealth(Some(cutoff)));
                }
            });
            alert.present(Some(&page2));
        });
    }
    {
        let (state, page2) = (state.clone(), page.clone());
        all.connect_activated(move |_| {
            let alert = adw::AlertDialog::new(Some("Delete All Health History?"), None);
            alert.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
            alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            alert.set_default_response(Some("cancel"));
            alert.set_close_response("cancel");
            let state = state.clone();
            alert.connect_response(None, move |_, r| {
                if r == "delete" {
                    state.send(Command::ClearHealth(None));
                }
            });
            alert.present(Some(&page2));
        });
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(grouped(1234567), "1,234,567");
        assert_eq!(grouped(999), "999");
        assert_eq!(duration(7 * 3600 + 12 * 60), "7 h 12 min");
        assert_eq!(duration(3600), "1 h");
        assert_eq!(duration(45 * 60), "45 min");
        assert_eq!(distance(1609, true), "1.0 mi");
        assert_eq!(short(12_400), "12k");
        assert_eq!(short(8_250), "8.2k");
    }
}

#[cfg(test)]
mod render {
    use super::*;
    use crate::backend::health::{HeartRate, Night, Sleep, Totals, Trend};
    use chrono::{Datelike, NaiveDate};

    fn sample() -> View {
        let today = Local::now().date_naive();
        let midnight = crate::backend::health::midnight(today);
        let week = (0..7)
            .map(|i| Day {
                date: today - chrono::Duration::days(6 - i),
                steps: [6200, 8400, 3100, 11250, 7600, 9800, 4321][i as usize],
                sleep: [7 * 3600, 6 * 3600 + 1800, 8 * 3600, 5 * 3600, 7 * 3600 + 900, 0, 7 * 3600 + 720][i as usize],
            })
            .collect();
        let walk = Session {
            kind: SessionKind::Walk,
            start: (midnight + 8 * 3600) as u32,
            seconds: 1920,
            utc_to_local: 0,
            steps: 3400,
            active_kcal: 120,
            resting_kcal: 40,
            distance_m: 2400,
        };
        View {
            profile: 1,
            today: Totals { steps: 4321, distance_m: 3100, active_minutes: 47, active_kcal: 210, resting_kcal: 1180, minutes: 600 },
            heart_rate: HeartRate { latest: Some((midnight + 10 * 3600, 72)), low: Some(54), high: Some(121), resting: Some(56) },
            night: Some(Night {
                sleeps: vec![Sleep { start: midnight - 3600, end: midnight + 6 * 3600 + 720, total: 7 * 3600 + 720, deep: 6000 }],
                total: 7 * 3600 + 720,
                deep: 6000,
            }),
            week,
            activities: vec![walk, Session { kind: SessionKind::Run, start: walk.start - 86_400, seconds: 1500, ..walk }],
            span: Some((midnight - 40 * 86_400, midnight + 10 * 3600)),
            settings: HealthSettings::default(),
            has_heart_rate: true,
            five_minutes: true,
            settable: true,
        }
    }

    fn render(widget: &impl IsA<gtk::Widget>, name: &str, height: i32) {
        let window = adw::Window::builder().default_width(360).default_height(height).content(widget).build();
        super::super::add_icons(&WidgetExt::display(&window));
        window.present();
        let ctx = glib::MainContext::default();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < end {
            ctx.iteration(false);
        }
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
        let texture = window.native().unwrap().renderer().unwrap().render_texture(snapshot.to_node().unwrap(), None);
        texture.save_to_png(std::env::temp_dir().join(name)).unwrap();
    }

    /// Renders the Health tab and its settings to PNGs in the temp folder:
    /// `xvfb-run cargo test -- --ignored health_layout --test-threads=1`.
    #[test]
    #[ignore]
    fn health_layout() {
        adw::init().unwrap();
        let (tx, _rx) = async_channel::unbounded();
        let state = State::new(tx);
        // The page is dropped, as the window drops it, before the summary arrives.
        let widget = HealthPage::new(&state).widget.clone();
        state.apply(&Event::Health(Some(Box::new(sample()))));
        assert_eq!(widget.visible_child_name().as_deref(), Some("health"));
        render(&widget, "health.png", 2100);
        let nav = adw::NavigationView::new();
        nav.add(&settings_page(&state));
        render(&nav, "health-settings.png", 1350);
    }

    fn trend(range: Range, n: i64, step: impl Fn(NaiveDate) -> NaiveDate) -> Trend {
        let mut date = Local::now().date_naive();
        let mut bars = Vec::new();
        for i in 0..n {
            bars.push(Day {
                date,
                steps: (4000 + (i * 3779) % 7000) as u32,
                sleep: if i % 9 == 4 { 0 } else { (6 * 3600 + (i * 1733) % 7200) as u32 },
            });
            date = step(date);
        }
        bars.reverse();
        Trend { profile: 1, range, bars }
    }

    /// The month and year charts and a day's page: `xvfb-run cargo test -- --ignored
    /// health_ranges --test-threads=1`.
    #[test]
    #[ignore]
    fn health_ranges() {
        adw::init().unwrap();
        let (tx, _rx) = async_channel::unbounded();
        let state = State::new(tx);
        state.apply(&Event::Health(Some(Box::new(sample()))));
        for (range, name, t) in [
            (Range::Month, "health-month.png", trend(Range::Month, 30, |d| d - chrono::Duration::days(1))),
            (Range::Year, "health-year.png", trend(Range::Year, 12, |d| d.with_day(1).unwrap() - chrono::Duration::days(1))),
        ] {
            let page = HealthPage::new(&state);
            page.range.set(range);
            state.apply(&Event::HealthTrend(Box::new(Trend {
                bars: t
                    .bars
                    .into_iter()
                    .map(|d| if range == Range::Year { Day { date: d.date.with_day(1).unwrap(), ..d } } else { d })
                    .collect(),
                ..t
            })));
            render(&page.widget, name, 2100);
        }

        let today = Local::now().date_naive();
        let (a, b) = (crate::backend::health::midnight(today), crate::backend::health::midnight(today + chrono::Duration::days(1)));
        let mut hourly = [0u32; 24];
        for (h, v) in hourly.iter_mut().enumerate().skip(7).take(14) {
            *v = 200 + (h as u32 * 997) % 1400;
        }
        let heart_rate = (0..600).step_by(5).map(|m| (a + 7 * 3600 + m * 60, (62 + (m / 7) % 40) as u8)).collect();
        let spo2 = (0..8).map(|h| (a + h * 3600 + 600, 95 + (h % 4) as u8)).collect();
        let night = Night {
            sleeps: vec![Sleep { start: a - 5400, end: a + 6 * 3600, total: 7 * 3600 + 1800, deep: 5400 }],
            total: 7 * 3600 + 1800,
            deep: 5400,
        };
        let sleep = vec![
            SleepBlock { start: a - 5400, end: a + 6 * 3600, deep: false },
            SleepBlock { start: a + 1800, end: a + 4200, deep: true },
            SleepBlock { start: a + 3 * 3600, end: a + 3 * 3600 + 3000, deep: true },
        ];
        let nav = adw::NavigationView::new();
        nav.add(&day_page(&state, today));
        state.apply(&Event::HealthDay(Box::new(DayDetail {
            profile: 1,
            date: today,
            bounds: (a, b),
            totals: Totals { steps: 9321, distance_m: 6400, active_minutes: 74, active_kcal: 310, resting_kcal: 1200, minutes: 900 },
            hourly,
            heart_rate,
            spo2,
            night: Some(night),
            sleep,
        })));
        render(&nav, "health-day.png", 1500);
    }
}
