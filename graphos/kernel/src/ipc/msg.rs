// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! IPC message types — header and tag definitions.
//!
//! Every IPC message has a fixed 24-byte header followed by a variable-length
//! payload. The header is designed for zero-parse dispatch: the receiver can
//! branch on `tag` without touching the payload.
//!
//! ## Layout contract
//! `MsgHeader` is `#[repr(C)]` and 24 bytes. The payload immediately follows
//! the header in the channel's message slot. Total slot size =
//! `size_of::<MsgHeader>() + max_msg_bytes`.
//!
//! ## Paper doctrine alignment
//! - `reply_endpoint` enables request/response IPC without out-of-band state.
//! - `timestamp` enables temporal ordering for happened-before analysis.
//! - `tag` enables typed message dispatch, preparing for the typed temporal
//!   graph message protocol that graphd will use.

use crate::graph::types::Timestamp;

/// Maximum payload size in bytes (excluding header).
pub const MAX_MSG_BYTES: usize = 256;

/// Message type tag — enables zero-parse dispatch on the receiver side.
///
/// Tags in the range 0x00–0x0F are reserved for the kernel.
/// Tags 0x10–0xFF are available for service protocols (graphd, modeld, etc.).
///
/// ## Service protocol convention
/// Each service defines its own tag sub-range:
/// - 0x10–0x1F: graphd (graph query/mutate)
/// - 0x20–0x2F: modeld (inference request/response)
/// - 0x30–0x3F: servicemgr (lifecycle)
/// - 0x40–0x4F: shell3d / compositor
/// - 0x50–0xFF: reserved for future services
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgTag {
    /// Raw data message (no semantic type).
    Data = 0x00,
    /// Ping — keepalive / health check. Payload is empty.
    Ping = 0x01,
    /// Pong — response to Ping. Payload is empty.
    Pong = 0x02,
    /// Error — payload contains a u64 error code.
    Error = 0x03,
    /// Shutdown request — receiver should clean up and exit.
    Shutdown = 0x04,
    /// Registry changed notification — kernel broadcasts to subscribers.
    /// Payload is a little-endian u64 new generation number.
    RegistryChanged = 0x05,

    // ---- graphd protocol (0x10–0x1F) ----
    /// Graph query request. Payload is a GraphQuery.
    GraphQuery = 0x10,
    /// Graph query response. Payload is a GraphQueryResult.
    GraphQueryResult = 0x11,
    /// Graph mutation request. Payload is a GraphMutation.
    GraphMutate = 0x12,
    /// Graph mutation acknowledgement. Payload is a MutationAck.
    GraphMutateAck = 0x13,
    /// Graph subscription request. Payload is a SubscribeRequest.
    GraphSubscribe = 0x14,
    /// Graph event notification. Payload is a GraphEvent.
    GraphEvent = 0x15,

    // ---- modeld protocol (0x20–0x2F) ----
    /// Inference request. Payload is an InferenceRequest.
    InferenceRequest = 0x20,
    /// Inference response. Payload is an InferenceResponse.
    InferenceResponse = 0x21,
    /// Tool invocation request. Payload is a ToolRequest.
    ToolRequest = 0x22,
    /// Tool invocation result. Payload is a ToolResult.
    ToolResult = 0x23,

    // ---- servicemgr protocol (0x30–0x3F) ----
    /// Service registration. Payload is a ServiceRegistration.
    ServiceRegister = 0x30,
    /// Service status report. Payload is a ServiceStatus.
    ServiceStatus = 0x31,

    // ---- trainerd protocol (0x40–0x4F) ----
    /// Job submission request. Payload is a JobRequest.
    JobSubmit = 0x40,
    /// Job status report. Payload is a JobReport.
    JobReport = 0x41,
    /// Training control command. Payload is a TrainingControl.
    TrainingControl = 0x42,
    /// Training status query (payload empty).
    TrainingStatusQuery = 0x43,
    /// Training status response. Payload is a TrainingStatus.
    TrainingStatusResponse = 0x44,

    // ---- artifactsd protocol (0x50–0x5F) ----
    /// Artifact registration. Payload is an ArtifactDesc.
    ArtifactCreated = 0x50,
    /// Artifact query. Payload is a node ID or content hash.
    ArtifactQuery = 0x51,
    /// Artifact query response. Payload is an ArtifactDesc.
    ArtifactResponse = 0x52,

    // ---- sysd protocol (0x60–0x6F) ----
    /// Runtime event. Payload is a RuntimeEvent.
    RuntimeEvent = 0x60,
    /// Audit entry. Payload is an AuditEntry.
    AuditEntry = 0x61,
    /// Diagnostic query (payload empty).
    DiagnosticQuery = 0x62,
    /// Diagnostic response. Payload is a DiagnosticStatus.
    DiagnosticResponse = 0x63,
    /// Cognitive pipeline status. Payload is a CognitiveStatus.
    CognitiveStatus = 0x64,
    /// Display-system frame-tick broadcast to ring-3 apps.
    /// Payload is 8 bytes: little-endian u64 `now_ms` (scheduler ticks).
    FrameTick = 0x65,
}

