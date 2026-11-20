//! Bringing a watch up to an open PPoGATT transport (PROTOCOL.md section 3): connect, read the
//! connectivity status, pair when needed, and pick reversed or forward PPoGATT.

use super::bluetooth::{self, characteristics, has_service, request};
use super::gatt_server::ForwardServer;
use anyhow::{anyhow, Context, Result};
use bluer::gatt::remote::Characteristic;
use bluer::gatt::WriteOp;
use bluer::{Adapter, Address, Device};
use futures::{pin_mut, StreamExt};
use pebble_protocol::ble::{self as pble, Connectivity};
use pebble_protocol::hardware::AppPlatform;
use pebble_protocol::ppog::Role;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, timeout, Instant};

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    ReversedV2,
    Forward,
}

impl Transport {
    pub fn label(self) -> &'static str {
        match self {
            Transport::ReversedV2 => "Reversed PPoGATT (watch hosts)",
            Transport::Forward => "Forward PPoGATT (Lithidion hosts)",
        }
    }

    pub fn role(self) -> Role {
        match self {
            Transport::ReversedV2 => Role::Reversed,
            Transport::Forward => Role::Forward,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Connecting,
    Pairing,
    Opening,
}

enum Writer {
    Reversed(Characteristic),
    Forward(Arc<ForwardServer>),
}

/// A watch link carrying raw PPoGATT packets. Dropping it stops its readers: BlueZ can drop a
/// link without ending a notify stream, and a reader left running keeps bluer's notification
/// session for the next link, which then hears nothing.
pub struct Link {
    pub device: Device,
    pub transport: Transport,
    writer: Writer,
    pub inbound: mpsc::UnboundedReceiver<Vec<u8>>,
    pub battery: mpsc::UnboundedReceiver<u8>,
    tasks: Vec<tokio::task::AbortHandle>,
}

impl Drop for Link {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Link {
    /// Battery levels from the watch, taken once; later calls get a closed receiver.
    pub fn take_battery(&mut self) -> mpsc::UnboundedReceiver<u8> {
        std::mem::replace(&mut self.battery, mpsc::unbounded_channel().1)
    }

    pub async fn write(&self, packet: &[u8]) -> Result<()> {
        match &self.writer {
            Writer::Reversed(ch) => ch.write_ext(packet, &request(WriteOp::Command)).await.context("PPoGATT write failed"),
            Writer::Forward(server) => server.send(packet).await,
        }
    }

    /// The ATT MTU, once known.
    pub async fn mtu(&self) -> usize {
        match &self.writer {
            Writer::Reversed(ch) => ch.mtu().await.unwrap_or(pebble_protocol::ppog::DEFAULT_MTU),
            Writer::Forward(server) => server.mtu(),
        }
    }
}

pub type ServerSlot = Arc<Mutex<Option<Arc<ForwardServer>>>>;

async fn forward_server(adapter: &Adapter, slot: &ServerSlot) -> Result<Arc<ForwardServer>> {
    let mut guard = slot.lock().await;
    if let Some(s) = guard.as_ref() {
        return Ok(s.clone());
    }
    let server = Arc::new(ForwardServer::register(adapter).await?);
    *guard = Some(server.clone());
    Ok(server)
}

async fn read_connectivity(ch: &Characteristic) -> Result<Connectivity> {
    let value = ch.read().await.context("cannot read the watch's connectivity status")?;
    Ok(Connectivity::parse(&value)?)
}

/// The watch had lost this phone's bond, which was removed; connecting again pairs afresh.
#[derive(Debug)]
struct BondRemoved;

impl std::fmt::Display for BondRemoved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the watch no longer had this {}'s pairing", crate::host::noun())
    }
}

impl std::error::Error for BondRemoved {}

/// Pairing the phone started did not finish; the watch may still pair if it starts it.
#[derive(Debug)]
struct PairingStalled(anyhow::Error);

impl std::fmt::Display for PairingStalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for PairingStalled {}

