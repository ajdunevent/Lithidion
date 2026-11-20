mod autostart;
mod client;
mod ui;

use lithidion_core::{appstore, backend, host, net, settings, store, units, APP_ID};
#[cfg(test)]
use lithidion_core::pkjs;

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const USAGE: &str = "\
Usage: lithidion [--hidden]

  --hidden  start the background service without a window";

/// Distrobox leaves the host's system bus, where BlueZ lives, out of the container; it stays
/// reachable under `/run/host`. Has to run before any other thread starts.
fn use_host_system_bus() {
    const CONTAINER: &str = "/run/dbus/system_bus_socket";
    const HOST: &str = "/run/host/run/dbus/system_bus_socket";
    if std::env::var_os("DBUS_SYSTEM_BUS_ADDRESS").is_none()
        && !std::path::Path::new(CONTAINER).exists()
        && std::path::Path::new(HOST).exists()
    {
        std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", format!("unix:path={HOST}"));
    }
}

fn main() -> glib::ExitCode {
    use_host_system_bus();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "lithidion=info,lithidion_core=info".into()))
        .init();
    // WebKit has to start on GTK's thread before any script thread uses JavaScriptCore: the first
    // thread to start either becomes WebKit's main thread (else WebKit aborts on the first web
    // view, and JavaScriptCore's timers crash once that script ends), and JavaScriptCore started
    // before WebKit leaves web views that never load.
    gtk::init().expect("GTK cannot start");
    let _ = webkit6::WebContext::default();
    host::detect();
    tracing::info!(zone = %backend::time_zone(), "time zone");

    let args: Vec<String> = std::env::args().skip(1).collect();
    let hidden = match args.as_slice() {
        [] => false,
        [flag] if flag == "--hidden" => true,
        _ => {
            eprintln!("{USAGE}");
            return glib::ExitCode::FAILURE;
        }
    };

    // For the catalog, calendars, and other lookups the window makes itself.
    let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start: {e}");
            return glib::ExitCode::FAILURE;
        }
    };
    net::set_runtime(rt.handle().clone());

    let app = adw::Application::builder().application_id(APP_ID).build();
    let window: Rc<RefCell<Option<adw::ApplicationWindow>>> = Rc::default();
    let starting = Rc::new(Cell::new(false));
    app.connect_activate(move |app| {
        if let Some(existing) = window.borrow().as_ref() {
            existing.present();
            return;
        }
        if starting.replace(true) {
            return;
        }
        // Held until the window exists: the app would otherwise quit with none.
        let hold = app.hold();
        let (app, window, starting) = (app.clone(), window.clone(), starting.clone());
        glib::spawn_future_local(async move {
            let connected = client::connect().await;
            if hidden {
                if let Err(e) = connected {
                    tracing::error!(error = format!("{e:#}"), "cannot start the service");
                }
                drop(hold);
                return;
            }
            let c = connected.unwrap_or_else(|e| {
                // The window still opens, to say why it is empty.
                let (commands, _) = async_channel::unbounded();
                let (tx, events) = async_channel::unbounded();
                let _ = tx.try_send(backend::Event::Error(format!("{e:#}")));
                client::Connected { snapshot: Vec::new(), commands, events }
            });
            let new_window = ui::build_window(&app, c.commands, c.events, c.snapshot);
            new_window.present();
            window.replace(Some(new_window));
            starting.set(false);
            drop(hold);
        });
    });
    // GTK would otherwise parse leftover arguments as files to open.
    app.run_with_args(&[] as &[&str])
}
