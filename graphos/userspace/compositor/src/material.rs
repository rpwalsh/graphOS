// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Material system — glassmorphism, shadows, translucency, and styling.
//!
//! ## Design
//!
//! Every render node carries a `Material` that describes how it should be drawn.
//! The compositor's frame executor interprets the material to produce the correct
//! draw commands for the active backend (CPU blitter or GPU encoder).
//!
//! ## Glassmorphism material stack
//!
//! ```text
//! GlassMaterial
//!  ├─ background_blur:  BlurMaterial   (dual-Kawase of the layer below)
//!  ├─ tint:             Argb           (semi-transparent color overlay)
//!  ├─ opacity:          u8             (overall panel opacity)
//!  ├─ edge_glow:        Option<Glow>   (luminous rim)
//!  ├─ specular:         Option<Specular> (fake reflection highlight)
//!  └─ noise_texture:    bool           (subtle film grain for depth)
//! ```

/// ARGB32 color (alpha in high byte, blue in low byte — same byte order as
/// the framebuffer BGRA8 format with alpha promoted to high byte).
pub type Argb = u32;

// ── Blend modes ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum BlendMode {
    /// Standard over-composite (Porter-Duff `src-over`).
    Alpha = 0,
    /// Additive: dst + src.  Good for glows and bloom.
    Additive = 1,
    /// Multiply: dst × src / 255.  Darkens.
    Multiply = 2,
    /// Screen: 255 − (255−dst)×(255−src)/255.  Lightens.
    Screen = 3,
    /// Replace destination entirely (no blending).
    Replace = 4,
}

// ── Gradient ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum GradientDirection {
    TopToBottom,
    LeftToRight,
    TopLeftToBottomRight,
    TopRightToBottomLeft,
    /// Radial gradient centered in the node's bounds.
    Radial,
}

#[derive(Clone, Copy, Debug)]
pub struct GradientStop {
    /// Position along the gradient axis: 0–255 (0 = start, 255 = end).
    pub position: u8,
    pub color: Argb,
}

/// Maximum gradient stops per material.
pub const MAX_GRADIENT_STOPS: usize = 4;

#[derive(Clone, Copy, Debug)]
pub struct GradientMaterial {
    pub direction: GradientDirection,
    pub stops: [GradientStop; MAX_GRADIENT_STOPS],
    pub stop_count: u8,
}

impl GradientMaterial {
    /// Create a simple two-stop linear top-to-bottom gradient.
    pub fn linear_v(from: Argb, to: Argb) -> Self {
        Self {
            direction: GradientDirection::TopToBottom,
            stops: [
                GradientStop {
                    position: 0,
                    color: from,
                },
                GradientStop {
                    position: 255,
                    color: to,
                },
                GradientStop {
                    position: 0,
                    color: 0,
                },
                GradientStop {
                    position: 0,
                    color: 0,
                },
            ],
            stop_count: 2,
        }
    }

    /// Interpolate color at gradient position `t` (0–255).
    pub fn sample(&self, t: u8) -> Argb {
        let n = self.stop_count as usize;
        if n == 0 {
            return 0;
        }
        if n == 1 {
            return self.stops[0].color;
        }

        // Find surrounding stops.
        let mut lo = &self.stops[0];
        let mut hi = &self.stops[n - 1];
        for i in 0..n - 1 {
            if t >= self.stops[i].position && t <= self.stops[i + 1].position {
                lo = &self.stops[i];
                hi = &self.stops[i + 1];
                break;
            }
        }

        let range = hi.position.saturating_sub(lo.position) as u32;
        if range == 0 {
            return hi.color;
        }
        let frac = (t.saturating_sub(lo.position)) as u32 * 255 / range;
        lerp_argb(lo.color, hi.color, frac as u8)
    }
}

