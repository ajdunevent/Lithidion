//! BlueZ access through bluer, with the FuriOS behaviour found in the first Lithidion and in
//! Ringmaster built in (PROTOCOL.md section 3).

use anyhow::{anyhow, Context, Result};
use bluer::agent::{Agent, AgentHandle};
use bluer::gatt::remote::{Characteristic, CharacteristicWriteRequest};
use bluer::gatt::WriteOp;
use bluer::{Adapter, AdapterEvent, Address, Device, DiscoveryFilter, DiscoveryTransport};
use futures::{pin_mut, StreamExt};
use pebble_protocol::ble::{is_pebble_vendor, ScanRecord};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout, Instant};

pub struct Bluetooth {
    _session: bluer::Session,
    pub adapter: Adapter,
    _agent: AgentHandle,
}

impl Bluetooth {
    pub async fn open() -> Result<Bluetooth> {
        let session = bluer::Session::new().await.context("cannot reach the Bluetooth service")?;
        let adapter = session.default_adapter().await.context("no Bluetooth adapter")?;
        if !adapter.is_powered().await.unwrap_or(false) {
            adapter.set_powered(true).await.context("cannot turn Bluetooth on")?;
        }
        let agent = session.register_agent(agent()).await.context("cannot register the pairing agent")?;
        Ok(Bluetooth { _session: session, adapter, _agent: agent })
    }
}

/// Accepts every pairing prompt; the watch shows its own confirmation. Without a
/// `request_confirmation` handler BlueZ rejects numeric comparison.
fn agent() -> Agent {
    Agent {
        request_default: true,
        request_confirmation: Some(Box::new(|req| {
            Box::pin(async move {
                tracing::info!(device = %req.device, passkey = req.passkey, "pairing confirmation accepted");
                Ok(())
            })
        })),
        request_authorization: Some(Box::new(|req| {
            Box::pin(async move {
                tracing::info!(device = %req.device, "pairing authorization accepted");
                Ok(())
            })
        })),
        authorize_service: Some(Box::new(|_| Box::pin(async { Ok(()) }))),
        ..Default::default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub address: Address,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    pub record: Option<ScanRecord>,
}

async fn pebble_record(device: &Device) -> Option<ScanRecord> {
    let data = device.manufacturer_data().await.ok().flatten()?;
    data.iter().find(|(company, _)| is_pebble_vendor(**company)).and_then(|(_, bytes)| ScanRecord::parse(bytes).ok())
}

/// Sends every Pebble advertising nearby, again whenever its signal or data changes, until
/// `limit` passes or the receiver is dropped. Watches in `skip` are left out.
pub async fn scan(adapter: &Adapter, limit: Duration, skip: Vec<Address>, tx: mpsc::UnboundedSender<Found>) -> Result<()> {
    let filter = DiscoveryFilter { transport: DiscoveryTransport::Le, duplicate_data: true, ..Default::default() };
    // FuriOS: "Operation already in progress" here still leaves a working scan.
    if let Err(e) = adapter.set_discovery_filter(filter).await {
        tracing::debug!(error = %e, "discovery filter not set");
    }
    let events = adapter.discover_devices_with_changes().await.context("cannot start a Bluetooth scan")?;
    pin_mut!(events);
    let deadline = Instant::now() + limit;
    let mut seen: HashMap<Address, Found> = HashMap::new();
    loop {
        let event = match timeout(deadline.saturating_duration_since(Instant::now()), events.next()).await {
            Ok(Some(e)) => e,
            Ok(None) | Err(_) => return Ok(()),
        };
        let address = match event {
            AdapterEvent::DeviceAdded(a) => a,
            AdapterEvent::DeviceRemoved(_) | AdapterEvent::PropertyChanged(_) => continue,
        };
        if skip.contains(&address) {
            continue;
        }
        let Ok(device) = adapter.device(address) else { continue };
        let Some(record) = pebble_record(&device).await else { continue };
        let found =
            Found { address, name: device.name().await.ok().flatten(), rssi: device.rssi().await.ok().flatten(), record: Some(record) };
        if seen.get(&address) != Some(&found) {
            seen.insert(address, found.clone());
            if tx.send(found).is_err() {
                return Ok(());
            }
        }
    }
}

const POLL: Duration = Duration::from_millis(500);

/// `connect()` may never return on FLX1s although the link comes up, so the state is polled.
pub async fn connect(device: &Device) -> Result<()> {
    if device.is_connected().await.unwrap_or(false) {
        return Ok(());
    }
    match timeout(Duration::from_secs(5), device.connect()).await {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(e)) => tracing::debug!(error = %e, "connect() failed; polling the connection state"),
        Err(_) => tracing::debug!("connect() did not return; polling the connection state"),
    }
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        if device.is_connected().await.unwrap_or(false) {
            return Ok(());
        }
        sleep(POLL).await;
    }
    Err(anyhow!("the watch did not answer; keep it nearby with Bluetooth on"))
}

