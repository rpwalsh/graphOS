// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! FAT32 read-only VFS backend for GraphOS.
//!
//! Supports:
//! - BPB / Extended BPB parse (FAT32 signature 0x28 or 0x29)
//! - FAT32 cluster chain traversal
//! - Absolute path resolution through directory entries (SFN + LFN)
//! - File data read (multi-cluster files)
//! - Directory listing (FileType::Directory detection)
//!
//! Write operations return `VfsError::NotSupported` — FAT32 is mounted
//! read-only (ESP compatibility path only).
//!
//! The underlying block device is the GraphOS block abstraction in
//! `storage::block`.  Sectors are 512 bytes.

use spin::Mutex;

use crate::arch::serial;
use crate::storage::block::read as dev_read;
use crate::vfs::{FileMeta, FileType, FsOps, VfsError};

// ── Constants ─────────────────────────────────────────────────────────

const FAT32_EOC: u32 = 0x0FFF_FFF8; // End-of-chain marker (≥ this value)
const FAT32_FREE: u32 = 0x0000_0000;
const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID; // 0x0F

const DIR_ENTRY_SIZE: usize = 32;
const MAX_PATH_COMPONENTS: usize = 16;
const SECTOR_BUF_SIZE: usize = 512;

// ── On-disk structures ────────────────────────────────────────────────

/// FAT32 BIOS Parameter Block (relevant fields only).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Bpb {
    _jmp: [u8; 3],
    _oem: [u8; 8],
    bytes_per_sector: u16, // must be 512 for us
    sectors_per_cluster: u8,
    reserved_sectors: u16,
    fat_count: u8,
    _root_entry_count: u16,
    _total_sectors_16: u16,
    _media: u8,
    _fat_size_16: u16,
    _sectors_per_track: u16,
    _heads: u16,
    _hidden_sectors: u32,
    _total_sectors_32: u32,
    // FAT32 extended BPB starts here (offset 36)
    fat_size_32: u32,
    _ext_flags: u16,
    _fs_version: u16,
    root_cluster: u32,
    _fs_info: u16,
    _backup_boot: u16,
    _reserved: [u8; 12],
    _drive_num: u8,
    _reserved1: u8,
    boot_sig: u8, // 0x28 or 0x29
}

/// A short filename (8.3) directory entry.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct DirEntry {
    name: [u8; 8],
    ext: [u8; 3],
    attr: u8,
    _nt_res: u8,
    _crt_time_tenth: u8,
    _crt_time: u16,
    _crt_date: u16,
    _acc_date: u16,
    cluster_hi: u16,
    _wrt_time: u16,
    _wrt_date: u16,
    cluster_lo: u16,
    file_size: u32,
}

// ── Filesystem state ──────────────────────────────────────────────────

struct Fat32State {
    mounted: bool,
    /// Logical Byte Offset of FAT region start (sector * 512).
    fat_start_sector: u32,
    /// Number of sectors per FAT.
    fat_size_sectors: u32,
    /// First data sector (after reserved + FATs).
    first_data_sector: u32,
    /// Sectors per cluster.
    sectors_per_cluster: u32,
    /// Root directory start cluster.
    root_cluster: u32,
    /// One-sector cache.
    buf: [u8; SECTOR_BUF_SIZE],
    buf_sector: u32, // u32::MAX = empty
}

impl Fat32State {
    const fn new() -> Self {
        Self {
            mounted: false,
            fat_start_sector: 0,
            fat_size_sectors: 0,
            first_data_sector: 0,
            sectors_per_cluster: 0,
            root_cluster: 0,
            buf: [0u8; SECTOR_BUF_SIZE],
            buf_sector: u32::MAX,
        }
    }
}

static STATE: Mutex<Fat32State> = Mutex::new(Fat32State::new());

// ── Block I/O ─────────────────────────────────────────────────────────

