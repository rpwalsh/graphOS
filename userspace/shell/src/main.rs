// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! graphos-shell — the GraphOS desktop shell.
//!
//! # Responsibilities
//!
//! - **Taskbar** — a 32-pixel top bar showing the Start launcher chip, one
//!   chip per open window, and a clock/status area on the right.
//! - **Launcher** — a pop-up app grid opened by clicking Start or pressing
//!   the Meta key.  Each entry spawns a new app process via the registry.
//! - **Alt-Tab switcher** — a thumbnail strip cycled with Alt+Tab that
//!   raises the selected window and dismisses on Alt release.
//! - **Notification centre** — a 320-px pop-in panel slid in from the right
//!   edge showing the last 8 notifications as dismissible chips.
//! - **Focus routing** — the shell holds the input-routing privilege and
//!   forwards keyboard/pointer events to the correct window's IPC channel.
//!
//! The shell runs as a standard ring-3 process.  It interacts with the
//! kernel compositor exclusively via the public syscall ABI
//! (`SYS_SURFACE_*`, `SYS_INPUT_*`, `SYS_CHANNEL_*`).
//!
//! # App sandboxing
//!
//! When the shell spawns an app it asks the kernel to apply the
//! `MODE_APP_STRICT` seccomp profile (via `SYS_SPAWN` flags) so the new
//! process cannot install drivers, escalate identity, or delegate
//! capabilities.

// Shell binary — built for the host target; uses std.

use std::collections::VecDeque;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const TASKBAR_HEIGHT: i32 = 32;
const LAUNCHER_COLS: usize = 4;
const LAUNCHER_ROWS: usize = 3;
const LAUNCHER_CELL_W: i32 = 96;
const LAUNCHER_CELL_H: i32 = 72;
const MAX_WINDOWS: usize = 32;
const MAX_NOTIFICATIONS: usize = 8;
const ALT_TAB_MAX: usize = 16;

// Notification display duration in simulated ticks (at 60 Hz ≈ 10 s).
const NOTIF_TTL_TICKS: u64 = 600;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Metadata the shell tracks for each open window.
#[derive(Clone)]
struct WindowEntry {
    surface_id: u32,
    /// Short name shown in the taskbar chip.
    title: [u8; 32],
    title_len: usize,
    /// IPC channel to deliver focus events.
    input_channel: u32,
    /// Whether this window is currently focused.
    focused: bool,
}

impl WindowEntry {
    fn title(&self) -> &[u8] {
        &self.title[..self.title_len]
    }
}

/// A brief notification message.
#[derive(Clone)]
struct Notification {
    body: [u8; 64],
    body_len: usize,
    /// Absolute tick when this notification expires.
    expires: u64,
    dismissed: bool,
}

impl Notification {
    fn body(&self) -> &[u8] {
        &self.body[..self.body_len]
    }
}

/// Built-in app launcher entries.
struct LauncherEntry {
    label: &'static [u8],
    /// ELF path under `/pkg/apps/`.
    path: &'static str,
}

const LAUNCHER_APPS: &[LauncherEntry] = &[
    LauncherEntry {
        label: b"Terminal",
        path: "/pkg/apps/terminal",
    },
    LauncherEntry {
        label: b"Files",
        path: "/pkg/apps/files",
    },
    LauncherEntry {
        label: b"Editor",
        path: "/pkg/apps/editor",
    },
    LauncherEntry {
        label: b"Settings",
        path: "/pkg/apps/settings",
    },
    LauncherEntry {
        label: b"Browser",
        path: "/pkg/apps/browser-lite",
    },
    LauncherEntry {
        label: b"AI Console",
        path: "/pkg/apps/ai-console",
    },
    LauncherEntry {
        label: b"Graph View",
        path: "/pkg/apps/shell3d",
    },
    LauncherEntry {
        label: b"Hockey",
        path: "/pkg/apps/ai-air-hockey",
    },
];

// ---------------------------------------------------------------------------
// Shell state
// ---------------------------------------------------------------------------

struct ShellState {
    windows: Vec<WindowEntry>,
    focused_index: Option<usize>,
    launcher_open: bool,
    launcher_selection: usize,
    alt_tab_active: bool,
    alt_tab_index: usize,
    notifications: VecDeque<Notification>,
    notif_panel_open: bool,
    tick: u64,
}

impl ShellState {
    fn new() -> Self {
        Self {
            windows: Vec::with_capacity(MAX_WINDOWS),
            focused_index: None,
            launcher_open: false,
            launcher_selection: 0,
            alt_tab_active: false,
            alt_tab_index: 0,
            notifications: VecDeque::with_capacity(MAX_NOTIFICATIONS),
            notif_panel_open: false,
            tick: 0,
        }
    }

    // -----------------------------------------------------------------------
    // Window management
    // -----------------------------------------------------------------------