fn lerp_argb(a: Argb, b: Argb, t: u8) -> Argb {
    let t = t as u32;
    let it = 255 - t;
    let aa = ((a >> 24) & 0xFF) * it / 255 + ((b >> 24) & 0xFF) * t / 255;
    let ar = ((a >> 16) & 0xFF) * it / 255 + ((b >> 16) & 0xFF) * t / 255;
    let ag = ((a >> 8) & 0xFF) * it / 255 + ((b >> 8) & 0xFF) * t / 255;
    let ab = (a & 0xFF) * it / 255 + (b & 0xFF) * t / 255;
    (aa << 24) | (ar << 16) | (ag << 8) | ab
}

// ── Blur material ─────────────────────────────────────────────────────────────

/// Dual-Kawase blur parameters.
#[derive(Clone, Copy, Debug)]
pub struct BlurMaterial {
    /// Blur kernel radius in pixels at full resolution.
    pub radius: u8,
    /// Downsample factor before blurring (1 = full-res, 2 = half-res, 4 = quarter).
    pub downsample: u8,
    /// Number of dual-Kawase iterative passes.
    pub iterations: u8,
}

impl BlurMaterial {
    pub const LIGHT: Self = Self {
        radius: 12,
        downsample: 2,
        iterations: 3,
    };
    pub const MEDIUM: Self = Self {
        radius: 24,
        downsample: 2,
        iterations: 5,
    };
    pub const HEAVY: Self = Self {
        radius: 40,
        downsample: 4,
        iterations: 6,
    };
}

// ── Shadow ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct ShadowMaterial {
    /// Shadow color (ARGB; alpha controls shadow strength).
    pub color: Argb,
    /// Horizontal offset in pixels (positive = right).
    pub offset_x: i16,
    /// Vertical offset in pixels (positive = down).
    pub offset_y: i16,
    /// Blur radius of the shadow in pixels.
    pub blur: u16,
    /// Spread (positive = shadow grows beyond node bounds; negative = shrinks).
    pub spread: i16,
}

impl ShadowMaterial {
    pub const WINDOW_DEFAULT: Self = Self {
        color: 0xC0000000,
        offset_x: 0,
        offset_y: 8,
        blur: 32,
        spread: -4,
    };
    pub const PANEL_SUBTLE: Self = Self {
        color: 0x80000000,
        offset_x: 0,
        offset_y: 4,
        blur: 12,
        spread: 0,
    };
    pub const NONE: Self = Self {
        color: 0,
        offset_x: 0,
        offset_y: 0,
        blur: 0,
        spread: 0,
    };

    pub fn is_active(self) -> bool {
        (self.color >> 24) != 0 && self.blur > 0
    }
}

// ── Edge glow ─────────────────────────────────────────────────────────────────

/// Luminous rim/glow along the border of a node.
#[derive(Clone, Copy, Debug)]
pub struct EdgeGlow {
    /// Glow color (ARGB; alpha controls intensity).
    pub color: Argb,
    /// Width of the glow in pixels.
    pub width: u8,
    /// Additive blur spread of the glow.
    pub blur: u8,
}

impl EdgeGlow {
    pub const ACCENT_SOFT: Self = Self {
        color: 0x305A8FFF,
        width: 1,
        blur: 2,
    };
    pub const ACCENT_BRIGHT: Self = Self {
        color: 0x8059A6FF,
        width: 2,
        blur: 4,
    };
    pub const WHITE_HAIRLINE: Self = Self {
        color: 0x18FFFFFF,
        width: 1,
        blur: 0,
    };
}

// ── Specular ──────────────────────────────────────────────────────────────────

