// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::arch::x86_64::keyboard::KeyInput;
use crate::input::pointer::PointerEvent as RawPointerEvent;

pub use crate::input::pointer::MouseButton;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    FocusNext,
    FocusPrev,
    Move(i32, i32),
    CloseFocused,
    SpawnTerminal,
    SpawnLog,
    Text(u8),
    Enter,
    Backspace,
    PointerMove(i16, i16),
    PointerAbsolute(i32, i32),
    PointerButton { button: MouseButton, pressed: bool },
}

pub fn poll_event() -> Option<InputEvent> {
    if let Some(event) = crate::input::pointer::try_read_event() {
        return Some(match event {
            RawPointerEvent::Move { dx, dy } => InputEvent::PointerMove(dx, dy),
            RawPointerEvent::Absolute { x, y } => InputEvent::PointerAbsolute(x, y),
            RawPointerEvent::Button { button, pressed } => {
                InputEvent::PointerButton { button, pressed }
            }
        });
    }

    match crate::arch::x86_64::keyboard::try_read_key()? {
        KeyInput::Tab { shift: false } => Some(InputEvent::FocusNext),
        KeyInput::Tab { shift: true } => Some(InputEvent::FocusPrev),
        KeyInput::Left => Some(InputEvent::Move(-12, 0)),
        KeyInput::Right => Some(InputEvent::Move(12, 0)),
        KeyInput::Up => Some(InputEvent::Move(0, -12)),
        KeyInput::Down => Some(InputEvent::Move(0, 12)),
        KeyInput::Backspace => Some(InputEvent::Backspace),
        KeyInput::Enter => Some(InputEvent::Enter),
        KeyInput::Char(b'q') => Some(InputEvent::CloseFocused),
        KeyInput::Char(b't') => Some(InputEvent::SpawnTerminal),
        KeyInput::Char(b'l') => Some(InputEvent::SpawnLog),
        KeyInput::Char(byte) => Some(InputEvent::Text(byte)),
    }
}

pub fn has_pending_input() -> bool {
    crate::input::pointer::has_pending_event() || crate::arch::x86_64::keyboard::has_pending_key()
}
