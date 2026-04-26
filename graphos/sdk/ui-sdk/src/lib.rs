#![no_std]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#![warn(missing_docs)]

//! GraphOS UI SDK - minimal substrate + toolkit for GraphOS-native UI.
//!
//! Built on top of `graphos-app-sdk`. All types are `no_std`, heap-free where
//! possible, and renderable directly onto a `graphos_app_sdk::canvas::Canvas`.

pub mod charts;
pub mod geom;
pub mod interactive;
pub mod native_views;
pub mod substrate;
pub mod tokens;
pub mod toolkit;
pub mod widgets;
