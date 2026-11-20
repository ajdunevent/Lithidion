//! An app's settings page, which its script names, shown in WebKit. The page ends by going to
//! `pebblejs://close#<response>`; the response goes back to the script.

use super::state::State;
use crate::backend::Command;
use adw::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use uuid::Uuid;
use webkit6::prelude::*;

const CLOSE: &str = "pebblejs://close";

/// What a close URL carries, decoded once. Apps also end with `pebblejs://close/?…` or
/// `pebblejs://close/…`, which the Pebble app accepted too.
pub fn response(url: &str) -> Option<String> {
    let rest = url.strip_prefix(CLOSE)?;
    let raw = ["#", "/?", "/"].iter().find_map(|p| rest.strip_prefix(p)).unwrap_or(rest);
    Some(glib::Uri::unescape_string(raw, None::<&str>).map(|s| s.to_string()).unwrap_or_else(|| raw.to_string()))
}

/// A web view that hands the close URL's response to `closed`, once, and sends links meant for a
/// new window to the browser.
fn view(closed: impl Fn(String) + 'static) -> webkit6::WebView {
    let view = webkit6::WebView::new();
    view.set_vexpand(true);
    if let Some(s) = WebViewExt::settings(&view) {
        s.set_enable_developer_extras(false);
        s.set_javascript_can_open_windows_automatically(false);
    }
    view.connect_decide_policy(move |_, decision, kind| {
        let Some(nav) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>() else { return false };
        let Some(target) = nav.navigation_action().and_then(|a| a.request()).and_then(|r| r.uri()) else { return false };
        if let Some(reply) = response(&target) {
            decision.ignore();
            closed(reply);
            return true;
        }
        if kind == webkit6::PolicyDecisionType::NewWindowAction {
            decision.ignore();
            gtk::UriLauncher::new(&target).launch(None::<&gtk::Window>, None::<&gio::Cancellable>, |_| {});
            return true;
        }
        false
    });
    view
}

/// A page's address for the log: `data:` pages, which hold the whole page, by their size.
fn page_name(url: &str) -> String {
    match url.strip_prefix("data:") {
        Some(rest) => format!("data: ({} bytes)", rest.len()),
        None => url.split(['?', '#']).next().unwrap_or(url).to_string(),
    }
}

