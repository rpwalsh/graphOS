// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! modeld — the local model manager and AI operating participant.
//!
//! modeld is NOT a blind chatbot wrapper. It is the bounded AI operating
//! layer that consumes graph context and acts through typed tool interfaces.
//!
//! ## Paper doctrine engines consumed
//!
//! - **Local Operator Engine** (via graphd): modeld requests scored
//!   neighborhoods to decide what graph context to include in inference
//!   prompts. The relevance_threshold in InferenceRequest filters out
//!   low-score nodes, keeping context windows efficient.
//!
//! - **Structural Engine** (via graphd): modeld requests structural
//!   summaries to understand workspace topology without loading the
//!   entire graph. This enables "what is related to what?" reasoning.
//!
//! - **Predictive Engine** (via graphd): modeld requests drift reports
//!   to surface "what is likely to fail or matter next" in its responses.
//!   Drift warnings are included in context assembly.
//!
//! - **Causal Decision Engine** (via graphd): modeld uses causal
//!   ancestor queries for root-cause analysis when diagnosing issues.
//!   Tool routing decisions carry confidence scores from causal reasoning.
//!
//! ## IPC protocol
//!
//! modeld listens on a dedicated IPC channel. It handles:
//! - MsgTag::InferenceRequest (0x20) -> assembles context, runs inference, replies InferenceResponse (0x21)
//! - MsgTag::ToolRequest (0x22) -> dispatches tool call, replies ToolResult (0x23)
//! - MsgTag::Ping (0x01) -> replies Pong (0x02)
//! - MsgTag::Shutdown (0x04) -> clean exit
//!
//! modeld also sends messages to graphd:
//! - MsgTag::GraphQuery (0x10) to fetch context for inference
//! - MsgTag::GraphMutate (0x12) to record inference results in the graph
//!
//! ## Design constraints
//!
//! - modeld does NOT run in ring 0.
//! - modeld does NOT hold a GPU handle directly. GPU access goes through
//!   a driver service when available.
//! - modeld is bounded: it does not autonomously mutate system state.
//!   All actions require explicit tool invocation with confidence thresholds.
//! - Context assembly is deterministic and auditable: modeld logs which
//!   graph nodes were included and why.
//!
//! ## Current status
//!
//! This host-build service scaffolding shares the real IPC contract and
//! context-assembly/tool-routing shape, but it is not yet launched by the
//! kernel. Protected ring-3 execution is still future work.

#[path = "../../common/host_sys.rs"]
mod host_sys;
#[path = "../../common/ipc.rs"]
mod ipc;

/// modeld configuration.
pub struct ModeldConfig {
    /// IPC channel this service listens on.
    pub listen_channel: u32,
    /// IPC channel for sending queries to graphd.
    pub graphd_channel: u32,
    /// Maximum context tokens to assemble for inference.
    pub max_context_tokens: usize,
    /// Default relevance threshold for context filtering (16.16 fixed-point).
    pub default_relevance_threshold: u32,
    /// Maximum concurrent inference requests (queued).
    pub max_queue_depth: usize,
    /// Whether to log context assembly decisions to the graph.
    pub log_context_decisions: bool,
}

impl ModeldConfig {
    pub const fn default() -> Self {
        Self {
            listen_channel: 3,
            graphd_channel: 2,
            max_context_tokens: 4096,
            // 0.3 in 16.16 = 19661
            default_relevance_threshold: 19661,
            max_queue_depth: 8,
            log_context_decisions: true,
        }
    }
}

/// Context assembly result — tracks what was included and why.
pub struct ContextAssembly {
    /// Number of graph nodes included in context.
    pub node_count: u16,
    /// Total bytes of context text assembled.
    pub context_bytes: u32,
    /// Strategy used.
    pub strategy: u8,
    /// Minimum score of included nodes (16.16).
    pub min_included_score: u32,
    /// Maximum score of included nodes (16.16).
    pub max_included_score: u32,
    /// Whether predictive warnings were included.
    pub includes_drift_warnings: bool,
    /// Whether causal context was included.
    pub includes_causal_context: bool,
}

