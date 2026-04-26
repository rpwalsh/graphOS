// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::arch::x86_64::serial;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const COMMAND_PORT: u16 = 0x64;
const STATUS_OUTPUT_READY: u8 = 1 << 0;
const STATUS_INPUT_BUSY: u8 = 1 << 1;
const STATUS_AUX_OUTPUT_READY: u8 = 1 << 5;
const CMD_ENABLE_AUX: u8 = 0xA8;
const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_WRITE_AUX: u8 = 0xD4;
const MOUSE_RESET_DEFAULTS: u8 = 0xF6;
const MOUSE_ENABLE_STREAMING: u8 = 0xF4;
const ACK: u8 = 0xFA;
const RESEND: u8 = 0xFE;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy)]
pub enum MouseEvent {
    Move { dx: i16, dy: i16 },
    Button { button: MouseButton, pressed: bool },
}

#[derive(Clone, Copy)]
struct MouseQueue {
    events: [Option<MouseEvent>; 64],
    head: usize,
    len: usize,
    packet: [u8; 3],
    packet_len: usize,
    buttons: u8,
}

impl MouseQueue {
    const fn new() -> Self {
        Self {
            events: [None; 64],
            head: 0,
            len: 0,
            packet: [0; 3],
            packet_len: 0,
            buttons: 0,
        }
    }

    fn push_event(&mut self, event: MouseEvent) {
        let tail = (self.head + self.len) % self.events.len();
        self.events[tail] = Some(event);
        if self.len == self.events.len() {
            self.head = (self.head + 1) % self.events.len();
        } else {
            self.len += 1;
        }
    }

    fn pop_event(&mut self) -> Option<MouseEvent> {
        if self.len == 0 {
            return None;
        }
        let event = self.events[self.head].take();
        self.head = (self.head + 1) % self.events.len();
        self.len -= 1;
        event
    }
}

static MOUSE: Mutex<MouseQueue> = Mutex::new(MouseQueue::new());

pub fn init() -> bool {
    flush_output();

    if !write_command(CMD_ENABLE_AUX) {
        serial::write_line(b"[mouse] failed to enable PS/2 auxiliary port");
        return false;
    }

    if !write_command(CMD_READ_CONFIG) {
        serial::write_line(b"[mouse] failed to read PS/2 controller config");
        return false;
    }
    let Some(mut config) = read_data_timeout() else {
        serial::write_line(b"[mouse] no PS/2 controller config byte");
        return false;
    };
    config |= 1 << 1;
    config &= !(1 << 5);

    if !write_command(CMD_WRITE_CONFIG) || !write_data(config) {
        serial::write_line(b"[mouse] failed to write PS/2 controller config");
        return false;
    }

    if !send_mouse_command(MOUSE_RESET_DEFAULTS) {
        serial::write_line(b"[mouse] mouse reset-defaults not acknowledged");
        return false;
    }
    drain_aux_output();
    if !send_mouse_command(MOUSE_ENABLE_STREAMING) {
        serial::write_line(b"[mouse] mouse enable-streaming not acknowledged");
        return false;
    }
    drain_aux_output();

    serial::write_line(b"[mouse] PS/2 mouse online");
    true
}

fn push_packet_byte(mouse: &mut MouseQueue, byte: u8) -> bool {
    if mouse.packet_len == 0 && byte & 0x08 == 0 {
        return false;
    }

    let packet_len = mouse.packet_len;
    mouse.packet[packet_len] = byte;
    mouse.packet_len += 1;
    if mouse.packet_len < 3 {
        return false;
    }

    let packet = mouse.packet;
    mouse.packet_len = 0;
    decode_packet(mouse, packet)
}

fn pump_available_aux_bytes() -> bool {
    interrupts::without_interrupts(|| {
        let mut mouse = MOUSE.lock();
        let mut produced_event = false;
        loop {
            let status = unsafe { inb(STATUS_PORT) };
            if status & STATUS_OUTPUT_READY == 0 || status & STATUS_AUX_OUTPUT_READY == 0 {
                break;
            }

            let byte = unsafe { inb(DATA_PORT) };
            produced_event |= push_packet_byte(&mut mouse, byte);
        }
        produced_event
    })
}

