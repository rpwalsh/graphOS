// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Conversation memory — multi-turn context tracking.
//!
//! Fixed-size ring buffer of conversation turns with recency-weighted
//! retrieval.  Each turn stores a compact representation (hash signature,
//! token count, topic fingerprint, timestamp) rather than raw text.
//!
//! ## Design
//!
//! The kernel doesn't store raw conversation text (that's a userspace
//! concern).  Instead, this module tracks:
//!
//! 1. **Turn metadata**: topic fingerprint (SimHash), token count, timestamp.
//! 2. **Topic drift**: cosine similarity between consecutive turns via
//!    Hamming distance on SimHash fingerprints.
//! 3. **Context window**: which prior turns are relevant to the current
//!    query, based on recency decay and topic similarity.
//! 4. **Entity mentions**: per-turn entity ID set (compact bloom filter).
//!
//! ## Capacity
//! - MAX_TURNS = 256 turns in the ring buffer.
//! - MAX_ENTITIES_PER_TURN = 32 entity IDs tracked per turn.
//! - MAX_SESSIONS = 16 concurrent conversation sessions.

use crate::graph::types::{Timestamp, Weight};
use crate::uuid::SessionUuid;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

const MAX_TURNS: usize = 256;
const MAX_ENTITIES_PER_TURN: usize = 32;
const MAX_SESSIONS: usize = 16;
const FP_ONE: u32 = 1 << 16;

/// Recency decay half-life in turns.  After HALF_LIFE turns, a turn's
/// relevance weight is halved.  λ = ln(2)/HALF_LIFE ≈ 0.0866 in 16.16 = 5677.
const DECAY_LAMBDA: u32 = 5677;

// ────────────────────────────────────────────────────────────────────
// Turn record
// ────────────────────────────────────────────────────────────────────

/// A single conversation turn's metadata.
#[derive(Clone, Copy)]
pub struct Turn {
    /// Timestamp (graph generation at time of turn).
    pub timestamp: Timestamp,
    /// SimHash fingerprint of the turn's content.
    pub fingerprint: u64,
    /// Number of tokens in the turn.
    pub token_count: u16,
    /// Speaker: 0 = user, 1 = system, 2+ = other.
    pub speaker: u8,
    /// Turn flags (reserved).
    pub flags: u8,
    /// Entity IDs mentioned in this turn (0 = unused slot).
    pub entities: [u32; MAX_ENTITIES_PER_TURN],
    /// Number of valid entity entries.
    pub entity_count: u8,
    /// Padding for alignment.
    pub(crate) _pad: [u8; 3],
}

impl Turn {
    pub const EMPTY: Self = Self {
        timestamp: 0,
        fingerprint: 0,
        token_count: 0,
        speaker: 0,
        flags: 0,
        entities: [0u32; MAX_ENTITIES_PER_TURN],
        entity_count: 0,
        _pad: [0; 3],
    };

    pub fn is_valid(&self) -> bool {
        self.timestamp > 0
    }
}

// ────────────────────────────────────────────────────────────────────
// Session
// ────────────────────────────────────────────────────────────────────

/// A conversation session — ring buffer of turns.
pub struct Session {
    turns: [Turn; MAX_TURNS],
    write_idx: usize,
    total_turns: u64,
    /// Session ID (caller-assigned).
    pub session_id: u32,

    /// Canonical typed UUID identity for this session.
    pub session_uuid: SessionUuid,
}

impl Session {
    pub fn new(session_id: u32) -> Self {
        Self {
            turns: [Turn::EMPTY; MAX_TURNS],
            write_idx: 0,
            total_turns: 0,
            session_id,
            session_uuid: SessionUuid::from_session_id(session_id),
        }
    }

    /// Record a new turn.
    pub fn add_turn(&mut self, turn: Turn) {
        self.turns[self.write_idx] = turn;
        self.write_idx = (self.write_idx + 1) % MAX_TURNS;
        self.total_turns += 1;
    }

    /// Get the most recent turn.
    pub fn latest(&self) -> Option<&Turn> {
        if self.total_turns == 0 {
            return None;
        }
        let idx = if self.write_idx == 0 {
            MAX_TURNS - 1
        } else {
            self.write_idx - 1
        };
        let t = &self.turns[idx];
        if t.is_valid() { Some(t) } else { None }
    }

    /// Get a turn at offset from the most recent (0 = most recent).
    pub fn turn_at_offset(&self, offset: usize) -> Option<&Turn> {
        if offset >= MAX_TURNS || (self.total_turns as usize) <= offset {
            return None;
        }
        let idx = if self.write_idx > offset {
            self.write_idx - 1 - offset
        } else {
            MAX_TURNS - 1 - (offset - self.write_idx)
        };
        let t = &self.turns[idx];
        if t.is_valid() { Some(t) } else { None }
    }

