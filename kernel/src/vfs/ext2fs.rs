// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! ext2 filesystem — writable block device driver for GraphOS VFS.
//!
//! Reads and writes an ext2 (or compatible ext3/ext4 with no journal replay needed)
//! filesystem from the block device provided by `storage::block`.
//!
//! Supports:
//! - Superblock parse + magic check
//! - Block group descriptor table
//! - Inode read/write (rev 0 and rev 1 inode sizes)
//! - Absolute path resolution via directory entry traversal
//! - File data read/write via direct blocks (i_block[0..11])
//! - Indirect block support (i_block[12])
//! - Block allocation via block bitmap
//! - File growth (direct blocks only)

use core::mem::size_of;

use spin::Mutex;

use crate::arch::serial;
use crate::storage::block::{BLOCK_SIZE as DEV_BLOCK_SIZE, read as dev_read, write as dev_write};
use crate::vfs::{FileMeta, FileType, FsOps, VfsError};

// ── Constants ─────────────────────────────────────────────────────────

const EXT2_MAGIC: u16 = 0xEF53;
const EXT2_ROOT_INODE: u32 = 2;
const EXT2_FT_REG: u8 = 1;
const EXT2_FT_DIR: u8 = 2;

const IMODE_REG: u16 = 0x8000;
const IMODE_DIR: u16 = 0x4000;

const N_DIRECT: usize = 12;
const INDIRECT_IDX: usize = 12;
const BLOCK_BUF_SIZE: usize = 4096; // max supported ext2 block size

// ── On-disk structures ────────────────────────────────────────────────

/// Ext2 superblock (bytes 1024..2047 on disk, 264 bytes we care about).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Superblock {
    s_inodes_count: u32,
    s_blocks_count: u32,
    _r1: [u32; 4],
    s_log_block_size: u32,
    _r2: u32,
    s_blocks_per_group: u32,
    _r3: u32,
    s_inodes_per_group: u32,
    _r4: [u32; 3],
    s_magic: u16,
    _r5: [u16; 2],
    _r6: [u32; 13],
    s_rev_level: u32,
    _r7: [u16; 2],
    s_inode_size: u16,
}

/// Ext2 block group descriptor (32 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GroupDesc {
    bg_block_bitmap: u32,
    bg_inode_bitmap: u32,
    bg_inode_table: u32,
    _rest: [u8; 20],
}

/// Ext2 inode (128 bytes for rev-0; may be larger in rev-1+).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Inode {
    i_mode: u16,
    i_uid: u16,
    i_size: u32,
    _times: [u32; 3],
    _gid: u16,
    i_links_count: u16,
    i_blocks: u32,
    _flags: u32,
    _osd1: u32,
    i_block: [u32; 15],
    _rest: [u8; 28],
}

// ── Filesystem state ──────────────────────────────────────────────────

struct Ext2State {
    mounted: bool,
    block_size: u32,
    inodes_per_group: u32,
    inode_size: u32,
    blocks_per_group: u32,
    /// Total block count from superblock.
    blocks_count: u32,
    /// Number of block groups.
    groups_count: u32,
    /// Byte offset of the first block group descriptor on device.
    bgdt_byte_offset: u64,
    /// Cached single block buffer.
    buf: [u8; BLOCK_BUF_SIZE],
    buf_block: u64, // which block is currently cached (u64::MAX = none)
}

impl Ext2State {
    const fn new() -> Self {
        Self {
            mounted: false,
            block_size: 0,
            inodes_per_group: 0,
            inode_size: 128,
            blocks_per_group: 0,
            blocks_count: 0,
            groups_count: 0,
            bgdt_byte_offset: 0,
            buf: [0u8; BLOCK_BUF_SIZE],
            buf_block: u64::MAX,
        }
    }
}

static STATE: Mutex<Ext2State> = Mutex::new(Ext2State::new());

// ── Block I/O helpers ─────────────────────────────────────────────────

/// Read an ext2 block (potentially larger than 512-byte dev sectors) into
/// `out`. Returns true on success.
fn read_ext2_block(state: &Ext2State, ext2_block: u32, out: &mut [u8]) -> bool {
    let bs = state.block_size as usize;
    if out.len() < bs {
        return false;
    }
    let dev_sectors_per_block = bs / DEV_BLOCK_SIZE;
    let start_sector = (ext2_block as usize) * dev_sectors_per_block;
    for i in 0..dev_sectors_per_block {
        let mut sec = [0u8; DEV_BLOCK_SIZE];
        if !dev_read((start_sector + i) as u32, &mut sec) {
            return false;
        }
        out[i * DEV_BLOCK_SIZE..(i + 1) * DEV_BLOCK_SIZE].copy_from_slice(&sec);
    }
    true
}

