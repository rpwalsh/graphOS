// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! LDAP client — RFC 4511 subset for user/group lookup.
//!
//! Implements LDAP v3 Bind, Search, and unbind over a TCP socket.
//! Plugs into `UserDb` as a secondary lookup provider for enterprise
//! directory authentication (Session 24).
//!
//! ## BER encoding
//! Only the minimum BER operations required for LDAP v3 are implemented.
//! No ASN.1 compiler is used; encoding is hand-written for no_std
//! compatibility.

use crate::uuid::Uuid128 as Uuid;
use spin::Mutex;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const LDAP_PORT: u16 = 389;
const LDAP_VERSION: u8 = 3;
const MAX_ATTR_LEN: usize = 256;
const MAX_RESULTS: usize = 32;

// ---------------------------------------------------------------------------
// BER tag constants
// ---------------------------------------------------------------------------

const TAG_SEQUENCE: u8 = 0x30;
const TAG_INTEGER: u8 = 0x02;
const TAG_OCTET_STR: u8 = 0x04;
const TAG_ENUM: u8 = 0x0A;
const TAG_APP_BIND: u8 = 0x60; // BindRequest [APPLICATION 0]
const TAG_APP_UNBIND: u8 = 0x42; // UnbindRequest [APPLICATION 2]
const TAG_APP_SEARCH: u8 = 0x63; // SearchRequest [APPLICATION 3]
const TAG_APP_SRCH_RESULT: u8 = 0x64; // SearchResultEntry [APPLICATION 4]

// ---------------------------------------------------------------------------
// LDAP scope
// ---------------------------------------------------------------------------

/// LDAP search scope.
#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Scope {
    Base = 0,
    OneLevel = 1,
    SubTree = 2,
}

// ---------------------------------------------------------------------------
// User record resolved from LDAP
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct LdapUser {
    pub dn: [u8; MAX_ATTR_LEN],
    pub dn_len: usize,
    pub uid: [u8; 64],
    pub uid_len: usize,
    pub display_name: [u8; 128],
    pub display_len: usize,
    pub uuid: Uuid,
}

// ---------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------

struct LdapClient {
    connected: bool,
    bound: bool,
    msg_id: u32,
    /// TCP socket key (16-byte UUID-like handle).
    socket: [u8; 16],
}

impl LdapClient {
    const fn new() -> Self {
        Self {
            connected: false,
            bound: false,
            msg_id: 1,
            socket: [0u8; 16],
        }
    }
}

static CLIENT: Mutex<LdapClient> = Mutex::new(LdapClient::new());

// ---------------------------------------------------------------------------
// BER helpers
// ---------------------------------------------------------------------------

fn ber_len(out: &mut [u8], off: usize, len: usize) -> usize {
    if len < 0x80 {
        out[off] = len as u8;
        1
    } else if len < 0x100 {
        out[off] = 0x81;
        out[off + 1] = len as u8;
        2
    } else {
        out[off] = 0x82;
        out[off + 1] = (len >> 8) as u8;
        out[off + 2] = len as u8;
        3
    }
}

fn ber_octet_string(out: &mut [u8], mut off: usize, data: &[u8]) -> usize {
    out[off] = TAG_OCTET_STR;
    off += 1;
    let n = ber_len(out, off, data.len());
    off += n;
    out[off..off + data.len()].copy_from_slice(data);
    off + data.len()
}

