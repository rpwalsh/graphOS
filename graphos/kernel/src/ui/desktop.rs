// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use spin::Mutex;

use crate::bootinfo::{BootInfo, FramebufferFormat};
use crate::drivers::display;
use crate::drivers::gpu::virtio_gpu;
use crate::gfx::surface::FramebufferConfig;
use crate::input::event::{InputEvent, MouseButton, has_pending_input, poll_event};
use crate::ipc;
use crate::registry;
use crate::userland;
use crate::wm::gpu_compositor;

static SCREEN_CONFIG: Mutex<Option<FramebufferConfig>> = Mutex::new(None);
const GPU_DESKTOP_CUTOVER: bool = true;
const GPU_DESKTOP_STRICT: bool = true;
const INPUT_BATCH_LIMIT: usize = 256;
const DISPLAY_RENDER_TICKS: u64 = 16;
const DISPLAY_SCENE_POLL_TICKS: u64 = 250;
const DISPLAY_BOOTSTRAP_TICKS: u64 = 1000;
const DISPLAY_HANDOFF_TICKS: u64 = 2000;
const DISPLAY_TIMER_STALL_STEPS: u32 = 2048;

static RUNTIME_DISPLAY_CLAIMED: AtomicBool = AtomicBool::new(false);

const MAX_FRAME_TICK_SUBSCRIBERS: usize = 64;
static FRAME_TICK_CHANNELS: Mutex<[u32; MAX_FRAME_TICK_SUBSCRIBERS]> =
    Mutex::new([0u32; MAX_FRAME_TICK_SUBSCRIBERS]);
static FRAME_TICK_NOW_MS: AtomicU64 = AtomicU64::new(0);
static FRAME_TICK_NEXT_DEADLINE_MS: AtomicU64 = AtomicU64::new(0);
static FRAME_TICK_SUBSCRIBED: AtomicBool = AtomicBool::new(false);
static FRAME_TICK_DELIVERED: AtomicBool = AtomicBool::new(false);
static SCENE_POLL_PENDING: AtomicBool = AtomicBool::new(false);
static BOOTSTRAP_POLL_PENDING: AtomicBool = AtomicBool::new(false);
static HANDOFF_STATUS_PENDING: AtomicBool = AtomicBool::new(false);
static STARTUP_TEST_FAILURES: AtomicU32 = AtomicU32::new(0);
static STARTUP_PROTECTED_OK: AtomicBool = AtomicBool::new(false);

fn mark_frame_tick_delivered() {
    if !FRAME_TICK_DELIVERED.swap(true, Ordering::AcqRel) {
        crate::arch::serial::write_line(b"[desktop] compositor frame clock active\n");
    }
    HANDOFF_STATUS_PENDING.store(true, Ordering::Release);
    crate::sched::wake_desktop_task();
}

fn frame_tick_prune_stale(subs: &mut [u32; MAX_FRAME_TICK_SUBSCRIBERS]) {
    for alias in subs.iter_mut() {
        if *alias == 0 {
            continue;
        }
        let uuid = crate::ipc::channel::uuid_for_alias(*alias);
        if !crate::ipc::channel::is_active(uuid) {
            *alias = 0;
        }
    }
}

enum FrameTickDispatch {
    Delivered,
    Backpressured,
    Inactive,
}

fn send_frame_tick(alias: u32, payload: &[u8; 8]) -> FrameTickDispatch {
    let uuid = crate::ipc::channel::uuid_for_alias(alias);
    if !crate::ipc::channel::is_active(uuid) {
        return FrameTickDispatch::Inactive;
    }
    if crate::ipc::channel::send_and_wake(uuid, crate::ipc::msg::MsgTag::FrameTick, 0, payload) {
        FrameTickDispatch::Delivered
    } else {
        // A full queue is transient backpressure, not a dead subscriber.
        FrameTickDispatch::Backpressured
    }
}

pub fn subscribe_frame_tick(channel_alias: u32) -> bool {
    if channel_alias == 0 {
        return false;
    }
    let mut subs = FRAME_TICK_CHANNELS.lock();
    frame_tick_prune_stale(&mut subs);
    if subs.iter().any(|&c| c == channel_alias) {
        FRAME_TICK_SUBSCRIBED.store(true, Ordering::Release);
        drop(subs);
        kick_frame_tick_channel(channel_alias);
        return true;
    }
    if let Some(slot) = subs.iter_mut().find(|c| **c == 0) {
        *slot = channel_alias;
        FRAME_TICK_SUBSCRIBED.store(true, Ordering::Release);
        drop(subs);
        kick_frame_tick_channel(channel_alias);
        true
    } else {
        false
    }
}

