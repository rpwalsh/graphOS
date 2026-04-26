// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Regression tests for the built-in RAM-backed VFS path.

use crate::diag;
use crate::vfs;

pub fn run_tests() -> u32 {
    let mut failures = 0;

    diag::test_info(b"vfs: begin smoke suite");

    diag::test_info(b"vfs: test_ramfs_round_trip");
    if !test_ramfs_round_trip() {
        failures += 1;
    }
    diag::test_info(b"vfs: test_boot_service_catalog");
    if !test_boot_service_catalog() {
        failures += 1;
    }
    diag::test_info(b"vfs: test_boot_graph_manifest");
    if !test_boot_graph_manifest() {
        failures += 1;
    }
    diag::test_info(b"vfs: test_cube_boot_artifacts");
    if !test_cube_boot_artifacts() {
        failures += 1;
    }
    diag::test_info(b"vfs: test_graphfs_state_view");
    if !test_graphfs_state_view() {
        failures += 1;
    }
    diag::test_info(b"vfs: test_graphfs_service_view");
    if !test_graphfs_service_view() {
        failures += 1;
    }
    diag::test_info(b"vfs: test_posixfs_status_alias");
    if !test_posixfs_status_alias() {
        failures += 1;
    }
    diag::test_info(b"vfs: test_posixfs_graph_alias_round_trip");
    if !test_posixfs_graph_alias_round_trip() {
        failures += 1;
    }
    diag::test_info(b"vfs: test_boot_config_manifest");
    if !test_boot_config_manifest() {
        failures += 1;
    }
    diag::test_info(b"vfs: test_protected_bootstrap_preflight");
    if !test_protected_bootstrap_preflight() {
        failures += 1;
    }
    diag::test_info(b"vfs: test_persistfs_round_trip");
    if !test_persistfs_round_trip() {
        failures += 1;
    }

    diag::test_info(b"vfs: end smoke suite");

    failures
}

fn test_ramfs_round_trip() -> bool {
    let path = b"/tmp/mvp.txt";
    let payload = b"GraphOS MVP path is writable";

    let fd = match vfs::create(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: create /tmp/mvp.txt");
            return false;
        }
    };

    match vfs::write(fd, payload) {
        Ok(written) if written == payload.len() => {}
        _ => {
            let _ = vfs::close(fd);
            diag::test_fail(b"vfs: write /tmp/mvp.txt");
            return false;
        }
    }

    if vfs::close(fd).is_err() {
        diag::test_fail(b"vfs: close after write");
        return false;
    }

    match vfs::lookup(path) {
        Ok(meta) if meta.size == payload.len() as u64 => {}
        _ => {
            diag::test_fail(b"vfs: lookup metadata");
            return false;
        }
    }

    let fd = match vfs::open(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: reopen /tmp/mvp.txt");
            return false;
        }
    };

    let mut buf = [0u8; 64];
    let ok = matches!(vfs::read(fd, &mut buf), Ok(n) if n == payload.len() && &buf[..n] == payload);
    let _ = vfs::close(fd);

    if ok {
        diag::test_pass(b"vfs: ramfs round-trip");
        true
    } else {
        diag::test_fail(b"vfs: ramfs round-trip");
        false
    }
}

fn test_boot_service_catalog() -> bool {
    let path = b"/pkg/services/init.elf";

    let meta = match vfs::lookup(path) {
        Ok(meta) => meta,
        Err(_) => {
            diag::test_fail(b"vfs: lookup /pkg/services/init.elf");
            return false;
        }
    };

    if meta.size < 4 {
        diag::test_fail(b"vfs: pkgfs service image too small");
        return false;
    }

    let fd = match vfs::open(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: open /pkg/services/init.elf");
            return false;
        }
    };

    let mut magic = [0u8; 4];
    let ok = matches!(vfs::read(fd, &mut magic), Ok(4) if magic == [0x7F, b'E', b'L', b'F']);
    let _ = vfs::close(fd);

    if ok {
        diag::test_pass(b"vfs: pkgfs service catalog");
        true
    } else {
        diag::test_fail(b"vfs: pkgfs service catalog");
        false
    }
}