fn ber_integer(out: &mut [u8], off: usize, val: i32) -> usize {
    out[off] = TAG_INTEGER;
    out[off + 1] = 4;
    let b = val.to_be_bytes();
    out[off + 2..off + 6].copy_from_slice(&b);
    off + 6
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Connect and perform an anonymous or simple bind to `server_addr`.
///
/// `server_addr` is a 4-byte IPv4 address.
/// `bind_dn` and `password` may be empty for anonymous bind.
/// Returns `true` on success.
pub fn connect_and_bind(server_ip: [u8; 4], bind_dn: &[u8], password: &[u8]) -> bool {
    let mut c = CLIENT.lock();
    if c.bound {
        return true;
    }

    // Use a fixed socket key derived from the LDAP port.
    let mut key = [0u8; 16];
    key[0] = 0x1d;
    key[1] = 0xab; // magic prefix
    key[14] = (LDAP_PORT >> 8) as u8;
    key[15] = LDAP_PORT as u8;

    let remote_ip = u32::from_be_bytes(server_ip);
    if !crate::net::tcp::connect(key, 0, remote_ip, LDAP_PORT) {
        crate::arch::serial::write_line(b"[ldap] connect failed");
        return false;
    }
    c.socket = key;
    c.connected = true;

    // Send BindRequest.
    let mut buf = [0u8; 256];
    let msg_id = c.msg_id;
    c.msg_id = c.msg_id.wrapping_add(1);

    let mut off = 2; // leave room for outer SEQUENCE tag+len
    off = ber_integer(&mut buf, off, msg_id as i32);

    let bind_start = off;
    buf[off] = TAG_APP_BIND;
    off += 2; // len placeholder
    let ver_start = off;
    off = ber_integer(&mut buf, off, LDAP_VERSION as i32);
    off = ber_octet_string(&mut buf, off, bind_dn);
    buf[off] = 0x80;
    off += 1;
    let n = ber_len(&mut buf, off, password.len());
    off += n;
    let pw_end = (off + password.len()).min(buf.len());
    let pw_copy = pw_end - off;
    buf[off..pw_end].copy_from_slice(&password[..pw_copy]);
    off = pw_end;

    let bind_len = off - bind_start - 2;
    buf[bind_start + 1] = bind_len as u8;
    let _ = ver_start;

    buf[0] = TAG_SEQUENCE;
    buf[1] = (off - 2) as u8;

    let sock = c.socket;
    let sent = crate::net::tcp::send(sock, &buf[..off]).unwrap_or(0);
    if sent != off {
        crate::arch::serial::write_line(b"[ldap] bind send failed");
        return false;
    }

    let mut rbuf = [0u8; 64];
    let n = crate::net::tcp::recv(sock, &mut rbuf).unwrap_or(0);
    if n < 4 {
        crate::arch::serial::write_line(b"[ldap] bind response truncated");
        return false;
    }
    let result_code = if n > 7 { rbuf[7] } else { 0xFF };
    c.bound = result_code == 0;
    if c.bound {
        crate::arch::serial::write_line(b"[ldap] bound");
    }
    c.bound
}

/// Search for a user by `uid_value` under `base_dn`.
///
/// Sends an RFC 4511 SearchRequest with EqualityMatch filter `(uid=<uid_value>)`,
/// scope=WholeSubtree, sizeLimit=1.  Parses SearchResultEntry (tag 0x64) and
/// returns the first match.
pub fn search_user(base_dn: &[u8], uid_value: &[u8]) -> Option<LdapUser> {
    let (sock, msg_id) = {
        let mut c = CLIENT.lock();
        if !c.bound {
            return None;
        }
        let sock = c.socket;
        let mid = c.msg_id;
        c.msg_id = c.msg_id.wrapping_add(1);
        (sock, mid)
    };

    // ── Build SearchRequest body into a staging buffer ────────────────────
    // SearchRequest body fields (before APPLICATION 3 envelope):
    //   baseObject  OCTET STRING
    //   scope       ENUMERATED  (2 = wholeSubtree)
    //   derefAlias  ENUMERATED  (0 = neverDerefAliases)
    //   sizeLimit   INTEGER     (1)
    //   timeLimit   INTEGER     (10)
    //   typesOnly   BOOLEAN     (FALSE)
    //   filter      equalityMatch [3] { "uid", uid_value }
    //   attributes  SEQUENCE OF (empty = all)
    let mut body = [0u8; 512];
    let mut bp = 0usize;

    bp = ber_octet_string(&mut body, bp, base_dn);

    // scope = 2 (wholeSubtree)
    body[bp] = TAG_ENUM;
    body[bp + 1] = 1;
    body[bp + 2] = 2;
    bp += 3;
    // derefAliases = 0
    body[bp] = TAG_ENUM;
    body[bp + 1] = 1;
    body[bp + 2] = 0;
    bp += 3;
    // sizeLimit = 1
    bp = ber_integer(&mut body, bp, 1);
    // timeLimit = 10
    bp = ber_integer(&mut body, bp, 10);
    // typesOnly = FALSE
    body[bp] = 0x01;
    body[bp + 1] = 0x01;
    body[bp + 2] = 0x00;
    bp += 3;

    // filter: EqualityMatch [3] IMPLICIT SEQUENCE { attrDesc, assertionValue }
    // Tag 0xa3 = context(3) | constructed | 3
    // Inner: OCTET_STRING("uid") + OCTET_STRING(uid_value)
    let filter_inner = (2 + b"uid".len()) + (2 + uid_value.len().min(128));
    body[bp] = 0xa3;
    bp += 1;
    let fh = ber_len(&mut body, bp, filter_inner);
    bp += fh;
    // attributeDesc = "uid"
    body[bp] = TAG_OCTET_STR;
    body[bp + 1] = 3;
    body[bp + 2..bp + 5].copy_from_slice(b"uid");
    bp += 5;
    // assertionValue = uid_value (clamped to 128 bytes)
    let avl = uid_value.len().min(128);
    body[bp] = TAG_OCTET_STR;
    body[bp + 1] = avl as u8;
    body[bp + 2..bp + 2 + avl].copy_from_slice(&uid_value[..avl]);
    bp += 2 + avl;

    // attributes: empty SEQUENCE (request all attributes)
    body[bp] = TAG_SEQUENCE;
    body[bp + 1] = 0;
    bp += 2;

    // ── Wrap in LDAPMessage SEQUENCE ──────────────────────────────────────
    // LDAPMessage ::= SEQUENCE { messageID INTEGER, SearchRequest [APP 3] ... }
    let mut buf = [0u8; 640];
    let mut off = 2usize; // reserve 2 bytes for outer tag+len (filled at end)

    off = ber_integer(&mut buf, off, msg_id as i32);

    // SearchRequest APPLICATION 3 tag
    buf[off] = TAG_APP_SEARCH;
    off += 1;
    let sh = ber_len(&mut buf, off, bp);
    off += sh;
    buf[off..off + bp].copy_from_slice(&body[..bp]);
    off += bp;

    // Outer SEQUENCE: tag 0x30, then length of everything after the first 2 bytes
    let payload_len = off - 2;
    buf[0] = TAG_SEQUENCE;
    // For simplicity use multi-byte if needed
    if payload_len < 0x80 {
        buf[1] = payload_len as u8;
    } else {
        // Need to shift content right by 1 byte to fit 2-byte length
        core::hint::black_box(()); // prevent dead-code elision of branch
        let end = off + 1;
        for i in (3..end).rev() {
            buf[i] = buf[i - 1];
        }
        buf[1] = 0x81;
        buf[2] = payload_len as u8;
        off += 1;
    }

    // ── Send ──────────────────────────────────────────────────────────────
    let sent = crate::net::tcp::send(sock, &buf[..off]).unwrap_or(0);
    if sent == 0 {
        crate::arch::serial::write_line(b"[ldap] search send failed");
        return None;
    }

    // ── Receive ───────────────────────────────────────────────────────────
    let mut rbuf = [0u8; 512];
    let n = crate::net::tcp::recv(sock, &mut rbuf).unwrap_or(0);
    if n < 4 {
        return None;
    }

    parse_search_result(&rbuf[..n])
}

/// Parse a SearchResultEntry BER message (LDAPMessage wrapper tag 0x30,
/// protocol op SearchResultEntry tag 0x64).
fn parse_search_result(data: &[u8]) -> Option<LdapUser> {
    if data.len() < 4 || data[0] != TAG_SEQUENCE {
        return None;
    }
    let (_, oh) = ber_read_len(data, 1)?;
    let mut pos = 1 + oh;

    // Skip messageID INTEGER
    if pos >= data.len() || data[pos] != TAG_INTEGER {
        return None;
    }
    let (mid_len, mh) = ber_read_len(data, pos + 1)?;
    pos += 1 + mh + mid_len;

    // Expect SearchResultEntry [APPLICATION 4] = 0x64
    if pos >= data.len() || data[pos] != TAG_APP_SRCH_RESULT {
        return None;
    }
    let (_, sh) = ber_read_len(data, pos + 1)?;
    pos += 1 + sh;

    // objectName OCTET STRING (DN)
    if pos >= data.len() || data[pos] != TAG_OCTET_STR {
        return None;
    }
    let (dn_len, dh) = ber_read_len(data, pos + 1)?;
    pos += 1 + dh;
    let dn_end = (pos + dn_len).min(data.len());

    let mut user = LdapUser {
        dn: [0u8; MAX_ATTR_LEN],
        dn_len: 0,
        uid: [0u8; 64],
        uid_len: 0,
        display_name: [0u8; 128],
        display_len: 0,
        uuid: Uuid::NIL,
    };
    let copy_dn = (dn_end - pos).min(MAX_ATTR_LEN);
    user.dn[..copy_dn].copy_from_slice(&data[pos..pos + copy_dn]);
    user.dn_len = copy_dn;
    pos = dn_end;

    // attributes SEQUENCE
    if pos >= data.len() || data[pos] != TAG_SEQUENCE {
        return Some(user);
    }
    let (attrs_len, ah) = ber_read_len(data, pos + 1)?;
    pos += 1 + ah;
    let attrs_end = (pos + attrs_len).min(data.len());

    while pos < attrs_end {
        if pos >= data.len() || data[pos] != TAG_SEQUENCE {
            break;
        }
        let (pa_len, ph) = ber_read_len(data, pos + 1)?;
        let pa_end = (pos + 1 + ph + pa_len).min(data.len());
        pos += 1 + ph;

        // attribute type OCTET STRING
        if pos >= data.len() || data[pos] != TAG_OCTET_STR {
            pos = pa_end;
            continue;
        }
        let (tlen, th) = ber_read_len(data, pos + 1)?;
        pos += 1 + th;
        let type_end = (pos + tlen).min(data.len());
        let attr_type = &data[pos..type_end];
        pos = type_end;

        // values SET (tag 0x31)
        if pos >= data.len() || data[pos] != 0x31 {
            pos = pa_end;
            continue;
        }
        let (set_len, set_h) = ber_read_len(data, pos + 1)?;
        let set_end = (pos + 1 + set_h + set_len).min(data.len());
        pos += 1 + set_h;

        // First value OCTET STRING
        if pos < set_end && pos < data.len() && data[pos] == TAG_OCTET_STR {
            let (vlen, vh) = ber_read_len(data, pos + 1)?;
            pos += 1 + vh;
            let val_end = (pos + vlen).min(data.len());
            let val = &data[pos..val_end];
            if attr_type == b"uid" || attr_type == b"sAMAccountName" {
                let n = val.len().min(64);
                user.uid[..n].copy_from_slice(&val[..n]);
                user.uid_len = n;
            } else if attr_type == b"cn" || attr_type == b"displayName" {
                let n = val.len().min(128);
                user.display_name[..n].copy_from_slice(&val[..n]);
                user.display_len = n;
            }
        }
        pos = pa_end;
    }

    if user.uid_len == 0 && user.dn_len == 0 {
        None
    } else {
        Some(user)
    }
}

/// Read BER definite length at `data[pos]`.  Returns `(length, header_bytes_consumed)`.
fn ber_read_len(data: &[u8], pos: usize) -> Option<(usize, usize)> {
    if pos >= data.len() {
        return None;
    }
    let b = data[pos];
    if b < 0x80 {
        return Some((b as usize, 1));
    }
    let n = (b & 0x7f) as usize;
    if n == 0 || n > 2 || pos + n >= data.len() {
        return None;
    }
    let mut len = 0usize;
    for i in 0..n {
        len = (len << 8) | data[pos + 1 + i] as usize;
    }
    Some((len, 1 + n))
}

/// Unbind and close the LDAP connection.
pub fn disconnect() {
    let mut c = CLIENT.lock();
    if !c.connected {
        return;
    }
    let mut buf = [0u8; 7];
    buf[0] = TAG_SEQUENCE;
    buf[1] = 5;
    ber_integer(&mut buf, 2, c.msg_id as i32);
    buf[6] = TAG_APP_UNBIND;
    let _ = crate::net::tcp::send(c.socket, &buf);
    crate::net::tcp::close(c.socket);
    c.connected = false;
    c.bound = false;
}

// ---------------------------------------------------------------------------
// Group resolution (RFC 4511 §4.5 SearchRequest on groupOfNames / posixGroup)
// ---------------------------------------------------------------------------

/// A group record resolved from an LDAP directory.
#[derive(Clone)]
pub struct LdapGroup {
    /// Distinguished name of the group entry.
    pub dn: [u8; MAX_ATTR_LEN],
    pub dn_len: usize,
    /// Common name of the group (cn attribute).
    pub cn: [u8; 128],
    pub cn_len: usize,
    /// Member DNs (member / uniqueMember attributes, up to MAX_RESULTS).
    pub members: [[u8; MAX_ATTR_LEN]; MAX_RESULTS],
    pub member_lens: [usize; MAX_RESULTS],
    pub member_count: usize,
}

/// Search for a group by common name (`cn`) under `base_dn`.
///
/// Sends an RFC 4511 SearchRequest with EqualityMatch filter `(cn=<group_name>)`,
/// scope=WholeSubtree, sizeLimit=1.  Returns the first matching group entry
/// with its member list populated from the `member` and `uniqueMember`
/// multi-valued attributes.
///
/// Returns `None` if not bound, the group is not found, or a parse error occurs.
pub fn search_group(base_dn: &[u8], group_name: &[u8]) -> Option<LdapGroup> {
    let (sock, msg_id) = {
        let mut c = CLIENT.lock();
        if !c.bound {
            return None;
        }
        let sock = c.socket;
        let mid = c.msg_id;
        c.msg_id = c.msg_id.wrapping_add(1);
        (sock, mid)
    };

    // ── Build SearchRequest body ──────────────────────────────────────────
    let mut body = [0u8; 512];
    let mut bp = 0usize;

    bp = ber_octet_string(&mut body, bp, base_dn);
    // scope = 2 (wholeSubtree)
    body[bp] = TAG_ENUM;
    body[bp + 1] = 1;
    body[bp + 2] = 2;
    bp += 3;
    // derefAliases = 0
    body[bp] = TAG_ENUM;
    body[bp + 1] = 1;
    body[bp + 2] = 0;
    bp += 3;
    // sizeLimit = 1
    bp = ber_integer(&mut body, bp, 1);
    // timeLimit = 10
    bp = ber_integer(&mut body, bp, 10);
    // typesOnly = FALSE
    body[bp] = 0x01;
    body[bp + 1] = 0x01;
    body[bp + 2] = 0x00;
    bp += 3;

    // filter: EqualityMatch [3] { "cn", group_name }
    let avl = group_name.len().min(128);
    let filter_inner = (2 + b"cn".len()) + (2 + avl);
    body[bp] = 0xa3;
    bp += 1;
    let fh = ber_len(&mut body, bp, filter_inner);
    bp += fh;
    body[bp] = TAG_OCTET_STR;
    body[bp + 1] = 2;
    body[bp + 2..bp + 4].copy_from_slice(b"cn");
    bp += 4;
    body[bp] = TAG_OCTET_STR;
    body[bp + 1] = avl as u8;
    body[bp + 2..bp + 2 + avl].copy_from_slice(&group_name[..avl]);
    bp += 2 + avl;

    // attributes: empty SEQUENCE (request all attributes)
    body[bp] = TAG_SEQUENCE;
    body[bp + 1] = 0;
    bp += 2;

    // ── Wrap in LDAPMessage ───────────────────────────────────────────────
    let mut buf = [0u8; 640];
    let mut off = 2usize;
    off = ber_integer(&mut buf, off, msg_id as i32);
    buf[off] = TAG_APP_SEARCH;
    off += 1;
    let sh = ber_len(&mut buf, off, bp);
    off += sh;
    buf[off..off + bp].copy_from_slice(&body[..bp]);
    off += bp;
    let payload_len = off - 2;
    buf[0] = TAG_SEQUENCE;
    if payload_len < 0x80 {
        buf[1] = payload_len as u8;
    } else {
        let end = off + 1;
        for i in (3..end).rev() {
            buf[i] = buf[i - 1];
        }
        buf[1] = 0x81;
        buf[2] = payload_len as u8;
        off += 1;
    }

    let sent = crate::net::tcp::send(sock, &buf[..off]).unwrap_or(0);
    if sent == 0 {
        return None;
    }

    let mut rbuf = [0u8; 1024];
    let n = crate::net::tcp::recv(sock, &mut rbuf).unwrap_or(0);
    if n < 4 {
        return None;
    }

    parse_group_result(&rbuf[..n])
}

/// Parse a SearchResultEntry (tag 0x64) for a group object.
/// Collects `cn` and all `member` / `uniqueMember` values.
fn parse_group_result(data: &[u8]) -> Option<LdapGroup> {
    if data.len() < 4 || data[0] != TAG_SEQUENCE {
        return None;
    }
    let (_, oh) = ber_read_len(data, 1)?;
    let mut pos = 1 + oh;

    // Skip messageID
    if pos >= data.len() || data[pos] != TAG_INTEGER {
        return None;
    }
    let (mid_len, mh) = ber_read_len(data, pos + 1)?;
    pos += 1 + mh + mid_len;

    // SearchResultEntry tag 0x64
    if pos >= data.len() || data[pos] != TAG_APP_SRCH_RESULT {
        return None;
    }
    let (_, sh) = ber_read_len(data, pos + 1)?;
    pos += 1 + sh;

    // objectName OCTET STRING (DN)
    if pos >= data.len() || data[pos] != TAG_OCTET_STR {
        return None;
    }
    let (dn_len, dh) = ber_read_len(data, pos + 1)?;
    pos += 1 + dh;
    let dn_end = (pos + dn_len).min(data.len());

    let mut group = LdapGroup {
        dn: [0u8; MAX_ATTR_LEN],
        dn_len: 0,
        cn: [0u8; 128],
        cn_len: 0,
        members: [[0u8; MAX_ATTR_LEN]; MAX_RESULTS],
        member_lens: [0usize; MAX_RESULTS],
        member_count: 0,
    };
    let copy_dn = (dn_end - pos).min(MAX_ATTR_LEN);
    group.dn[..copy_dn].copy_from_slice(&data[pos..pos + copy_dn]);
    group.dn_len = copy_dn;
    pos = dn_end;

    // attributes SEQUENCE
    if pos >= data.len() || data[pos] != TAG_SEQUENCE {
        return Some(group);
    }
    let (attrs_len, ah) = ber_read_len(data, pos + 1)?;
    pos += 1 + ah;
    let attrs_end = (pos + attrs_len).min(data.len());

    while pos < attrs_end {
        if pos >= data.len() || data[pos] != TAG_SEQUENCE {
            break;
        }
        let (pa_len, ph) = ber_read_len(data, pos + 1)?;
        let pa_end = (pos + 1 + ph + pa_len).min(data.len());
        pos += 1 + ph;

        // attribute type
        if pos >= data.len() || data[pos] != TAG_OCTET_STR {
            pos = pa_end;
            continue;
        }
        let (tlen, th) = ber_read_len(data, pos + 1)?;
        pos += 1 + th;
        let type_end = (pos + tlen).min(data.len());
        let attr_type = &data[pos..type_end];
        pos = type_end;

        let is_member_attr =
            attr_type == b"member" || attr_type == b"uniqueMember" || attr_type == b"memberUid";
        let is_cn = attr_type == b"cn";

        // values SET
        if pos >= data.len() || data[pos] != 0x31 {
            pos = pa_end;
            continue;
        }
        let (set_len, set_h) = ber_read_len(data, pos + 1)?;
        let set_end = (pos + 1 + set_h + set_len).min(data.len());
        pos += 1 + set_h;

        while pos < set_end {
            if pos >= data.len() || data[pos] != TAG_OCTET_STR {
                break;
            }
            let (vlen, vh) = ber_read_len(data, pos + 1)?;
            pos += 1 + vh;
            let val_end = (pos + vlen).min(data.len());
            let val = &data[pos..val_end];
            if is_cn {
                let n = val.len().min(128);
                group.cn[..n].copy_from_slice(&val[..n]);
                group.cn_len = n;
            } else if is_member_attr && group.member_count < MAX_RESULTS {
                let idx = group.member_count;
                let n = val.len().min(MAX_ATTR_LEN);
                group.members[idx][..n].copy_from_slice(&val[..n]);
                group.member_lens[idx] = n;
                group.member_count += 1;
            }
            pos = val_end;
        }
        pos = pa_end;
    }

    if group.dn_len == 0 && group.cn_len == 0 {
        None
    } else {
        Some(group)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// These unit tests exercise BER encoding helpers and result parsers
// without network I/O, using hand-crafted BER payloads.

#[cfg(test)]
mod tests {
    use super::*;

    // ── ber_len helper ────────────────────────────────────────────────────

    #[test]
    fn ber_len_short_form() {
        let mut buf = [0u8; 4];
        let n = ber_len(&mut buf, 0, 0x7f);
        assert_eq!(n, 1);
        assert_eq!(buf[0], 0x7f);
    }

    #[test]
    fn ber_len_one_byte_long_form() {
        let mut buf = [0u8; 4];
        let n = ber_len(&mut buf, 0, 0x80);
        assert_eq!(n, 2);
        assert_eq!(buf[0], 0x81);
        assert_eq!(buf[1], 0x80);
    }

    // ── parse_search_result ───────────────────────────────────────────────

    fn make_user_entry(dn: &[u8], uid: &[u8], display: &[u8]) -> alloc::vec::Vec<u8> {
        use alloc::vec::Vec;
        let mut v = Vec::new();
        // objectName
        v.push(TAG_OCTET_STR);
        v.push(dn.len() as u8);
        v.extend_from_slice(dn);
        // attributes SEQUENCE containing two PartialAttribute SEQUENCEs
        let mut attrs: Vec<u8> = Vec::new();
        for (attr_name, val) in &[(b"uid" as &[u8], uid), (b"cn", display)] {
            let inner_len = (2 + attr_name.len()) + (3 + 1 + val.len()); // approx
            let mut pa: Vec<u8> = Vec::new();
            // attr type
            pa.push(TAG_OCTET_STR);
            pa.push(attr_name.len() as u8);
            pa.extend_from_slice(attr_name);
            // values SET
            let mut val_set: Vec<u8> = Vec::new();
            val_set.push(TAG_OCTET_STR);
            val_set.push(val.len() as u8);
            val_set.extend_from_slice(val);
            pa.push(0x31); // SET
            pa.push(val_set.len() as u8);
            pa.extend(val_set);
            attrs.push(TAG_SEQUENCE);
            attrs.push(pa.len() as u8);
            attrs.extend(pa);
            let _ = inner_len;
        }
        v.push(TAG_SEQUENCE);
        v.push(attrs.len() as u8);
        v.extend(attrs);

        // Wrap in SearchResultEntry [APPLICATION 4] (0x64)
        let mut sre: Vec<u8> = Vec::new();
        sre.push(TAG_APP_SRCH_RESULT);
        sre.push(v.len() as u8);
        sre.extend(v);

        // Wrap in LDAPMessage SEQUENCE { messageID INTEGER, searchResultEntry }
        let mut msg_body: Vec<u8> = Vec::new();
        // messageID = 1
        msg_body.push(TAG_INTEGER);
        msg_body.push(4);
        msg_body.extend_from_slice(&1i32.to_be_bytes());
        msg_body.extend(sre);

        let mut out: Vec<u8> = Vec::new();
        out.push(TAG_SEQUENCE);
        out.push(msg_body.len() as u8);
        out.extend(msg_body);
        out
    }

    #[test]
    fn parse_search_result_success() {
        let raw = make_user_entry(b"uid=alice,dc=test", b"alice", b"Alice Example");
        let result = parse_search_result(&raw);
        assert!(result.is_some(), "should find user entry");
        let u = result.unwrap();
        assert_eq!(&u.uid[..u.uid_len], b"alice");
        assert_eq!(&u.display_name[..u.display_len], b"Alice Example");
    }

    #[test]
    fn parse_search_result_truncated_returns_none() {
        // Feed a too-short buffer.
        let result = parse_search_result(&[0x30, 0x04, 0x02, 0x01, 0x01]);
        assert!(result.is_none(), "truncated data must return None");
    }

    #[test]
    fn parse_search_result_wrong_tag_returns_none() {
        // Start byte is 0x00 (not TAG_SEQUENCE).
        let result = parse_search_result(&[0x00; 16]);
        assert!(result.is_none());
    }

    #[test]
    fn parse_group_result_success() {
        use alloc::vec::Vec;
        // Build a minimal group SearchResultEntry.
        let dn = b"cn=admins,dc=test";
        let members: &[&[u8]] = &[b"uid=alice,dc=test", b"uid=bob,dc=test"];

        let mut v: Vec<u8> = Vec::new();
        // objectName
        v.push(TAG_OCTET_STR);
        v.push(dn.len() as u8);
        v.extend_from_slice(dn);
        // attributes SEQUENCE
        let mut attrs: Vec<u8> = Vec::new();
        // cn attribute
        {
            let mut pa: Vec<u8> = Vec::new();
            pa.push(TAG_OCTET_STR);
            pa.push(2);
            pa.extend_from_slice(b"cn");
            let mut vs: Vec<u8> = Vec::new();
            vs.push(TAG_OCTET_STR);
            vs.push(6);
            vs.extend_from_slice(b"admins");
            pa.push(0x31);
            pa.push(vs.len() as u8);
            pa.extend(vs);
            attrs.push(TAG_SEQUENCE);
            attrs.push(pa.len() as u8);
            attrs.extend(pa);
        }
        // member attribute (multi-valued)
        {
            let mut pa: Vec<u8> = Vec::new();
            pa.push(TAG_OCTET_STR);
            pa.push(6);
            pa.extend_from_slice(b"member");
            let mut vs: Vec<u8> = Vec::new();
            for m in members {
                vs.push(TAG_OCTET_STR);
                vs.push(m.len() as u8);
                vs.extend_from_slice(m);
            }
            pa.push(0x31);
            pa.push(vs.len() as u8);
            pa.extend(vs);
            attrs.push(TAG_SEQUENCE);
            attrs.push(pa.len() as u8);
            attrs.extend(pa);
        }
        v.push(TAG_SEQUENCE);
        v.push(attrs.len() as u8);
        v.extend(attrs);

        let mut sre: Vec<u8> = Vec::new();
        sre.push(TAG_APP_SRCH_RESULT);
        sre.push(v.len() as u8);
        sre.extend(v);

        let mut body: Vec<u8> = Vec::new();
        body.push(TAG_INTEGER);
        body.push(4);
        body.extend_from_slice(&1i32.to_be_bytes());
        body.extend(sre);

        let mut out: Vec<u8> = Vec::new();
        out.push(TAG_SEQUENCE);
        out.push(body.len() as u8);
        out.extend(body);

        let g = parse_group_result(&out);
        assert!(g.is_some());
        let g = g.unwrap();
        assert_eq!(&g.cn[..g.cn_len], b"admins");
        assert_eq!(g.member_count, 2);
        assert_eq!(&g.members[0][..g.member_lens[0]], b"uid=alice,dc=test");
    }
}
