// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS Service Manager — launches, supervises, and routes between core services.
//!
//! servicemgr is the first userspace service spawned by the kernel init task.
//! It owns the service registry and manages the lifecycle of all other services.
//!
//! ## Responsibilities
//!
//! 1. **Service lifecycle**: start, stop, restart services in dependency order.
//! 2. **Service registry**: maintain a name -> channel_id mapping so services
//!    can discover each other by name rather than hardcoded channel IDs.
//! 3. **Health monitoring**: periodically poll services via Ping/Pong. If a
//!    service fails to respond, mark it degraded in the graph.
//! 4. **Graph integration**: register each service as a Service node in the
//!    graph. Create CommunicatesWith edges between services that share channels.
//!
//! ## IPC protocol
//!
//! servicemgr listens on channel 1 (well-known). It handles:
//! - MsgTag::ServiceRegister (0x30) -> registers a new service, assigns channel
//! - MsgTag::ServiceStatus (0x31) -> updates health state in the registry
//! - MsgTag::Ping (0x01) -> replies Pong (0x02)
//! - MsgTag::Shutdown (0x04) -> begins graceful shutdown of all services
//!
//! ## Boot order
//!
//! servicemgr starts services in this order:
//! 1. graphd (graph runtime — must be first, others depend on it)
//! 2. modeld (AI operating participant — consumes graphd)
//! 3. compositor / shell3d (user interface — consumes graphd + modeld)
//! 4. packaged (package manager — consumes graphd)
//! 5. other services as registered
//!
//! ## Current status
//!
//! Host-build service-manager scaffolding with the real control protocol and
//! health-tracking logic. Kernel-launched ring-3 execution is still pending.

#[path = "../../common/host_sys.rs"]
mod host_sys;
#[path = "../../common/ipc.rs"]
mod ipc;

/// Maximum number of registered services.
const MAX_SERVICES: usize = 32;
const CORE_SERVICES: [CoreServiceSpec; 5] = [
    CoreServiceSpec::new(b"graphd", 2, 0),
    CoreServiceSpec::new(b"sysd", 6, 0),
    CoreServiceSpec::new(b"modeld", 3, 0),
    CoreServiceSpec::new(b"trainerd", 4, 0),
    CoreServiceSpec::new(b"artifactsd", 5, 0),
];

#[derive(Clone, Copy)]
struct CoreServiceSpec {
    name: &'static [u8],
    channel_id: u32,
    capabilities: u32,
}

impl CoreServiceSpec {
    const fn new(name: &'static [u8], channel_id: u32, capabilities: u32) -> Self {
        Self {
            name,
            channel_id,
            capabilities,
        }
    }
}

/// A registered service entry.
#[derive(Clone, Copy)]
pub struct ServiceEntry {
    /// Service name (null-terminated, up to 31 chars).
    pub name: [u8; 32],
    /// IPC channel the service listens on.
    pub channel_id: u32,
    /// Graph node ID for this service.
    pub graph_node: u64,
    /// Capabilities bitmask (same as ServiceRegistration.capabilities).
    pub capabilities: u32,
    /// Health score (16.16 fixed-point). 1.0 = healthy.
    pub health: u32,
    /// Whether this service is currently running.
    pub alive: bool,
    /// Number of consecutive missed pings.
    pub missed_pings: u8,
    /// Whether servicemgr is currently waiting on a Pong for this service.
    pub awaiting_pong: bool,
}

impl ServiceEntry {
    pub const fn empty() -> Self {
        Self {
            name: [0u8; 32],
            channel_id: 0,
            graph_node: 0,
            capabilities: 0,
            health: 0,
            alive: false,
            missed_pings: 0,
            awaiting_pong: false,
        }
    }

    pub fn name_str(&self) -> &[u8] {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        &self.name[..len]
    }
}

/// The service registry.
pub struct ServiceRegistry {
    entries: [ServiceEntry; MAX_SERVICES],
    count: usize,
}

impl ServiceRegistry {
    pub const fn new() -> Self {
        const EMPTY: ServiceEntry = ServiceEntry::empty();
        Self {
            entries: [EMPTY; MAX_SERVICES],
            count: 0,
        }
    }