fn test_boot_config_manifest() -> bool {
    let path = b"/pkg/config/services.txt";

    let fd = match vfs::open(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: open /pkg/config/services.txt");
            return false;
        }
    };

    let mut buf = [0u8; 192];
    let ok = matches!(
        vfs::read(fd, &mut buf),
        Ok(n) if n > 0 && buf[..n].windows(b"/pkg/services/graphd.elf".len()).any(|w| w == b"/pkg/services/graphd.elf")
    );
    let _ = vfs::close(fd);

    if ok {
        diag::test_pass(b"vfs: pkgfs service manifest");
        true
    } else {
        diag::test_fail(b"vfs: pkgfs service manifest");
        false
    }
}

fn test_boot_graph_manifest() -> bool {
    let path = b"/pkg/config/bootstrap.graph";

    let fd = match vfs::open(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: open /pkg/config/bootstrap.graph");
            return false;
        }
    };

    let mut buf = [0u8; 256];
    let ok = matches!(
        vfs::read(fd, &mut buf),
        Ok(n) if n > 0
            && buf[..n].windows(b"graph-manifest-v1".len()).any(|w| w == b"graph-manifest-v1")
            && buf[..n].windows(b"service graphd critical".len()).any(|w| w == b"service graphd critical")
    );
    let _ = vfs::close(fd);

    if ok {
        diag::test_pass(b"vfs: pkgfs graph manifest");
        true
    } else {
        diag::test_fail(b"vfs: pkgfs graph manifest");
        false
    }
}

fn test_cube_boot_artifacts() -> bool {
    let cube_path = b"/pkg/apps/cube.elf";
    let fd = match vfs::open(cube_path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: open /pkg/apps/cube.elf");
            return false;
        }
    };

    let mut magic = [0u8; 4];
    let cube_ok = matches!(vfs::read(fd, &mut magic), Ok(4) if magic == [0x7F, b'E', b'L', b'F']);
    let _ = vfs::close(fd);
    if !cube_ok {
        diag::test_fail(b"vfs: cube app ELF invalid");
        return false;
    }

    let manifest_path = b"/pkg/config/bootstrap.graph";
    let fd = match vfs::open(manifest_path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: open /pkg/config/bootstrap.graph for cube policy");
            return false;
        }
    };

    let mut buf = [0u8; 640];
    let manifest_ok = matches!(
        vfs::read(fd, &mut buf),
        Ok(n) if n > 0
            && !buf[..n].windows(b"service compositor ".len()).any(|w| w == b"service compositor ")
    );
    let _ = vfs::close(fd);

    if !manifest_ok {
        diag::test_fail(b"vfs: cube boot manifest unexpectedly declares compositor");
        return false;
    }

    diag::test_pass(b"vfs: cube boot artifacts");
    true
}

fn test_graphfs_state_view() -> bool {
    let path = b"/graph/bootstrap/state";

    let fd = match vfs::open(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: open /graph/bootstrap/state");
            return false;
        }
    };

    let mut buf = [0u8; 320];
    let ok = matches!(
        vfs::read(fd, &mut buf),
        Ok(n) if n > 0
            && buf[..n].windows(b"graph-manifest-v1".len()).any(|w| w == b"graph-manifest-v1")
            && buf[..n].windows(b"service sid=".len()).any(|w| w == b"service sid=")
    );
    let _ = vfs::close(fd);

    if ok {
        diag::test_pass(b"vfs: graphfs bootstrap state");
        true
    } else {
        diag::test_fail(b"vfs: graphfs bootstrap state");
        false
    }
}

fn test_graphfs_service_view() -> bool {
    let path = b"/graph/services/servicemgr";

    let fd = match vfs::open(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: open /graph/services/servicemgr");
            return false;
        }
    };

    let mut buf = [0u8; 192];
    let ok = matches!(
        vfs::read(fd, &mut buf),
        Ok(n) if n > 0
            && buf[..n].windows(b"graph-service-v1".len()).any(|w| w == b"graph-service-v1")
            && buf[..n].windows(b"health=".len()).any(|w| w == b"health=")
    );
    let _ = vfs::close(fd);

    if ok {
        diag::test_pass(b"vfs: graphfs service view");
        true
    } else {
        diag::test_fail(b"vfs: graphfs service view");
        false
    }
}

