//! The app store: home pages for faces and apps, full lists, search, and an app's own page.

use super::images;
use super::screenshots;
use super::state::{sentence, State};
use crate::appstore::{self, Kind, Shelf, Source, StoreApp};
use crate::backend::{AppProgress, Command, Event};
use crate::net;
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The first apps a home shelf shows before "See All".
const SHELF_PREVIEW: usize = 5;

fn icon_for(kind: Kind) -> &'static str {
    match kind {
        Kind::Watchface => "lithidion-watch-symbolic",
        Kind::Watchapp => "lithidion-grid-symbolic",
    }
}

/// A row for a store app; opens its page.
pub fn app_row(state: &Rc<State>, nav: &adw::NavigationView, app: &StoreApp) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&app.title))
        .subtitle(glib::markup_escape_text(&app.author))
        .activatable(true)
        .build();
    row.add_prefix(&images::thumbnail(app.thumbnail(), 64, icon_for(app.kind)));
    if let Some(installed) = state.installed(app.uuid) {
        if appstore::newer(&app.version, &installed.app.version) {
            row.add_suffix(&gtk::Label::builder().label("Update").css_classes(["accent", "caption-heading"]).build());
        } else {
            let check = gtk::Image::from_icon_name("lithidion-check-symbolic");
            check.set_tooltip_text(Some("Installed"));
            check.add_css_class("dim-label");
            row.add_suffix(&check);
        }
    }
    row.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
    let (state, nav, app) = (state.clone(), nav.clone(), app.clone());
    row.connect_activated(move |_| nav.push(&app_page(&state, app.clone())));
    row
}

fn status(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).wrap(true).margin_top(24).margin_bottom(24).css_classes(["dim-label"]).build()
}

fn column() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build()
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder().maximum_size(640).child(child).build();
    gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build()
}

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

fn shelf_group(state: &Rc<State>, nav: &adw::NavigationView, shelf: &Shelf, kind: Kind) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(glib::markup_escape_text(&shelf.name)).build();
    if shelf.apps.len() > SHELF_PREVIEW || shelf.apps.is_empty() {
        let all = gtk::Button::builder().label("See All").valign(gtk::Align::Center).css_classes(["flat"]).build();
        let (state, nav, shelf) = (state.clone(), nav.clone(), shelf.clone());
        all.connect_clicked(move |_| nav.push(&list_page(&state, &nav, shelf.clone(), kind)));
        group.set_header_suffix(Some(&all));
    }
    for app in shelf.apps.iter().take(SHELF_PREVIEW) {
        group.add(&app_row(state, nav, app));
    }
    group
}

/// A full collection or category, a page at a time.
fn list_page(state: &Rc<State>, nav: &adw::NavigationView, shelf: Shelf, kind: Kind) -> adw::NavigationPage {
    let content = column();
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    let more = gtk::Button::builder().label("Show More").halign(gtk::Align::Center).css_classes(["pill"]).visible(false).build();
    let spinner = gtk::Spinner::builder().spinning(true).halign(gtk::Align::Center).build();
    let note = status("");
    note.set_visible(false);
    content.append(&list);
    content.append(&spinner);
    content.append(&more);
    content.append(&note);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scrolled(&content)));
    let page = adw::NavigationPage::builder().title(&shelf.name).child(&toolbar).build();

    let next = Rc::new(Cell::new(Some(0usize)));
    let load: Rc<dyn Fn()> = {
        let (state, nav, page) = (state.clone(), nav.clone(), page.downgrade());
        let (list, more, spinner, note, next) = (list.clone(), more.clone(), spinner.clone(), note.clone(), next.clone());
        Rc::new(move || {
            let (Some(offset), Some(platform)) = (next.get(), state.platform()) else { return };
            more.set_visible(false);
            spinner.set_visible(true);
            let (state, nav, page, list, more, spinner, note, next) =
                (state.clone(), nav.clone(), page.clone(), list.clone(), more.clone(), spinner.clone(), note.clone(), next.clone());
            let (source, path) = (shelf.source, shelf.path.clone());
            glib::spawn_future_local(async move {
                let result = net::run(async move { appstore::list(source, &path, platform, offset).await }).await;
                if page.upgrade().is_none() {
                    return;
                }
                spinner.set_visible(false);
                match result {
                    Ok((apps, after)) => {
                        for app in apps.iter().filter(|a| a.kind == kind) {
                            list.append(&app_row(&state, &nav, app));
                        }
                        next.set(after);
                        more.set_visible(after.is_some());
                        if list.first_child().is_none() {
                            note.set_label("Nothing here runs on this watch.");
                            note.set_visible(true);
                        }
                    }
                    Err(e) => {
                        note.set_label(&sentence(&format!("{e:#}")));
                        note.set_visible(true);
                        more.set_visible(true);
                    }
                }
            });
        })
    };
    {
        let load = load.clone();
        more.connect_clicked(move |_| load());
    }
    load();
    page
}

