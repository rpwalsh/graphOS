// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Graph-first predictive orchestrator.
//!
//! This kernel task fuses telemetry from the twin predictor, network stack,
//! and service manager, then applies bounded policy actions through a unified
//! control plane.

use core::mem::size_of;
use core::slice;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use crate::arch;
use crate::graph::twin;
use crate::ipc;
use crate::net;
use crate::sched;
use crate::svc;
use crate::uuid::ChannelUuid;

const ORCHESTRATOR_POLICY_TICKS: u64 = 20;
const ORCHESTRATOR_TELEMETRY_TICKS: u64 = 100;
const NET_POLL_CMD_RX_DRAIN: u8 = 1;
const NET_POLL_CHANNEL_NAME: &[u8] = b"k-orchestrator-netpoll";

/// 0 means "no override"; otherwise value is a scheduler time slice in ticks.
static SCHED_QUANTUM_OVERRIDE_TICKS: AtomicU64 = AtomicU64::new(0);
/// Net poll cadence consumed by `net_poll_task`.
static NET_POLL_INTERVAL_TICKS: AtomicU64 = AtomicU64::new(1);

struct OrchestratorState {
    last_rx_packets: u64,
    last_tx_packets: u64,
}

impl OrchestratorState {
    const fn new() -> Self {
        Self {
            last_rx_packets: 0,
            last_tx_packets: 0,
        }
    }
}

static STATE: Mutex<OrchestratorState> = Mutex::new(OrchestratorState::new());

/// Compact, fixed-size telemetry frame sent to AI services.
#[repr(C)]
#[derive(Clone, Copy)]
struct AiTelemetryFrame {
    tick_ms: u64,
    suggested_quantum: u64,
    net_poll_interval: u64,
    tx_packets: u64,
    rx_packets: u64,
    dropped_packets: u64,
    twin_dropped_samples: u64,
    running_services: u16,
    failed_services: u16,
    alarm_count: u8,
    thermal_pressure: u8,
    confident: u8,
    _pad: u8,
    coherence: u32,
}

#[inline]
pub fn scheduler_quantum_override() -> Option<u64> {
    let raw = SCHED_QUANTUM_OVERRIDE_TICKS.load(Ordering::Relaxed);
    if raw == 0 { None } else { Some(raw) }
}

#[inline]
pub fn net_poll_interval_ticks() -> u64 {
    NET_POLL_INTERVAL_TICKS.load(Ordering::Relaxed).max(1)
}

#[inline]
pub fn net_poll_control_channel() -> ChannelUuid {
    ChannelUuid::from_service_name(NET_POLL_CHANNEL_NAME)
}

#[inline]
pub fn is_net_poll_rx_drain_cmd(payload: &[u8]) -> bool {
    payload.first().copied() == Some(NET_POLL_CMD_RX_DRAIN)
}

#[inline]
fn set_scheduler_quantum_override(ticks: Option<u64>) {
    let value = ticks.unwrap_or(0);
    SCHED_QUANTUM_OVERRIDE_TICKS.store(value, Ordering::Relaxed);
}

#[inline]
fn set_net_poll_interval_ticks(ticks: u64) {
    NET_POLL_INTERVAL_TICKS.store(ticks.max(1), Ordering::Relaxed);
}

