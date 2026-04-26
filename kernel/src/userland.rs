// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Protected-userspace program catalog backed by pkgfs.
//!
//! The UEFI loader stages a persistent package-store image from the ESP. The
//! kernel mounts that image at `/pkg`, and this module resolves service names
//! against the bootstrap graph manifest before launching protected ELFs.

use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::arch::serial;
use crate::bootstrap_manifest::{self, GraphManifest};
use crate::task::tcb::TaskId;

struct ProtectedProgram {
    name: &'static [u8],
    path: &'static [u8],
}

struct ResolvedProgram {
    task_name: Vec<u8>,
    path: Vec<u8>,
}

const PROGRAMS: [ProtectedProgram; 23] = [
    ProtectedProgram {
        name: b"init",
        path: b"/pkg/services/init.elf",
    },
    ProtectedProgram {
        name: b"servicemgr",
        path: b"/pkg/services/servicemgr.elf",
    },
    ProtectedProgram {
        name: b"graphd",
        path: b"/pkg/services/graphd.elf",
    },
    ProtectedProgram {
        name: b"modeld",
        path: b"/pkg/services/modeld.elf",
    },
    ProtectedProgram {
        name: b"trainerd",
        path: b"/pkg/services/trainerd.elf",
    },
    ProtectedProgram {
        name: b"artifactsd",
        path: b"/pkg/services/artifactsd.elf",
    },
    ProtectedProgram {
        name: b"sysd",
        path: b"/pkg/services/sysd.elf",
    },
    ProtectedProgram {
        name: b"netd",
        path: b"/pkg/services/netd.elf",
    },
    ProtectedProgram {
        name: b"compositor",
        path: b"/pkg/services/compositor.elf",
    },
    ProtectedProgram {
        name: b"ai-console",
        path: b"/pkg/apps/ai-console.elf",
    },
    ProtectedProgram {
        name: b"greeter",
        path: b"/pkg/apps/greeter.elf",
    },
    ProtectedProgram {
        name: b"launcher",
        path: b"/pkg/apps/launcher.elf",
    },
    ProtectedProgram {
        name: b"cube",
        path: b"/pkg/apps/cube.elf",
    },
    ProtectedProgram {
        name: b"notepad",
        path: b"/pkg/apps/notepad.elf",
    },
    ProtectedProgram {
        name: b"calculator",
        path: b"/pkg/apps/calculator.elf",
    },
    ProtectedProgram {
        name: b"paint",
        path: b"/pkg/apps/paint.elf",
    },
    ProtectedProgram {
        name: b"terminal",
        path: b"/pkg/apps/terminal.elf",
    },
    ProtectedProgram {
        name: b"editor",
        path: b"/pkg/apps/editor.elf",
    },
    ProtectedProgram {
        name: b"files",
        path: b"/pkg/apps/files.elf",
    },
    ProtectedProgram {
        name: b"display-canary",
        path: b"/pkg/apps/display-canary.elf",
    },
    ProtectedProgram {
        name: b"ssh",
        path: b"/pkg/apps/ssh.elf",
    },
    ProtectedProgram {
        name: b"settings",
        path: b"/pkg/apps/settings.elf",
    },
    ProtectedProgram {
        name: b"config",
        path: b"/pkg/apps/settings.elf",
    },
];

const SERVICES_TXT_COMPAT: &[u8] = b"/pkg/config/services.txt";

static MANIFEST_CACHE: Mutex<Option<GraphManifest>> = Mutex::new(None);

fn write_line_joined(prefix: &[u8], middle: &[u8], suffix: &[u8]) {
    let mut line = [0u8; 160];
    let mut len = 0usize;
    for part in [prefix, middle, suffix] {
        if len + part.len() > line.len() {
            serial::write_line(b"[userland] log line truncated");
            return;
        }
        line[len..len + part.len()].copy_from_slice(part);
        len += part.len();
    }
    serial::write_line(&line[..len]);
}

