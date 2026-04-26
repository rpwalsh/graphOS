// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! modeld IPC protocol — inference, tool routing, and context assembly payloads.
//!
//! These types define the message payloads carried over IPC channels when
//! communicating with modeld. They match `MsgTag` values 0x20–0x2F.
//!
//! All types are `#[repr(C)]` and fixed-size for zero-copy channel reads.
//!
//! ## Paper doctrine alignment
//!
//! modeld is NOT a blind chatbot wrapper. It is a graph-native operating
//! participant. These types make that concrete:
//!
//! - **Context assembly**: `InferenceRequest` carries a `context_strategy`
//!   that tells modeld how to assemble graph context. modeld consumes
//!   Local Operator scores, Structural summaries, and Predictive warnings
//!   to decide what graph context to include in the prompt.
//!
//! - **Tool routing**: `ToolRequest` / `ToolResult` enable modeld to
//!   dispatch tool calls to graphd, servicemgr, or other services over
//!   IPC channels. The tool planner uses Causal Decision Engine rankings
//!   to choose which tools are relevant.
//!
//! - **Rank awareness**: `InferenceRequest` includes a `relevance_threshold`
//!   from the Local Operator Engine. Only graph context above this threshold
//!   is included, keeping context windows efficient.
//!
//! - **Causality awareness**: `ToolResult` carries a `confidence` score
//!   from the Causal Decision Engine. modeld uses this to weight
//!   recommendations and explain why a tool was chosen.

use crate::graph::types::{NodeId, Timestamp, Weight};

// ────────────────────────────────────────────────────────────────────
// Inference request (MsgTag::InferenceRequest = 0x20)
// ────────────────────────────────────────────────────────────────────

/// Maximum prompt bytes that fit in a single IPC message payload.
/// For longer prompts, the requester must use a shared-memory region
/// (future optimisation) or chunk across multiple messages.
pub const MAX_PROMPT_BYTES: usize = 192;

/// Strategy for how modeld should assemble graph context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ContextStrategy {
    /// No graph context — raw prompt only.
    None = 0,
    /// Include scored neighborhood of the focus node.
    ScoredNeighborhood = 1,
    /// Include structural summary of the workspace.
    StructuralSummary = 2,
    /// Include predictive warnings (drift, risk).
    PredictiveWarnings = 3,
    /// Include causal ancestors of the focus node.
    CausalContext = 4,
    /// Full context: scored + structural + predictive + causal.
    /// modeld decides what fits in the context window.
    Full = 5,
}

/// Inference request payload.
///
/// The prompt is embedded inline (up to MAX_PROMPT_BYTES). For longer
/// prompts, `prompt_len` indicates how many bytes are valid.
///
/// `focus_node` tells modeld which graph node the request is about.
/// modeld uses this to assemble relevant graph context per `context_strategy`.
///
/// `relevance_threshold` (16.16 fixed-point) filters out graph context
/// below this score. This keeps context windows efficient per the Local
/// Operator Engine doctrine.
///
/// Total size: 8 + 8 + 4 + 1 + 1 + 1 + 1 + 192 + 8 + 8 + ... ~232 bytes.
/// Fits in MAX_MSG_BYTES (256).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct InferenceRequest {
    /// Focus node in the graph (what is this request about?).
    pub focus_node: NodeId,
    /// Timestamp of the request (for temporal context windowing).
    pub timestamp: Timestamp,
    /// Minimum relevance score for graph context inclusion (16.16).
    pub relevance_threshold: Weight,
    /// How to assemble graph context.
    pub context_strategy: ContextStrategy,
    /// Priority: 0=background, 1=normal, 2=urgent.
    pub priority: u8,
    /// Length of the prompt in bytes.
    pub prompt_len: u8,
    /// Padding.
    pub _pad: u8,
    /// Inline prompt bytes.
    pub prompt: [u8; MAX_PROMPT_BYTES],
}

impl InferenceRequest {
    pub const EMPTY: Self = Self {
        focus_node: 0,
        timestamp: 0,
        relevance_threshold: 0,
        context_strategy: ContextStrategy::None,
        priority: 1,
        prompt_len: 0,
        _pad: 0,
        prompt: [0u8; MAX_PROMPT_BYTES],
    };

    /// Create a request with an inline prompt.
    pub fn with_prompt(focus_node: NodeId, strategy: ContextStrategy, prompt: &[u8]) -> Self {
        let mut req = Self::EMPTY;
        req.focus_node = focus_node;
        req.context_strategy = strategy;
        let copy_len = prompt.len().min(MAX_PROMPT_BYTES);
        req.prompt[..copy_len].copy_from_slice(&prompt[..copy_len]);
        req.prompt_len = copy_len as u8;
        req
    }