fn test_posixfs_status_alias() -> bool {
    let path = b"/fs/proc/self/status";
    diag::test_info(b"vfs: posixfs status alias open");

    let fd = match vfs::open(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: open /fs/proc/self/status");
            return false;
        }
    };

    let mut buf = [0u8; 192];
    diag::test_info(b"vfs: posixfs status alias read");
    let ok = matches!(
        vfs::read(fd, &mut buf),
        Ok(n) if n > 0
            && buf[..n].windows(b"posix-status-v1".len()).any(|w| w == b"posix-status-v1")
            && buf[..n].windows(b"system=operational-bootstrap".len()).any(|w| w == b"system=operational-bootstrap")
    );
    diag::test_info(b"vfs: posixfs status alias close");
    let _ = vfs::close(fd);

    if ok {
        diag::test_pass(b"vfs: posixfs status alias");
        true
    } else {
        diag::test_fail(b"vfs: posixfs status alias");
        false
    }
}

fn test_posixfs_graph_alias_round_trip() -> bool {
    let path = b"/fs/var/lib/graphos/compat.txt";
    let payload = b"graph-native compatibility";
    diag::test_info(b"vfs: posixfs graph alias create");

    let fd = match vfs::create(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: create /fs/var/lib/graphos/compat.txt");
            return false;
        }
    };

    diag::test_info(b"vfs: posixfs graph alias write");
    match vfs::write(fd, payload) {
        Ok(n) if n == payload.len() => {}
        _ => {
            let _ = vfs::close(fd);
            diag::test_fail(b"vfs: write /fs/var/lib/graphos/compat.txt");
            return false;
        }
    }
    let _ = vfs::close(fd);

    diag::test_info(b"vfs: posixfs graph alias reopen");
    let fd = match vfs::open(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: reopen /fs/var/lib/graphos/compat.txt");
            return false;
        }
    };

    let mut buf = [0u8; 96];
    diag::test_info(b"vfs: posixfs graph alias read");
    let ok = matches!(vfs::read(fd, &mut buf), Ok(n) if n == payload.len() && &buf[..n] == payload);
    diag::test_info(b"vfs: posixfs graph alias close");
    let _ = vfs::close(fd);

    if ok {
        diag::test_pass(b"vfs: posixfs graph alias");
        true
    } else {
        diag::test_fail(b"vfs: posixfs graph alias");
        false
    }
}

fn test_protected_bootstrap_preflight() -> bool {
    if crate::userland::bootstrap_preflight().is_ok() {
        diag::test_pass(b"vfs: protected bootstrap preflight");
        true
    } else {
        diag::test_fail(b"vfs: protected bootstrap preflight");
        false
    }
}

fn test_persistfs_round_trip() -> bool {
    let path = b"/persist/bootstrap.test";
    let payload = b"graph bootstrap metadata survives writes";

    let fd = match vfs::create(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: create /persist/bootstrap.test");
            return false;
        }
    };

    match vfs::write(fd, payload) {
        Ok(n) if n == payload.len() => {}
        _ => {
            let _ = vfs::close(fd);
            diag::test_fail(b"vfs: write /persist/bootstrap.test");
            return false;
        }
    }
    let _ = vfs::close(fd);

    let fd = match vfs::open(path) {
        Ok(fd) => fd,
        Err(_) => {
            diag::test_fail(b"vfs: open /persist/bootstrap.test");
            return false;
        }
    };

    let mut buf = [0u8; 96];
    let ok = matches!(vfs::read(fd, &mut buf), Ok(n) if n == payload.len() && &buf[..n] == payload);
    let _ = vfs::close(fd);

    if ok {
        diag::test_pass(b"vfs: persistfs metadata round-trip");
        true
    } else {
        diag::test_fail(b"vfs: persistfs metadata round-trip");
        false
    }
}