impl MsgTag {
    /// Convert from a raw u8.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x00 => Some(Self::Data),
            0x01 => Some(Self::Ping),
            0x02 => Some(Self::Pong),
            0x03 => Some(Self::Error),
            0x04 => Some(Self::Shutdown),
            0x05 => Some(Self::RegistryChanged),
            0x10 => Some(Self::GraphQuery),
            0x11 => Some(Self::GraphQueryResult),
            0x12 => Some(Self::GraphMutate),
            0x13 => Some(Self::GraphMutateAck),
            0x14 => Some(Self::GraphSubscribe),
            0x15 => Some(Self::GraphEvent),
            0x20 => Some(Self::InferenceRequest),
            0x21 => Some(Self::InferenceResponse),
            0x22 => Some(Self::ToolRequest),
            0x23 => Some(Self::ToolResult),
            0x30 => Some(Self::ServiceRegister),
            0x31 => Some(Self::ServiceStatus),
            0x40 => Some(Self::JobSubmit),
            0x41 => Some(Self::JobReport),
            0x42 => Some(Self::TrainingControl),
            0x43 => Some(Self::TrainingStatusQuery),
            0x44 => Some(Self::TrainingStatusResponse),
            0x50 => Some(Self::ArtifactCreated),
            0x51 => Some(Self::ArtifactQuery),
            0x52 => Some(Self::ArtifactResponse),
            0x60 => Some(Self::RuntimeEvent),
            0x61 => Some(Self::AuditEntry),
            0x62 => Some(Self::DiagnosticQuery),
            0x63 => Some(Self::DiagnosticResponse),
            0x64 => Some(Self::CognitiveStatus),
            0x65 => Some(Self::FrameTick),
            _ => None,
        }
    }
}

/// Fixed-size message header. Precedes the payload in every channel slot.
///
/// 24 bytes, `#[repr(C)]`, stable layout for cross-task consumption.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MsgHeader {
    /// Message type tag for zero-parse dispatch.
    pub tag: MsgTag,
    /// Padding for alignment.
    pub _pad: [u8; 3],
    /// Payload length in bytes (0..=MAX_MSG_BYTES).
    pub payload_len: u32,
    /// IPC channel that should receive replies for this sender (0 = none/kernel).
    pub reply_endpoint: u32,
    /// Boot-relative timestamp when the message was enqueued.
    pub timestamp: Timestamp,
}

impl MsgHeader {
    /// A zeroed header (empty slot sentinel).
    pub const EMPTY: Self = Self {
        tag: MsgTag::Data,
        _pad: [0; 3],
        payload_len: 0,
        reply_endpoint: 0,
        timestamp: 0,
    };

    /// Total size of this message (header + payload).
    pub const fn total_size(&self) -> usize {
        core::mem::size_of::<Self>() + self.payload_len as usize
    }
}
