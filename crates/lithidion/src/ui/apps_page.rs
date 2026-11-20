//! The Apps tab: the active watch's watchfaces and apps, in the watch's order, and the way into
//! the store.

use super::images;
use super::state::State;
use super::store_page;
use crate::appstore::{self, Source, StoreApp};
use crate::backend::{AppProgress, Command, Event};
use crate::net;
use crate::store::{AppKind, OnWatch, WatchApp};
use adw::prelude::*;
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use uuid::Uuid;

pub struct AppsPage {
    pub widget: gtk::Stack,
    nav: OnceCell<adw::NavigationView>,
    faces: adw::PreferencesGroup,
    apps: adw::PreferencesGroup,
    rows: RefCell<Vec<(adw::PreferencesGroup, adw::ActionRow)>>,
    /// The store's copy of each installed store app, looked up once per run.
    latest: RefCell<HashMap<Uuid, StoreApp>>,
    asked: RefCell<HashSet<Uuid>>,
}

impl AppsPage {
    pub fn new(state: &Rc<State>) -> Rc<AppsPage> {
        let empty = adw::StatusPage::builder()
            .icon_name("lithidion-grid-symbolic")
            .title("No Watch")
            .description("Add a watch to install watchfaces and apps on it.")
            .vexpand(true)
            .build();
        let store_group = adw::PreferencesGroup::new();
        let store_row = adw::ActionRow::builder().title("Catalog").subtitle("Find new watchfaces and apps").activatable(true).build();
        store_row.add_prefix(&gtk::Image::builder().icon_name("lithidion-grid-symbolic").pixel_size(24).build());
        store_row.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
        store_group.add(&store_row);
        let faces = adw::PreferencesGroup::builder().title("Watchfaces").build();
        let apps = adw::PreferencesGroup::builder().title("Apps").build();
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        column.append(&store_group);
        column.append(&faces);
        column.append(&apps);
        let clamp = adw::Clamp::builder().maximum_size(640).child(&column).build();
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();
        let widget = gtk::Stack::new();
        widget.add_named(&empty, Some("empty"));
        widget.add_named(&scroller, Some("apps"));

        let page = Rc::new(AppsPage {
            widget,
            nav: OnceCell::new(),
            faces,
            apps,
            rows: RefCell::default(),
            latest: RefCell::default(),
            asked: RefCell::default(),
        });
        {
            let (state, page) = (state.clone(), Rc::downgrade(&page));
            store_row.connect_activated(move |_| {
                let Some(page) = page.upgrade() else { return };
                if let Some(nav) = page.nav.get() {
                    nav.push(&store_page::store_page(&state, nav));
                }
            });
        }
        {
            let (page2, state2) = (page.clone(), Rc::downgrade(state));
            state.listen(move |e| {
                let Some(state) = state2.upgrade() else { return false };
                if matches!(e, Event::Watches { .. } | Event::Apps(_) | Event::App(..) | Event::Link(_)) {
                    page2.refresh(&state);
                }
                true
            });
        }
        page
    }

    /// The tab's navigation stack, for the store's pages.
    pub fn set_nav(self: &Rc<Self>, state: &Rc<State>, nav: &adw::NavigationView) {
        let _ = self.nav.set(nav.clone());
        self.refresh(state);
    }

    fn refresh(self: &Rc<Self>, state: &Rc<State>) {
        if state.active.get().is_none() {
            self.widget.set_visible_child_name("empty");
            return;
        }
        self.widget.set_visible_child_name("apps");
        for (group, row) in self.rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        let list: Vec<WatchApp> = state.apps.borrow().iter().filter(|a| !a.app.hidden).cloned().collect();
        self.look_up_updates(state, &list);
        for kind in [AppKind::Watchface, AppKind::Watchapp] {
            let group = if kind == AppKind::Watchface { &self.faces } else { &self.apps };
            let of_kind: Vec<&WatchApp> = list.iter().filter(|a| a.app.kind == kind).collect();
            if of_kind.is_empty() {
                let row = adw::ActionRow::builder()
                    .title(if kind == AppKind::Watchface { "No watchfaces added" } else { "No apps added" })
                    .css_classes(["dim-label"])
                    .build();
                group.add(&row);
                self.rows.borrow_mut().push((group.clone(), row));
                continue;
            }
            for (i, item) in of_kind.iter().enumerate() {
                let neighbours = (i.checked_sub(1).map(|j| of_kind[j].app.uuid), of_kind.get(i + 1).map(|a| a.app.uuid));
                let row = self.row(state, item, &of_kind, neighbours);
                group.add(&row);
                self.rows.borrow_mut().push((group.clone(), row));
            }
        }
    }

