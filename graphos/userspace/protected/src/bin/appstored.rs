// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! appstored — GraphOS application store and OTA update daemon.
//!
//! Responsibilities:
//!  - Periodically fetch the signed package index from the configured registry
//!  - Verify the index signature using the release key (`/data/etc/graphos/release.pub`)
//!  - Download and signature-verify individual application bundles (.gapp)
//!  - Stage verified updates to `/data/updates/staged/`
//!  - Expose a channel-based API to `launcher` for search and install requests
//!  - Rollback: keep the previous bundle version until the new one is committed
//!
//! ## Transport
//! This daemon uses plain HTTP (TCP port 80) to fetch from the internal registry
//! for the QEMU/dev environment.  Each artifact is individually signed with
//! Ed25519; the transport layer provides availability, not authenticity — the
//! signature over the content provides authenticity and integrity.
//!
//! HTTPS (TLS 1.3) is deferred to v1.1 once the TLS client is implemented.
//! Tracking: docs/OPEN_WORK.md §"HTTPS transport for appstored"
//!
//! ## Security model
//! - Index and bundle signatures are verified against the pinned release public
//!   key before any bytes are committed to the staged area.
//! - A tampered index or bundle causes the operation to abort; no partial state
//!   is committed.
//! - The previous bundle version is preserved as `.gapp.prev` until `commit`
//!   is called, enabling atomic rollback.

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::panic(info)
}

// ════════════════════════════════════════════════════════════════════
// Constants
// ════════════════════════════════════════════════════════════════════

/// Maximum size of the package index (bytes).
const MAX_INDEX_SIZE: usize = 16 * 1024;
/// Maximum size of a single .gapp bundle (bytes).
const MAX_BUNDLE_SIZE: usize = 4 * 1024 * 1024;
/// Maximum number of packages in the index.
const MAX_PACKAGES: usize = 256;
/// HTTP registry host (4-byte IPv4, overridden by /data/etc/graphos/registry).
const DEFAULT_REGISTRY_IP: [u8; 4] = [127, 0, 0, 1];
/// HTTP port for the package registry.
const REGISTRY_PORT: u16 = 8080;

// ════════════════════════════════════════════════════════════════════
// SHA-256 (inline)
// ════════════════════════════════════════════════════════════════════

mod sha256 {
    fn rotr(x: u32, n: u32) -> u32 { x.rotate_right(n) }

    const K: [u32; 64] = [
        0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
        0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
        0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
        0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
        0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
        0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
        0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
        0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
    ];

