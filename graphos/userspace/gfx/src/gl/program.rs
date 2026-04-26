// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Shader programs — glCreateShader / glCreateProgram / glCompileShader /
//! glLinkProgram equivalents.
//!
//! ## Shader bytecode model
//!
//! Phase 1: shader source strings are stored verbatim.  The kernel executor
//! uses a built-in fixed-function renderer keyed by a `ShaderHint` embedded
//! in the program descriptor, selected at link time by pattern-matching the
//! source.
//!
//! Phase 2: source is compiled to GraphOS IR bytecode by a userspace compiler
//! pass (same crate).  The kernel executor forwards the bytecode to the
//! hardware shader unit.
//!
//! ## Uniform locations
//!
//! `GlProgram::uniform_location(name)` returns an `Option<u32>`.  Locations
//! are stable for the lifetime of the program.

extern crate alloc;
use super::error::GlError;
use alloc::{string::String, vec::Vec};

// ── Shader stage ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ShaderStage {
    Vertex = 0,
    Fragment = 1,
    Geometry = 2,
    Compute = 3,
}

// ── Built-in shader hints (Phase 1 executor) ──────────────────────────────────

/// Built-in rendering mode selected when no hardware shader unit is available.
///
/// The kernel executor uses this to choose a fixed-function equivalent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ShaderHint {
    /// Flat colored geometry (color comes from vertex attrib or uniform).
    FlatColor = 0,
    /// Textured quad (texture2D in slot 0, optional tint in uniform slot 0).
    Textured2D = 1,
    /// Textured quad with pre-multiplied alpha blending.
    TexturedAlpha = 2,
    /// Gaussian blur (radius in uniform slot 0).
    Blur = 3,
    /// Post-processing: bloom extract (threshold in uniform slot 0).
    BloomExtract = 4,
    /// Post-processing: tone map (exposure in uniform slot 0).
    ToneMap = 5,
    /// Glass/frosted material: blur + tint + edge glow.
    Glass = 6,
    /// Shadow (Gaussian-blurred, ARGB color from uniform slot 0).
    Shadow = 7,
    /// Custom — reserved for future bytecode dispatch.
    Custom = 255,
}

// ── Compiled shader ───────────────────────────────────────────────────────────

/// A single compiled shader stage, stored in a `GlProgram`.
pub struct GlShader {
    pub stage: ShaderStage,
    /// Original GLSL source (stored for Phase 2 compilation).
    pub source: String,
    pub hint: ShaderHint,
}

// ── Uniform descriptor ────────────────────────────────────────────────────────

/// Describes one uniform in a linked program.
#[derive(Debug, Clone)]
pub struct UniformDesc {
    pub name: String,
    pub location: u32,
}

// ── GlProgram ─────────────────────────────────────────────────────────────────

/// A linked shader program — the GL equivalent of a `glCreateProgram` object.
pub struct GlProgram {
    pub(crate) id: u32,
    pub(crate) vertex: Option<GlShader>,
    pub(crate) fragment: Option<GlShader>,
    pub(crate) geometry: Option<GlShader>,
    pub(crate) compute: Option<GlShader>,
    pub(crate) uniforms: Vec<UniformDesc>,
    /// Selected built-in rendering mode for Phase 1 executor.
    pub(crate) hint: ShaderHint,
    pub(crate) linked: bool,
}

impl GlProgram {
    pub(crate) fn new(id: u32) -> Self {
        Self {
            id,
            vertex: None,
            fragment: None,
            geometry: None,
            compute: None,
            uniforms: Vec::new(),
            hint: ShaderHint::FlatColor,
            linked: false,
        }
    }

    /// Return the location of a named uniform, or `None` if not found.
    pub fn uniform_location(&self, name: &str) -> Option<u32> {
        self.uniforms
            .iter()
            .find(|u| u.name == name)
            .map(|u| u.location)
    }