fn kick_frame_tick_channel(channel_alias: u32) {
    let now_ms = FRAME_TICK_NOW_MS
        .load(Ordering::Acquire)
        .max(crate::arch::timer::ticks());
    FRAME_TICK_NEXT_DEADLINE_MS.store(
        now_ms.saturating_add(DISPLAY_RENDER_TICKS),
        Ordering::Release,
    );
    let payload = now_ms.to_le_bytes();
    if matches!(send_frame_tick(channel_alias, &payload), FrameTickDispatch::Delivered) {
        mark_frame_tick_delivered();
    }
    HANDOFF_STATUS_PENDING.store(true, Ordering::Release);
    crate::sched::wake_desktop_task();
}

pub fn signal_render_tick(now_ms: u64) {
    FRAME_TICK_NOW_MS.store(now_ms, Ordering::Release);
    let payload = now_ms.to_le_bytes();
    let mut delivered = false;
    let mut subs = FRAME_TICK_CHANNELS.lock();
    for alias in subs.iter_mut() {
        if *alias == 0 {
            continue;
        }
        match send_frame_tick(*alias, &payload) {
            FrameTickDispatch::Delivered => delivered = true,
            FrameTickDispatch::Inactive => *alias = 0,
            FrameTickDispatch::Backpressured => {}
        }
    }
    if delivered {
        mark_frame_tick_delivered();
    }
}

pub fn pump_frame_clock_from_timer(now_ms: u64) {
    FRAME_TICK_NOW_MS.store(now_ms, Ordering::Release);

    if !FRAME_TICK_SUBSCRIBED.load(Ordering::Acquire) {
        FRAME_TICK_NEXT_DEADLINE_MS.store(
            now_ms.saturating_add(DISPLAY_RENDER_TICKS),
            Ordering::Release,
        );
        return;
    }

    let mut next_due = FRAME_TICK_NEXT_DEADLINE_MS.load(Ordering::Acquire);
    if next_due == 0 {
        next_due = now_ms;
    }
    if now_ms < next_due {
        return;
    }

    let Some(mut subs) = FRAME_TICK_CHANNELS.try_lock() else {
        return;
    };

    while next_due <= now_ms {
        let payload = next_due.to_le_bytes();
        let mut delivered = false;
        for alias in subs.iter_mut() {
            if *alias == 0 {
                continue;
            }
            match send_frame_tick(*alias, &payload) {
                FrameTickDispatch::Delivered => delivered = true,
                FrameTickDispatch::Inactive => *alias = 0,
                FrameTickDispatch::Backpressured => {}
            }
        }
        if delivered {
            mark_frame_tick_delivered();
        }
        next_due = next_due.saturating_add(DISPLAY_RENDER_TICKS);
    }

    drop(subs);
    FRAME_TICK_NEXT_DEADLINE_MS.store(next_due, Ordering::Release);
}

pub fn signal_scene_poll() {
    SCENE_POLL_PENDING.store(true, Ordering::Release);
}

pub fn signal_bootstrap_poll() {
    BOOTSTRAP_POLL_PENDING.store(true, Ordering::Release);
}

pub fn signal_handoff_status() {
    HANDOFF_STATUS_PENDING.store(true, Ordering::Release);
}

pub fn runtime_display_claimed() -> bool {
    RUNTIME_DISPLAY_CLAIMED.load(Ordering::Acquire)
}

pub fn frame_clock_armed() -> bool {
    runtime_display_claimed() || FRAME_TICK_SUBSCRIBED.load(Ordering::Acquire)
}

fn compositor_registry_live(binding: &registry::RegistryLookup) -> bool {
    binding.channel_alias != 0
        && ipc::channel::is_active(binding.channel_uuid)
        && matches!(
            binding.health,
            crate::registry::RegistryHealth::Launched
                | crate::registry::RegistryHealth::Ready
                | crate::registry::RegistryHealth::Degraded,
        )
}