impl ContextAssembly {
    pub const fn empty() -> Self {
        Self {
            node_count: 0,
            context_bytes: 0,
            strategy: 0,
            min_included_score: 0,
            max_included_score: 0,
            includes_drift_warnings: false,
            includes_causal_context: false,
        }
    }
}

/// Tool routing decision — records why a tool was chosen.
pub struct ToolDecision {
    /// Which tool was selected.
    pub tool_id: u8,
    /// Confidence that this is the right tool (16.16).
    pub confidence: u32,
    /// Number of candidate tools considered.
    pub candidates_considered: u8,
    /// Rank of the selected tool among candidates.
    pub rank: u8,
    /// Whether causal reasoning influenced the decision.
    pub causal_influenced: bool,
}

/// modeld service state.
pub struct ModeldService {
    config: ModeldConfig,
    /// Number of inference requests processed.
    inferences_processed: u64,
    /// Number of tool invocations dispatched.
    tools_dispatched: u64,
    /// Number of errors.
    errors: u64,
    /// Whether the service is running.
    running: bool,
}

impl ModeldService {
    pub fn new(config: ModeldConfig) -> Self {
        Self {
            config,
            inferences_processed: 0,
            tools_dispatched: 0,
            errors: 0,
            running: false,
        }
    }

    /// Start the service event loop.
    pub fn run(&mut self) {
        self.running = true;
        let mut buf = [0u8; 256];

        loop {
            let result = sys_channel_recv(self.config.listen_channel, &mut buf);
            let Some(msg) = ipc::decode_recv_result(result) else {
                sys_yield();
                continue;
            };

            match msg.tag {
                0x20 => {
                    // MsgTag::InferenceRequest
                    let assembly =
                        self.handle_inference(&buf[..msg.payload_len], msg.reply_endpoint);
                    let mut resp = [0u8; 16];
                    resp[0] = assembly.strategy;
                    resp[1..3].copy_from_slice(&assembly.node_count.to_le_bytes());
                    resp[3..7].copy_from_slice(&assembly.context_bytes.to_le_bytes());
                    resp[7..11].copy_from_slice(&assembly.min_included_score.to_le_bytes());
                    resp[11..15].copy_from_slice(&assembly.max_included_score.to_le_bytes());
                    let mut flags: u8 = 0;
                    if assembly.includes_drift_warnings {
                        flags |= 1;
                    }
                    if assembly.includes_causal_context {
                        flags |= 2;
                    }
                    resp[15] = flags;
                    sys_channel_send(msg.reply_endpoint, &resp, 0x21);
                }
                0x22 => {
                    // MsgTag::ToolRequest
                    let decision =
                        self.handle_tool_request(&buf[..msg.payload_len], msg.reply_endpoint);
                    let mut resp = [0u8; 8];
                    resp[0] = decision.tool_id;
                    resp[1..5].copy_from_slice(&decision.confidence.to_le_bytes());
                    resp[5] = decision.candidates_considered;
                    resp[6] = decision.rank;
                    resp[7] = if decision.causal_influenced { 1 } else { 0 };
                    sys_channel_send(msg.reply_endpoint, &resp, 0x23);
                }
                0x01 => {
                    sys_channel_send(msg.reply_endpoint, &[], 0x02);
                }
                0x04 => {
                    break;
                }
                _ => {
                    self.errors += 1;
                }
            }
        }

        self.running = false;
    }