    /// The built-in shader hint for Phase 1.
    pub fn hint(&self) -> ShaderHint {
        self.hint
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn is_linked(&self) -> bool {
        self.linked
    }
}

// ── Shader compilation (Phase 1) ─────────────────────────────────────────────

/// Compile a GLSL source string into a `GlShader`.
///
/// Phase 1: performs basic syntax validation (non-empty source).
/// Phase 2: invokes the full GLSL→IR compiler.
pub fn compile_shader(stage: ShaderStage, source: &str) -> Result<GlShader, GlError> {
    if source.trim().is_empty() {
        return Err(GlError::CompileFailed);
    }
    let hint = detect_hint(stage, source);
    Ok(GlShader {
        stage,
        source: String::from(source),
        hint,
    })
}

/// Detect a `ShaderHint` from source pattern-matching (Phase 1 heuristic).
fn detect_hint(stage: ShaderStage, src: &str) -> ShaderHint {
    if stage == ShaderStage::Fragment {
        if src.contains("blur") || src.contains("gaussian") {
            return ShaderHint::Blur;
        }
        if src.contains("bloom") || src.contains("luminance") {
            return ShaderHint::BloomExtract;
        }
        if src.contains("tonemap") || src.contains("exposure") {
            return ShaderHint::ToneMap;
        }
        if src.contains("glass") || src.contains("frost") {
            return ShaderHint::Glass;
        }
        if src.contains("shadow") {
            return ShaderHint::Shadow;
        }
        if src.contains("texture") || src.contains("sampler2D") {
            if src.contains("alpha") || src.contains("opacity") {
                return ShaderHint::TexturedAlpha;
            }
            return ShaderHint::Textured2D;
        }
    }
    ShaderHint::FlatColor
}

/// Link a program from compiled shaders.
///
/// Extracts uniform names and locations from GLSL source.
/// Phase 2: performs full type-checking and IR linking.
pub fn link_program(prog: &mut GlProgram) -> Result<(), GlError> {
    let has_vert = prog.vertex.is_some();
    let has_frag = prog.fragment.is_some();
    let has_comp = prog.compute.is_some();

    // A program needs either (vertex + fragment) or a compute shader.
    if !has_comp && !(has_vert && has_frag) {
        return Err(GlError::LinkFailed);
    }

    // Derive overall hint from fragment shader.
    prog.hint = prog
        .fragment
        .as_ref()
        .map(|f| f.hint)
        .or_else(|| prog.compute.as_ref().map(|_| ShaderHint::Custom))
        .unwrap_or(ShaderHint::FlatColor);

    // Collect uniforms from all stages.
    let mut loc: u32 = 0;
    for src in [
        prog.vertex.as_ref().map(|s| s.source.as_str()),
        prog.fragment.as_ref().map(|s| s.source.as_str()),
        prog.geometry.as_ref().map(|s| s.source.as_str()),
        prog.compute.as_ref().map(|s| s.source.as_str()),
    ]
    .iter()
    .flatten()
    {
        extract_uniforms(src, &mut prog.uniforms, &mut loc);
    }
    prog.linked = true;
    Ok(())
}

/// Parse `uniform <type> <name>;` declarations from GLSL source.
fn extract_uniforms(src: &str, out: &mut Vec<UniformDesc>, next_loc: &mut u32) {
    for line in src.lines() {
        let t = line.trim();
        if !t.starts_with("uniform ") {
            continue;
        }
        // "uniform TYPE NAME;" or "uniform TYPE NAME[N];"
        let after = &t["uniform ".len()..];
        let parts: Vec<&str> = after.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let raw_name = parts[1]
            .trim_end_matches(';')
            .trim_end_matches(|c: char| c == ']' || c.is_ascii_digit())
            .trim_end_matches('[');
        if raw_name.is_empty() {
            continue;
        }
        // Deduplicate.
        if out.iter().any(|u| u.name == raw_name) {
            continue;
        }
        out.push(UniformDesc {
            name: String::from(raw_name),
            location: *next_loc,
        });
        *next_loc += 1;
    }
}
