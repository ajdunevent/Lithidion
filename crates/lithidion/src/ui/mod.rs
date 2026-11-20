//! The window: four fixed tabs, each with its own header and navigation stack; dialogs for the
//! watch switcher, About This Watch, Advanced, and Preferences.

mod about_watch;
mod advanced;
mod app_settings;
mod apps_page;
mod firmware;
mod health_page;
mod images;
mod locale;
mod notifications_page;
mod phone_data;
mod preferences;
mod profiles;
mod screenshots;
mod setup_page;
mod state;
mod store_page;
mod watch_art;
mod watch_page;
mod watch_settings;
mod watches_dialog;

use crate::backend::{AppProgress, Command, Event, Update};
use adw::prelude::*;
use state::State;
use std::rc::Rc;

const NARROW_SP: f64 = 550.0;

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        add_icons(&display);
    }
}

/// The app's icons are compiled into the binary (`data/icons.gresource.xml`); GTK treats the
/// resource path like a theme directory, so symbolic icons recolour without a system icon theme.
fn add_icons(display: &gtk::gdk::Display) {
    gio::resources_register_include!("icons.gresource").expect("icons compiled by build.rs");
    gtk::IconTheme::for_display(display).add_resource_path("/net/dunevent/Lithidion/icons");
}

/// A header for one tab: narrow windows show `narrow_title` with the tabs in a bottom bar; wide
/// windows show the tabs in the title and the watch button at the start.
struct Header {
    bar: adw::HeaderBar,
    switcher: adw::ViewSwitcher,
    watch_button: gtk::Button,
}

fn watch_title_button(state: &Rc<State>, wide: bool) -> (gtk::Button, Box<dyn Fn()>) {
    let name = gtk::Label::builder().css_classes(["heading"]).ellipsize(gtk::pango::EllipsizeMode::End).build();
    let status = gtk::Label::builder().css_classes(["caption", "dim-label"]).build();
    let arrow = gtk::Image::from_icon_name("lithidion-pan-down-symbolic");
    let content = if wide {
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).build();
        name.set_xalign(0.0);
        status.set_xalign(0.0);
        text.append(&name);
        text.append(&status);
        let b = gtk::Box::builder().spacing(6).build();
        b.append(&gtk::Image::from_icon_name("lithidion-watch-symbolic"));
        b.append(&text);
        b.append(&arrow);
        b
    } else {
        let top = gtk::Box::builder().spacing(4).halign(gtk::Align::Center).build();
        top.append(&name);
        top.append(&arrow);
        let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        b.append(&top);
        b.append(&status);
        b
    };
    let button = gtk::Button::builder().child(&content).css_classes(["flat", "watch-title"]).tooltip_text("Watches").build();
    // The watch, with the profile under it; the connection shows on the watch tile below.
    let update = {
        let state = state.clone();
        Box::new(move || {
            let title = state.active_watch().map(|w| state.display_name(&w)).unwrap_or_else(|| "Lithidion".to_string());
            name.set_label(&title);
            let profile = state.profile_name();
            status.set_label(profile.as_deref().unwrap_or(""));
            status.set_visible(profile.is_some());
        })
    };
    update();
    (button, update)
}

fn header(
    state: &Rc<State>,
    stack: &adw::ViewStack,
    menu: &gio::Menu,
    narrow_title: gtk::Widget,
    updaters: &mut Vec<Box<dyn Fn()>>,
    on_watches: Rc<dyn Fn()>,
) -> Header {
    let switcher = adw::ViewSwitcher::builder().stack(stack).policy(adw::ViewSwitcherPolicy::Wide).build();
    let (watch_button, update) = watch_title_button(state, true);
    updaters.push(update);
    watch_button.connect_clicked(move |_| on_watches());
    watch_button.set_visible(false);
    let bar = adw::HeaderBar::builder().title_widget(&narrow_title).build();
    bar.pack_start(&watch_button);
    bar.pack_end(
        &gtk::MenuButton::builder().icon_name("lithidion-menu-symbolic").menu_model(menu).tooltip_text("Main Menu").primary(true).build(),
    );
    Header { bar, switcher, watch_button }
}

fn tab(content: &impl IsA<gtk::Widget>, header: &Header, title: &str) -> adw::NavigationView {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header.bar);
    toolbar.set_content(Some(content));
    let nav = adw::NavigationView::new();
    nav.add(&adw::NavigationPage::builder().title(title).child(&toolbar).build());
    nav
}

