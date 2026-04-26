// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciLocation {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciDeviceInfo {
    pub location: PciLocation,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub irq_line: u8,
}

const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;
const PCI_COMMAND_OFFSET: u8 = 0x04;
const PCI_CLASS_OFFSET: u8 = 0x08;
const PCI_HEADER_TYPE_OFFSET: u8 = 0x0E;
const PCI_INTERRUPT_LINE_OFFSET: u8 = 0x3C;

const PCI_COMMAND_IO_SPACE: u16 = 1 << 0;
const PCI_COMMAND_MEMORY_SPACE: u16 = 1 << 1;
const PCI_COMMAND_BUS_MASTER: u16 = 1 << 2;

fn probe_device(bus: u8, slot: u8, func: u8) -> Option<PciDeviceInfo> {
    let vendor_id = read_u16(bus, slot, func, 0x00);
    if vendor_id == 0xFFFF {
        return None;
    }
    let device_id = read_u16(bus, slot, func, 0x02);
    let class_reg = read_u32(bus, slot, func, PCI_CLASS_OFFSET);
    Some(PciDeviceInfo {
        location: PciLocation { bus, slot, func },
        vendor_id,
        device_id,
        class_code: ((class_reg >> 24) & 0xFF) as u8,
        subclass: ((class_reg >> 16) & 0xFF) as u8,
        prog_if: ((class_reg >> 8) & 0xFF) as u8,
        irq_line: read_u8(bus, slot, func, PCI_INTERRUPT_LINE_OFFSET),
    })
}

pub fn for_each_device<F: FnMut(PciDeviceInfo)>(mut visit: F) {
    let mut bus = 0u16;
    while bus <= 255 {
        let mut slot = 0u8;
        while slot < 32 {
            let header_type = read_u8(bus as u8, slot, 0, PCI_HEADER_TYPE_OFFSET);
            let function_count = if header_type & 0x80 != 0 { 8 } else { 1 };
            let mut func = 0u8;
            while func < function_count {
                if let Some(info) = probe_device(bus as u8, slot, func) {
                    visit(info);
                }
                func += 1;
            }
            slot += 1;
        }
        bus += 1;
    }
}

pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDeviceInfo> {
    let mut found = None;
    for_each_device(|info| {
        if found.is_none() && info.vendor_id == vendor_id && info.device_id == device_id {
            found = Some(info);
        }
    });
    found
}

pub fn log_scan() {
    use crate::arch::x86_64::serial;

    let mut count = 0usize;
    for_each_device(|info| {
        count += 1;
        serial::write_bytes(b"[pci] dev bus=");
        serial::write_u64_dec_inline(info.location.bus as u64);
        serial::write_bytes(b" slot=");
        serial::write_u64_dec_inline(info.location.slot as u64);
        serial::write_bytes(b" func=");
        serial::write_u64_dec_inline(info.location.func as u64);
        serial::write_bytes(b" vid=");
        serial::write_hex_inline(info.vendor_id as u64);
        serial::write_bytes(b" did=");
        serial::write_hex_inline(info.device_id as u64);
        serial::write_bytes(b" class=");
        serial::write_hex_inline(info.class_code as u64);
        serial::write_bytes(b" subclass=");
        serial::write_hex_inline(info.subclass as u64);
        serial::write_bytes(b" irq=");
        serial::write_u64_dec(info.irq_line as u64);
    });
    serial::write_bytes(b"[pci] scan complete devices=");
    serial::write_u64_dec(count as u64);
}

pub fn enable_bus_master(location: PciLocation) {
    let mut command = read_u16(
        location.bus,
        location.slot,
        location.func,
        PCI_COMMAND_OFFSET,
    );
    command |= PCI_COMMAND_IO_SPACE | PCI_COMMAND_MEMORY_SPACE | PCI_COMMAND_BUS_MASTER;
    write_u16(
        location.bus,
        location.slot,
        location.func,
        PCI_COMMAND_OFFSET,
        command,
    );
}

pub fn read_u8(bus: u8, slot: u8, func: u8, offset: u8) -> u8 {
    let value = read_u32(bus, slot, func, offset);
    ((value >> (((offset & 0x3) as u32) * 8)) & 0xFF) as u8
}

pub fn read_u16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let value = read_u32(bus, slot, func, offset);
    ((value >> (((offset & 0x2) as u32) * 8)) & 0xFFFF) as u16
}

pub fn read_u32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    let address = 0x8000_0000u32
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        outl(PCI_CONFIG_ADDRESS, address);
        inl(PCI_CONFIG_DATA)
    }
}

pub fn write_u16(bus: u8, slot: u8, func: u8, offset: u8, value: u16) {
    let address = 0x8000_0000u32
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        outl(PCI_CONFIG_ADDRESS, address);
        let shift = ((offset & 0x2) as u32) * 8;
        let current = inl(PCI_CONFIG_DATA);
        let mask = !(0xFFFFu32 << shift);
        outl(
            PCI_CONFIG_DATA,
            (current & mask) | ((value as u32) << shift),
        );
    }
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!(
            "in eax, dx",
            in("dx") port,
            out("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

unsafe fn outl(port: u16, value: u32) {
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") port,
            in("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}