    /// Register a service. Returns the index, or None if full.
    pub fn register(&mut self, name: &[u8], channel_id: u32, capabilities: u32) -> Option<usize> {
        if self.count >= MAX_SERVICES {
            return None;
        }
        let idx = self.count;
        let entry = &mut self.entries[idx];
        let copy_len = name.len().min(31);
        entry.name[..copy_len].copy_from_slice(&name[..copy_len]);
        entry.channel_id = channel_id;
        entry.capabilities = capabilities;
        // 1.0 in 16.16
        entry.health = 1 << 16;
        entry.alive = true;
        entry.missed_pings = 0;
        entry.awaiting_pong = false;
        self.count += 1;
        Some(idx)
    }

    /// Look up a service by name. Returns the entry index.
    pub fn find_by_name(&self, name: &[u8]) -> Option<usize> {
        for i in 0..self.count {
            let entry_name = self.entries[i].name_str();
            if entry_name.len() == name.len() && entry_name == name {
                return Some(i);
            }
        }
        None
    }

    /// Look up a service by capability. Returns the first match.
    pub fn find_by_capability(&self, cap_bit: u32) -> Option<usize> {
        for i in 0..self.count {
            if self.entries[i].alive && (self.entries[i].capabilities & cap_bit) != 0 {
                return Some(i);
            }
        }
        None
    }

    /// Look up a service by its reply endpoint channel.
    pub fn find_by_channel(&self, channel_id: u32) -> Option<usize> {
        for i in 0..self.count {
            if self.entries[i].channel_id == channel_id {
                return Some(i);
            }
        }
        None
    }

    /// Get a service entry by index.
    pub fn get(&self, idx: usize) -> Option<&ServiceEntry> {
        if idx < self.count {
            Some(&self.entries[idx])
        } else {
            None
        }
    }

    /// Mark a service as dead (missed too many pings).
    pub fn mark_dead(&mut self, idx: usize) {
        if idx < self.count {
            self.entries[idx].alive = false;
            self.entries[idx].health = 0;
            self.entries[idx].awaiting_pong = false;
        }
    }

    /// Update health for a service.
    pub fn update_health(&mut self, idx: usize, health: u32) {
        if idx < self.count {
            self.entries[idx].health = health;
            self.entries[idx].alive = true;
            self.entries[idx].missed_pings = 0;
            self.entries[idx].awaiting_pong = false;
        }
    }

    /// Begin a new health probe for a service and return its ping channel.
    pub fn begin_health_probe(&mut self, idx: usize) -> Option<u32> {
        if idx >= self.count {
            return None;
        }
        let entry = &mut self.entries[idx];
        if !entry.alive {
            return None;
        }
        if entry.awaiting_pong {
            entry.missed_pings += 1;
            if entry.missed_pings >= 3 {
                entry.alive = false;
                entry.health = 0;
                entry.awaiting_pong = false;
                return None;
            }
        }
        entry.awaiting_pong = true;
        Some(entry.channel_id)
    }

    /// Mark a service as having answered the current health probe.
    pub fn mark_pong(&mut self, idx: usize) {
        if idx < self.count {
            let entry = &mut self.entries[idx];
            entry.alive = true;
            entry.missed_pings = 0;
            entry.awaiting_pong = false;
            if entry.health == 0 {
                entry.health = 1 << 16;
            }
        }
    }

    /// Return the number of alive services.
    pub fn alive_count(&self) -> usize {
        self.entries[..self.count]
            .iter()
            .filter(|e| e.alive)
            .count()
    }
}

/// servicemgr state.
pub struct ServiceManager {
    registry: ServiceRegistry,
    /// This service's own IPC channel (well-known channel 1).
    listen_channel: u32,
    running: bool,
}

impl ServiceManager {
    pub fn new(listen_channel: u32) -> Self {
        Self {
            registry: ServiceRegistry::new(),
            listen_channel,
            running: false,
        }
    }