/// Write a raw byte range to device (read-modify-write per sector).
fn write_raw(byte_offset: u64, data: &[u8]) -> bool {
    let mut pos = 0usize;
    while pos < data.len() {
        let sector = ((byte_offset as usize + pos) / DEV_BLOCK_SIZE) as u32;
        let off_in_sector = (byte_offset as usize + pos) % DEV_BLOCK_SIZE;
        let mut sec = [0u8; DEV_BLOCK_SIZE];
        if !dev_read(sector, &mut sec) {
            return false;
        }
        let copy = (DEV_BLOCK_SIZE - off_in_sector).min(data.len() - pos);
        sec[off_in_sector..off_in_sector + copy].copy_from_slice(&data[pos..pos + copy]);
        if !dev_write(sector, &sec) {
            return false;
        }
        pos += copy;
    }
    true
}

/// Read a raw byte range from device (using 512-byte sectors).
fn read_raw(byte_offset: u64, out: &mut [u8]) -> bool {
    let mut pos = 0usize;
    while pos < out.len() {
        let sector = ((byte_offset as usize + pos) / DEV_BLOCK_SIZE) as u32;
        let off_in_sector = (byte_offset as usize + pos) % DEV_BLOCK_SIZE;
        let mut sec = [0u8; DEV_BLOCK_SIZE];
        if !dev_read(sector, &mut sec) {
            return false;
        }
        let copy = (DEV_BLOCK_SIZE - off_in_sector).min(out.len() - pos);
        out[pos..pos + copy].copy_from_slice(&sec[off_in_sector..off_in_sector + copy]);
        pos += copy;
    }
    true
}

// ── Mount ─────────────────────────────────────────────────────────────

/// Try to mount an ext2 filesystem from the block device.
/// Returns true if the magic number matches and state is initialised.
pub fn try_mount() -> bool {
    // Superblock is at byte offset 1024.
    let mut sb_bytes = [0u8; size_of::<Superblock>()];
    if !read_raw(1024, &mut sb_bytes) {
        return false;
    }
    let sb: Superblock = unsafe { core::ptr::read(sb_bytes.as_ptr().cast()) };

    if sb.s_magic != EXT2_MAGIC {
        return false;
    }

    let block_size = 1024u32 << sb.s_log_block_size;
    if block_size > BLOCK_BUF_SIZE as u32 {
        return false;
    }

    let inode_size = if sb.s_rev_level >= 1 && sb.s_inode_size >= 128 {
        sb.s_inode_size as u32
    } else {
        128
    };

    // BGDT starts at block 2 when block_size == 1024, else block 1 (i.e., byte 1024*2 or 1024).
    let bgdt_block = if block_size == 1024 { 2u32 } else { 1u32 };
    let bgdt_byte_offset = (bgdt_block as u64) * (block_size as u64);

    let mut state = STATE.lock();
    let groups_count = sb.s_blocks_count.div_ceil(sb.s_blocks_per_group);
    state.mounted = true;
    state.block_size = block_size;
    state.inodes_per_group = sb.s_inodes_per_group;
    state.inode_size = inode_size;
    state.blocks_per_group = sb.s_blocks_per_group;
    state.blocks_count = sb.s_blocks_count;
    state.groups_count = groups_count;
    state.bgdt_byte_offset = bgdt_byte_offset;
    state.buf_block = u64::MAX;

    serial::write_bytes(b"[ext2] mounted block_size=");
    serial::write_u64_dec(block_size as u64);
    serial::write_bytes(b" inode_size=");
    serial::write_u64_dec(inode_size as u64);
    serial::write_line(b"");
    true
}

pub fn is_mounted() -> bool {
    STATE.lock().mounted
}

// ── Inode helpers ─────────────────────────────────────────────────────

fn read_group_desc(bgdt_off: u64, group: u32) -> Option<GroupDesc> {
    let off = bgdt_off + (group as u64) * (size_of::<GroupDesc>() as u64);
    let mut bytes = [0u8; size_of::<GroupDesc>()];
    if !read_raw(off, &mut bytes) {
        return None;
    }
    Some(unsafe { core::ptr::read(bytes.as_ptr().cast()) })
}