    /// Handle an inference request.
    /// Payload: strategy(1) + focus_node(8) + relevance_threshold(4) + max_tokens(4) = 17 bytes.
    pub fn handle_inference(&mut self, payload: &[u8], reply_endpoint: u32) -> ContextAssembly {
        self.inferences_processed += 1;
        if payload.len() < 17 {
            return ContextAssembly::empty();
        }

        let strategy = payload[0];
        let focus_node = u64::from_le_bytes([
            payload[1], payload[2], payload[3], payload[4], payload[5], payload[6], payload[7],
            payload[8],
        ]);
        let relevance_threshold =
            u32::from_le_bytes([payload[9], payload[10], payload[11], payload[12]]);

        let assembly = self.assemble_context(strategy, focus_node, relevance_threshold);

        // Record inference in graph via graphd.
        let mut mutation = [0u8; 9];
        mutation[0] = 0; // AddNode
        mutation[1..9].copy_from_slice(&focus_node.to_le_bytes());
        sys_channel_send(self.config.graphd_channel, &mutation, 0x12);

        let _ = reply_endpoint;
        assembly
    }

    /// Handle a tool request.
    /// Payload: tool_id(1) + target_node(8) + context_hint(4) = 13 bytes.
    pub fn handle_tool_request(&mut self, payload: &[u8], reply_endpoint: u32) -> ToolDecision {
        self.tools_dispatched += 1;
        if payload.len() < 13 {
            return ToolDecision {
                tool_id: 0,
                confidence: 0,
                candidates_considered: 0,
                rank: 0,
                causal_influenced: false,
            };
        }

        let tool_id = payload[0];
        let target_node = u64::from_le_bytes([
            payload[1], payload[2], payload[3], payload[4], payload[5], payload[6], payload[7],
            payload[8],
        ]);
        let context_hint = u32::from_le_bytes([payload[9], payload[10], payload[11], payload[12]]);

        let route_channel = match tool_id {
            0 | 1 => self.config.graphd_channel,
            _ => 0,
        };
        if route_channel != 0 {
            let tag = match tool_id {
                0 => 0x10,
                1 => 0x12,
                _ => 0x00,
            };
            sys_channel_send(route_channel, payload, tag);
        }

        let _ = (reply_endpoint, target_node, context_hint);
        let candidates: u8 = if tool_id <= 1 { 2 } else { 1 };
        ToolDecision {
            tool_id,
            confidence: 48000,
            candidates_considered: candidates,
            rank: 1,
            causal_influenced: false,
        }
    }

    /// Assemble graph context for an inference request.
    pub fn assemble_context(
        &self,
        strategy: u8,
        focus_node: u64,
        threshold: u32,
    ) -> ContextAssembly {
        let query_kind: u8 = match strategy {
            0 => 4,
            1 => 5,
            2 => 6,
            3 => 7,
            _ => 4,
        };
        let mut query_buf = [0u8; 13];
        query_buf[0] = query_kind;
        query_buf[1..9].copy_from_slice(&focus_node.to_le_bytes());
        query_buf[9..13].copy_from_slice(&threshold.to_le_bytes());
        sys_channel_send(self.config.graphd_channel, &query_buf, 0x10);

        let includes_drift = strategy == 2 || strategy == 4;
        let includes_causal = strategy == 3 || strategy == 4;

        if strategy == 4 {
            query_buf[0] = 6;
            sys_channel_send(self.config.graphd_channel, &query_buf, 0x10);
            query_buf[0] = 7;
            sys_channel_send(self.config.graphd_channel, &query_buf, 0x10);
        }

        ContextAssembly {
            node_count: 0,
            context_bytes: 0,
            strategy,
            min_included_score: threshold,
            max_included_score: 0,
            includes_drift_warnings: includes_drift,
            includes_causal_context: includes_causal,
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Host-mode syscall shims until ring-3 exists
// ════════════════════════════════════════════════════════════════════

fn sys_channel_recv(channel: u32, buf: &mut [u8]) -> u64 {
    host_sys::channel_recv(channel, buf)
}

fn sys_channel_send(channel: u32, payload: &[u8], tag: u8) {
    host_sys::channel_send(channel, payload, tag);
}

fn sys_yield() {
    host_sys::yield_now();
}

fn main() {
    let mut service = ModeldService::new(ModeldConfig::default());
    service.run();
}