pub fn claim_runtime_display(surface_id: u32) -> bool {
    crate::arch::serial::write_line(b"[desktop][claim] ENTRY\n");
    if surface_id == 0 {
        crate::arch::serial::write_line(b"[desktop][claim] FAILED: surface_id=0\n");
        return false;
    }

    let fb = virtio_gpu::framebuffer_addr();
    let (w, h) = virtio_gpu::resolution();
    if fb == 0 || w == 0 || h == 0 {
        crate::arch::serial::write_line(b"[desktop][claim] FAILED: GPU framebuffer not ready\n");
        return false;
    }
    crate::arch::serial::write_line(b"[desktop][claim] GPU framebuffer verified\n");

    crate::arch::serial::write_line(b"[desktop][claim] calling set_background_surface()\n");
    gpu_compositor::set_background_surface(surface_id);
    crate::arch::serial::write_line(b"[desktop][claim] calling gpu_compositor::init()\n");
    gpu_compositor::init(w, h);
    crate::arch::serial::write_line(b"[desktop][claim] gpu_compositor::init() COMPLETED\n");

    crate::arch::serial::write_line(b"[desktop][claim] swapping RUNTIME_DISPLAY_CLAIMED atomic\n");
    if !RUNTIME_DISPLAY_CLAIMED.swap(true, Ordering::AcqRel) {
        crate::arch::serial::write_line(
            b"[desktop][claim] RUNTIME_DISPLAY_CLAIMED set=true, attaching runtime FB\n",
        );
        display::attach_runtime_framebuffer(fb, w, h, w, FramebufferFormat::Bgr);
        crate::input::pointer::set_display_bounds(w, h);
        crate::arch::serial::write_line(
            b"[desktop] display ownership transition: boot-fb -> virtio-gpu runtime\n",
        );
    } else {
        crate::arch::serial::write_line(b"[desktop][claim] display already claimed (race)\n");
    }

    crate::arch::serial::write_line(b"[desktop][claim] RETURNING true\n");
    true
}

pub fn init_screen(info: &BootInfo) {
    display::attach_boot_framebuffer(info);
    *SCREEN_CONFIG.lock() = Some(FramebufferConfig {
        addr: info.framebuffer_addr,
        width: info.framebuffer_width,
        height: info.framebuffer_height,
        stride: info.framebuffer_stride,
        format: info.framebuffer_format,
    });
}

pub fn spawn_task(test_failures: u32, protected_ok: bool) -> bool {
    STARTUP_TEST_FAILURES.store(test_failures, Ordering::Release);
    STARTUP_PROTECTED_OK.store(protected_ok, Ordering::Release);
    FRAME_TICK_NEXT_DEADLINE_MS.store(0, Ordering::Release);
    FRAME_TICK_SUBSCRIBED.store(false, Ordering::Release);
    FRAME_TICK_DELIVERED.store(false, Ordering::Release);

    match crate::task::table::create_kernel_task_with_index(
        b"display",
        display_task_entry as *const () as u64,
    ) {
        Some((id, index)) => {
            crate::sched::register_desktop_task(index);
            let _ = crate::task::table::set_priority(index, 3);
            crate::graph::seed::register_task(b"display", crate::graph::types::NODE_ID_KERNEL);
            crate::arch::serial::write_bytes(b"[desktop] spawned dedicated display task id=");
            crate::arch::serial::write_u64_dec_inline(id);
            crate::arch::serial::write_bytes(b" idx=");
            crate::arch::serial::write_u64_dec(index as u64);
            true
        }
        None => false,
    }
}

fn display_task_entry() {
    let Some(config) = SCREEN_CONFIG.lock().as_ref().copied() else {
        crate::arch::serial::write_line(b"[desktop::task] starting\n");
        crate::arch::serial::write_line(b"[desktop] missing screen config\n");
        return;
    };

    let mut desktop = Desktop::new(
        config,
        STARTUP_TEST_FAILURES.load(Ordering::Acquire),
        STARTUP_PROTECTED_OK.load(Ordering::Acquire),
    );
    crate::arch::serial::write_line(b"[desktop::task] display task online\n");
    desktop.run();
}

struct Desktop {
    gpu_desktop_enabled: bool,
    gpu_required_unavailable: bool,
    compositor_handoff_complete: bool,
    cursor_x: i32,
    cursor_y: i32,
    pointer_buttons: u8,
    no_compositor_mode: bool,
    next_render_tick: u64,
    next_scene_poll_tick: u64,
    next_bootstrap_tick: u64,
    next_handoff_tick: u64,
    last_timer_tick: u64,
    synthetic_tick: u64,
    timer_stall_steps: u32,
    timer_stall_reported: bool,
}

