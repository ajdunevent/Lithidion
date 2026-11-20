//! Bluetooth LE side of a Pebble (sections 1 to 3): UUIDs, scan records, connectivity status, and
//! the pairing trigger.

use crate::wire::{err, Reader, Result};
use uuid::Uuid;

const fn pebble(prefix: u32) -> Uuid {
    Uuid::from_u128(((prefix as u128) << 96) | 0x328E_0FBB_C642_1AA6_699B_DADA)
}

const fn sig(short: u16) -> Uuid {
    Uuid::from_u128(((short as u128) << 96) | 0x0000_1000_8000_0080_5F9B_34FB)
}

pub const PAIRING_SERVICE: Uuid = sig(0xFED9);
pub const CONNECTIVITY: Uuid = pebble(0x0000_0001);
pub const PAIRING_TRIGGER: Uuid = pebble(0x0000_0002);
pub const CONNECTION_PARAMETERS: Uuid = pebble(0x0000_0005);

pub const PPOG_V2_SERVICE: Uuid = pebble(0x4000_0000);
pub const PPOG_V2_FROM_WATCH: Uuid = pebble(0x4000_0001);
pub const PPOG_V2_TO_WATCH: Uuid = pebble(0x4000_0003);

pub const PPOG_FORWARD_SERVICE: Uuid = pebble(0x1000_0000);
pub const PPOG_FORWARD_DATA: Uuid = pebble(0x1000_0001);
pub const PPOG_FORWARD_META: Uuid = pebble(0x1000_0002);

pub const BATTERY_SERVICE: Uuid = sig(0x180F);
pub const BATTERY_LEVEL: Uuid = sig(0x2A19);

/// Read from the forward meta characteristic: PPoGATT versions 0 to 1.
pub const FORWARD_META_VALUE: [u8; 19] = [0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

pub const CONNECTION_PARAMETERS_VALUE: [u8; 2] = [0, 1];

pub const TARGET_MTU: u16 = 339;

pub const VENDOR_PEBBLE: u16 = 0x0154;
pub const VENDOR_CORE: u16 = 0x0EEA;

pub fn is_pebble_vendor(company: u16) -> bool {
    company == VENDOR_PEBBLE || company == VENDOR_CORE
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRecord {
    pub payload_type: u8,
    pub serial: String,
    pub extended: Option<ExtendedScanRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedScanRecord {
    pub hardware_platform: u8,
    pub model: u8,
    pub firmware: (u8, u8, u8),
    pub running_recovery: bool,
    pub first_use: bool,
}

impl ScanRecord {
    /// Parses manufacturer data, without the company ID.
    pub fn parse(data: &[u8]) -> Result<ScanRecord> {
        let mut r = Reader::new(data);
        let payload_type = r.u8()?;
        let serial = r.fixed_string(12)?;
        let extended = if r.remaining() >= 6 {
            let hardware_platform = r.u8()?;
            let model = r.u8()?;
            let firmware = (r.u8()?, r.u8()?, r.u8()?);
            let flags = r.u8()?;
            Some(ExtendedScanRecord { hardware_platform, model, firmware, running_recovery: flags & 1 != 0, first_use: flags & 2 != 0 })
        } else {
            None
        };
        Ok(ScanRecord { payload_type, serial, extended })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connectivity {
    pub connected: bool,
    pub paired: bool,
    pub encrypted: bool,
    pub has_bonded_gateway: bool,
    pub supports_pinning_without_security: bool,
    pub remote_attempted_stale_pairing: bool,
    pub pairing_error: u8,
}

impl Connectivity {
    pub fn parse(value: &[u8]) -> Result<Connectivity> {
        if value.len() < 4 {
            return err(format!("connectivity status is {} bytes; the watch may need a restart", value.len()));
        }
        let f = value[0];
        Ok(Connectivity {
            connected: f & 0x01 != 0,
            paired: f & 0x02 != 0,
            encrypted: f & 0x04 != 0,
            has_bonded_gateway: f & 0x08 != 0,
            supports_pinning_without_security: f & 0x10 != 0,
            remote_attempted_stale_pairing: f & 0x20 != 0,
            pairing_error: value[3],
        })
    }
}

/// The pairing trigger byte for watch-started pairing, as the official iOS app sends it: no
/// address pinning, and the watch asks for security (section 3).
pub const WATCH_STARTS_PAIRING: u8 = 1 << 2;

/// The pairing trigger byte and whether the phone then asks the OS to bond (section 3).
pub fn pairing_trigger(connectivity: &Connectivity) -> (u8, bool) {
    let phone_bonds = connectivity.supports_pinning_without_security;
    let pin_address = 1;
    let no_security_request = if phone_bonds { 1 << 1 } else { 0 };
    let force_security_request = if phone_bonds { 0 } else { 1 << 2 };
    (pin_address | no_security_request | force_security_request, phone_bonds)
}

pub fn battery_level(value: &[u8]) -> Option<u8> {
    match value {
        [level] if *level <= 100 => Some(*level),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids() {
        assert_eq!(PPOG_V2_SERVICE.to_string(), "40000000-328e-0fbb-c642-1aa6699bdada");
        assert_eq!(PAIRING_SERVICE.to_string(), "0000fed9-0000-1000-8000-00805f9b34fb");
        assert_eq!(BATTERY_LEVEL.to_string(), "00002a19-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn scan_record() {
        let mut data = vec![0x00];
        data.extend_from_slice(b"C111A24B0123");
        data.extend_from_slice(&[18, 37, 4, 35, 0, 0b10]);
        let r = ScanRecord::parse(&data).unwrap();
        assert_eq!(r.serial, "C111A24B0123");
        let x = r.extended.unwrap();
        assert_eq!((x.hardware_platform, x.model, x.firmware), (18, 37, (4, 35, 0)));
        assert!(x.first_use && !x.running_recovery);
        assert_eq!(ScanRecord::parse(&data[..13]).unwrap().extended, None);
    }

    #[test]
    fn connectivity_and_trigger() {
        let c = Connectivity::parse(&[0b0001_0011, 0, 0, 0]).unwrap();
        assert!(c.connected && c.paired && !c.encrypted && c.supports_pinning_without_security);
        assert_eq!(pairing_trigger(&c), (0b011, true));
        let c = Connectivity::parse(&[0b0000_0001, 0, 0, 12]).unwrap();
        assert_eq!(c.pairing_error, 12);
        assert_eq!(pairing_trigger(&c), (0b101, false));
        assert!(Connectivity::parse(&[1]).is_err());
    }

    #[test]
    fn battery() {
        assert_eq!(battery_level(&[72]), Some(72));
        assert_eq!(battery_level(&[101]), None);
        assert_eq!(battery_level(&[]), None);
    }
}