/// After bonding BlueZ reads the services again, and a lookup during that fails with "GATT
/// services have not been resolved". Waits for the re-read to start, then for it to finish.
pub async fn wait_reresolved(device: &Device) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && device.is_services_resolved().await.unwrap_or(false) {
        sleep(Duration::from_millis(200)).await;
    }
    wait_ready(device).await
}

/// Waits until the services can be read.
pub async fn wait_ready(device: &Device) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if !device.is_connected().await.unwrap_or(false) {
            connect(device).await?;
        }
        if device.is_services_resolved().await.unwrap_or(false) && device.services().await.is_ok_and(|s| !s.is_empty()) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("the watch's services could not be read"));
        }
        sleep(POLL).await;
    }
}

/// Asks BlueZ to bond, or only waits for the watch to start it, polling `Paired` since `Pair()`
/// may not return. Needs a confirmation on the watch, so it allows 60 s.
pub async fn bond(device: &Device, phone_starts: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    tracing::info!(phone_starts, "bonding");
    if phone_starts {
        let pairing = device.pair();
        pin_mut!(pairing);
        while Instant::now() < deadline {
            tokio::select! {
                result = &mut pairing => {
                    match result {
                        Ok(()) => tracing::debug!("pair() returned"),
                        Err(e) => tracing::warn!(error = %e, "pair() returned an error"),
                    }
                    break;
                }
                _ = sleep(POLL) => {
                    let paired = device.is_paired().await.unwrap_or(false);
                    if paired || !device.is_connected().await.unwrap_or(false) {
                        break;
                    }
                }
            }
        }
    }
    // Pairing needs the link; once it has been down a few seconds, it is over.
    let mut down_since: Option<Instant> = None;
    while Instant::now() < deadline {
        if device.is_paired().await.unwrap_or(false) {
            let _ = device.set_trusted(true).await;
            return Ok(());
        }
        if device.is_connected().await.unwrap_or(false) {
            down_since = None;
        } else if down_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(3) {
            tracing::info!("the link dropped during pairing");
            break;
        }
        sleep(POLL).await;
    }
    let (connected, paired) = (device.is_connected().await.ok(), device.is_paired().await.ok());
    tracing::info!(?connected, ?paired, "bonding timed out");
    Err(anyhow!("pairing did not finish; confirm the request on the watch"))
}

/// Disconnects first: removing a connected device can fail.
pub async fn remove_bond(adapter: &Adapter, address: Address) -> Result<()> {
    if let Ok(device) = adapter.device(address) {
        let _ = timeout(Duration::from_secs(5), device.disconnect()).await;
    }
    match adapter.remove_device(address).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind == bluer::ErrorKind::DoesNotExist => Ok(()),
        Err(e) => Err(e).context("cannot remove the Bluetooth bond"),
    }
}

/// Keeps an LE scan running until dropped.
pub struct Sighting {
    pub device: Device,
    scan: tokio::task::AbortHandle,
}