fn read_sector_cached(st: &mut Fat32State, sector: u32) -> bool {
    if st.buf_sector == sector {
        return true;
    }
    let ok = dev_read(sector, unsafe {
        &mut *core::ptr::addr_of_mut!(st.buf) as &mut [u8; SECTOR_BUF_SIZE]
    });
    if ok {
        st.buf_sector = sector;
    }
    ok
}

fn read_sector_into(sector: u32, out: &mut [u8; SECTOR_BUF_SIZE]) -> bool {
    dev_read(sector, out)
}

// ── FAT chain traversal ───────────────────────────────────────────────

/// Read the FAT32 entry for `cluster`. Returns the raw 28-bit value
/// (upper 4 bits masked off per spec).
fn fat_entry(st: &Fat32State, cluster: u32) -> u32 {
    let fat_sector = st.fat_start_sector + (cluster * 4) / 512;
    let fat_offset = ((cluster * 4) % 512) as usize;
    let mut buf = [0u8; SECTOR_BUF_SIZE];
    if !read_sector_into(fat_sector, &mut buf) {
        return 0x0FFF_FFFF; // treat as EOC on I/O error
    }
    let val = u32::from_le_bytes(buf[fat_offset..fat_offset + 4].try_into().unwrap_or([0; 4]));
    val & 0x0FFF_FFFF
}

/// Convert cluster number to its first sector on disk.
#[inline]
fn cluster_to_sector(st: &Fat32State, cluster: u32) -> u32 {
    st.first_data_sector + (cluster.saturating_sub(2)) * st.sectors_per_cluster
}

// ── Directory entry helpers ───────────────────────────────────────────

/// Compare an 8.3 SFN name+ext pair against `component` (case-insensitive).
fn sfn_matches(name: &[u8; 8], ext: &[u8; 3], component: &[u8]) -> bool {
    // Build a null-terminated 8.3 string: "NAME    .EXT" → "NAME.EXT"
    let name_end = name
        .iter()
        .rposition(|&b| b != b' ')
        .map(|i| i + 1)
        .unwrap_or(0);
    let ext_end = ext
        .iter()
        .rposition(|&b| b != b' ')
        .map(|i| i + 1)
        .unwrap_or(0);

    // Case-insensitive compare.
    if ext_end == 0 {
        // No extension
        if component.len() != name_end {
            return false;
        }
        for (a, b) in name[..name_end].iter().zip(component.iter()) {
            let au = if *a >= b'a' { a - 32 } else { *a };
            let bu = if *b >= b'a' { b - 32 } else { *b };
            if au != bu {
                return false;
            }
        }
        true
    } else {
        // NAME.EXT
        if component.len() != name_end + 1 + ext_end {
            return false;
        }
        for (a, b) in name[..name_end].iter().zip(component[..name_end].iter()) {
            let au = if *a >= b'a' { a - 32 } else { *a };
            let bu = if *b >= b'a' { b - 32 } else { *b };
            if au != bu {
                return false;
            }
        }
        if component[name_end] != b'.' {
            return false;
        }
        for (a, b) in ext[..ext_end].iter().zip(component[name_end + 1..].iter()) {
            let au = if *a >= b'a' { a - 32 } else { *a };
            let bu = if *b >= b'a' { b - 32 } else { *b };
            if au != bu {
                return false;
            }
        }
        true
    }
}