/// Allocate one free block from the block bitmaps, returning its block number.
/// Updates the bitmap on disk. Returns None if no free block is found.
fn alloc_block(state: &Ext2State) -> Option<u32> {
    for g in 0..state.groups_count {
        let gd = read_group_desc(state.bgdt_byte_offset, g)?;
        let bitmap_block = gd.bg_block_bitmap;
        let bs = state.block_size as usize;
        let mut bitmap = [0u8; BLOCK_BUF_SIZE];
        if !read_ext2_block(state, bitmap_block, &mut bitmap[..bs]) {
            continue;
        }

        // Scan for a free bit (0 = free, 1 = used).
        for byte_idx in 0..bs {
            if bitmap[byte_idx] == 0xFF {
                continue;
            }
            for bit in 0..8u8 {
                if bitmap[byte_idx] & (1 << bit) == 0 {
                    // Mark as used.
                    bitmap[byte_idx] |= 1 << bit;
                    // Write updated bitmap back.
                    if !write_ext2_block(state, bitmap_block, &bitmap[..bs]) {
                        return None;
                    }
                    let block_num =
                        g * state.blocks_per_group + (byte_idx as u32) * 8 + (bit as u32);
                    return Some(block_num);
                }
            }
        }
    }
    None
}

/// Write an inode back to disk.
fn write_inode(state: &Ext2State, inode_num: u32, inode: &Inode) -> bool {
    if inode_num == 0 {
        return false;
    }
    let idx = inode_num - 1;
    let group = idx / state.inodes_per_group;
    let local = idx % state.inodes_per_group;
    let gd = match read_group_desc(state.bgdt_byte_offset, group) {
        Some(g) => g,
        None => return false,
    };
    let inode_table_byte = (gd.bg_inode_table as u64) * (state.block_size as u64);
    let inode_off = inode_table_byte + (local as u64) * (state.inode_size as u64);
    let bytes = unsafe {
        core::slice::from_raw_parts(inode as *const Inode as *const u8, size_of::<Inode>())
    };
    write_raw(inode_off, bytes)
}

fn read_inode(state: &Ext2State, inode_num: u32) -> Option<Inode> {
    if inode_num == 0 {
        return None;
    }
    let idx = inode_num - 1; // inodes are 1-indexed
    let group = idx / state.inodes_per_group;
    let local = idx % state.inodes_per_group;

    let gd = read_group_desc(state.bgdt_byte_offset, group)?;
    let inode_table_byte = (gd.bg_inode_table as u64) * (state.block_size as u64);
    let inode_off = inode_table_byte + (local as u64) * (state.inode_size as u64);

    let mut bytes = [0u8; size_of::<Inode>()];
    if !read_raw(inode_off, &mut bytes) {
        return None;
    }
    Some(unsafe { core::ptr::read(bytes.as_ptr().cast()) })
}

// ── Directory lookup ──────────────────────────────────────────────────

/// Walk one ext2 data block looking for a directory entry named `name`.
/// Returns the child inode number on match.
fn search_dir_block(state: &Ext2State, block: u32, name: &[u8]) -> Option<u32> {
    if block == 0 {
        return None;
    }
    let bs = state.block_size as usize;
    let mut blk_buf = [0u8; BLOCK_BUF_SIZE];
    if !read_ext2_block(state, block, &mut blk_buf[..bs]) {
        return None;
    }

    let mut off = 0usize;
    while off + 8 <= bs {
        let inode = u32::from_le_bytes(blk_buf[off..off + 4].try_into().unwrap());
        let rec_len = u16::from_le_bytes(blk_buf[off + 4..off + 6].try_into().unwrap()) as usize;
        let name_len = blk_buf[off + 6] as usize;
        if rec_len == 0 {
            break;
        }
        if inode != 0 && name_len == name.len() {
            let entry_name = &blk_buf[off + 8..off + 8 + name_len];
            if entry_name == name {
                return Some(inode);
            }
        }
        off += rec_len;
    }
    None
}