    /// Compute topic drift between the two most recent turns.
    /// Returns Hamming distance (0 = identical topic, 64 = maximally different).
    pub fn topic_drift(&self) -> Option<u32> {
        let latest = self.turn_at_offset(0)?;
        let prev = self.turn_at_offset(1)?;
        Some((latest.fingerprint ^ prev.fingerprint).count_ones())
    }

    /// Find the `k` most relevant prior turns for a query fingerprint.
    ///
    /// Relevance = topic_similarity × recency_weight.
    /// - topic_similarity = 1.0 − hamming_distance/64 (in 16.16).
    /// - recency_weight = exp(−λ · offset) ≈ 1.0 − λ·offset (linear approx for small λ).
    ///
    /// `out` is filled with (turn_offset, relevance_score) pairs.
    /// Returns number of results.
    pub fn relevant_context(&self, query_fingerprint: u64, out: &mut [(usize, Weight)]) -> usize {
        let k = out.len();
        let available = core::cmp::min(self.total_turns as usize, MAX_TURNS);
        if available == 0 || k == 0 {
            return 0;
        }

        // Score all available turns.
        let mut scores = [(0usize, 0u32); MAX_TURNS];
        let mut scored = 0usize;

        let mut offset = 0;
        while offset < available {
            if let Some(turn) = self.turn_at_offset(offset) {
                let ham = (query_fingerprint ^ turn.fingerprint).count_ones();
                // similarity = (64 - ham) / 64 in 16.16
                let sim = ((64u32.saturating_sub(ham)) << 16) / 64;

                // recency = max(0, 1.0 - λ * offset)
                let decay = (DECAY_LAMBDA as u64 * offset as u64) >> 16;
                let recency = FP_ONE.saturating_sub(decay as u32);

                let relevance = fp_mul(sim, recency);
                scores[scored] = (offset, relevance);
                scored += 1;
            }
            offset += 1;
        }

        // Extract top-k.
        let mut written = 0;
        let mut used = [false; MAX_TURNS];
        while written < k && written < scored {
            let mut best = usize::MAX;
            let mut best_score = 0u32;
            let mut i = 0;
            while i < scored {
                if !used[i] && scores[i].1 > best_score {
                    best_score = scores[i].1;
                    best = i;
                }
                i += 1;
            }
            if best == usize::MAX {
                break;
            }
            used[best] = true;
            out[written] = scores[best];
            written += 1;
        }
        written
    }

    /// Check if an entity was mentioned in any of the last `window` turns.
    pub fn entity_mentioned_recently(&self, entity_id: u32, window: usize) -> bool {
        let available = core::cmp::min(self.total_turns as usize, MAX_TURNS);
        let check = core::cmp::min(window, available);
        let mut offset = 0;
        while offset < check {
            if let Some(turn) = self.turn_at_offset(offset) {
                let mut j = 0;
                while j < turn.entity_count as usize {
                    if turn.entities[j] == entity_id {
                        return true;
                    }
                    j += 1;
                }
            }
            offset += 1;
        }
        false
    }

    pub fn total_turns(&self) -> u64 {
        self.total_turns
    }
}

fn fp_mul(a: u32, b: u32) -> u32 {
    ((a as u64 * b as u64) >> 16) as u32
}

// ────────────────────────────────────────────────────────────────────
// Session table
// ────────────────────────────────────────────────────────────────────

/// Global session table.
pub struct SessionTable {
    sessions: [Option<Session>; MAX_SESSIONS],
    count: usize,
}

impl SessionTable {
    pub const fn new() -> Self {
        const NONE: Option<Session> = None;
        Self {
            sessions: [NONE; MAX_SESSIONS],
            count: 0,
        }
    }

    /// Create a new session.  Returns the slot index, or None if full.
    pub fn create(&mut self, session_id: u32) -> Option<usize> {
        let mut i = 0;
        while i < MAX_SESSIONS {
            if self.sessions[i].is_none() {
                self.sessions[i] = Some(Session::new(session_id));
                self.count += 1;
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Get a mutable reference to session at slot index.
    pub fn get_mut(&mut self, slot: usize) -> Option<&mut Session> {
        if slot < MAX_SESSIONS {
            self.sessions[slot].as_mut()
        } else {
            None
        }
    }

    /// Get an immutable reference to session at slot index.
    pub fn get(&self, slot: usize) -> Option<&Session> {
        if slot < MAX_SESSIONS {
            self.sessions[slot].as_ref()
        } else {
            None
        }
    }

    /// Find session by ID.
    pub fn find_by_id(&self, session_id: u32) -> Option<usize> {
        let mut i = 0;
        while i < MAX_SESSIONS {
            if let Some(ref s) = self.sessions[i]
                && s.session_id == session_id
            {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Destroy a session.
    pub fn destroy(&mut self, slot: usize) {
        if slot < MAX_SESSIONS && self.sessions[slot].is_some() {
            self.sessions[slot] = None;
            self.count -= 1;
        }
    }

    pub fn active_count(&self) -> usize {
        self.count
    }
}
