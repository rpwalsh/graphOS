// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Material system — PBR-lite + glassmorphism materials.

/// Opaque material handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MaterialId(pub u32);

// ── Material kinds ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialKind {
    /// Unlit flat color.
    Unlit,
    /// Physically-based (metallic/roughness).
    Pbr,
    /// Glassmorphism: blur + frosted tint + edge glow.
    Glass,
    /// Panel background: solid tinted blur.
    Panel,
    /// UI: textured quad with optional tint and transparency.
    Ui,
    /// Post-process material applied as a full-screen pass.
    PostProcess,
}

// ── PBR parameters ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct PbrParams {
    pub base_color: [f32; 4], // linear RGBA
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub ao: f32,
    pub albedo_map: u32, // texture resource ID, 0 = none
    pub normal_map: u32,
    pub metallic_map: u32,
}

impl Default for PbrParams {
    fn default() -> Self {
        Self {
            base_color: [1.0, 1.0, 1.0, 1.0],
            metallic: 0.0,
            roughness: 0.5,
            emissive: [0.0; 3],
            ao: 1.0,
            albedo_map: 0,
            normal_map: 0,
            metallic_map: 0,
        }
    }
}

// ── Glass material ────────────────────────────────────────────────────────────

/// Glassmorphism / frosted-glass material.
///
/// Rendered as: blur background → tint overlay → edge glow → rounded corners.
#[derive(Debug, Clone, Copy)]
pub struct GlassMaterial {
    /// Background blur radius in pixels.
    pub blur_sigma: f32,
    /// Tint color (RGBA, alpha controls tint strength).
    pub tint: [f32; 4],
    /// Edge glow color and intensity.
    pub edge_glow: [f32; 4],
    /// Corner radius in pixels.
    pub corner_radius: f32,
    /// Border width in pixels (0 = no border).
    pub border_width: f32,
    /// Specular highlight strength.
    pub specular: f32,
    /// Saturation boost of the blurred background.
    pub saturation: f32,
}

impl Default for GlassMaterial {
    fn default() -> Self {
        Self {
            blur_sigma: 12.0,
            tint: [1.0, 1.0, 1.0, 0.12],
            edge_glow: [1.0, 1.0, 1.0, 0.3],
            corner_radius: 16.0,
            border_width: 1.0,
            specular: 0.4,
            saturation: 1.1,
        }
    }
}

// ── Material ──────────────────────────────────────────────────────────────────

pub struct Material {
    pub id: MaterialId,
    pub kind: MaterialKind,
    pub pbr: PbrParams,
    pub glass: GlassMaterial,
    /// Shader program ID (from GlContext).
    pub program: u32,
    /// Render order within the same pass (lower = earlier).
    pub sort_key: i32,
    pub double_sided: bool,
    pub alpha_blend: bool,
    pub alpha_cutoff: f32,
}

impl Material {
    pub fn unlit_color(id: MaterialId, r: f32, g: f32, b: f32, a: f32) -> Self {
        let mut pbr = PbrParams::default();
        pbr.base_color = [r, g, b, a];
        Self {
            id,
            kind: MaterialKind::Unlit,
            pbr,
            glass: GlassMaterial::default(),
            program: 0,
            sort_key: 0,
            double_sided: false,
            alpha_blend: a < 1.0,
            alpha_cutoff: 0.0,
        }
    }

    pub fn glass(id: MaterialId) -> Self {
        Self {
            id,
            kind: MaterialKind::Glass,
            pbr: PbrParams::default(),
            glass: GlassMaterial::default(),
            program: 0,
            sort_key: 100,
            double_sided: false,
            alpha_blend: true,
            alpha_cutoff: 0.0,
        }
    }

    pub fn pbr(id: MaterialId, params: PbrParams) -> Self {
        Self {
            id,
            kind: MaterialKind::Pbr,
            pbr: params,
            glass: GlassMaterial::default(),
            program: 0,
            sort_key: 0,
            double_sided: false,
            alpha_blend: params.base_color[3] < 1.0,
            alpha_cutoff: 0.0,
        }
    }
}