/// Resolve an absolute path (e.g., b"/home/user/file.txt") relative to the
/// root inode. Returns the inode number on success.
fn resolve_path(state: &Ext2State, path: &[u8]) -> Option<u32> {
    // Strip leading '/'.
    let path = if path.first() == Some(&b'/') {
        &path[1..]
    } else {
        path
    };

    let mut current_inode = EXT2_ROOT_INODE;
    if path.is_empty() {
        return Some(current_inode);
    }

    for component in path.split(|&b| b == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }
        let inode = read_inode(state, current_inode)?;
        if inode.i_mode & IMODE_DIR == 0 {
            return None;
        } // not a directory
        // Search direct blocks.
        let mut found = None;
        for i in 0..N_DIRECT {
            let blk = inode.i_block[i];
            if blk == 0 {
                break;
            }
            if let Some(child) = search_dir_block(state, blk, component) {
                found = Some(child);
                break;
            }
        }
        // Search indirect block if needed.
        if found.is_none() {
            let ind_blk = inode.i_block[INDIRECT_IDX];
            if ind_blk != 0 {
                let bs = state.block_size as usize;
                let mut ind_buf = [0u8; BLOCK_BUF_SIZE];
                if read_ext2_block(state, ind_blk, &mut ind_buf[..bs]) {
                    let ptrs = bs / 4;
                    for p in 0..ptrs {
                        let blk = u32::from_le_bytes(ind_buf[p * 4..p * 4 + 4].try_into().unwrap());
                        if blk == 0 {
                            break;
                        }
                        if let Some(child) = search_dir_block(state, blk, component) {
                            found = Some(child);
                            break;
                        }
                    }
                }
            }
        }
        current_inode = found?;
    }
    Some(current_inode)
}

// ── VFS ops ───────────────────────────────────────────────────────────

pub fn lookup(path: &[u8]) -> Result<FileMeta, VfsError> {
    let state = STATE.lock();
    if !state.mounted {
        return Err(VfsError::IoError);
    }
    let ino_num = resolve_path(&state, path).ok_or(VfsError::NotFound)?;
    let inode = read_inode(&state, ino_num).ok_or(VfsError::IoError)?;
    let file_type = if inode.i_mode & IMODE_DIR != 0 {
        FileType::Directory
    } else {
        FileType::Regular
    };
    Ok(FileMeta {
        file_type,
        size: inode.i_size as u64,
        node_id: ino_num as u64,
        created_at: 0,
    })
}

fn resolve_data_block(state: &Ext2State, inode: &Inode, block_idx: usize) -> Option<u32> {
    if block_idx < N_DIRECT {
        let block = inode.i_block[block_idx];
        return if block == 0 { None } else { Some(block) };
    }

    let ind_blk = inode.i_block[INDIRECT_IDX];
    if ind_blk == 0 {
        return None;
    }
    let bs = state.block_size as usize;
    let ptrs_per_block = bs / 4;
    let indirect_idx = block_idx - N_DIRECT;
    if indirect_idx >= ptrs_per_block {
        return None;
    }

    let mut ind_buf = [0u8; BLOCK_BUF_SIZE];
    if !read_ext2_block(state, ind_blk, &mut ind_buf[..bs]) {
        return None;
    }
    let block = u32::from_le_bytes(
        ind_buf[indirect_idx * 4..indirect_idx * 4 + 4]
            .try_into()
            .unwrap(),
    );
    if block == 0 { None } else { Some(block) }
}

fn write_ext2_block(state: &Ext2State, ext2_block: u32, data: &[u8]) -> bool {
    let bs = state.block_size as usize;
    if data.len() < bs {
        return false;
    }
    let dev_sectors_per_block = bs / DEV_BLOCK_SIZE;
    let start_sector = (ext2_block as usize) * dev_sectors_per_block;
    for i in 0..dev_sectors_per_block {
        let start = i * DEV_BLOCK_SIZE;
        let mut sector = [0u8; DEV_BLOCK_SIZE];
        sector.copy_from_slice(&data[start..start + DEV_BLOCK_SIZE]);
        if !dev_write((start_sector + i) as u32, &sector) {
            return false;
        }
    }
    true
}

