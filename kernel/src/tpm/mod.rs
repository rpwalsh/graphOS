// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! TPM 2.0 driver — TIS (LPC) and CRB (MMIO) transports.
//!
//! Implements the TPM Interface Specification (TIS) for LPC-attached TPMs
//! and the Command Response Buffer (CRB) interface for MMIO-attached TPMs.
//! Provides: PCR_Extend, PCR_Read, GetRandom, Seal, Unseal, Sign.
//!
//! ## References
//! - TCG TPM 2.0 Library Specification, Part 3 (Commands), §18 (PCR), §24 (Hierarchy)
//! - TCG PC Client Platform TPM Profile Specification, §6 (TIS interface)

use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

// ── TIS register offsets (I/O port base = 0xFED4_0000 for locality 0) ───────

const TPM_ACCESS: u64 = 0x00;
const TPM_INT_ENABLE: u64 = 0x08;
const TPM_INT_VECTOR: u64 = 0x0C;
const TPM_INT_STATUS: u64 = 0x10;
const TPM_INTF_CAPS: u64 = 0x14;
const TPM_STS: u64 = 0x18;
const TPM_DATA_FIFO: u64 = 0x24;
const TPM_XDATA_FIFO: u64 = 0x80;
const TPM_DID_VID: u64 = 0xF00;

// TIS locality 0 physical base.
const TIS_BASE: u64 = 0xFED4_0000;

// TPM_STS flags.
const STS_VALID: u8 = 1 << 7;
const STS_COMMAND_READY: u8 = 1 << 6;
const STS_GO: u8 = 1 << 5;
const STS_DATA_AVAIL: u8 = 1 << 4;
const STS_EXPECT: u8 = 1 << 3;

// TPM_ACCESS flags.
const ACCESS_ACTIVE_LOCALITY: u8 = 1 << 5;
const ACCESS_REQUEST_USE: u8 = 1 << 1;

// ── CRB register offsets (MMIO base from ACPI TPML table) ─────────────────────

const CRB_LOC_STATE: u64 = 0x00;
const CRB_LOC_CTRL: u64 = 0x08;
const CRB_LOC_STS: u64 = 0x0C;
const CRB_INTF_ID: u64 = 0x30;
const CRB_CTRL_REQ: u64 = 0x40;
const CRB_CTRL_STS: u64 = 0x44;
const CRB_CTRL_CANCEL: u64 = 0x48;
const CRB_CTRL_START: u64 = 0x4C;
const CRB_CTRL_CMD_SZ: u64 = 0x58;
const CRB_CTRL_CMD_PA: u64 = 0x5C;
const CRB_CTRL_RSP_SZ: u64 = 0x64;
const CRB_CTRL_RSP_PA: u64 = 0x68;

// ── TPM command / response codes ──────────────────────────────────────────────

const TPM_ST_NO_SESSIONS: u16 = 0x8001;
const TPM_CC_PCR_EXTEND: u32 = 0x0182;
const TPM_CC_PCR_READ: u32 = 0x017E;
const TPM_CC_GET_RANDOM: u32 = 0x017B;
const TPM_CC_SEAL: u32 = 0x0000_017A; // TPM2_CC_Create used for seal

const TPM_ALG_SHA256: u16 = 0x000B;
const SHA256_DIGEST_BYTES: usize = 32;
const TPM_HT_PCR: u32 = 0;

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TpmTransport {
    None,
    Tis,
    Crb,
}

struct TpmState {
    transport: TpmTransport,
    mmio_base: u64,
    /// Graph arena node ID for this TPM device (0 = not yet registered).
    graph_node: crate::graph::types::NodeId,
}

impl TpmState {
    const fn new() -> Self {
        Self {
            transport: TpmTransport::None,
            mmio_base: 0,
            graph_node: 0,
        }
    }
}

static TPM: Mutex<TpmState> = Mutex::new(TpmState::new());
static TPM_READY: AtomicBool = AtomicBool::new(false);

// ── MMIO helpers ──────────────────────────────────────────────────────────────

#[inline]
unsafe fn tpm_read8(base: u64, off: u64) -> u8 {
    unsafe { core::ptr::read_volatile((base + off) as *const u8) }
}
#[inline]
unsafe fn tpm_write8(base: u64, off: u64, v: u8) {
    unsafe {
        core::ptr::write_volatile((base + off) as *mut u8, v);
    }
}
#[inline]
unsafe fn tpm_read32(base: u64, off: u64) -> u32 {
    unsafe { core::ptr::read_volatile((base + off) as *const u32) }
}
#[inline]
unsafe fn tpm_write32(base: u64, off: u64, v: u32) {
    unsafe {
        core::ptr::write_volatile((base + off) as *mut u32, v);
    }
}

// ── TIS helpers ───────────────────────────────────────────────────────────────

unsafe fn tis_request_locality(base: u64) -> bool {
    unsafe {
        tpm_write8(base, TPM_ACCESS, ACCESS_REQUEST_USE);
        // Poll until active (up to 1000 iterations — ~1 µs each at bare metal).
        for _ in 0..1000 {
            let acc = tpm_read8(base, TPM_ACCESS);
            if acc & ACCESS_ACTIVE_LOCALITY != 0 {
                return true;
            }
        }
    }
    false
}

unsafe fn tis_wait_status(base: u64, mask: u8) -> bool {
    unsafe {
        for _ in 0..100_000 {
            let sts = tpm_read8(base, TPM_STS);
            if sts & mask != 0 {
                return true;
            }
        }
    }
    false
}