/// One kind's home: search, collections, categories.
fn kind_home(state: &Rc<State>, nav: &adw::NavigationView, kind: Kind) -> gtk::ScrolledWindow {
    let content = column();
    let search = gtk::SearchEntry::builder()
        .placeholder_text(match kind {
            Kind::Watchface => "Search watchfaces",
            Kind::Watchapp => "Search apps",
        })
        .build();
    let home = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).build();
    let results = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).visible(false).build();
    content.append(&search);
    content.append(&home);
    content.append(&results);
    let scroller = scrolled(&content);

    let loaded = Rc::new(Cell::new(false));
    let load_home: Rc<dyn Fn()> = {
        let (state, nav, home, loaded) = (state.clone(), nav.clone(), home.clone(), loaded.clone());
        Rc::new(move || {
            let Some(platform) = state.platform() else {
                clear(&home);
                home.append(&status("The store opens once the active watch has connected once."));
                return;
            };
            clear(&home);
            home.append(&gtk::Spinner::builder().spinning(true).halign(gtk::Align::Center).margin_top(24).build());
            loaded.set(true);
            let (state, nav, home, loaded) = (state.clone(), nav.clone(), home.clone(), loaded.clone());
            glib::spawn_future_local(async move {
                let result = net::run(appstore::home(catalog_sources(&state), kind, platform)).await;
                clear(&home);
                match result {
                    Ok(h) => {
                        for shelf in &h.collections {
                            home.append(&shelf_group(&state, &nav, shelf, kind));
                        }
                        if !h.categories.is_empty() && kind == Kind::Watchapp {
                            let group = adw::PreferencesGroup::builder().title("Categories").build();
                            for c in &h.categories {
                                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&c.name)).activatable(true).build();
                                row.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
                                let (state, nav, c) = (state.clone(), nav.clone(), c.clone());
                                row.connect_activated(move |_| nav.push(&list_page(&state, &nav, c.clone(), kind)));
                                group.add(&row);
                            }
                            home.append(&group);
                        }
                    }
                    Err(e) => {
                        loaded.set(false);
                        home.append(&status(&sentence(&format!("{e:#}"))));
                    }
                }
            });
        })
    };
    {
        let (load_home, loaded) = (load_home.clone(), loaded.clone());
        scroller.connect_map(move |_| {
            if !loaded.get() {
                load_home();
            }
        });
    }

    // Each search supersedes the one before; only the latest one's answer is shown.
    let generation = Rc::new(Cell::new(0u64));
    {
        let (state, nav, home, results) = (state.clone(), nav.clone(), home.clone(), results.clone());
        search.connect_search_changed(move |entry| {
            let text = entry.text().trim().to_string();
            generation.set(generation.get() + 1);
            let mine = generation.get();
            if text.is_empty() {
                results.set_visible(false);
                home.set_visible(true);
                return;
            }
            let Some(platform) = state.platform() else { return };
            home.set_visible(false);
            results.set_visible(true);
            clear(&results);
            results.append(&gtk::Spinner::builder().spinning(true).halign(gtk::Align::Center).margin_top(24).build());
            let (state, nav, results, generation) = (state.clone(), nav.clone(), results.clone(), generation.clone());
            glib::spawn_future_local(async move {
                let found = net::run(appstore::search(catalog_sources(&state), text, kind, platform)).await;
                if generation.get() != mine {
                    return;
                }
                clear(&results);
                match found {
                    Ok(apps) if apps.is_empty() => results.append(&status("Nothing found")),
                    Ok(apps) => {
                        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
                        for app in &apps {
                            list.append(&app_row(&state, &nav, app));
                        }
                        results.append(&list);
                    }
                    Err(e) => results.append(&status(&sentence(&format!("{e:#}")))),
                }
            });
        });
    }
    scroller
}

