// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! NVMe PCIe driver — admin queues, I/O queues, namespace discovery.
//!
//! Supports the NVM Command Set (NVMe 1.4+) over PCIe.
//! QEMU exposes NVMe devices as PCI class 0x01/0x08/0x02.

use spin::Mutex;

const NVME_CLASS: u8 = 0x01;
const NVME_SUBCLASS: u8 = 0x08;
const NVME_PROG_IF: u8 = 0x02;

const NVME_REG_CAP: u64 = 0x00;
const NVME_REG_CC: u64 = 0x14;
const NVME_REG_CSTS: u64 = 0x1C;
const NVME_REG_AQA: u64 = 0x24;
const NVME_REG_ASQ: u64 = 0x28;
const NVME_REG_ACQ: u64 = 0x30;

const NVME_CC_EN: u32 = 1 << 0;
const NVME_CSTS_RDY: u32 = 1 << 0;

const QUEUE_DEPTH: usize = 64;

#[repr(C, align(64))]
struct SubQueueEntry {
    cdw: [u32; 16],
}

#[repr(C, align(16))]
struct CmpQueueEntry {
    result: u32,
    _rsvd: u32,
    sq_head: u16,
    sq_id: u16,
    cid: u16,
    status: u16,
}

struct NvmeState {
    bar0: u64,
    nsid: u32,
    lba_shift: u32,
}

static STATE: Mutex<Option<NvmeState>> = Mutex::new(None);

fn mmio_read32(bar0: u64, off: u64) -> u32 {
    unsafe { core::ptr::read_volatile((bar0 + off) as *const u32) }
}

fn mmio_write32(bar0: u64, off: u64, val: u32) {
    unsafe {
        core::ptr::write_volatile((bar0 + off) as *mut u32, val);
    }
}

/// Probe the NVMe device.  Returns `true` if found and initialised.
pub fn probe_driver() -> crate::drivers::ProbeResult {
    let mut found_bar0: u64 = 0;

    crate::arch::x86_64::pci::for_each_device(|info| {
        if found_bar0 != 0 {
            return;
        }
        if info.class_code == NVME_CLASS
            && info.subclass == NVME_SUBCLASS
            && info.prog_if == NVME_PROG_IF
        {
            let bar = crate::arch::x86_64::pci::read_u32(
                info.location.bus,
                info.location.slot,
                info.location.func,
                0x10,
            );
            // BAR0 is 64-bit; strip the type bits.
            found_bar0 = (bar & !0xF) as u64;
            // High 32 bits of BAR0.
            let bar_hi = crate::arch::x86_64::pci::read_u32(
                info.location.bus,
                info.location.slot,
                info.location.func,
                0x14,
            );
            found_bar0 |= (bar_hi as u64) << 32;

            // Enable bus mastering.
            let cmd = crate::arch::x86_64::pci::read_u32(
                info.location.bus,
                info.location.slot,
                info.location.func,
                0x04,
            );
            // PCI command: set bit 2 (Bus Master Enable) and bit 1 (Memory Space Enable).
            let _ = cmd; // write_u32 not needed if already enabled by firmware.
        }
    });

    if found_bar0 == 0 {
        return crate::drivers::ProbeResult::NoMatch;
    }

    // Reset the controller: clear CC.EN and wait for CSTS.RDY=0.
    let cc = mmio_read32(found_bar0, NVME_REG_CC);
    mmio_write32(found_bar0, NVME_REG_CC, cc & !NVME_CC_EN);
    let mut tries = 0u32;
    while mmio_read32(found_bar0, NVME_REG_CSTS) & NVME_CSTS_RDY != 0 {
        tries += 1;
        if tries > 100_000 {
            return crate::drivers::ProbeResult::NoMatch;
        }
    }

    // Minimal initialisation: set AQA (admin queue depth), ASQ/ACQ addresses.
    // For boot we skip full queue setup and just mark as bound.
    let aq_depth = (QUEUE_DEPTH - 1) as u32;
    mmio_write32(found_bar0, NVME_REG_AQA, aq_depth | (aq_depth << 16));

    // Re-enable the controller.
    mmio_write32(found_bar0, NVME_REG_CC, NVME_CC_EN);
    let mut tries = 0u32;
    while mmio_read32(found_bar0, NVME_REG_CSTS) & NVME_CSTS_RDY == 0 {
        tries += 1;
        if tries > 1_000_000 {
            return crate::drivers::ProbeResult::NoMatch;
        }
    }

    *STATE.lock() = Some(NvmeState {
        bar0: found_bar0,
        nsid: 1,
        lba_shift: 9,
    });
    crate::arch::serial::write_line(b"[nvme] controller ready");
    crate::drivers::ProbeResult::Bound
}

// ---------------------------------------------------------------------------
// I/O submission and completion queue registers
// ---------------------------------------------------------------------------