    /// Start the service manager event loop.
    pub fn run(&mut self) {
        self.running = true;
        self.bootstrap_core_services();
        let mut buf = [0u8; 256];
        let mut tick_counter: u32 = 0;

        loop {
            let result = sys_channel_recv(self.listen_channel, &mut buf);
            let Some(msg) = ipc::decode_recv_result(result) else {
                tick_counter += 1;
                if tick_counter & 63 == 0 {
                    self.health_check_cycle();
                }
                sys_yield();
                continue;
            };

            match msg.tag {
                0x30 => {
                    // MsgTag::ServiceRegister
                    // Payload: name_len(1) + name(name_len) + channel_id(4) + capabilities(4)
                    if msg.payload_len >= 9 {
                        let name_len = buf[0] as usize;
                        if msg.payload_len >= 1 + name_len + 8 && name_len <= 31 {
                            let name = &buf[1..1 + name_len];
                            let ch_off = 1 + name_len;
                            let channel_id = u32::from_le_bytes([
                                buf[ch_off],
                                buf[ch_off + 1],
                                buf[ch_off + 2],
                                buf[ch_off + 3],
                            ]);
                            let cap_off = ch_off + 4;
                            let capabilities = u32::from_le_bytes([
                                buf[cap_off],
                                buf[cap_off + 1],
                                buf[cap_off + 2],
                                buf[cap_off + 3],
                            ]);
                            let ok = self.handle_register(name, channel_id, capabilities);
                            let resp = [if ok { 1u8 } else { 0u8 }];
                            sys_channel_send(msg.reply_endpoint, &resp, 0x02);
                        }
                    }
                }
                0x31 => {
                    // MsgTag::ServiceStatus — health update
                    // Payload: name_len(1) + name(len) + health(4)
                    if msg.payload_len >= 5 {
                        let name_len = buf[0] as usize;
                        if msg.payload_len >= 1 + name_len + 4 && name_len <= 31 {
                            let name = &buf[1..1 + name_len];
                            let h_off = 1 + name_len;
                            let health = u32::from_le_bytes([
                                buf[h_off],
                                buf[h_off + 1],
                                buf[h_off + 2],
                                buf[h_off + 3],
                            ]);
                            if let Some(idx) = self.registry.find_by_name(name) {
                                self.registry.update_health(idx, health);
                            }
                        }
                    }
                }
                0x02 => {
                    if let Some(idx) = self.registry.find_by_channel(msg.reply_endpoint) {
                        self.registry.mark_pong(idx);
                    }
                }
                0x01 => {
                    sys_channel_send(msg.reply_endpoint, &[], 0x02);
                }
                0x04 => {
                    // Shutdown all alive services first.
                    for i in 0..self.registry.count {
                        if let Some(entry) = self.registry.get(i) {
                            if entry.alive {
                                sys_channel_send(entry.channel_id, &[], 0x04);
                            }
                        }
                    }
                    break;
                }
                _ => {}
            }

            tick_counter += 1;
            if tick_counter & 63 == 0 {
                self.health_check_cycle();
            }
        }

        self.running = false;
    }

    /// Handle a ServiceRegister message.
    pub fn handle_register(&mut self, name: &[u8], channel_id: u32, capabilities: u32) -> bool {
        self.registry
            .register(name, channel_id, capabilities)
            .is_some()
    }

    /// Run a health check cycle: ping all alive services.
    pub fn health_check_cycle(&mut self) {
        for i in 0..self.registry.count {
            if let Some(channel_id) = self.registry.begin_health_probe(i) {
                sys_channel_send(channel_id, &[], 0x01);
            }
        }
    }

    fn bootstrap_core_services(&mut self) {
        for spec in CORE_SERVICES {
            if self.registry.find_by_name(spec.name).is_some() {
                continue;
            }

            let Some(idx) = self
                .registry
                .register(spec.name, spec.channel_id, spec.capabilities)
            else {
                log(b"[servicemgr] registry full during bootstrap\n");
                return;
            };

            let pid = host_sys::spawn(spec.name);
            log(b"[servicemgr] start ");
            log(spec.name);
            if pid == 0 {
                log(b" FAILED\n");
                self.registry.mark_dead(idx);
                continue;
            }

            log(b" pid=");
            log_u64(pid);
            log(b"\n");
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Host-mode syscall shims until ring-3 exists
// ════════════════════════════════════════════════════════════════════

fn sys_channel_recv(channel: u32, buf: &mut [u8]) -> u64 {
    host_sys::channel_recv(channel, buf)
}

fn sys_channel_send(channel: u32, payload: &[u8], tag: u8) {
    host_sys::channel_send(channel, payload, tag);
}

fn sys_yield() {
    host_sys::yield_now();
}

fn log(msg: &[u8]) {
    let _ = host_sys::write(1, msg);
}

fn log_u64(mut value: u64) {
    if value == 0 {
        let _ = host_sys::write(1, b"0");
        return;
    }

    let mut buf = [0u8; 20];
    let mut len = 0usize;
    while value > 0 {
        buf[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }
    for idx in (0..len).rev() {
        let _ = host_sys::write(1, &buf[idx..idx + 1]);
    }
}

fn main() {
    let mut mgr = ServiceManager::new(1);
    mgr.run();
    host_sys::exit(0);
}