    fn row(
        self: &Rc<Self>,
        state: &Rc<State>,
        item: &WatchApp,
        of_kind: &[&WatchApp],
        neighbours: (Option<Uuid>, Option<Uuid>),
    ) -> adw::ActionRow {
        let a = &item.app;
        let latest = self.latest.borrow().get(&a.uuid).cloned();
        let update = latest.as_ref().filter(|l| appstore::newer(&l.version, &a.version)).cloned();
        let subtitle = match state.app_progress.borrow().get(&a.uuid) {
            Some(AppProgress::Downloading(f)) => format!("Downloading… {} %", (f * 100.0) as u32),
            Some(AppProgress::Sending(f)) => format!("Sending to the watch… {} %", (f * 100.0) as u32),
            _ if item.state == OnWatch::ToInsert => "Goes on the watch when it connects".to_string(),
            _ => match &update {
                Some(u) => format!("Update available: {}", u.version),
                None if a.developer.is_empty() => a.version.clone(),
                None => format!("{} · {}", a.developer, a.version),
            },
        };
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&a.title))
            .subtitle(glib::markup_escape_text(&subtitle))
            .activatable(true)
            .build();
        let icon = if a.kind == AppKind::Watchface { "lithidion-watch-symbolic" } else { "lithidion-grid-symbolic" };
        row.add_prefix(&images::thumbnail(a.image.as_deref(), 64, icon));

        // Reordering; everything else is on the app's page.
        let uuid = a.uuid;
        let ids: Vec<Uuid> = of_kind.iter().map(|x| x.app.uuid).collect();
        for (icon, tooltip, other) in
            [("lithidion-go-up-symbolic", "Move Up", neighbours.0), ("lithidion-go-down-symbolic", "Move Down", neighbours.1)]
        {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tooltip)
                .valign(gtk::Align::Center)
                .sensitive(other.is_some())
                .css_classes(["flat"])
                .build();
            if let Some(other) = other {
                let (state, ids) = (state.clone(), ids.clone());
                button.connect_clicked(move |_| {
                    let mut ids = ids.clone();
                    let (i, j) = (ids.iter().position(|x| *x == uuid), ids.iter().position(|x| *x == other));
                    if let (Some(i), Some(j)) = (i, j) {
                        ids.swap(i, j);
                        state.send(Command::ReorderApps(ids));
                    }
                });
            }
            row.add_suffix(&button);
        }
        {
            let (state, page, installed) = (state.clone(), Rc::downgrade(self), a.clone());
            row.connect_activated(move |_| {
                let Some(nav) = page.upgrade().and_then(|p| p.nav.get().cloned()) else { return };
                let listing = page.upgrade().and_then(|p| p.latest.borrow().get(&installed.uuid).cloned());
                nav.push(&match listing {
                    Some(l) => store_page::app_page(&state, l),
                    None => store_page::installed_page(&state, &installed),
                });
            });
        }
        row
    }

    /// Asks the store once per app and run for its latest version.
    fn look_up_updates(self: &Rc<Self>, state: &Rc<State>, list: &[WatchApp]) {
        let Some(platform) = state.platform() else { return };
        for item in list {
            let Some((source, id)) = item.app.store.clone() else { continue };
            let Some(source) = Source::from_key(&source) else { continue };
            if !self.asked.borrow_mut().insert(item.app.uuid) {
                continue;
            }
            let (page, state, uuid) = (Rc::downgrade(self), state.clone(), item.app.uuid);
            glib::spawn_future_local(async move {
                let Ok(found) = net::run(appstore::app(source, id, platform)).await else { return };
                let Some(page) = page.upgrade() else { return };
                if found.uuid != uuid {
                    return;
                }
                page.latest.borrow_mut().insert(uuid, found);
                page.refresh(&state);
            });
        }
    }
}
