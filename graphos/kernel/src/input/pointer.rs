// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PointerEvent {
    Move { dx: i16, dy: i16 },
    Absolute { x: i32, y: i32 },
    Button { button: MouseButton, pressed: bool },
}

#[derive(Clone, Copy)]
struct DriverPointerState {
    width: u32,
    height: u32,
    abs_x: i32,
    abs_y: i32,
}

impl DriverPointerState {
    const fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            abs_x: 0,
            abs_y: 0,
        }
    }
}

static DRIVER_POINTER_STATE: spin::Mutex<DriverPointerState> =
    spin::Mutex::new(DriverPointerState::new());

#[derive(Clone, Copy, PartialEq, Eq)]
enum PointerBackend {
    None,
    VirtioArch,
    VirtioDriver,
    Ps2,
}

#[derive(Clone, Copy)]
struct PointerState {
    selected: PointerBackend,
    virtio_arch_ready: bool,
    virtio_driver_ready: bool,
    ps2_ready: bool,
}

impl PointerState {
    const fn new() -> Self {
        Self {
            selected: PointerBackend::None,
            virtio_arch_ready: false,
            virtio_driver_ready: false,
            ps2_ready: false,
        }
    }

    fn any_ready(&self) -> bool {
        self.virtio_arch_ready || self.virtio_driver_ready || self.ps2_ready
    }
}

static POINTER_STATE: spin::Mutex<PointerState> = spin::Mutex::new(PointerState::new());

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;

fn scale_abs(raw: i32, dim: u32) -> i32 {
    if dim <= 1 {
        return 0;
    }
    let max = dim.saturating_sub(1) as i32;
    if raw >= 0 && raw <= max {
        return raw;
    }

    let clamped = raw.clamp(0, 32_767) as i64;
    ((clamped * max as i64) / 32_767) as i32
}

pub fn init(display_width: u32, display_height: u32) -> bool {
    crate::input::diagnostics::init(display_width, display_height);
    {
        let mut state = DRIVER_POINTER_STATE.lock();
        state.width = display_width;
        state.height = display_height;
        state.abs_x = (display_width / 2) as i32;
        state.abs_y = (display_height / 2) as i32;
    }

    crate::arch::x86_64::virtio_input::set_display_bounds(display_width, display_height);
    let virtio_arch_ready = crate::arch::x86_64::virtio_input::init();
    let virtio_driver_ready = crate::drivers::input::virtio_input::is_present();
    let ps2_ready = crate::arch::x86_64::mouse::init();

    {
        let mut state = POINTER_STATE.lock();
        *state = PointerState {
            selected: PointerBackend::None,
            virtio_arch_ready,
            virtio_driver_ready,
            ps2_ready,
        };
    }

    if virtio_arch_ready || virtio_driver_ready || ps2_ready {
        crate::input::diagnostics::set_pointer_online(true);
        if virtio_arch_ready && ps2_ready {
            crate::input::diagnostics::set_pointer_backend(b"virtio-input-arch+ps2");
            crate::arch::serial::write_line(
                b"[pointer] backends armed: virtio-input-arch + ps2-mouse",
            );
        } else if virtio_arch_ready {
            crate::input::diagnostics::set_pointer_backend(b"virtio-input-arch");
            crate::arch::serial::write_line(b"[pointer] backend: virtio-input-arch");
        } else if virtio_driver_ready && ps2_ready {
            crate::input::diagnostics::set_pointer_backend(b"virtio-driver+ps2");
            crate::arch::serial::write_line(
                b"[pointer] backends armed: virtio-input-driver + ps2-mouse",
            );
        } else if virtio_driver_ready {
            crate::input::diagnostics::set_pointer_backend(b"virtio-input-driver");
            crate::arch::serial::write_line(b"[pointer] backend: virtio-input-driver");
        } else {
            crate::input::diagnostics::set_pointer_backend(b"ps2-mouse");
            crate::arch::serial::write_line(b"[pointer] backend: ps2-mouse");
        }
        return true;
    }

    *POINTER_STATE.lock() = PointerState::new();
    crate::input::diagnostics::set_pointer_backend(b"none");
    crate::input::diagnostics::set_pointer_online(false);
    crate::arch::serial::write_line(b"[pointer] no supported tablet/pointer backend detected");
    false
}

pub fn set_display_bounds(width: u32, height: u32) {
    {
        let mut state = DRIVER_POINTER_STATE.lock();
        state.width = width;
        state.height = height;
        state.abs_x = state.abs_x.clamp(0, width.saturating_sub(1) as i32);
        state.abs_y = state.abs_y.clamp(0, height.saturating_sub(1) as i32);
    }
    crate::arch::x86_64::virtio_input::set_display_bounds(width, height);
}

pub fn try_read_event() -> Option<PointerEvent> {
    if let Some(event) = try_selected_backend_event() {
        return Some(event);
    }

    let state = *POINTER_STATE.lock();
    for backend in [
        PointerBackend::VirtioArch,
        PointerBackend::Ps2,
        PointerBackend::VirtioDriver,
    ] {
        if backend == state.selected || !backend_ready(state, backend) {
            continue;
        }
        if let Some(event) = try_backend_event(backend) {
            select_backend(backend);
            return Some(event);
        }
    }

    None
}

pub fn has_pending_event() -> bool {
    let state = *POINTER_STATE.lock();
    if state.selected != PointerBackend::None {
        return backend_has_pending(state.selected);
    }

    (state.virtio_arch_ready && backend_has_pending(PointerBackend::VirtioArch))
        || (state.ps2_ready && backend_has_pending(PointerBackend::Ps2))
        || (state.virtio_driver_ready && backend_has_pending(PointerBackend::VirtioDriver))
}

