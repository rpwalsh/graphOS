// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Graph manifest v1 for the protected bootstrap fabric.
//!
//! The manifest is the static source of truth for the bootstrap service graph:
//! service nodes, launch ownership, inbox channels, and dependency edges.
//! A compatibility `/pkg/config/services.txt` view still exists, but it is now
//! derived from this manifest instead of being the primary model.

use alloc::vec::Vec;

use crate::uuid::ServiceUuid;

pub const MANIFEST_PATH: &[u8] = b"/pkg/config/bootstrap.graph";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceLauncher {
    Kernel,
    Init,
    ServiceMgr,
}

impl ServiceLauncher {
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Kernel => b"kernel",
            Self::Init => b"init",
            Self::ServiceMgr => b"servicemgr",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServiceDef {
    pub stable_id: u16,
    pub stable_uuid: ServiceUuid,
    pub name: Vec<u8>,
    pub path: Vec<u8>,
    pub critical: bool,
    pub launcher: ServiceLauncher,
}

#[derive(Clone, Debug)]
pub struct DependencyDef {
    pub from_id: u16,
    pub from_uuid: ServiceUuid,
    pub to_id: u16,
    pub to_uuid: ServiceUuid,
}

#[derive(Clone, Debug)]
pub struct GraphManifest {
    pub services: Vec<ServiceDef>,
    pub dependencies: Vec<DependencyDef>,
}

impl GraphManifest {
    pub fn find_service(&self, name: &[u8]) -> Option<&ServiceDef> {
        self.services.iter().find(|service| service.name == name)
    }

    pub fn find_service_mut(&mut self, name: &[u8]) -> Option<&mut ServiceDef> {
        self.services
            .iter_mut()
            .find(|service| service.name == name)
    }

    pub fn find_service_by_id(&self, stable_id: u16) -> Option<&ServiceDef> {
        self.services
            .iter()
            .find(|service| service.stable_id == stable_id)
    }

    pub fn find_service_by_uuid(&self, stable_uuid: ServiceUuid) -> Option<&ServiceDef> {
        self.services
            .iter()
            .find(|service| service.stable_uuid == stable_uuid)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestError {
    MissingHeader,
    BadHeader,
    BadServiceLine,
    BadDependencyLine,
    DuplicateService,
    UnknownDependencyEndpoint,
    InvalidCriticality,
    InvalidLauncher,
    InvalidPath,
    TooManyServices,
}

pub fn parse(bytes: &[u8]) -> Result<GraphManifest, ManifestError> {
    let mut header_seen = false;
    let mut manifest = GraphManifest {
        services: Vec::new(),
        dependencies: Vec::new(),
    };

    for raw_line in bytes.split(|&byte| byte == b'\n') {
        let line = trim_line(raw_line);
        if line.is_empty() {
            continue;
        }

        if !header_seen {
            header_seen = true;
            if line != b"graph-manifest-v1" {
                return Err(ManifestError::BadHeader);
            }
            continue;
        }

        let mut parts = line.split(|&byte| byte == b' ' || byte == b'\t');
        let Some(kind) = next_token(&mut parts) else {
            continue;
        };

        if kind == b"service" {
            parse_service_line(&mut manifest, &mut parts)?;
        } else if kind == b"depends" {
            parse_dependency_line(&mut manifest, &mut parts)?;
        } else {
            return Err(ManifestError::BadHeader);
        }
    }

    if !header_seen {
        return Err(ManifestError::MissingHeader);
    }

    Ok(manifest)
}

fn parse_service_line<'a>(
    manifest: &mut GraphManifest,
    parts: &mut impl Iterator<Item = &'a [u8]>,
) -> Result<(), ManifestError> {
    let Some(name) = next_token(parts) else {
        return Err(ManifestError::BadServiceLine);
    };
    let Some(criticality) = next_token(parts) else {
        return Err(ManifestError::BadServiceLine);
    };
    let Some(launcher_raw) = next_token(parts) else {
        return Err(ManifestError::BadServiceLine);
    };
    let Some(path) = next_token(parts) else {
        return Err(ManifestError::BadServiceLine);
    };
    if next_token(parts).is_some() {
        return Err(ManifestError::BadServiceLine);
    }

    if manifest.find_service(name).is_some() {
        return Err(ManifestError::DuplicateService);
    }
    if manifest.services.len() >= u16::MAX as usize {
        return Err(ManifestError::TooManyServices);
    }

    let critical = match criticality {
        b"critical" => true,
        b"optional" => false,
        _ => return Err(ManifestError::InvalidCriticality),
    };
    let launcher = match launcher_raw {
        b"kernel" => ServiceLauncher::Kernel,
        b"init" => ServiceLauncher::Init,
        b"servicemgr" => ServiceLauncher::ServiceMgr,
        _ => return Err(ManifestError::InvalidLauncher),
    };
    if path.is_empty() || path[0] != b'/' {
        return Err(ManifestError::InvalidPath);
    }

    manifest.services.push(ServiceDef {
        stable_id: (manifest.services.len() + 1) as u16,
        stable_uuid: ServiceUuid::from_service_name(name),
        name: name.to_vec(),
        path: path.to_vec(),
        critical,
        launcher,
    });
    Ok(())
}

fn parse_dependency_line<'a>(
    manifest: &mut GraphManifest,
    parts: &mut impl Iterator<Item = &'a [u8]>,
) -> Result<(), ManifestError> {
    let Some(from_name) = next_token(parts) else {
        return Err(ManifestError::BadDependencyLine);
    };
    let Some(to_name) = next_token(parts) else {
        return Err(ManifestError::BadDependencyLine);
    };
    if next_token(parts).is_some() {
        return Err(ManifestError::BadDependencyLine);
    }

    let Some(from_id) = manifest
        .find_service(from_name)
        .map(|service| service.stable_id)
    else {
        return Err(ManifestError::UnknownDependencyEndpoint);
    };
    let Some(from_uuid) = manifest
        .find_service(from_name)
        .map(|service| service.stable_uuid)
    else {
        return Err(ManifestError::UnknownDependencyEndpoint);
    };
    let Some(to_id) = manifest
        .find_service(to_name)
        .map(|service| service.stable_id)
    else {
        return Err(ManifestError::UnknownDependencyEndpoint);
    };
    let Some(to_uuid) = manifest
        .find_service(to_name)
        .map(|service| service.stable_uuid)
    else {
        return Err(ManifestError::UnknownDependencyEndpoint);
    };

    manifest.dependencies.push(DependencyDef {
        from_id,
        from_uuid,
        to_id,
        to_uuid,
    });
    Ok(())
}

fn trim_line(line: &[u8]) -> &[u8] {
    let mut start = 0usize;
    while start < line.len() && matches!(line[start], b' ' | b'\t' | b'\r') {
        start += 1;
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

    while end > start && matches!(line[end - 1], b' ' | b'\t' | b'\r') {
        end -= 1;
    }

    &line[start..end]
}

fn next_token<'a>(parts: &mut impl Iterator<Item = &'a [u8]>) -> Option<&'a [u8]> {
    parts.find(|token| !token.is_empty())
}
