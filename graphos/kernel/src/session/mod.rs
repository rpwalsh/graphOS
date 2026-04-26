// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use core::sync::atomic::{AtomicU32, Ordering};

use spin::Mutex;

use crate::graph::handles::GraphHandle;
use crate::graph::types::{EdgeKind, NODE_ID_KERNEL};
use crate::uuid::{SessionUuid, Uuid128};

const MAX_SESSIONS: usize = 32;
const PTY_BUF: usize = 256;

/// Byte ring buffer for PTY I/O.
struct Ring {
    buf: [u8; PTY_BUF],
    head: usize,
    tail: usize,
}

impl Ring {
    const fn new() -> Self {
        Self {
            buf: [0; PTY_BUF],
            head: 0,
            tail: 0,
        }
    }
    fn len(&self) -> usize {
        self.tail.wrapping_sub(self.head) & (PTY_BUF - 1)
    }
    fn is_empty(&self) -> bool {
        self.head == self.tail
    }
    fn push(&mut self, b: u8) -> bool {
        let next = (self.tail + 1) & (PTY_BUF - 1);
        if next == self.head {
            return false;
        }
        self.buf[self.tail] = b;
        self.tail = next;
        true
    }
    fn pop(&mut self) -> Option<u8> {
        if self.is_empty() {
            return None;
        }
        let b = self.buf[self.head];
        self.head = (self.head + 1) & (PTY_BUF - 1);
        Some(b)
    }
    fn write_slice(&mut self, data: &[u8]) -> usize {
        let mut n = 0;
        for &b in data {
            if !self.push(b) {
                break;
            }
            n += 1;
        }
        n
    }
    fn read_slice(&mut self, out: &mut [u8]) -> usize {
        let mut n = 0;
        for slot in out.iter_mut() {
            match self.pop() {
                Some(b) => {
                    *slot = b;
                    n += 1;
                }
                None => break,
            }
        }
        n
    }
}

#[derive(Clone, Copy)]
struct SessionRecord {
    active: bool,
    session_uuid: SessionUuid,
    owner_uid: u32,
    owner_gid: u32,
    tty: u32,
    /// Graph arena node ID for this session (0 = not registered).
    graph_node: crate::graph::types::NodeId,
}

impl SessionRecord {
    const EMPTY: Self = Self {
        active: false,
        session_uuid: SessionUuid(Uuid128::NIL),
        owner_uid: 0,
        owner_gid: 0,
        tty: 0,
        graph_node: 0,
    };
}

struct PtyBuffers {
    /// Bytes written by SSH client / host → shell reads (input)
    input: Ring,
    /// Bytes written by shell → SSH daemon reads (output)
    output: Ring,
}

impl PtyBuffers {
    const fn new() -> Self {
        Self {
            input: Ring::new(),
            output: Ring::new(),
        }
    }
}

struct SessionManager {
    sessions: [SessionRecord; MAX_SESSIONS],
    pty: [PtyBuffers; MAX_SESSIONS],
}

impl SessionManager {
    const fn new() -> Self {
        const EMPTY_PTY: PtyBuffers = PtyBuffers::new();
        Self {
            sessions: [SessionRecord::EMPTY; MAX_SESSIONS],
            pty: [EMPTY_PTY; MAX_SESSIONS],
        }
    }
}

static SESSIONS: Mutex<SessionManager> = Mutex::new(SessionManager::new());
static NEXT_TTY: AtomicU32 = AtomicU32::new(1);

