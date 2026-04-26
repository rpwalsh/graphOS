// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Minimal HTTP/1.1 GET client for OTA bundle download.
//!
//! This is a kernel-resident, no_std HTTP client that operates over the
//! existing TCP socket layer (`net::socket_*`). It fetches a single URL,
//! reads the response body, and returns the byte count for the caller to
//! pass to `update::stage_update()`.
//!
//! ## Limitations (intentional for kernel use)
//! - Only HTTP/1.1 GET (no TLS — integrity is provided by ed25519 bundle signature).
//! - Maximum response body: `MAX_BODY` bytes (configured below).
//! - Synchronous spin-wait on socket data (acceptable in early-boot context).
//! - No redirects, no chunked transfer-encoding, no gzip.
//! - URL format: `http://<host_ipv4_dotted>:<port>/<path>`
//!   e.g. `http://10.0.2.2:8080/update.bundle`
//!
//! ## Security note
//! The ed25519 signature on the bundle (checked in `update::stage_update`) is
//! the integrity and authenticity guarantee. The HTTP channel need not be
//! encrypted for the update to be tamper-evident.

use crate::net::{
    socket_bind, socket_close, socket_connect, socket_open, socket_recv, socket_send,
};

/// Maximum response body size for an OTA bundle (32 MiB).
pub const MAX_BODY: usize = 32 * 1024 * 1024;

/// Maximum HTTP response header section (16 KiB).
const MAX_HEADERS: usize = 16 * 1024;

/// Result of a GET operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpGetResult {
    /// Successfully read `n` bytes into the caller buffer.
    Ok(usize),
    /// URL parse error.
    BadUrl,
    /// Socket allocation or connection failed.
    ConnectFailed,
    /// Failed to send the HTTP request.
    SendFailed,
    /// Failed to receive a valid HTTP response.
    RecvFailed,
    /// HTTP response status was not 200.
    BadStatus(u16),
    /// Response body exceeded the caller buffer.
    BodyTooLarge,
}

/// Parsed URL components (HTTP only).
#[derive(Debug, Clone, Copy)]
struct ParsedUrl<'a> {
    host_ipv4: u32,
    port: u16,
    path: &'a [u8],
}

/// Parse `http://<ipv4>:<port>/<path>`.
fn parse_url(url: &[u8]) -> Option<ParsedUrl<'_>> {
    // Strip "http://"
    let rest = url.strip_prefix(b"http://")?;

    // Find end of host:port section (first '/')
    let slash_pos = rest.iter().position(|&b| b == b'/')?;
    let hostport = &rest[..slash_pos];
    let path = &rest[slash_pos..];

    // Split host:port at ':'
    let colon_pos = hostport.iter().position(|&b| b == b':')?;
    let host_bytes = &hostport[..colon_pos];
    let port_bytes = &hostport[colon_pos + 1..];

    let host_ipv4 = parse_ipv4(host_bytes)?;
    let port = parse_u16_ascii(port_bytes)?;

    Some(ParsedUrl {
        host_ipv4,
        port,
        path,
    })
}

/// Parse a dotted-decimal IPv4 address.
fn parse_ipv4(s: &[u8]) -> Option<u32> {
    let mut octets = [0u8; 4];
    let mut cur = 0u16;
    let mut dot_count = 0usize;

    for &b in s {
        if b == b'.' {
            if cur > 255 || dot_count >= 3 {
                return None;
            }
            octets[dot_count] = cur as u8;
            dot_count += 1;
            cur = 0;
        } else if b.is_ascii_digit() {
            cur = cur * 10 + (b - b'0') as u16;
            if cur > 255 {
                return None;
            }
        } else {
            return None;
        }
    }
    if dot_count != 3 {
        return None;
    }
    octets[3] = cur as u8;
    Some(u32::from_be_bytes(octets))
}

/// Parse an ASCII decimal u16.
fn parse_u16_ascii(s: &[u8]) -> Option<u16> {
    let mut val = 0u32;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u32;
        if val > 65535 {
            return None;
        }
    }
    Some(val as u16)
}

/// Find the byte offset of `needle` in `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parse the HTTP status code from the first response line.
/// Expects `HTTP/1.1 NNN ...`.
fn parse_status(line: &[u8]) -> Option<u16> {
    // Minimum: "HTTP/1.1 200"
    if line.len() < 12 {
        return None;
    }
    if &line[..9] != b"HTTP/1.1 " {
        return None;
    }
    parse_u16_ascii(&line[9..12])
}

/// Parse Content-Length from response headers (returns 0 if absent).
fn parse_content_length(headers: &[u8]) -> usize {
    const FIELD: &[u8] = b"Content-Length: ";
    let Some(start) = find_subsequence(headers, FIELD) else {
        return 0;
    };
    let after = &headers[start + FIELD.len()..];
    let end = after
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .unwrap_or(after.len());
    let mut val = 0usize;
    for &b in &after[..end] {
        if b.is_ascii_digit() {
            val = val.saturating_mul(10).saturating_add((b - b'0') as usize);
        } else {
            break;
        }
    }
    val
}

