// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Programmable shader stages.
//!
//! A [`Shader`] is the GraphOS equivalent of a paired GLSL vertex+fragment
//! program. Implementations transform a single vertex to clip space and
//! produce a `Varying` payload that is interpolated perspective-correctly
//! across the triangle and fed into [`Shader::fragment`] for each pixel.

use crate::math::Vec4;

/// Up to 8 logical fragment color outputs plus optional custom depth.
#[derive(Clone, Copy, Debug)]
pub struct FragmentOutputs {
    pub colors: [Option<Vec4>; 8],
    pub depth: Option<f32>,
}

impl FragmentOutputs {
    #[inline]
    pub fn with_color(color: Vec4) -> Self {
        let mut colors = [None; 8];
        colors[0] = Some(color);
        Self {
            colors,
            depth: None,
        }
    }
}

/// Per-vertex output that gets interpolated across the triangle.
///
/// Implementations must be linear: `(a*wa + b*wb + c*wc) / (wa+wb+wc)` must
/// produce a sensible mid-point. Color, world-space normals, world-space
/// positions, and texture coordinates all satisfy this trivially.
pub trait Varying: Copy {
    /// Compute `a*wa + b*wb + c*wc`. Weights are barycentric and need not
    /// sum to 1; the rasterizer divides by `wa+wb+wc`.
    fn weighted_sum(a: Self, wa: f32, b: Self, wb: f32, c: Self, wc: f32) -> Self;
    /// Component-wise multiplication by a scalar (used by perspective divide).
    fn scale(self, s: f32) -> Self;
    /// Receive the rasterizer's winding sign so implementations can expose
    /// `gl_FrontFacing`. Default: no-op (GAP-010).
    #[inline]
    fn set_front_facing(&mut self, _front: bool) {}

    /// Called once per triangle (before the pixel loop) with the pre-scaled
    /// vertex varyings and per-triangle screen-space gradient data.
    ///
    /// `vy0/vy1/vy2` are the vertex varyings already scaled by their
    /// reciprocal clip-W (i.e. `varying * (1/w)`).  The six `dw` parameters
    /// are `d(barycentric_weight_i) / d(screen_x or y)`, which are constant
    /// across the triangle.  `d_inv_w_dx/dy` is `d(1/W_interp)/dx dy`.
    ///
    /// Default: no-op.  Override to precompute per-triangle derivative data
    /// (e.g. for `dFdx` / `dFdy` support).
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn init_triangle_derivatives(
        _vy0: &Self,
        _vy1: &Self,
        _vy2: &Self,
        _dw0dx: f32,
        _dw1dx: f32,
        _dw2dx: f32,
        _dw0dy: f32,
        _dw1dy: f32,
        _dw2dy: f32,
        _d_inv_w_dx: f32,
        _d_inv_w_dy: f32,
    ) {
    }

    /// Called per pixel, right after the perspective-correct interpolation,
    /// with the local reciprocal clip-W for this pixel.
    ///
    /// Default: no-op.  Override to compute the final per-pixel varying-level
    /// derivatives using data stored by `init_triangle_derivatives`.
    #[inline]
    fn finalize_pixel_derivatives(&mut self, _one_over_w: f32) {}
}

/// Programmable pipeline stage pair.
pub trait Shader {
    /// Per-vertex input (e.g. position + normal + uv).
    type Vertex: Copy;
    /// Per-vertex output that gets interpolated for the fragment stage.
    type Varying: Varying;

    /// Vertex stage. Returns clip-space position and a varying payload.
    fn vertex(&self, v: &Self::Vertex) -> (Vec4, Self::Varying);

    /// Fragment stage. Returns a logical RGBA color in 0..=1 linear space.
    ///
    /// The software target is packed as BGRA32 internally, but shader authors
    /// should always treat this API as RGBA.
    /// Returning `None` discards the fragment (like GL `discard`).
    fn fragment(&self, v: &Self::Varying) -> Option<Vec4>;

    /// Optional custom depth override (gl_FragDepth, GAP-008).
    ///
    /// If `Some(d)` is returned, `d` replaces the interpolated z-buffer value
    /// for this fragment. Default: `None` (use interpolated depth).
    #[allow(unused_variables)]
    #[inline]
    fn fragment_depth(&self, v: &Self::Varying) -> Option<f32> {
        None
    }

    /// Fragment stage with explicit MRT outputs. Default: attachment 0 only.
    #[inline]
    fn fragment_outputs(&self, v: &Self::Varying) -> Option<FragmentOutputs> {
        let color = self.fragment(v)?;
        let mut outputs = FragmentOutputs::with_color(color);
        outputs.depth = self.fragment_depth(v);
        Some(outputs)
    }
}