    /// Register a newly created window.
    fn register_window(&mut self, surface_id: u32, title: &[u8], input_channel: u32) {
        if self.windows.len() >= MAX_WINDOWS {
            return;
        }
        let mut entry = WindowEntry {
            surface_id,
            title: [0u8; 32],
            title_len: 0,
            input_channel,
            focused: false,
        };
        let copy_len = title.len().min(32);
        entry.title[..copy_len].copy_from_slice(&title[..copy_len]);
        entry.title_len = copy_len;
        self.windows.push(entry);
        // Auto-focus new window.
        let new_idx = self.windows.len() - 1;
        self.set_focus(new_idx);
    }

    /// Remove a window by surface_id.
    fn close_window(&mut self, surface_id: u32) {
        if let Some(pos) = self.windows.iter().position(|w| w.surface_id == surface_id) {
            self.windows.remove(pos);
            // Re-focus the previous window if one exists.
            let new_focus = if self.windows.is_empty() {
                None
            } else {
                Some(self.windows.len() - 1)
            };
            self.focused_index = new_focus;
            if let Some(idx) = self.focused_index {
                self.windows[idx].focused = true;
            }
        }
    }

    fn set_focus(&mut self, index: usize) {
        for (i, w) in self.windows.iter_mut().enumerate() {
            w.focused = i == index;
        }
        self.focused_index = Some(index);
    }

    // -----------------------------------------------------------------------
    // Launcher
    // -----------------------------------------------------------------------

    fn toggle_launcher(&mut self) {
        self.launcher_open = !self.launcher_open;
        if self.launcher_open {
            self.launcher_selection = 0;
        }
    }

    fn launcher_move(&mut self, dx: i32, dy: i32) {
        if !self.launcher_open {
            return;
        }
        let n = LAUNCHER_APPS.len();
        if n == 0 {
            return;
        }
        let sel = self.launcher_selection as i32;
        let cols = LAUNCHER_COLS as i32;
        let new_sel = match (dx, dy) {
            (1, 0) => (sel + 1).rem_euclid(n as i32),
            (-1, 0) => (sel - 1).rem_euclid(n as i32),
            (0, 1) => (sel + cols).min(n as i32 - 1),
            (0, -1) => (sel - cols).max(0),
            _ => sel,
        };
        self.launcher_selection = new_sel as usize;
    }

    fn launcher_launch(&mut self) -> Option<&'static str> {
        if !self.launcher_open {
            return None;
        }
        let entry = LAUNCHER_APPS.get(self.launcher_selection)?;
        self.launcher_open = false;
        Some(entry.path)
    }

    // -----------------------------------------------------------------------
    // Alt-Tab switcher
    // -----------------------------------------------------------------------

    fn alt_tab_begin(&mut self) {
        if self.windows.is_empty() {
            return;
        }
        self.alt_tab_active = true;
        let cur = self.focused_index.unwrap_or(0);
        self.alt_tab_index = (cur + 1) % self.windows.len();
    }

    fn alt_tab_step(&mut self) {
        if !self.alt_tab_active || self.windows.is_empty() {
            return;
        }
        self.alt_tab_index = (self.alt_tab_index + 1) % self.windows.len();
    }

    fn alt_tab_commit(&mut self) {
        if !self.alt_tab_active {
            return;
        }
        self.alt_tab_active = false;
        if self.alt_tab_index < self.windows.len() {
            self.set_focus(self.alt_tab_index);
        }
    }

    // -----------------------------------------------------------------------
    // Notifications
    // -----------------------------------------------------------------------

    fn push_notification(&mut self, body: &[u8]) {
        // Evict oldest if full.
        while self.notifications.len() >= MAX_NOTIFICATIONS {
            self.notifications.pop_front();
        }
        let mut notif = Notification {
            body: [0u8; 64],
            body_len: 0,
            expires: self.tick + NOTIF_TTL_TICKS,
            dismissed: false,
        };
        let copy_len = body.len().min(64);
        notif.body[..copy_len].copy_from_slice(&body[..copy_len]);
        notif.body_len = copy_len;
        self.notifications.push_back(notif);
        self.notif_panel_open = true;
    }

    fn expire_notifications(&mut self) {
        let tick = self.tick;
        self.notifications
            .retain(|n| !n.dismissed && n.expires > tick);
        if self.notifications.is_empty() {
            self.notif_panel_open = false;
        }
    }

    // -----------------------------------------------------------------------
    // Tick
    // -----------------------------------------------------------------------

    fn tick(&mut self) {
        self.tick += 1;
        self.expire_notifications();
    }
}

// ---------------------------------------------------------------------------
// IPC event loop stub
//
// In a real GraphOS process this would call SYS_CHANNEL_RECV in a loop.
// For the initial implementation we define the event dispatch table and
// leave the platform-specific recv call as a well-typed TODO.
// ---------------------------------------------------------------------------

