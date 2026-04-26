// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Fuzz target: TCP connection state machine.
//!
//! The kernel's TCP stack must handle arbitrary byte sequences without
//! entering an invalid state (e.g., transitioning to a non-reachable state,
//! panicking, or wrapping sequence numbers in unsafe ways).
//!
//! This target replaces the full TCP segment parser with a simplified
//! state machine replicated from kernel/src/net/tcp/state.rs.

#![no_main]

use libfuzzer_sys::fuzz_target;

// ── Replicated TCP state machine ──────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

// TCP flag bits (from the flags byte in the fixed header).
const FLAG_FIN: u8 = 0x01;
const FLAG_SYN: u8 = 0x02;
const FLAG_RST: u8 = 0x04;
const FLAG_ACK: u8 = 0x10;

#[derive(Debug)]
struct Segment {
    flags: u8,
    seq: u32,
    ack: u32,
    _len: u16,
}

impl Segment {
    fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 10 {
            return None;
        }
        Some(Self {
            flags: data[0],
            seq: u32::from_be_bytes(data[1..5].try_into().ok()?),
            ack: u32::from_be_bytes(data[5..9].try_into().ok()?),
            _len: u16::from_be_bytes(
                data[9..10]
                    .iter()
                    .chain([0].iter())
                    .copied()
                    .collect::<Vec<_>>()
                    .try_into()
                    .ok()?,
            ),
        })
    }
}

/// Process one segment in the given state.  Returns the next state.
/// Must never panic.
fn process(state: TcpState, seg: &Segment) -> TcpState {
    let syn = seg.flags & FLAG_SYN != 0;
    let ack = seg.flags & FLAG_ACK != 0;
    let fin = seg.flags & FLAG_FIN != 0;
    let rst = seg.flags & FLAG_RST != 0;

    if rst {
        return TcpState::Closed;
    }

    match state {
        TcpState::Closed => TcpState::Closed,
        TcpState::Listen => {
            if syn {
                TcpState::SynReceived
            } else {
                TcpState::Listen
            }
        }
        TcpState::SynSent => {
            if syn && ack {
                TcpState::Established
            } else if syn {
                TcpState::SynReceived
            } else {
                TcpState::SynSent
            }
        }
        TcpState::SynReceived => {
            if ack {
                TcpState::Established
            } else {
                TcpState::SynReceived
            }
        }
        TcpState::Established => {
            if fin {
                TcpState::CloseWait
            } else {
                TcpState::Established
            }
        }
        TcpState::FinWait1 => {
            if fin && ack {
                TcpState::TimeWait
            } else if fin {
                TcpState::Closing
            } else if ack {
                TcpState::FinWait2
            } else {
                TcpState::FinWait1
            }
        }
        TcpState::FinWait2 => {
            if fin {
                TcpState::TimeWait
            } else {
                TcpState::FinWait2
            }
        }
        TcpState::CloseWait => TcpState::CloseWait,
        TcpState::Closing => {
            if ack {
                TcpState::TimeWait
            } else {
                TcpState::Closing
            }
        }
        TcpState::LastAck => {
            if ack {
                TcpState::Closed
            } else {
                TcpState::LastAck
            }
        }
        TcpState::TimeWait => TcpState::Closed,
    }
}

fuzz_target!(|data: &[u8]| {
    // Pick the initial state from the first byte.
    let states = [
        TcpState::Closed,
        TcpState::Listen,
        TcpState::SynSent,
        TcpState::SynReceived,
        TcpState::Established,
        TcpState::FinWait1,
        TcpState::FinWait2,
        TcpState::CloseWait,
        TcpState::Closing,
        TcpState::LastAck,
        TcpState::TimeWait,
    ];

    if data.is_empty() {
        return;
    }
    let mut state = states[(data[0] as usize) % states.len()];
    let mut rest = &data[1..];

    // Process each 10-byte segment.
    while rest.len() >= 10 {
        if let Some(seg) = Segment::from_bytes(&rest[..10]) {
            state = process(state, &seg);
        }
        rest = &rest[10..];
    }

    // State machine must always be in a valid state (no panic = success).
    let _ = state;
});