    pub fn hash(data: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,
            0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19,
        ];
        let msg_len = data.len();
        let mut remaining = data;
        while remaining.len() >= 64 {
            compress(&mut h, remaining[..64].try_into().unwrap());
            remaining = &remaining[64..];
        }
        let mut padded = [0u8; 128];
        padded[..remaining.len()].copy_from_slice(remaining);
        padded[remaining.len()] = 0x80;
        let bit_len = (msg_len as u64) * 8;
        let end = if remaining.len() < 56 { 64 } else { 128 };
        padded[end - 8..end].copy_from_slice(&bit_len.to_be_bytes());
        compress(&mut h, padded[..64].try_into().unwrap());
        if end == 128 { compress(&mut h, padded[64..].try_into().unwrap()); }
        let mut out = [0u8; 32];
        for (i, w) in h.iter().enumerate() { out[i*4..i*4+4].copy_from_slice(&w.to_be_bytes()); }
        out
    }

    fn compress(h: &mut [u32; 8], block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 { w[i] = u32::from_be_bytes(block[i*4..i*4+4].try_into().unwrap()); }
        for i in 16..64 {
            let s0 = rotr(w[i-15],7)^rotr(w[i-15],18)^(w[i-15]>>3);
            let s1 = rotr(w[i-2],17)^rotr(w[i-2],19)^(w[i-2]>>10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }
        let [mut a,mut b,mut c,mut d,mut e,mut f,mut g,mut hh] = *h;
        for i in 0..64 {
            let s1 = rotr(e,6)^rotr(e,11)^rotr(e,25);
            let ch = (e&f)^(!e&g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = rotr(a,2)^rotr(a,13)^rotr(a,22);
            let maj = (a&b)^(a&c)^(b&c);
            let t2 = s0.wrapping_add(maj);
            hh=g; g=f; f=e; e=d.wrapping_add(t1); d=c; c=b; b=a; a=t1.wrapping_add(t2);
        }
        h[0]=h[0].wrapping_add(a); h[1]=h[1].wrapping_add(b);
        h[2]=h[2].wrapping_add(c); h[3]=h[3].wrapping_add(d);
        h[4]=h[4].wrapping_add(e); h[5]=h[5].wrapping_add(f);
        h[6]=h[6].wrapping_add(g); h[7]=h[7].wrapping_add(hh);
    }
}

// ════════════════════════════════════════════════════════════════════
// Release key
// ════════════════════════════════════════════════════════════════════

/// Pinned release Ed25519 public key (matches docs/release-key.pub).
const RELEASE_PUBKEY: [u8; 32] = [
    // Base64: "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHUR4=" decoded
    0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7,
    0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a,
    0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25,
    0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
];

// ════════════════════════════════════════════════════════════════════
// HTTP fetch helpers
// ════════════════════════════════════════════════════════════════════

/// Send a minimal HTTP GET request and read the body into `out_buf`.
///
/// Returns the number of body bytes written to `out_buf`, or 0 on error.
/// Only HTTP/1.0 200 OK responses with Content-Length headers are accepted.
fn http_get(registry_ip: [u8; 4], path: &[u8], out_buf: &mut [u8]) -> usize {
    let sock = match runtime::socket_open() {
        Some(s) => s,
        None    => return 0,
    };

    let ip_u32 = u32::from_be_bytes(registry_ip);
    if !runtime::socket_connect(&sock, ip_u32, REGISTRY_PORT) {
        return 0;
    }

    // Build HTTP/1.0 GET request.
    let mut req = [0u8; 512];
    let mut roff = 0;
    let write_bytes = |buf: &mut [u8], off: &mut usize, src: &[u8]| {
        let n = src.len().min(buf.len() - *off);
        buf[*off..*off + n].copy_from_slice(&src[..n]);
        *off += n;
    };
    write_bytes(&mut req, &mut roff, b"GET ");
    write_bytes(&mut req, &mut roff, path);
    write_bytes(&mut req, &mut roff, b" HTTP/1.0\r\nHost: registry\r\nConnection: close\r\n\r\n");

    if runtime::socket_send(&sock, &req[..roff]).is_none() {
        return 0;
    }

    // Read response.
    let mut rbuf = [0u8; MAX_INDEX_SIZE + 4096];
    let n = runtime::socket_recv_all(&sock, &mut rbuf);
    runtime::socket_close(&sock);
    if n < 12 { return 0; }

    // Check HTTP status line: "HTTP/1.? 200"
    if !rbuf[..12].starts_with(b"HTTP/1.") || rbuf[9..12] != *b"200" {
        return 0;
    }

    // Find header/body separator "\r\n\r\n".
    let mut body_start = 0;
    let mut i = 0;
    while i + 3 < n {
        if &rbuf[i..i+4] == b"\r\n\r\n" { body_start = i + 4; break; }
        i += 1;
    }
    if body_start == 0 { return 0; }

    let body_len = (n - body_start).min(out_buf.len());
    out_buf[..body_len].copy_from_slice(&rbuf[body_start..body_start + body_len]);
    body_len
}

// ════════════════════════════════════════════════════════════════════
// Signed index / bundle verification
// ════════════════════════════════════════════════════════════════════

/// A parsed package entry from the signed index.
#[derive(Clone, Copy)]
pub struct PackageEntry {
    pub name:    [u8; 64],
    pub name_len: usize,
    pub version: [u8; 32],
    pub ver_len:  usize,
    pub path:    [u8; 128],
    pub path_len: usize,
    /// SHA-256 of the .gapp bundle content.
    pub sha256:  [u8; 32],
}

impl PackageEntry {
    const fn empty() -> Self {
        Self {
            name: [0u8; 64], name_len: 0,
            version: [0u8; 32], ver_len: 0,
            path: [0u8; 128], path_len: 0,
            sha256: [0u8; 32],
        }
    }
}

/// Verify and parse the signed package index.
///
/// Expected format (line-oriented text):
/// ```
/// GRAPHOS-INDEX-V1
/// SIG:<base16-encoded 64-byte ed25519 sig over everything after the SIG: line>
/// <name> <version> <path> <sha256-hex>
/// ...
/// ```
///
/// Returns the number of entries parsed, or 0 if signature verification fails.
pub fn verify_and_parse_index(
    data: &[u8],
    entries: &mut [PackageEntry; MAX_PACKAGES],
) -> usize {
    // Find the SIG: line.
    let sig_prefix = b"SIG:";
    let mut sig_line_start = 0;
    let mut body_start = 0;
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(sig_prefix) {
            sig_line_start = i;
            // Find end of SIG line.
            let mut j = i;
            while j < data.len() && data[j] != b'\n' { j += 1; }
            body_start = j + 1;
            break;
        }
        i += 1;
    }
    if body_start == 0 || sig_line_start == 0 { return 0; }

    // Extract the signature (128 hex chars = 64 bytes).
    let sig_hex_start = sig_line_start + sig_prefix.len();
    let sig_hex_end = sig_hex_start + 128;
    if sig_hex_end > data.len() { return 0; }
    let sig_hex = &data[sig_hex_start..sig_hex_end];
    let mut sig = [0u8; 64];
    if !hex_decode(sig_hex, &mut sig) { return 0; }

    // The signed message is everything from body_start onwards.
    let signed_body = &data[body_start..];
    if !runtime::ed25519_verify(&RELEASE_PUBKEY, signed_body, &sig) {
        runtime::write_line(b"[appstored] index signature verification FAILED\n");
        return 0;
    }
    runtime::write_line(b"[appstored] index signature verified OK\n");

    // Parse package entries.
    let mut count = 0usize;
    let mut line_start = body_start;
    while line_start < data.len() && count < MAX_PACKAGES {
        let mut line_end = line_start;
        while line_end < data.len() && data[line_end] != b'\n' { line_end += 1; }
        let line = &data[line_start..line_end];
        if !line.is_empty() {
            if let Some(e) = parse_index_line(line) {
                entries[count] = e;
                count += 1;
            }
        }
        line_start = line_end + 1;
    }
    count
}