/// The store, with watchfaces and apps as two views.
pub(super) fn catalog_sources(state: &State) -> Vec<Source> {
    state.prefs.borrow().as_ref().map(|p| p.catalog_sources.clone()).unwrap_or(Source::ALL.to_vec())
}

pub fn store_page(state: &Rc<State>, nav: &adw::NavigationView) -> adw::NavigationPage {
    let stack = adw::ViewStack::new();
    stack.add_titled_with_icon(&kind_home(state, nav, Kind::Watchface), Some("faces"), "Watchfaces", "lithidion-watch-symbolic");
    stack.add_titled_with_icon(&kind_home(state, nav, Kind::Watchapp), Some("apps"), "Apps", "lithidion-grid-symbolic");
    let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
    let header = adw::HeaderBar::builder().title_widget(&switcher).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    adw::NavigationPage::builder().title("Catalog").tag("store").child(&toolbar).build()
}

fn percent(f: f64) -> String {
    format!("{} %", (f * 100.0).floor() as u32)
}

/// An app's page: screenshots, description, and what can be done with it.
pub fn app_page(state: &Rc<State>, app: StoreApp) -> adw::NavigationPage {
    page_for(state, app, true)
}

/// The page of an installed app no store lists (sideloaded, or its store unreachable).
pub fn installed_page(state: &Rc<State>, installed: &crate::store::App) -> adw::NavigationPage {
    let app = StoreApp {
        source: Source::Rebble,
        id: String::new(),
        uuid: installed.uuid,
        kind: if installed.kind == crate::store::AppKind::Watchface { appstore::Kind::Watchface } else { appstore::Kind::Watchapp },
        title: installed.title.clone(),
        author: installed.developer.clone(),
        description: String::new(),
        version: installed.version.clone(),
        pbw: None,
        hearts: 0,
        list_image: installed.image.clone(),
        screenshots: Vec::new(),
        changelog: Vec::new(),
        website: None,
        source_code: None,
        category: String::new(),
    };
    page_for(state, app, false)
}