fn apply_policy(now_ms: u64) -> (u64, u64, twin::SchedHint, net::NetStats, usize, usize) {
    let hint = twin::query_sched_hint();
    let stats = net::stats();
    let running = svc::running_count();
    let failed = svc::failed_count();

    let mut state = STATE.lock();

    let mut quantum = if hint.confident {
        hint.quantum
    } else {
        arch::timer::SCHED_TIME_SLICE_TICKS
    };

    let mut poll_interval = 4u64;

    // If TX advances without RX progress, prioritize network turnaround.
    let tx_advanced = stats.tx_packets > state.last_tx_packets;
    let rx_stalled = stats.rx_packets == state.last_rx_packets;
    if stats.link_ready && tx_advanced && rx_stalled {
        quantum = quantum.min(3);
        poll_interval = 1;
    }

    // Service failures push the system into a responsive recovery mode.
    if failed > 0 {
        quantum = quantum.min(2);
        poll_interval = 1;
    }

    // Thermal pressure backs off poll intensity while preserving liveness.
    if hint.thermal_pressure >= 2 {
        poll_interval = poll_interval.max(2);
    }

    // Clamp policy actions to bounded, safe ranges.
    quantum = quantum.clamp(1, arch::timer::SCHED_TIME_SLICE_TICKS * 2);
    poll_interval = poll_interval.clamp(1, 16);

    set_scheduler_quantum_override(Some(quantum));
    set_net_poll_interval_ticks(poll_interval);

    state.last_rx_packets = stats.rx_packets;
    state.last_tx_packets = stats.tx_packets;

    if now_ms.is_multiple_of(500) {
        arch::serial::write_bytes(b"[orchestrator] q=");
        arch::serial::write_u64_dec_inline(quantum);
        arch::serial::write_bytes(b" poll=");
        arch::serial::write_u64_dec_inline(poll_interval);
        arch::serial::write_bytes(b" failed=");
        arch::serial::write_u64_dec_inline(failed as u64);
        arch::serial::write_bytes(b" coherent=");
        arch::serial::write_u64_dec(hint.coherence as u64);
    }

    (quantum, poll_interval, hint, stats, running, failed)
}

fn emit_ai_telemetry(
    tick_ms: u64,
    quantum: u64,
    poll_interval: u64,
    hint: twin::SchedHint,
    stats: net::NetStats,
    running: usize,
    failed: usize,
) {
    let frame = AiTelemetryFrame {
        tick_ms,
        suggested_quantum: quantum,
        net_poll_interval: poll_interval,
        tx_packets: stats.tx_packets,
        rx_packets: stats.rx_packets,
        dropped_packets: stats.dropped_packets,
        twin_dropped_samples: twin::dropped_sample_count(),
        running_services: running.min(u16::MAX as usize) as u16,
        failed_services: failed.min(u16::MAX as usize) as u16,
        alarm_count: hint.alarm_count,
        thermal_pressure: hint.thermal_pressure,
        confident: if hint.confident { 1 } else { 0 },
        _pad: 0,
        coherence: hint.coherence,
    };

    let payload = unsafe {
        slice::from_raw_parts(
            (&frame as *const AiTelemetryFrame).cast::<u8>(),
            size_of::<AiTelemetryFrame>(),
        )
    };

    for name in [
        b"trainerd".as_slice(),
        b"modeld".as_slice(),
        b"graphd".as_slice(),
    ] {
        let ch = ChannelUuid::from_service_name(name);
        let _ = ipc::channel_send_tagged(ch, ipc::msg::MsgTag::Data, payload);
    }
}

fn dispatch_net_poll_request() -> bool {
    crate::arch::x86_64::virtio_net::poll_rx();
    true
}

/// Entry point for the orchestrator kernel task.
pub fn task_entry() {
    arch::serial::write_line(b"[orchestrator] online");
    let mut next_policy_tick = arch::timer::ticks();
    let mut next_emit_tick = next_policy_tick;
    let mut next_poll_tick = next_policy_tick;

    loop {
        let now = arch::timer::ticks();

        if now >= next_policy_tick {
            let (quantum, poll_interval, hint, stats, running, failed) = apply_policy(now);
            if now >= next_emit_tick {
                emit_ai_telemetry(now, quantum, poll_interval, hint, stats, running, failed);
                next_emit_tick = now.saturating_add(ORCHESTRATOR_TELEMETRY_TICKS);
            }
            next_policy_tick = now.saturating_add(ORCHESTRATOR_POLICY_TICKS);

            // Bring forward the next poll when policy demands a faster cadence.
            let min_next_poll = now.saturating_add(poll_interval);
            if min_next_poll < next_poll_tick {
                next_poll_tick = min_next_poll;
            }
        }

        if now >= next_poll_tick {
            dispatch_net_poll_request();
            next_poll_tick = now.saturating_add(net_poll_interval_ticks());
        }

        unsafe { sched::schedule() };
    }
}