pub fn open(uid: u32, gid: u32) -> Option<SessionUuid> {
    let mut mgr = SESSIONS.lock();
    for slot in &mut mgr.sessions {
        if !slot.active {
            let tty = NEXT_TTY.fetch_add(1, Ordering::Relaxed);
            let session_uuid = SessionUuid::from_session_id(tty);
            let gn = crate::graph::handles::register_principal(NODE_ID_KERNEL);
            if gn.is_valid() {
                crate::graph::arena::add_edge(NODE_ID_KERNEL, gn.node_id(), EdgeKind::Created, 0);
            }
            *slot = SessionRecord {
                active: true,
                session_uuid,
                owner_uid: uid,
                owner_gid: gid,
                tty,
                graph_node: gn.node_id(),
            };
            return Some(session_uuid);
        }
    }
    None
}

pub fn close(session_uuid: SessionUuid) -> bool {
    let mut mgr = SESSIONS.lock();
    for slot in &mut mgr.sessions {
        if slot.active && slot.session_uuid == session_uuid {
            let gn = slot.graph_node;
            *slot = SessionRecord::EMPTY;
            if gn != 0 {
                crate::graph::arena::detach_node(gn);
            }
            return true;
        }
    }
    false
}

pub fn owner(session_uuid: SessionUuid) -> Option<(u32, u32)> {
    let mgr = SESSIONS.lock();
    for slot in &mgr.sessions {
        if slot.active && slot.session_uuid == session_uuid {
            return Some((slot.owner_uid, slot.owner_gid));
        }
    }
    None
}

pub fn tty(session_uuid: SessionUuid) -> Option<u32> {
    let mgr = SESSIONS.lock();
    for slot in &mgr.sessions {
        if slot.active && slot.session_uuid == session_uuid {
            return Some(slot.tty);
        }
    }
    None
}

/// Allocate a new PTY for the current user identified by `uid`/`gid`.
///
/// Internally this opens a new session and returns the assigned tty index
/// (the PTY number). The caller can use `SYS_SESSION_ATTACH` to bind other
/// tasks into the same session.
pub fn pty_alloc(uid: u32, gid: u32) -> Option<u32> {
    let session = open(uid, gid)?;
    tty(session)
}

/// Look up the session owning a given tty index.
pub fn find_by_tty(tty_index: u32) -> Option<SessionUuid> {
    let mgr = SESSIONS.lock();
    for slot in &mgr.sessions {
        if slot.active && slot.tty == tty_index {
            return Some(slot.session_uuid);
        }
    }
    None
}

/// Write bytes to the PTY's input ring (SSH → shell direction).
/// Returns the number of bytes written; 0 if tty not found or ring full.
pub fn pty_write(tty_index: u32, data: &[u8]) -> usize {
    let mut mgr = SESSIONS.lock();
    for (i, slot) in mgr.sessions.iter().enumerate() {
        if slot.active && slot.tty == tty_index {
            return mgr.pty[i].input.write_slice(data);
        }
    }
    0
}

/// Read bytes from the PTY's output ring (shell → SSH direction).
/// Returns the number of bytes read; 0 if ring empty.
pub fn pty_read(tty_index: u32, out: &mut [u8]) -> usize {
    let mut mgr = SESSIONS.lock();
    for (i, slot) in mgr.sessions.iter().enumerate() {
        if slot.active && slot.tty == tty_index {
            return mgr.pty[i].output.read_slice(out);
        }
    }
    0
}

/// Write bytes to the PTY's output ring (shell → SSH direction, called by shell task).
pub fn pty_write_output(tty_index: u32, data: &[u8]) -> usize {
    let mut mgr = SESSIONS.lock();
    for (i, slot) in mgr.sessions.iter().enumerate() {
        if slot.active && slot.tty == tty_index {
            return mgr.pty[i].output.write_slice(data);
        }
    }
    0
}

/// Read bytes from the PTY's input ring (shell ← SSH direction, called by shell task).
pub fn pty_read_input(tty_index: u32, out: &mut [u8]) -> usize {
    let mut mgr = SESSIONS.lock();
    for (i, slot) in mgr.sessions.iter().enumerate() {
        if slot.active && slot.tty == tty_index {
            return mgr.pty[i].input.read_slice(out);
        }
    }
    0
}
