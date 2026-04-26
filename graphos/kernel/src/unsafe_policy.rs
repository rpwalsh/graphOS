// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS Unsafe Policy
//!
//! This module exists solely as documentation-in-code. It is not compiled
//! into the binary but serves as the canonical reference for when and how
//! `unsafe` is permitted in the GraphOS kernel.
//!
//! # Rules
//!
//! 1. `unsafe` is allowed only where Rust's safe abstractions cannot express
//!    the required operation: boot/firmware boundaries, MMU/page-table ops,
//!    interrupt descriptor setup, device I/O (port and MMIO), ABI boundaries,
//!    and tightly-reviewed low-level primitives.
//!
//! 2. Every `unsafe` block must include a `// SAFETY:` comment that states:
//!    - what invariant is being relied upon
//!    - why safe Rust is insufficient
//!    - what breaks if the invariant is violated
//!
//! 3. `unsafe` must be wrapped behind the narrowest safe interface possible.
//!    Callers should never need to know that unsafe code exists inside.
//!
//! 4. New unsafe code requires review and must be traceable to a specific
//!    hardware or ABI requirement.
//!
//! 5. The burden of proof is on introducing unsafe, not on avoiding it.
