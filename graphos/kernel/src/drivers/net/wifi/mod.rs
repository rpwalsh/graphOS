// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Wi-Fi driver — virtio-net / SDIO 802.11 stub.
//!
//! Provides WPA2-PSK 4-way handshake support and BSSID scanning for
//! platforms that expose an 802.11 interface via virtio-net or a future
//! native SDIO Wi-Fi chip driver.
//!
//! # Architecture
//! The driver has two layers:
//! - **HAL** (`hal.rs` / inline stubs here): send/receive 802.11 frames.
//! - **WPA2** (`wpa2` sub-module): EAPOL 4-way handshake, PTK/GTK derivation.
//!
//! For the initial revision the HAL is a no-op stub; the WPA2 crypto is
//! implemented in-kernel using the existing AES and SHA-256 primitives.

pub mod wpa2;

use spin::Mutex;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_SSID_LEN: usize = 32;
const MAX_BSSID: usize = 16;
const BSSID_LEN: usize = 6;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WifiState {
    Down,
    Scanning,
    Associating,
    Connected,
    Error,
}

#[derive(Clone, Copy)]
pub struct BssEntry {
    pub bssid: [u8; BSSID_LEN],
    pub ssid: [u8; MAX_SSID_LEN],
    pub ssid_len: usize,
    /// RSSI in dBm (negative; e.g. -60).
    pub rssi_dbm: i8,
    /// Channel number (1–14 for 2.4 GHz; 36–165 for 5 GHz).
    pub channel: u8,
    /// Whether WPA2 is required.
    pub wpa2: bool,
}

impl BssEntry {
    const EMPTY: Self = Self {
        bssid: [0u8; BSSID_LEN],
        ssid: [0u8; MAX_SSID_LEN],
        ssid_len: 0,
        rssi_dbm: -127,
        channel: 0,
        wpa2: false,
    };
}

struct WifiDriver {
    state: WifiState,
    scan_results: [BssEntry; MAX_BSSID],
    scan_count: usize,
    /// SSID to connect to (set by `associate`).
    target_ssid: [u8; MAX_SSID_LEN],
    target_ssid_len: usize,
    /// WPA2 PMK derived from SSID + passphrase.
    pmk: [u8; 32],
}

impl WifiDriver {
    const fn new() -> Self {
        Self {
            state: WifiState::Down,
            scan_results: [BssEntry::EMPTY; MAX_BSSID],
            scan_count: 0,
            target_ssid: [0u8; MAX_SSID_LEN],
            target_ssid_len: 0,
            pmk: [0u8; 32],
        }
    }
}

static DRIVER: Mutex<WifiDriver> = Mutex::new(WifiDriver::new());

/// Probe hook used by the shared driver registry.
///
/// We treat a present network adapter as sufficient to bind the Wi-Fi stack
/// plumbing in QEMU/dev bring-up environments.
pub fn probe_driver() -> crate::drivers::ProbeResult {
    let mut found = false;
    crate::arch::x86_64::pci::for_each_device(|info| {
        if found {
            return;
        }
        // PCI class 0x02 = network controller.
        if info.class_code == 0x02 {
            found = true;
        }
    });
    if !found {
        return crate::drivers::ProbeResult::NoMatch;
    }
    init();
    crate::drivers::ProbeResult::Bound
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialise the Wi-Fi driver.  Must be called once during early boot.
pub fn init() {
    let mut d = DRIVER.lock();
    d.state = WifiState::Down;
    crate::arch::serial::write_line(b"[wifi] driver init");
}

/// Begin a passive/active scan on all supported channels.
///
/// Results are stored internally and accessible via `scan_results()`.
/// Returns `true` if a scan was started (always `true` for the stub).
pub fn start_scan() -> bool {
    let mut d = DRIVER.lock();
    d.state = WifiState::Scanning;
    d.scan_count = 0;
    crate::arch::serial::write_line(b"[wifi] scan started");
    // HAL would inject scan request here.
    true
}

/// Copy up to `buf.len()` scan results into `buf`.
/// Returns the number of entries written.
pub fn scan_results(buf: &mut [BssEntry]) -> usize {
    let d = DRIVER.lock();
    let n = buf.len().min(d.scan_count);
    buf[..n].copy_from_slice(&d.scan_results[..n]);
    n
}

/// Associate with the given SSID using WPA2-PSK.
///
/// `passphrase` must be 8–63 bytes (WPA2-Personal PSK).
/// Returns `true` if the association attempt was initiated.
pub fn associate(ssid: &[u8], passphrase: &[u8]) -> bool {
    if ssid.is_empty() || ssid.len() > MAX_SSID_LEN {
        return false;
    }
    if passphrase.len() < 8 || passphrase.len() > 63 {
        return false;
    }

    let mut d = DRIVER.lock();
    let copy_len = ssid.len().min(MAX_SSID_LEN);
    d.target_ssid[..copy_len].copy_from_slice(&ssid[..copy_len]);
    d.target_ssid_len = copy_len;

    // Derive PMK via PBKDF2-SHA256(passphrase, ssid, 4096, 32).
    let pmk = wpa2::derive_pmk(passphrase, ssid);
    d.pmk = pmk;
    d.state = WifiState::Associating;

    crate::arch::serial::write_bytes(b"[wifi] associating with SSID len=");
    crate::arch::serial::write_hex(ssid.len() as u64);
    crate::arch::serial::write_line(b"");
    true
}

/// Return the current driver state.
pub fn state() -> WifiState {
    DRIVER.lock().state
}

/// Mark the connection as up (called by HAL on association success).
pub fn on_associated() {
    let mut d = DRIVER.lock();
    d.state = WifiState::Connected;
    crate::arch::serial::write_line(b"[wifi] connected");
}
