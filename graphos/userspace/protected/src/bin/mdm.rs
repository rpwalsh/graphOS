// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS MDM agent (`mdm`) — policy fetch, apply, heartbeat, attestation.

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::panic(info)
}

// ---------------------------------------------------------------------------
// Extra syscall IDs not in runtime.rs
// ---------------------------------------------------------------------------

const SYS_NET_HTTP_GET:  u64 = 0x700;
const SYS_NET_HTTP_POST: u64 = 0x701;
const SYS_TPM_QUOTE:     u64 = 0x800;

// ---------------------------------------------------------------------------
// Policy record
// ---------------------------------------------------------------------------

const MAX_ALLOWED_APPS: usize = 64;
const APP_ID_LEN:       usize = 16;

#[derive(Clone, Copy)]
struct Policy {
    /// UUIDs of allowed apps.
    allowed_apps:     [[u8; APP_ID_LEN]; MAX_ALLOWED_APPS],
    allowed_count:    usize,
    /// Screen-time quota per day in minutes (0 = unlimited).
    screen_time_mins: u32,
    /// VPN required flag.
    require_vpn:      bool,
    /// Policy serial number.
    serial:           u64,
}

static mut CURRENT_POLICY: Policy = Policy {
    allowed_apps:     [[0u8; APP_ID_LEN]; MAX_ALLOWED_APPS],
    allowed_count:    0,
    screen_time_mins: 0,
    require_vpn:      false,
    serial:           0,
};

// ---------------------------------------------------------------------------
// MDM server URL
// ---------------------------------------------------------------------------

const MDM_SERVER: &[u8] = b"http://mdm.graphos.local";
const DEVICE_UUID: [u8; 16] = [0xAA; 16]; // provisioned at enrolment

// ---------------------------------------------------------------------------
// Enrolment
// ---------------------------------------------------------------------------

fn enrol() {
    runtime::write_line(b"[mdm] enrolling device");
    let mut body = [0u8; 128];
    body[..7].copy_from_slice(b"{\"uuid\"");
    let blen = 7usize;
    runtime::raw_syscall(
        SYS_NET_HTTP_POST,
        MDM_SERVER.as_ptr() as u64,
        body.as_ptr() as u64,
        blen as u64,
        0,
    );
    runtime::write_line(b"[mdm] enrolment sent");
}

// ---------------------------------------------------------------------------
// Policy fetch + apply
// ---------------------------------------------------------------------------

fn fetch_and_apply_policy() {
    let mut resp_buf = [0u8; 2048];
    let n = runtime::raw_syscall(
        SYS_NET_HTTP_GET,
        MDM_SERVER.as_ptr() as u64,
        resp_buf.as_mut_ptr() as u64,
        resp_buf.len() as u64,
        0,
    ) as usize;
    if n == 0 {
        runtime::write_line(b"[mdm] policy fetch failed");
        return;
    }
    // Parse minimal policy format (length-prefixed binary):
    // serial(8) + allowed_count(2) + allowed_apps(N*16) + screen_time(4) + require_vpn(1)
    if n < 10 { return; }
    let serial = u64::from_le_bytes(resp_buf[0..8].try_into().unwrap_or([0u8; 8]));
    let count  = u16::from_le_bytes([resp_buf[8], resp_buf[9]]) as usize;
    let count  = count.min(MAX_ALLOWED_APPS);
    let mut off = 10usize;

    unsafe {
        CURRENT_POLICY.serial = serial;
        CURRENT_POLICY.allowed_count = 0;
        for i in 0..count {
            if off + APP_ID_LEN > n { break; }
            CURRENT_POLICY.allowed_apps[i].copy_from_slice(&resp_buf[off..off + APP_ID_LEN]);
            CURRENT_POLICY.allowed_count += 1;
            off += APP_ID_LEN;
        }
        if off + 4 <= n {
            CURRENT_POLICY.screen_time_mins = u32::from_le_bytes(
                resp_buf[off..off + 4].try_into().unwrap_or([0u8; 4])
            );
            off += 4;
        }
        if off < n {
            CURRENT_POLICY.require_vpn = resp_buf[off] != 0;
        }
    }
    runtime::write_line(b"[mdm] policy applied");
}

// ---------------------------------------------------------------------------
// Heartbeat + attestation
// ---------------------------------------------------------------------------

fn send_heartbeat() {
    // Collect TPM attestation quote.
    let mut quote_buf = [0u8; 512];
    let q_len = runtime::raw_syscall(
        SYS_TPM_QUOTE,
        quote_buf.as_mut_ptr() as u64,
        quote_buf.len() as u64,
        0, 0,
    ) as usize;

    // Build heartbeat payload: device_uuid(16) + policy_serial(8) + quote(variable).
    let mut payload = [0u8; 540];
    payload[0..16].copy_from_slice(&DEVICE_UUID);
    let serial = unsafe { CURRENT_POLICY.serial };
    payload[16..24].copy_from_slice(&serial.to_le_bytes());
    let q_copy = q_len.min(512);
    payload[24..24 + q_copy].copy_from_slice(&quote_buf[..q_copy]);
    let plen = 24 + q_copy;

    runtime::raw_syscall(
        SYS_NET_HTTP_POST,
        MDM_SERVER.as_ptr() as u64,
        payload.as_ptr() as u64,
        plen as u64,
        1,
    );
    runtime::write_line(b"[mdm] heartbeat sent");
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[mdm] started");
    enrol();
    fetch_and_apply_policy();

    const CHANNEL_ID: u32 = 0x4D4D_0001;
    const HEARTBEAT_INTERVAL: u64 = 300;
    let mut ticks: u64 = 0;

    loop {
        let mut msg = [0u8; 256];
        let n = runtime::channel_recv(CHANNEL_ID, &mut msg) as usize;
        ticks = ticks.wrapping_add(1);

        if n > 0 && msg[0] == 0x01 {
            fetch_and_apply_policy();
        }

        if ticks % HEARTBEAT_INTERVAL == 0 {
            send_heartbeat();
        }
    }
}