/// Synchronously fetch `url` over HTTP GET using the kernel TCP socket layer.
///
/// Writes the response body into `out_buf` and returns the number of bytes
/// written.  The caller should pass the buffer slice to
/// `update::stage_update()`.
///
/// `task_index` is the kernel task index for socket ownership (usually 0 for
/// the update service task).
pub fn http_get(task_index: usize, url: &[u8], out_buf: &mut [u8]) -> HttpGetResult {
    // ── Parse URL ────────────────────────────────────────────────────────────
    let parsed = match parse_url(url) {
        Some(p) => p,
        None => return HttpGetResult::BadUrl,
    };

    // ── Open and connect socket ──────────────────────────────────────────────
    let handle = match socket_open(task_index) {
        Some(h) => h,
        None => return HttpGetResult::ConnectFailed,
    };

    // Use an ephemeral local port derived from the parsed host to avoid
    // collisions with any other active socket.
    let ephemeral_port = 49152u16.wrapping_add((parsed.host_ipv4 & 0xFFFF) as u16);
    socket_bind(task_index, handle, ephemeral_port);

    if !socket_connect(task_index, handle, parsed.host_ipv4, parsed.port) {
        let _ = socket_close(task_index, handle);
        return HttpGetResult::ConnectFailed;
    }

    // ── Build GET request ────────────────────────────────────────────────────
    // "GET /path HTTP/1.1\r\nHost: ip\r\nConnection: close\r\n\r\n"
    let mut req = [0u8; 512];
    let req_len = build_get_request(&mut req, parsed.path, parsed.host_ipv4, parsed.port);
    if req_len == 0 {
        let _ = socket_close(task_index, handle);
        return HttpGetResult::SendFailed;
    }

    if socket_send(task_index, handle, &req[..req_len]).is_none() {
        let _ = socket_close(task_index, handle);
        return HttpGetResult::SendFailed;
    }

    // ── Receive response ─────────────────────────────────────────────────────
    // Read into a fixed header buffer first to locate the header/body split.
    let mut hdr_buf = [0u8; MAX_HEADERS];
    let hdr_received = recv_all(task_index, handle, &mut hdr_buf);
    if hdr_received == 0 {
        let _ = socket_close(task_index, handle);
        return HttpGetResult::RecvFailed;
    }

    // Locate end of headers ("\r\n\r\n")
    let header_end = match find_subsequence(&hdr_buf[..hdr_received], b"\r\n\r\n") {
        Some(pos) => pos + 4,
        None => {
            let _ = socket_close(task_index, handle);
            return HttpGetResult::RecvFailed;
        }
    };

    // Parse status line
    let first_line_end = find_subsequence(&hdr_buf[..header_end], b"\r\n").unwrap_or(header_end);
    let status = match parse_status(&hdr_buf[..first_line_end]) {
        Some(s) => s,
        None => {
            let _ = socket_close(task_index, handle);
            return HttpGetResult::RecvFailed;
        }
    };
    if status != 200 {
        let _ = socket_close(task_index, handle);
        return HttpGetResult::BadStatus(status);
    }

    let content_length = parse_content_length(&hdr_buf[..header_end]);

    // Copy any body bytes already received past the header boundary
    let body_already = hdr_received.saturating_sub(header_end);
    if body_already > out_buf.len() {
        let _ = socket_close(task_index, handle);
        return HttpGetResult::BodyTooLarge;
    }
    out_buf[..body_already].copy_from_slice(&hdr_buf[header_end..header_end + body_already]);

    // Read remaining body
    let remaining = if content_length > 0 {
        content_length.saturating_sub(body_already)
    } else {
        // No Content-Length — read until close
        out_buf.len().saturating_sub(body_already)
    };

    let body_end = body_already + remaining.min(out_buf.len().saturating_sub(body_already));
    let body_rest = recv_all(task_index, handle, &mut out_buf[body_already..body_end]);
    let total = body_already + body_rest;

    let _ = socket_close(task_index, handle);
    HttpGetResult::Ok(total)
}