pub fn show(state: &Rc<State>, parent: &impl IsA<gtk::Widget>, uuid: Uuid, title: &str, url: &str) {
    let answered = Rc::new(Cell::new(false));
    let dialog = adw::Dialog::builder().title(title).content_width(440).content_height(720).build();
    let view = {
        let (state, d, answered) = (state.clone(), dialog.downgrade(), answered.clone());
        view(move |reply| {
            if !answered.replace(true) {
                state.send(Command::AppSettingsClosed(uuid, reply));
            }
            if let Some(d) = d.upgrade() {
                d.close();
            }
        })
    };
    // WebKit's failures, which otherwise leave the page blank.
    let failed = adw::StatusPage::builder().icon_name("lithidion-image-missing-symbolic").title("Page Not Shown").build();
    let stack = gtk::Stack::new();
    stack.add_named(&view, Some("page"));
    stack.add_named(&failed, Some("failed"));
    {
        let (stack, failed) = (stack.clone(), failed.clone());
        view.connect_web_process_terminated(move |_, reason| {
            tracing::warn!(?reason, "the settings page's web process stopped");
            failed.set_description(Some("The web process stopped"));
            stack.set_visible_child_name("failed");
        });
    }
    {
        let (stack, failed, answered) = (stack.clone(), failed.clone(), answered.clone());
        view.connect_load_failed(move |_, _, uri, error| {
            // The close URL is stopped on purpose.
            if answered.get() || response(uri).is_some() {
                return false;
            }
            tracing::warn!(url = %page_name(uri), error = %error, "the settings page did not load");
            failed.set_description(Some(&super::state::sentence(error.message())));
            stack.set_visible_child_name("failed");
            true
        });
    }
    let started = Rc::new(Cell::new(false));
    {
        let started = started.clone();
        view.connect_load_changed(move |v, event| {
            started.set(true);
            tracing::debug!(?event, url = %v.uri().map(|u| page_name(&u)).unwrap_or_default(), "settings page");
        });
    }
    {
        // WebKit says nothing when its page process cannot start.
        let (stack, failed) = (stack.downgrade(), failed.downgrade());
        glib::timeout_add_seconds_local_once(10, move || {
            let (Some(stack), Some(failed)) = (stack.upgrade(), failed.upgrade()) else { return };
            if !started.get() {
                tracing::warn!("the settings page did not start loading; WebKit's page process may not have started");
                failed.set_description(Some("WebKit did not start"));
                stack.set_visible_child_name("failed");
            }
        });
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&stack));
    dialog.set_child(Some(&toolbar));
    tracing::info!(url = %page_name(url), "settings page opened");

    {
        let (state, answered) = (state.clone(), answered.clone());
        dialog.connect_closed(move |_| {
            if !answered.replace(true) {
                state.send(Command::AppSettingsClosed(uuid, String::new()));
            }
        });
    }
    view.load_uri(url);
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::response;

    /// The app's own path: an `adw::Dialog` on an application window, opened from the main loop
    /// while a script runs on its thread. `xvfb-run cargo test -- --ignored settings_dialog`.
    #[test]
    #[ignore]
    fn settings_dialog() {
        use adw::prelude::*;
        let _ = tracing_subscriber::fmt().with_env_filter("lithidion=debug").with_test_writer().try_init();
        let app = adw::Application::builder().application_id("net.dunevent.LithidionTest").build();
        app.connect_activate(|app| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let (tx, _out) = tokio::sync::mpsc::unbounded_channel();
            let script = crate::pkjs::Script {
                uuid: uuid::Uuid::nil(),
                title: "T".into(),
                code: "Pebble.addEventListener('ready', function () { console.log('hi'); });".into(),
                keys: Default::default(),
                location: false,
                watch: serde_json::json!({}),
                account_token: String::new(),
                watch_token: String::new(),
                profile: 1,
            };
            let db = std::sync::Arc::new(std::sync::Mutex::new(crate::store::Store::in_memory()));
            let runner = crate::pkjs::Runner::start(script, db, Default::default(), rt.handle().clone(), tx);
            std::thread::sleep(std::time::Duration::from_millis(300));
            let window = adw::ApplicationWindow::builder().application(app).default_width(400).default_height(700).build();
            window.present();
            let (ctx_tx, _rx) = async_channel::unbounded();
            let state = super::super::state::State::new(ctx_tx);
            super::show(&state, &window, uuid::Uuid::nil(), "Settings", "data:text/html,<p>Hello</p><a href='pebblejs://close#ok'>x</a>");
            let app = app.clone();
            glib::timeout_add_seconds_local_once(4, move || {
                drop(runner);
                drop(rt);
                app.quit();
            });
        });
        app.run_with_args::<&str>(&[]);
    }

    /// Shows the settings page a real app's script opens, submits it, and hands the response back
    /// to the script: `PKJS_PBW=app.pbw xvfb-run cargo test -- --ignored --nocapture settings_round_trip`.
    /// The page is saved to `/tmp/settings.png`.
    #[test]
    #[ignore]
    fn settings_round_trip() {
        use crate::pkjs::{In, Out, Runner, Script};
        use adw::prelude::*;
        use std::time::{Duration, Instant};
        use webkit6::prelude::*;
        adw::init().unwrap();
        let path = std::path::PathBuf::from(std::env::var("PKJS_PBW").expect("PKJS_PBW"));
        let (uuid, title, code, keys) = crate::backend::pbw_for_tests(&path);
        let rt = tokio::runtime::Runtime::new().unwrap();
        crate::net::set_runtime(rt.handle().clone());
        let (tx, mut out) = tokio::sync::mpsc::unbounded_channel();
        let script = Script {
            uuid,
            title,
            code,
            keys,
            location: false,
            watch: serde_json::json!({ "platform": "basalt", "model": "pebble_time_black", "language": "en_US",
                                       "firmware": { "major": 4, "minor": 9, "patch": 150, "suffix": "" } }),
            account_token: "a".into(),
            watch_token: "w".into(),
            profile: 1,
        };
        let db = std::sync::Arc::new(std::sync::Mutex::new(crate::store::Store::in_memory()));
        let runner = Runner::start(script, db, std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)), rt.handle().clone(), tx);
        let ctx = glib::MainContext::default();
        let wait_for = |out: &mut tokio::sync::mpsc::UnboundedReceiver<Out>, want: &dyn Fn(&Out) -> bool, runner: &Runner| -> Out {
            let end = Instant::now() + Duration::from_secs(15);
            loop {
                ctx.iteration(false);
                match out.try_recv() {
                    Ok(o) if want(&o) => return o,
                    Ok(Out::Send { transaction, .. }) => runner.send(In::Answer { transaction, ok: true }),
                    Ok(_) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
                assert!(Instant::now() < end, "timed out");
            }
        };
        wait_for(&mut out, &|o| *o == Out::Ready, &runner);
        runner.send(In::ShowSettings);
        let Out::Open(url) = wait_for(&mut out, &|o| matches!(o, Out::Open(_)), &runner) else { unreachable!() };

        let reply = std::rc::Rc::new(std::cell::RefCell::new(None));
        let r = reply.clone();
        let view = super::view(move |s| *r.borrow_mut() = Some(s));
        let window = adw::Window::builder().default_width(400).default_height(1400).content(&view).build();
        window.present();
        view.load_uri(&url);
        let end = Instant::now() + Duration::from_secs(20);
        while view.is_loading() || Instant::now() < end - Duration::from_secs(16) {
            ctx.iteration(false);
            assert!(Instant::now() < end, "the page did not load");
        }
        let settle = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < settle {
            ctx.iteration(false);
        }
        let paintable = gtk::WidgetPaintable::new(Some(&view));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, view.width() as f64, view.height() as f64);
        if let Some(node) = snapshot.to_node() {
            let texture = view.native().unwrap().renderer().unwrap().render_texture(&node, None);
            texture.save_to_png("/tmp/settings.png").unwrap();
        }
        // Clay's submit button, or any submit.
        view.evaluate_javascript(
            "(document.querySelector('[data-manipulator-type=\"submit\"], .clay-submit-button, button[type=submit], input[type=submit]') || {click(){}}).click()",
            None,
            None,
            None::<&gio::Cancellable>,
            |_| {},
        );
        let end = Instant::now() + Duration::from_secs(10);
        while reply.borrow().is_none() {
            ctx.iteration(false);
            assert!(Instant::now() < end, "the page did not close");
        }
        let response = reply.borrow_mut().take().unwrap();
        eprintln!("RESPONSE {}", &response[..response.len().min(400)]);
        // Messages the script sent on its own before now.
        while let Ok(o) = out.try_recv() {
            if let Out::Send { transaction, .. } = o {
                runner.send(In::Answer { transaction, ok: true });
            }
        }
        runner.send(In::SettingsClosed(response));
        let sent = wait_for(&mut out, &|o| matches!(o, Out::Send { .. }), &runner);
        eprintln!("SENT {sent:?}");
    }

    #[test]
    fn close_urls() {
        assert_eq!(response("pebblejs://close#%7B%22a%22%3A1%7D").as_deref(), Some("{\"a\":1}"));
        assert_eq!(response("pebblejs://close/?x=1").as_deref(), Some("x=1"));
        assert_eq!(response("pebblejs://close").as_deref(), Some(""));
        assert_eq!(response("https://example.com/"), None);
    }
}