/// Fake specular highlight — a gradient ramp along one edge to simulate
/// an ambient light source.
#[derive(Clone, Copy, Debug)]
pub struct Specular {
    /// Specular color (ARGB).
    pub color: Argb,
    /// Edge on which the specular highlight appears.
    pub edge: SpecularEdge,
    /// Width of the highlight gradient in pixels.
    pub width: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecularEdge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Specular {
    pub const TOP_LIGHT: Self = Self {
        color: 0x28FFFFFF,
        edge: SpecularEdge::Top,
        width: 1,
    };
}

// ── Noise / film grain ────────────────────────────────────────────────────────

/// Subtle film grain for perceived depth.
#[derive(Clone, Copy, Debug)]
pub struct NoiseOverlay {
    /// Noise amplitude (0–255).
    pub strength: u8,
    /// Noise scale (x256; 256 = 1:1 pixel noise, 512 = 2× larger grain).
    pub scale_fp: u8,
}

impl NoiseOverlay {
    pub const SUBTLE: Self = Self {
        strength: 8,
        scale_fp: 255,
    };
    pub const MEDIUM: Self = Self {
        strength: 18,
        scale_fp: 200,
    };
}

// ── Glass material ────────────────────────────────────────────────────────────

/// Full glassmorphism surface material.
///
/// Combines a background blur, semi-transparent tint, edge glow, specular
/// highlight, and optional noise grain to produce a frosted-glass appearance.
#[derive(Clone, Copy, Debug)]
pub struct GlassMaterial {
    /// Blur applied to the content behind this node.
    pub blur: BlurMaterial,
    /// ARGB tint overlaid after blur.  Alpha controls transparency.
    pub tint: Argb,
    /// Overall panel opacity (0 = invisible, 255 = fully opaque).
    pub opacity: u8,
    /// Optional luminous rim.
    pub edge_glow: Option<EdgeGlow>,
    /// Optional specular highlight edge.
    pub specular: Option<Specular>,
    /// Optional film grain overlay.
    pub noise: Option<NoiseOverlay>,
    /// Border radius in pixels (0 = sharp corners).
    pub corner_radius: u8,
}

impl GlassMaterial {
    /// Dark frosted glass — suitable for sidebars, panels, context menus.
    pub const DARK_PANEL: Self = Self {
        blur: BlurMaterial::MEDIUM,
        tint: 0xD00D1520,
        opacity: 220,
        edge_glow: Some(EdgeGlow::WHITE_HAIRLINE),
        specular: Some(Specular::TOP_LIGHT),
        noise: Some(NoiseOverlay::SUBTLE),
        corner_radius: 16,
    };

    /// Lighter frosted glass — suitable for toolbar, search, popovers.
    pub const LIGHT_FROST: Self = Self {
        blur: BlurMaterial::MEDIUM,
        tint: 0xC0F0F4FC,
        opacity: 200,
        edge_glow: Some(EdgeGlow::WHITE_HAIRLINE),
        specular: Some(Specular::TOP_LIGHT),
        noise: Some(NoiseOverlay::SUBTLE),
        corner_radius: 12,
    };

    /// Acrylic — heavy blur, strong tint.  For focused windows.
    pub const ACRYLIC: Self = Self {
        blur: BlurMaterial::HEAVY,
        tint: 0xE8101828,
        opacity: 245,
        edge_glow: Some(EdgeGlow::ACCENT_SOFT),
        specular: Some(Specular::TOP_LIGHT),
        noise: Some(NoiseOverlay::MEDIUM),
        corner_radius: 20,
    };

