// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Spring physics and keyframe animation.

extern crate alloc;
use crate::math::{Quat, Vec3};
use alloc::vec::Vec;

// ── Animation targets ─────────────────────────────────────────────────────────

/// Which property a `Timeline` or `Spring` drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimTarget {
    TranslationX,
    TranslationY,
    TranslationZ,
    Translation,  // drives all 3 axes
    RotationQuat, // drives quaternion
    ScaleX,
    ScaleY,
    ScaleZ,
    Scale, // uniform scale
    Opacity,
    BlurSigma,
    Custom(u32),
}

// ── Spring ────────────────────────────────────────────────────────────────────

/// Critically-damped spring for a single scalar.
///
/// `update(dt)` should be called once per frame.  The spring converges to
/// `target` with the given `stiffness` and `damping` ratio.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    pub stiffness: f32,
    pub damping: f32,
}

impl Spring {
    /// Create a critically-damped spring.
    pub fn new(initial: f32, stiffness: f32) -> Self {
        let damping = 2.0 * libm::sqrtf(stiffness);
        Self {
            value: initial,
            velocity: 0.0,
            target: initial,
            stiffness,
            damping,
        }
    }

    /// Common presets.
    pub const fn snappy() -> Self {
        Self {
            value: 0.0,
            velocity: 0.0,
            target: 0.0,
            stiffness: 350.0,
            damping: 35.0,
        }
    }
    pub const fn smooth() -> Self {
        Self {
            value: 0.0,
            velocity: 0.0,
            target: 0.0,
            stiffness: 150.0,
            damping: 22.0,
        }
    }
    pub const fn bouncy() -> Self {
        Self {
            value: 0.0,
            velocity: 0.0,
            target: 0.0,
            stiffness: 300.0,
            damping: 10.0,
        }
    }
    pub const fn gentle() -> Self {
        Self {
            value: 0.0,
            velocity: 0.0,
            target: 0.0,
            stiffness: 60.0,
            damping: 14.0,
        }
    }

    /// Step the spring forward by `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        // Semi-implicit Euler.
        let err = self.value - self.target;
        let accel = -self.stiffness * err - self.damping * self.velocity;
        self.velocity += accel * dt;
        self.value += self.velocity * dt;
    }

    pub fn set_target(&mut self, t: f32) {
        self.target = t;
    }

    /// Returns true once the spring has settled (< 0.1px / 0.01 velocity).
    pub fn is_settled(&self) -> bool {
        (self.value - self.target).abs() < 0.1 && self.velocity.abs() < 0.01
    }

    /// Snap immediately to target.
    pub fn snap(&mut self) {
        self.value = self.target;
        self.velocity = 0.0;
    }
}

// ── Spring3 ───────────────────────────────────────────────────────────────────

/// Three independent springs driving a `Vec3`.
#[derive(Debug, Clone, Copy)]
pub struct Spring3 {
    pub x: Spring,
    pub y: Spring,
    pub z: Spring,
}

impl Spring3 {
    pub fn new(initial: Vec3, stiffness: f32) -> Self {
        Self {
            x: Spring::new(initial.x, stiffness),
            y: Spring::new(initial.y, stiffness),
            z: Spring::new(initial.z, stiffness),
        }
    }

    pub fn snappy_at(p: Vec3) -> Self {
        let mut s = Self::new(p, 350.0);
        s.x.damping = 35.0;
        s.y.damping = 35.0;
        s.z.damping = 35.0;
        s
    }

    /// Smooth preset (wraps `Spring::smooth()`).
    pub fn smooth() -> Self {
        Self {
            x: Spring::smooth(),
            y: Spring::smooth(),
            z: Spring::smooth(),
        }
    }

    pub fn set_target(&mut self, t: Vec3) {
        self.x.set_target(t.x);
        self.y.set_target(t.y);
        self.z.set_target(t.z);
    }

    pub fn value(&self) -> Vec3 {
        Vec3::new(self.x.value, self.y.value, self.z.value)
    }

    pub fn update(&mut self, dt: f32) {
        self.x.update(dt);
        self.y.update(dt);
        self.z.update(dt);
    }

    pub fn is_settled(&self) -> bool {
        self.x.is_settled() && self.y.is_settled() && self.z.is_settled()
    }

    pub fn snap(&mut self) {
        self.x.snap();
        self.y.snap();
        self.z.snap();
    }
}

// ── Keyframe animation ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum EasingFn {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    Spring { stiffness: f32, damping: f32 },
}

impl EasingFn {
    pub fn apply(self, t: f32) -> f32 {
        match self {
            Self::Linear => t,
            Self::EaseIn => t * t,
            Self::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::EaseInOut => {
                let t2 = t * 2.0;
                if t < 0.5 {
                    t2 * t2 * 0.5
                } else {
                    let u = t2 - 2.0;
                    1.0 - u * u * 0.5
                }
            }
            Self::Spring { .. } => t, // handled by Spring directly
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Keyframe {
    /// Time in seconds from animation start.
    pub time: f32,
    pub value: f32,
    pub easing: EasingFn,
}

/// A scalar keyframe animation track.
pub struct Timeline {
    pub target: AnimTarget,
    pub keys: Vec<Keyframe>,
    pub looping: bool,
    pub current_time: f32,
}

impl Timeline {
    pub fn new(target: AnimTarget) -> Self {
        Self {
            target,
            keys: Vec::new(),
            looping: false,
            current_time: 0.0,
        }
    }

    pub fn add_key(&mut self, time: f32, value: f32, easing: EasingFn) {
        let key = Keyframe {
            time,
            value,
            easing,
        };
        // Insert sorted by time.
        let pos = self.keys.partition_point(|k| k.time <= time);
        self.keys.insert(pos, key);
    }

    pub fn duration(&self) -> f32 {
        self.keys.last().map(|k| k.time).unwrap_or(0.0)
    }

    /// Step forward and return the current interpolated value.
    pub fn tick(&mut self, dt: f32) -> f32 {
        self.current_time += dt;
        if self.looping {
            let dur = self.duration();
            if dur > 0.0 {
                self.current_time %= dur;
            }
        }
        self.sample(self.current_time)
    }

    pub fn sample(&self, t: f32) -> f32 {
        if self.keys.is_empty() {
            return 0.0;
        }
        let t = t.max(0.0).min(self.duration());
        // Find surrounding keyframes.
        let next = self.keys.partition_point(|k| k.time < t);
        if next == 0 {
            return self.keys[0].value;
        }
        if next >= self.keys.len() {
            return self.keys[self.keys.len() - 1].value;
        }
        let a = &self.keys[next - 1];
        let b = &self.keys[next];
        let span = (b.time - a.time).max(1e-6);
        let local_t = (t - a.time) / span;
        let eased_t = a.easing.apply(local_t);
        a.value + (b.value - a.value) * eased_t
    }

    pub fn is_finished(&self) -> bool {
        !self.looping && self.current_time >= self.duration()
    }
}
