// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Secret redaction — credential/API-key/token detection via manual pattern matching.
//!
//! No regex crate available in no_std.  All patterns are hand-coded
//! finite-state matchers that recognise common secret formats:
//!
//! 1. **API keys**: hex strings ≥ 32 chars, base64 strings ≥ 40 chars.
//! 2. **Bearer tokens**: "Bearer " prefix + base64url.
//! 3. **AWS keys**: "AKIA" prefix + 16 alphanumeric chars.
//! 4. **GitHub tokens**: "ghp_", "gho_", "ghs_", "ghr_" prefixes + 36 alnum.
//! 5. **Password patterns**: "password=", "passwd=", "secret=", "token=" followed by value.
//! 6. **Connection strings**: keywords like "Password=", "AccountKey=" in connection strings.
//! 7. **Private keys**: "-----BEGIN" PEM headers.
//! 8. **JWT**: three base64url segments separated by dots.
//!
//! ## API
//!
//! `redact(input, output)` copies `input` to `output`, replacing detected
//! secrets with `[REDACTED]`.  Returns the number of bytes written to output
//! and the number of redactions performed.

/// Maximum number of patterns we check.
const MAX_PATTERNS: usize = 16;

/// The replacement string.
const REDACTED: &[u8] = b"[REDACTED]";

/// Result of a redaction pass.
pub struct RedactResult {
    /// Number of bytes written to the output buffer.
    pub output_len: usize,
    /// Number of secret spans redacted.
    pub redaction_count: u32,
}

/// Scan `input` and copy to `output`, replacing detected secrets with `[REDACTED]`.
///
/// `output` must be at least as large as `input` (redacted text is shorter
/// or equal in length).
pub fn redact(input: &[u8], output: &mut [u8]) -> RedactResult {
    let ilen = input.len();
    let olen = output.len();
    let mut ri = 0usize; // read index into input
    let mut wi = 0usize; // write index into output
    let mut redactions = 0u32;

    while ri < ilen && wi < olen {
        // Try each pattern at the current position.
        let mut matched_len = 0usize;

        // Pattern 1: AWS access key (AKIA + 16 alnum)
        if matched_len == 0 {
            matched_len = match_aws_key(input, ri);
        }

        // Pattern 2: GitHub tokens (ghp_, gho_, ghs_, ghr_ + 36 alnum)
        if matched_len == 0 {
            matched_len = match_github_token(input, ri);
        }

        // Pattern 3: "Bearer " + base64url token (at least 20 chars)
        if matched_len == 0 {
            matched_len = match_bearer_token(input, ri);
        }

        // Pattern 4: PEM private key header
        if matched_len == 0 {
            matched_len = match_pem_header(input, ri);
        }

        // Pattern 5: password=/secret=/token= followed by value
        if matched_len == 0 {
            matched_len = match_key_value_secret(input, ri);
        }

        // Pattern 6: Connection string secrets (Password=...; AccountKey=...)
        if matched_len == 0 {
            matched_len = match_connection_string_secret(input, ri);
        }

        // Pattern 7: JWT (three dot-separated base64url segments)
        if matched_len == 0 {
            matched_len = match_jwt(input, ri);
        }

        // Pattern 8: Long hex strings (≥ 32 hex chars in a row)
        if matched_len == 0 {
            matched_len = match_long_hex(input, ri);
        }

        // Pattern 9: Long base64 strings (≥ 40 chars, containing +/= chars)
        if matched_len == 0 {
            matched_len = match_long_base64(input, ri);
        }

        if matched_len > 0 {
            // Replace with [REDACTED].
            let copy_len = REDACTED.len().min(olen - wi);
            let mut j = 0;
            while j < copy_len {
                output[wi + j] = REDACTED[j];
                j += 1;
            }
            wi += copy_len;
            ri += matched_len;
            redactions += 1;
        } else {
            // Copy byte verbatim.
            output[wi] = input[ri];
            wi += 1;
            ri += 1;
        }
    }

    RedactResult {
        output_len: wi,
        redaction_count: redactions,
    }
}

// ────────────────────────────────────────────────────────────────────
// Pattern matchers
// ────────────────────────────────────────────────────────────────────

/// AWS access key: "AKIA" followed by exactly 16 alphanumeric characters.
fn match_aws_key(input: &[u8], pos: usize) -> usize {
    let remaining = input.len() - pos;
    if remaining < 20 {
        return 0;
    }
    if &input[pos..pos + 4] != b"AKIA" {
        return 0;
    }
    let mut i = 4;
    while i < 20 {
        if !is_alnum(input[pos + i]) {
            return 0;
        }
        i += 1;
    }
    // Check that it's a word boundary (not part of a longer alnum sequence that isn't a key).
    if remaining > 20 && is_alnum(input[pos + 20]) {
        return 0;
    }
    20
}

/// GitHub tokens: "ghp_", "gho_", "ghs_", "ghr_" + 36 alphanumeric.
fn match_github_token(input: &[u8], pos: usize) -> usize {
    let remaining = input.len() - pos;
    if remaining < 40 {
        return 0;
    }
    let prefix = &input[pos..pos + 4];
    if prefix != b"ghp_" && prefix != b"gho_" && prefix != b"ghs_" && prefix != b"ghr_" {
        return 0;
    }
    let mut i = 4;
    while i < 40 {
        if !is_alnum(input[pos + i]) {
            return 0;
        }
        i += 1;
    }
    if remaining > 40 && is_alnum(input[pos + 40]) {
        return 0;
    }
    40
}