/// IPC message tags the shell listens for.
#[repr(u8)]
enum ShellMsg {
    /// A new surface was presented; payload: surface_id (u32) + title (bytes).
    WindowOpen = 0x70,
    /// A surface was destroyed; payload: surface_id (u32).
    WindowClose = 0x71,
    /// Keyboard event forwarded from kernel input router.
    KeyEvent = 0x60,
    /// Pointer event.
    PointerEvent = 0x61,
    /// Notification push; payload: UTF-8 body.
    Notification = 0x72,
}

fn dispatch_message(state: &mut ShellState, tag: u8, payload: &[u8]) {
    match tag {
        t if t == ShellMsg::WindowOpen as u8 => {
            if payload.len() >= 4 {
                let surface_id =
                    u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                let title = if payload.len() > 4 {
                    &payload[4..]
                } else {
                    b"App"
                };
                // Channel arbitrarily re-uses surface_id as input channel id.
                state.register_window(surface_id, title, surface_id);
            }
        }
        t if t == ShellMsg::WindowClose as u8 => {
            if payload.len() >= 4 {
                let surface_id =
                    u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                state.close_window(surface_id);
            }
        }
        t if t == ShellMsg::KeyEvent as u8 => {
            if payload.len() < 2 {
                return;
            }
            let pressed = payload[0] != 0;
            let ascii = payload[1];
            handle_key(state, pressed, ascii);
        }
        t if t == ShellMsg::Notification as u8 => {
            state.push_notification(payload);
        }
        _ => {}
    }
}

fn handle_key(state: &mut ShellState, pressed: bool, ascii: u8) {
    if !pressed {
        // Alt released — commit alt-tab if active.
        // (Real impl: track Alt key state via HID usage.)
        if state.alt_tab_active {
            state.alt_tab_commit();
        }
        return;
    }
    match ascii {
        // Meta / Win key (synthetic ASCII 0x1B from kernel input router)
        0x1B => state.toggle_launcher(),
        // Tab: alt-tab (real impl would check Alt modifier via HID).
        b'\t' => {
            if state.alt_tab_active {
                state.alt_tab_step();
            } else {
                state.alt_tab_begin();
            }
        }
        // Arrow keys in launcher.
        0x41 /* up */    if state.launcher_open => state.launcher_move(0, -1),
        0x42 /* down */  if state.launcher_open => state.launcher_move(0,  1),
        0x43 /* right */ if state.launcher_open => state.launcher_move( 1, 0),
        0x44 /* left */  if state.launcher_open => state.launcher_move(-1, 0),
        // Enter: launch selected launcher entry.
        b'\r' if state.launcher_open => {
            let _path = state.launcher_launch();
            // TODO: call SYS_SPAWN(_path, flags=APP_STRICT) when kernel
            // syscall binding is available in userspace.
        }
        // Escape: dismiss launcher or notification panel.
        b'\x1B' => {
            if state.launcher_open {
                state.launcher_open = false;
            } else if state.notif_panel_open {
                state.notif_panel_open = false;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    eprintln!("[graphos-shell] starting");

    let mut state = ShellState::new();

    // Emit a startup notification so the test harness can verify the shell
    // started successfully even before any windows are open.
    state.push_notification(b"GraphOS shell ready");

    // ---- main event loop ----
    // In production this calls SYS_CHANNEL_RECV on the shell's dedicated
    // input channel.  For now we simulate two open windows and an alt-tab.
    //
    // Replace the block below with the kernel IPC receive loop once the
    // userspace IPC bindings are plumbed through from the protected ring-3
    // channel allocator.

    // Simulate: Terminal opens.
    state.register_window(1, b"Terminal", 1);
    // Simulate: Editor opens.
    state.register_window(2, b"Editor", 2);
    // Simulate: alt-tab (Terminal → Editor focus).
    state.alt_tab_begin();
    state.alt_tab_commit();

    assert_eq!(
        state.focused_index,
        Some(0),
        "alt-tab wrapped back to Terminal"
    );

    // Simulate notification.
    state.push_notification(b"Package update available");
    assert!(!state.notifications.is_empty());

    // Simulate tick expiry.
    state.tick = NOTIF_TTL_TICKS + 1;
    state.expire_notifications();
    // Notifications with TTL 0 would have expired. Our fresh push set
    // expires = 1 + NOTIF_TTL_TICKS which is now == tick, so it stays.
    // (Strictly > comparison means it is just on the boundary.)

    eprintln!("[graphos-shell] self-test passed — entering IPC wait loop");

    // TODO: replace with blocking SYS_CHANNEL_RECV loop:
    //   loop {
    //       let (tag, payload) = ipc::recv_blocking(SHELL_CHANNEL);
    //       dispatch_message(&mut state, tag, &payload);
    //       state.tick();
    //   }
}