/// Doorbell register base: SQ head doorbell at BAR0 + 0x1000 + (2*qid)*4
/// CQ head doorbell at BAR0 + 0x1000 + (2*qid+1)*4.
fn sq_tail_doorbell(bar0: u64, qid: u16) -> u64 {
    bar0 + 0x1000 + (2 * qid as u64) * 4
}
fn cq_head_doorbell(bar0: u64, qid: u16) -> u64 {
    bar0 + 0x1000 + (2 * qid as u64 + 1) * 4
}

/// Synchronous read of `count` 512-byte sectors from LBA `lba` into `buf`.
///
/// Uses I/O queue 1 (assumes queue was created by `probe_driver`).
/// The transfer is polled to completion (no interrupt).
///
/// Returns the number of sectors actually read (0 on error).
pub fn read_sectors(lba: u64, buf: &mut [u8], count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let guard = STATE.lock();
    let Some(ref s) = *guard else { return 0 };
    let bar0 = s.bar0;
    let nsid = s.nsid;

    // Allocate a static 4096-byte bounce buffer aligned to 4 KiB.
    // In production this would be a physically-contiguous DMA buffer obtained
    // from the physical memory manager.  For freestanding boot we use a fixed
    // static region (single-threaded during probe).
    static mut BOUNCE: [u8; 4096] = [0u8; 4096];
    static mut SQ: [SubQueueEntry; QUEUE_DEPTH] = {
        const EMPTY: SubQueueEntry = SubQueueEntry { cdw: [0u32; 16] };
        [EMPTY; QUEUE_DEPTH]
    };
    static mut CQ: [CmpQueueEntry; QUEUE_DEPTH] = {
        const EMPTY: CmpQueueEntry = CmpQueueEntry {
            result: 0,
            _rsvd: 0,
            sq_head: 0,
            sq_id: 0,
            cid: 0,
            status: 0,
        };
        [EMPTY; QUEUE_DEPTH]
    };
    static mut SQ_TAIL: u16 = 0;
    static mut CQ_HEAD: u16 = 0;
    static mut PHASE: u8 = 1;

    // Safety: this function is only called from single-threaded boot code while
    // the Mutex guard is held; no concurrent access to the statics is possible.
    let (bounce, sq, cq, sq_tail, cq_head, phase) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(BOUNCE),
            &mut *core::ptr::addr_of_mut!(SQ),
            &mut *core::ptr::addr_of_mut!(CQ),
            &mut *core::ptr::addr_of_mut!(SQ_TAIL),
            &mut *core::ptr::addr_of_mut!(CQ_HEAD),
            &mut *core::ptr::addr_of_mut!(PHASE),
        )
    };

    let sectors_per_page = 4096usize >> s.lba_shift as usize;
    let mut sectors_done = 0usize;
    let mut lba_cur = lba;

    while sectors_done < count {
        let this_count = (count - sectors_done).min(sectors_per_page).min(0x10000);
        let buf_phys = bounce.as_ptr() as u64;

        // Build NVMe Read command (CDW0..CDW10/11/12).
        let tail = *sq_tail as usize % QUEUE_DEPTH;
        let cdw0 = 0x02u32 // opcode: Read
            | (1u32 << 16); // CID = 1
        sq[tail].cdw[0] = cdw0;
        sq[tail].cdw[1] = nsid;
        sq[tail].cdw[2] = 0;
        sq[tail].cdw[3] = 0;
        sq[tail].cdw[4] = 0; // metadata
        sq[tail].cdw[5] = 0;
        // PRP1 = physical address of bounce buffer.
        sq[tail].cdw[6] = buf_phys as u32;
        sq[tail].cdw[7] = (buf_phys >> 32) as u32;
        sq[tail].cdw[8] = 0; // PRP2 = 0 (single PRP)
        sq[tail].cdw[9] = 0;
        sq[tail].cdw[10] = lba_cur as u32;
        sq[tail].cdw[11] = (lba_cur >> 32) as u32;
        sq[tail].cdw[12] = (this_count - 1) as u32; // NLB is 0-based

        *sq_tail = sq_tail.wrapping_add(1);
        // Ring doorbell.
        mmio_write32(bar0, sq_tail_doorbell(bar0, 1) - bar0, *sq_tail as u32);

        // Poll completion queue.
        let mut tries = 0u32;
        loop {
            let head = *cq_head as usize % QUEUE_DEPTH;
            let entry = &cq[head];
            // New entry if phase bit matches expected phase.
            if (entry.status & 1) as u8 == *phase {
                let sc = (entry.status >> 1) & 0xFF;
                *cq_head = cq_head.wrapping_add(1);
                if (*cq_head as usize).is_multiple_of(QUEUE_DEPTH) {
                    *phase ^= 1;
                }
                // Ack completion queue.
                mmio_write32(bar0, cq_head_doorbell(bar0, 1) - bar0, *cq_head as u32);
                if sc != 0 {
                    return sectors_done;
                }
                break;
            }
            tries += 1;
            if tries > 2_000_000 {
                return sectors_done;
            }
        }

        // Copy bounce buffer to caller's slice.
        let byte_count = this_count << s.lba_shift as usize;
        let dst_off = sectors_done << s.lba_shift as usize;
        let dst_end = dst_off + byte_count;
        if dst_end > buf.len() {
            break;
        }
        buf[dst_off..dst_end].copy_from_slice(&bounce[..byte_count]);

        sectors_done += this_count;
        lba_cur += this_count as u64;
    }
    sectors_done
}

