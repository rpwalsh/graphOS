#![no_std]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#![no_main]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::panic(info)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let mut inbox = [0u8; 32];
    let mut fabric_ready = false;
    let init_channel = runtime::service_inbox_or_die(b"init");

    runtime::claim_inbox(init_channel);
    runtime::write_line(b"[uinit] protected PID1 online\n");
    let _ = runtime::bootstrap_status(b"uinit-online");

    if !runtime::spawn_named_checked(b"servicemgr\0") {
        runtime::write_line(b"[uinit] failed to spawn servicemgr\n");
        let _ = runtime::bootstrap_status(b"servicemgr-spawn-failed");
        runtime::exit(1);
    }

    runtime::write_line(b"[uinit] servicemgr spawn requested\n");

    for _ in 0..192 {
        if let Some(meta) = runtime::try_recv(init_channel, &mut inbox)
            && meta.tag == runtime::TAG_SERVICE_STATUS
            && &inbox[..meta.payload_len] == b"fabric-ready"
        {
            runtime::write_line(b"[uinit] service fabric alive\n");
            let _ = runtime::bootstrap_status(b"fabric-ready");
            fabric_ready = true;
            break;
        }

        runtime::yield_now();
    }

    if !fabric_ready {
        runtime::write_line(b"[uinit] servicemgr wait budget expired; continuing degraded\n");
        let _ = runtime::bootstrap_status(b"fabric-degraded");
    }

    loop {
        let raw = runtime::channel_recv(init_channel, &mut inbox);
        if raw == u64::MAX {
            runtime::yield_now();
            continue;
        }
        let payload_len = (raw & 0xFFFF) as usize;
        if !fabric_ready && &inbox[..payload_len] == b"fabric-ready" {
            runtime::write_line(b"[uinit] service fabric alive\n");
            let _ = runtime::bootstrap_status(b"fabric-ready");
            fabric_ready = true;
        }
        if &inbox[..payload_len] == b"shutdown" {
            let _ = runtime::bootstrap_status(b"uinit-shutdown");
            runtime::write_line(b"[uinit] shutdown request received\n");
            runtime::exit(0);
        }
    }
}