/// "Bearer " followed by ≥ 20 base64url characters.
fn match_bearer_token(input: &[u8], pos: usize) -> usize {
    let remaining = input.len() - pos;
    if remaining < 27 {
        return 0;
    } // "Bearer " (7) + 20
    if &input[pos..pos + 7] != b"Bearer " {
        return 0;
    }
    let mut end = pos + 7;
    while end < input.len() && is_base64url(input[end]) {
        end += 1;
    }
    let token_len = end - (pos + 7);
    if token_len >= 20 { end - pos } else { 0 }
}

/// PEM header: "-----BEGIN " ... "-----"
fn match_pem_header(input: &[u8], pos: usize) -> usize {
    let remaining = input.len() - pos;
    if remaining < 11 {
        return 0;
    }
    if &input[pos..pos + 11] != b"-----BEGIN " {
        return 0;
    }
    // Find the closing "-----".
    let mut end = pos + 11;
    while end + 5 <= input.len() {
        if &input[end..end + 5] == b"-----" {
            return (end + 5) - pos;
        }
        end += 1;
    }
    0
}

/// Key-value secret: "password=", "passwd=", "secret=", "token=", "api_key=" followed by value.
fn match_key_value_secret(input: &[u8], pos: usize) -> usize {
    let remaining = input.len() - pos;

    let prefixes: [&[u8]; 5] = [b"password=", b"passwd=", b"secret=", b"token=", b"api_key="];

    let mut pi = 0;
    while pi < prefixes.len() {
        let p = prefixes[pi];
        if remaining >= p.len() && ci_eq(&input[pos..pos + p.len()], p) {
            let val_start = pos + p.len();
            let mut end = val_start;
            // Value extends until whitespace, semicolon, or end-of-input.
            while end < input.len()
                && input[end] != b' '
                && input[end] != b'\t'
                && input[end] != b'\n'
                && input[end] != b'\r'
                && input[end] != b';'
                && input[end] != b'&'
            {
                end += 1;
            }
            let val_len = end - val_start;
            if val_len >= 4 {
                // Redact the value but keep the key.
                // Actually, redact the whole thing including key for safety.
                return end - pos;
            }
        }
        pi += 1;
    }
    0
}

/// Connection string secrets: "Password=...", "AccountKey=...", "SharedAccessKey=..."
fn match_connection_string_secret(input: &[u8], pos: usize) -> usize {
    let remaining = input.len() - pos;

    let prefixes: [&[u8]; 3] = [b"Password=", b"AccountKey=", b"SharedAccessKey="];

    let mut pi = 0;
    while pi < prefixes.len() {
        let p = prefixes[pi];
        if remaining >= p.len() && ci_eq(&input[pos..pos + p.len()], p) {
            let val_start = pos + p.len();
            let mut end = val_start;
            while end < input.len() && input[end] != b';' && input[end] != b'\n' {
                end += 1;
            }
            if end > val_start {
                return end - pos;
            }
        }
        pi += 1;
    }
    0
}

/// JWT: header.payload.signature — three base64url segments joined by dots.
fn match_jwt(input: &[u8], pos: usize) -> usize {
    // Must start with eyJ (base64 of '{"') to catch JWTs specifically.
    let remaining = input.len() - pos;
    if remaining < 10 {
        return 0;
    }
    if &input[pos..pos + 3] != b"eyJ" {
        return 0;
    }

    let mut dots = 0u32;
    let mut end = pos;
    while end < input.len() {
        let b = input[end];
        if b == b'.' {
            dots += 1;
            if dots > 2 {
                break;
            }
        } else if is_base64url(b) {
            // ok
        } else {
            break;
        }
        end += 1;
    }
    if dots == 2 && (end - pos) >= 20 {
        end - pos
    } else {
        0
    }
}

/// Long hex string: ≥ 32 contiguous hex characters (likely a key or hash).
fn match_long_hex(input: &[u8], pos: usize) -> usize {
    let mut end = pos;
    while end < input.len() && is_hex(input[end]) {
        end += 1;
    }
    let len = end - pos;
    if len >= 32 { len } else { 0 }
}

/// Long base64 string: ≥ 40 contiguous base64 chars including at least one
/// of '+', '/', or '=' (to distinguish from plain text).
fn match_long_base64(input: &[u8], pos: usize) -> usize {
    let mut end = pos;
    let mut has_b64_char = false;
    while end < input.len() && is_base64(input[end]) {
        if input[end] == b'+' || input[end] == b'/' || input[end] == b'=' {
            has_b64_char = true;
        }
        end += 1;
    }
    let len = end - pos;
    if len >= 40 && has_b64_char { len } else { 0 }
}

// ────────────────────────────────────────────────────────────────────
// Character class helpers
// ────────────────────────────────────────────────────────────────────

fn is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

fn is_base64url(b: u8) -> bool {
    is_alnum(b) || b == b'-' || b == b'_' || b == b'.' || b == b'='
}

fn is_base64(b: u8) -> bool {
    is_alnum(b) || b == b'+' || b == b'/' || b == b'='
}

/// Case-insensitive comparison for ASCII bytes.
fn ci_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        let ca = if a[i] >= b'A' && a[i] <= b'Z' {
            a[i] + 32
        } else {
            a[i]
        };
        let cb = if b[i] >= b'A' && b[i] <= b'Z' {
            b[i] + 32
        } else {
            b[i]
        };
        if ca != cb {
            return false;
        }
        i += 1;
    }
    true
}