pub fn handle_interrupt() {
    if pump_available_aux_bytes() {
        crate::sched::notify_interactive_input();
    }
}

pub fn poll_input() {
    if pump_available_aux_bytes() {
        crate::sched::notify_interactive_input();
    }
}

pub fn try_read_event() -> Option<MouseEvent> {
    interrupts::without_interrupts(|| MOUSE.lock().pop_event()).or_else(|| {
        if pump_available_aux_bytes() {
            interrupts::without_interrupts(|| MOUSE.lock().pop_event())
        } else {
            None
        }
    })
}

pub fn has_pending_event() -> bool {
    interrupts::without_interrupts(|| MOUSE.lock().len != 0) || {
        let status = unsafe { inb(STATUS_PORT) };
        status & STATUS_OUTPUT_READY != 0 && status & STATUS_AUX_OUTPUT_READY != 0
    }
}

fn decode_packet(mouse: &mut MouseQueue, packet: [u8; 3]) -> bool {
    let buttons = packet[0] & 0x07;
    let old_buttons = mouse.buttons;
    mouse.buttons = buttons;
    let mut produced_event = false;

    if packet[0] & 0x40 == 0 && packet[0] & 0x80 == 0 {
        let dx = sign_extend(packet[1], packet[0] & 0x10 != 0);
        let dy = -sign_extend(packet[2], packet[0] & 0x20 != 0);
        if dx != 0 || dy != 0 {
            mouse.push_event(MouseEvent::Move { dx, dy });
            produced_event = true;
        }
    }

    for (mask, button) in [
        (0x01u8, MouseButton::Left),
        (0x02u8, MouseButton::Right),
        (0x04u8, MouseButton::Middle),
    ] {
        let was_pressed = old_buttons & mask != 0;
        let is_pressed = buttons & mask != 0;
        if was_pressed != is_pressed {
            mouse.push_event(MouseEvent::Button {
                button,
                pressed: is_pressed,
            });
            produced_event = true;
        }
    }

    produced_event
}

fn sign_extend(value: u8, negative: bool) -> i16 {
    if negative {
        (value as i16) - 256
    } else {
        value as i16
    }
}

fn send_mouse_command(command: u8) -> bool {
    for _ in 0..4 {
        if !write_command(CMD_WRITE_AUX) || !write_data(command) {
            return false;
        }
        for _ in 0..32 {
            match read_data_timeout() {
                Some(ACK) => return true,
                Some(RESEND) => break,
                Some(_) => continue,
                None => break,
            }
        }
    }
    false
}

fn flush_output() {
    for _ in 0..32 {
        let status = unsafe { inb(STATUS_PORT) };
        if status & STATUS_OUTPUT_READY == 0 {
            break;
        }
        let _ = unsafe { inb(DATA_PORT) };
    }
}

fn drain_aux_output() {
    for _ in 0..32 {
        let status = unsafe { inb(STATUS_PORT) };
        if status & STATUS_OUTPUT_READY == 0 || status & STATUS_AUX_OUTPUT_READY == 0 {
            break;
        }
        let _ = unsafe { inb(DATA_PORT) };
    }
}

fn write_command(command: u8) -> bool {
    if !wait_write_ready() {
        return false;
    }
    unsafe { outb(COMMAND_PORT, command) };
    true
}

fn write_data(value: u8) -> bool {
    if !wait_write_ready() {
        return false;
    }
    unsafe { outb(DATA_PORT, value) };
    true
}

fn read_data_timeout() -> Option<u8> {
    if !wait_read_ready() {
        return None;
    }
    Some(unsafe { inb(DATA_PORT) })
}

fn wait_write_ready() -> bool {
    for _ in 0..100_000 {
        if unsafe { inb(STATUS_PORT) } & STATUS_INPUT_BUSY == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn wait_read_ready() -> bool {
    for _ in 0..100_000 {
        if unsafe { inb(STATUS_PORT) } & STATUS_OUTPUT_READY != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}
