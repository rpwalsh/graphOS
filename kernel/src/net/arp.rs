// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
pub type MacAddr = [u8; 6];

const ARP_TABLE_SIZE: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArpEntry {
    pub ipv4: u32,
    pub mac: MacAddr,
    pub last_seen_tick: u64,
    pub valid: bool,
}

impl ArpEntry {
    const EMPTY: Self = Self {
        ipv4: 0,
        mac: [0; 6],
        last_seen_tick: 0,
        valid: false,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArpTable {
    entries: [ArpEntry; ARP_TABLE_SIZE],
    next_replace: usize,
}

impl ArpTable {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self {
            entries: [ArpEntry::EMPTY; ARP_TABLE_SIZE],
            next_replace: 0,
        }
    }

    pub fn insert(&mut self, ipv4: u32, mac: MacAddr, tick: u64) {
        for entry in &mut self.entries {
            if entry.valid && entry.ipv4 == ipv4 {
                entry.mac = mac;
                entry.last_seen_tick = tick;
                return;
            }
        }

        self.entries[self.next_replace] = ArpEntry {
            ipv4,
            mac,
            last_seen_tick: tick,
            valid: true,
        };
        self.next_replace = (self.next_replace + 1) % ARP_TABLE_SIZE;
    }

    pub fn lookup(&self, ipv4: u32) -> Option<MacAddr> {
        for entry in &self.entries {
            if entry.valid && entry.ipv4 == ipv4 {
                return Some(entry.mac);
            }
        }
        None
    }

    pub fn age_out(&mut self, min_tick: u64) {
        for entry in &mut self.entries {
            if entry.valid && entry.last_seen_tick < min_tick {
                *entry = ArpEntry::EMPTY;
            }
        }
    }
}