fn write_line_u64(prefix: &[u8], value: u64, suffix: &[u8]) {
    let mut line = [0u8; 64];
    let mut len = 0usize;
    if len + prefix.len() > line.len() {
        serial::write_line(b"[userland] log line truncated");
        return;
    }
    line[len..len + prefix.len()].copy_from_slice(prefix);
    len += prefix.len();
    let mut digits = [0u8; 20];
    let mut value = value;
    if value == 0 {
        if len < line.len() {
            line[len] = b'0';
            len += 1;
        }
    } else {
        let mut dlen = 0usize;
        while value > 0 && dlen < digits.len() {
            digits[dlen] = b'0' + (value % 10) as u8;
            value /= 10;
            dlen += 1;
        }
        while dlen > 0 {
            dlen -= 1;
            if len < line.len() {
                line[len] = digits[dlen];
                len += 1;
            }
        }
    }
    if len + suffix.len() > line.len() {
        serial::write_line(b"[userland] log line truncated");
        return;
    }
    line[len..len + suffix.len()].copy_from_slice(suffix);
    len += suffix.len();
    serial::write_line(&line[..len]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootstrapPreflightError {
    MissingPath(&'static [u8]),
    NotRegular(&'static [u8]),
    EmptyFile(&'static [u8]),
    BadElf(&'static [u8]),
    ReadFailed(&'static [u8]),
    ManifestMissingEntry(&'static [u8]),
    ManifestInvalid(&'static [u8]),
}

impl BootstrapPreflightError {
    pub fn path(self) -> &'static [u8] {
        match self {
            Self::MissingPath(path)
            | Self::NotRegular(path)
            | Self::EmptyFile(path)
            | Self::BadElf(path)
            | Self::ReadFailed(path)
            | Self::ManifestMissingEntry(path)
            | Self::ManifestInvalid(path) => path,
        }
    }

    pub fn reason(self) -> &'static [u8] {
        match self {
            Self::MissingPath(_) => b"missing",
            Self::NotRegular(_) => b"not a regular file",
            Self::EmptyFile(_) => b"empty",
            Self::BadElf(_) => b"invalid ELF header",
            Self::ReadFailed(_) => b"read failed",
            Self::ManifestMissingEntry(_) => b"manifest missing required service",
            Self::ManifestInvalid(_) => b"invalid graph manifest",
        }
    }
}

pub fn spawn_named_service(name: &[u8]) -> Option<TaskId> {
    write_line_joined(b"[userland] resolve ", trim_name(name), b"");
    let resolved = match resolve_program(name) {
        Some(resolved) => resolved,
        None => {
            write_line_joined(b"[userland] ERROR: resolve failed ", trim_name(name), b"");
            return None;
        }
    };

    // Keep singleton services/apps from launching duplicate instances during
    // desktop handoff retries or repeated launcher clicks.
    if (resolved.task_name == b"launcher" || resolved.task_name == b"compositor")
        && let Some(existing) = crate::task::table::active_task_id_by_name(&resolved.task_name)
    {
        write_line_u64(b"[userland] singleton active ", existing, b"");
        return Some(existing);
    }

    write_line_joined(b"[userland] load ", &resolved.task_name, b"");
    let image = match load_service_image(&resolved.path) {
        Some(image) => image,
        None => {
            write_line_joined(b"[userland] ERROR: load failed ", &resolved.path, b"");
            return None;
        }
    };
    write_line_joined(b"[userland] launch ", &resolved.task_name, b"");
    write_line_joined(b"[userland] image loaded ", &resolved.task_name, b"");
    let task_id = match crate::task::table::create_user_task_from_elf(&resolved.task_name, &image) {
        Some(task_id) => task_id,
        None => {
            write_line_joined(
                b"[userland] ERROR: create_user_task_from_elf failed ",
                &resolved.task_name,
                b"",
            );
            return None;
        }
    };
    write_line_joined(b"[userland] task created ", &resolved.task_name, b"");

    // Apps run with the least-privilege profile; services get the protected
    // profile.  Both must be explicit — the default is APP_STRICT.
    if resolved.path.starts_with(b"/pkg/apps/") {
        if crate::task::table::apply_app_seccomp_profile(task_id) {
            write_line_joined(
                b"[userland] app-strict seccomp applied ",
                &resolved.task_name,
                b"",
            );
        } else {
            write_line_joined(
                b"[userland] WARNING: app-strict seccomp failed ",
                &resolved.task_name,
                b"",
            );
        }
    } else {
        // Trusted services (/pkg/services/ or kernel-managed) run with the
        // protected-strict profile which allows driver, registry, and IPC ops.
        if let Some(idx) = crate::task::table::task_index_by_id(task_id) {
            if crate::security::seccomp::set_protected_strict(idx) {
                write_line_joined(
                    b"[userland] protected-strict seccomp applied ",
                    &resolved.task_name,
                    b"",
                );
            } else {
                write_line_joined(
                    b"[userland] WARNING: protected-strict seccomp failed ",
                    &resolved.task_name,
                    b"",
                );
            }
        }
    }

    grant_service_ipc_caps(task_id, &resolved.task_name);

    if let Some((stable_id, node_id)) =
        crate::graph::bootstrap::mark_service_launched(&resolved.task_name, task_id)
    {
        crate::registry::mark_service_launched(
            &resolved.task_name,
            crate::uuid::TaskUuid::from_task_id(task_id),
        );
        let mut line = [0u8; 96];
        let mut len = 0usize;
        let part = b"[userland] graph bind sid=".as_slice();
        line[len..len + part.len()].copy_from_slice(part);
        len += part.len();
        append_u64(&mut line, &mut len, stable_id as u64);
        let part = b" node=".as_slice();
        line[len..len + part.len()].copy_from_slice(part);
        len += part.len();
        append_u64(&mut line, &mut len, node_id);
        serial::write_line(&line[..len]);
        write_line_joined(b"[userland] graph bind complete ", &resolved.task_name, b"");
    } else {
        let _ = crate::graph::seed::register_task(
            &resolved.task_name,
            crate::graph::types::NODE_ID_KERNEL,
        );
    }
    let mut line = [0u8; 96];
    let mut len = 0usize;
    for part in [
        b"[userland] launched ".as_slice(),
        &resolved.task_name,
        b" id=".as_slice(),
    ] {
        if len + part.len() > line.len() {
            serial::write_line(b"[userland] log line truncated");
            break;
        }
        line[len..len + part.len()].copy_from_slice(part);
        len += part.len();
    }
    append_u64(&mut line, &mut len, task_id);
    serial::write_line(&line[..len]);
    write_line_joined(b"[userland] spawn complete ", &resolved.task_name, b"");

    // Register the compositor task index so SYS_SURFACE_PRESENT can wake it.
    if resolved.task_name == b"compositor" {
        // task_id is the TCB index (usize), not a TaskId.  Look it up.
        if let Some(idx) = crate::task::table::task_index_by_id(task_id) {
            crate::syscall::register_compositor_task(idx);
            let _ = crate::task::table::set_priority(idx, 3);
            serial::write_line(b"[userland] compositor task registered for surface wakeup");
        }
    }

    if resolved.task_name == b"sshd"
        && let Some(idx) = crate::task::table::task_index_by_id(task_id)
    {
        let _ = crate::task::table::set_priority(idx, 1);
    }

    Some(task_id)
}

fn grant_service_ipc_caps(task_id: TaskId, service_name: &[u8]) {
    use crate::ipc::capability::{CAP_MANAGE, CAP_RECV, CAP_SEND};
    use crate::uuid::ChannelUuid;

    let grant_send = |name: &[u8]| {
        let uuid = ChannelUuid::from_service_name(name);
        let _ = crate::task::table::ipc_cap_grant_by_task_id(task_id, uuid, CAP_SEND);
    };

    let own_uuid = ChannelUuid::from_service_name(service_name);
    // Service tasks must always be able to receive/manage their own inbox,
    // even if registry hydration races with early task startup.
    let _ = crate::task::table::ipc_cap_grant_by_task_id(task_id, own_uuid, CAP_RECV | CAP_MANAGE);

    if let Some(binding) = crate::registry::lookup(service_name) {
        let _ = crate::task::table::ipc_cap_grant_by_task_id(
            task_id,
            binding.channel_uuid,
            CAP_RECV | CAP_MANAGE,
        );
    }

    // Service readiness/control-plane routes.
    grant_send(b"bootstrap");
    grant_send(b"servicemgr");
    grant_send(b"init");

    let Some(manifest) = load_and_cache_manifest() else {
        // Legacy fallback: grant send to all known protected services when manifest is unavailable.
        for svc in [
            b"bootstrap".as_slice(),
            b"servicemgr",
            b"graphd",
            b"modeld",
            b"trainerd",
            b"artifactsd",
            b"sysd",
            b"compositor",
            b"init",
        ] {
            grant_send(svc);
        }
        return;
    };

    let Some(service) = manifest.find_service(service_name) else {
        return;
    };

    for dep in &manifest.dependencies {
        if dep.from_id != service.stable_id {
            continue;
        }
        if let Some(target) = manifest.find_service_by_id(dep.to_id) {
            grant_send(&target.name);
        }
    }

    if service_name == b"servicemgr" {
        for entry in &manifest.services {
            grant_send(&entry.name);
        }
    }
}

pub fn image_for_named_service(name: &[u8]) -> Option<Vec<u8>> {
    let resolved = resolve_program(name)?;
    load_service_image(&resolved.path)
}

pub fn manifest_declares_service(name: &[u8]) -> bool {
    load_and_cache_manifest()
        .map(|manifest| manifest.find_service(trim_name(name)).is_some())
        .unwrap_or(false)
}

pub fn bootstrap_preflight() -> Result<usize, BootstrapPreflightError> {
    if let Some(manifest) = load_and_cache_manifest() {
        crate::graph::bootstrap::sync_manifest(&manifest);
        crate::registry::sync_from_manifest(&manifest);

        let mut checked = 0usize;
        for service in &manifest.services {
            match check_elf_program_bytes(&service.path) {
                Ok(()) => {
                    checked += 1;
                }
                Err(err) if !service.critical => {
                    crate::graph::bootstrap::mark_service_missing(&service.name);
                    serial::write_bytes(b"[userland] optional bootstrap service unavailable: ");
                    serial::write_bytes(&service.name);
                    serial::write_bytes(b" <- ");
                    serial::write_line(err.reason());
                }
                Err(err) => return Err(err),
            }
        }
        return Ok(checked + manifest.dependencies.len());
    }

    bootstrap_preflight_legacy()
}

fn bootstrap_preflight_legacy() -> Result<usize, BootstrapPreflightError> {
    let mut checked = 0usize;

    check_elf_program_bytes(b"/pkg/services/init.elf")?;
    checked += 1;
    check_elf_program_bytes(b"/pkg/services/servicemgr.elf")?;
    checked += 1;

    let manifest = read_required_file(SERVICES_TXT_COMPAT)?;
    checked += 1;

    for &path in [
        b"/pkg/services/graphd.elf".as_slice(),
        b"/pkg/services/modeld.elf".as_slice(),
        b"/pkg/services/trainerd.elf".as_slice(),
        b"/pkg/services/artifactsd.elf".as_slice(),
        b"/pkg/services/sysd.elf".as_slice(),
        b"/pkg/services/compositor.elf".as_slice(),
    ]
    .iter()
    {
        check_elf_program_bytes(path)?;
        checked += 1;
        if !manifest_lists_path(&manifest, path) {
            return Err(BootstrapPreflightError::ManifestMissingEntry(path));
        }
    }

    Ok(checked)
}

fn load_service_image(path: &[u8]) -> Option<Vec<u8>> {
    let meta = crate::vfs::lookup(path).ok()?;
    let size = usize::try_from(meta.size).ok()?;
    if size == 0 {
        return None;
    }

    let fd = crate::vfs::open(path).ok()?;
    let mut image = vec![0u8; size];
    let mut filled = 0usize;

    while filled < image.len() {
        match crate::vfs::read(fd, &mut image[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => {
                let _ = crate::vfs::close(fd);
                return None;
            }
        }
    }

    let _ = crate::vfs::close(fd);
    if filled != image.len() {
        return None;
    }

    Some(image)
}

fn append_u64(out: &mut [u8], len: &mut usize, mut value: u64) {
    if value == 0 {
        if *len < out.len() {
            out[*len] = b'0';
            *len += 1;
        }
        return;
    }

    let mut digits = [0u8; 20];
    let mut dlen = 0usize;
    while value > 0 && dlen < digits.len() {
        digits[dlen] = b'0' + (value % 10) as u8;
        value /= 10;
        dlen += 1;
    }
    while dlen > 0 {
        dlen -= 1;
        if *len < out.len() {
            out[*len] = digits[dlen];
            *len += 1;
        }
    }
}

fn load_and_cache_manifest() -> Option<GraphManifest> {
    if let Some(cached) = MANIFEST_CACHE.lock().as_ref().cloned() {
        serial::write_line(b"[userland] manifest cache hit");
        return Some(cached);
    }

    serial::write_line(b"[userland] manifest read begin");
    let bytes = read_required_file(bootstrap_manifest::MANIFEST_PATH).ok()?;
    serial::write_line(b"[userland] manifest read done");
    let manifest = bootstrap_manifest::parse(&bytes).ok()?;
    serial::write_line(b"[userland] manifest parse done");
    *MANIFEST_CACHE.lock() = Some(manifest.clone());
    serial::write_line(b"[userland] manifest cache store done");
    Some(manifest)
}

fn lookup(name: &[u8]) -> Option<&'static ProtectedProgram> {
    let key = trim_name(name);
    PROGRAMS.iter().find(|program| program.name == key)
}

fn check_elf_program_bytes(path: &[u8]) -> Result<(), BootstrapPreflightError> {
    let bytes = read_required_dynamic_file(path)?;
    if bytes.len() < 4 || bytes[..4] != [0x7F, b'E', b'L', b'F'] {
        return Err(BootstrapPreflightError::BadElf(stabilize_path(path)));
    }
    Ok(())
}

fn read_required_dynamic_file(path: &[u8]) -> Result<Vec<u8>, BootstrapPreflightError> {
    let stable = stabilize_path(path);
    read_required_file(stable)
}

fn read_required_file(path: &'static [u8]) -> Result<Vec<u8>, BootstrapPreflightError> {
    write_line_joined(b"[userland] file lookup ", path, b"");
    let meta = crate::vfs::lookup(path).map_err(|_| BootstrapPreflightError::MissingPath(path))?;
    if meta.file_type != crate::vfs::FileType::Regular {
        return Err(BootstrapPreflightError::NotRegular(path));
    }
    let size = usize::try_from(meta.size).map_err(|_| BootstrapPreflightError::ReadFailed(path))?;
    if size == 0 {
        return Err(BootstrapPreflightError::EmptyFile(path));
    }
    write_line_joined(b"[userland] file open ", path, b"");
    let fd = crate::vfs::open(path).map_err(|_| BootstrapPreflightError::ReadFailed(path))?;
    let mut image = vec![0u8; size];
    let mut filled = 0usize;
    write_line_joined(b"[userland] file read begin ", path, b"");

    while filled < image.len() {
        match crate::vfs::read(fd, &mut image[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => {
                let _ = crate::vfs::close(fd);
                return Err(BootstrapPreflightError::ReadFailed(path));
            }
        }
    }

    let _ = crate::vfs::close(fd);
    write_line_joined(b"[userland] file read done ", path, b"");
    if filled != image.len() {
        return Err(BootstrapPreflightError::ReadFailed(path));
    }
    Ok(image)
}

fn manifest_lists_path(manifest: &[u8], wanted: &[u8]) -> bool {
    manifest
        .split(|&byte| byte == b'\n')
        .map(trim_manifest_line)
        .any(|line| line == wanted)
}

fn resolve_program(name: &[u8]) -> Option<ResolvedProgram> {
    let key = trim_name(name);
    if key.starts_with(b"/boot/") || key.starts_with(b"/pkg/") {
        return Some(ResolvedProgram {
            task_name: task_name_from_path(key).to_vec(),
            path: key.to_vec(),
        });
    }

    if let Some(manifest) = load_and_cache_manifest()
        && let Some(service) = manifest.find_service(key)
    {
        return Some(ResolvedProgram {
            task_name: service.name.clone(),
            path: service.path.clone(),
        });
    }

    let program = lookup(key)?;
    Some(ResolvedProgram {
        task_name: program.name.to_vec(),
        path: program.path.to_vec(),
    })
}

fn stabilize_path(path: &[u8]) -> &'static [u8] {
    if path == b"/pkg/services/init.elf" {
        b"/pkg/services/init.elf"
    } else if path == b"/pkg/services/servicemgr.elf" {
        b"/pkg/services/servicemgr.elf"
    } else if path == b"/pkg/services/graphd.elf" {
        b"/pkg/services/graphd.elf"
    } else if path == b"/pkg/services/modeld.elf" {
        b"/pkg/services/modeld.elf"
    } else if path == b"/pkg/services/trainerd.elf" {
        b"/pkg/services/trainerd.elf"
    } else if path == b"/pkg/services/artifactsd.elf" {
        b"/pkg/services/artifactsd.elf"
    } else if path == b"/pkg/services/sysd.elf" {
        b"/pkg/services/sysd.elf"
    } else if path == b"/pkg/services/compositor.elf" {
        b"/pkg/services/compositor.elf"
    } else if path == b"/pkg/services/sshd.elf" {
        b"/pkg/services/sshd.elf"
    } else if path == b"/pkg/services/netd.elf" {
        b"/pkg/services/netd.elf"
    } else if path == b"/pkg/config/bootstrap.graph" {
        b"/pkg/config/bootstrap.graph"
    } else if path == b"/pkg/config/services.txt" {
        b"/pkg/config/services.txt"
    } else {
        b"/pkg/config/bootstrap.graph"
    }
}

fn task_name_from_path(path: &[u8]) -> &[u8] {
    let basename = path
        .rsplit(|&b| b == b'/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(path);
    basename
        .split(|&b| b == b'.')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(basename)
}

fn trim_name(name: &[u8]) -> &[u8] {
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    &name[..end]
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
