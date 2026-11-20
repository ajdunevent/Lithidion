//! WebSockets for scripts. Each connection runs as a task on the backend's runtime and talks to
//! the script thread through its channel; the script's `WebSocket` objects only mirror them.
//!
//! `wss://` goes anywhere, as `https://` does. `ws://` is unencrypted, so it goes only to local
//! addresses: the name is resolved here and the connection made to the address that was checked,
//! so a name cannot be pointed somewhere else between the check and the connection.

use super::{In, MAX_BODY};
use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use url::Url;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// What the script asks of an open socket.
#[derive(Debug)]
pub enum Command {
    Text(String),
    Binary(Vec<u8>),
    Close(Option<u16>, String),
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a closing handshake may take before the connection is dropped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// The close code a script sees when the connection failed or dropped without one.
const ABNORMAL: u16 = 1006;

/// Parses a URL as a browser's `WebSocket` constructor does, with the WHATWG parser: surrounding
/// spaces are trimmed, the scheme may be in any case, `http` and `https` become `ws` and `wss`,
/// and a fragment is refused. Gives the URL as the script should see it.
pub fn parse(url: &str) -> Result<Url, String> {
    let mut parsed = Url::parse(url).map_err(|e| format!("{} is not a usable URL: {e}", url.trim()))?;
    let scheme = match parsed.scheme() {
        "ws" | "http" => "ws",
        "wss" | "https" => "wss",
        _ => return Err("only ws and wss are supported".into()),
    };
    if parsed.scheme() != scheme {
        let _ = parsed.set_scheme(scheme);
    }
    if parsed.fragment().is_some() {
        return Err("a WebSocket URL cannot have a fragment".into());
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(format!("{parsed} has no host"));
    }
    Ok(parsed)
}

/// Loopback, private, link-local, and shared (CGNAT, as Tailscale uses) addresses.
pub fn is_local(ip: IpAddr) -> bool {
    fn v4(a: Ipv4Addr) -> bool {
        let [x, y, ..] = a.octets();
        a.is_loopback() || a.is_private() || a.is_link_local() || (x == 100 && (64..128).contains(&y))
    }
    match ip {
        IpAddr::V4(a) => v4(a),
        IpAddr::V6(a) => {
            if let Some(m) = a.to_ipv4_mapped() {
                return v4(m);
            }
            let first = a.segments()[0];
            a == Ipv6Addr::LOCALHOST || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Runs one connection until it closes, the script closes it, or the script stops (its sender
/// is dropped). Events go to the script as `In::Socket`.
pub async fn session(
    id: u64,
    url: String,
    protocols: Vec<String>,
    mut commands: UnboundedReceiver<Command>,
    to_script: async_channel::Sender<In>,
    app: String,
) {
    let tell = |v: serde_json::Value| {
        let tx = to_script.clone();
        async move {
            let _ = tx.send(In::Socket(v)).await;
        }
    };
    let failed = |why: String| {
        let tell = &tell;
        async move {
            tell(json!({ "id": id, "kind": "error", "message": why })).await;
            tell(json!({ "id": id, "kind": "close", "code": ABNORMAL, "reason": "", "clean": false })).await;
        }
    };
    let host = Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();

    let opened = tokio::select! {
        r = tokio::time::timeout(CONNECT_TIMEOUT, open(&url, &protocols)) => r.unwrap_or_else(|_| Err(anyhow::anyhow!("the connection timed out"))),
        c = commands.recv() => {
            // Closed while connecting, or the script stopped.
            if c.is_some() {
                failed("the socket was closed before it connected".into()).await;
            }
            return;
        }
    };
    let (mut ws, protocol) = match opened {
        Ok(o) => o,
        Err(e) => {
            tracing::info!(%app, %host, error = %format!("{e:#}"), "socket failed");
            failed(format!("{e:#}")).await;
            return;
        }
    };
    tracing::info!(%app, %host, "socket opened");
    tell(json!({ "id": id, "kind": "open", "protocol": protocol })).await;

    let mut closed: Option<(u16, String)> = None;
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let wait = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            m = ws.next() => match m {
                Some(Ok(Message::Text(t))) => tell(json!({ "id": id, "kind": "message", "text": t.as_str() })).await,
                Some(Ok(Message::Binary(b))) => {
                    tell(json!({ "id": id, "kind": "message", "base64": base64::engine::general_purpose::STANDARD.encode(&b) })).await
                }
                Some(Ok(Message::Close(frame))) => {
                    closed = Some(frame.map(|f| (u16::from(f.code), f.reason.to_string())).unwrap_or((1005, String::new())));
                    deadline.get_or_insert_with(|| tokio::time::Instant::now() + CLOSE_TIMEOUT);
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    if closed.is_none() {
                        tracing::info!(%app, %host, error = %e, "socket dropped");
                        failed(e.to_string()).await;
                        return;
                    }
                    break;
                }
                None => break,
            },
            c = commands.recv() => match c {
                Some(Command::Text(t)) => {
                    if let Err(e) = ws.send(Message::text(t)).await {
                        tracing::debug!(%app, error = %e, "socket send failed");
                    }
                }
                Some(Command::Binary(b)) => {
                    if let Err(e) = ws.send(Message::binary(b)).await {
                        tracing::debug!(%app, error = %e, "socket send failed");
                    }
                }
                Some(Command::Close(code, reason)) => {
                    let frame = code.map(|c| CloseFrame { code: CloseCode::from(c), reason: reason.into() });
                    let _ = ws.close(frame).await;
                    deadline.get_or_insert_with(|| tokio::time::Instant::now() + CLOSE_TIMEOUT);
                }
                None => {
                    let _ = tokio::time::timeout(CLOSE_TIMEOUT, ws.close(None)).await;
                    tracing::info!(%app, %host, "socket closed with the script");
                    return;
                }
            },
            _ = wait => break,
        }
    }
    let (code, reason, clean) = match closed {
        Some((code, reason)) => (code, reason, true),
        None => (ABNORMAL, String::new(), false),
    };
    tracing::info!(%app, %host, code, "socket closed");
    tell(json!({ "id": id, "kind": "close", "code": code, "reason": reason, "clean": clean })).await;
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connects, checking `ws://` addresses first. Gives the subprotocol the server chose.
async fn open(url: &str, protocols: &[String]) -> anyhow::Result<(Socket, String)> {
    let parsed = Url::parse(url)?;
    let secure = parsed.scheme() == "wss";
    let port = parsed.port_or_known_default().unwrap_or(if secure { 443 } else { 80 });
    let host = parsed.host_str().unwrap_or_default().to_string();
    let mut addrs: Vec<SocketAddr> = match parsed.host() {
        Some(url::Host::Ipv4(a)) => vec![SocketAddr::new(a.into(), port)],
        Some(url::Host::Ipv6(a)) => vec![SocketAddr::new(a.into(), port)],
        _ => tokio::net::lookup_host((host.as_str(), port)).await?.collect(),
    };
    if !secure {
        addrs.retain(|a| is_local(a.ip()));
        anyhow::ensure!(!addrs.is_empty(), "ws:// is only for local addresses, and {host} is not one; use wss://");
    }
    let tcp = TcpStream::connect(&addrs[..]).await?;
    let mut request = url.into_client_request()?;
    let headers = request.headers_mut();
    headers.insert("User-Agent", HeaderValue::from_static(concat!("Lithidion/", env!("CARGO_PKG_VERSION"))));
    if !protocols.is_empty() {
        headers.insert("Sec-WebSocket-Protocol", HeaderValue::from_str(&protocols.join(", "))?);
    }
    let config = WebSocketConfig::default().max_message_size(Some(MAX_BODY)).max_frame_size(Some(MAX_BODY));
    let (ws, reply) = tokio_tungstenite::client_async_tls_with_config(request, tcp, Some(config), None).await?;
    let protocol = reply
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Ok((ws, protocol))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses() {
        for a in ["127.0.0.1", "10.1.2.3", "172.20.0.1", "192.168.1.10", "169.254.3.4", "100.101.102.103", "::1", "fd12::1", "fe80::1", "::ffff:192.168.0.2"] {
            assert!(is_local(a.parse().unwrap()), "{a}");
        }
        for a in ["8.8.8.8", "172.32.0.1", "100.128.0.1", "2001:db8::1", "2606:4700::1111", "::ffff:1.1.1.1"] {
            assert!(!is_local(a.parse().unwrap()), "{a}");
        }
    }

    #[test]
    fn urls() {
        let ok = |u: &str| parse(u).map(|u| u.to_string());
        assert_eq!(ok("ws://homeassistant.local:8123/api/websocket").unwrap(), "ws://homeassistant.local:8123/api/websocket");
        assert_eq!(ok(" WS://ha.local:8123/api/websocket\n").unwrap(), "ws://ha.local:8123/api/websocket");
        assert_eq!(ok("http://ha.local/api/websocket").unwrap(), "ws://ha.local/api/websocket");
        assert_eq!(ok("https://example.com").unwrap(), "wss://example.com/");
        assert_eq!(ok("ws://ha.local/my home/").unwrap(), "ws://ha.local/my%20home/");
        assert_eq!(ok("ws://[::1]:8080/").unwrap(), "ws://[::1]:8080/");
        assert!(parse("ftp://example.com/").is_err());
        assert!(parse("ws://example.com/#x").is_err());
        assert!(parse("ws://ha.local:8123 /api").is_err());
        assert!(parse("not a url").is_err());
    }
}