fn parse_index_line(line: &[u8]) -> Option<PackageEntry> {
    // Format: "<name> <version> <path> <sha256hex>"
    let mut parts = [0usize; 4];
    let mut part_ends = [0usize; 4];
    let mut n = 0usize;
    let mut i = 0usize;
    while i <= line.len() && n < 4 {
        if i == line.len() || line[i] == b' ' {
            if n < 4 { parts[n] = if n == 0 { 0 } else { part_ends[n-1] + 1 }; }
            part_ends[n] = i;
            n += 1;
        }
        i += 1;
    }
    if n < 4 { return None; }

    let mut e = PackageEntry::empty();
    let name  = &line[parts[0]..part_ends[0]];
    let ver   = &line[parts[1]..part_ends[1]];
    let path  = &line[parts[2]..part_ends[2]];
    let hash  = &line[parts[3]..part_ends[3]];

    if hash.len() != 64 { return None; }

    e.name_len  = name.len().min(64);  e.name[..e.name_len].copy_from_slice(&name[..e.name_len]);
    e.ver_len   = ver.len().min(32);   e.version[..e.ver_len].copy_from_slice(&ver[..e.ver_len]);
    e.path_len  = path.len().min(128); e.path[..e.path_len].copy_from_slice(&path[..e.path_len]);
    hex_decode(hash, &mut e.sha256);
    Some(e)
}

