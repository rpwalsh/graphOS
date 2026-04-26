// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Graph-first read-only filesystem.
//!
//! This exposes the live kernel graph as a synthetic namespace. It is not a
//! generic filesystem and does not try to pretend otherwise. The goal is to
//! make the graph visible as a first-class object while keeping the VFS API
//! simple enough for early boot.

use alloc::vec::Vec;

use super::{FileMeta, FileType, FsOps, VfsError};

pub const GRAPH_MOUNT: &[u8] = b"/graph";

fn is_root(path: &[u8]) -> bool {
    path.is_empty() || path == b"/"
}

fn is_dir(path: &[u8]) -> bool {
    path == b"/bootstrap" || path == b"/services" || path == b"/nodes" || path == b"/edges"
}

fn validate_path(path: &[u8]) -> Result<(), VfsError> {
    if is_root(path) || is_dir(path) {
        return Ok(());
    }
    if path.is_empty() || path[0] != b'/' || path.len() > 63 {
        return Err(VfsError::InvalidPath);
    }
    if path.windows(2).any(|pair| pair == b"..") {
        return Err(VfsError::InvalidPath);
    }
    Ok(())
}

fn lookup(path: &[u8]) -> Result<FileMeta, VfsError> {
    validate_path(path)?;
    if is_root(path) {
        return Ok(dir_meta(4));
    }
    if is_dir(path) {
        return Ok(dir_meta(match path {
            b"/bootstrap" => 3,
            b"/services" => crate::graph::bootstrap::service_count() as u64,
            b"/nodes" => crate::graph::arena::node_count() as u64,
            b"/edges" => crate::graph::arena::edge_count() as u64,
            _ => 0,
        }));
    }

    if matches_file(path) {
        let content = build_content(path).ok_or(VfsError::NotFound)?;
        return Ok(FileMeta {
            file_type: FileType::Regular,
            size: content.len() as u64,
            node_id: service_node_id(path).unwrap_or_else(|| node_id(path).unwrap_or(0)),
            created_at: 0,
        });
    }

    Err(VfsError::NotFound)
}

fn read(path: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
    validate_path(path)?;
    if is_root(path) || is_dir(path) {
        return Err(VfsError::NotSupported);
    }

    let content = build_content(path).ok_or(VfsError::NotFound)?;
    slice_read(&content, offset, buf)
}

fn write(_: &[u8], _: u64, _: &[u8]) -> Result<usize, VfsError> {
    Err(VfsError::NotSupported)
}

fn fs_name() -> &'static [u8] {
    b"graphfs"
}

pub fn ops() -> FsOps {
    FsOps {
        lookup,
        read,
        write,
        fs_name,
        mkdir: |_| Err(VfsError::NotSupported),
        unlink: |_| Err(VfsError::NotSupported),
    }
}

fn matches_file(path: &[u8]) -> bool {
    path == b"/bootstrap/manifest"
        || path == b"/bootstrap/state"
        || path == b"/bootstrap/services"
        || path.starts_with(b"/services/")
        || path.starts_with(b"/nodes/")
        || path.starts_with(b"/edges/")
}

fn build_content(path: &[u8]) -> Option<Vec<u8>> {
    if path == b"/bootstrap/manifest" {
        return Some(crate::graph::bootstrap::manifest_snapshot());
    }
    if path == b"/bootstrap/state" {
        return Some(crate::graph::bootstrap::snapshot());
    }
    if path == b"/bootstrap/services" {
        return Some(build_services_listing());
    }
    if let Some(name) = path.strip_prefix(b"/services/") {
        return build_service_record(name);
    }
    if let Some(id_bytes) = path.strip_prefix(b"/nodes/") {
        return build_node_record(id_bytes);
    }
    if let Some(id_bytes) = path.strip_prefix(b"/edges/") {
        return build_edge_record(id_bytes);
    }
    None
}

fn build_services_listing() -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(b"graph-services-v1\n");
    if let Some(manifest) = crate::graph::bootstrap::manifest() {
        for service in manifest.services {
            append_service_summary(
                &mut out,
                &service.name,
                service.stable_id,
                crate::ipc::channel::alias_for_uuid(crate::uuid::ChannelUuid::from_service_name(
                    &service.name,
                ))
                .unwrap_or(0),
                service.critical,
                service.launcher.as_bytes(),
            );
        }
    }
    out
}