impl Drop for Sighting {
    fn drop(&mut self) {
        self.scan.abort();
    }
}

/// Scans LE-only until `address` advertises. A device BlueZ already knows is reported once from
/// its cache when the scan starts; that report is skipped, so only a live advertisement counts.
/// The scan keeps running while the returned value lives: connecting during it is what worked on
/// the x86_64 desktop, where a connection without it went to classic Bluetooth and hung.
pub async fn sight(adapter: &Adapter, address: Address, limit: Duration) -> Result<Sighting> {
    let filter = DiscoveryFilter { transport: DiscoveryTransport::Le, ..Default::default() };
    if let Err(e) = adapter.set_discovery_filter(filter).await {
        tracing::info!(error = %e, "LE-only discovery filter not set");
    }
    let cached = adapter.device_addresses().await.unwrap_or_default().contains(&address);
    let events = adapter.discover_devices_with_changes().await.context("cannot start a Bluetooth scan")?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let scan = tokio::spawn(async move {
        pin_mut!(events);
        let mut skip_cached = cached;
        while let Some(e) = events.next().await {
            if let AdapterEvent::DeviceAdded(a) = e {
                if a != address {
                    continue;
                }
                if skip_cached {
                    skip_cached = false;
                    continue;
                }
                let _ = tx.send(());
            }
        }
    })
    .abort_handle();
    let sighting = Sighting { device: adapter.device(address)?, scan };
    match timeout(limit, rx.recv()).await {
        Ok(Some(())) => Ok(sighting),
        _ => Err(anyhow!("the watch is not advertising; press a button to wake it and keep it nearby")),
    }
}

/// Bit 2 of the advertising flags: "BR/EDR not supported".
pub async fn advertises_le_only(device: &Device) -> Option<bool> {
    device.advertising_flags().await.ok().flatten().and_then(|f| f.first().map(|b| b & 0x04 != 0))
}

/// Address type, advertising flags, and services, for the log.
pub async fn describe(device: &Device) -> String {
    let address_type = device.address_type().await.map(|t| t.to_string()).unwrap_or_else(|_| "unknown".into());
    let flags = device.advertising_flags().await.map(|f| format!("{f:02x?}")).unwrap_or_else(|_| "none".into());
    let uuids = device.uuids().await.ok().flatten().map(|u| u.len()).unwrap_or(0);
    let rssi = device.rssi().await.ok().flatten();
    format!("address type {address_type}, advertising flags {flags}, {uuids} services, rssi {rssi:?}")
}

/// Characteristics of `service` named in `wanted`, in that order. Retried for a few seconds
/// while BlueZ is still reading the services.
pub async fn characteristics(device: &Device, service: bluer::Uuid, wanted: &[bluer::Uuid]) -> Result<Vec<Option<Characteristic>>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match characteristics_once(device, service, wanted).await {
            Ok(found) => return Ok(found),
            Err(e) if Instant::now() < deadline => {
                tracing::debug!(error = %e, "services not readable yet");
                sleep(POLL).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn characteristics_once(device: &Device, service: bluer::Uuid, wanted: &[bluer::Uuid]) -> Result<Vec<Option<Characteristic>>> {
    let mut found = vec![None; wanted.len()];
    for s in device.services().await? {
        if s.uuid().await? != service {
            continue;
        }
        for ch in s.characteristics().await? {
            let uuid = ch.uuid().await?;
            if let Some(i) = wanted.iter().position(|w| *w == uuid) {
                found[i] = Some(ch);
            }
        }
    }
    Ok(found)
}

pub async fn has_service(device: &Device, service: bluer::Uuid) -> bool {
    let Ok(services) = device.services().await else { return false };
    for s in services {
        if s.uuid().await.is_ok_and(|u| u == service) {
            return true;
        }
    }
    false
}

pub fn request(op_type: WriteOp) -> CharacteristicWriteRequest {
    CharacteristicWriteRequest { op_type, ..Default::default() }
}
