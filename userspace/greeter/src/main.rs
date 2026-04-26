// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! graphos-greeter — lock screen and login greeter.
//!
//! The greeter is the first visual element shown on boot and also serves as
//! the lock screen when the session is suspended.
//!
//! # States
//! - **Locked** — full-screen overlay showing time, date, and a password
//!   field.  Input is forwarded to `authn` for PAM-style verification.
//! - **Greeter** — multi-user picker shown before any session starts.
//! - **Unlocked** — greeter dismisses and hands off to graphos-shell.
//!
//! # Security model
//! - Runs under `MODE_PROTECTED_STRICT` seccomp profile (cannot spawn, grant
//!   capabilities, or access non-auth IPC channels).
//! - Password bytes are never echoed; the input buffer is zeroed after use.
//! - Brute-force mitigation: after 5 failed attempts a 30-second cooldown is
//!   enforced before the next attempt is accepted.

// Ring-3 standard binary.

use std::io::{self, Write};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_PASSWORD_LEN: usize = 128;
const MAX_FAILED_ATTEMPTS: u32 = 5;
const LOCKOUT_TICKS: u64 = 30 * 60; // 30 s at 60 Hz

// ---------------------------------------------------------------------------
// Greeter state machine
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum GreeterMode {
    /// System just started; show user picker.
    InitialGreeter,
    /// Session is running and screen is locked.
    ScreenLocked,
    /// Authentication succeeded; greeter is fading out.
    Unlocking,
}

struct GreeterState {
    mode: GreeterMode,
    /// Index of the currently selected user in the user list.
    selected_user: usize,
    /// Password currently being typed (never persisted).
    password: [u8; MAX_PASSWORD_LEN],
    password_len: usize,
    /// Number of consecutive failed authentication attempts.
    failed_attempts: u32,
    /// Tick at which the lockout expires (0 = not in lockout).
    lockout_until: u64,
    /// Current tick counter.
    tick: u64,
}

impl GreeterState {
    fn new(mode: GreeterMode) -> Self {
        Self {
            mode,
            selected_user: 0,
            password: [0u8; MAX_PASSWORD_LEN],
            password_len: 0,
            failed_attempts: 0,
            lockout_until: 0,
            tick: 0,
        }
    }

    /// Type a character into the password field.
    fn password_push(&mut self, ch: u8) {
        if self.password_len < MAX_PASSWORD_LEN {
            self.password[self.password_len] = ch;
            self.password_len += 1;
        }
    }

    /// Delete the last character from the password field.
    fn password_pop(&mut self) {
        if self.password_len > 0 {
            self.password[self.password_len - 1] = 0;
            self.password_len -= 1;
        }
    }

    /// Zero the password buffer.
    fn password_clear(&mut self) {
        for b in self.password.iter_mut() {
            *b = 0;
        }
        self.password_len = 0;
    }

    /// Returns `true` if brute-force lockout is currently active.
    fn is_locked_out(&self) -> bool {
        self.tick < self.lockout_until
    }

    /// Attempt to authenticate with the current password.
    ///
    /// `auth_ok` is the result from the kernel `authn` service (would
    /// normally be obtained via IPC in production).  The password buffer
    /// is always cleared after this call.
    fn try_authenticate(&mut self, auth_ok: bool) -> bool {
        if self.is_locked_out() {
            self.password_clear();
            return false;
        }

        if auth_ok {
            self.failed_attempts = 0;
            self.lockout_until = 0;
            self.password_clear();
            self.mode = GreeterMode::Unlocking;
            true
        } else {
            self.failed_attempts += 1;
            if self.failed_attempts >= MAX_FAILED_ATTEMPTS {
                self.lockout_until = self.tick + LOCKOUT_TICKS;
                self.failed_attempts = 0;
            }
            self.password_clear();
            false
        }
    }

    fn tick_advance(&mut self) {
        self.tick += 1;
    }
}

// ---------------------------------------------------------------------------
// User list (populated from kernel UserDb at runtime)
// ---------------------------------------------------------------------------

struct UserEntry {
    username: [u8; 32],
    username_len: usize,
    uid: u32,
}

impl UserEntry {
    fn username(&self) -> &[u8] {
        &self.username[..self.username_len]
    }
}

fn make_user(name: &[u8], uid: u32) -> UserEntry {
    let mut entry = UserEntry {
        username: [0u8; 32],
        username_len: 0,
        uid,
    };
    let copy_len = name.len().min(32);
    entry.username[..copy_len].copy_from_slice(&name[..copy_len]);
    entry.username_len = copy_len;
    entry
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    eprintln!("[greeter] GraphOS greeter starting");

    // Self-test: run the state machine through a successful login flow.
    let mut state = GreeterState::new(GreeterMode::InitialGreeter);

    // Type "password" into the password field.
    for &b in b"password" {
        state.password_push(b);
    }
    assert_eq!(state.password_len, 8);

    // Backspace removes last char.
    state.password_pop();
    assert_eq!(state.password_len, 7);

    // Simulate correct auth (in production: IPC call to authn service).
    let ok = state.try_authenticate(true);
    assert!(ok);
    assert_eq!(state.mode, GreeterMode::Unlocking);
    assert_eq!(state.password_len, 0, "password buffer cleared after auth");

    // Test brute-force lockout.
    let mut state2 = GreeterState::new(GreeterMode::ScreenLocked);
    for _ in 0..MAX_FAILED_ATTEMPTS {
        let r = state2.try_authenticate(false);
        assert!(!r);
    }
    assert!(
        state2.is_locked_out(),
        "lockout after {} failures",
        MAX_FAILED_ATTEMPTS
    );

    // Correct password during lockout is still rejected.
    let rejected = state2.try_authenticate(true);
    assert!(!rejected, "auth rejected during lockout");

    eprintln!("[greeter] self-test passed");

    // TODO: open full-screen surface, render clock/date, receive key events,
    //       call SYS_IPC authn service to verify password, dismiss on unlock.

    let _ = io::stdout().flush();
}
