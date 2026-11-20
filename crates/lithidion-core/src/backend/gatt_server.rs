//! The phone's forward PPoGATT service (PROTOCOL.md section 2), for watches without reversed
//! PPoGATT. Registered once, on first need, and kept: the official app found that removing and
//! adding the service again breaks connectivity.

use anyhow::{anyhow, Context, Result};
use bluer::gatt::local::{
    Application, ApplicationHandle, Characteristic, CharacteristicNotifier, CharacteristicNotify, CharacteristicNotifyMethod,
    CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod, Service,
};
use bluer::Adapter;
use futures::FutureExt;
use pebble_protocol::ble::{FORWARD_META_VALUE, PPOG_FORWARD_DATA, PPOG_FORWARD_META, PPOG_FORWARD_SERVICE};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};

type Inbound = Arc<Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>>;

pub struct ForwardServer {
    _app: ApplicationHandle,
    inbound: Inbound,
    notifier: Arc<tokio::sync::Mutex<Option<CharacteristicNotifier>>>,
    subscribed: Arc<Notify>,
    mtu: Arc<AtomicUsize>,
}

impl ForwardServer {
    pub async fn register(adapter: &Adapter) -> Result<ForwardServer> {
        let inbound: Inbound = Arc::default();
        let notifier: Arc<tokio::sync::Mutex<Option<CharacteristicNotifier>>> = Arc::default();
        let subscribed = Arc::new(Notify::new());
        let mtu = Arc::new(AtomicUsize::new(pebble_protocol::ppog::DEFAULT_MTU));

        let write = {
            let (inbound, mtu) = (inbound.clone(), mtu.clone());
            CharacteristicWriteMethod::Fun(Box::new(move |value, req| {
                mtu.fetch_max(req.mtu as usize, Ordering::Relaxed);
                if let Some(tx) = inbound.lock().expect("inbound lock").as_ref() {
                    let _ = tx.send(value);
                }
                async { Ok(()) }.boxed()
            }))
        };
        let notify = {
            let (notifier, subscribed) = (notifier.clone(), subscribed.clone());
            CharacteristicNotifyMethod::Fun(Box::new(move |n| {
                let (notifier, subscribed) = (notifier.clone(), subscribed.clone());
                async move {
                    tracing::info!("the watch subscribed to the forward PPoGATT service");
                    *notifier.lock().await = Some(n);
                    subscribed.notify_waiters();
                }
                .boxed()
            }))
        };
        let app = Application {
            services: vec![Service {
                uuid: PPOG_FORWARD_SERVICE,
                primary: true,
                characteristics: vec![
                    Characteristic {
                        uuid: PPOG_FORWARD_DATA,
                        write: Some(CharacteristicWrite {
                            write_without_response: true,
                            encrypt_write: true,
                            method: write,
                            ..Default::default()
                        }),
                        notify: Some(CharacteristicNotify { notify: true, method: notify, ..Default::default() }),
                        ..Default::default()
                    },
                    Characteristic {
                        uuid: PPOG_FORWARD_META,
                        read: Some(CharacteristicRead {
                            read: true,
                            encrypt_read: true,
                            fun: Box::new(|_| async { Ok(FORWARD_META_VALUE.to_vec()) }.boxed()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let handle = adapter.serve_gatt_application(app).await.context("cannot register the PPoGATT service with BlueZ")?;
        Ok(ForwardServer { _app: handle, inbound, notifier, subscribed, mtu })
    }

    /// Routes the watch's writes to a new receiver for a new session, and drops the previous
    /// connection's subscription.
    pub async fn attach(&self) -> mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.inbound.lock().expect("inbound lock") = Some(tx);
        *self.notifier.lock().await = None;
        rx
    }

    pub fn mtu(&self) -> usize {
        self.mtu.load(Ordering::Relaxed)
    }

    /// Notifies the watch. The watch subscribes more than once per connection, so a send on a
    /// stopped subscription waits for the next one.
    pub async fn send(&self, bytes: &[u8]) -> Result<()> {
        for _ in 0..2 {
            let waiting = self.subscribed.notified();
            {
                let mut slot = self.notifier.lock().await;
                if let Some(n) = slot.as_mut() {
                    if !n.is_stopped() && n.notify(bytes.to_vec()).await.is_ok() {
                        return Ok(());
                    }
                    *slot = None;
                }
            }
            if tokio::time::timeout(Duration::from_secs(5), waiting).await.is_err() {
                break;
            }
        }
        Err(anyhow!("the watch is not subscribed to the PPoGATT service"))
    }
}
