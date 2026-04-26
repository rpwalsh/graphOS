// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS authentication daemon (`authn`) — WebAuthn RP + FIDO2 gate.

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

const SYS_FIDO2_GET_ASSERTION: u64 = 0x600;

// ---------------------------------------------------------------------------
// Message formats
// ---------------------------------------------------------------------------

const MSG_AUTHN_REQUEST:  u8 = 0x01;
const MSG_AUTHN_RESPONSE: u8 = 0x81;
const CHALLENGE_LEN:      usize = 32;
const UUID_LEN:           usize = 16;

// ---------------------------------------------------------------------------
// Credential store (static, max 32 entries)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Credential {
    user_uuid: [u8; UUID_LEN],
    pub_key:   [u8; 65], // uncompressed P-256 point
    cred_id:   [u8; 64],
    active:    bool,
}

impl Credential {
    const EMPTY: Self = Self {
        user_uuid: [0u8; UUID_LEN],
        pub_key:   [0u8; 65],
        cred_id:   [0u8; 64],
        active:    false,
    };
}

static mut CREDENTIALS: [Credential; 32] = [Credential::EMPTY; 32];

fn find_credential(cred_id: &[u8]) -> Option<Credential> {
    // SAFETY: single-threaded service; no concurrent mutation.
    let creds = unsafe { &raw const CREDENTIALS };
    let creds = unsafe { &*creds };
    creds.iter().copied().find(|c| c.active && c.cred_id[..cred_id.len()] == *cred_id)
}

// ---------------------------------------------------------------------------
// Authentication logic
// ---------------------------------------------------------------------------

fn handle_authn_request(msg: &[u8]) -> [u8; UUID_LEN] {
    // msg layout: [MSG_AUTHN_REQUEST, challenge(32), origin_len(1), origin(...)]
    if msg.len() < 1 + CHALLENGE_LEN + 1 {
        runtime::write_line(b"[authn] request too short");
        return [0u8; UUID_LEN];
    }
    if msg[0] != MSG_AUTHN_REQUEST {
        return [0u8; UUID_LEN];
    }
    let challenge = &msg[1..1 + CHALLENGE_LEN];

    // Call SYS_FIDO2_GET_ASSERTION via raw_syscall.
    let mut authr_data = [0u8; 256];
    let result = runtime::raw_syscall(
            SYS_FIDO2_GET_ASSERTION,
            challenge.as_ptr() as u64,
            CHALLENGE_LEN as u64,
            authr_data.as_mut_ptr() as u64,
            authr_data.len() as u64,
        );

    if result == 0 {
        runtime::write_line(b"[authn] FIDO2 assertion failed");
        return [0u8; UUID_LEN];
    }

    // Parse authenticatorData to extract credential ID (bytes 37..37+len).
    let cred_id_len = authr_data.get(37).copied().unwrap_or(0) as usize;
    let cred_id_start = 38;
    let cred_id_end   = cred_id_start + cred_id_len;

    if let Some(cred) = find_credential(&authr_data[cred_id_start..cred_id_end.min(authr_data.len())]) {
        runtime::write_line(b"[authn] assertion verified");
        cred.user_uuid
    } else {
        runtime::write_line(b"[authn] credential not found");
        [0u8; UUID_LEN]
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[authn] started");

    const CHANNEL_ID: u32 = 0xAE77_0001;

    loop {
        let mut msg = [0u8; 512];
        let n = runtime::channel_recv(CHANNEL_ID, &mut msg) as usize;
        if n == 0 { continue; }

        let user_uuid = handle_authn_request(&msg[..n]);

        // Reply with MSG_AUTHN_RESPONSE + ok(1) + user_uuid(16).
        let mut resp = [0u8; 18];
        resp[0] = MSG_AUTHN_RESPONSE;
        let ok = user_uuid != [0u8; UUID_LEN];
        resp[1] = if ok { 1 } else { 0 };
        resp[2..18].copy_from_slice(&user_uuid);

        runtime::channel_send(CHANNEL_ID, &resp, 0);
    }
}
