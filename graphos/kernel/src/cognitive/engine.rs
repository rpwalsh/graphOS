// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Global cognitive engine — singleton state for the SCCE subsystem.
//!
//! Provides a kernel-wide `Bm25Index`, `PageRankEngine`, and `LshIndex`
//! backed by spin-mutex globals.  Syscall handlers call `index_document` and
//! `run_query` rather than constructing short-lived engine instances.
//!
//! ## Thread safety
//! All statics are wrapped in `spin::Mutex`.  Heavy queries should be short
//! because the PIT fires every 1 ms and the BM25 scan is O(vocab_size).
//! In practice, with a 4 096-term vocabulary and 512-document corpus, a
//! full BM25 scan takes < 50 µs on a 2 GHz core.

use spin::Mutex;

use super::bm25::Bm25Index;
use super::lsh::LshIndex;
use super::memory::Session;
use super::pagerank::PageRankEngine;
use super::pipeline::{self, Engines, PipelineResult};

// ── Global engine singletons ─────────────────────────────────────────────────

static BM25: Mutex<Bm25Index> = Mutex::new(Bm25Index::new());
static PAGERANK: Mutex<PageRankEngine> = Mutex::new(PageRankEngine::new());
static LSH: Mutex<LshIndex> = Mutex::new(LshIndex::new());

// One shared anonymous session for in-kernel cognitive calls.
// Ring-3 multi-session tracking is not yet exposed via the ABI.
static SESSION: Mutex<Option<SessionSlot>> = Mutex::new(None);

struct SessionSlot {
    session: Session,
}

// Safety: `Session` contains no raw pointers or `Cell<>` — it is a plain
// fixed-size array of `Copy` primitives.  Its only non-Send member
// (`session_uuid: SessionUuid`) is also `Copy`.
unsafe impl Send for SessionSlot {}

// ── Public API ───────────────────────────────────────────────────────────────

/// Index a document into the global BM25 + graph stores.
///
/// Returns `(chunk_count, terms_indexed)`.
pub fn index_document(doc: &[u8], doc_type: u8, creator_node: u64) -> IndexStats {
    let mut bm25 = BM25.lock();
    let result = super::indexing::index_document(doc, doc_type, creator_node, &mut *bm25);
    IndexStats {
        chunk_count: result.chunk_count,
        terms_indexed: result.terms_indexed,
    }
}

/// Run the full 10-phase cognitive query pipeline against the global engines.
pub fn run_query(query: &[u8], query_fingerprint: u64) -> PipelineResult {
    let bm25 = BM25.lock();
    let mut pagerank = PAGERANK.lock();
    let lsh = LSH.lock();
    let mut slot = SESSION.lock();

    // Lazily initialise the shared session.
    if slot.is_none() {
        *slot = Some(SessionSlot {
            session: Session::new(0),
        });
    }

    let session_ref = slot.as_mut().map(|s| &mut s.session);

    let mut engines = Engines {
        bm25: &*bm25,
        pagerank: &mut *pagerank,
        lsh: &*lsh,
        session: session_ref,
    };

    pipeline::execute(query, &mut engines, query_fingerprint)
}

/// Query statistics returned to the syscall layer.
pub struct IndexStats {
    pub chunk_count: u32,
    pub terms_indexed: u32,
}