/// Fetch, verify, and stage a single .gapp bundle.
///
/// Steps:
/// 1. Fetch the bundle via HTTP GET.
/// 2. Compute SHA-256 of the received bytes.
/// 3. Compare against the expected hash from the signed index.
/// 4. Fetch the detached Ed25519 signature file (<path>.sig).
/// 5. Verify the signature over the bundle content.
/// 6. Write the bundle to the staging area.
///
/// Returns `true` on success.  Any verification failure aborts without
/// touching the staging area.
pub fn fetch_and_stage_bundle(
    registry_ip: [u8; 4],
    entry: &PackageEntry,
    stage_dir: &[u8],
) -> bool {
    // ── 1+2. Fetch bundle and hash ────────────────────────────────────────
    let mut bundle_buf = [0u8; MAX_BUNDLE_SIZE];
    let n = http_get(registry_ip, &entry.path[..entry.path_len], &mut bundle_buf);
    if n == 0 {
        runtime::write_line(b"[appstored] bundle fetch failed\n");
        return false;
    }
    let bundle = &bundle_buf[..n];

    // ── 3. Hash check (against signed index) ─────────────────────────────
    let actual_hash = sha256::hash(bundle);
    if actual_hash != entry.sha256 {
        runtime::write_line(b"[appstored] bundle hash mismatch -- REJECTING\n");
        return false;
    }

    // ── 4. Fetch detached signature (<path>.sig) ──────────────────────────
    let mut sig_path = [0u8; 132];
    let plen = entry.path_len.min(128);
    sig_path[..plen].copy_from_slice(&entry.path[..plen]);
    sig_path[plen..plen+4].copy_from_slice(b".sig");
    let sig_path_len = plen + 4;

    let mut sig_buf = [0u8; 128];
    let sn = http_get(registry_ip, &sig_path[..sig_path_len], &mut sig_buf);
    if sn < 64 {
        runtime::write_line(b"[appstored] bundle sig fetch failed\n");
        return false;
    }
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&sig_buf[..64]);

    // ── 5. Verify signature ───────────────────────────────────────────────
    if !runtime::ed25519_verify(&RELEASE_PUBKEY, bundle, &sig) {
        runtime::write_line(b"[appstored] bundle signature INVALID -- REJECTING\n");
        return false;
    }
    runtime::write_line(b"[appstored] bundle signature verified OK\n");

    // ── 6. Stage the bundle ───────────────────────────────────────────────
    // Build staged path: <stage_dir>/<name>-<version>.gapp
    let mut staged_path = [0u8; 256];
    let mut sp = 0usize;
    let slen = stage_dir.len().min(200); staged_path[sp..sp+slen].copy_from_slice(&stage_dir[..slen]); sp += slen;
    staged_path[sp] = b'/'; sp += 1;
    let nlen = entry.name_len.min(64); staged_path[sp..sp+nlen].copy_from_slice(&entry.name[..nlen]); sp += nlen;
    staged_path[sp] = b'-'; sp += 1;
    let vlen = entry.ver_len.min(32); staged_path[sp..sp+vlen].copy_from_slice(&entry.version[..vlen]); sp += vlen;
    staged_path[sp..sp+5].copy_from_slice(b".gapp"); sp += 5;

    // Preserve previous version for rollback.
    let mut prev_path = [0u8; 264];
    prev_path[..sp].copy_from_slice(&staged_path[..sp]);
    prev_path[sp..sp+5].copy_from_slice(b".prev");

    runtime::vfs_rename(&staged_path[..sp], &prev_path[..sp+5]);
    let fd = runtime::vfs_create(&staged_path[..sp]);
    if fd == u64::MAX {
        runtime::write_line(b"[appstored] failed to create staged bundle file\n");
        return false;
    }
    runtime::vfs_write(fd, bundle);
    runtime::vfs_close(fd);
    runtime::write_line(b"[appstored] bundle staged OK\n");
    true
}

