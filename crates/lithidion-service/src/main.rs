use lithidion_core::{backend, database_path, host, service};

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

fn main() -> std::process::ExitCode {
    use_host_system_bus();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "lithidion_core=info,lithidion_service=info".into()),
        )
        .init();
    // JavaScriptCore takes the first thread to use it as its main thread; this one outlives every
    // script.
    let _jsc = javascriptcore6::Context::new();
    host::detect();
    tracing::info!(zone = %backend::time_zone(), "time zone");

    let (commands, events) = backend::spawn(database_path());
    let main = glib::MainLoop::new(None, false);
    let failed = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let (main, failed) = (main.clone(), failed.clone());
        glib::MainContext::default().spawn_local(async move {
            if let Err(e) = service::serve(commands, events).await {
                tracing::error!(error = format!("{e:#}"), "cannot serve on the session bus");
                failed.set(true);
            }
            main.quit();
        });
    }
    main.run();
    if failed.get() {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