/// Build a minimal HTTP/1.1 GET request into `buf`.
/// Returns the length written, or 0 on overflow.
fn build_get_request(buf: &mut [u8], path: &[u8], host_ipv4: u32, port: u16) -> usize {
    let mut pos = 0usize;

    macro_rules! write_bytes {
        ($b:expr) => {{
            let src: &[u8] = $b;
            if pos + src.len() > buf.len() {
                return 0;
            }
            buf[pos..pos + src.len()].copy_from_slice(src);
            pos += src.len();
        }};
    }

    write_bytes!(b"GET ");
    write_bytes!(path);
    write_bytes!(b" HTTP/1.1\r\nHost: ");

    // Write host IPv4
    let octets = host_ipv4.to_be_bytes();
    let mut ipbuf = [0u8; 15];
    let iplen = fmt_ipv4(&mut ipbuf, octets);
    write_bytes!(&ipbuf[..iplen]);

    if port != 80 {
        write_bytes!(b":");
        let mut portbuf = [0u8; 5];
        let portlen = fmt_u16(&mut portbuf, port);
        write_bytes!(&portbuf[..portlen]);
    }

    write_bytes!(b"\r\nConnection: close\r\n\r\n");
    pos
}

/// Format `octets` as dotted-decimal IPv4 into `buf`.  Returns byte count.
fn fmt_ipv4(buf: &mut [u8; 15], octets: [u8; 4]) -> usize {
    let mut pos = 0usize;
    for (i, &octet) in octets.iter().enumerate() {
        let mut tmp = [0u8; 3];
        let n = fmt_u8(&mut tmp, octet);
        buf[pos..pos + n].copy_from_slice(&tmp[..n]);
        pos += n;
        if i < 3 {
            buf[pos] = b'.';
            pos += 1;
        }
    }
    pos
}

/// Format a `u8` as ASCII decimal into `buf[..3]`.  Returns byte count.
fn fmt_u8(buf: &mut [u8; 3], v: u8) -> usize {
    if v >= 100 {
        buf[0] = b'0' + v / 100;
        buf[1] = b'0' + (v / 10) % 10;
        buf[2] = b'0' + v % 10;
        3
    } else if v >= 10 {
        buf[0] = b'0' + v / 10;
        buf[1] = b'0' + v % 10;
        2
    } else {
        buf[0] = b'0' + v;
        1
    }
}

/// Format a `u16` as ASCII decimal into `buf[..5]`.  Returns byte count.
fn fmt_u16(buf: &mut [u8; 5], v: u16) -> usize {
    let mut tmp = [0u8; 5];
    let mut pos = 5usize;
    let mut n = v;
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    while n > 0 {
        pos -= 1;
        tmp[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let len = 5 - pos;
    buf[..len].copy_from_slice(&tmp[pos..]);
    len
}

/// Read bytes from the socket into `buf` until full or no more data.
/// Returns total bytes read.  Uses a bounded spin (256 empty polls) to
/// handle socket latency without blocking indefinitely.
fn recv_all(task_index: usize, handle: crate::uuid::Uuid128, buf: &mut [u8]) -> usize {
    let mut total = 0usize;
    let mut empty_polls = 0usize;
    while total < buf.len() && empty_polls < 256 {
        match socket_recv(task_index, handle, &mut buf[total..]) {
            Some(0) | None => {
                empty_polls += 1;
                core::hint::spin_loop();
            }
            Some(n) => {
                total += n;
                empty_polls = 0;
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_valid() {
        let url = b"http://10.0.2.2:8080/update.bundle";
        let parsed = parse_url(url).expect("should parse");
        assert_eq!(parsed.host_ipv4, 0x0A00_0202);
        assert_eq!(parsed.port, 8080);
        assert_eq!(parsed.path, b"/update.bundle");
    }

    #[test]
    fn parse_url_no_port_fails() {
        // No port — our parser requires explicit port
        assert!(parse_url(b"http://10.0.2.2/update.bundle").is_none());
    }

    #[test]
    fn parse_ipv4_roundtrip() {
        assert_eq!(parse_ipv4(b"10.0.2.2"), Some(0x0A00_0202));
        assert_eq!(parse_ipv4(b"192.168.1.1"), Some(0xC0A8_0101));
        assert_eq!(parse_ipv4(b"256.0.0.1"), None);
    }

    #[test]
    fn parse_content_length_present() {
        let headers = b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nContent-Type: application/octet-stream\r\n\r\n";
        assert_eq!(parse_content_length(headers), 1024);
    }

    #[test]
    fn parse_content_length_absent() {
        let headers = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
        assert_eq!(parse_content_length(headers), 0);
    }

    #[test]
    fn parse_status_200() {
        assert_eq!(parse_status(b"HTTP/1.1 200 OK"), Some(200));
    }

    #[test]
    fn parse_status_404() {
        assert_eq!(parse_status(b"HTTP/1.1 404 Not Found"), Some(404));
    }

    #[test]
    fn build_get_request_well_formed() {
        let mut buf = [0u8; 512];
        let n = build_get_request(&mut buf, b"/update.bundle", 0x0A00_0202, 8080);
        let req = &buf[..n];
        assert!(req.starts_with(b"GET /update.bundle HTTP/1.1\r\n"));
        assert!(req.windows(5).any(|w| w == b"Host:".as_ref()));
        assert!(req.ends_with(b"\r\n\r\n"));
    }
}