impl Desktop {
    fn new(_config: FramebufferConfig, _test_failures: u32, _protected_ok: bool) -> Self {
        crate::arch::serial::write_line(b"[Desktop::new] starting\n");
        let virtio_present = virtio_gpu::is_present();
        let virtio_fb = virtio_gpu::framebuffer_addr();
        let gpu_desktop_enabled = GPU_DESKTOP_CUTOVER && virtio_present && virtio_fb != 0;
        let gpu_required_unavailable =
            GPU_DESKTOP_STRICT && GPU_DESKTOP_CUTOVER && !gpu_desktop_enabled;

        if gpu_required_unavailable || !gpu_desktop_enabled {
            crate::arch::serial::write_line(
                b"[desktop] FATAL: GPU-only mode requires virtio-gpu runtime scanout\n",
            );
            loop {
                unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
            }
        }

        let fb = virtio_gpu::framebuffer_addr();
        let (w, h) = virtio_gpu::resolution();
        if fb == 0 || w == 0 || h == 0 {
            crate::arch::serial::write_line(
                b"[desktop] FATAL: runtime framebuffer missing after GPU initialization\n",
            );
            loop {
                unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
            }
        }

        crate::input::pointer::set_display_bounds(w, h);

        if let Some(binding) = registry::lookup(b"compositor") {
            if compositor_registry_live(&binding) {
                crate::arch::serial::write_line(
                    b"[desktop] compositor already registered at boot\n",
                );
            }
        }

        let center_x = (w / 2) as i32;
        let center_y = (h / 2) as i32;
        let compositor_declared = userland::manifest_declares_service(b"compositor");

        let mut desktop = Self {
            gpu_desktop_enabled,
            gpu_required_unavailable,
            compositor_handoff_complete: !compositor_declared,
            cursor_x: center_x,
            cursor_y: center_y,
            pointer_buttons: 0,
            no_compositor_mode: !compositor_declared,
            next_render_tick: 0,
            next_scene_poll_tick: 0,
            next_bootstrap_tick: 0,
            next_handoff_tick: 0,
            last_timer_tick: 0,
            synthetic_tick: 0,
            timer_stall_steps: 0,
            timer_stall_reported: false,
        };
        if !compositor_declared {
            crate::arch::serial::write_line(
                b"[desktop] compositor not declared; cube direct-present mode\n",
            );
            if !RUNTIME_DISPLAY_CLAIMED.swap(true, Ordering::AcqRel) {
                display::attach_runtime_framebuffer(fb, w, h, w, FramebufferFormat::Bgr);
                crate::input::pointer::set_display_bounds(w, h);
                crate::arch::serial::write_line(
                    b"[desktop] runtime display attached for direct-present mode\n",
                );
            }
            if let Some(task_id) = userland::spawn_named_service(b"cube") {
                if let Some(idx) = crate::task::table::task_index_by_id(task_id) {
                    crate::syscall::register_compositor_task(idx);
                    crate::arch::serial::write_line(
                        b"[desktop] cube registered for GPU submit/direct present\n",
                    );
                }
                crate::arch::serial::write_line(b"[desktop] cube spawned\n");
            } else {
                crate::arch::serial::write_line(b"[desktop] WARNING: cube spawn failed\n");
            }
        }
        desktop.init_display_ticker();
        desktop.bootstrap_runtime_compositor(true);
        desktop.request_handoff_status(true);
        desktop
    }

    fn run(&mut self) {
        let mut started = false;
        loop {
            if !started {
                crate::arch::serial::write_line(b"[desktop] event loop started\n");
                started = true;
            }

            self.drive_display_ticker();

            if self.gpu_desktop_enabled && !runtime_display_claimed() {
                // Retry compositor bootstrap only on the desktop ticker's
                // periodic poll path. The compositor task is created Ready,
                // so repeatedly force-waking it before any IPC arrives only
                // injects synthetic wakeups into the ring3 handoff path.
                self.bootstrap_runtime_compositor(false);
            }

            self.maybe_log_handoff_status();

            let mut processed = 0usize;
            while processed < INPUT_BATCH_LIMIT {
                let Some(event) = poll_event() else {
                    break;
                };
                processed += 1;
                self.handle_event(event);
            }

            if !self.compositor_handoff_complete {
                // During handoff, stay event-driven instead of spin-yielding.
                // The compositor owns first present; the display task should
                // only wake often enough to advance timers and observe handoff.
                crate::sched::sleep_for_ticks(1);
                continue;
            }

            if has_pending_input() {
                unsafe { crate::sched::schedule() };
                continue;
            }

            // Sleep until the next desktop deadline instead of spin-yielding.
            // This keeps the display broker event-driven and stops it from
            // dominating the only CPU core during compositor handoff.
            crate::sched::sleep_for_ticks(self.sleep_ticks_until_next_deadline());
        }
    }