pub fn irq_line() -> Option<u8> {
    let state = *POINTER_STATE.lock();
    if state.virtio_arch_ready {
        return crate::arch::x86_64::virtio_input::irq_line();
    }
    if state.virtio_driver_ready {
        return crate::drivers::input::virtio_input::irq_line();
    }
    if state.ps2_ready {
        return Some(12);
    }
    None
}

pub fn handle_irq(irq: u8) -> bool {
    let state = *POINTER_STATE.lock();
    let mut handled = false;
    if state.virtio_arch_ready {
        handled |= crate::arch::x86_64::virtio_input::handle_irq(irq);
    }
    if state.ps2_ready && irq == 12 {
        crate::arch::x86_64::mouse::handle_interrupt();
        handled = true;
    }
    if state.virtio_driver_ready && crate::drivers::input::virtio_input::irq_line() == Some(irq) {
        crate::drivers::input::virtio_input::handle_irq();
        handled = true;
    }
    handled
}

pub fn poll_input() {
    let state = *POINTER_STATE.lock();
    if state.virtio_arch_ready {
        crate::arch::x86_64::virtio_input::poll_input();
    }
    if state.ps2_ready {
        crate::arch::x86_64::mouse::poll_input();
    }
    if state.virtio_driver_ready {
        crate::drivers::input::virtio_input::poll_input();
    }
}

fn try_selected_backend_event() -> Option<PointerEvent> {
    let selected = POINTER_STATE.lock().selected;
    if selected == PointerBackend::None {
        return None;
    }

    if let Some(event) = try_backend_event(selected) {
        return Some(event);
    }

    None
}

fn backend_ready(state: PointerState, backend: PointerBackend) -> bool {
    match backend {
        PointerBackend::VirtioArch => state.virtio_arch_ready,
        PointerBackend::VirtioDriver => state.virtio_driver_ready,
        PointerBackend::Ps2 => state.ps2_ready,
        PointerBackend::None => false,
    }
}

fn backend_has_pending(backend: PointerBackend) -> bool {
    match backend {
        PointerBackend::VirtioArch => crate::arch::x86_64::virtio_input::has_pending_event(),
        PointerBackend::VirtioDriver => crate::drivers::input::virtio_input::has_pending_event(),
        PointerBackend::Ps2 => crate::arch::x86_64::mouse::has_pending_event(),
        PointerBackend::None => false,
    }
}

fn try_backend_event(backend: PointerBackend) -> Option<PointerEvent> {
    match backend {
        PointerBackend::VirtioArch => crate::arch::x86_64::virtio_input::try_read_event(),
        PointerBackend::Ps2 => try_ps2_event(),
        PointerBackend::VirtioDriver => try_driver_event(),
        PointerBackend::None => None,
    }
}

fn try_ps2_event() -> Option<PointerEvent> {
    let event = crate::arch::x86_64::mouse::try_read_event()?;
    Some(match event {
        crate::arch::x86_64::mouse::MouseEvent::Move { dx, dy } => PointerEvent::Move { dx, dy },
        crate::arch::x86_64::mouse::MouseEvent::Button { button, pressed } => {
            let button = match button {
                crate::arch::x86_64::mouse::MouseButton::Left => MouseButton::Left,
                crate::arch::x86_64::mouse::MouseButton::Right => MouseButton::Right,
                crate::arch::x86_64::mouse::MouseButton::Middle => MouseButton::Middle,
            };
            PointerEvent::Button { button, pressed }
        }
    })
}

fn try_driver_event() -> Option<PointerEvent> {
    while let Some(event) = crate::drivers::input::virtio_input::poll_event() {
        match event.typ {
            crate::drivers::input::virtio_input::EV_REL => match event.code {
                REL_X => {
                    let dx = event.value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                    return Some(PointerEvent::Move { dx, dy: 0 });
                }
                REL_Y => {
                    let dy = event.value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                    return Some(PointerEvent::Move { dx: 0, dy });
                }
                _ => {}
            },
            crate::drivers::input::virtio_input::EV_ABS => {
                let mut state = DRIVER_POINTER_STATE.lock();
                match event.code {
                    ABS_X => state.abs_x = scale_abs(event.value, state.width),
                    ABS_Y => state.abs_y = scale_abs(event.value, state.height),
                    _ => continue,
                }
                return Some(PointerEvent::Absolute {
                    x: state.abs_x,
                    y: state.abs_y,
                });
            }
            crate::drivers::input::virtio_input::EV_KEY => {
                let button = match event.code {
                    BTN_LEFT => MouseButton::Left,
                    BTN_RIGHT => MouseButton::Right,
                    BTN_MIDDLE => MouseButton::Middle,
                    _ => continue,
                };
                return Some(PointerEvent::Button {
                    button,
                    pressed: event.value != 0,
                });
            }
            _ => {}
        }
    }
    None
}

fn select_backend(backend: PointerBackend) {
    let mut state = POINTER_STATE.lock();
    if state.selected == backend {
        return;
    }
    state.selected = backend;
    drop(state);

    match backend {
        PointerBackend::VirtioArch => {
            crate::input::diagnostics::set_pointer_backend(b"virtio-input-arch");
            crate::arch::serial::write_line(b"[pointer] active backend: virtio-input-arch");
        }
        PointerBackend::VirtioDriver => {
            crate::input::diagnostics::set_pointer_backend(b"virtio-input-driver");
            crate::arch::serial::write_line(b"[pointer] active backend: virtio-input-driver");
        }
        PointerBackend::Ps2 => {
            crate::input::diagnostics::set_pointer_backend(b"ps2-mouse");
            crate::arch::serial::write_line(b"[pointer] active backend: ps2-mouse");
        }
        PointerBackend::None => {}
    }
}