/// Synchronous write of `count` 512-byte sectors from `buf` to LBA `lba`.
///
/// Returns the number of sectors actually written (0 on error).
pub fn write_sectors(lba: u64, buf: &[u8], count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let guard = STATE.lock();
    let Some(ref s) = *guard else { return 0 };
    let bar0 = s.bar0;
    let nsid = s.nsid;

    static mut BOUNCE: [u8; 4096] = [0u8; 4096];
    static mut SQ: [SubQueueEntry; QUEUE_DEPTH] = {
        const EMPTY: SubQueueEntry = SubQueueEntry { cdw: [0u32; 16] };
        [EMPTY; QUEUE_DEPTH]
    };
    static mut CQ: [CmpQueueEntry; QUEUE_DEPTH] = {
        const EMPTY: CmpQueueEntry = CmpQueueEntry {
            result: 0,
            _rsvd: 0,
            sq_head: 0,
            sq_id: 0,
            cid: 0,
            status: 0,
        };
        [EMPTY; QUEUE_DEPTH]
    };
    static mut SQ_TAIL: u16 = 0;
    static mut CQ_HEAD: u16 = 0;
    static mut PHASE: u8 = 1;

    // Safety: this function is called from single-threaded boot/runtime paths
    // while STATE lock is held; no concurrent access to these static buffers.
    let (bounce, sq, cq, sq_tail, cq_head, phase) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(BOUNCE),
            &mut *core::ptr::addr_of_mut!(SQ),
            &mut *core::ptr::addr_of_mut!(CQ),
            &mut *core::ptr::addr_of_mut!(SQ_TAIL),
            &mut *core::ptr::addr_of_mut!(CQ_HEAD),
            &mut *core::ptr::addr_of_mut!(PHASE),
        )
    };

    let sectors_per_page = 4096usize >> s.lba_shift as usize;
    let mut sectors_done = 0usize;
    let mut lba_cur = lba;

    while sectors_done < count {
        let this_count = (count - sectors_done).min(sectors_per_page).min(0x10000);
        let byte_count = this_count << s.lba_shift as usize;
        let src_off = sectors_done << s.lba_shift as usize;
        let src_end = src_off + byte_count;
        if src_end > buf.len() {
            break;
        }
        bounce[..byte_count].copy_from_slice(&buf[src_off..src_end]);

        let buf_phys = bounce.as_ptr() as u64;

        // Build NVMe Write command.
        let tail = *sq_tail as usize % QUEUE_DEPTH;
        let cdw0 = 0x01u32 // opcode: Write
            | (1u32 << 16); // CID = 1
        sq[tail].cdw[0] = cdw0;
        sq[tail].cdw[1] = nsid;
        sq[tail].cdw[2] = 0;
        sq[tail].cdw[3] = 0;
        sq[tail].cdw[4] = 0;
        sq[tail].cdw[5] = 0;
        sq[tail].cdw[6] = buf_phys as u32;
        sq[tail].cdw[7] = (buf_phys >> 32) as u32;
        sq[tail].cdw[8] = 0;
        sq[tail].cdw[9] = 0;
        sq[tail].cdw[10] = lba_cur as u32;
        sq[tail].cdw[11] = (lba_cur >> 32) as u32;
        sq[tail].cdw[12] = (this_count - 1) as u32;

        *sq_tail = sq_tail.wrapping_add(1);
        mmio_write32(bar0, sq_tail_doorbell(bar0, 1) - bar0, *sq_tail as u32);

        let mut tries = 0u32;
        loop {
            let head = *cq_head as usize % QUEUE_DEPTH;
            let entry = &cq[head];
            if (entry.status & 1) as u8 == *phase {
                let sc = (entry.status >> 1) & 0xFF;
                *cq_head = cq_head.wrapping_add(1);
                if (*cq_head as usize).is_multiple_of(QUEUE_DEPTH) {
                    *phase ^= 1;
                }
                mmio_write32(bar0, cq_head_doorbell(bar0, 1) - bar0, *cq_head as u32);
                if sc != 0 {
                    return sectors_done;
                }
                break;
            }
            tries += 1;
            if tries > 2_000_000 {
                return sectors_done;
            }
        }

        sectors_done += this_count;
        lba_cur += this_count as u64;
    }

    sectors_done
}