    fn handle_event(&mut self, event: InputEvent) {
        match event {
            InputEvent::Text(byte) => {
                crate::input::router::route_key_event(true, byte, 0);
            }
            InputEvent::Enter => {
                crate::input::router::route_key_event(true, b'\n', 0);
            }
            InputEvent::Backspace => {
                crate::input::router::route_key_event(true, 0x08, 0);
            }
            InputEvent::PointerMove(dx, dy) => {
                let (w, h) = virtio_gpu::resolution();
                let max_x = w.saturating_sub(1) as i32;
                let max_y = h.saturating_sub(1) as i32;
                self.cursor_x = self.cursor_x.saturating_add(dx as i32).clamp(0, max_x);
                self.cursor_y = self.cursor_y.saturating_add(dy as i32).clamp(0, max_y);
                gpu_compositor::set_cursor_pos(self.cursor_x, self.cursor_y);
                crate::input::router::route_pointer_event(
                    self.cursor_x,
                    self.cursor_y,
                    self.pointer_buttons,
                );
                self.present_direct_cursor_overlay();
            }
            InputEvent::PointerAbsolute(x, y) => {
                let (w, h) = virtio_gpu::resolution();
                let max_x = w.saturating_sub(1) as i32;
                let max_y = h.saturating_sub(1) as i32;
                self.cursor_x = x.clamp(0, max_x);
                self.cursor_y = y.clamp(0, max_y);
                gpu_compositor::set_cursor_pos(self.cursor_x, self.cursor_y);
                crate::input::router::route_pointer_event(
                    self.cursor_x,
                    self.cursor_y,
                    self.pointer_buttons,
                );
                self.present_direct_cursor_overlay();
            }
            InputEvent::PointerButton { button, pressed } => {
                let mask = match button {
                    MouseButton::Left => 0x01,
                    MouseButton::Right => 0x02,
                    MouseButton::Middle => 0x04,
                };
                if pressed {
                    self.pointer_buttons |= mask;
                } else {
                    self.pointer_buttons &= !mask;
                }
                gpu_compositor::set_cursor_pos(self.cursor_x, self.cursor_y);
                crate::input::router::route_pointer_event(
                    self.cursor_x,
                    self.cursor_y,
                    self.pointer_buttons,
                );
                self.present_direct_cursor_overlay();
            }
            _ => {}
        }
    }

    fn present_direct_cursor_overlay(&self) {
        if !self.no_compositor_mode || !runtime_display_claimed() {
            return;
        }
        if crate::wm::gpu_exec::direct_present_active() {
            virtio_gpu::draw_cursor_overlay(self.cursor_x, self.cursor_y, self.pointer_buttons);
            // Cursor overlay is tiny; flushing a local rect avoids scanout flicker.
            let fx = self.cursor_x.saturating_sub(8).max(0) as u32;
            let fy = self.cursor_y.saturating_sub(8).max(0) as u32;
            virtio_gpu::flush_rect(fx, fy, 28, 28);
            return;
        }

        let sid = gpu_compositor::background_surface_id();
        if sid == 0 {
            return;
        }

        let Some((w, h)) = crate::wm::surface_table::surface_dimensions(sid) else {
            return;
        };

        virtio_gpu::blit_surface_scene(sid, w as u32, h as u32, 0, 0, 1024, 255);
        virtio_gpu::draw_cursor_overlay(self.cursor_x, self.cursor_y, self.pointer_buttons);
        virtio_gpu::flush_rect(0, 0, w as u32, h as u32);
    }

