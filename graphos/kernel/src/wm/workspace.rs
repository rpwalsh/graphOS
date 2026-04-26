// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Phase J Session 29 — Virtual workspace grid.
//!
//! Provides a 2×2 grid of virtual workspaces (configurable up to 4×4).
//! Each workspace owns a list of surface IDs.  The compositor reads the
//! active workspace to decide which surfaces to include in the frame.
//!
//! ## Slide transition
//! When the active workspace changes, a spring-physics slide animation is
//! triggered.  The animation state is exported via `slide_offset_fp()` so
//! `gpu_compositor.rs` can apply a screen-space translate to every window
//! during the transition frame.
//!
//! ## Exposé / overview mode
//! Exposé lays out windows in a grid (managed by `gpu.rs` / `GpuCompositor`).
//! This module tracks the "in overview" flag and the focused workspace so the
//! compositor can correctly filter surface visibility.
//!
//! ## Graph identity
//! Each workspace slot is identified by a UUID derived from its (col, row)
//! position (`UUID v5` with namespace = graphOS workspace NS UUID).  The
//! graph arena records `NodeKind::Workspace` + `Contains` edges to active
//! surfaces.  Graph mutations are deferred to ring-3 desktop service; the
//! kernel stores only the slot IDs and surface lists.

use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use spin::Mutex;

// ── Configuration ─────────────────────────────────────────────────────────────

/// Maximum workspace grid dimension (columns or rows).
pub const MAX_WORKSPACE_DIM: usize = 4;
/// Default workspace grid: 2 columns × 2 rows.
pub const DEFAULT_COLS: usize = 2;
pub const DEFAULT_ROWS: usize = 2;
/// Maximum surfaces per workspace.
pub const MAX_SURFACES_PER_WS: usize = 16;

// ── Workspace slot ─────────────────────────────────────────────────────────────

/// One virtual workspace.
#[derive(Clone, Copy)]
pub struct Workspace {
    /// UUID v5 of this workspace (first 8 bytes; 0 = uninitialised).
    pub uuid_hi: u64,
    pub uuid_lo: u64,
    /// Surface IDs present on this workspace.
    pub surfaces: [u32; MAX_SURFACES_PER_WS],
    pub surface_count: usize,
}

impl Workspace {
    const EMPTY: Self = Self {
        uuid_hi: 0,
        uuid_lo: 0,
        surfaces: [0u32; MAX_SURFACES_PER_WS],
        surface_count: 0,
    };

    /// Assign a stable UUID v5-style identity (simple XOR name hash for kernel use).
    pub fn init_uuid(&mut self, col: usize, row: usize) {
        // Namespace UUID for GraphOS workspaces (first 16 bytes, hard-coded).
        const NS_HI: u64 = 0x6B657796_2F3D4B5E;
        const NS_LO: u64 = 0xA1B2C3D4_E5F60718;
        let name: u64 = ((col as u64) << 32) | (row as u64);
        self.uuid_hi = NS_HI ^ name.rotate_left(17);
        self.uuid_lo = NS_LO ^ name.rotate_right(23);
    }

    pub fn add_surface(&mut self, id: u32) {
        if self.surface_count < MAX_SURFACES_PER_WS
            && !self.surfaces[..self.surface_count].contains(&id)
        {
            self.surfaces[self.surface_count] = id;
            self.surface_count += 1;
        }
    }

    pub fn remove_surface(&mut self, id: u32) {
        if let Some(pos) = self.surfaces[..self.surface_count]
            .iter()
            .position(|&s| s == id)
        {
            self.surfaces[pos] = self.surfaces[self.surface_count - 1];
            self.surface_count -= 1;
        }
    }

    pub fn surfaces(&self) -> &[u32] {
        &self.surfaces[..self.surface_count]
    }
}

// ── Grid state ─────────────────────────────────────────────────────────────────

struct WorkspaceGrid {
    cols: usize,
    rows: usize,
    slots: [[Workspace; MAX_WORKSPACE_DIM]; MAX_WORKSPACE_DIM],
    active_col: usize,
    active_row: usize,
}

impl WorkspaceGrid {
    const fn new() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            slots: [[Workspace::EMPTY; MAX_WORKSPACE_DIM]; MAX_WORKSPACE_DIM],
            active_col: 0,
            active_row: 0,
        }
    }
}

static GRID: Mutex<WorkspaceGrid> = Mutex::new(WorkspaceGrid::new());

/// True while the overview (Exposé) mode is active.
pub static OVERVIEW_MODE: AtomicBool = AtomicBool::new(false);

// ── Slide animation ────────────────────────────────────────────────────────────

/// Fixed-point 1024-scale screen-space X offset for the slide transition.
/// 0 = no slide.  Positive = content moving right (navigating left).
/// Negative = content moving left (navigating right).
static SLIDE_OFFSET_FP: AtomicI32 = AtomicI32::new(0);

/// Spring velocity for slide (fixed-point 1024, pixels/tick at 60 Hz).
static SLIDE_VELOCITY_FP: AtomicI32 = AtomicI32::new(0);

/// Slide target (0 when slide complete).
static SLIDE_TARGET_FP: AtomicI32 = AtomicI32::new(0);