fn page_for(state: &Rc<State>, app: StoreApp, listed: bool) -> adw::NavigationPage {
    let app = Rc::new(RefCell::new(app));
    let content = column();
    let shots = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    let title = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).css_classes(["title-1"]).build();
    let author = gtk::Label::builder().wrap(true).css_classes(["dim-label"]).build();
    let main = gtk::Button::builder().halign(gtk::Align::Center).css_classes(["pill", "suggested-action"]).build();
    let second = gtk::Button::builder().halign(gtk::Align::Center).css_classes(["pill"]).build();
    let remove = gtk::Button::builder().label("Uninstall").halign(gtk::Align::Center).css_classes(["pill", "destructive-action"]).build();
    let buttons = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
    buttons.append(&main);
    buttons.append(&second);
    buttons.append(&remove);
    let progress_label = gtk::Label::builder().css_classes(["dim-label"]).build();
    let bar = gtk::ProgressBar::builder().margin_start(24).margin_end(24).build();
    let progress = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    progress.append(&progress_label);
    progress.append(&bar);
    // Not selectable: on touchscreens a swipe would select text instead of scrolling.
    let description = gtk::Label::builder().wrap(true).xalign(0.0).build();
    // What this watch does with the app.
    let options = adw::PreferencesGroup::new();
    let settings_row = adw::ActionRow::builder().title("Settings").activatable(true).build();
    settings_row.add_suffix(&gtk::Image::from_icon_name("lithidion-go-next-symbolic"));
    let network_row = adw::SwitchRow::builder().title("Network Access").build();
    options.add(&settings_row);
    options.add(&network_row);
    let details = adw::PreferencesGroup::new();
    let changes = adw::PreferencesGroup::builder().title("Changes").build();
    let head = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    head.append(&title);
    head.append(&author);
    for w in [
        shots.upcast_ref::<gtk::Widget>(),
        head.upcast_ref(),
        buttons.upcast_ref(),
        progress.upcast_ref(),
        options.upcast_ref(),
        description.upcast_ref(),
        details.upcast_ref(),
        changes.upcast_ref(),
    ] {
        content.append(w);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scrolled(&content)));
    let page = adw::NavigationPage::builder().title(&app.borrow().title).child(&toolbar).build();

    let detail_rows: Rc<RefCell<Vec<(adw::PreferencesGroup, adw::ActionRow)>>> = Rc::default();
    let fill = {
        let (app, shots, title, author, description, details, changes, detail_rows) = (
            app.clone(),
            shots.clone(),
            title.clone(),
            author.clone(),
            description.clone(),
            details.clone(),
            changes.clone(),
            detail_rows.clone(),
        );
        move || {
            let a = app.borrow();
            clear(&shots);
            if a.screenshots.is_empty() {
                let picture = images::thumbnail(a.list_image.as_deref(), 128, icon_for(a.kind));
                picture.set_halign(gtk::Align::Center);
                shots.append(&picture);
            } else {
                shots.append(&screenshots::view(a.screenshots.clone()));
            }
            title.set_label(&a.title);
            author.set_label(&a.author);
            description.set_label(&a.description);
            description.set_visible(!a.description.is_empty());
            for (group, row) in detail_rows.borrow_mut().drain(..) {
                group.remove(&row);
            }
            let add = |group: &adw::PreferencesGroup, row: adw::ActionRow| {
                group.add(&row);
                detail_rows.borrow_mut().push((group.clone(), row));
            };
            let value = |title: &str, v: &str| {
                let row = adw::ActionRow::builder().title(title).build();
                row.add_suffix(&gtk::Label::builder().label(v).css_classes(["dim-label"]).wrap(true).xalign(1.0).build());
                row
            };
            add(&details, value("Version", &a.version));
            if !a.category.is_empty() {
                add(&details, value("Category", &a.category));
            }
            if listed {
                add(&details, value("Hearts", &a.hearts.to_string()));
                add(&details, value("From", a.source.name()));
            }
            for (label, url) in [("Website", &a.website), ("Source Code", &a.source_code)] {
                if let Some(url) = url {
                    let row = adw::ActionRow::builder().title(label).subtitle(glib::markup_escape_text(url)).activatable(true).build();
                    row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
                    let url = url.clone();
                    row.connect_activated(move |r| {
                        let window = r.root().and_downcast::<gtk::Window>();
                        gtk::UriLauncher::new(&url).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
                    });
                    add(&details, row);
                }
            }
            let notes: Vec<_> = a.changelog.iter().filter(|c| !c.version.is_empty()).rev().take(10).collect();
            for c in &notes {
                let row = adw::ActionRow::builder()
                    .title(&c.version)
                    .subtitle(glib::markup_escape_text(if c.notes.is_empty() { "No notes" } else { &c.notes }))
                    .build();
                row.set_subtitle_lines(0);
                add(&changes, row);
            }
            changes.set_visible(!notes.is_empty());
        }
    };
    fill();

    let quiet = Rc::new(std::cell::Cell::new(false));
    let render = {
        let (state, app, main, second, remove, progress, progress_label, bar) = (
            state.clone(),
            app.clone(),
            main.clone(),
            second.clone(),
            remove.clone(),
            progress.clone(),
            progress_label.clone(),
            bar.clone(),
        );
        let (options, settings_row, network_row, quiet) = (options.clone(), settings_row.clone(), network_row.clone(), quiet.clone());
        move || {
            let a = app.borrow();
            let installed = state.installed(a.uuid);
            // Settings while it is on the connected watch; network access for any phone-side script.
            let entry = installed.as_ref().map(|i| &i.app);
            let on_watch = installed.as_ref().is_some_and(|i| i.state == crate::store::OnWatch::Inserted);
            settings_row.set_visible(entry.is_some_and(|e| e.configurable) && on_watch && state.is_connected());
            network_row.set_visible(entry.is_some_and(|e| e.script));
            if network_row.is_visible() {
                let allowed = state.pref(|p| p.network.contains(&a.uuid));
                quiet.set(true);
                network_row.set_active(allowed);
                quiet.set(false);
            }
            options.set_visible(settings_row.is_visible() || network_row.is_visible());
            let busy = state.app_progress.borrow().get(&a.uuid).cloned();
            match &busy {
                Some(AppProgress::Downloading(f)) => {
                    progress_label.set_label(&format!("Downloading… {}", percent(*f)));
                    bar.set_fraction(*f);
                }
                Some(AppProgress::Sending(f)) => {
                    progress_label.set_label(&format!("Sending to the watch… {}", percent(*f)));
                    bar.set_fraction(*f);
                }
                _ => {}
            }
            progress.set_visible(busy.is_some());
            if busy.is_some() {
                for b in [&main, &second, &remove] {
                    b.set_visible(false);
                }
                return;
            }
            match installed {
                None => {
                    main.set_label("Install");
                    main.set_visible(true);
                    second.set_visible(false);
                    remove.set_visible(false);
                }
                Some(i) => {
                    let update = appstore::newer(&a.version, &i.app.version);
                    main.set_label(if update { "Update" } else { "Open on Watch" });
                    main.set_visible(update || state.is_connected());
                    main.set_sensitive(update || state.is_connected());
                    second.set_visible(update && state.is_connected());
                    second.set_label("Open on Watch");
                    remove.set_visible(true);
                }
            }
        }
    };
    render();
    {
        let (state, app) = (state.clone(), app.clone());
        main.connect_clicked(move |_| {
            let a = app.borrow().clone();
            match state.installed(a.uuid) {
                Some(i) if !appstore::newer(&a.version, &i.app.version) => state.send(Command::LaunchApp(a.uuid)),
                _ => state.send(Command::InstallApp(Box::new(a))),
            }
        });
    }
    {
        let (state, app) = (state.clone(), app.clone());
        second.connect_clicked(move |_| state.send(Command::LaunchApp(app.borrow().uuid)));
    }
    {
        let (state, app) = (state.clone(), app.clone());
        settings_row.connect_activated(move |_| state.send(Command::AppSettings(app.borrow().uuid)));
    }
    {
        let (state, app) = (state.clone(), app.clone());
        network_row.connect_active_notify(move |r| {
            if !quiet.get() {
                state.send(Command::AllowNetwork(app.borrow().uuid, r.is_active()));
            }
        });
    }
    {
        let (state, app) = (state.clone(), app.clone());
        remove.connect_clicked(move |b| confirm_remove(&state, b, &app.borrow().title, app.borrow().uuid));
    }

    // The page from a list or search may lack the changelog and bundle link.
    {
        let (app, page_weak, fill, render) = (app.clone(), page.downgrade(), Rc::new(fill), Rc::new(render.clone()));
        let platform = state.platform().filter(|_| listed);
        let (source, id) = (app.borrow().source, app.borrow().id.clone());
        if let Some(platform) = platform {
            glib::spawn_future_local(async move {
                if let Ok(full) = net::run(appstore::app(source, id, platform)).await {
                    if page_weak.upgrade().is_some() {
                        *app.borrow_mut() = full;
                        fill();
                        render();
                    }
                }
            });
        }
    }
    let page_weak = page.downgrade();
    state.listen(move |e| {
        if page_weak.upgrade().is_none() {
            return false;
        }
        if matches!(e, Event::Apps(_) | Event::App(..) | Event::Link(_)) {
            render();
        }
        true
    });
    page
}

/// Asks before removing an app from the active watch.
pub fn confirm_remove(state: &Rc<State>, parent: &impl IsA<gtk::Widget>, title: &str, uuid: uuid::Uuid) {
    let alert =
        adw::AlertDialog::new(Some(&format!("Uninstall {title}?")), Some("It is removed from this watch. Other watches keep their copy."));
    alert.add_responses(&[("cancel", "Cancel"), ("remove", "Uninstall")]);
    alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");
    let state = state.clone();
    alert.connect_response(None, move |_, r| {
        if r == "remove" {
            state.send(Command::RemoveApp(uuid));
        }
    });
    alert.present(Some(parent));
}