    fn bootstrap_runtime_compositor(&mut self, force: bool) {
        if !force && !BOOTSTRAP_POLL_PENDING.swap(false, Ordering::AcqRel) {
            return;
        }

        if !userland::manifest_declares_service(b"compositor") {
            return;
        }

        if let Some(binding) = registry::lookup(b"compositor") {
            if compositor_registry_live(&binding) {
                return;
            }
        }

        if userland::spawn_named_service(b"compositor").is_none() {
            crate::arch::serial::write_line(b"[desktop] WARNING: compositor spawn retry failed\n");
        }
    }

    fn maybe_log_handoff_status(&mut self) {
        if !HANDOFF_STATUS_PENDING.swap(false, Ordering::AcqRel) {
            return;
        }
        if !runtime_display_claimed() {
            return;
        }
        if FRAME_TICK_SUBSCRIBED.load(Ordering::Acquire)
            && FRAME_TICK_DELIVERED.load(Ordering::Acquire)
        {
            if !self.compositor_handoff_complete {
                self.compositor_handoff_complete = true;
                crate::arch::serial::write_line(b"[desktop] compositor handoff complete\n");
            }
            return;
        }

        if let Some(binding) = registry::lookup(b"compositor") {
            if compositor_registry_live(&binding) {
                let line = if FRAME_TICK_SUBSCRIBED.load(Ordering::Acquire) {
                    b"[desktop] waiting for compositor frame-tick delivery\n" as &[u8]
                } else {
                    b"[desktop] waiting for compositor frame-clock subscription\n" as &[u8]
                };
                crate::arch::serial::write_line(line);
                return;
            }
        }

        crate::arch::serial::write_line(b"[desktop] waiting for compositor launch\n");
    }

    fn request_handoff_status(&mut self, _force: bool) {
        HANDOFF_STATUS_PENDING.store(true, Ordering::Release);
    }

    fn init_display_ticker(&mut self) {
        let now = crate::arch::timer::ticks();
        FRAME_TICK_NEXT_DEADLINE_MS.store(now, Ordering::Release);
        self.next_render_tick = now;
        self.next_scene_poll_tick = now;
        self.next_bootstrap_tick = now;
        self.next_handoff_tick = now;
        self.last_timer_tick = now;
        self.synthetic_tick = now;
        self.timer_stall_steps = 0;
        self.timer_stall_reported = false;
    }

    fn sample_display_ticks(&mut self) -> u64 {
        let now = crate::arch::timer::ticks();
        if now == self.last_timer_tick {
            self.timer_stall_steps = self.timer_stall_steps.saturating_add(1);
            self.synthetic_tick = self.synthetic_tick.saturating_add(1);
            if self.timer_stall_steps >= DISPLAY_TIMER_STALL_STEPS && !self.timer_stall_reported {
                crate::arch::serial::write_line(
                    b"[desktop] timer stall detected; using synthetic display ticks\n",
                );
                self.timer_stall_reported = true;
            }
            return self.synthetic_tick;
        }

        self.last_timer_tick = now;
        self.synthetic_tick = now;
        self.timer_stall_steps = 0;
        self.timer_stall_reported = false;
        now
    }

    fn drive_display_ticker(&mut self) {
        let now = self.sample_display_ticks();

        if now >= self.next_render_tick {
            if runtime_display_claimed() && !crate::wm::gpu_exec::direct_present_active() {
                gpu_compositor::frame_tick();
            }
            if self.no_compositor_mode && runtime_display_claimed() {
                self.present_direct_cursor_overlay();
            }
            self.next_render_tick = now.saturating_add(DISPLAY_RENDER_TICKS);
        }
        if now >= self.next_scene_poll_tick {
            signal_scene_poll();
            self.next_scene_poll_tick = now.saturating_add(DISPLAY_SCENE_POLL_TICKS);
        }
        if now >= self.next_bootstrap_tick {
            signal_bootstrap_poll();
            self.next_bootstrap_tick = now.saturating_add(DISPLAY_BOOTSTRAP_TICKS);
        }
        if now >= self.next_handoff_tick {
            signal_handoff_status();
            self.next_handoff_tick = now.saturating_add(DISPLAY_HANDOFF_TICKS);
        }
    }

    fn sleep_ticks_until_next_deadline(&self) -> u64 {
        let now = crate::arch::timer::ticks();
        let next_deadline = self
            .next_render_tick
            .min(self.next_scene_poll_tick)
            .min(self.next_bootstrap_tick)
            .min(self.next_handoff_tick);
        next_deadline.saturating_sub(now).max(1)
    }
}
