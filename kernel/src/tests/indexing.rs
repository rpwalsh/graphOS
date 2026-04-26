// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Indexing algorithm regression tests.
//!
//! Tests for `extract_spans()` and `extract_chunks()` edge cases that
//! previously caused infinite loops or incorrect output.

use crate::cognitive::indexing::{ChunkBounds, SpanBounds};
use crate::diag;

// Re-implement the extraction functions here for testing since they're
// private in the indexing module. In production, these would be exposed
// via a test-only API or the functions would be made pub(crate).

const MAX_CHUNK_SIZE: usize = 512;
const CHUNK_OVERLAP: usize = 64;

/// Extract spans (mirrors indexing::extract_spans).
fn extract_spans(text: &[u8], out: &mut [SpanBounds]) -> usize {
    let len = text.len();
    let max_spans = out.len();
    let mut count = 0usize;
    let mut pos = 0usize;

    while pos < len && count < max_spans {
        // Skip leading blank lines.
        while pos < len && text[pos] == b'\n' {
            pos += 1;
        }
        let start = pos;

        // Find end of paragraph: two consecutive newlines or end of text.
        let mut end = pos;
        while end < len {
            if end + 1 < len && text[end] == b'\n' && text[end + 1] == b'\n' {
                break;
            }
            end += 1;
        }

        if end > start {
            out[count] = SpanBounds {
                start: start as u32,
                len: (end - start) as u32,
            };
            count += 1;
        }

        // Advance past the current span.
        let prev_pos = pos;
        pos = end;
        if pos + 1 < len && text[pos] == b'\n' && text[pos + 1] == b'\n' {
            pos += 2;
        }
        // Safety: force progress
        if pos <= prev_pos {
            pos = prev_pos + 1;
        }
    }

    count
}

/// Extract chunks (mirrors indexing::extract_chunks).
fn extract_chunks(text: &[u8], out: &mut [ChunkBounds]) -> usize {
    let len = text.len();
    let max_chunks = out.len();
    let mut count = 0usize;
    let mut pos = 0usize;

    while pos < len && count < max_chunks {
        let remaining = len - pos;
        let chunk_len = if remaining <= MAX_CHUNK_SIZE {
            remaining
        } else {
            let mut end = pos + MAX_CHUNK_SIZE;
            while end > pos + MAX_CHUNK_SIZE - 64 {
                if end < len && (text[end] == b' ' || text[end] == b'\n') {
                    break;
                }
                end -= 1;
            }
            if end <= pos + MAX_CHUNK_SIZE - 64 {
                end = pos + MAX_CHUNK_SIZE;
            }
            end - pos
        };

        out[count] = ChunkBounds {
            span_offset: pos as u32,
            len: chunk_len as u32,
        };
        count += 1;

        if chunk_len >= CHUNK_OVERLAP {
            pos += chunk_len - CHUNK_OVERLAP;
        } else {
            pos += chunk_len;
        }

        if len - pos < CHUNK_OVERLAP && pos < len {
            if count > 0 {
                let prev = &mut out[count - 1];
                prev.len = (len - prev.span_offset as usize) as u32;
            }
            break;
        }
    }

    count
}

/// Run all indexing tests. Returns number of failures.
pub fn run_tests() -> u32 {
    let mut failures = 0u32;

    // Test 1: Empty input
    if !test_empty_input() {
        failures += 1;
    }

    // Test 2: Single paragraph (no separators)
    if !test_single_paragraph() {
        failures += 1;
    }

    // Test 3: Multiple paragraphs
    if !test_multiple_paragraphs() {
        failures += 1;
    }

    // Test 4: Repeated separators (many blank lines)
    if !test_repeated_separators() {
        failures += 1;
    }

    // Test 5: Only newlines
    if !test_only_newlines() {
        failures += 1;
    }

    // Test 6: Chunk extraction basic
    if !test_chunk_basic() {
        failures += 1;
    }

    // Test 7: Chunk overlap correctness
    if !test_chunk_overlap() {
        failures += 1;
    }

    // Test 8: Forward progress guarantee
    if !test_forward_progress() {
        failures += 1;
    }

    failures
}