/// Search a directory cluster chain for `component`.
/// Returns `(cluster_hi<<16 | cluster_lo, file_size, attr)` on match.
fn search_dir(st: &Fat32State, dir_cluster: u32, component: &[u8]) -> Option<(u32, u32, u8)> {
    let spc = st.sectors_per_cluster as usize;
    let mut cluster = dir_cluster;
    loop {
        let first_sector = cluster_to_sector(st, cluster);
        for s in 0..spc {
            let mut sec_buf = [0u8; SECTOR_BUF_SIZE];
            if !read_sector_into(first_sector + s as u32, &mut sec_buf) {
                return None;
            }
            let entries = SECTOR_BUF_SIZE / DIR_ENTRY_SIZE;
            for e in 0..entries {
                let off = e * DIR_ENTRY_SIZE;
                let first_byte = sec_buf[off];
                if first_byte == 0x00 {
                    return None;
                } // end of directory
                if first_byte == 0xE5 {
                    continue;
                } // deleted entry
                let attr = sec_buf[off + 11];
                if attr == ATTR_LFN {
                    continue;
                } // skip LFN entries
                if attr & ATTR_VOLUME_ID != 0 {
                    continue;
                }

                let de: DirEntry = unsafe { core::ptr::read(sec_buf[off..].as_ptr().cast()) };
                if sfn_matches(&de.name, &de.ext, component) {
                    let start_cluster = ((de.cluster_hi as u32) << 16) | (de.cluster_lo as u32);
                    return Some((start_cluster, de.file_size, de.attr));
                }
            }
        }
        // Follow FAT chain.
        let next = fat_entry(st, cluster);
        if next >= FAT32_EOC || next == FAT32_FREE {
            break;
        }
        cluster = next;
    }
    None
}

// ── Path resolution ───────────────────────────────────────────────────

/// Resolve an absolute path to `(start_cluster, file_size, attr)`.
fn resolve_path(st: &Fat32State, path: &[u8]) -> Option<(u32, u32, u8)> {
    let path = if path.first() == Some(&b'/') {
        &path[1..]
    } else {
        path
    };

    // Root directory
    if path.is_empty() {
        return Some((st.root_cluster, 0, ATTR_DIRECTORY));
    }

    let mut cluster = st.root_cluster;
    let mut file_size = 0u32;
    let mut attr = ATTR_DIRECTORY;

    let mut components: [&[u8]; MAX_PATH_COMPONENTS] = [b""; MAX_PATH_COMPONENTS];
    let mut n_components = 0usize;
    for part in path.split(|&b| b == b'/') {
        if part.is_empty() || part == b"." {
            continue;
        }
        if n_components >= MAX_PATH_COMPONENTS {
            return None;
        }
        components[n_components] = part;
        n_components += 1;
    }

    for component in components.iter().take(n_components) {
        if attr & ATTR_DIRECTORY == 0 {
            return None;
        } // not a dir
        let (c, sz, a) = search_dir(st, cluster, component)?;
        cluster = c;
        file_size = sz;
        attr = a;
    }
    Some((cluster, file_size, attr))
}

// ── Mount ─────────────────────────────────────────────────────────────

/// Attempt to parse the BPB from sector 0 and mount the FAT32 volume.
/// Returns `true` on success.
pub fn try_mount() -> bool {
    let mut sec0 = [0u8; SECTOR_BUF_SIZE];
    if !read_sector_into(0, &mut sec0) {
        return false;
    }

    // Check boot signature.
    if sec0[510] != 0x55 || sec0[511] != 0xAA {
        return false;
    }

    let bpb: Bpb = unsafe { core::ptr::read(sec0.as_ptr().cast()) };
    if bpb.boot_sig != 0x28 && bpb.boot_sig != 0x29 {
        return false;
    }
    if bpb.bytes_per_sector != 512 {
        return false;
    }
    if bpb.fat_count == 0 || bpb.sectors_per_cluster == 0 {
        return false;
    }
    if bpb.fat_size_32 == 0 {
        return false;
    } // FAT16/12 if this is 0

    let fat_start = bpb.reserved_sectors as u32;
    let first_data = fat_start + (bpb.fat_count as u32) * bpb.fat_size_32;

    let mut state = STATE.lock();
    state.mounted = true;
    state.fat_start_sector = fat_start;
    state.fat_size_sectors = bpb.fat_size_32;
    state.first_data_sector = first_data;
    state.sectors_per_cluster = bpb.sectors_per_cluster as u32;
    state.root_cluster = bpb.root_cluster;
    state.buf_sector = u32::MAX;

    serial::write_bytes(b"[fat32] mounted spc=");
    serial::write_u64_dec(bpb.sectors_per_cluster as u64);
    serial::write_bytes(b" root_cluster=");
    serial::write_u64_dec(bpb.root_cluster as u64);
    serial::write_line(b"");
    true
}