/// The Profiles screen's menu: the main menu without the profile items.
fn chooser_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let first = gio::Menu::new();
    first.append(Some("Preferences"), Some("app.preferences"));
    let second = gio::Menu::new();
    second.append(Some("Keyboard Shortcuts"), Some("app.shortcuts"));
    second.append(Some("About Lithidion"), Some("app.about"));
    let third = gio::Menu::new();
    third.append(Some("Quit"), Some("app.quit"));
    menu.append_section(None, &first);
    menu.append_section(None, &second);
    menu.append_section(None, &third);
    menu
}

fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let first = gio::Menu::new();
    first.append(Some("Switch Profile"), Some("app.switch-profile"));
    first.append(Some("Edit Profile"), Some("app.edit-profile"));
    first.append(Some("Preferences"), Some("app.preferences"));
    first.append(Some("Advanced"), Some("app.advanced"));
    let second = gio::Menu::new();
    second.append(Some("Keyboard Shortcuts"), Some("app.shortcuts"));
    second.append(Some("About Lithidion"), Some("app.about"));
    let third = gio::Menu::new();
    third.append(Some("Quit"), Some("app.quit"));
    menu.append_section(None, &first);
    menu.append_section(None, &second);
    menu.append_section(None, &third);
    menu
}

pub fn build_window(
    app: &adw::Application,
    commands: async_channel::Sender<Command>,
    events: async_channel::Receiver<Event>,
    snapshot: Vec<Event>,
) -> adw::ApplicationWindow {
    load_css();
    let state = State::new(commands);
    for e in &snapshot {
        state.apply(e);
    }
    let toasts = adw::ToastOverlay::new();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Lithidion")
        .default_width(390)
        .default_height(780)
        .width_request(360)
        .height_request(294)
        .build();

    let stack = adw::ViewStack::new();
    let menu = main_menu();
    let mut updaters: Vec<Box<dyn Fn()>> = Vec::new();

    let open_watches: Rc<dyn Fn(bool)> = {
        let (state, window, toasts) = (state.clone(), window.clone(), toasts.clone());
        Rc::new(move |add: bool| watches_dialog::present(&state, &window, &toasts, add))
    };
    let on_watches: Rc<dyn Fn()> = {
        let open = open_watches.clone();
        Rc::new(move || open(false))
    };

    let page = {
        let (open, state2, window2) = (open_watches.clone(), state.clone(), window.clone());
        watch_page::WatchPage::new(&state, move || open(true), move || about_watch::present(&state2, &window2))
    };
    let (watch_title, update) = watch_title_button(&state, false);
    updaters.push(update);
    {
        let on = on_watches.clone();
        watch_title.connect_clicked(move |_| on());
    }
    let watch_header = header(&state, &stack, &menu, watch_title.upcast(), &mut updaters, on_watches.clone());

    let apps = apps_page::AppsPage::new(&state);
    let notifications = notifications_page::NotificationsPage::new(&state);
    let health = health_page::HealthPage::new(&state);
    let tabs: Vec<(&str, &str, &str, gtk::Widget)> = vec![
        ("apps", "Apps", "lithidion-grid-symbolic", apps.widget.clone().upcast()),
        ("notifications", "Notifications", "lithidion-notifications-symbolic", notifications.widget.clone().upcast()),
        ("health", "Health", "lithidion-health-symbolic", health.widget.clone().upcast()),
    ];

    let watch_nav = tab(&page.widget, &watch_header, "Watch");
    page.set_nav(&state, &watch_nav);
    stack.add_titled_with_icon(&watch_nav, Some("watch"), "Watch", "lithidion-watch-symbolic");
    let mut headers = vec![watch_header];
    let mut subtitled: Vec<adw::WindowTitle> = Vec::new();
    for (name, title, icon, content) in tabs {
        let window_title = adw::WindowTitle::new(title, "");
        subtitled.push(window_title.clone());
        let h = header(&state, &stack, &menu, window_title.upcast(), &mut updaters, on_watches.clone());
        let nav = tab(&content, &h, title);
        if name == "apps" {
            apps.set_nav(&state, &nav);
        }
        if name == "health" {
            health.set_nav(&nav);
        }
        stack.add_titled_with_icon(&nav, Some(name), title, icon);
        headers.push(h);
    }
    {
        let state = state.clone();
        updaters.push(Box::new(move || {
            let sub = state.active_watch().map(|w| state.display_name(&w)).unwrap_or_default();
            for t in &subtitled {
                t.set_subtitle(&sub);
            }
        }));
    }
    {
        // The watch is asked for its latest whenever the tab is opened.
        let state = state.clone();
        stack.connect_visible_child_name_notify(move |s| {
            if s.visible_child_name().as_deref() == Some("health") {
                state.send(Command::RefreshHealth);
            }
        });
    }
    let updaters = Rc::new(updaters);
    for u in updaters.iter() {
        u();
    }

    let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).reveal(true).build();
    let outer = adw::ToolbarView::new();
    outer.set_content(Some(&stack));
    outer.add_bottom_bar(&switcher_bar);
    // The profile chooser covers the tabs: at first run, after Switch Profile, when the active
    // profile was deleted, and at startup when Preferences says to choose.
    let root = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
    let show_main: Rc<dyn Fn()> = {
        let root = root.clone();
        Rc::new(move || root.set_visible_child_name("main"))
    };
    let chooser = profiles::chooser(&state, &chooser_menu(), show_main);
    root.add_named(&outer, Some("main"));
    root.add_named(&chooser, Some("profiles"));
    let choose_at_start = state.pref(|p| p.launch_with_chooser);
    root.set_visible_child_name(if choose_at_start { "profiles" } else { "main" });
    toasts.set_child(Some(&root));
    window.set_content(Some(&toasts));

    // Wide windows put the tabs in each header and the watch button at its start.
    let wide = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MinWidth,
        NARROW_SP,
        adw::LengthUnit::Sp,
    ));
    wide.add_setter(&switcher_bar, "reveal", Some(&false.to_value()));
    for h in &headers {
        wide.add_setter(&h.bar, "title-widget", Some(&h.switcher.to_value()));
        wide.add_setter(&h.watch_button, "visible", Some(&true.to_value()));
    }
    window.add_breakpoint(wide);

    {
        let (state, toasts, updaters, root) = (state.clone(), toasts.clone(), updaters.clone(), root.clone());
        glib::spawn_future_local(async move {
            while let Ok(e) = events.recv().await {
                state.apply(&e);
                match &e {
                    Event::Toast(t) => toasts.add_toast(adw::Toast::new(&glib::markup_escape_text(t))),
                    Event::Error(t) => toasts.add_toast(adw::Toast::builder().title(glib::markup_escape_text(t)).timeout(8).build()),
                    Event::Dictation(crate::backend::dictation::State::Failed(why)) if !state.preferences_open.get() => {
                        toasts.add_toast(adw::Toast::builder().title(glib::markup_escape_text(&state::sentence(why))).timeout(8).build());
                    }
                    Event::App(uuid, AppProgress::Failed(why)) => {
                        let title = state.installed(*uuid).map(|a| a.app.title).unwrap_or_else(|| "The app".to_string());
                        let text = format!("{title}: {}", state::sentence(why));
                        toasts.add_toast(adw::Toast::builder().title(glib::markup_escape_text(&text)).timeout(8).build());
                    }
                    Event::AppSettings { uuid, title, url } => app_settings::show(&state, &root, *uuid, title, url),
                    Event::WantsNetwork { uuid, title } => {
                        let toast = adw::Toast::builder()
                            .title(glib::markup_escape_text(&format!("{title} wants network access")))
                            .button_label("Allow")
                            .timeout(10)
                            .build();
                        let (state, uuid) = (state.clone(), *uuid);
                        toast.connect_button_clicked(move |_| {
                            state.send(Command::AllowNetwork(uuid, true));
                        });
                        toasts.add_toast(toast);
                    }
                    Event::Watches { .. } | Event::Link(_) | Event::Battery(_) => {
                        for u in updaters.iter() {
                            u();
                        }
                    }
                    Event::Profiles { active, .. } => {
                        for u in updaters.iter() {
                            u();
                        }
                        if active.is_none() {
                            root.set_visible_child_name("profiles");
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    {
        let state = state.clone();
        window.connect_close_request(move |w| {
            if state.settings.borrow().run_in_background {
                return glib::Propagation::Proceed;
            }
            let w = w.clone();
            glib::spawn_future_local(async move {
                crate::client::quit_service().await;
                w.destroy();
            });
            glib::Propagation::Stop
        });
    }
    add_actions(app, &window, &state, &toasts);
    let switch_profile = gio::SimpleAction::new("switch-profile", None);
    {
        let root = root.clone();
        switch_profile.connect_activate(move |_, _| root.set_visible_child_name("profiles"));
    }
    let edit_profile = gio::SimpleAction::new("edit-profile", None);
    {
        let (state, window) = (state.clone(), window.clone());
        edit_profile.connect_activate(move |_, _| profiles::edit(&state, &window));
    }
    app.add_action(&switch_profile);
    app.add_action(&edit_profile);
    window
}

fn add_actions(app: &adw::Application, window: &adw::ApplicationWindow, state: &Rc<State>, toasts: &adw::ToastOverlay) {
    let preferences = gio::SimpleAction::new("preferences", None);
    {
        let (state, window, toasts) = (state.clone(), window.clone(), toasts.clone());
        preferences.connect_activate(move |_, _| preferences::present(&state, &window, &toasts));
    }
    let advanced = gio::SimpleAction::new("advanced", None);
    {
        let (state, window) = (state.clone(), window.clone());
        advanced.connect_activate(move |_, _| advanced::present(&state, &window));
    }
    let shortcuts = gio::SimpleAction::new("shortcuts", None);
    {
        let window = window.clone();
        shortcuts.connect_activate(move |_, _| shortcuts_window(&window).present());
    }
    let about = gio::SimpleAction::new("about", None);
    {
        let window = window.clone();
        about.connect_activate(move |_, _| {
            let dialog = adw::AboutDialog::builder()
                .application_name("Lithidion")
                .application_icon("net.dunevent.Lithidion")
                .version(env!("CARGO_PKG_VERSION"))
                .comments("A companion for Pebble watches on mobile Linux. Not affiliated with Core Devices or Pebble.")
                .developer_name("Lithidion contributors")
                .developers(["AJ Dunevent"])
                .copyright("© 2026 Lithidion contributors")
                .license_type(gtk::License::Gpl30)
                .website("https://dev.dunevent.net/aj/Lithidion")
                .issue_url("https://dev.dunevent.net/aj/Lithidion/issues")
                .build();
            dialog.add_link("Donate", "https://dunevent.net/donate");
            // Downloaded for dictation; CC BY 4.0 asks for this attribution.
            dialog.add_legal_section(
                "Speech Model",
                Some("Parakeet TDT-CTC 110M © NVIDIA, converted for transcribe.cpp by handy-computer"),
                gtk::License::Custom,
                Some("Licensed under <a href=\"https://creativecommons.org/licenses/by/4.0/\">CC BY 4.0</a>"),
            );
            dialog.present(Some(&window));
        });
    }
    let quit = gio::SimpleAction::new("quit", None);
    {
        let (app, state, window) = (app.clone(), state.clone(), window.clone());
        quit.connect_activate(move |_, _| {
            let really_quit = {
                let app = app.clone();
                move || {
                    let app = app.clone();
                    glib::spawn_future_local(async move {
                        crate::client::quit_service().await;
                        app.quit();
                    });
                }
            };
            let installing = matches!(*state.firmware.borrow(), Update::Downloading(_) | Update::Installing(_));
            if !installing {
                return really_quit();
            }
            let alert = adw::AlertDialog::new(
                Some("Stop Installing PebbleOS?"),
                Some("Quitting stops the install. The watch keeps the firmware it has, and the install can start again later."),
            );
            alert.add_responses(&[("cancel", "Keep Installing"), ("quit", "Quit")]);
            alert.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
            alert.set_default_response(Some("cancel"));
            alert.set_close_response("cancel");
            alert.connect_response(None, move |_, r| {
                if r == "quit" {
                    really_quit();
                }
            });
            window.present();
            alert.present(Some(&window));
        });
    }
    let close = gio::SimpleAction::new("close", None);
    {
        let window = window.clone();
        close.connect_activate(move |_, _| window.close());
    }
    window.add_action(&close);
    for a in [&preferences, &advanced, &shortcuts, &about, &quit] {
        app.add_action(a);
    }
    app.set_accels_for_action("app.preferences", &["<Control>comma"]);
    app.set_accels_for_action("app.shortcuts", &["<Control>question"]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("win.close", &["<Control>w"]);
}

fn shortcuts_window(parent: &adw::ApplicationWindow) -> gtk::ShortcutsWindow {
    const UI: &str = r#"<interface>
  <object class="GtkShortcutsWindow" id="shortcuts">
    <property name="modal">1</property>
    <child>
      <object class="GtkShortcutsSection">
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">General</property>
            <child><object class="GtkShortcutsShortcut"><property name="title">Preferences</property><property name="accelerator">&lt;Control&gt;comma</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Keyboard Shortcuts</property><property name="accelerator">&lt;Control&gt;question</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Close Window</property><property name="accelerator">&lt;Control&gt;w</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Quit</property><property name="accelerator">&lt;Control&gt;q</property></object></child>
          </object>
        </child>
      </object>
    </child>
  </object>
</interface>"#;
    let builder = gtk::Builder::from_string(UI);
    let window: gtk::ShortcutsWindow = builder.object("shortcuts").expect("shortcuts window in the UI definition");
    window.set_transient_for(Some(parent));
    window
}
