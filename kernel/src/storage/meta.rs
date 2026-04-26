// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use spin::Mutex;

use super::block::{self, BLOCK_COUNT, BLOCK_SIZE};

const SUPER_MAGIC: &[u8; 8] = b"GSTOREV1";
const RECORD_MAGIC: &[u8; 8] = b"GMETA001";
const SUPER_BLOCK: u32 = 0;
const RECORD_START_BLOCK: u32 = 1;
const MAX_KEY_LEN: usize = 63;
const MAX_VALUE_LEN: usize = BLOCK_SIZE - 24 - MAX_KEY_LEN;

#[derive(Clone, Copy)]
struct EntryMeta {
    block: u32,
    seq: u64,
    value_len: usize,
}

#[derive(Default)]
struct MetaState {
    initialized: bool,
    next_seq: u64,
}

impl MetaState {
    const fn new() -> Self {
        Self {
            initialized: false,
            next_seq: 0,
        }
    }
}

static META: Mutex<MetaState> = Mutex::new(MetaState::new());

pub fn init() {
    let mut state = META.lock();
    if state.initialized {
        return;
    }

    block::init();

    let mut super_block = [0u8; BLOCK_SIZE];
    if !block::read(SUPER_BLOCK, &mut super_block) || &super_block[..8] != SUPER_MAGIC {
        format_store(&mut state);
        return;
    }

    state.next_seq = read_u64(&super_block, 8).unwrap_or(1).max(1);
    state.initialized = true;
}

pub fn put(key: &[u8], value: &[u8]) -> bool {
    if !validate_key(key) || value.len() > MAX_VALUE_LEN {
        return false;
    }
    init();

    let mut state = META.lock();
    let target_block = if let Some(existing) = find_entry(key) {
        existing.block
    } else {
        match first_free_block() {
            Some(block) => block,
            None => return false,
        }
    };

    let seq = state.next_seq.max(1);
    state.next_seq = seq.saturating_add(1);
    write_superblock(state.next_seq);
    drop(state);

    let mut block_bytes = [0u8; BLOCK_SIZE];
    block_bytes[..8].copy_from_slice(RECORD_MAGIC);
    block_bytes[8..16].copy_from_slice(&seq.to_le_bytes());
    block_bytes[16..18].copy_from_slice(&(key.len() as u16).to_le_bytes());
    block_bytes[18..20].copy_from_slice(&(value.len() as u16).to_le_bytes());
    block_bytes[20] = 1;
    block_bytes[24..24 + key.len()].copy_from_slice(key);
    let value_start = 24 + MAX_KEY_LEN;
    block_bytes[value_start..value_start + value.len()].copy_from_slice(value);
    block::write(target_block, &block_bytes)
}

pub fn get(key: &[u8], buf: &mut [u8]) -> Option<usize> {
    if !validate_key(key) {
        return None;
    }
    init();

    let entry = find_entry(key)?;
    let block_bytes = read_block(entry.block)?;
    let value_start = 24 + MAX_KEY_LEN;
    let to_copy = entry.value_len.min(buf.len());
    buf[..to_copy].copy_from_slice(&block_bytes[value_start..value_start + to_copy]);
    Some(to_copy)
}

pub fn size(key: &[u8]) -> Option<usize> {
    init();
    find_entry(key).map(|entry| entry.value_len)
}

pub fn contains(key: &[u8]) -> bool {
    init();
    find_entry(key).is_some()
}

pub fn entry_count() -> usize {
    init();
    let mut count = 0usize;
    let mut block_index = RECORD_START_BLOCK;
    while block_index < BLOCK_COUNT as u32 {
        if let Some(block_bytes) = read_block(block_index)
            && is_active_record(&block_bytes)
        {
            count += 1;
        }
        block_index += 1;
    }
    count
}

fn format_store(state: &mut MetaState) {
    let zero = [0u8; BLOCK_SIZE];
    let mut block_index = 0u32;
    while block_index < BLOCK_COUNT as u32 {
        let _ = block::write(block_index, &zero);
        block_index += 1;
    }

    state.next_seq = 1;
    state.initialized = true;
    write_superblock(state.next_seq);
}

fn write_superblock(next_seq: u64) {
    let mut super_block = [0u8; BLOCK_SIZE];
    super_block[..8].copy_from_slice(SUPER_MAGIC);
    super_block[8..16].copy_from_slice(&next_seq.to_le_bytes());
    let _ = block::write(SUPER_BLOCK, &super_block);
}

fn find_entry(key: &[u8]) -> Option<EntryMeta> {
    let mut best: Option<EntryMeta> = None;
    let mut block_index = RECORD_START_BLOCK;
    while block_index < BLOCK_COUNT as u32 {
        let block_bytes = read_block(block_index)?;
        if !is_active_record(&block_bytes) {
            block_index += 1;
            continue;
        }

        let key_len = read_u16(&block_bytes, 16)? as usize;
        let value_len = read_u16(&block_bytes, 18)? as usize;
        if key_len != key.len() || value_len > MAX_VALUE_LEN {
            block_index += 1;
            continue;
        }
        if &block_bytes[24..24 + key_len] != key {
            block_index += 1;
            continue;
        }

        let seq = read_u64(&block_bytes, 8)?;
        match best {
            Some(current) if current.seq >= seq => {}
            _ => {
                best = Some(EntryMeta {
                    block: block_index,
                    seq,
                    value_len,
                });
            }
        }
        block_index += 1;
    }
    best
}

fn first_free_block() -> Option<u32> {
    let mut block_index = RECORD_START_BLOCK;
    while block_index < BLOCK_COUNT as u32 {
        let block_bytes = read_block(block_index)?;
        if !is_active_record(&block_bytes) {
            return Some(block_index);
        }
        block_index += 1;
    }
    None
}

fn is_active_record(block_bytes: &[u8; BLOCK_SIZE]) -> bool {
    &block_bytes[..8] == RECORD_MAGIC && block_bytes[20] == 1
}

fn read_block(block_index: u32) -> Option<[u8; BLOCK_SIZE]> {
    let mut buf = [0u8; BLOCK_SIZE];
    if !block::read(block_index, &mut buf) {
        return None;
    }
    Some(buf)
}

fn validate_key(key: &[u8]) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_LEN && key[0] == b'/'
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let raw: [u8; 2] = bytes.get(offset..offset + 2)?.try_into().ok()?;
    Some(u16::from_le_bytes(raw))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let raw: [u8; 8] = bytes.get(offset..offset + 8)?.try_into().ok()?;
    Some(u64::from_le_bytes(raw))
}
