#![no_std]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#![no_main]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;

const GRAPH_MANIFEST: &[u8] = b"/pkg/config/bootstrap.graph\0";
const SERVICES_MANIFEST_COMPAT: &[u8] = b"/pkg/config/services.txt\0";
const SHUTDOWN: &[u8] = b"shutdown";
const MAX_PENDING_READY: usize = 8;
const READY_WAIT_IDLE_PASSES: usize = 512;
const REGISTRY_HEALTH_LAUNCHED: u8 = 3;
const COMPOSITOR_SERVICE_NAME: &[u8] = b"compositor";

enum ManifestLaunchOutcome {
    Complete,
    Degraded,
    CriticalFailure,
    Shutdown,
    Unavailable,
}

struct ServiceSpec<'a> {
    name: &'a [u8],
    critical: bool,
    launcher: &'a [u8],
    path: &'a [u8],
}

#[derive(Clone, Copy)]
struct PendingService<'a> {
    name: &'a [u8],
    critical: bool,
    ready: bool,
}

impl PendingService<'_> {
    const EMPTY: Self = Self {
        name: b"",
        critical: false,
        ready: false,
    };
}

fn write_line_joined(prefix: &[u8], middle: &[u8], suffix: &[u8]) {
    let mut line = [0u8; 160];
    let mut len = 0usize;
    for part in [prefix, middle, suffix] {
        if len + part.len() + 1 > line.len() {
            runtime::write_line(b"[servicemgr] log line truncated\n");
            return;
        }
        line[len..len + part.len()].copy_from_slice(part);
        len += part.len();
    }
    line[len] = b'\n';
    runtime::write_line(&line[..=len]);
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::panic(info)
}

fn trim_manifest_line(line: &[u8]) -> &[u8] {
    let mut start = 0usize;
    while start < line.len() {
        match line[start] {
            b' ' | b'\t' | b'\r' => start += 1,
            _ => break,
        }
    }

    let mut end = line.len();
    let mut idx = start;
    while idx < end {
        if line[idx] == b'#' {
            end = idx;
            break;
        }
        idx += 1;
    }

    while end > start {
        match line[end - 1] {
            b' ' | b'\t' | b'\r' => end -= 1,
            _ => break,
        }
    }

    &line[start..end]
}

fn next_token<'a>(parts: &mut impl Iterator<Item = &'a [u8]>) -> Option<&'a [u8]> {
    parts.find(|token| !token.is_empty())
}

fn parse_service_spec(line: &[u8]) -> Option<ServiceSpec<'_>> {
    let trimmed = trim_manifest_line(line);
    if trimmed.is_empty() || trimmed == b"graph-manifest-v1" {
        return None;
    }

    let mut parts = trimmed.split(|&byte| byte == b' ' || byte == b'\t');
    if next_token(&mut parts)? != b"service" {
        return None;
    }
    let name = next_token(&mut parts)?;
    let criticality = next_token(&mut parts)?;
    let launcher = next_token(&mut parts)?;
    let path = next_token(&mut parts)?;

    Some(ServiceSpec {
        name,
        critical: criticality == b"critical",
        launcher,
        path,
    })
}

fn spawn_manifest_path(path: &[u8], name: &[u8], critical: bool) -> bool {
    let mut request = [0u8; 97];
    if path.len() >= request.len() {
        runtime::write_line(b"[servicemgr] manifest entry too long\n");
        return false;
    }
    request[..path.len()].copy_from_slice(path);
    request[path.len()] = 0;

    write_line_joined(b"[servicemgr] launch ", path, b"");
    write_line_joined(b"[servicemgr] spawn begin ", name, b"");
    let ok = runtime::spawn_named_checked(&request[..=path.len()]);
    if ok {
        write_line_joined(b"[servicemgr] spawn complete ", name, b"");
    }
    if !ok {
        if critical {
            let _ = runtime::bootstrap_named_status(b"spawn-failed:", name);
        } else {
            let _ = runtime::bootstrap_named_status(b"service-missing:", name);
        }
    }
    ok
}