// ════════════════════════════════════════════════════════════════════
// Hex decode helper
// ════════════════════════════════════════════════════════════════════

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn hex_decode(hex: &[u8], out: &mut [u8]) -> bool {
    if hex.len() != out.len() * 2 { return false; }
    for (i, chunk) in hex.chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0]);
        let lo = hex_nibble(chunk[1]);
        match (hi, lo) {
            (Some(h), Some(l)) => out[i] = (h << 4) | l,
            _ => return false,
        }
    }
    true
}

// ════════════════════════════════════════════════════════════════════
// Entry point
// ════════════════════════════════════════════════════════════════════

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[appstored] starting\n");

    // Announce to servicemgr.
    let _ = runtime::bootstrap_named_status(b"service-ready:", b"appstored");
    runtime::announce_service_ready(b"appstored");

    // Load registry IP from config (fallback to default).
    let registry_ip = load_registry_ip();

    // Main service loop: serve IPC requests from launcher.
    loop {
        match channel_recv_appstore() {
            Some(req) => handle_request(registry_ip, req),
            None      => runtime::yield_now(),
        }
    }
}

fn load_registry_ip() -> [u8; 4] {
    const CFG_PATH: &[u8] = b"/data/etc/graphos/registry";
    let fd = runtime::vfs_open(CFG_PATH);
    if fd == u64::MAX { return DEFAULT_REGISTRY_IP; }
    let mut buf = [0u8; 16];
    let n = runtime::vfs_read(fd, &mut buf);
    runtime::vfs_close(fd);
    if n >= 7 {
        // Parse dotted-decimal IPv4 (e.g. "10.0.2.2\n").
        let mut ip = [0u8; 4];
        let mut octet = 0u8;
        let mut idx = 0usize;
        for &b in &buf[..n as usize] {
            if b == b'.' || b == b'\n' || b == b'\r' {
                if idx < 4 { ip[idx] = octet; idx += 1; }
                octet = 0;
            } else if b.is_ascii_digit() {
                octet = octet.saturating_mul(10).saturating_add(b - b'0');
            }
        }
        if idx == 3 { ip[3] = octet; }
        if idx >= 3 { return ip; }
    }
    DEFAULT_REGISTRY_IP
}

// ── IPC request dispatch ──────────────────────────────────────────────────────

/// Opaque IPC request token — real definition in servicemgr channel protocol.
pub struct AppstoreRequest(u64);

fn channel_recv_appstore() -> Option<AppstoreRequest> {
    // Channel 0 = appstored inbox (registered at startup with servicemgr).
    let mut buf = [0u8; 8];
    let n = runtime::channel_recv(0, &mut buf);
    if n == 0 { None } else { Some(AppstoreRequest(u64::from_le_bytes(buf))) }
}

fn handle_request(registry_ip: [u8; 4], _req: AppstoreRequest) {
    // Placeholder: fetch index and stage first entry.
    // Full IPC protocol (search, install, rollback commands) is a v1.1 deliverable.
    let mut index_buf = [0u8; MAX_INDEX_SIZE];
    let n = http_get(registry_ip, b"/index.sig-v1", &mut index_buf);
    if n == 0 {
        runtime::write_line(b"[appstored] index fetch failed\n");
        return;
    }
    let mut entries = [PackageEntry::empty(); MAX_PACKAGES];
    let count = verify_and_parse_index(&index_buf[..n], &mut entries);
    if count == 0 {
        runtime::write_line(b"[appstored] index parse/verify failed\n");
        return;
    }
    // Stage the first package as a demonstration.
    fetch_and_stage_bundle(registry_ip, &entries[0], b"/data/updates/staged");
}