fn build_service_record(name: &[u8]) -> Option<Vec<u8>> {
    let manifest = crate::graph::bootstrap::manifest()?;
    let service = manifest.find_service(name)?;
    let health = crate::graph::bootstrap::service_health(&service.name)
        .unwrap_or(crate::graph::bootstrap::ServiceHealth::Defined);
    let node_id = crate::graph::bootstrap::service_node_id(&service.name).unwrap_or(0);

    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(b"graph-service-v1\n");
    out.extend_from_slice(b"name=");
    out.extend_from_slice(&service.name);
    out.push(b'\n');
    out.extend_from_slice(b"sid=");
    append_u64(&mut out, service.stable_id as u64);
    out.push(b'\n');
    out.extend_from_slice(b"node=");
    append_u64(&mut out, node_id);
    out.push(b'\n');
    out.extend_from_slice(b"channel=");
    append_u64(
        &mut out,
        crate::ipc::channel::alias_for_uuid(crate::uuid::ChannelUuid::from_service_name(
            &service.name,
        ))
        .unwrap_or(0) as u64,
    );
    out.push(b'\n');
    out.extend_from_slice(b"critical=");
    out.extend_from_slice(if service.critical { b"yes" } else { b"no" });
    out.push(b'\n');
    out.extend_from_slice(b"launcher=");
    out.extend_from_slice(service.launcher.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(b"health=");
    out.extend_from_slice(health.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(b"path=");
    out.extend_from_slice(&service.path);
    out.push(b'\n');
    Some(out)
}

fn build_node_record(id_bytes: &[u8]) -> Option<Vec<u8>> {
    let node_id = parse_u64(id_bytes)?;
    let node = crate::graph::arena::get_node(node_id)?;
    let mut out = Vec::with_capacity(192);
    out.extend_from_slice(b"graph-node-v1\n");
    out.extend_from_slice(b"id=");
    append_u64(&mut out, node.id);
    out.push(b'\n');
    out.extend_from_slice(b"kind=");
    append_u64(&mut out, node.kind as u64);
    out.push(b'\n');
    out.extend_from_slice(b"creator=");
    append_u64(&mut out, node.creator);
    out.push(b'\n');
    out.extend_from_slice(b"created=");
    append_u64(&mut out, node.created_at);
    out.push(b'\n');
    out.extend_from_slice(b"flags=0x");
    append_hex(&mut out, node.flags as u64);
    out.push(b'\n');
    out.extend_from_slice(b"degree_out=");
    append_u64(&mut out, node.degree_out as u64);
    out.push(b'\n');
    out.extend_from_slice(b"degree_in=");
    append_u64(&mut out, node.degree_in as u64);
    out.push(b'\n');
    Some(out)
}

fn build_edge_record(id_bytes: &[u8]) -> Option<Vec<u8>> {
    let edge_id = parse_u64(id_bytes)?;
    let edge = crate::graph::arena::get_edge(edge_id)?;
    let mut out = Vec::with_capacity(192);
    out.extend_from_slice(b"graph-edge-v1\n");
    out.extend_from_slice(b"id=");
    append_u64(&mut out, edge.id);
    out.push(b'\n');
    out.extend_from_slice(b"from=");
    append_u64(&mut out, edge.from);
    out.push(b'\n');
    out.extend_from_slice(b"to=");
    append_u64(&mut out, edge.to);
    out.push(b'\n');
    out.extend_from_slice(b"kind=");
    append_u64(&mut out, edge.kind as u64);
    out.push(b'\n');
    out.extend_from_slice(b"flags=0x");
    append_hex(&mut out, edge.flags as u64);
    out.push(b'\n');
    out.extend_from_slice(b"weight=");
    append_u64(&mut out, edge.weight as u64);
    out.push(b'\n');
    out.extend_from_slice(b"created=");
    append_u64(&mut out, edge.created_at);
    out.push(b'\n');
    Some(out)
}

fn slice_read(content: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
    let offset = offset as usize;
    if offset >= content.len() {
        return Ok(0);
    }
    let to_copy = (content.len() - offset).min(buf.len());
    buf[..to_copy].copy_from_slice(&content[offset..offset + to_copy]);
    Ok(to_copy)
}

fn parse_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0u64;
    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?;
        value = value.checked_add((byte - b'0') as u64)?;
    }
    Some(value)
}

fn service_node_id(path: &[u8]) -> Option<u64> {
    let name = path.strip_prefix(b"/services/")?;
    crate::graph::bootstrap::manifest()?
        .find_service(name)
        .and_then(|service| crate::graph::bootstrap::service_node_id(&service.name))
}

fn node_id(path: &[u8]) -> Option<u64> {
    path.strip_prefix(b"/nodes/").and_then(parse_u64)
}

fn append_service_summary(
    out: &mut Vec<u8>,
    name: &[u8],
    stable_id: u16,
    channel: u32,
    critical: bool,
    launcher: &[u8],
) {
    out.extend_from_slice(b"service ");
    out.extend_from_slice(name);
    out.extend_from_slice(b" sid=");
    append_u64(out, stable_id as u64);
    out.extend_from_slice(b" ch=");
    append_u64(out, channel as u64);
    out.extend_from_slice(b" critical=");
    out.extend_from_slice(if critical { b"yes" } else { b"no" });
    out.extend_from_slice(b" launcher=");
    out.extend_from_slice(launcher);
    out.push(b'\n');
}

fn append_u64(out: &mut Vec<u8>, mut value: u64) {
    if value == 0 {
        out.push(b'0');
        return;
    }
    let mut digits = [0u8; 20];
    let mut len = 0usize;
    while value > 0 {
        digits[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }
    while len > 0 {
        len -= 1;
        out.push(digits[len]);
    }
}

fn append_hex(out: &mut Vec<u8>, mut value: u64) {
    if value == 0 {
        out.push(b'0');
        return;
    }
    let mut digits = [0u8; 16];
    let mut len = 0usize;
    while value > 0 {
        let nibble = (value & 0xF) as u8;
        digits[len] = match nibble {
            0..=9 => b'0' + nibble,
            _ => b'a' + (nibble - 10),
        };
        value >>= 4;
        len += 1;
    }
    while len > 0 {
        len -= 1;
        out.push(digits[len]);
    }
}

fn dir_meta(size: u64) -> FileMeta {
    FileMeta {
        file_type: FileType::Directory,
        size,
        node_id: 0,
        created_at: 0,
    }
}