fn remember_pending_service<'a>(
    pending: &mut [PendingService<'a>; MAX_PENDING_READY],
    pending_len: &mut usize,
    name: &'a [u8],
    critical: bool,
) {
    if *pending_len >= pending.len() {
        runtime::write_line(b"[servicemgr] ready wait list full\n");
        return;
    }

    pending[*pending_len] = PendingService {
        name,
        critical,
        ready: false,
    };
    *pending_len += 1;
}

fn mark_service_ready(pending: &mut [PendingService<'_>], name: &[u8]) -> bool {
    for service in pending.iter_mut() {
        if !service.ready && service.name == name {
            service.ready = true;
            return true;
        }
    }
    false
}

fn service_ready_via_registry(name: &[u8]) -> bool {
    runtime::registry_lookup(name)
        .map(|entry| entry.health >= REGISTRY_HEALTH_LAUNCHED)
        .unwrap_or(false)
}

fn should_skip_manifest_spawn(name: &[u8]) -> bool {
    // Desktop handoff may launch compositor before servicemgr optional phase.
    // Avoid a second compositor owner on the display path.
    if name == COMPOSITOR_SERVICE_NAME && service_ready_via_registry(name) {
        runtime::write_line(b"[servicemgr] compositor already online; skipping duplicate launch\n");
        return true;
    }
    false
}

fn await_service_readiness(pending: &mut [PendingService<'_>]) -> ManifestLaunchOutcome {
    if pending.is_empty() {
        return ManifestLaunchOutcome::Complete;
    }

    let mut inbox = [0u8; 64];
    let mut ready_count = 0usize;
    let mut idle_passes = 0usize;
    let service_mgr_channel = runtime::service_inbox_or_die(b"servicemgr");

    while ready_count < pending.len() && idle_passes < READY_WAIT_IDLE_PASSES {
        if let Some(meta) = runtime::try_recv(service_mgr_channel, &mut inbox) {
            idle_passes = 0;
            let payload = &inbox[..meta.payload_len];
            if payload == SHUTDOWN {
                runtime::write_line(b"[servicemgr] shutdown requested during readiness wait\n");
                return ManifestLaunchOutcome::Shutdown;
            }
            if mark_service_ready(pending, payload) {
                ready_count += 1;
                write_line_joined(b"[servicemgr] service online ", payload, b"");
            }
            continue;
        }

        runtime::yield_now();
        idle_passes += 1;
    }

    let mut degraded = false;
    for service in pending.iter_mut() {
        if service.ready {
            continue;
        }

        if service_ready_via_registry(service.name) {
            service.ready = true;
            write_line_joined(b"[servicemgr] service online (registry) ", service.name, b"");
            continue;
        }

        write_line_joined(b"[servicemgr] ready timeout ", service.name, b"");
        if service.critical {
            let _ = runtime::bootstrap_named_status(b"spawn-failed:", service.name);
            return ManifestLaunchOutcome::CriticalFailure;
        }

        let _ = runtime::bootstrap_named_status(b"service-missing:", service.name);
        degraded = true;
    }

    if degraded {
        ManifestLaunchOutcome::Degraded
    } else {
        ManifestLaunchOutcome::Complete
    }
}

fn launch_from_graph_manifest() -> ManifestLaunchOutcome {
    let fd = runtime::vfs_open(GRAPH_MANIFEST);
    if fd == u64::MAX {
        runtime::write_line(b"[servicemgr] graph manifest unavailable\n");
        return ManifestLaunchOutcome::Unavailable;
    }

    let mut buf = [0u8; 768];
    let bytes = runtime::vfs_read(fd, &mut buf);
    let _ = runtime::vfs_close(fd);
    if bytes == u64::MAX || bytes == 0 {
        runtime::write_line(b"[servicemgr] empty graph manifest\n");
        return ManifestLaunchOutcome::Unavailable;
    }

    runtime::write_line(b"[servicemgr] graph manifest loaded\n");

    let content_len = bytes as usize;
    let mut critical_failed = false;
    let mut optional_failed = false;
    let mut pending = [PendingService::EMPTY; MAX_PENDING_READY];
    let mut pending_len = 0usize;

    // Deterministic boot ordering:
    // 1. Launch the critical core.
    // 2. Wait for the critical core to announce online.
    // 3. Launch the optional tail and wait for any successful spawns to announce online.
    runtime::write_line(b"[servicemgr] critical launch phase\n");
    for critical_pass in [true, false] {
        let mut line_start = 0usize;
        if !critical_pass {
            pending_len = 0;
            runtime::write_line(b"[servicemgr] optional launch phase\n");
        }
        while line_start < content_len {
            let mut line_end = line_start;
            while line_end < content_len && buf[line_end] != b'\n' {
                line_end += 1;
            }

            if let Some(spec) = parse_service_spec(&buf[line_start..line_end])
                && spec.launcher == b"servicemgr"
                && spec.critical == critical_pass
            {
                if should_skip_manifest_spawn(spec.name) {
                    line_start = line_end.saturating_add(1);
                    continue;
                }
                if !spawn_manifest_path(spec.path, spec.name, spec.critical) {
                    if spec.critical {
                        critical_failed = true;
                        break;
                    } else {
                        write_line_joined(
                            b"[servicemgr] optional launch failed ",
                            spec.name,
                            b"",
                        );
                        optional_failed = true;
                    }
                } else {
                    remember_pending_service(&mut pending, &mut pending_len, spec.name, spec.critical);
                    runtime::yield_now();
                }
            }
            line_start = line_end.saturating_add(1);
        }

        if critical_failed {
            break;
        }

        if critical_pass {
            match await_service_readiness(&mut pending[..pending_len]) {
                ManifestLaunchOutcome::Complete => {}
                ManifestLaunchOutcome::Degraded | ManifestLaunchOutcome::CriticalFailure => {
                    critical_failed = true;
                    break;
                }
                ManifestLaunchOutcome::Shutdown => return ManifestLaunchOutcome::Shutdown,
                ManifestLaunchOutcome::Unavailable => {}
            }
            runtime::write_line(b"[servicemgr] critical fabric online\n");
            runtime::yield_now();
        } else {
            match await_service_readiness(&mut pending[..pending_len]) {
                ManifestLaunchOutcome::Complete => {}
                ManifestLaunchOutcome::Degraded => optional_failed = true,
                ManifestLaunchOutcome::CriticalFailure => {
                    critical_failed = true;
                    break;
                }
                ManifestLaunchOutcome::Shutdown => return ManifestLaunchOutcome::Shutdown,
                ManifestLaunchOutcome::Unavailable => {}
            }
        }
    }

    if critical_failed {
        ManifestLaunchOutcome::CriticalFailure
    } else if optional_failed {
        ManifestLaunchOutcome::Degraded
    } else {
        ManifestLaunchOutcome::Complete
    }
}

fn launch_from_services_txt() -> ManifestLaunchOutcome {
    let fd = runtime::vfs_open(SERVICES_MANIFEST_COMPAT);
    if fd == u64::MAX {
        runtime::write_line(b"[servicemgr] failed to open compatibility manifest\n");
        return ManifestLaunchOutcome::Unavailable;
    }

    let mut buf = [0u8; 256];
    let bytes = runtime::vfs_read(fd, &mut buf);
    let _ = runtime::vfs_close(fd);
    if bytes == u64::MAX || bytes == 0 {
        runtime::write_line(b"[servicemgr] empty compatibility manifest\n");
        return ManifestLaunchOutcome::Unavailable;
    }

    let mut line_start = 0usize;
    let content_len = bytes as usize;
    let mut critical_failed = false;

    while line_start < content_len {
        let mut line_end = line_start;
        while line_end < content_len && buf[line_end] != b'\n' {
            line_end += 1;
        }

        let path = trim_manifest_line(&buf[line_start..line_end]);
        if !path.is_empty() && !spawn_manifest_path(path, path, true) {
            critical_failed = true;
        }
        line_start = line_end.saturating_add(1);
    }

    if critical_failed {
        ManifestLaunchOutcome::CriticalFailure
    } else {
        ManifestLaunchOutcome::Complete
    }
}

fn relay_shutdown() {
    for service in [
        &b"graphd"[..],
        &b"modeld"[..],
        &b"trainerd"[..],
        &b"artifactsd"[..],
        &b"sysd"[..],
        &b"compositor"[..],
    ] {
        let Some(channel) = runtime::service_inbox(service) else {
            continue;
        };
        let _ = runtime::channel_send(channel, SHUTDOWN, runtime::TAG_SERVICE_STATUS);
        runtime::yield_now();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let service_mgr_channel = runtime::service_inbox_or_die(b"servicemgr");
    runtime::claim_inbox(service_mgr_channel);
    runtime::write_line(b"[servicemgr] protected fabric conductor online\n");
    let _ = runtime::bootstrap_status(b"servicemgr-online");
    let _ = runtime::bootstrap_status(b"service-ready:servicemgr");

    let outcome = match launch_from_graph_manifest() {
        ManifestLaunchOutcome::Complete => ManifestLaunchOutcome::Complete,
        ManifestLaunchOutcome::Degraded => {
            runtime::write_line(
                b"[servicemgr] noncritical bootstrap services missing; continuing degraded\n",
            );
            let _ = runtime::bootstrap_status(b"fabric-degraded");
            ManifestLaunchOutcome::Degraded
        }
        ManifestLaunchOutcome::CriticalFailure => {
            runtime::write_line(b"[servicemgr] critical bootstrap service missing\n");
            let _ = runtime::bootstrap_status(b"fabric-critical-failure");
            ManifestLaunchOutcome::CriticalFailure
        }
        ManifestLaunchOutcome::Shutdown => ManifestLaunchOutcome::Shutdown,
        ManifestLaunchOutcome::Unavailable => match launch_from_services_txt() {
            ManifestLaunchOutcome::Complete => ManifestLaunchOutcome::Complete,
            ManifestLaunchOutcome::Degraded => {
                let _ = runtime::bootstrap_status(b"fabric-degraded");
                ManifestLaunchOutcome::Degraded
            }
            ManifestLaunchOutcome::CriticalFailure => {
                let _ = runtime::bootstrap_status(b"fabric-critical-failure");
                ManifestLaunchOutcome::CriticalFailure
            }
            ManifestLaunchOutcome::Shutdown => ManifestLaunchOutcome::Shutdown,
            ManifestLaunchOutcome::Unavailable => {
                runtime::write_line(b"[servicemgr] no usable bootstrap manifest\n");
                let _ = runtime::bootstrap_status(b"fabric-critical-failure");
                ManifestLaunchOutcome::CriticalFailure
            }
        },
    };

    if matches!(outcome, ManifestLaunchOutcome::Shutdown) {
        runtime::write_line(b"[servicemgr] shutdown acknowledged during bootstrap\n");
        relay_shutdown();
        runtime::yield_cycles(4);
        runtime::exit(0);
    }

    if matches!(
        outcome,
        ManifestLaunchOutcome::Complete | ManifestLaunchOutcome::Degraded
    ) {
        runtime::write_line(b"[servicemgr] protected fanout complete\n");
        let _ = runtime::bootstrap_status(b"fanout-complete");
        let _ = runtime::bootstrap_status(b"fabric-ready");
        let _ = runtime::channel_send(
            runtime::service_inbox_or_die(b"init"),
            b"fabric-ready",
            runtime::TAG_SERVICE_STATUS,
        );
    }

    let mut inbox = [0u8; 64];
    loop {
        let raw = runtime::channel_recv(service_mgr_channel, &mut inbox);
        if raw == u64::MAX {
            runtime::yield_now();
            continue;
        }
        let payload_len = (raw & 0xFFFF) as usize;
        let payload = &inbox[..payload_len];

        if payload == SHUTDOWN {
            runtime::write_line(b"[servicemgr] shutdown fanout\n");
            let _ = runtime::bootstrap_status(b"servicemgr-shutdown");
            relay_shutdown();
            runtime::yield_cycles(4);
            runtime::exit(0);
        }
    }
}
