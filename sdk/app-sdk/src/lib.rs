// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS App SDK
//!
//! This crate provides the high-level API for writing ring-3 applications
//! targeting the GraphOS kernel. It is `#![no_std]` and has no allocator
//! dependency — all objects use static or stack storage.
//!
//! # Example
//! ```no_run
//! use graphos_app_sdk::window::Window;
//! use graphos_app_sdk::event::Event;
//!
//! const INPUT_CHANNEL: u32 = 10;
//!
//! let mut win = Window::open(640, 480, 0, 0, INPUT_CHANNEL).expect("window open");
//! win.request_focus();
//! loop {
//!     let mut c = win.canvas();
//!     c.clear(0xFF_1A1A2E);
//!     c.draw_text(8, 8, b"Hello, GraphOS!", 0xFF_E0E0FF, 0);
//!     drop(c);
//!     win.present();
//!     match win.poll_event() {
//!         Event::Key { pressed: true, ascii: b'q', .. } => break,
//!         _ => {}
//!     }
//! }
//! ```

#![no_std]
#![warn(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod accessibility;
pub mod animation;
pub mod canvas;
pub mod chrome;
pub mod clipboard;
pub mod drag;
pub mod event;
pub mod sys;
pub mod window;