    /// Get the prompt as a byte slice.
    pub fn prompt_bytes(&self) -> &[u8] {
        &self.prompt[..self.prompt_len as usize]
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Inference response (MsgTag::InferenceResponse = 0x21)
// ────────────────────────────────────────────────────────────────────

/// Maximum response bytes inline.
pub const MAX_RESPONSE_BYTES: usize = 208;

/// Inference response payload.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct InferenceResponse {
    /// Status: 0=success, 1=error, 2=truncated, 3=model_unavailable.
    pub status: u8,
    /// Confidence of the response (16.16 fixed-point, from Causal Engine).
    pub _pad: u8,
    /// Length of the response text.
    pub response_len: u16,
    /// Confidence score from causal reasoning (16.16).
    pub confidence: Weight,
    /// Which graph context was actually included (bitmask of ContextStrategy).
    pub context_used: u8,
    /// Number of graph nodes included in context.
    pub context_node_count: u8,
    /// Padding.
    pub _pad2: [u8; 2],
    /// Timestamp of the response.
    pub timestamp: Timestamp,
    /// Inline response bytes.
    pub response: [u8; MAX_RESPONSE_BYTES],
}

impl InferenceResponse {
    pub const EMPTY: Self = Self {
        status: 0,
        _pad: 0,
        response_len: 0,
        confidence: 0,
        context_used: 0,
        context_node_count: 0,
        _pad2: [0; 2],
        timestamp: 0,
        response: [0u8; MAX_RESPONSE_BYTES],
    };

    /// Create a successful response with inline text.
    pub fn success(text: &[u8], confidence: Weight, context_nodes: u8) -> Self {
        let mut resp = Self::EMPTY;
        let copy_len = text.len().min(MAX_RESPONSE_BYTES);
        resp.response[..copy_len].copy_from_slice(&text[..copy_len]);
        resp.response_len = copy_len as u16;
        resp.confidence = confidence;
        resp.context_node_count = context_nodes;
        resp
    }

    /// Get the response text as a byte slice.
    pub fn response_bytes(&self) -> &[u8] {
        &self.response[..self.response_len as usize]
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Tool request (MsgTag::ToolRequest = 0x22)
// ────────────────────────────────────────────────────────────────────

/// Maximum tool argument bytes inline.
pub const MAX_TOOL_ARG_BYTES: usize = 192;

/// Tool identifiers that modeld can invoke.
///
/// These map to IPC channels that modeld maintains to service endpoints.
/// The tool planner uses Causal Decision Engine rankings to pick tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ToolId {
    /// Query the graph (routes to graphd).
    GraphQuery = 0,
    /// Mutate the graph (routes to graphd).
    GraphMutate = 1,
    /// Read a file (routes to VFS service).
    FileRead = 2,
    /// Write a file (routes to VFS service).
    FileWrite = 3,
    /// Run a shell command (routes to shell3d / compositor).
    ShellCommand = 4,
    /// Query package state (routes to package service).
    PackageQuery = 5,
    /// Run diagnostics (routes to diagnostics service).
    Diagnostics = 6,
    /// Search (routes to search/index service).
    Search = 7,
}

/// Tool invocation request.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ToolRequest {
    /// Which tool to invoke.
    pub tool: ToolId,
    /// Priority: 0=background, 1=normal, 2=urgent.
    pub priority: u8,
    /// Argument length in bytes.
    pub arg_len: u16,
    /// Confidence that this tool is the right choice (16.16, from Causal Engine).
    pub confidence: Weight,
    /// The requesting inference's focus node (for provenance).
    pub focus_node: NodeId,
    /// Timestamp.
    pub timestamp: Timestamp,
    /// Inline argument bytes (tool-specific encoding).
    pub args: [u8; MAX_TOOL_ARG_BYTES],
}

impl ToolRequest {
    pub const EMPTY: Self = Self {
        tool: ToolId::GraphQuery,
        priority: 1,
        arg_len: 0,
        confidence: 0,
        focus_node: 0,
        timestamp: 0,
        args: [0u8; MAX_TOOL_ARG_BYTES],
    };

    /// Create a tool request with inline arguments.
    pub fn new(tool: ToolId, focus: NodeId, confidence: Weight, args: &[u8]) -> Self {
        let mut req = Self::EMPTY;
        req.tool = tool;
        req.focus_node = focus;
        req.confidence = confidence;
        let copy_len = args.len().min(MAX_TOOL_ARG_BYTES);
        req.args[..copy_len].copy_from_slice(&args[..copy_len]);
        req.arg_len = copy_len as u16;
        req
    }

