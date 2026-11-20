//! Pebble Protocol over Bluetooth LE, without I/O: PPoGATT, framing, and the messages Lithidion
//! sends and receives. Section numbers refer to `docs/PROTOCOL.md`.

pub mod appmessage;
pub mod apps;
pub mod ble;
pub mod blobdb;
pub mod capabilities;
pub mod crc;
pub mod datalog;
pub mod diagnostics;
pub mod endpoint;
pub mod firmware;
pub mod frame;
pub mod hardware;
pub mod health;
pub mod imaging;
pub mod music;
pub mod phone;
pub mod ppog;
pub mod putbytes;
pub mod system;
pub mod timeline;
pub mod voice;
pub mod watchprefs;
pub mod weather;
pub mod wire;

pub use wire::DecodeError;
