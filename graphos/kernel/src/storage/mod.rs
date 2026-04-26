// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Tiny block-backed metadata storage for early persistence work.
//!
//! This is intentionally narrow: a writable block device abstraction with a
//! minimal metadata record layer. It is not a mature filesystem, but it gives
//! GraphOS a real place to persist small control-plane objects and future graph
//! state without routing everything through ramfs.

pub mod block;
pub mod meta;