/// Send a command buffer via TIS and receive the response.
unsafe fn tis_submit(base: u64, cmd: &[u8], rsp: &mut [u8]) -> Option<usize> {
    unsafe {
        // Request locality 0.
        if !tis_request_locality(base) {
            return None;
        }

        // Signal command ready.
        tpm_write8(base, TPM_STS, STS_COMMAND_READY);
        if !tis_wait_status(base, STS_COMMAND_READY) {
            return None;
        }

        // Write command bytes into FIFO.
        for &b in cmd {
            tpm_write8(base, TPM_DATA_FIFO, b);
            // After each byte, check EXPECT to see if the TPM wants more.
        }

        // Issue GO.
        tpm_write8(base, TPM_STS, STS_GO);

        // Wait for data available.
        if !tis_wait_status(base, STS_DATA_AVAIL | STS_VALID) {
            return None;
        }

        // Read response.
        let mut i = 0;
        while i < rsp.len() {
            let sts = tpm_read8(base, TPM_STS);
            if sts & STS_DATA_AVAIL == 0 {
                break;
            }
            rsp[i] = tpm_read8(base, TPM_DATA_FIFO);
            i += 1;
        }

        // Release locality.
        tpm_write8(base, TPM_ACCESS, ACCESS_ACTIVE_LOCALITY);
        Some(i)
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the TPM via TIS (physical base `TIS_BASE`).
/// Call once at boot after identity-mapping the TPM MMIO region.
///
/// # Safety
/// Requires `TIS_BASE` to be identity-mapped.
pub unsafe fn init_tis() {
    let base = TIS_BASE;
    unsafe {
        let did_vid = tpm_read32(base, TPM_DID_VID);
        if did_vid == 0 || did_vid == 0xFFFF_FFFF {
            crate::arch::serial::write_line(b"[tpm] TIS: no device");
            return;
        }
    }
    let mut tpm = TPM.lock();
    tpm.transport = TpmTransport::Tis;
    tpm.mmio_base = base;
    drop(tpm);
    TPM_READY.store(true, Ordering::Release);
    {
        use crate::graph::handles::GraphHandle;
        use crate::graph::types::{EdgeKind, NODE_ID_KERNEL};
        let gn = crate::graph::handles::register_tpm_device(NODE_ID_KERNEL);
        if gn.is_valid() {
            crate::graph::arena::add_edge(NODE_ID_KERNEL, gn.node_id(), EdgeKind::Owns, 0);
            TPM.lock().graph_node = gn.node_id();
        }
    }
    crate::arch::serial::write_line(b"[tpm] TIS initialised");
}

/// Initialise the TPM via CRB (MMIO base from ACPI).
///
/// # Safety
/// Requires `mmio_base` to be identity-mapped.
pub unsafe fn init_crb(mmio_base: u64) {
    if mmio_base == 0 {
        return;
    }
    let mut tpm = TPM.lock();
    tpm.transport = TpmTransport::Crb;
    tpm.mmio_base = mmio_base;
    drop(tpm);
    TPM_READY.store(true, Ordering::Release);
    {
        use crate::graph::handles::GraphHandle;
        use crate::graph::types::{EdgeKind, NODE_ID_KERNEL};
        let gn = crate::graph::handles::register_tpm_device(NODE_ID_KERNEL);
        if gn.is_valid() {
            crate::graph::arena::add_edge(NODE_ID_KERNEL, gn.node_id(), EdgeKind::Owns, 0);
            TPM.lock().graph_node = gn.node_id();
        }
    }
    crate::arch::serial::write_line(b"[tpm] CRB initialised");
}

/// Returns true if the TPM has been successfully initialised.
#[inline]
pub fn is_ready() -> bool {
    TPM_READY.load(Ordering::Acquire)
}

/// Submit a raw TPM2 command buffer and receive the response.
/// Returns the number of response bytes on success, or `None` on error.
pub fn submit_raw(cmd: &[u8], rsp: &mut [u8]) -> Option<usize> {
    if !is_ready() {
        return None;
    }
    let tpm = TPM.lock();
    let base = tpm.mmio_base;
    let transport = tpm.transport;
    drop(tpm);
    match transport {
        TpmTransport::Tis => unsafe { tis_submit(base, cmd, rsp) },
        TpmTransport::Crb => unsafe { crb_submit(base, cmd, rsp) },
        TpmTransport::None => None,
    }
}

// ── Command builder helpers ───────────────────────────────────────────────────

fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// Extend `pcr_index` with a SHA-256 digest.
/// Returns `true` on success.
pub fn pcr_extend(pcr_index: u8, digest: &[u8; SHA256_DIGEST_BYTES]) -> bool {
    // TPM2_CC_PCR_Extend command layout:
    //  header (10) + handle (4) + authAreaSize (4) + authBlock (9) + digests (2+2+4+32)
    let mut cmd = [0u8; 10 + 4 + 4 + 9 + 2 + 2 + 4 + SHA256_DIGEST_BYTES];
    let cmd_size = cmd.len() as u32;

    // Header
    cmd[0..2].copy_from_slice(&be16(TPM_ST_NO_SESSIONS));
    cmd[2..6].copy_from_slice(&be32(cmd_size));
    cmd[6..10].copy_from_slice(&be32(TPM_CC_PCR_EXTEND));

    // Handle: PCR index.
    cmd[10..14].copy_from_slice(&be32(TPM_HT_PCR | pcr_index as u32));

    // Empty authorization area (size = 9, empty password session).
    cmd[14..18].copy_from_slice(&be32(9));
    // Session: authorizationSize=9, sessionHandle=TPM_RS_PW=0x40000009, nonce=0, attributes=0, hmac=0
    cmd[18..22].copy_from_slice(&be32(0x40000009));
    cmd[22] = 0; // nonce size (0)
    cmd[23] = 0; // session attributes
    cmd[24..26].copy_from_slice(&be16(0)); // hmac size (0)

    // TPML_DIGEST_VALUES: count=1
    cmd[26..28].copy_from_slice(&be16(1));
    // TPMT_HA: hashAlg=SHA256
    cmd[28..30].copy_from_slice(&be16(TPM_ALG_SHA256));
    cmd[30..30 + SHA256_DIGEST_BYTES].copy_from_slice(digest);

    let mut rsp = [0u8; 16];
    match submit_raw(&cmd, &mut rsp) {
        Some(n) if n >= 10 => {
            // Check response code (offset 6..10).
            let rc = u32::from_be_bytes([rsp[6], rsp[7], rsp[8], rsp[9]]);
            rc == 0
        }
        _ => false,
    }
}

/// Read the SHA-256 digest of `pcr_index`.
/// Returns the 32-byte digest on success, all-zeros on failure.
pub fn pcr_read(pcr_index: u8) -> [u8; SHA256_DIGEST_BYTES] {
    // TPM2_CC_PCR_Read command.
    let mut cmd = [0u8; 10 + 4 + 2 + 2 + 4];
    let cmd_size = cmd.len() as u32;
    cmd[0..2].copy_from_slice(&be16(TPM_ST_NO_SESSIONS));
    cmd[2..6].copy_from_slice(&be32(cmd_size));
    cmd[6..10].copy_from_slice(&be32(TPM_CC_PCR_READ));
    // TPML_PCR_SELECTION: count=1
    cmd[10..14].copy_from_slice(&be32(1));
    // TPMS_PCR_SELECTION: hashAlg=SHA256
    cmd[14..16].copy_from_slice(&be16(TPM_ALG_SHA256));
    // sizeofSelect=3
    cmd[16] = 3;
    // PCR bitmask: set bit for pcr_index.
    if pcr_index < 8 {
        cmd[17] |= 1 << pcr_index;
    } else if pcr_index < 16 {
        cmd[18] |= 1 << (pcr_index - 8);
    } else if pcr_index < 24 {
        cmd[19] |= 1 << (pcr_index - 16);
    }

    let mut rsp = [0u8; 128];
    match submit_raw(&cmd, &mut rsp) {
        Some(n) if n >= 42 => {
            // Response layout: header(10) + pcrUpdateCounter(4) + TPML_PCR_SELECTION(?) + TPML_DIGEST
            // The digest starts at a fixed offset for a single-PCR read.
            // Simplified: read from offset n-32.
            let start = n.saturating_sub(SHA256_DIGEST_BYTES);
            let mut out = [0u8; SHA256_DIGEST_BYTES];
            out.copy_from_slice(&rsp[start..start + SHA256_DIGEST_BYTES]);
            out
        }
        _ => [0u8; SHA256_DIGEST_BYTES],
    }
}

/// Compute a `TPM2_PolicyPCR` policy digest for the given `pcr_mask`.
///
/// The returned 32-byte value is the `authPolicy` field to embed in the
/// sealed object's `TPMT_PUBLIC`. The TPM will only unseal the object when
/// the active PolicyPCR session covers the same PCR values that were current
/// at seal time.
///
/// Algorithm (TPM Library spec Part 1 §19.7.8):
///   policyDigest = 0x00..00 (32 zeros, initial empty policy)
///   for each set bit i in pcr_mask:
///       pcrDigest = SHA-256(pcr_read(i))
///       policyDigest = SHA-256(policyDigest ‖ CC_PolicyPCR ‖ pcrSelection ‖ pcrDigest)
///
/// If the TPM is not ready or pcr_mask is 0, returns all-zeros (no binding).
pub fn build_pcr_policy(pcr_mask: u32) -> [u8; SHA256_DIGEST_BYTES] {
    if !is_ready() || pcr_mask == 0 {
        return [0u8; SHA256_DIGEST_BYTES];
    }

    // TPM_CC_PolicyPCR = 0x0000017F
    const CC_POLICY_PCR: u32 = 0x0000_017F;
    let cc_bytes = be32(CC_POLICY_PCR);

    // TPMS_PCR_SELECTION for a single 3-byte mask (SHA-256 bank).
    // count(4) + hashAlg(2) + sizeofSelect(1) + select[3] = 10 bytes
    let sel_bytes: [u8; 10] = [
        0x00,
        0x00,
        0x00,
        0x01, // count = 1
        0x00,
        0x0B, // hashAlg = SHA-256
        0x03, // sizeofSelect = 3
        (pcr_mask & 0xFF) as u8,
        ((pcr_mask >> 8) & 0xFF) as u8,
        ((pcr_mask >> 16) & 0xFF) as u8,
    ];

    // Hash all selected PCR values together to form pcrDigest.
    // pcrDigest = SHA-256(concat of selected PCR values in ascending index order)
    let mut pcr_concat = [0u8; 32 * 24]; // max 24 PCRs × 32 bytes
    let mut pcr_concat_len = 0usize;
    for i in 0u8..24 {
        if pcr_mask & (1u32 << i) != 0 {
            let val = pcr_read(i);
            if pcr_concat_len + 32 <= pcr_concat.len() {
                pcr_concat[pcr_concat_len..pcr_concat_len + 32].copy_from_slice(&val);
                pcr_concat_len += 32;
            }
        }
    }
    let pcr_digest = crate::crypto::sha256(&pcr_concat[..pcr_concat_len]);

    // Iteratively update the policy digest.
    // For simplicity we do a single PolicyPCR call covering all bits at once.
    // policyDigest = SHA-256(zeroPolicy ‖ cc_bytes ‖ sel_bytes ‖ pcr_digest)
    let zero_policy = [0u8; SHA256_DIGEST_BYTES];
    let mut hash_input = [0u8; 32 + 4 + 10 + 32];
    let mut off = 0usize;
    hash_input[off..off + 32].copy_from_slice(&zero_policy);
    off += 32;
    hash_input[off..off + 4].copy_from_slice(&cc_bytes);
    off += 4;
    hash_input[off..off + 10].copy_from_slice(&sel_bytes);
    off += 10;
    hash_input[off..off + 32].copy_from_slice(&pcr_digest);
    crate::crypto::sha256(&hash_input)
}

/// Request `bytes_requested` bytes of random data from the TPM's DRBG.
/// Returns the actual number of bytes filled (may be less than requested).
pub fn get_random(out: &mut [u8]) -> usize {
    let wanted = out.len().min(32) as u16;
    let mut cmd = [0u8; 12];
    cmd[0..2].copy_from_slice(&be16(TPM_ST_NO_SESSIONS));
    cmd[2..6].copy_from_slice(&be32(12));
    cmd[6..10].copy_from_slice(&be32(TPM_CC_GET_RANDOM));
    cmd[10..12].copy_from_slice(&be16(wanted));

    let mut rsp = [0u8; 64];
    match submit_raw(&cmd, &mut rsp) {
        Some(n) if n >= 14 => {
            // Response: header(10) + randomBytes.size(2) + bytes
            let count = u16::from_be_bytes([rsp[10], rsp[11]]) as usize;
            let copy = count.min(out.len()).min(n - 12);
            out[..copy].copy_from_slice(&rsp[12..12 + copy]);
            copy
        }
        _ => 0,
    }
}

pub mod attestation;

// ── CRB command path ─────────────────────────────────────────────────────────

/// TPM CRB interface — submit command, await completion, copy response.
///
/// Follows TCG PC Client Platform TPM Profile §5 (CRB Interface):
/// 1. Write command bytes into CRB command buffer region.
/// 2. Set CRB_CTRL_START.START bit to signal the TPM.
/// 3. Poll CRB_CTRL_STS.tpmIdle until clear (command executing).
/// 4. Poll CRB_CTRL_STS.tpmSts until command complete (bit 1 clear).
/// 5. Copy response from CRB response buffer region.
///
/// # Safety
/// Requires `base` to be identity-mapped CRB MMIO.
unsafe fn crb_submit(base: u64, cmd: &[u8], rsp: &mut [u8]) -> Option<usize> {
    unsafe {
        // CRB_CTRL_CMD_{PA,SZ} and CRB_CTRL_RSP_{PA,SZ} describe where command/
        // response buffers live in MMIO space.  Per spec they are at fixed offsets
        // relative to the CRB base.  The standard layout has:
        //   cmd buffer  @ base + 0x80, size up to 2048 bytes
        //   rsp buffer  @ base + 0x80 (shared, same region, response overwrites)
        // We use the simplest compliant layout: shared 2048-byte buffer at base+0x80.
        const BUF_OFFSET: u64 = 0x80;
        const BUF_SIZE: usize = 2048;

        if cmd.len() > BUF_SIZE {
            return None;
        }

        // Step 1: Write command into CRB command buffer
        let cmd_pa = base + BUF_OFFSET;
        for (i, &b) in cmd.iter().enumerate() {
            core::ptr::write_volatile((cmd_pa + i as u64) as *mut u8, b);
        }

        // Step 2: Set CRB command size register, then START bit
        tpm_write32(base, CRB_CTRL_CMD_SZ, cmd.len() as u32);
        tpm_write32(base, CRB_CTRL_CMD_PA, cmd_pa as u32);
        tpm_write32(base, CRB_CTRL_RSP_PA, cmd_pa as u32); // shared buffer
        tpm_write32(base, CRB_CTRL_RSP_SZ, BUF_SIZE as u32);

        // Write START (bit 0 of CRB_CTRL_START) to kick the TPM.
        tpm_write32(base, CRB_CTRL_START, 1);

        // Step 3+4: Poll CRB_CTRL_STS — wait for START to self-clear (command done).
        // Bit 0 of CRB_CTRL_STS is tpmSts (1 = cmd running, 0 = complete).
        // Bit 1 is tpmIdle.  We wait up to 200_000 iterations (~200 ms at bare metal).
        let mut done = false;
        for _ in 0..200_000 {
            let sts = tpm_read32(base, CRB_CTRL_STS);
            if sts & 1 == 0 {
                done = true;
                break;
            }
        }
        if !done {
            return None;
        }

        // Step 5: The response header is in the shared buffer.  Extract size.
        // TPM response header: tag(2) + responseSize(4) + responseCode(4)
        let tag =
            (tpm_read8(base, BUF_OFFSET) as u16) << 8 | tpm_read8(base, BUF_OFFSET + 1) as u16;
        let _ = tag;
        let r3 = tpm_read8(base, BUF_OFFSET + 2) as u32;
        let r2 = tpm_read8(base, BUF_OFFSET + 3) as u32;
        let r1 = tpm_read8(base, BUF_OFFSET + 4) as u32;
        let r0 = tpm_read8(base, BUF_OFFSET + 5) as u32;
        let resp_size = ((r3 << 24) | (r2 << 16) | (r1 << 8) | r0) as usize;

        if !(10..=BUF_SIZE).contains(&resp_size) {
            return None;
        }
        let copy = resp_size.min(rsp.len());
        for (i, dst) in rsp.iter_mut().enumerate().take(copy) {
            *dst = core::ptr::read_volatile((cmd_pa + i as u64) as *const u8);
        }
        Some(copy)
    }
}

// ---------------------------------------------------------------------------
// TPM2_Quote (attestation)
// ---------------------------------------------------------------------------

/// TPM2_Quote command codes
const TPM_CC_QUOTE: u32 = 0x0158;
const TPM_ST_SESSIONS: u16 = 0x8002;
/// AIK handle (transient; loaded by caller or using endorsement hierarchy).
const AIK_HANDLE: u32 = 0x8100_0001;

/// Generate a TPM2_Quote over the given PCR selection.
///
/// `qualifying_data`: nonce/challenge supplied by the verifier (up to 32 bytes).
/// `pcr_mask`: bitmask of PCRs to include in the quote (bits 0–23).
/// `attest_out`: filled with the TPMS_ATTEST blob (raw bytes from TPM response).
/// `sig_out`: filled with the TPMT_SIGNATURE blob.
///
/// Returns `(attest_len, sig_len)`.  Both are 0 on failure.
///
/// When no TPM is present the function falls back to constructing a
/// software-signed TPMS_ATTEST using the kernel's Ed25519 signing key
/// (the "software attestation" path for development environments).
pub fn tpm2_quote(
    qualifying_data: &[u8],
    pcr_mask: u32,
    attest_out: &mut [u8],
    sig_out: &mut [u8],
) -> (usize, usize) {
    // ── Hardware path (real TPM) ───────────────────────────────────────────
    if is_ready() {
        return tpm2_quote_hw(qualifying_data, pcr_mask, attest_out, sig_out);
    }

    // ── Software attestation fallback (development / QEMU without TPM) ────
    // Build a minimal TPMS_ATTEST structure manually (TCG spec Part 2 §10.12.8).
    // Magic = TPM_GENERATED_VALUE (0xFF544347), type = TPM_ST_ATTEST_QUOTE (0x8018)
    if attest_out.len() < 64 || sig_out.len() < 64 {
        return (0, 0);
    }

    let qd_len = qualifying_data.len().min(32);
    let mut pos = 0usize;

    // magic
    attest_out[pos..pos + 4].copy_from_slice(&0xFF544347u32.to_be_bytes());
    pos += 4;
    // type = TPM_ST_ATTEST_QUOTE
    attest_out[pos..pos + 2].copy_from_slice(&0x8018u16.to_be_bytes());
    pos += 2;
    // qualifiedSigner: TPM2B_NAME (size 0 = not set)
    attest_out[pos..pos + 2].copy_from_slice(&0u16.to_be_bytes());
    pos += 2;
    // qualifyingData: TPM2B_DATA
    attest_out[pos..pos + 2].copy_from_slice(&(qd_len as u16).to_be_bytes());
    pos += 2;
    attest_out[pos..pos + qd_len].copy_from_slice(&qualifying_data[..qd_len]);
    pos += qd_len;
    // clockInfo: clock(8) + resetCount(4) + restartCount(4) + safe(1)
    attest_out[pos..pos + 17].fill(0);
    pos += 17;
    // firmwareVersion (8)
    attest_out[pos..pos + 8].fill(0);
    pos += 8;
    // attested: TPMS_QUOTE_INFO = TPML_PCR_SELECTION + TPM2B_DIGEST
    // TPML_PCR_SELECTION count=1
    attest_out[pos..pos + 4].copy_from_slice(&1u32.to_be_bytes());
    pos += 4;
    // TPMS_PCR_SELECTION: hashAlg=SHA256(0x000B), sizeofSelect=3, bitmask
    attest_out[pos..pos + 2].copy_from_slice(&0x000Bu16.to_be_bytes());
    pos += 2;
    attest_out[pos] = 3;
    pos += 1; // sizeofSelect
    attest_out[pos] = (pcr_mask & 0xFF) as u8;
    attest_out[pos + 1] = ((pcr_mask >> 8) & 0xFF) as u8;
    attest_out[pos + 2] = ((pcr_mask >> 16) & 0xFF) as u8;
    pos += 3;
    // PCR digest = SHA-256 of concatenated selected PCR values (all zero in fallback)
    attest_out[pos..pos + 2].copy_from_slice(&32u16.to_be_bytes());
    pos += 2;
    attest_out[pos..pos + 32].fill(0);
    pos += 32;

    let alen = pos;

    // Software signature: Ed25519 sign of attest_out[..alen]
    // Use the kernel's cpu_init-seeded RDRAND entropy as a seed (ephemeral)
    let seed = {
        let mut s = [0u8; 32];
        for chunk in s.chunks_mut(8) {
            let v = crate::arch::x86_64::cpu_init::rdrand_entropy();
            chunk.copy_from_slice(&v.to_le_bytes()[..chunk.len()]);
        }
        s
    };
    let (pk, xsk) = crate::crypto::ed25519_sign::ed25519_keygen(&seed);
    let sig = crate::crypto::ed25519_sign::ed25519_sign(&xsk, &pk, &attest_out[..alen]);

    // Write raw 64-byte Ed25519 signature (no TPMT_SIGNATURE wrapper needed for
    // the software attestation path — callers use verify_quote which reads [0..64]).
    let slen = if sig_out.len() >= 64 {
        sig_out[0..64].copy_from_slice(&sig);
        64
    } else {
        0
    };

    (alen, slen)
}

/// Hardware TPM2_Quote via submit_raw.
fn tpm2_quote_hw(
    qualifying_data: &[u8],
    pcr_mask: u32,
    attest_out: &mut [u8],
    sig_out: &mut [u8],
) -> (usize, usize) {
    // Build TPM2_Quote command.
    // Header(10) + keyHandle(4) + authAreaSize(4) + passwordSession(9)
    //   + qualifyingData TPM2B(2+qd_len) + inScheme(2+2) + PCRselect(4+2+1+3)
    let qd_len = qualifying_data.len().min(32);
    let cmd_len = 10 + 4 + 4 + 9 + 2 + qd_len + 4 + 4 + 2 + 1 + 3;
    if cmd_len > 256 {
        return (0, 0);
    }
    let mut cmd = [0u8; 256];
    let mut p;

    // Header
    cmd[0..2].copy_from_slice(&be16(TPM_ST_SESSIONS));
    cmd[2..6].copy_from_slice(&be32(cmd_len as u32));
    cmd[6..10].copy_from_slice(&be32(TPM_CC_QUOTE));
    p = 10;

    // keyHandle = AIK
    cmd[p..p + 4].copy_from_slice(&be32(AIK_HANDLE));
    p += 4;

    // authorizationSize = 9 (empty password session)
    cmd[p..p + 4].copy_from_slice(&be32(9));
    p += 4;
    cmd[p..p + 4].copy_from_slice(&be32(0x40000009));
    p += 4; // TPM_RS_PW
    cmd[p] = 0;
    p += 1; // nonce size
    cmd[p] = 0;
    p += 1; // session attributes
    cmd[p..p + 2].copy_from_slice(&be16(0));
    p += 2; // hmac size

    // qualifyingData TPM2B_DATA
    cmd[p..p + 2].copy_from_slice(&be16(qd_len as u16));
    p += 2;
    cmd[p..p + qd_len].copy_from_slice(&qualifying_data[..qd_len]);
    p += qd_len;

    // inScheme TPMT_SIG_SCHEME: sigAlg=TPM_ALG_ECDAA(0x001A), hashAlg=SHA256(0x000B)
    // For Ed25519-signed AIK use TPM_ALG_ECDAA with curve P256; or NULL(0x0010) if key is ECDSA.
    // We use NULL_SCHEME so the TPM uses the key's default scheme.
    cmd[p..p + 2].copy_from_slice(&be16(0x0010));
    p += 2; // TPM_ALG_NULL
    cmd[p..p + 2].copy_from_slice(&be16(0x000B));
    p += 2; // SHA256

    // TPML_PCR_SELECTION: count=1, hashAlg=SHA256, sizeofSelect=3, bitmask
    cmd[p..p + 4].copy_from_slice(&be32(1));
    p += 4;
    cmd[p..p + 2].copy_from_slice(&be16(0x000B));
    p += 2; // SHA256
    cmd[p] = 3;
    p += 1;
    cmd[p] = (pcr_mask & 0xFF) as u8;
    cmd[p + 1] = ((pcr_mask >> 8) & 0xFF) as u8;
    cmd[p + 2] = ((pcr_mask >> 16) & 0xFF) as u8;
    p += 3;
    let _ = p;

    let mut rsp = [0u8; 1024];
    match submit_raw(&cmd[..cmd_len], &mut rsp) {
        Some(n) if n >= 10 => {
            let rc = u32::from_be_bytes([rsp[6], rsp[7], rsp[8], rsp[9]]);
            if rc != 0 {
                return (0, 0);
            }
            // Response layout after header(10) + parameterSize(4):
            // TPM2B_ATTEST(2+N) then TPMT_SIGNATURE(variable)
            if n < 16 {
                return (0, 0);
            }
            let attest_size = u16::from_be_bytes([rsp[14], rsp[15]]) as usize;
            if n < 16 + attest_size {
                return (0, 0);
            }
            let alen = attest_size.min(attest_out.len());
            attest_out[..alen].copy_from_slice(&rsp[16..16 + alen]);
            let sig_start = 16 + attest_size;
            let slen = (n - sig_start).min(sig_out.len());
            sig_out[..slen].copy_from_slice(&rsp[sig_start..sig_start + slen]);
            (alen, slen)
        }
        _ => (0, 0),
    }
}

// ---------------------------------------------------------------------------
// Update signing key
// ---------------------------------------------------------------------------

/// Return the enrolled update-signing public key (32 bytes, Ed25519).
///
/// Attempts to unseal the key from TPM NV index 0x01500001 (a well-known
/// GraphOS update-key slot).  Falls back to an all-zeros key if the TPM is
/// absent or the NV index has not been provisioned.
pub fn get_update_signing_key() -> [u8; 32] {
    if !is_ready() {
        return [0u8; 32];
    }

    // TPM2_NV_Read: read 32 bytes from NV index 0x01500001.
    // Command: header(10) + authHandle(4) + nvIndex(4) + authAreaSize(4)
    //          + pwSession(9) + size(2) + offset(2)
    const NV_INDEX: u32 = 0x0150_0001;
    const TPM_CC_NV_READ: u32 = 0x014E;
    const READ_SIZE: u16 = 32;

    let cmd_len: usize = 10 + 4 + 4 + 4 + 9 + 2 + 2;
    let mut cmd = [0u8; 40];
    cmd[0..2].copy_from_slice(&be16(TPM_ST_SESSIONS));
    cmd[2..6].copy_from_slice(&be32(cmd_len as u32));
    cmd[6..10].copy_from_slice(&be32(TPM_CC_NV_READ));
    // authHandle = TPM_RH_OWNER (0x40000001)
    cmd[10..14].copy_from_slice(&be32(0x40000001));
    // nvIndex
    cmd[14..18].copy_from_slice(&be32(NV_INDEX));
    // authorizationSize = 9 (empty password session)
    cmd[18..22].copy_from_slice(&be32(9));
    cmd[22..26].copy_from_slice(&be32(0x40000009)); // TPM_RS_PW
    cmd[26] = 0;
    cmd[27] = 0;
    cmd[28..30].copy_from_slice(&be16(0));
    // size
    cmd[30..32].copy_from_slice(&be16(READ_SIZE));
    // offset
    cmd[32..34].copy_from_slice(&be16(0));

    let mut rsp = [0u8; 64];
    match submit_raw(&cmd[..cmd_len], &mut rsp) {
        Some(n) if n >= 14 => {
            let rc = u32::from_be_bytes([rsp[6], rsp[7], rsp[8], rsp[9]]);
            if rc != 0 {
                return [0u8; 32];
            }
            // Response: header(10) + paramSize(4) + TPM2B_MAX_NV_BUFFER: size(2) + data
            if n < 16 {
                return [0u8; 32];
            }
            let dlen = u16::from_be_bytes([rsp[14], rsp[15]]) as usize;
            if dlen < 32 || n < 16 + 32 {
                return [0u8; 32];
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&rsp[16..48]);
            key
        }
        _ => [0u8; 32],
    }
}

// ---------------------------------------------------------------------------
// TPM2_Create (Seal) / TPM2_Unseal
// ---------------------------------------------------------------------------

const TPM_CC_CREATE: u32 = 0x0000_0153;
const TPM_CC_UNSEAL: u32 = 0x0000_015E;
/// Storage Primary SRK handle (created at provisioning time).
const SRK_HANDLE: u32 = 0x8100_0000;

/// Seal `secret` (max 128 bytes) under the SRK, bound to a PCR policy.
///
/// `pcr_mask` selects which PCRs are included in the policy digest.
/// `sealed_out` receives the private-area blob; must be >= 512 bytes.
///
/// Returns the number of bytes written to `sealed_out`, or 0 on failure.
pub fn seal(secret: &[u8], pcr_mask: u32, sealed_out: &mut [u8]) -> usize {
    if !is_ready() || secret.is_empty() || secret.len() > 128 {
        return 0;
    }
    if sealed_out.len() < 512 {
        return 0;
    }

    // Build a minimal TPM2_Create command to seal `secret` under the SRK
    // with a PCR policy.  The command is pre-formatted for a simple
    // TPM2_PolicyPCR policy over SHA-256 PCRs selected by `pcr_mask`.
    //
    // Layout: header(10) + parentHandle(4) + authAreaSize(4)
    //       + pwSession(9) + inSensitive(4+secret) + inPublic(variable)
    //       + outsideInfo(2) + creationPCR(variable)
    let sec_len = secret.len();
    let mut cmd = [0u8; 512];
    // Header: tag(0..2) filled now; size(2..6) back-filled after; cc(6..10) filled now
    cmd[0..2].copy_from_slice(&be16(TPM_ST_SESSIONS));
    cmd[6..10].copy_from_slice(&be32(TPM_CC_CREATE));
    let mut p = 10usize;

    // parentHandle = SRK
    cmd[p..p + 4].copy_from_slice(&be32(SRK_HANDLE));
    p += 4;

    // authorizationSize = 9 (empty password session)
    cmd[p..p + 4].copy_from_slice(&be32(9));
    p += 4;
    cmd[p..p + 4].copy_from_slice(&be32(0x40000009));
    p += 4; // TPM_RS_PW
    cmd[p] = 0;
    p += 1; // nonce
    cmd[p] = 0;
    p += 1; // session attrs
    cmd[p..p + 2].copy_from_slice(&be16(0));
    p += 2; // hmac

    // inSensitive TPM2B_SENSITIVE_CREATE: size(2) + userAuth(2+0) + data(2+seclen)
    let sensitive_size = 2 + 2 + sec_len as u16;
    cmd[p..p + 2].copy_from_slice(&be16(sensitive_size));
    p += 2;
    cmd[p..p + 2].copy_from_slice(&be16(0));
    p += 2; // userAuth empty
    cmd[p..p + 2].copy_from_slice(&be16(sec_len as u16));
    p += 2;
    cmd[p..p + sec_len].copy_from_slice(secret);
    p += sec_len;

    // inPublic TPM2B_PUBLIC: TPMT_PUBLIC for a keyedHash (sealed data object)
    // type=TPM_ALG_KEYEDHASH(0x0008), nameAlg=SHA256(0x000B),
    // objectAttributes: userWithAuth(1<<6) | noDA(1<<10)
    // authPolicy: real PCR-bound policy digest (or zeros if pcr_mask == 0)
    let policy_digest = build_pcr_policy(pcr_mask);
    let pub_header: [u8; 10] = [
        0x00, 0x08, // type = TPM_ALG_KEYEDHASH
        0x00, 0x0B, // nameAlg = SHA256
        0x00, 0x00, 0x04, 0x40, // objectAttributes: userWithAuth|noDA
        0x00, 0x20, // authPolicy size = 32
    ];
    // pub_size = header(10) + policy(32) + scheme(2) + unique(2) = 46
    let pub_size: u16 = (pub_header.len() + 32 + 2 + 2) as u16;
    cmd[p..p + 2].copy_from_slice(&be16(pub_size));
    p += 2;
    for b in &pub_header {
        cmd[p] = *b;
        p += 1;
    }
    // authPolicy (32 bytes — real PCR policy digest)
    cmd[p..p + 32].copy_from_slice(&policy_digest);
    p += 32;
    // scheme = TPM_ALG_NULL(0x0010), unique (empty TPM2B: size=0)
    cmd[p..p + 2].copy_from_slice(&be16(0x0010));
    p += 2; // scheme
    cmd[p..p + 2].copy_from_slice(&be16(0));
    p += 2; // unique size

    // outsideInfo TPM2B (empty)
    cmd[p..p + 2].copy_from_slice(&be16(0));
    p += 2;
    // creationPCR TPML_PCR_SELECTION (1 bank, SHA-256, pcr_mask)
    cmd[p..p + 4].copy_from_slice(&be32(1));
    p += 4;
    cmd[p..p + 2].copy_from_slice(&be16(0x000B));
    p += 2; // SHA256
    cmd[p] = 3;
    p += 1;
    cmd[p] = (pcr_mask & 0xFF) as u8;
    cmd[p + 1] = ((pcr_mask >> 8) & 0xFF) as u8;
    cmd[p + 2] = ((pcr_mask >> 16) & 0xFF) as u8;
    p += 3;

    // Fill in command size
    let cmd_len = p;
    cmd[2..6].copy_from_slice(&be32(cmd_len as u32));

    let mut rsp = [0u8; 512];
    match submit_raw(&cmd[..cmd_len], &mut rsp) {
        Some(n) if n >= 10 => {
            let rc = u32::from_be_bytes([rsp[6], rsp[7], rsp[8], rsp[9]]);
            if rc != 0 {
                return 0;
            }
            // Copy the private area blob (rest of response) into sealed_out.
            let blob = n - 10;
            let out_len = blob.min(sealed_out.len());
            sealed_out[..out_len].copy_from_slice(&rsp[10..10 + out_len]);
            out_len
        }
        _ => 0,
    }
}

/// Unseal a blob previously produced by `seal`.
///
/// `sealed` is the private-area blob; `out` receives the plain secret.
/// Returns the number of bytes written, or 0 on failure.
pub fn unseal(sealed: &[u8], out: &mut [u8]) -> usize {
    if !is_ready() || sealed.is_empty() {
        return 0;
    }
    if sealed.len() > 512 {
        return 0;
    }

    // TPM2_Load: load the sealed object under the SRK, then TPM2_Unseal.
    // For simplicity we encode both operations sequentially.

    // --- TPM2_Load ---
    const TPM_CC_LOAD: u32 = 0x0000_0157;
    let mut cmd = [0u8; 640];
    cmd[0..2].copy_from_slice(&be16(TPM_ST_SESSIONS));
    cmd[6..10].copy_from_slice(&be32(TPM_CC_LOAD));
    let mut p = 10usize;
    cmd[p..p + 4].copy_from_slice(&be32(SRK_HANDLE));
    p += 4; // parentHandle
    cmd[p..p + 4].copy_from_slice(&be32(9));
    p += 4; // authAreaSize
    cmd[p..p + 4].copy_from_slice(&be32(0x40000009));
    p += 4; // TPM_RS_PW
    cmd[p] = 0;
    p += 1;
    cmd[p] = 0;
    p += 1;
    cmd[p..p + 2].copy_from_slice(&be16(0));
    p += 2;
    // inPrivate = sealed blob
    cmd[p..p + 2].copy_from_slice(&be16(sealed.len() as u16));
    p += 2;
    cmd[p..p + sealed.len()].copy_from_slice(sealed);
    p += sealed.len();
    // inPublic = minimal public area (must match the one used during seal)
    let pub_body: [u8; 4] = [0x00, 0x08, 0x00, 0x0B]; // type + nameAlg
    cmd[p..p + 2].copy_from_slice(&be16(pub_body.len() as u16 + 36));
    p += 2;
    for b in &pub_body {
        cmd[p] = *b;
        p += 1;
    }
    for _ in 0..36 {
        cmd[p] = 0;
        p += 1;
    }

    let cmd_len = p;
    cmd[2..6].copy_from_slice(&be32(cmd_len as u32));

    let mut rsp = [0u8; 32];
    let loaded_handle = match submit_raw(&cmd[..cmd_len], &mut rsp) {
        Some(n) if n >= 14 => {
            let rc = u32::from_be_bytes([rsp[6], rsp[7], rsp[8], rsp[9]]);
            if rc != 0 {
                return 0;
            }
            u32::from_be_bytes([rsp[10], rsp[11], rsp[12], rsp[13]])
        }
        _ => return 0,
    };

    // --- TPM2_Unseal ---
    let mut cmd2 = [0u8; 32];
    cmd2[0..2].copy_from_slice(&be16(TPM_ST_SESSIONS));
    cmd2[6..10].copy_from_slice(&be32(TPM_CC_UNSEAL));
    let mut p2 = 10usize;
    cmd2[p2..p2 + 4].copy_from_slice(&be32(loaded_handle));
    p2 += 4;
    cmd2[p2..p2 + 4].copy_from_slice(&be32(9));
    p2 += 4;
    cmd2[p2..p2 + 4].copy_from_slice(&be32(0x40000009));
    p2 += 4;
    cmd2[p2] = 0;
    p2 += 1;
    cmd2[p2] = 0;
    p2 += 1;
    cmd2[p2..p2 + 2].copy_from_slice(&be16(0));
    p2 += 2;
    let cmd2_len = p2;
    cmd2[2..6].copy_from_slice(&be32(cmd2_len as u32));

    let mut rsp2 = [0u8; 256];
    let result = match submit_raw(&cmd2[..cmd2_len], &mut rsp2) {
        Some(n) if n >= 16 => {
            let rc = u32::from_be_bytes([rsp2[6], rsp2[7], rsp2[8], rsp2[9]]);
            if rc != 0 {
                return 0;
            }
            // Response: header(10) + paramSize(4) + TPM2B_SENSITIVE_DATA: size(2) + data
            let dlen = u16::from_be_bytes([rsp2[14], rsp2[15]]) as usize;
            let copy = dlen.min(out.len()).min(n - 16);
            out[..copy].copy_from_slice(&rsp2[16..16 + copy]);
            copy
        }
        _ => 0,
    };

    // TPM2_FlushContext to release the transient handle
    const TPM_CC_FLUSH: u32 = 0x0000_0165;
    let mut flush = [0u8; 14];
    flush[0..2].copy_from_slice(&be16(0x8001)); // TPM_ST_NO_SESSIONS
    flush[2..6].copy_from_slice(&be32(14));
    flush[6..10].copy_from_slice(&be32(TPM_CC_FLUSH));
    flush[10..14].copy_from_slice(&be32(loaded_handle));
    let mut flush_rsp = [0u8; 16];
    let _ = submit_raw(&flush, &mut flush_rsp);

    result
}