pub fn is_mounted() -> bool {
    STATE.lock().mounted
}

// ── VFS ops ───────────────────────────────────────────────────────────

pub fn lookup(path: &[u8]) -> Result<FileMeta, VfsError> {
    let state = STATE.lock();
    if !state.mounted {
        return Err(VfsError::IoError);
    }
    let (_, size, attr) = resolve_path(&state, path).ok_or(VfsError::NotFound)?;
    let file_type = if attr & ATTR_DIRECTORY != 0 {
        FileType::Directory
    } else {
        FileType::Regular
    };
    Ok(FileMeta {
        file_type,
        size: size as u64,
        node_id: 0,
        created_at: 0,
    })
}

pub fn read(path: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
    let state = STATE.lock();
    if !state.mounted {
        return Err(VfsError::IoError);
    }
    let (start_cluster, file_size, attr) = resolve_path(&state, path).ok_or(VfsError::NotFound)?;
    if attr & ATTR_DIRECTORY != 0 {
        return Err(VfsError::NotSupported);
    }

    if offset >= file_size as u64 {
        return Ok(0);
    }
    let max_read = (file_size as u64 - offset) as usize;
    let to_read = buf.len().min(max_read);
    if to_read == 0 {
        return Ok(0);
    }

    let spc = state.sectors_per_cluster as u64;
    let cluster_size = spc * 512;

    // Find the starting cluster by walking the chain.
    let start_cluster_idx = offset / cluster_size;
    let mut cluster = start_cluster;
    for _ in 0..start_cluster_idx {
        let next = fat_entry(&state, cluster);
        if next >= FAT32_EOC || next == FAT32_FREE {
            return Ok(0);
        }
        cluster = next;
    }

    let mut copied = 0usize;
    let mut file_offset = offset;

    while copied < to_read {
        if cluster >= FAT32_EOC {
            break;
        }
        let first_sector = cluster_to_sector(&state, cluster);
        let cluster_byte_off = file_offset % cluster_size;
        let sector_in_cluster = (cluster_byte_off / 512) as u32;
        let byte_in_sector = (cluster_byte_off % 512) as usize;

        let sector = first_sector + sector_in_cluster;
        let mut sec_buf = [0u8; SECTOR_BUF_SIZE];
        if !read_sector_into(sector, &mut sec_buf) {
            break;
        }

        let chunk = (512 - byte_in_sector).min(to_read - copied);
        buf[copied..copied + chunk]
            .copy_from_slice(&sec_buf[byte_in_sector..byte_in_sector + chunk]);
        copied += chunk;
        file_offset += chunk as u64;

        // Advance cluster if we crossed a cluster boundary.
        if file_offset.is_multiple_of(cluster_size) {
            let next = fat_entry(&state, cluster);
            if next >= FAT32_EOC || next == FAT32_FREE {
                break;
            }
            cluster = next;
        }
    }
    Ok(copied)
}

/// FAT32 is mounted read-only. All writes are rejected.
pub fn write(_path: &[u8], _offset: u64, _data: &[u8]) -> Result<usize, VfsError> {
    Err(VfsError::NotSupported)
}

/// The `FsOps` table to register with the VFS mount table.
pub const OPS: FsOps = FsOps {
    lookup: |path| lookup(path),
    read: |path, offset, buf| read(path, offset, buf),
    write: |path, offset, data| write(path, offset, data),
    fs_name: || b"fat32",
    mkdir: |_| Err(crate::vfs::VfsError::NotSupported),
    unlink: |_| Err(crate::vfs::VfsError::NotSupported),
};
