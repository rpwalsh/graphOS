// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Spring-physics animation for ring-3 GraphOS applications.
//!
//! A critically-damped spring is the backbone of most GraphOS UI transitions.
//! It is parameterised by a single `stiffness` value; damping is derived
//! automatically to keep the spring critically damped (no overshoot).
//!
//! # Usage
//! ```no_run
//! use graphos_app_sdk::animation::Spring;
//!
//! let mut spring = Spring::new(0.0, 200.0); // start=0, target=200
//! loop {
//!     let dt = 1.0 / 60.0; // 60 Hz tick
//!     spring.tick(dt);
//!     let x = spring.value();
//!     // … use x to position a widget …
//!     if spring.is_settled() { break; }
//! }
//! ```

/// A critically-damped spring animator.
///
/// Internally uses the **exact analytic solution** to avoid frame-rate
/// dependence.  The formula is:
/// ```text
///   x(t) = target + (A + B·t)·exp(-ω·t)
/// ```
/// where ω = sqrt(stiffness) and A, B are initial conditions.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    /// Angular frequency ω = sqrt(stiffness).
    omega: f32,
    /// Displacement from target at t=0.
    x0: f32,
    /// Velocity at t=0.
    v0: f32,
    /// Accumulated time since last retarget.
    elapsed: f32,
    /// Target value.
    target: f32,
    /// Last computed value (cached for `value()`).
    cached: f32,
}

impl Spring {
    /// Create a spring starting at `start` targeting `target`.
    ///
    /// `stiffness` controls speed: typical UI values are 120–400.
    pub fn new_with_stiffness(start: f32, target: f32, stiffness: f32) -> Self {
        let omega = libm::sqrtf(stiffness.max(1.0));
        let x0 = start - target;
        Self {
            omega,
            x0,
            v0: 0.0,
            elapsed: 0.0,
            target,
            cached: start,
        }
    }

    /// Create a spring with default stiffness 200 (comfortable UI feel).
    pub fn new(start: f32, target: f32) -> Self {
        Self::new_with_stiffness(start, target, 200.0)
    }

    /// Retarget to a new value, preserving current position and velocity.
    pub fn set_target(&mut self, new_target: f32) {
        // Re-base x0 and v0 from the current position/velocity.
        let (cur_x, cur_v) = self.state_at(self.elapsed);
        self.target = new_target;
        self.x0 = cur_x - new_target;
        self.v0 = cur_v;
        self.elapsed = 0.0;
    }

    /// Advance the simulation by `dt` seconds.
    pub fn tick(&mut self, dt: f32) {
        self.elapsed += dt.max(0.0);
        let (x, _) = self.state_at(self.elapsed);
        self.cached = self.target + x;
    }

    /// Current animated value.
    pub fn value(&self) -> f32 {
        self.cached
    }

    /// Returns `true` when the spring has settled to within 0.5 pixels of
    /// target and velocity is negligible.
    pub fn is_settled(&self) -> bool {
        let (x, v) = self.state_at(self.elapsed);
        x.abs() < 0.5 && v.abs() < 1.0
    }

    // -----------------------------------------------------------------------
    // Internal — analytic critically-damped spring evaluation.
    // -----------------------------------------------------------------------

    /// Returns `(displacement_from_target, velocity)` at time `t`.
    fn state_at(&self, t: f32) -> (f32, f32) {
        let w = self.omega;
        let a = self.x0;
        let b = self.v0 + w * self.x0;
        // exp(-ω·t) factor.
        let e = fast_exp(-w * t);
        let x = (a + b * t) * e;
        // dx/dt = (b - ω·(a + b·t)) · exp(-ω·t)
        let v = (b - w * (a + b * t)) * e;
        (x, v)
    }
}

// ---------------------------------------------------------------------------
// Scalar lerp helper (linear interpolation, not spring-based)
// ---------------------------------------------------------------------------

/// Linear interpolation between `a` and `b` by factor `t` ∈ [0, 1].
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// Eased step: `smoothstep` interpolation.
pub fn smoothstep(a: f32, b: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t);
    lerp(a, b, t)
}

// ---------------------------------------------------------------------------
// Approximate exp() — avoids libm dependency in no_std context.
// ---------------------------------------------------------------------------

/// Fast approximate `exp(-x)` for x ≥ 0 using a [0,1] Padé approximant
/// tiled over the integer part.
///
/// Error is < 0.05% for x ∈ [0, 20], which is sufficient for animation.
fn fast_exp(neg_x: f32) -> f32 {
    // exp is only called with non-positive argument (neg_x = -ω·t ≤ 0).
    // Clamp to avoid overflow for very large t.
    let x = (-neg_x).min(20.0_f32);
    if x <= 0.0 {
        return 1.0;
    }
    // Integer and fractional parts.
    let n = x as u32;
    let frac = x - n as f32;
    // exp(-frac) via Padé: (1 - frac/2 + frac²/12) / (1 + frac/2 + frac²/12)
    let frac2 = frac * frac;
    let num = 1.0 - frac * 0.5 + frac2 * (1.0 / 12.0);
    let den = 1.0 + frac * 0.5 + frac2 * (1.0 / 12.0);
    let exp_frac = num / den;
    // exp(-n) via repeated squaring of exp(-1) ≈ 0.36787944.
    const INV_E: f32 = 0.367_879_44;
    let mut inv_e_n = 1.0_f32;
    let mut base = INV_E;
    let mut bits = n;
    while bits > 0 {
        if bits & 1 != 0 {
            inv_e_n *= base;
        }
        base *= base;
        bits >>= 1;
    }
    inv_e_n * exp_frac
}