/// Spring constant for workspace slide (pixels²/tick²).
const SLIDE_K_FP: i32 = 512; // ≈ 0.5 × 1024
/// Damping coefficient.
const SLIDE_C_FP: i32 = 192; // ≈ 0.19 × 1024

/// Advance the slide spring by one 60 Hz frame.  Call from the vsync path.
pub fn tick_slide() {
    let x = SLIDE_OFFSET_FP.load(Ordering::Relaxed);
    let v = SLIDE_VELOCITY_FP.load(Ordering::Relaxed);
    let tgt = SLIDE_TARGET_FP.load(Ordering::Relaxed);

    if x == tgt && v == 0 {
        return;
    }

    // v[n+1] = v[n] + (-k(x - target) - c·v) / 1024
    let force = -SLIDE_K_FP * (x - tgt) / 1024 - SLIDE_C_FP * v / 1024;
    let new_v = v + force;
    let new_x = x + new_v / 1024;

    // Snap to target when close enough.
    if (new_x - tgt).unsigned_abs() < 4 && new_v.unsigned_abs() < 4 {
        SLIDE_OFFSET_FP.store(tgt, Ordering::Relaxed);
        SLIDE_VELOCITY_FP.store(0, Ordering::Relaxed);
    } else {
        SLIDE_OFFSET_FP.store(new_x, Ordering::Relaxed);
        SLIDE_VELOCITY_FP.store(new_v, Ordering::Relaxed);
    }
}

/// Current slide offset (fixed-point 1024).  0 = no transition.
pub fn slide_offset_fp() -> i32 {
    SLIDE_OFFSET_FP.load(Ordering::Relaxed)
}

/// True when a slide transition is ongoing.
pub fn slide_in_progress() -> bool {
    let x = SLIDE_OFFSET_FP.load(Ordering::Relaxed);
    let t = SLIDE_TARGET_FP.load(Ordering::Relaxed);
    x != t || SLIDE_VELOCITY_FP.load(Ordering::Relaxed) != 0
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Initialise the workspace grid.  Must be called once after boot.
pub fn init(cols: usize, rows: usize) {
    let cols = cols.clamp(1, MAX_WORKSPACE_DIM);
    let rows = rows.clamp(1, MAX_WORKSPACE_DIM);
    let mut g = GRID.lock();
    g.cols = cols;
    g.rows = rows;
    for r in 0..rows {
        for c in 0..cols {
            g.slots[r][c] = Workspace::EMPTY;
            g.slots[r][c].init_uuid(c, r);
        }
    }
}

/// Navigate to the workspace at `(col, row)`.  Triggers a slide animation.
/// Screen width is needed to compute the slide distance.
pub fn navigate_to(col: usize, row: usize, screen_w: u32) {
    let mut g = GRID.lock();
    let col = col.clamp(0, g.cols - 1);
    let row = row.clamp(0, g.rows - 1);
    if col == g.active_col && row == g.active_row {
        return;
    }

    let delta_col = col as i32 - g.active_col as i32;
    // Horizontal slide: positive delta_col means user moved right → content moves left.
    let target = -(delta_col * screen_w as i32);
    SLIDE_TARGET_FP.store(target * 1024, Ordering::Relaxed);
    SLIDE_OFFSET_FP.store(0, Ordering::Relaxed);
    SLIDE_VELOCITY_FP.store(0, Ordering::Relaxed);

    g.active_col = col;
    g.active_row = row;
}

/// Convenience wrappers for keyboard shortcuts.
pub fn navigate_left(screen_w: u32) {
    let (col, row, cols) = {
        let g = GRID.lock();
        (g.active_col, g.active_row, g.cols)
    };
    if col > 0 {
        navigate_to(col - 1, row, screen_w);
    } else {
        navigate_to(cols - 1, row, screen_w);
    } // wrap
}

pub fn navigate_right(screen_w: u32) {
    let (col, row, cols) = {
        let g = GRID.lock();
        (g.active_col, g.active_row, g.cols)
    };
    navigate_to((col + 1) % cols, row, screen_w);
}

/// Assign a surface to the active workspace.
pub fn assign_surface_to_active(surface_id: u32) {
    let mut g = GRID.lock();
    let (c, r) = (g.active_col, g.active_row);
    g.slots[r][c].add_surface(surface_id);
}

/// Remove a surface from all workspaces (called on surface free).
pub fn remove_surface(surface_id: u32) {
    let mut g = GRID.lock();
    for r in 0..g.rows {
        for c in 0..g.cols {
            g.slots[r][c].remove_surface(surface_id);
        }
    }
}

/// Snapshot of surface IDs for the active workspace.
/// Returns the count written.
pub fn active_surface_snapshot(out: &mut [u32]) -> usize {
    let g = GRID.lock();
    let (c, r) = (g.active_col, g.active_row);
    let ws = &g.slots[r][c];
    let n = ws.surface_count.min(out.len());
    out[..n].copy_from_slice(&ws.surfaces[..n]);
    n
}

/// Enter / exit overview (Exposé) mode.
pub fn set_overview(active: bool) {
    OVERVIEW_MODE.store(active, Ordering::Release);
}

/// Returns `(active_col, active_row, cols, rows)`.
pub fn active_pos() -> (usize, usize, usize, usize) {
    let g = GRID.lock();
    (g.active_col, g.active_row, g.cols, g.rows)
}