pub fn read(path: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
    let state = STATE.lock();
    if !state.mounted {
        return Err(VfsError::IoError);
    }
    let ino_num = resolve_path(&state, path).ok_or(VfsError::NotFound)?;
    let inode = read_inode(&state, ino_num).ok_or(VfsError::IoError)?;
    if inode.i_mode & IMODE_REG == 0 {
        return Err(VfsError::NotSupported);
    }

    let file_size = inode.i_size as u64;
    if offset >= file_size {
        return Ok(0);
    }

    let bs = state.block_size as usize;
    let max_read = (file_size - offset) as usize;
    let to_read = buf.len().min(max_read);
    let mut copied = 0usize;

    while copied < to_read {
        let file_off = offset + copied as u64;
        let block_idx = (file_off / bs as u64) as usize;
        let off_in_block = (file_off % bs as u64) as usize;

        let Some(phys_block) = resolve_data_block(&state, &inode, block_idx) else {
            break;
        };

        let mut blk_buf = [0u8; BLOCK_BUF_SIZE];
        if !read_ext2_block(&state, phys_block, &mut blk_buf[..bs]) {
            break;
        }

        let chunk = (bs - off_in_block).min(to_read - copied);
        buf[copied..copied + chunk].copy_from_slice(&blk_buf[off_in_block..off_in_block + chunk]);
        copied += chunk;
    }
    Ok(copied)
}

pub fn write(path: &[u8], offset: u64, data: &[u8]) -> Result<usize, VfsError> {
    if data.is_empty() {
        return Ok(0);
    }

    let state = STATE.lock();
    if !state.mounted {
        return Err(VfsError::IoError);
    }
    let ino_num = resolve_path(&state, path).ok_or(VfsError::NotFound)?;
    let mut inode = read_inode(&state, ino_num).ok_or(VfsError::IoError)?;
    if inode.i_mode & IMODE_REG == 0 {
        return Err(VfsError::NotSupported);
    }

    let bs = state.block_size as usize;
    let end_offset = offset
        .checked_add(data.len() as u64)
        .ok_or(VfsError::IoError)?;

    // Grow file by allocating direct blocks if needed (up to 12 direct blocks).
    if end_offset > inode.i_size as u64 {
        let needed_blocks = end_offset.div_ceil(bs as u64) as usize;
        if needed_blocks > N_DIRECT {
            // Beyond direct block range — reject rather than corrupt.
            return Err(VfsError::NotSupported);
        }
        for blk_idx in 0..needed_blocks {
            if inode.i_block[blk_idx] == 0 {
                let new_block = alloc_block(&state).ok_or(VfsError::IoError)?;
                inode.i_block[blk_idx] = new_block;
                // Zero the new block.
                let zero = [0u8; BLOCK_BUF_SIZE];
                write_ext2_block(&state, new_block, &zero[..bs]);
                // Update i_blocks (in 512-byte units).
                let sectors_per_block = (bs / DEV_BLOCK_SIZE) as u32;
                inode.i_blocks = inode.i_blocks.saturating_add(sectors_per_block);
            }
        }
        inode.i_size = end_offset as u32;
        // Persist the updated inode.
        if !write_inode(&state, ino_num, &inode) {
            return Err(VfsError::IoError);
        }
    }

    let file_size = inode.i_size as u64;
    let to_write = (file_size.saturating_sub(offset) as usize).min(data.len());
    let mut written = 0usize;

    while written < to_write {
        let file_off = offset + written as u64;
        let block_idx = (file_off / bs as u64) as usize;
        let off_in_block = (file_off % bs as u64) as usize;
        let Some(phys_block) = resolve_data_block(&state, &inode, block_idx) else {
            break;
        };

        let mut blk_buf = [0u8; BLOCK_BUF_SIZE];
        if !read_ext2_block(&state, phys_block, &mut blk_buf[..bs]) {
            return Err(VfsError::IoError);
        }

        let chunk = (bs - off_in_block).min(to_write - written);
        blk_buf[off_in_block..off_in_block + chunk]
            .copy_from_slice(&data[written..written + chunk]);
        if !write_ext2_block(&state, phys_block, &blk_buf[..bs]) {
            return Err(VfsError::IoError);
        }
        written += chunk;
    }

    Ok(written)
}

/// The `FsOps` table to register with the VFS mount table.
pub const OPS: FsOps = FsOps {
    lookup: |path| lookup(path),
    read: |path, offset, buf| read(path, offset, buf),
    write: |path, offset, data| write(path, offset, data),
    fs_name: || b"ext2",
    mkdir: |_| Err(crate::vfs::VfsError::NotSupported),
    unlink: |_| Err(crate::vfs::VfsError::NotSupported),
};
