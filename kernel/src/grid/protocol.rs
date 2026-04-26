// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Grid wire protocol — message types sent over UDP/IPv6 between nodes.
//!
//! All messages are sent to UDP port 6749 (hex "GO"), either unicast or to
//! the `ff02::6749` link-local multicast group.
//!
//! Messages are fixed-size binary structs: no heap, no alloc, no serde.
//! The first byte of every message is the `GridMsgKind` discriminant.

/// UDP port used by all grid protocol messages.
pub const GRID_UDP_PORT: u16 = 6749;

/// Message kind discriminant (first byte of every grid message).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum GridMsgKind {
    /// Node announcement beacon — sent periodically to ff02::6749.
    Hello = 0x01,
    /// Goodbye — voluntary departure from cluster.
    Bye = 0x02,
    /// Remote task spawn request.
    TaskSpawn = 0x10,
    /// Remote task spawn reply (accepted / denied).
    TaskSpawnReply = 0x11,
    /// Remote task completion notification.
    TaskDone = 0x12,
    /// Remote IPC channel message forward.
    IpcForward = 0x20,
    /// Remote IPC channel reply.
    IpcReply = 0x21,
    /// Remote memory allocation request.
    MemAlloc = 0x30,
    /// Remote memory allocation reply.
    MemAllocReply = 0x31,
    /// Remote memory free.
    MemFree = 0x32,
    /// Remote VFS read request.
    VfsRead = 0x40,
    /// Remote VFS read reply.
    VfsReadReply = 0x41,
    /// Remote VFS write request.
    VfsWrite = 0x42,
    /// Remote VFS write reply.
    VfsWriteReply = 0x43,
    /// Resource usage broadcast — updates cpu/ram/gpu stats in peer table.
    ResourceUpdate = 0x50,
    /// Unknown / reserved.
    Unknown = 0xff,
}

impl GridMsgKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0x01 => Self::Hello,
            0x02 => Self::Bye,
            0x10 => Self::TaskSpawn,
            0x11 => Self::TaskSpawnReply,
            0x12 => Self::TaskDone,
            0x20 => Self::IpcForward,
            0x21 => Self::IpcReply,
            0x30 => Self::MemAlloc,
            0x31 => Self::MemAllocReply,
            0x32 => Self::MemFree,
            0x40 => Self::VfsRead,
            0x41 => Self::VfsReadReply,
            0x42 => Self::VfsWrite,
            0x43 => Self::VfsWriteReply,
            0x50 => Self::ResourceUpdate,
            _ => Self::Unknown,
        }
    }
}

// ── Message structs (all repr(C), packed for on-wire encoding) ───────────────

/// `GridHello` — 48-byte node announcement.
///
/// Sent periodically to the grid multicast group and in response to
/// incoming Hello messages from unknown peers.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GridHello {
    /// Must be `GridMsgKind::Hello as u8`.
    pub kind: u8,
    /// Protocol version, currently 1.
    pub version: u8,
    /// Capability bitmask (see `super::cap`).
    pub capabilities: u8,
    /// Number of logical CPU cores available.
    pub cpu_cores: u8,
    /// Free RAM in MiB (saturating u8).
    pub ram_free_mib: u8,
    /// Free GPU VRAM in MiB (saturating u8; 0 if no GPU).
    pub gpu_vram_mib: u8,
    /// Free storage in GiB (saturating u8).
    pub storage_free_gib: u8,
    /// Reserved, must be zero.
    pub _pad: u8,
    /// 128-bit node UUID.
    pub node_uuid: [u8; 16],
    /// Link-local IPv6 address of this node (for unicast replies).
    pub link_local_ip: [u8; 16],
    /// MAC address (6 bytes + 2 pad).
    pub mac: [u8; 6],
    pub _mac_pad: [u8; 2],
}

impl GridHello {
    pub const SIZE: usize = core::mem::size_of::<GridHello>();

    pub fn encode(&self, out: &mut [u8]) -> usize {
        if out.len() < Self::SIZE {
            return 0;
        }
        // Safety: repr(C, packed) — byte-level copy is correct.
        let bytes: &[u8; Self::SIZE] = unsafe { core::mem::transmute(self) };
        out[..Self::SIZE].copy_from_slice(bytes);
        Self::SIZE
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SIZE || data[0] != GridMsgKind::Hello as u8 {
            return None;
        }
        let mut out = GridHello {
            kind: 0,
            version: 0,
            capabilities: 0,
            cpu_cores: 0,
            ram_free_mib: 0,
            gpu_vram_mib: 0,
            storage_free_gib: 0,
            _pad: 0,
            node_uuid: [0u8; 16],
            link_local_ip: [0u8; 16],
            mac: [0u8; 6],
            _mac_pad: [0u8; 2],
        };
        let dst: &mut [u8; Self::SIZE] = unsafe { core::mem::transmute(&mut out) };
        dst.copy_from_slice(&data[..Self::SIZE]);
        Some(out)
    }
}