    /// Notification toast — lighter, narrower blur.
    pub const TOAST: Self = Self {
        blur: BlurMaterial::LIGHT,
        tint: 0xB0182030,
        opacity: 240,
        edge_glow: Some(EdgeGlow::ACCENT_SOFT),
        specular: None,
        noise: Some(NoiseOverlay::SUBTLE),
        corner_radius: 12,
    };
}

// ── Border ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct BorderMaterial {
    pub color: Argb,
    pub width: u8,
    pub style: BorderStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderStyle {
    Solid,
    Dashed,
    Dotted,
}

impl BorderMaterial {
    pub const SUBTLE: Self = Self {
        color: 0x30FFFFFF,
        width: 1,
        style: BorderStyle::Solid,
    };
    pub const ACCENT: Self = Self {
        color: 0x8059A6FF,
        width: 1,
        style: BorderStyle::Solid,
    };
}

// ── Surface material ──────────────────────────────────────────────────────────

/// A ring-3 application surface imported as a GPU texture.
#[derive(Clone, Copy, Debug)]
pub struct SurfaceMaterial {
    /// Kernel surface ID (from `SYS_SURFACE_CREATE`).
    pub surface_id: u32,
    /// Per-frame alpha override (255 = fully opaque).
    pub opacity: u8,
    /// Optional drop shadow.
    pub shadow: ShadowMaterial,
    /// Corner rounding applied at composite time.
    pub corner_radius: u8,
}

// ── Top-level material enum ───────────────────────────────────────────────────

/// The complete GraphOS material model.
///
/// Each variant maps to a concrete render strategy in the frame executor.
#[derive(Clone, Copy, Debug)]
pub enum Material {
    /// Solid ARGB fill.
    Solid(Argb),
    /// Linear or radial gradient.
    Gradient(GradientMaterial),
    /// Frosted/acrylic glass with background blur.
    Glass(GlassMaterial),
    /// A ring-3 application surface (texture quad).
    Surface(SurfaceMaterial),
    /// Background / wallpaper (special treatment: always drawn first, no shadow).
    Wallpaper { surface_id: u32 },
    /// Invisible — node is kept in the scene for layout/hit-test only.
    None,
}

impl Material {
    pub fn is_opaque(&self) -> bool {
        match self {
            Self::Solid(c) => (*c >> 24) == 0xFF,
            Self::Wallpaper { .. } => true,
            Self::Surface(s) => s.opacity == 255 && s.shadow.color == 0,
            _ => false,
        }
    }

    pub fn opacity(&self) -> u8 {
        match self {
            Self::Solid(c) => (*c >> 24) as u8,
            Self::Glass(g) => g.opacity,
            Self::Surface(s) => s.opacity,
            Self::Wallpaper { .. } => 255,
            _ => 0,
        }
    }
}

// ── Shell style sheet ─────────────────────────────────────────────────────────

/// Preset material assignments for shell surface kinds.
///
/// Resolved at startup from the active `ThemeTone`.  Overridable per-node.
#[derive(Clone, Copy, Debug)]
pub struct ShellStyleSheet {
    pub background: Material,
    pub topbar: Material,
    pub sidebar: Material,
    pub window_active: Material,
    pub window_idle: Material,
    pub overlay: Material,
    pub context_menu: Material,
    pub toast: Material,
    pub cursor: Material,
}

impl ShellStyleSheet {
    pub fn dark_glass(wallpaper_id: u32) -> Self {
        Self {
            background: Material::Wallpaper {
                surface_id: wallpaper_id,
            },
            topbar: Material::Glass(GlassMaterial::DARK_PANEL),
            sidebar: Material::Glass(GlassMaterial::DARK_PANEL),
            window_active: Material::Glass(GlassMaterial::ACRYLIC),
            window_idle: Material::Glass(GlassMaterial::DARK_PANEL),
            overlay: Material::Glass(GlassMaterial::DARK_PANEL),
            context_menu: Material::Glass(GlassMaterial::DARK_PANEL),
            toast: Material::Glass(GlassMaterial::TOAST),
            cursor: Material::Solid(0xFFFFFFFF),
        }
    }

    pub fn light_frost(wallpaper_id: u32) -> Self {
        Self {
            background: Material::Wallpaper {
                surface_id: wallpaper_id,
            },
            topbar: Material::Glass(GlassMaterial::LIGHT_FROST),
            sidebar: Material::Glass(GlassMaterial::LIGHT_FROST),
            window_active: Material::Glass(GlassMaterial::ACRYLIC),
            window_idle: Material::Glass(GlassMaterial::LIGHT_FROST),
            overlay: Material::Glass(GlassMaterial::LIGHT_FROST),
            context_menu: Material::Glass(GlassMaterial::LIGHT_FROST),
            toast: Material::Glass(GlassMaterial::TOAST),
            cursor: Material::Solid(0xFF000000),
        }
    }
}
