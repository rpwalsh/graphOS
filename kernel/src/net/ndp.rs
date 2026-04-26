// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Neighbor Discovery Protocol (RFC 4861) — IPv6 equivalent of ARP.
//!
//! The NDP table maps IPv6 addresses to MAC addresses (neighbor cache).
//! On cache miss, a Neighbor Solicitation is queued to the driver.
//! This module is stateless with respect to timers — the caller (net/mod.rs)
//! is responsible for aging entries via periodic calls to `tick()`.

use super::ipv6::Ipv6Addr;

const NDP_TABLE_SIZE: usize = 32;
/// After this many milliseconds without a reachability confirmation,
/// an entry is considered STALE (but still usable until PROBE phase).
const NDP_REACHABLE_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NdpState {
    /// Entry allocated but NS not yet sent.
    Incomplete = 0,
    /// Successfully resolved; within reachable time.
    Reachable = 1,
    /// Resolved but not recently confirmed reachable.
    Stale = 2,
    /// Being probed to confirm reachability.
    Probe = 3,
}

#[derive(Clone, Copy, Debug)]
pub struct NdpEntry {
    pub ip: Ipv6Addr,
    pub mac: [u8; 6],
    pub state: NdpState,
    /// Millisecond tick when this entry was last confirmed reachable.
    pub last_reachable_ms: u64,
    pub valid: bool,
}

impl NdpEntry {
    const EMPTY: Self = Self {
        ip: Ipv6Addr::UNSPECIFIED,
        mac: [0u8; 6],
        state: NdpState::Incomplete,
        last_reachable_ms: 0,
        valid: false,
    };
}

/// The neighbor cache for IPv6.
pub struct NdpTable {
    entries: [NdpEntry; NDP_TABLE_SIZE],
    next_replace: usize,
}

impl NdpTable {
    pub const fn new() -> Self {
        Self {
            entries: [NdpEntry::EMPTY; NDP_TABLE_SIZE],
            next_replace: 0,
        }
    }

    /// Insert or update an entry, marking it `Reachable`.
    pub fn insert(&mut self, ip: &Ipv6Addr, mac: [u8; 6], now_ms: u64) {
        for entry in &mut self.entries {
            if entry.valid && entry.ip == *ip {
                entry.mac = mac;
                entry.state = NdpState::Reachable;
                entry.last_reachable_ms = now_ms;
                return;
            }
        }
        // Evict oldest / invalid slot.
        self.entries[self.next_replace] = NdpEntry {
            ip: *ip,
            mac,
            state: NdpState::Reachable,
            last_reachable_ms: now_ms,
            valid: true,
        };
        self.next_replace = (self.next_replace + 1) % NDP_TABLE_SIZE;
    }

    /// Look up a MAC for an IPv6 address.
    pub fn lookup(&self, ip: &Ipv6Addr) -> Option<[u8; 6]> {
        for entry in &self.entries {
            if entry.valid && entry.ip == *ip && entry.state != NdpState::Incomplete {
                return Some(entry.mac);
            }
        }
        None
    }

    /// Mark an entry INCOMPLETE (NS sent, awaiting NA).
    pub fn mark_incomplete(&mut self, ip: &Ipv6Addr) {
        for entry in &mut self.entries {
            if entry.valid && entry.ip == *ip {
                entry.state = NdpState::Incomplete;
                return;
            }
        }
        // Allocate slot as Incomplete.
        self.entries[self.next_replace] = NdpEntry {
            ip: *ip,
            mac: [0u8; 6],
            state: NdpState::Incomplete,
            last_reachable_ms: 0,
            valid: true,
        };
        self.next_replace = (self.next_replace + 1) % NDP_TABLE_SIZE;
    }

    /// Age entries: Reachable → Stale after `NDP_REACHABLE_MS`.
    pub fn tick(&mut self, now_ms: u64) {
        for entry in &mut self.entries {
            if entry.valid
                && entry.state == NdpState::Reachable
                && now_ms.saturating_sub(entry.last_reachable_ms) > NDP_REACHABLE_MS
            {
                entry.state = NdpState::Stale;
            }
        }
    }

    /// Remove all entries (e.g., on interface reset).
    pub fn flush(&mut self) {
        for entry in &mut self.entries {
            entry.valid = false;
        }
        self.next_replace = 0;
    }

    /// Iterate valid entries — used for debugging / `/sys/net/ndp` VFS export.
    pub fn entries(&self) -> impl Iterator<Item = &NdpEntry> {
        self.entries.iter().filter(|e| e.valid)
    }
}

impl Default for NdpTable {
    fn default() -> Self {
        Self::new()
    }
}