/// `GridTaskSpawn` — request to execute a task on a remote node.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GridTaskSpawn {
    pub kind: u8,
    pub _pad: [u8; 3],
    /// Caller-side correlation UUID (used in reply matching).
    pub correlation_uuid: [u8; 16],
    /// UUID of the task binary to execute (must be installed on remote node).
    pub task_binary_uuid: [u8; 16],
    /// Entry-point argument (64-bit).
    pub arg: u64,
    /// Required capability bitmask (must be subset of remote node's offerings).
    pub required_caps: u8,
    pub _pad2: [u8; 7],
}

impl GridTaskSpawn {
    pub const SIZE: usize = core::mem::size_of::<GridTaskSpawn>();
}

/// `GridTaskSpawnReply` — reply from remote node to `GridTaskSpawn`.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GridTaskSpawnReply {
    pub kind: u8,
    /// 0 = accepted, 1 = denied (no capability), 2 = rejected (overloaded).
    pub status: u8,
    pub _pad: [u8; 2],
    /// Correlation UUID from the request.
    pub correlation_uuid: [u8; 16],
    /// Remote task UUID assigned by the remote node (valid only when status = 0).
    pub remote_task_uuid: [u8; 16],
}

impl GridTaskSpawnReply {
    pub const SIZE: usize = core::mem::size_of::<GridTaskSpawnReply>();
}

/// `GridIpcForward` — forward an IPC message to a remote channel.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GridIpcForward {
    pub kind: u8,
    pub _pad: [u8; 1],
    /// Payload length in bytes (up to `GRID_IPC_MAX_PAYLOAD`).
    pub payload_len: u16,
    /// Correlation UUID for reply matching.
    pub correlation_uuid: [u8; 16],
    /// Target channel UUID on the remote node.
    pub target_channel_uuid: [u8; 16],
    /// Inline payload (up to 200 bytes; fragmentation not yet supported).
    pub payload: [u8; 200],
}

impl GridIpcForward {
    pub const SIZE: usize = core::mem::size_of::<GridIpcForward>();
}

/// `GridMemAlloc` — request N pages from a remote node's free RAM.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GridMemAlloc {
    pub kind: u8,
    pub _pad: [u8; 3],
    pub correlation_uuid: [u8; 16],
    /// Number of 4 KiB pages to allocate.
    pub pages: u32,
}

impl GridMemAlloc {
    pub const SIZE: usize = core::mem::size_of::<GridMemAlloc>();
}

/// `GridMemAllocReply` — reply to `GridMemAlloc`.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GridMemAllocReply {
    pub kind: u8,
    /// 0 = granted, 1 = denied (out of memory).
    pub status: u8,
    pub _pad: [u8; 2],
    pub correlation_uuid: [u8; 16],
    /// Physical base address on the remote node (used to map DMA transfer).
    pub remote_phys_base: u64,
    /// Pages granted (may be less than requested).
    pub pages_granted: u32,
    pub _pad2: u32,
}

impl GridMemAllocReply {
    pub const SIZE: usize = core::mem::size_of::<GridMemAllocReply>();
}

/// `GridVfsRead` — read up to 512 bytes from a remote VFS path.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GridVfsRead {
    pub kind: u8,
    pub _pad: [u8; 3],
    pub correlation_uuid: [u8; 16],
    /// Null-terminated absolute path (up to 128 bytes).
    pub path: [u8; 128],
    /// Byte offset.
    pub offset: u64,
    /// Bytes to read (max 512).
    pub length: u16,
    pub _pad2: [u8; 6],
}

impl GridVfsRead {
    pub const SIZE: usize = core::mem::size_of::<GridVfsRead>();
}

/// `GridVfsReadReply`.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GridVfsReadReply {
    pub kind: u8,
    /// 0 = success, non-zero = error code.
    pub status: u8,
    pub _pad: [u8; 2],
    pub correlation_uuid: [u8; 16],
    pub bytes_read: u16,
    pub _pad2: [u8; 6],
    pub data: [u8; 512],
}

impl GridVfsReadReply {
    pub const SIZE: usize = core::mem::size_of::<GridVfsReadReply>();
}

/// `GridResourceUpdate` — periodic resource stats broadcast.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GridResourceUpdate {
    pub kind: u8,
    pub cpu_load_percent: u8,
    pub ram_free_mib: u8,
    pub gpu_load_percent: u8,
    pub node_uuid: [u8; 16],
}

impl GridResourceUpdate {
    pub const SIZE: usize = core::mem::size_of::<GridResourceUpdate>();
}