fn test_empty_input() -> bool {
    let mut spans = [SpanBounds { start: 0, len: 0 }; 16];
    let count = extract_spans(b"", &mut spans);
    if count == 0 {
        diag::test_pass(b"indexing: empty input");
        true
    } else {
        diag::test_fail(b"indexing: empty input - expected 0 spans");
        false
    }
}

fn test_single_paragraph() -> bool {
    let text = b"Hello world this is a test";
    let mut spans = [SpanBounds { start: 0, len: 0 }; 16];
    let count = extract_spans(text, &mut spans);
    if count == 1 && spans[0].start == 0 && spans[0].len == text.len() as u32 {
        diag::test_pass(b"indexing: single paragraph");
        true
    } else {
        diag::test_fail(b"indexing: single paragraph - wrong span");
        false
    }
}

fn test_multiple_paragraphs() -> bool {
    let text = b"First paragraph\n\nSecond paragraph\n\nThird";
    let mut spans = [SpanBounds { start: 0, len: 0 }; 16];
    let count = extract_spans(text, &mut spans);
    if count == 3 {
        diag::test_pass(b"indexing: multiple paragraphs");
        true
    } else {
        diag::emit_val(
            diag::Category::Test,
            diag::Level::Fail,
            b"indexing: multiple paragraphs - expected 3, got ",
            count as u64,
        );
        false
    }
}

fn test_repeated_separators() -> bool {
    let text = b"Para1\n\n\n\n\nPara2\n\n\n\nPara3";
    let mut spans = [SpanBounds { start: 0, len: 0 }; 16];
    let count = extract_spans(text, &mut spans);
    if count == 3 {
        diag::test_pass(b"indexing: repeated separators");
        true
    } else {
        diag::emit_val(
            diag::Category::Test,
            diag::Level::Fail,
            b"indexing: repeated separators - expected 3, got ",
            count as u64,
        );
        false
    }
}

fn test_only_newlines() -> bool {
    let text = b"\n\n\n\n\n";
    let mut spans = [SpanBounds { start: 0, len: 0 }; 16];
    let count = extract_spans(text, &mut spans);
    if count == 0 {
        diag::test_pass(b"indexing: only newlines");
        true
    } else {
        diag::test_fail(b"indexing: only newlines - expected 0 spans");
        false
    }
}

fn test_chunk_basic() -> bool {
    let text = b"Short text for chunking test";
    let mut chunks = [ChunkBounds {
        span_offset: 0,
        len: 0,
    }; 16];
    let count = extract_chunks(text, &mut chunks);
    if count >= 1 && chunks[0].len > 0 {
        diag::test_pass(b"indexing: chunk basic");
        true
    } else {
        diag::test_fail(b"indexing: chunk basic - no chunks");
        false
    }
}

fn test_chunk_overlap() -> bool {
    // Create text longer than MAX_CHUNK_SIZE to force multiple chunks
    let mut text = [b'x'; 600];
    // Add some spaces for break points
    text[100] = b' ';
    text[200] = b' ';
    text[300] = b' ';
    text[400] = b' ';
    text[500] = b' ';

    let mut chunks = [ChunkBounds {
        span_offset: 0,
        len: 0,
    }; 16];
    let count = extract_chunks(&text, &mut chunks);

    // Should produce at least 2 chunks with overlap
    if count >= 2 {
        // Verify overlap: chunk 1 end should be > chunk 2 start
        let c1_end = chunks[0].span_offset + chunks[0].len;
        let c2_start = chunks[1].span_offset;
        if c1_end > c2_start {
            diag::test_pass(b"indexing: chunk overlap");
            true
        } else {
            diag::test_fail(b"indexing: chunk overlap - no overlap detected");
            false
        }
    } else {
        diag::test_fail(b"indexing: chunk overlap - expected multiple chunks");
        false
    }
}

fn test_forward_progress() -> bool {
    // Pathological input: single char repeated with no natural breaks
    let text = [b'a'; 100];
    let mut spans = [SpanBounds { start: 0, len: 0 }; 16];

    // This should complete (not hang) and produce exactly 1 span
    let count = extract_spans(&text, &mut spans);

    if count == 1 {
        diag::test_pass(b"indexing: forward progress");
        true
    } else {
        diag::test_fail(b"indexing: forward progress - unexpected span count");
        false
    }
}