    pub fn arg_bytes(&self) -> &[u8] {
        &self.args[..self.arg_len as usize]
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Tool result (MsgTag::ToolResult = 0x23)
// ────────────────────────────────────────────────────────────────────

/// Maximum tool result bytes inline.
pub const MAX_TOOL_RESULT_BYTES: usize = 208;

/// Tool invocation result.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ToolResult {
    /// Status: 0=success, 1=error, 2=timeout, 3=tool_not_found.
    pub status: u8,
    /// Which tool produced this result.
    pub tool: ToolId,
    /// Result length in bytes.
    pub result_len: u16,
    /// Confidence in the result (16.16, from Causal Engine).
    pub confidence: Weight,
    /// Timestamp.
    pub timestamp: Timestamp,
    /// Inline result bytes.
    pub result: [u8; MAX_TOOL_RESULT_BYTES],
}

impl ToolResult {
    pub const EMPTY: Self = Self {
        status: 0,
        tool: ToolId::GraphQuery,
        result_len: 0,
        confidence: 0,
        timestamp: 0,
        result: [0u8; MAX_TOOL_RESULT_BYTES],
    };

    /// Create a successful result.
    pub fn success(tool: ToolId, data: &[u8], confidence: Weight) -> Self {
        let mut res = Self::EMPTY;
        res.tool = tool;
        res.confidence = confidence;
        let copy_len = data.len().min(MAX_TOOL_RESULT_BYTES);
        res.result[..copy_len].copy_from_slice(&data[..copy_len]);
        res.result_len = copy_len as u16;
        res
    }

    /// Create an error result.
    pub fn error(tool: ToolId, code: u8) -> Self {
        let mut res = Self::EMPTY;
        res.status = code;
        res.tool = tool;
        res
    }

    pub fn result_bytes(&self) -> &[u8] {
        &self.result[..self.result_len as usize]
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Service registration (MsgTag::ServiceRegister = 0x30)
// ────────────────────────────────────────────────────────────────────

/// Maximum service name length.
pub const MAX_SERVICE_NAME: usize = 32;

/// Service registration request — sent by a service to servicemgr
/// when it starts up.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ServiceRegistration {
    /// Service name (null-terminated, up to 31 chars + NUL).
    pub name: [u8; MAX_SERVICE_NAME],
    /// The IPC channel ID this service listens on.
    pub channel_id: u32,
    /// The graph node ID of this service (if already registered).
    pub graph_node: NodeId,
    /// Service capabilities bitmask:
    ///   bit 0: handles graph queries
    ///   bit 1: handles graph mutations
    ///   bit 2: handles inference
    ///   bit 3: handles tool requests
    ///   bit 4: handles file operations
    ///   bit 5: handles diagnostics
    pub capabilities: u32,
    /// Padding.
    pub _pad: u32,
}

impl ServiceRegistration {
    pub const EMPTY: Self = Self {
        name: [0u8; MAX_SERVICE_NAME],
        channel_id: 0,
        graph_node: 0,
        capabilities: 0,
        _pad: 0,
    };

    /// Create a registration with a name and capabilities.
    pub fn new(name: &[u8], channel_id: u32, capabilities: u32) -> Self {
        let mut reg = Self::EMPTY;
        let copy_len = name.len().min(MAX_SERVICE_NAME - 1);
        reg.name[..copy_len].copy_from_slice(&name[..copy_len]);
        reg.channel_id = channel_id;
        reg.capabilities = capabilities;
        reg
    }

    pub fn name_bytes(&self) -> &[u8] {
        let len = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(MAX_SERVICE_NAME);
        &self.name[..len]
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

/// Service status report.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ServiceStatus {
    /// Service name.
    pub name: [u8; MAX_SERVICE_NAME],
    /// Health score (16.16 fixed-point). 1.0 = healthy, 0.0 = dead.
    pub health: Weight,
    /// Number of messages processed since last report.
    pub msgs_processed: u32,
    /// Number of errors since last report.
    pub error_count: u32,
    /// Uptime in abstract ticks.
    pub uptime: Timestamp,
    /// Padding.
    pub _pad: u32,
    pub _pad2: u32,
}

impl ServiceStatus {
    pub const EMPTY: Self = Self {
        name: [0u8; MAX_SERVICE_NAME],
        health: 0,
        msgs_processed: 0,
        error_count: 0,
        uptime: 0,
        _pad: 0,
        _pad2: 0,
    };

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Compile-time size assertions
// ────────────────────────────────────────────────────────────────────
// Every payload type must fit inside MAX_MSG_BYTES (256).

const _: () = {
    assert!(core::mem::size_of::<InferenceRequest>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<InferenceResponse>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<ToolRequest>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<ToolResult>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<ServiceRegistration>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<ServiceStatus>() <= super::msg::MAX_MSG_BYTES);
};
