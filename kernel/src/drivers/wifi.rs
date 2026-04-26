// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Wi-Fi driver scaffold.
//!
//! For current QEMU validation this module aliases virtio-net availability
//! as WLAN presence, while preserving a dedicated Wi-Fi API surface.

use crate::drivers::ProbeResult;

/// Runtime Wi-Fi link state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiState {
    Down,
    Up,
}

static mut STATE: WifiState = WifiState::Down;

/// Probe Wi-Fi transport availability.
///
/// In QEMU builds we treat virtio-net as the WLAN transport for tray/status
/// integration until native hardware drivers are added.
pub fn probe() -> ProbeResult {
    if crate::arch::x86_64::virtio_net::is_present() {
        unsafe {
            STATE = WifiState::Up;
        }
        ProbeResult::Bound
    } else {
        unsafe {
            STATE = WifiState::Down;
        }
        ProbeResult::NoMatch
    }
}

/// Current Wi-Fi link state.
pub fn state() -> WifiState {
    unsafe { STATE }
}

/// Returns a synthetic SSID for UI/tray integration.
pub fn ssid() -> &'static [u8] {
    match state() {
        WifiState::Up => b"virtio-wlan",
        WifiState::Down => b"offline",
    }
}
