//! The tokio runtime the backend runs on, for network work started from the UI, and the one
//! HTTP client everything shares.

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub fn set_runtime(handle: tokio::runtime::Handle) {
    let _ = RUNTIME.set(handle);
}

/// Runs `f` on the backend's runtime; the handle can be awaited from the GTK main loop.
pub fn spawn<F>(f: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    RUNTIME.get().expect("the backend starts before the window").spawn(f)
}

/// Runs `f` on the backend's runtime and gives its result back to the GTK main loop.
pub async fn run<T: Send + 'static>(f: impl Future<Output = anyhow::Result<T>> + Send + 'static) -> anyhow::Result<T> {
    spawn(f).await.map_err(|e| anyhow::anyhow!("the request stopped: {e}"))?
}

pub fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("Lithidion/", env!("CARGO_PKG_VERSION"), " (https://dev.dunevent.net/aj/Lithidion)"))
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(120))
            .build()
            .expect("the TLS backend is built in")
    })
}