/// Connects and opens a transport. `platform` comes from an earlier session or the scan record
/// and decides whether the pairing trigger is used; unknown means it is.
pub async fn establish(
    adapter: &Adapter,
    address: Address,
    platform: Option<AppPlatform>,
    server: &ServerSlot,
    stage: impl Fn(Stage),
) -> Result<Link> {
    let mut watch_starts = false;
    let mut bond_removed = false;
    loop {
        match establish_once(adapter, address, platform, server, &stage, watch_starts).await {
            Err(e) if e.is::<BondRemoved>() && !bond_removed => bond_removed = true,
            // As the official iOS app pairs: the watch asks for security and the phone answers.
            Err(e) if e.is::<PairingStalled>() && !watch_starts => {
                tracing::info!("pairing stalled; trying again with the watch starting it");
                if let Err(e) = bluetooth::remove_bond(adapter, address).await {
                    tracing::debug!(error = format!("{e:#}"), "nothing to remove");
                }
                sleep(Duration::from_secs(2)).await;
                watch_starts = true;
            }
            Err(e) => return Err(e.downcast::<PairingStalled>().map(|p| p.0).unwrap_or_else(|e| e)),
            ok => return ok,
        }
    }
}

async fn establish_once(
    adapter: &Adapter,
    address: Address,
    platform: Option<AppPlatform>,
    server: &ServerSlot,
    stage: &impl Fn(Stage),
    watch_starts: bool,
) -> Result<Link> {
    stage(Stage::Connecting);
    let mut device = adapter.device(address)?;
    let bonded = device.is_paired().await.unwrap_or(false);
    if !device.is_connected().await.unwrap_or(false) {
        if bonded {
            bluetooth::connect(&device).await?;
        } else {
            // BlueZ's record of an unpaired watch can carry stale classic-Bluetooth state.
            let _ = adapter.remove_device(address).await;
            let sighting = bluetooth::sight(adapter, address, Duration::from_secs(30)).await?;
            device = sighting.device.clone();
            let details = bluetooth::describe(&device).await;
            tracing::info!(details, "watch seen");
            let _ = device.set_trusted(true).await;
            if let Err(e) = bluetooth::connect(&device).await {
                return Err(match bluetooth::advertises_le_only(&device).await {
                    Some(false) => e.context("the watch advertises classic Bluetooth too, and BlueZ may have tried that"),
                    _ => e,
                });
            }
            drop(sighting);
        }
    }
    bluetooth::wait_ready(&device).await?;

    let mut pairing =
        characteristics(&device, pble::PAIRING_SERVICE, &[pble::CONNECTIVITY, pble::PAIRING_TRIGGER, pble::CONNECTION_PARAMETERS])
            .await?
            .into_iter();
    let connectivity_ch = pairing.next().flatten().ok_or_else(|| anyhow!("this device is not a Pebble watch (no pairing service)"))?;
    let trigger_ch = pairing.next().flatten();
    let params_ch = pairing.next().flatten();

    let transport = if has_service(&device, pble::PPOG_V2_SERVICE).await { Transport::ReversedV2 } else { Transport::Forward };
    tracing::info!(?transport, "transport chosen");
    let forward = match transport {
        Transport::Forward => Some(forward_server(adapter, server).await?),
        Transport::ReversedV2 => None,
    };
    // Attached before pairing: the watch starts a forward session as soon as the link encrypts.
    let forward_inbound = match &forward {
        Some(s) => Some(s.attach().await),
        None => None,
    };

    if let Some(ch) = &params_ch {
        if let Err(e) = ch.write_ext(&pble::CONNECTION_PARAMETERS_VALUE, &request(WriteOp::Request)).await {
            tracing::info!(error = %e, "connection parameters not set (older firmware lacks them)");
        }
    }

    let mut connectivity = read_connectivity(&connectivity_ch).await?;
    tracing::info!(?connectivity, "connectivity");
    let phone_bonded = device.is_paired().await.unwrap_or(false);
    let mut needs_pairing = !connectivity.paired || !phone_bonded;
    if !needs_pairing && !connectivity.encrypted {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !connectivity.encrypted {
            sleep(Duration::from_millis(500)).await;
            connectivity = read_connectivity(&connectivity_ch).await?;
        }
        needs_pairing = !connectivity.encrypted;
    }

    if needs_pairing {
        stage(Stage::Pairing);
        if phone_bonded {
            tracing::info!("the watch no longer has this phone's bond; pairing again");
            bluetooth::remove_bond(adapter, address).await?;
            return Err(BondRemoved.into());
        }
        let uses_trigger = watch_starts || platform.is_none_or(|p| p.needs_pairing_trigger());
        let mut phone_starts = true;
        if let (true, Some(trigger)) = (uses_trigger, &trigger_ch) {
            let writable = trigger.flags().await.map(|f| f.write).unwrap_or(false);
            if writable {
                let (value, phone_bonds) =
                    if watch_starts { (pble::WATCH_STARTS_PAIRING, false) } else { pble::pairing_trigger(&connectivity) };
                phone_starts = phone_bonds;
                if let Err(e) = trigger.write_ext(&[value], &request(WriteOp::Request)).await {
                    // A failed trigger write is ignored, as in the official app.
                    tracing::warn!(error = %e, "pairing trigger write failed");
                }
            } else if let Err(e) = trigger.read().await {
                tracing::warn!(error = %e, "pairing trigger read failed");
            }
        }
        if let Err(e) = bluetooth::bond(&device, phone_starts).await {
            return Err(if phone_starts { PairingStalled(e).into() } else { e });
        }
        bluetooth::wait_reresolved(&device).await?;
    }

    stage(Stage::Opening);
    let (tx, inbound) = mpsc::unbounded_channel();
    let mut tasks = Vec::new();
    let writer = match transport {
        Transport::ReversedV2 => {
            let mut v2 =
                characteristics(&device, pble::PPOG_V2_SERVICE, &[pble::PPOG_V2_FROM_WATCH, pble::PPOG_V2_TO_WATCH]).await?.into_iter();
            let from_watch = v2.next().flatten().ok_or_else(|| anyhow!("the watch's PPoGATT service has no data characteristic"))?;
            let to_watch = v2.next().flatten().ok_or_else(|| anyhow!("the watch's PPoGATT service has no write characteristic"))?;
            let stream = from_watch.notify().await.context("cannot subscribe to the watch's PPoGATT data")?;
            tasks.push(
                tokio::spawn(async move {
                    pin_mut!(stream);
                    while let Some(packet) = stream.next().await {
                        if tx.send(packet).is_err() {
                            break;
                        }
                    }
                })
                .abort_handle(),
            );
            Writer::Reversed(to_watch)
        }
        Transport::Forward => {
            let mut rx = forward_inbound.expect("attached for forward");
            tasks.push(
                tokio::spawn(async move {
                    while let Some(packet) = rx.recv().await {
                        if tx.send(packet).is_err() {
                            break;
                        }
                    }
                })
                .abort_handle(),
            );
            Writer::Forward(forward.expect("registered for forward"))
        }
    };

    let (battery_tx, battery) = mpsc::unbounded_channel();
    let level =
        characteristics(&device, pble::BATTERY_SERVICE, &[pble::BATTERY_LEVEL]).await.ok().and_then(|c| c.into_iter().next().flatten());
    if let Some(level) = level {
        tasks.push(
            tokio::spawn(async move {
                if let Some(v) = level.read().await.ok().and_then(|v| pble::battery_level(&v)) {
                    let _ = battery_tx.send(v);
                }
                // Subscribing too early fails with "Authorization is insufficient".
                sleep(Duration::from_secs(2)).await;
                let Ok(stream) = level.notify().await else { return };
                pin_mut!(stream);
                while let Some(v) = stream.next().await {
                    if let Some(v) = pble::battery_level(&v) {
                        if battery_tx.send(v).is_err() {
                            break;
                        }
                    }
                }
            })
            .abort_handle(),
        );
    }

    Ok(Link { device, transport, writer, inbound, battery, tasks })
}

/// Waits until the device reports a disconnection.
pub async fn disconnected(device: &Device) {
    let Ok(events) = device.events().await else {
        return std::future::pending().await;
    };
    pin_mut!(events);
    while let Some(bluer::DeviceEvent::PropertyChanged(p)) = events.next().await {
        if let bluer::DeviceProperty::Connected(false) = p {
            return;
        }
    }
    std::future::pending().await
}

pub async fn with_limit<T>(limit: Duration, what: &str, f: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    timeout(limit, f).await.map_err(|_| anyhow!("{what} took too long"))?
}
