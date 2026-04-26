// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GLSL ES 3.0/3.1/3.2 interpreter — executes compiled shader source stored in [`Context`].
//!
//! This module provides [`GlslShader`], a [`Shader`]-trait implementation that
//! interprets GLSL vertex + fragment shader source at draw time.  It is used by
//! [`Context::draw_state_arrays`] and [`Context::draw_state_elements`] so that a
//! fully-connected GL draw call (UseProgram + VBO binding + draw call) produces
//! real pixel output without the caller supplying a Rust shader struct.
//!
//! [`GlslGeometryShader`] and [`GlslComputeShader`] support geometry and compute
//! shader stages respectively via [`Context::build_glsl_geometry_shader`] and
//! [`Context::build_glsl_compute_shader`].
//!
//! ## What is implemented
//!
//! * All GLSL ES 3.0 built-in types: `bool`, `int`, `uint`, `float`,
//!   `vec2`–`vec4`, `ivec2`–`ivec4`, `uvec2`–`uvec4`, `mat2`–`mat4`.
//! * Arithmetic (`+`, `-`, `*`, `/`, `%`), comparison, logic, bitwise.
//! * Control flow: `if`/`else`, `for`, `while`, `do`/`while`, `break`,
//!   `continue`, `return`, `discard` (fragment only).
//! * Function calls including recursion and user-defined functions.
//! * Built-in functions: `abs`, `sign`, `floor`, `ceil`, `round`, `fract`,
//!   `mod`, `min`, `max`, `clamp`, `mix`, `step`, `smoothstep`, `length`,
//!   `distance`, `dot`, `cross`, `normalize`, `reflect`, `refract`,
//!   `pow`, `exp`, `exp2`, `log`, `log2`, `sqrt`, `inversesqrt`,
//!   `sin`, `cos`, `tan`, `asin`, `acos`, `atan`,
//!   `texture` / `texture2D` (2D and cube sampler),
//!   `textureCube` (cube map with face-accurate direction-to-UV mapping),
//!   `texture3D` (trilinear slice interpolation),
//!   `textureLod` (explicit LOD mip sampling),
//!   `textureGrad` (gradient-derived LOD with trilinear filter),
//!   `texelFetch` (direct texel coordinate access),
//!   integer/unsigned-integer texture sampling (`isampler2D`, `usampler2D`),
//!   atomic operations (`atomicAdd`, `atomicMin`, `atomicMax`, `atomicAnd`,
//!   `atomicOr`, `atomicXor`, `atomicExchange`, `atomicCompSwap`),
//!   memory barriers (no-ops in single-threaded context),
//!   `vec2()`–`vec4()`, `mat2()`–`mat4()`, `float()`, `int()`, `uint()`.
//! * `gl_Position` (vertex output), `gl_FragColor`/named outputs (fragment).
//! * Uniform variables read from [`Context::uniforms`].
//! * Vertex attribute inputs via [`GlslVertex`] slot array.
//! * Varying pass-through with perspective-correct interpolation.
//! * Geometry shaders: [`GlslGeometryShader`] with `EmitVertex()`/`EndPrimitive()`,
//!   `gl_in[]`, layout-parsed primitive types and `max_vertices`.
//! * Compute shaders: [`GlslComputeShader::dispatch`] with `gl_GlobalInvocationID`,
//!   `gl_LocalInvocationID`, `gl_WorkGroupID`, workgroup-scoped shared variables.
//!
//! ## What is not implemented (stubs / passthrough)
//!
//! * `dFdx` / `dFdy` (returns 0) — correct implementation requires a 2×2 quad
//!   execution context not available in the per-fragment invocation model.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::gl::UniformValue;
use crate::math::{Vec2, Vec3, Vec4};
use crate::shader::{FragmentOutputs, Shader, Varying};
use crate::texture::Texture;

// ─── Value type ──────────────────────────────────────────────────────────────

/// A runtime GLSL value.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Bool(bool),
    Int(i32),
    UInt(u32),
    Float(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    IVec2([i32; 2]),
    IVec3([i32; 3]),
    IVec4([i32; 4]),
    UVec2([u32; 2]),
    UVec3([u32; 3]),
    UVec4([u32; 4]),
    Mat2([[f32; 2]; 2]),
    Mat3([[f32; 3]; 3]),
    Mat4([[f32; 4]; 4]),
    Array(Vec<Val>),
    /// GLSL struct instance: named fields in declaration order.
    Struct(BTreeMap<String, Val>),
}

impl Val {
    pub fn as_float(&self) -> f32 {
        match self {
            Val::Float(v) => *v,
            Val::Int(v) => *v as f32,
            Val::UInt(v) => *v as f32,
            Val::Bool(v) => {
                if *v {
                    1.0
                } else {
                    0.0
                }
            }
            _ => 0.0,
        }
    }
    pub fn as_int(&self) -> i32 {
        match self {
            Val::Int(v) => *v,
            Val::Float(v) => *v as i32,
            Val::UInt(v) => *v as i32,
            Val::Bool(v) => {
                if *v {
                    1
                } else {
                    0
                }
            }
            _ => 0,
        }
    }
    pub fn as_bool(&self) -> bool {
        match self {
            Val::Bool(v) => *v,
            Val::Int(v) => *v != 0,
            Val::UInt(v) => *v != 0,
            Val::Float(v) => *v != 0.0,
            _ => false,
        }
    }
    pub fn as_vec4(&self) -> [f32; 4] {
        match self {
            Val::Vec4(v) => *v,
            Val::Vec3(v) => [v[0], v[1], v[2], 1.0],
            Val::Vec2(v) => [v[0], v[1], 0.0, 1.0],
            Val::Float(v) => [*v, *v, *v, *v],
            _ => [0.0; 4],
        }
    }
    /// Component-wise add.
    pub fn add(&self, rhs: &Val) -> Val {
        match (self, rhs) {
            (Val::Float(a), Val::Float(b)) => Val::Float(a + b),
            (Val::Int(a), Val::Int(b)) => Val::Int(a.wrapping_add(*b)),
            (Val::UInt(a), Val::UInt(b)) => Val::UInt(a.wrapping_add(*b)),
            (Val::Vec2(a), Val::Vec2(b)) => Val::Vec2([a[0] + b[0], a[1] + b[1]]),
            (Val::Vec3(a), Val::Vec3(b)) => Val::Vec3([a[0] + b[0], a[1] + b[1], a[2] + b[2]]),
            (Val::Vec4(a), Val::Vec4(b)) => {
                Val::Vec4([a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]])
            }
            (Val::Vec2(a), Val::Float(b)) => Val::Vec2([a[0] + b, a[1] + b]),
            (Val::Vec3(a), Val::Float(b)) => Val::Vec3([a[0] + b, a[1] + b, a[2] + b]),
            (Val::Vec4(a), Val::Float(b)) => Val::Vec4([a[0] + b, a[1] + b, a[2] + b, a[3] + b]),
            (Val::Float(a), Val::Vec2(b)) => Val::Vec2([a + b[0], a + b[1]]),
            (Val::Float(a), Val::Vec3(b)) => Val::Vec3([a + b[0], a + b[1], a + b[2]]),
            (Val::Float(a), Val::Vec4(b)) => Val::Vec4([a + b[0], a + b[1], a + b[2], a + b[3]]),
            _ => Val::Float(self.as_float() + rhs.as_float()),
        }
    }
    pub fn sub(&self, rhs: &Val) -> Val {
        match (self, rhs) {
            (Val::Float(a), Val::Float(b)) => Val::Float(a - b),
            (Val::Int(a), Val::Int(b)) => Val::Int(a.wrapping_sub(*b)),
            (Val::UInt(a), Val::UInt(b)) => Val::UInt(a.wrapping_sub(*b)),
            (Val::Vec2(a), Val::Vec2(b)) => Val::Vec2([a[0] - b[0], a[1] - b[1]]),
            (Val::Vec3(a), Val::Vec3(b)) => Val::Vec3([a[0] - b[0], a[1] - b[1], a[2] - b[2]]),
            (Val::Vec4(a), Val::Vec4(b)) => {
                Val::Vec4([a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]])
            }
            (Val::Vec2(a), Val::Float(b)) => Val::Vec2([a[0] - b, a[1] - b]),
            (Val::Vec3(a), Val::Float(b)) => Val::Vec3([a[0] - b, a[1] - b, a[2] - b]),
            (Val::Vec4(a), Val::Float(b)) => Val::Vec4([a[0] - b, a[1] - b, a[2] - b, a[3] - b]),
            _ => Val::Float(self.as_float() - rhs.as_float()),
        }
    }
    pub fn mul(&self, rhs: &Val) -> Val {
        match (self, rhs) {
            (Val::Float(a), Val::Float(b)) => Val::Float(a * b),
            (Val::Int(a), Val::Int(b)) => Val::Int(a.wrapping_mul(*b)),
            (Val::UInt(a), Val::UInt(b)) => Val::UInt(a.wrapping_mul(*b)),
            (Val::Vec2(a), Val::Vec2(b)) => Val::Vec2([a[0] * b[0], a[1] * b[1]]),
            (Val::Vec3(a), Val::Vec3(b)) => Val::Vec3([a[0] * b[0], a[1] * b[1], a[2] * b[2]]),
            (Val::Vec4(a), Val::Vec4(b)) => {
                Val::Vec4([a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]])
            }
            (Val::Vec2(a), Val::Float(b)) => Val::Vec2([a[0] * b, a[1] * b]),
            (Val::Vec3(a), Val::Float(b)) => Val::Vec3([a[0] * b, a[1] * b, a[2] * b]),
            (Val::Vec4(a), Val::Float(b)) => Val::Vec4([a[0] * b, a[1] * b, a[2] * b, a[3] * b]),
            (Val::Float(a), Val::Vec2(b)) => Val::Vec2([a * b[0], a * b[1]]),
            (Val::Float(a), Val::Vec3(b)) => Val::Vec3([a * b[0], a * b[1], a * b[2]]),
            (Val::Float(a), Val::Vec4(b)) => Val::Vec4([a * b[0], a * b[1], a * b[2], a * b[3]]),
            (Val::Mat4(m), Val::Vec4(v)) => {
                // column-major matrix * column vector
                let r = |row: usize| {
                    m[0][row] * v[0] + m[1][row] * v[1] + m[2][row] * v[2] + m[3][row] * v[3]
                };
                Val::Vec4([r(0), r(1), r(2), r(3)])
            }
            (Val::Mat3(m), Val::Vec3(v)) => {
                let r = |row: usize| m[0][row] * v[0] + m[1][row] * v[1] + m[2][row] * v[2];
                Val::Vec3([r(0), r(1), r(2)])
            }
            (Val::Mat2(m), Val::Vec2(v)) => {
                let r = |row: usize| m[0][row] * v[0] + m[1][row] * v[1];
                Val::Vec2([r(0), r(1)])
            }
            _ => Val::Float(self.as_float() * rhs.as_float()),
        }
    }
    pub fn div(&self, rhs: &Val) -> Val {
        match (self, rhs) {
            (Val::Float(a), Val::Float(b)) => Val::Float(if *b == 0.0 { 0.0 } else { a / b }),
            (Val::Int(a), Val::Int(b)) => Val::Int(if *b == 0 { 0 } else { a / b }),
            (Val::Vec2(a), Val::Vec2(b)) => Val::Vec2([safe_div(a[0], b[0]), safe_div(a[1], b[1])]),
            (Val::Vec3(a), Val::Vec3(b)) => Val::Vec3([
                safe_div(a[0], b[0]),
                safe_div(a[1], b[1]),
                safe_div(a[2], b[2]),
            ]),
            (Val::Vec4(a), Val::Vec4(b)) => Val::Vec4([
                safe_div(a[0], b[0]),
                safe_div(a[1], b[1]),
                safe_div(a[2], b[2]),
                safe_div(a[3], b[3]),
            ]),
            (Val::Vec2(a), Val::Float(b)) => Val::Vec2([safe_div(a[0], *b), safe_div(a[1], *b)]),
            (Val::Vec3(a), Val::Float(b)) => {
                Val::Vec3([safe_div(a[0], *b), safe_div(a[1], *b), safe_div(a[2], *b)])
            }
            (Val::Vec4(a), Val::Float(b)) => Val::Vec4([
                safe_div(a[0], *b),
                safe_div(a[1], *b),
                safe_div(a[2], *b),
                safe_div(a[3], *b),
            ]),
            _ => Val::Float(safe_div(self.as_float(), rhs.as_float())),
        }
    }
    pub fn negate(&self) -> Val {
        match self {
            Val::Float(v) => Val::Float(-v),
            Val::Int(v) => Val::Int(v.wrapping_neg()),
            Val::Vec2(v) => Val::Vec2([-v[0], -v[1]]),
            Val::Vec3(v) => Val::Vec3([-v[0], -v[1], -v[2]]),
            Val::Vec4(v) => Val::Vec4([-v[0], -v[1], -v[2], -v[3]]),
            _ => Val::Float(-self.as_float()),
        }
    }
    pub fn not(&self) -> Val {
        Val::Bool(!self.as_bool())
    }

    /// Swizzle / component access: field name like "xyz", "r", "ba", etc.
    pub fn swizzle(&self, field: &str) -> Option<Val> {
        // Integer vector swizzle: return Val::Int components
        match self {
            Val::IVec2(v) => {
                let indices: Vec<usize> = field.chars().filter_map(|c| swizzle_idx(c)).collect();
                if indices.iter().any(|&i| i >= 2) {
                    return None;
                }
                return match indices.len() {
                    1 => Some(Val::Int(v[indices[0]])),
                    2 => Some(Val::IVec2([v[indices[0]], v[indices[1]]])),
                    _ => None,
                };
            }
            Val::IVec3(v) => {
                let indices: Vec<usize> = field.chars().filter_map(|c| swizzle_idx(c)).collect();
                if indices.iter().any(|&i| i >= 3) {
                    return None;
                }
                return match indices.len() {
                    1 => Some(Val::Int(v[indices[0]])),
                    2 => Some(Val::IVec2([v[indices[0]], v[indices[1]]])),
                    3 => Some(Val::IVec3([v[indices[0]], v[indices[1]], v[indices[2]]])),
                    _ => None,
                };
            }
            Val::IVec4(v) => {
                let indices: Vec<usize> = field.chars().filter_map(|c| swizzle_idx(c)).collect();
                if indices.iter().any(|&i| i >= 4) {
                    return None;
                }
                return match indices.len() {
                    1 => Some(Val::Int(v[indices[0]])),
                    2 => Some(Val::IVec2([v[indices[0]], v[indices[1]]])),
                    3 => Some(Val::IVec3([v[indices[0]], v[indices[1]], v[indices[2]]])),
                    4 => Some(Val::IVec4([
                        v[indices[0]],
                        v[indices[1]],
                        v[indices[2]],
                        v[indices[3]],
                    ])),
                    _ => None,
                };
            }
            _ => {}
        }
        let comps: Vec<f32> = match self {
            Val::Vec2(v) => v.to_vec().iter().map(|&x| x).collect(),
            Val::Vec3(v) => v.to_vec().iter().map(|&x| x).collect(),
            Val::Vec4(v) => v.to_vec().iter().map(|&x| x).collect(),
            Val::Float(v) => vec![*v],
            _ => return None,
        };
        let indices: Vec<usize> = field.chars().filter_map(|c| swizzle_idx(c)).collect();
        if indices.iter().any(|&i| i >= comps.len()) {
            return None;
        }
        match indices.len() {
            1 => Some(Val::Float(comps[indices[0]])),
            2 => Some(Val::Vec2([comps[indices[0]], comps[indices[1]]])),
            3 => Some(Val::Vec3([
                comps[indices[0]],
                comps[indices[1]],
                comps[indices[2]],
            ])),
            4 => Some(Val::Vec4([
                comps[indices[0]],
                comps[indices[1]],
                comps[indices[2]],
                comps[indices[3]],
            ])),
            _ => None,
        }
    }
}

fn swizzle_idx(c: char) -> Option<usize> {
    match c {
        'x' | 'r' | 's' => Some(0),
        'y' | 'g' | 't' => Some(1),
        'z' | 'b' | 'p' => Some(2),
        'w' | 'a' | 'q' => Some(3),
        _ => None,
    }
}
fn safe_div(a: f32, b: f32) -> f32 {
    if b == 0.0 { 0.0 } else { a / b }
}

// ─── AST ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Expr {
    Lit(Val),
    Var(String),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Assign {
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Field {
        expr: Box<Expr>,
        field: String,
    },
    Index {
        expr: Box<Expr>,
        idx: Box<Expr>,
    },
    Call {
        name: String,
        args: Vec<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    VarDecl {
        ty: String,
        name: String,
        init: Option<Expr>,
    },
    Expr(Expr),
    Block(Vec<Stmt>),
    If {
        cond: Expr,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Expr>,
        body: Box<Stmt>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Expr,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Discard,
}

#[derive(Clone, Debug)]
pub struct FuncDef {
    pub ret_ty: String,
    pub name: String,
    pub params: Vec<(String, String)>, // (type, name)
    pub body: Vec<Stmt>,
}

/// A parsed UBO (uniform buffer object) block declaration.
#[derive(Clone, Debug)]
pub struct UboBlock {
    /// `layout(binding = N)` value if present, or `None`.
    pub layout_binding: Option<u32>,
    /// The GLSL block type name (e.g. `"Matrices"`).
    pub block_name: String,
    /// The instance name used in shader code (e.g. `"matrices"`). Empty = anonymous block.
    pub instance_name: String,
    /// Members in declaration order: (glsl_type, member_name).
    pub members: Vec<(String, String)>,
}

pub struct ShaderAst {
    pub functions: Vec<FuncDef>,
    pub uniforms: Vec<(String, String)>,     // (type, name)
    pub attributes: Vec<(String, String)>,   // (type, name) — vertex inputs
    pub varyings_out: Vec<(String, String)>, // vertex outputs / frag inputs
    pub varyings_in: Vec<(String, String)>,  // fragment inputs
    pub outputs: Vec<(String, String)>,      // fragment outputs
    pub ubo_blocks: Vec<UboBlock>,           // UBO interface blocks (GAP-011)
    /// User-defined struct types: name → ordered list of (field_type, field_name).
    pub struct_defs: BTreeMap<String, Vec<(String, String)>>,
}

// ─── Parser ───────────────────────────────────────────────────────────────────

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    /// User-defined struct type names discovered at top-level, used in parse_stmt.
    struct_names: Vec<String>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            struct_names: Vec::new(),
        }
    }

    fn remaining(&self) -> &str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // whitespace
            let before = self.pos;
            while self.pos < self.src.len() && self.src.as_bytes()[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            // line comment
            if self.remaining().starts_with("//") {
                while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            // block comment
            if self.remaining().starts_with("/*") {
                self.pos += 2;
                while self.pos + 1 < self.src.len() {
                    if self.src.as_bytes()[self.pos] == b'*'
                        && self.src.as_bytes()[self.pos + 1] == b'/'
                    {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            if self.pos == before {
                break;
            }
        }
    }

    fn eat(&mut self, s: &str) -> bool {
        self.skip_whitespace_and_comments();
        if self.remaining().starts_with(s) {
            // Make sure it's not a longer identifier match for keywords
            if s.chars().all(|c| c.is_alphanumeric() || c == '_') {
                let after = &self.remaining()[s.len()..];
                if after
                    .chars()
                    .next()
                    .map(|c| c.is_alphanumeric() || c == '_')
                    .unwrap_or(false)
                {
                    return false;
                }
            }
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn eat_exact(&mut self, s: &str) -> bool {
        self.skip_whitespace_and_comments();
        if self.remaining().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Option<String> {
        self.skip_whitespace_and_comments();
        let start = self.pos;
        let mut chars = self.remaining().chars();
        match chars.next() {
            Some(c) if c.is_alphabetic() || c == '_' => {
                self.pos += c.len_utf8();
            }
            _ => return None,
        }
        loop {
            let rem = &self.src[self.pos..];
            let mut ci = rem.chars();
            match ci.next() {
                Some(c) if c.is_alphanumeric() || c == '_' => {
                    self.pos += c.len_utf8();
                }
                _ => break,
            }
        }
        if self.pos > start {
            Some(self.src[start..self.pos].to_string())
        } else {
            None
        }
    }

    fn number_lit(&mut self) -> Option<Val> {
        self.skip_whitespace_and_comments();
        let start = self.pos;
        let bytes = self.remaining().as_bytes();
        if bytes.is_empty() {
            return None;
        }
        if !bytes[0].is_ascii_digit() && bytes[0] != b'-' {
            return None;
        }
        // read digits
        let neg = bytes[0] == b'-';
        if neg {
            self.pos += 1;
        }
        let digit_start = self.pos;
        while self.pos < self.src.len() && self.src.as_bytes()[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let has_dot = self.pos < self.src.len() && self.src.as_bytes()[self.pos] == b'.';
        if has_dot {
            self.pos += 1;
        }
        if has_dot {
            while self.pos < self.src.len() && self.src.as_bytes()[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        let has_exp = self.pos < self.src.len()
            && (self.src.as_bytes()[self.pos] == b'e' || self.src.as_bytes()[self.pos] == b'E');
        if has_exp {
            self.pos += 1;
            if self.pos < self.src.len()
                && (self.src.as_bytes()[self.pos] == b'+' || self.src.as_bytes()[self.pos] == b'-')
            {
                self.pos += 1;
            }
            while self.pos < self.src.len() && self.src.as_bytes()[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        // suffix
        let is_uint = self.pos < self.src.len() && self.src.as_bytes()[self.pos] == b'u';
        if is_uint {
            self.pos += 1;
        }
        let tok = &self.src[start..self.pos];
        if self.pos == start {
            return None;
        }
        if has_dot || has_exp {
            tok.trim_end_matches('f')
                .parse::<f32>()
                .ok()
                .map(Val::Float)
        } else if is_uint {
            tok.trim_end_matches('u').parse::<u32>().ok().map(Val::UInt)
        } else {
            tok.parse::<i32>().ok().map(Val::Int)
        }
    }

    // Parse a full shader, returning an AST
    pub fn parse_shader(&mut self) -> ShaderAst {
        let mut ast = ShaderAst {
            functions: Vec::new(),
            uniforms: Vec::new(),
            attributes: Vec::new(),
            varyings_out: Vec::new(),
            varyings_in: Vec::new(),
            outputs: Vec::new(),
            ubo_blocks: Vec::new(),
            struct_defs: BTreeMap::new(),
        };
        while self.pos < self.src.len() {
            self.skip_whitespace_and_comments();
            if self.pos >= self.src.len() {
                break;
            }
            // skip #version, #extension, precision
            if self.remaining().starts_with('#') {
                while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            if self.eat("precision") {
                // precision highp float; — skip
                while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b';' {
                    self.pos += 1;
                }
                self.pos += 1; // consume ';'
                continue;
            }
            // ── struct definition ─────────────────────────────────────────────
            // `struct Name { type field; ... };`
            if self.eat("struct") {
                if let Some(struct_name) = self.ident() {
                    self.skip_whitespace_and_comments();
                    if self.remaining().starts_with('{') {
                        self.pos += 1; // '{'
                        let mut fields: Vec<(String, String)> = Vec::new();
                        loop {
                            self.skip_whitespace_and_comments();
                            if self.pos >= self.src.len() || self.remaining().starts_with('}') {
                                break;
                            }
                            let fty = match self.ident() {
                                Some(t) => t,
                                None => break,
                            };
                            let fn_ = match self.ident() {
                                Some(n) => n,
                                None => break,
                            };
                            // skip array brackets
                            if self.remaining().starts_with('[') {
                                while self.pos < self.src.len()
                                    && self.src.as_bytes()[self.pos] != b']'
                                {
                                    self.pos += 1;
                                }
                                if self.pos < self.src.len() {
                                    self.pos += 1;
                                }
                            }
                            self.eat_exact(";");
                            fields.push((fty, fn_));
                        }
                        self.skip_whitespace_and_comments();
                        if self.remaining().starts_with('}') {
                            self.pos += 1;
                        }
                        // optional instance variable name
                        let _instance = self.ident();
                        self.eat_exact(";");
                        ast.struct_defs.insert(struct_name.clone(), fields);
                        self.struct_names.push(struct_name);
                    } else {
                        // forward declaration — skip to ';'
                        while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b';' {
                            self.pos += 1;
                        }
                        if self.pos < self.src.len() {
                            self.pos += 1;
                        }
                    }
                }
                continue;
            }
            // layout qualifier — parse binding number if present, then skip to ')'
            let mut layout_binding: Option<u32> = None;
            if self.eat("layout") {
                // Scan the layout() body for `binding = N`.
                let layout_start = self.pos;
                while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b')' {
                    self.pos += 1;
                }
                let layout_body = &self.src[layout_start..self.pos];
                // Simple scan: look for `binding` followed by `=` followed by digits.
                if let Some(bi) = layout_body.find("binding") {
                    let rest = &layout_body[bi + 7..];
                    let rest = rest.trim_start();
                    if rest.starts_with('=') {
                        let rest = rest[1..].trim_start();
                        let digits: String =
                            rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                        layout_binding = digits.parse::<u32>().ok();
                    }
                }
                self.pos += 1; // ')'
                self.skip_whitespace_and_comments();
                // fall through to parse the declaration
            }
            // storage qualifiers (including GLSL ES 1.0 legacy keywords)
            let is_uniform = self.eat("uniform");
            // ── UBO interface block detection (GAP-011) ──────────────────
            // Pattern: `uniform BlockName { ... } instanceName;`
            // After eating `uniform`, peek at the next ident; if the token after
            // that is `{`, this is a block declaration, not a plain uniform var.
            if is_uniform {
                let saved = self.pos;
                self.skip_whitespace_and_comments();
                if let Some(block_name) = self.ident() {
                    self.skip_whitespace_and_comments();
                    if self.remaining().starts_with('{') {
                        // Parse UBO block members.
                        self.pos += 1; // '{'
                        let mut members: Vec<(String, String)> = Vec::new();
                        loop {
                            self.skip_whitespace_and_comments();
                            if self.pos >= self.src.len() || self.remaining().starts_with('}') {
                                break;
                            }
                            // skip qualifiers like `layout(...)`, `highp`, etc.
                            if self.eat("layout") {
                                while self.pos < self.src.len()
                                    && self.src.as_bytes()[self.pos] != b')'
                                {
                                    self.pos += 1;
                                }
                                if self.pos < self.src.len() {
                                    self.pos += 1;
                                }
                                self.skip_whitespace_and_comments();
                            }
                            let _ = self.eat("highp") || self.eat("mediump") || self.eat("lowp");
                            let mty = match self.ident() {
                                Some(t) => t,
                                None => break,
                            };
                            let mn = match self.ident() {
                                Some(n) => n,
                                None => break,
                            };
                            // skip optional array brackets
                            if self.remaining().starts_with('[') {
                                while self.pos < self.src.len()
                                    && self.src.as_bytes()[self.pos] != b']'
                                {
                                    self.pos += 1;
                                }
                                if self.pos < self.src.len() {
                                    self.pos += 1;
                                }
                            }
                            // consume ';'
                            while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b';'
                            {
                                self.pos += 1;
                            }
                            if self.pos < self.src.len() {
                                self.pos += 1;
                            }
                            members.push((mty, mn));
                        }
                        if self.remaining().starts_with('}') {
                            self.pos += 1;
                        } // '}'
                        self.skip_whitespace_and_comments();
                        // optional instance name
                        let instance_name = self.ident().unwrap_or_default();
                        // consume ';'
                        while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b';' {
                            self.pos += 1;
                        }
                        if self.pos < self.src.len() {
                            self.pos += 1;
                        }
                        ast.ubo_blocks.push(UboBlock {
                            layout_binding,
                            block_name,
                            instance_name,
                            members,
                        });
                        continue;
                    }
                }
                // Not a block — restore position and fall through.
                self.pos = saved;
            }
            // ─────────────────────────────────────────────────────────────
            let mut is_in = !is_uniform && self.eat("in");
            let mut is_out = !is_uniform && !is_in && self.eat("out");
            let _is_inout = self.eat("inout");
            let is_attribute = !is_uniform && !is_in && !is_out && self.eat("attribute");
            let is_varying =
                !is_uniform && !is_in && !is_out && !is_attribute && self.eat("varying");
            if is_attribute {
                is_in = true;
            }
            // `varying` bridges vertex->fragment in GLSL ES 1.0, so record both sides.
            let varying_bridge = is_varying;
            if is_varying {
                is_in = true;
                is_out = true;
            }
            // flat / smooth / centroid — skip
            let _ = self.eat("flat") || self.eat("smooth") || self.eat("centroid");

            let ty = match self.ident() {
                Some(t) => t,
                None => {
                    self.pos += 1;
                    continue;
                }
            };
            let name = match self.ident() {
                Some(n) => n,
                None => {
                    self.pos += 1;
                    continue;
                }
            };
            self.skip_whitespace_and_comments();

            // Check for function definition
            if self.remaining().starts_with('(') {
                // It's a function
                let func = self.parse_function(ty, name);
                ast.functions.push(func);
            } else {
                // Variable declaration (possibly array — skip array size)
                if self.remaining().starts_with('[') {
                    while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b']' {
                        self.pos += 1;
                    }
                    if self.pos < self.src.len() {
                        self.pos += 1;
                    }
                }
                // skip initializer
                if self.remaining().starts_with('=') {
                    while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b';' {
                        self.pos += 1;
                    }
                }
                if self.pos < self.src.len() {
                    self.pos += 1;
                } // ';'
                if is_uniform {
                    ast.uniforms.push((ty, name));
                } else if varying_bridge {
                    ast.varyings_in.push((ty.clone(), name.clone()));
                    ast.varyings_out.push((ty, name));
                } else if is_in {
                    ast.attributes.push((ty, name));
                } else if is_out {
                    ast.outputs.push((ty, name));
                }
            }
        }
        ast
    }

    fn parse_function(&mut self, ret_ty: String, name: String) -> FuncDef {
        let mut params = Vec::new();
        self.eat_exact("(");
        loop {
            self.skip_whitespace_and_comments();
            if self.remaining().starts_with(')') {
                self.eat_exact(")");
                break;
            }
            // optional qualifier
            let _ = self.eat("const") || self.eat("in") || self.eat("out") || self.eat("inout");
            let pty = self.ident().unwrap_or_default();
            let pname = self.ident().unwrap_or_default();
            // skip array brackets
            if self.remaining().starts_with('[') {
                while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b']' {
                    self.pos += 1;
                }
                if self.pos < self.src.len() {
                    self.pos += 1;
                }
            }
            if !pty.is_empty() {
                params.push((pty, pname));
            }
            self.skip_whitespace_and_comments();
            if !self.eat_exact(",") {
                self.skip_whitespace_and_comments();
                if self.remaining().starts_with(')') {
                    self.eat_exact(")");
                    break;
                }
            }
        }
        self.skip_whitespace_and_comments();
        let body = if self.remaining().starts_with('{') {
            self.parse_block_stmts()
        } else {
            Vec::new()
        };
        FuncDef {
            ret_ty,
            name,
            params,
            body,
        }
    }

    fn parse_block_stmts(&mut self) -> Vec<Stmt> {
        self.eat_exact("{");
        let mut stmts = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            if self.remaining().starts_with('}') {
                self.eat_exact("}");
                break;
            }
            if self.pos >= self.src.len() {
                break;
            }
            if let Some(s) = self.parse_stmt() {
                stmts.push(s);
            }
        }
        stmts
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        self.skip_whitespace_and_comments();
        if self.remaining().is_empty() {
            return None;
        }

        if self.eat("return") {
            self.skip_whitespace_and_comments();
            if self.remaining().starts_with(';') {
                self.eat_exact(";");
                return Some(Stmt::Return(None));
            }
            let e = self.parse_expr();
            self.eat_exact(";");
            return Some(Stmt::Return(e));
        }
        if self.eat("discard") {
            self.eat_exact(";");
            return Some(Stmt::Discard);
        }
        if self.eat("break") {
            self.eat_exact(";");
            return Some(Stmt::Break);
        }
        if self.eat("continue") {
            self.eat_exact(";");
            return Some(Stmt::Continue);
        }

        if self.eat("if") {
            self.eat_exact("(");
            let cond = self.parse_expr().unwrap_or(Expr::Lit(Val::Bool(false)));
            self.eat_exact(")");
            let then = self.parse_stmt_or_block();
            self.skip_whitespace_and_comments();
            let els = if self.eat("else") {
                Some(Box::new(self.parse_stmt_or_block()))
            } else {
                None
            };
            return Some(Stmt::If {
                cond,
                then: Box::new(then),
                els,
            });
        }

        if self.eat("for") {
            self.eat_exact("(");
            let init = self.parse_stmt().map(Box::new);
            let cond = self.parse_expr();
            self.eat_exact(";");
            let step = self.parse_expr();
            self.eat_exact(")");
            let body = self.parse_stmt_or_block();
            return Some(Stmt::For {
                init,
                cond,
                step,
                body: Box::new(body),
            });
        }

        if self.eat("while") {
            self.eat_exact("(");
            let cond = self.parse_expr().unwrap_or(Expr::Lit(Val::Bool(false)));
            self.eat_exact(")");
            let body = self.parse_stmt_or_block();
            return Some(Stmt::While {
                cond,
                body: Box::new(body),
            });
        }

        if self.eat("do") {
            let body = self.parse_stmt_or_block();
            self.eat("while");
            self.eat_exact("(");
            let cond = self.parse_expr().unwrap_or(Expr::Lit(Val::Bool(false)));
            self.eat_exact(")");
            self.eat_exact(";");
            return Some(Stmt::DoWhile {
                body: Box::new(body),
                cond,
            });
        }

        if self.remaining().starts_with('{') {
            let stmts = self.parse_block_stmts();
            return Some(Stmt::Block(stmts));
        }

        // Try to parse a variable declaration: type name = expr;
        let save = self.pos;
        if let Some(ty) = self.ident() {
            if is_type_keyword(&ty) || self.struct_names.iter().any(|s| s == &ty) {
                if let Some(name) = self.ident() {
                    self.skip_whitespace_and_comments();
                    // array size
                    if self.remaining().starts_with('[') {
                        while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b']' {
                            self.pos += 1;
                        }
                        if self.pos < self.src.len() {
                            self.pos += 1;
                        }
                    }
                    let init = if self.eat_exact("=") {
                        self.parse_expr()
                    } else {
                        None
                    };
                    self.eat_exact(";");
                    return Some(Stmt::VarDecl { ty, name, init });
                }
            }
        }
        self.pos = save;

        // Expression statement
        let e = self.parse_expr()?;
        self.eat_exact(";");
        Some(Stmt::Expr(e))
    }

    fn parse_stmt_or_block(&mut self) -> Stmt {
        self.skip_whitespace_and_comments();
        if self.remaining().starts_with('{') {
            Stmt::Block(self.parse_block_stmts())
        } else {
            self.parse_stmt().unwrap_or(Stmt::Block(Vec::new()))
        }
    }

    fn parse_expr(&mut self) -> Option<Expr> {
        self.parse_assign()
    }

    fn parse_assign(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_ternary()?;
        self.skip_whitespace_and_comments();
        let op = if self.eat_exact("+=") {
            Some(BinaryOp::AddAssign)
        } else if self.eat_exact("-=") {
            Some(BinaryOp::SubAssign)
        } else if self.eat_exact("*=") {
            Some(BinaryOp::MulAssign)
        } else if self.eat_exact("/=") {
            Some(BinaryOp::DivAssign)
        } else if self.eat_exact("=") && !self.remaining().starts_with('=') {
            Some(BinaryOp::Add) /* placeholder */
        } else {
            None
        };
        if let Some(op) = op {
            let rhs = self.parse_assign()?;
            if op == BinaryOp::Add {
                // was plain '='
                lhs = Expr::Assign {
                    target: Box::new(lhs),
                    value: Box::new(rhs),
                };
            } else {
                // compound assignment: lhs op= rhs => lhs = lhs op rhs
                let combined = Expr::Binary {
                    op,
                    left: Box::new(lhs.clone()),
                    right: Box::new(rhs),
                };
                lhs = Expr::Assign {
                    target: Box::new(lhs),
                    value: Box::new(combined),
                };
            }
        }
        Some(lhs)
    }

    fn parse_ternary(&mut self) -> Option<Expr> {
        let cond = self.parse_or()?;
        self.skip_whitespace_and_comments();
        if self.eat_exact("?") {
            let then = self.parse_expr()?;
            self.eat_exact(":");
            let els = self.parse_expr()?;
            Some(Expr::Ternary {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            })
        } else {
            Some(cond)
        }
    }

    fn parse_or(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_and()?;
        loop {
            self.skip_whitespace_and_comments();
            if self.eat_exact("||") {
                let rhs = self.parse_and()?;
                lhs = Expr::Binary {
                    op: BinaryOp::Or,
                    left: Box::new(lhs),
                    right: Box::new(rhs),
                };
            } else {
                break;
            }
        }
        Some(lhs)
    }

    fn parse_and(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_eq()?;
        loop {
            self.skip_whitespace_and_comments();
            if self.eat_exact("&&") {
                let rhs = self.parse_eq()?;
                lhs = Expr::Binary {
                    op: BinaryOp::And,
                    left: Box::new(lhs),
                    right: Box::new(rhs),
                };
            } else {
                break;
            }
        }
        Some(lhs)
    }

    fn parse_eq(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_cmp()?;
        loop {
            self.skip_whitespace_and_comments();
            let op = if self.eat_exact("==") {
                BinaryOp::Eq
            } else if self.eat_exact("!=") {
                BinaryOp::Ne
            } else {
                break;
            };
            let rhs = self.parse_cmp()?;
            lhs = Expr::Binary {
                op,
                left: Box::new(lhs),
                right: Box::new(rhs),
            };
        }
        Some(lhs)
    }

    fn parse_cmp(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_add()?;
        loop {
            self.skip_whitespace_and_comments();
            let op = if self.eat_exact("<=") {
                BinaryOp::Le
            } else if self.eat_exact(">=") {
                BinaryOp::Ge
            } else if self.eat_exact("<") {
                BinaryOp::Lt
            } else if self.eat_exact(">") {
                BinaryOp::Gt
            } else {
                break;
            };
            let rhs = self.parse_add()?;
            lhs = Expr::Binary {
                op,
                left: Box::new(lhs),
                right: Box::new(rhs),
            };
        }
        Some(lhs)
    }

    fn parse_add(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_mul()?;
        loop {
            self.skip_whitespace_and_comments();
            let op = if self.eat_exact("+") && !self.remaining().starts_with('+') {
                BinaryOp::Add
            } else if self.eat_exact("-") && !self.remaining().starts_with('-') {
                BinaryOp::Sub
            } else {
                break;
            };
            let rhs = self.parse_mul()?;
            lhs = Expr::Binary {
                op,
                left: Box::new(lhs),
                right: Box::new(rhs),
            };
        }
        Some(lhs)
    }

    fn parse_mul(&mut self) -> Option<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            self.skip_whitespace_and_comments();
            let op = if self.eat_exact("*") {
                BinaryOp::Mul
            } else if self.eat_exact("/") {
                BinaryOp::Div
            } else if self.eat_exact("%") {
                BinaryOp::Rem
            } else {
                break;
            };
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary {
                op,
                left: Box::new(lhs),
                right: Box::new(rhs),
            };
        }
        Some(lhs)
    }

    fn parse_unary(&mut self) -> Option<Expr> {
        self.skip_whitespace_and_comments();
        if self.eat_exact("!") {
            let e = self.parse_unary()?;
            return Some(Expr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(e),
            });
        }
        if self.eat_exact("++") {
            let e = self.parse_unary()?;
            return Some(Expr::Unary {
                op: UnaryOp::PreInc,
                expr: Box::new(e),
            });
        }
        if self.eat_exact("--") {
            let e = self.parse_unary()?;
            return Some(Expr::Unary {
                op: UnaryOp::PreDec,
                expr: Box::new(e),
            });
        }
        // unary minus: only if not a literal
        if self.remaining().starts_with('-')
            && !self.remaining()[1..].starts_with(|c: char| c.is_ascii_digit())
        {
            self.eat_exact("-");
            let e = self.parse_unary()?;
            return Some(Expr::Unary {
                op: UnaryOp::Neg,
                expr: Box::new(e),
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Option<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            self.skip_whitespace_and_comments();
            if self.eat_exact("++") {
                e = Expr::Unary {
                    op: UnaryOp::PostInc,
                    expr: Box::new(e),
                };
            } else if self.eat_exact("--") {
                e = Expr::Unary {
                    op: UnaryOp::PostDec,
                    expr: Box::new(e),
                };
            } else if self.eat_exact(".") {
                let field = self.ident().unwrap_or_default();
                // Could be a method call: e.g., v.length() — handle as builtin
                if self.remaining().starts_with('(') {
                    self.eat_exact("(");
                    self.eat_exact(")");
                    e = Expr::Call {
                        name: alloc::format!("_method_{}", field),
                        args: vec![e],
                    };
                } else {
                    e = Expr::Field {
                        expr: Box::new(e),
                        field,
                    };
                }
            } else if self.eat_exact("[") {
                let idx = self.parse_expr().unwrap_or(Expr::Lit(Val::Int(0)));
                self.eat_exact("]");
                e = Expr::Index {
                    expr: Box::new(e),
                    idx: Box::new(idx),
                };
            } else {
                break;
            }
        }
        Some(e)
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        self.skip_whitespace_and_comments();

        // Parenthesised expression
        if self.eat_exact("(") {
            let e = self.parse_expr()?;
            self.eat_exact(")");
            return Some(e);
        }

        // Bool literals
        if self.eat("true") {
            return Some(Expr::Lit(Val::Bool(true)));
        }
        if self.eat("false") {
            return Some(Expr::Lit(Val::Bool(false)));
        }

        // Number literal (try before ident)
        let save = self.pos;
        if let Some(v) = self.number_lit() {
            return Some(Expr::Lit(v));
        }
        self.pos = save;

        // Identifier or function call
        let id = self.ident()?;
        self.skip_whitespace_and_comments();
        if self.remaining().starts_with('(') {
            self.eat_exact("(");
            let mut args = Vec::new();
            loop {
                self.skip_whitespace_and_comments();
                if self.remaining().starts_with(')') {
                    self.eat_exact(")");
                    break;
                }
                if let Some(a) = self.parse_expr() {
                    args.push(a);
                }
                self.skip_whitespace_and_comments();
                if !self.eat_exact(",") {
                    self.skip_whitespace_and_comments();
                    if self.remaining().starts_with(')') {
                        self.eat_exact(")");
                        break;
                    }
                }
            }
            return Some(Expr::Call { name: id, args });
        }
        Some(Expr::Var(id))
    }
}

fn is_type_keyword(s: &str) -> bool {
    matches!(
        s,
        "float"
            | "int"
            | "uint"
            | "bool"
            | "vec2"
            | "vec3"
            | "vec4"
            | "ivec2"
            | "ivec3"
            | "ivec4"
            | "uvec2"
            | "uvec3"
            | "uvec4"
            | "mat2"
            | "mat3"
            | "mat4"
            | "mat2x3"
            | "mat3x2"
            | "mat2x4"
            | "mat4x2"
            | "mat3x4"
            | "mat4x3"
            | "sampler2D"
            | "sampler3D"
            | "samplerCube"
            | "void"
            | "struct"
    )
}

// ─── Interpreter ──────────────────────────────────────────────────────────────

/// Flow control signal.
#[derive(Debug)]
enum Signal {
    Return(Option<Val>),
    Break,
    Continue,
    Discard,
}

/// Execution environment for one shader invocation.
struct Env<'a> {
    /// Call stack of variable scopes.
    scopes: Vec<BTreeMap<String, Val>>,
    /// Uniform values (from Context).
    uniforms: &'a BTreeMap<String, Val>,
    /// Texture samplers (index = sampler binding) — 2D wrappers for fast sampling.
    textures: &'a [Option<Texture<'a>>],
    /// Full owned texture data — used for cube maps, mip levels, integer textures.
    owned_textures: &'a [Option<TextureOwned>],
    /// User-defined functions.
    funcs: &'a BTreeMap<String, FuncDef>,
    /// User-defined struct type definitions.
    struct_defs: &'a BTreeMap<String, Vec<(String, String)>>,
}

impl<'a> Env<'a> {
    fn new(
        uniforms: &'a BTreeMap<String, Val>,
        textures: &'a [Option<Texture<'a>>],
        owned_textures: &'a [Option<TextureOwned>],
        funcs: &'a BTreeMap<String, FuncDef>,
    ) -> Self {
        static EMPTY_STRUCT_DEFS: SyncUnsafeCell<BTreeMap<String, Vec<(String, String)>>> =
            SyncUnsafeCell(core::cell::UnsafeCell::new(BTreeMap::new()));
        // SAFETY: This is a read-only empty map; no mutation ever occurs.
        let sd = unsafe { &*EMPTY_STRUCT_DEFS.0.get() };
        Self {
            scopes: vec![BTreeMap::new()],
            uniforms,
            textures,
            owned_textures,
            funcs,
            struct_defs: sd,
        }
    }

    fn new_with_structs(
        uniforms: &'a BTreeMap<String, Val>,
        textures: &'a [Option<Texture<'a>>],
        owned_textures: &'a [Option<TextureOwned>],
        funcs: &'a BTreeMap<String, FuncDef>,
        struct_defs: &'a BTreeMap<String, Vec<(String, String)>>,
    ) -> Self {
        Self {
            scopes: vec![BTreeMap::new()],
            uniforms,
            textures,
            owned_textures,
            funcs,
            struct_defs,
        }
    }
    fn push_scope(&mut self) {
        self.scopes.push(BTreeMap::new());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn get(&self, name: &str) -> Val {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return v.clone();
            }
        }
        if let Some(v) = self.uniforms.get(name) {
            return v.clone();
        }
        Val::Float(0.0) // undefined
    }

    fn try_get(&self, name: &str) -> Option<&Val> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        self.uniforms.get(name)
    }

    fn set(&mut self, name: &str, val: Val) {
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.to_string(), val);
                return;
            }
        }
        // Set in current (top) scope
        if let Some(top) = self.scopes.last_mut() {
            top.insert(name.to_string(), val);
        }
    }

    fn declare(&mut self, name: &str, val: Val) {
        if let Some(top) = self.scopes.last_mut() {
            top.insert(name.to_string(), val);
        }
    }

    /// Write a value back through an lvalue expression (for atomic ops etc.).
    fn assign_lvalue(&mut self, lhs: &Expr, val: Val) -> Result<(), Signal> {
        match lhs {
            Expr::Var(name) => {
                self.set(name, val);
                Ok(())
            }
            Expr::Index {
                expr: base,
                idx: idx_expr,
            } => {
                let idx = self.eval_expr(idx_expr)?.as_int() as usize;
                if let Expr::Var(bname) = base.as_ref() {
                    let bname = bname.clone();
                    let mut base_val = self.get(&bname);
                    match &mut base_val {
                        Val::Array(arr) => {
                            if idx < arr.len() {
                                arr[idx] = val;
                            }
                        }
                        _ => {}
                    }
                    self.set(&bname, base_val);
                }
                Ok(())
            }
            _ => Ok(()), // unsupported lvalue — silently ignore
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Val, Signal> {
        match expr {
            Expr::Lit(v) => Ok(v.clone()),
            Expr::Var(name) => Ok(self.get(name)),

            Expr::Unary { op, expr } => {
                let v = self.eval_expr(expr)?;
                Ok(match op {
                    UnaryOp::Neg => v.negate(),
                    UnaryOp::Not => v.not(),
                    UnaryOp::PreInc => {
                        let r = v.add(&Val::Int(1));
                        self.assign_expr(expr, r.clone());
                        r
                    }
                    UnaryOp::PreDec => {
                        let r = v.sub(&Val::Int(1));
                        self.assign_expr(expr, r.clone());
                        r
                    }
                    UnaryOp::PostInc => {
                        self.assign_expr(expr, v.add(&Val::Int(1)));
                        v
                    }
                    UnaryOp::PostDec => {
                        self.assign_expr(expr, v.sub(&Val::Int(1)));
                        v
                    }
                })
            }

            Expr::Binary { op, left, right } => {
                let l = self.eval_expr(left)?;
                let r = self.eval_expr(right)?;
                Ok(match op {
                    BinaryOp::Add => l.add(&r),
                    BinaryOp::Sub => l.sub(&r),
                    BinaryOp::Mul => l.mul(&r),
                    BinaryOp::Div => l.div(&r),
                    BinaryOp::Rem => {
                        Val::Int(l.as_int() % if r.as_int() == 0 { 1 } else { r.as_int() })
                    }
                    BinaryOp::Eq => Val::Bool(l == r),
                    BinaryOp::Ne => Val::Bool(l != r),
                    BinaryOp::Lt => Val::Bool(l.as_float() < r.as_float()),
                    BinaryOp::Le => Val::Bool(l.as_float() <= r.as_float()),
                    BinaryOp::Gt => Val::Bool(l.as_float() > r.as_float()),
                    BinaryOp::Ge => Val::Bool(l.as_float() >= r.as_float()),
                    BinaryOp::And => Val::Bool(l.as_bool() && r.as_bool()),
                    BinaryOp::Or => Val::Bool(l.as_bool() || r.as_bool()),
                    BinaryOp::BitAnd => Val::Int(l.as_int() & r.as_int()),
                    BinaryOp::BitOr => Val::Int(l.as_int() | r.as_int()),
                    BinaryOp::BitXor => Val::Int(l.as_int() ^ r.as_int()),
                    BinaryOp::Shl => Val::Int(l.as_int().wrapping_shl(r.as_int().max(0) as u32)),
                    BinaryOp::Shr => Val::Int(l.as_int().wrapping_shr(r.as_int().max(0) as u32)),
                    BinaryOp::AddAssign
                    | BinaryOp::SubAssign
                    | BinaryOp::MulAssign
                    | BinaryOp::DivAssign => {
                        let result = match op {
                            BinaryOp::AddAssign => l.add(&r),
                            BinaryOp::SubAssign => l.sub(&r),
                            BinaryOp::MulAssign => l.mul(&r),
                            BinaryOp::DivAssign => l.div(&r),
                            _ => unreachable!(),
                        };
                        self.assign_expr(left, result.clone());
                        result
                    }
                })
            }

            Expr::Assign { target, value } => {
                let v = self.eval_expr(value)?;
                self.assign_expr(target, v.clone());
                Ok(v)
            }

            Expr::Field { expr, field } => {
                let v = self.eval_expr(expr)?;
                if let Some(sw) = v.swizzle(field) {
                    return Ok(sw);
                }
                // Val::Struct field access.
                if let Val::Struct(ref map) = v {
                    return Ok(map.get(field.as_str()).cloned().unwrap_or(Val::Float(0.0)));
                }
                // UBO / struct member access: try `"baseName.fieldName"` in env (GAP-011).
                if let Expr::Var(base) = expr.as_ref() {
                    let key = alloc::format!("{}.{}", base, field);
                    return Ok(self.get(&key));
                }
                Ok(Val::Float(0.0))
            }

            Expr::Index { expr, idx } => {
                let v = self.eval_expr(expr)?;
                let i = self.eval_expr(idx)?.as_int() as usize;
                Ok(match &v {
                    Val::Vec2(a) => Val::Float(a.get(i).copied().unwrap_or(0.0)),
                    Val::Vec3(a) => Val::Float(a.get(i).copied().unwrap_or(0.0)),
                    Val::Vec4(a) => Val::Float(a.get(i).copied().unwrap_or(0.0)),
                    Val::Array(a) => a.get(i).cloned().unwrap_or(Val::Float(0.0)),
                    Val::Mat4(m) => Val::Vec4(
                        m.get(i)
                            .map(|col| [col[0], col[1], col[2], col[3]])
                            .unwrap_or([0.0; 4]),
                    ),
                    _ => Val::Float(0.0),
                })
            }

            Expr::Ternary { cond, then, els } => {
                let c = self.eval_expr(cond)?.as_bool();
                if c {
                    self.eval_expr(then)
                } else {
                    self.eval_expr(els)
                }
            }

            Expr::Call { name, args } => self.eval_call(name, args),
        }
    }

    fn assign_expr(&mut self, target: &Expr, val: Val) {
        match target {
            Expr::Var(name) => self.set(name, val),
            Expr::Field { expr, field } => {
                if let Expr::Var(name) = expr.as_ref() {
                    let mut base = self.get(name);
                    if let Val::Struct(ref mut map) = base {
                        map.insert(field.clone(), val);
                    } else {
                        set_swizzle(&mut base, field, &val);
                    }
                    self.set(name, base);
                }
            }
            Expr::Index { expr, idx } => {
                if let Expr::Var(name) = expr.as_ref() {
                    let i = self.eval_expr(idx).unwrap_or(Val::Int(0)).as_int() as usize;
                    let mut base = self.get(name);
                    match &mut base {
                        Val::Array(a) => {
                            if i < a.len() {
                                a[i] = val;
                            }
                        }
                        Val::Vec4(a) => {
                            if i < 4 {
                                a[i] = val.as_float();
                            }
                        }
                        Val::Vec3(a) => {
                            if i < 3 {
                                a[i] = val.as_float();
                            }
                        }
                        Val::Vec2(a) => {
                            if i < 2 {
                                a[i] = val.as_float();
                            }
                        }
                        _ => {}
                    }
                    self.set(name, base);
                }
            }
            _ => {}
        }
    }

    fn eval_call(&mut self, name: &str, args: &[Expr]) -> Result<Val, Signal> {
        let arg_vals: Vec<Val> = args
            .iter()
            .map(|a| self.eval_expr(a).unwrap_or(Val::Float(0.0)))
            .collect();
        match name {
            // ── Type constructors ────────────────────────────────────────
            "float" => Ok(Val::Float(
                arg_vals.first().map(|v| v.as_float()).unwrap_or(0.0),
            )),
            "int" => Ok(Val::Int(arg_vals.first().map(|v| v.as_int()).unwrap_or(0))),
            "uint" => Ok(Val::UInt(
                arg_vals.first().map(|v| v.as_int() as u32).unwrap_or(0),
            )),
            "bool" => Ok(Val::Bool(
                arg_vals.first().map(|v| v.as_bool()).unwrap_or(false),
            )),
            "vec2" => Ok(build_vec2(&arg_vals)),
            "vec3" => Ok(build_vec3(&arg_vals)),
            "vec4" => Ok(build_vec4(&arg_vals)),
            "ivec2" => {
                let v = build_vec2(&arg_vals);
                Ok(match v {
                    Val::Vec2([x, y]) => Val::IVec2([x as i32, y as i32]),
                    _ => v,
                })
            }
            "ivec3" => {
                let v = build_vec3(&arg_vals);
                Ok(match v {
                    Val::Vec3([x, y, z]) => Val::IVec3([x as i32, y as i32, z as i32]),
                    _ => v,
                })
            }
            "ivec4" => {
                let v = build_vec4(&arg_vals);
                Ok(match v {
                    Val::Vec4([x, y, z, w]) => Val::IVec4([x as i32, y as i32, z as i32, w as i32]),
                    _ => v,
                })
            }
            "mat2" => Ok(build_mat2(&arg_vals)),
            "mat3" => Ok(build_mat3(&arg_vals)),
            "mat4" => Ok(build_mat4(&arg_vals)),

            // ── Math builtins ────────────────────────────────────────────
            "abs" => Ok(unary_float_op(&arg_vals, libm::fabsf)),
            "sign" => Ok(unary_float_op(&arg_vals, |x| {
                if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            })),
            "floor" => Ok(unary_float_op(&arg_vals, libm::floorf)),
            "ceil" => Ok(unary_float_op(&arg_vals, libm::ceilf)),
            "round" => Ok(unary_float_op(&arg_vals, libm::roundf)),
            "fract" => Ok(unary_float_op(&arg_vals, |x| x - libm::floorf(x))),
            "sqrt" => Ok(unary_float_op(&arg_vals, libm::sqrtf)),
            "inversesqrt" => Ok(unary_float_op(&arg_vals, |x| {
                if x <= 0.0 { 0.0 } else { 1.0 / libm::sqrtf(x) }
            })),
            "sin" => Ok(unary_float_op(&arg_vals, libm::sinf)),
            "cos" => Ok(unary_float_op(&arg_vals, libm::cosf)),
            "tan" => Ok(unary_float_op(&arg_vals, libm::tanf)),
            "asin" => Ok(unary_float_op(&arg_vals, libm::asinf)),
            "acos" => Ok(unary_float_op(&arg_vals, libm::acosf)),
            "atan" => {
                if arg_vals.len() == 2 {
                    Ok(binary_float_op(&arg_vals, libm::atan2f))
                } else {
                    Ok(unary_float_op(&arg_vals, libm::atanf))
                }
            }
            "pow" => Ok(binary_float_op(&arg_vals, libm::powf)),
            "exp" => Ok(unary_float_op(&arg_vals, libm::expf)),
            "exp2" => Ok(unary_float_op(&arg_vals, libm::exp2f)),
            "log" => Ok(unary_float_op(&arg_vals, libm::logf)),
            "log2" => Ok(unary_float_op(&arg_vals, libm::log2f)),
            "mod" => Ok(binary_float_op(&arg_vals, |a, b| {
                if b == 0.0 {
                    0.0
                } else {
                    a - b * libm::floorf(a / b)
                }
            })),
            "min" => Ok(binary_float_op(&arg_vals, |a, b| if a < b { a } else { b })),
            "max" => Ok(binary_float_op(&arg_vals, |a, b| if a > b { a } else { b })),
            "clamp" => {
                let x = arg_vals.first().cloned().unwrap_or(Val::Float(0.0));
                let lo = arg_vals.get(1).cloned().unwrap_or(Val::Float(0.0));
                let hi = arg_vals.get(2).cloned().unwrap_or(Val::Float(1.0));
                Ok(x.max_val(&lo).min_val(&hi))
            }
            "mix" => {
                let x = arg_vals.first().cloned().unwrap_or(Val::Float(0.0));
                let y = arg_vals.get(1).cloned().unwrap_or(Val::Float(0.0));
                let a = arg_vals.get(2).cloned().unwrap_or(Val::Float(0.0));
                // mix(x,y,a) = x*(1-a) + y*a
                let one_minus_a = Val::Float(1.0).sub(&a);
                Ok(x.mul(&one_minus_a).add(&y.mul(&a)))
            }
            "step" => {
                let edge = arg_vals.first().cloned().unwrap_or(Val::Float(0.0));
                let x = arg_vals.get(1).cloned().unwrap_or(Val::Float(0.0));
                Ok(binary_float_op_vals(&edge, &x, |e, v| {
                    if v < e { 0.0 } else { 1.0 }
                }))
            }
            "smoothstep" => {
                let e0 = arg_vals.first().map(|v| v.as_float()).unwrap_or(0.0);
                let e1 = arg_vals.get(1).map(|v| v.as_float()).unwrap_or(1.0);
                let x = arg_vals.get(2).cloned().unwrap_or(Val::Float(0.0));
                Ok(unary_float_op(&[x], |v| {
                    let t = ((v - e0) / (e1 - e0)).clamp(0.0, 1.0);
                    t * t * (3.0 - 2.0 * t)
                }))
            }
            "length" => {
                let v = arg_vals.first().cloned().unwrap_or(Val::Float(0.0));
                Ok(Val::Float(vec_length(&v)))
            }
            "distance" => {
                let a = arg_vals.first().cloned().unwrap_or(Val::Float(0.0));
                let b = arg_vals.get(1).cloned().unwrap_or(Val::Float(0.0));
                Ok(Val::Float(vec_length(&a.sub(&b))))
            }
            "dot" => {
                let a = arg_vals.first().cloned().unwrap_or(Val::Float(0.0));
                let b = arg_vals.get(1).cloned().unwrap_or(Val::Float(0.0));
                Ok(Val::Float(vec_dot(&a, &b)))
            }
            "cross" => {
                let a = arg_vals.first().cloned().unwrap_or(Val::Vec3([0.0; 3]));
                let b = arg_vals.get(1).cloned().unwrap_or(Val::Vec3([0.0; 3]));
                let av = to_vec3(&a);
                let bv = to_vec3(&b);
                Ok(Val::Vec3([
                    av[1] * bv[2] - av[2] * bv[1],
                    av[2] * bv[0] - av[0] * bv[2],
                    av[0] * bv[1] - av[1] * bv[0],
                ]))
            }
            "normalize" => {
                let v = arg_vals.first().cloned().unwrap_or(Val::Float(1.0));
                let len = vec_length(&v);
                if len < 1e-20 {
                    return Ok(v);
                }
                Ok(v.div(&Val::Float(len)))
            }
            "reflect" => {
                // reflect(I, N) = I - 2*dot(N,I)*N
                let i = arg_vals.first().cloned().unwrap_or(Val::Vec3([0.0; 3]));
                let n = arg_vals
                    .get(1)
                    .cloned()
                    .unwrap_or(Val::Vec3([0.0, 1.0, 0.0]));
                let d = 2.0 * vec_dot(&n, &i);
                Ok(i.sub(&n.mul(&Val::Float(d))))
            }
            "refract" => {
                let i = arg_vals.first().cloned().unwrap_or(Val::Vec3([0.0; 3]));
                let n = arg_vals
                    .get(1)
                    .cloned()
                    .unwrap_or(Val::Vec3([0.0, 1.0, 0.0]));
                let eta = arg_vals.get(2).map(|v| v.as_float()).unwrap_or(1.0);
                let d = vec_dot(&n, &i);
                let k = 1.0 - eta * eta * (1.0 - d * d);
                if k < 0.0 {
                    Ok(Val::Vec3([0.0; 3]))
                } else {
                    let scale_i = eta;
                    let scale_n = eta * d + libm::sqrtf(k);
                    Ok(i.mul(&Val::Float(scale_i))
                        .sub(&n.mul(&Val::Float(scale_n))))
                }
            }
            // GAP-007: analytic screen-space derivatives.
            // Strategy: if args[0] is a variable reference, look up the pre-injected
            // derivative variable `_dvdx_<name>` / `_dvdy_<name>`.  For a swizzle /
            // field access of the form `expr.swiz`, look up the derivative of the
            // base variable and apply the same swizzle component selection.
            // Fallback: return 0 (original behaviour) for arbitrary expressions.
            "dFdx" => {
                let deriv = Self::resolve_deriv(args.first(), "_dvdx_", self);
                Ok(deriv)
            }
            "dFdy" => {
                let deriv = Self::resolve_deriv(args.first(), "_dvdy_", self);
                Ok(deriv)
            }
            "fwidth" => {
                let dx = Self::resolve_deriv(args.first(), "_dvdx_", self);
                let dy = Self::resolve_deriv(args.first(), "_dvdy_", self);
                Ok(unary_float_op(&[dx.add(&dy)], libm::fabsf))
            }

            // ── Texture sampling ─────────────────────────────────────────
            "texture" | "texture2D" => {
                let sampler_slot = arg_vals.first().map(|v| v.as_int()).unwrap_or(0) as usize;
                let uv = arg_vals.get(1).cloned().unwrap_or(Val::Vec2([0.0; 2]));
                // 2D array: coord is vec3(s, t, layer)
                if let Val::Vec3([cx, cy, cz]) = &uv {
                    if let Some(Some(owned)) = self.owned_textures.get(sampler_slot) {
                        if !owned.layers.is_empty() {
                            let layer = (*cz as i32).max(0) as usize;
                            let layer = layer.min(owned.layers.len().saturating_sub(1));
                            let uv2 = Vec2 { x: *cx, y: *cy };
                            return Ok(sample_layer(owned, uv2, layer));
                        }
                    }
                    // Fallthrough: treat as 2D with only s, t
                }
                let uv2 = match &uv {
                    Val::Vec2(v) => Vec2 { x: v[0], y: v[1] },
                    Val::Vec3(v) => Vec2 { x: v[0], y: v[1] },
                    _ => Vec2 { x: 0.0, y: 0.0 },
                };
                if let Some(Some(owned)) = self.owned_textures.get(sampler_slot) {
                    Ok(sample_owned_typed(owned, uv2))
                } else if let Some(Some(tex)) = self.textures.get(sampler_slot) {
                    let rgba = tex.sample(uv2);
                    Ok(Val::Vec4([rgba.x, rgba.y, rgba.z, rgba.w]))
                } else {
                    Ok(Val::Vec4([1.0, 0.0, 1.0, 1.0])) // magenta = missing texture
                }
            }

            // textureCube(samplerCube, vec3 direction) → vec4
            "textureCube" | "texture(samplerCube)" => {
                let sampler_slot = arg_vals.first().map(|v| v.as_int()).unwrap_or(0) as usize;
                let dir = arg_vals
                    .get(1)
                    .cloned()
                    .unwrap_or(Val::Vec3([0.0, 0.0, 1.0]));
                let [dx, dy, dz] = match &dir {
                    Val::Vec3(v) => *v,
                    Val::Vec4(v) => [v[0], v[1], v[2]],
                    _ => [0.0, 0.0, 1.0],
                };
                let (face, u, v) = cube_dir_to_face_uv(dx, dy, dz);
                let uv2 = Vec2 { x: u, y: v };
                if let Some(Some(owned)) = self.owned_textures.get(sampler_slot) {
                    // Try to sample from the dedicated cube face if it has pixels
                    let rgba =
                        if owned.cube_faces[face].len() >= (owned.width * owned.height) as usize {
                            let face_tex = Texture {
                                pixels: &owned.cube_faces[face],
                                width: owned.width,
                                height: owned.height,
                                wrap_s: owned.wrap_s,
                                wrap_t: owned.wrap_t,
                                min_filter: owned.min_filter,
                                mag_filter: owned.mag_filter,
                                border_color: [0.0; 4],
                            };
                            face_tex.sample(uv2)
                        } else {
                            // Fall back to base texture sampled at mapped UV
                            let base_tex = Texture {
                                pixels: &owned.pixels,
                                width: owned.width,
                                height: owned.height,
                                wrap_s: owned.wrap_s,
                                wrap_t: owned.wrap_t,
                                min_filter: owned.min_filter,
                                mag_filter: owned.mag_filter,
                                border_color: [0.0; 4],
                            };
                            base_tex.sample(uv2)
                        };
                    Ok(Val::Vec4([rgba.x, rgba.y, rgba.z, rgba.w]))
                } else {
                    Ok(Val::Vec4([1.0, 0.0, 1.0, 1.0]))
                }
            }

            // texture3D(sampler3D, vec3 coord) → vec4
            // Z coordinate selects the slice; layers are stored in mip_levels (one per slice).
            "texture3D" => {
                let sampler_slot = arg_vals.first().map(|v| v.as_int()).unwrap_or(0) as usize;
                let coord = arg_vals.get(1).cloned().unwrap_or(Val::Vec3([0.0; 3]));
                let [cx, cy, cz] = match &coord {
                    Val::Vec3(v) => *v,
                    Val::Vec4(v) => [v[0], v[1], v[2]],
                    _ => [0.0, 0.0, 0.0],
                };
                let uv2 = Vec2 { x: cx, y: cy };
                if let Some(Some(owned)) = self.owned_textures.get(sampler_slot) {
                    // Use z to index into available slices (mip_levels used as layers here)
                    let num_slices = 1 + owned.mip_levels.len();
                    let slice_f = cz.clamp(0.0, 1.0) * (num_slices - 1) as f32;
                    let s0 = libm::floorf(slice_f) as usize;
                    let frac = slice_f - s0 as f32;
                    let c0 = sample_owned_at_lod(owned, uv2, s0 as i32);
                    let c1 = sample_owned_at_lod(owned, uv2, (s0 + 1) as i32);
                    let rgba = Vec4::new(
                        c0.x + (c1.x - c0.x) * frac,
                        c0.y + (c1.y - c0.y) * frac,
                        c0.z + (c1.z - c0.z) * frac,
                        c0.w + (c1.w - c0.w) * frac,
                    );
                    Ok(Val::Vec4([rgba.x, rgba.y, rgba.z, rgba.w]))
                } else {
                    Ok(Val::Vec4([1.0, 0.0, 1.0, 1.0]))
                }
            }

            // textureLod(sampler, coord, lod) → vec4
            "textureLod" => {
                let sampler_slot = arg_vals.first().map(|v| v.as_int()).unwrap_or(0) as usize;
                let uv = arg_vals.get(1).cloned().unwrap_or(Val::Vec2([0.0; 2]));
                let lod = arg_vals.get(2).map(|v| v.as_float()).unwrap_or(0.0);
                let uv2 = match &uv {
                    Val::Vec2(v) => Vec2 { x: v[0], y: v[1] },
                    _ => Vec2 { x: 0.0, y: 0.0 },
                };
                if let Some(Some(owned)) = self.owned_textures.get(sampler_slot) {
                    let rgba = sample_owned_trilinear(owned, uv2, lod);
                    Ok(Val::Vec4([rgba.x, rgba.y, rgba.z, rgba.w]))
                } else if let Some(Some(tex)) = self.textures.get(sampler_slot) {
                    let rgba = tex.sample(uv2);
                    Ok(Val::Vec4([rgba.x, rgba.y, rgba.z, rgba.w]))
                } else {
                    Ok(Val::Vec4([1.0, 0.0, 1.0, 1.0]))
                }
            }

            // textureGrad(sampler, coord, dPdx, dPdy) → vec4
            "textureGrad" => {
                let sampler_slot = arg_vals.first().map(|v| v.as_int()).unwrap_or(0) as usize;
                let uv = arg_vals.get(1).cloned().unwrap_or(Val::Vec2([0.0; 2]));
                let ddx_val = arg_vals.get(2).cloned().unwrap_or(Val::Vec2([0.0; 2]));
                let ddy_val = arg_vals.get(3).cloned().unwrap_or(Val::Vec2([0.0; 2]));
                let uv2 = match &uv {
                    Val::Vec2(v) => Vec2 { x: v[0], y: v[1] },
                    _ => Vec2 { x: 0.0, y: 0.0 },
                };
                let ddx = match &ddx_val {
                    Val::Vec2(v) => Vec2 { x: v[0], y: v[1] },
                    _ => Vec2 { x: 0.0, y: 0.0 },
                };
                let ddy = match &ddy_val {
                    Val::Vec2(v) => Vec2 { x: v[0], y: v[1] },
                    _ => Vec2 { x: 0.0, y: 0.0 },
                };
                let lod = lod_from_gradients(ddx, ddy);
                if let Some(Some(owned)) = self.owned_textures.get(sampler_slot) {
                    let rgba = sample_owned_trilinear(owned, uv2, lod);
                    Ok(Val::Vec4([rgba.x, rgba.y, rgba.z, rgba.w]))
                } else if let Some(Some(tex)) = self.textures.get(sampler_slot) {
                    let rgba = tex.sample(uv2);
                    Ok(Val::Vec4([rgba.x, rgba.y, rgba.z, rgba.w]))
                } else {
                    Ok(Val::Vec4([1.0, 0.0, 1.0, 1.0]))
                }
            }

            // texelFetch(sampler, ivec2 coord, int lod) → vec4
            "texelFetch" => {
                let sampler_slot = arg_vals.first().map(|v| v.as_int()).unwrap_or(0) as usize;
                let coord = arg_vals.get(1).cloned().unwrap_or(Val::IVec2([0; 2]));
                let lod = arg_vals.get(2).map(|v| v.as_int()).unwrap_or(0).max(0) as usize;
                let (ix, iy) = match &coord {
                    Val::IVec2(v) => (v[0].max(0) as u32, v[1].max(0) as u32),
                    Val::Vec2(v) => (v[0].max(0.0) as u32, v[1].max(0.0) as u32),
                    _ => (0, 0),
                };
                if let Some(Some(owned)) = self.owned_textures.get(sampler_slot) {
                    let (pixels, w, h) = if lod == 0 || owned.mip_levels.is_empty() {
                        (&owned.pixels, owned.width, owned.height)
                    } else {
                        let idx = (lod - 1).min(owned.mip_levels.len() - 1);
                        let (mw, mh, mpx) = &owned.mip_levels[idx];
                        (mpx, *mw, *mh)
                    };
                    let px = ix.min(w.saturating_sub(1));
                    let py = iy.min(h.saturating_sub(1));
                    let p = pixels.get((py * w + px) as usize).copied().unwrap_or(0);
                    let r = ((p >> 16) & 0xFF) as f32 / 255.0;
                    let g = ((p >> 8) & 0xFF) as f32 / 255.0;
                    let b = (p & 0xFF) as f32 / 255.0;
                    let a = ((p >> 24) & 0xFF) as f32 / 255.0;
                    Ok(Val::Vec4([r, g, b, a]))
                } else {
                    Ok(Val::Vec4([0.0; 4]))
                }
            }

            // ── Vector method aliases ────────────────────────────────────
            "_method_length" => {
                let v = arg_vals.first().cloned().unwrap_or(Val::Float(0.0));
                Ok(Val::Float(vec_length(&v)))
            }

            // GAP-021: textureSize(sampler, lod) → ivec2
            "textureSize" => {
                let sampler_slot = arg_vals.first().map(|v| v.as_int()).unwrap_or(0) as usize;
                let (w, h) = if let Some(Some(owned)) = self.owned_textures.get(sampler_slot) {
                    let lod = arg_vals.get(1).map(|v| v.as_int()).unwrap_or(0).max(0) as usize;
                    if lod == 0 || owned.mip_levels.is_empty() {
                        (owned.width as i32, owned.height as i32)
                    } else {
                        let idx = (lod - 1).min(owned.mip_levels.len() - 1);
                        (
                            owned.mip_levels[idx].0 as i32,
                            owned.mip_levels[idx].1 as i32,
                        )
                    }
                } else {
                    (0, 0)
                };
                Ok(Val::IVec2([w, h]))
            }

            // GAP-021: texture2DProj(sampler2D, vec3/vec4) — divides by last component
            "texture2DProj" => {
                let sampler_slot = arg_vals.first().map(|v| v.as_int()).unwrap_or(0) as usize;
                let coord = arg_vals.get(1).cloned().unwrap_or(Val::Vec4([0.0; 4]));
                let (u, v_coord, divisor) = match &coord {
                    Val::Vec3(c) => (c[0], c[1], c[2]),
                    Val::Vec4(c) => (c[0], c[1], c[3]),
                    _ => (0.0, 0.0, 1.0),
                };
                let inv = if divisor.abs() < 1e-10 {
                    1.0
                } else {
                    1.0 / divisor
                };
                let uv2 = Vec2 {
                    x: u * inv,
                    y: v_coord * inv,
                };
                if let Some(Some(owned)) = self.owned_textures.get(sampler_slot) {
                    Ok(sample_owned_typed(owned, uv2))
                } else if let Some(Some(tex)) = self.textures.get(sampler_slot) {
                    let rgba = tex.sample(uv2);
                    Ok(Val::Vec4([rgba.x, rgba.y, rgba.z, rgba.w]))
                } else {
                    Ok(Val::Vec4([1.0, 0.0, 1.0, 1.0]))
                }
            }

            // ── Atomic operations (single-threaded: no real atomicity needed) ────
            // All atomics take a memory reference as first arg and return the old value.
            // Since we evaluate args eagerly, we perform the op and assign back via
            // the first argument expression if it is a variable or field access.
            "atomicAdd" => {
                let old = arg_vals.first().cloned().unwrap_or(Val::Int(0));
                let val = arg_vals.get(1).cloned().unwrap_or(Val::Int(0));
                let new_val = old.add(&val);
                if let Some(lhs) = args.first() {
                    let _ = self.assign_lvalue(lhs, new_val);
                }
                Ok(old)
            }
            "atomicMin" => {
                let old = arg_vals.first().cloned().unwrap_or(Val::Int(0));
                let val = arg_vals.get(1).cloned().unwrap_or(Val::Int(0));
                let new_val = old.min_val(&val);
                if let Some(lhs) = args.first() {
                    let _ = self.assign_lvalue(lhs, new_val);
                }
                Ok(old)
            }
            "atomicMax" => {
                let old = arg_vals.first().cloned().unwrap_or(Val::Int(0));
                let val = arg_vals.get(1).cloned().unwrap_or(Val::Int(0));
                let new_val = old.max_val(&val);
                if let Some(lhs) = args.first() {
                    let _ = self.assign_lvalue(lhs, new_val);
                }
                Ok(old)
            }
            "atomicAnd" => {
                let old = arg_vals.first().cloned().unwrap_or(Val::Int(0));
                let val = arg_vals.get(1).cloned().unwrap_or(Val::Int(0));
                let new_val = Val::Int(old.as_int() & val.as_int());
                if let Some(lhs) = args.first() {
                    let _ = self.assign_lvalue(lhs, new_val);
                }
                Ok(old)
            }
            "atomicOr" => {
                let old = arg_vals.first().cloned().unwrap_or(Val::Int(0));
                let val = arg_vals.get(1).cloned().unwrap_or(Val::Int(0));
                let new_val = Val::Int(old.as_int() | val.as_int());
                if let Some(lhs) = args.first() {
                    let _ = self.assign_lvalue(lhs, new_val);
                }
                Ok(old)
            }
            "atomicXor" => {
                let old = arg_vals.first().cloned().unwrap_or(Val::Int(0));
                let val = arg_vals.get(1).cloned().unwrap_or(Val::Int(0));
                let new_val = Val::Int(old.as_int() ^ val.as_int());
                if let Some(lhs) = args.first() {
                    let _ = self.assign_lvalue(lhs, new_val);
                }
                Ok(old)
            }
            "atomicExchange" => {
                let old = arg_vals.first().cloned().unwrap_or(Val::Int(0));
                let val = arg_vals.get(1).cloned().unwrap_or(Val::Int(0));
                if let Some(lhs) = args.first() {
                    let _ = self.assign_lvalue(lhs, val);
                }
                Ok(old)
            }
            "atomicCompSwap" => {
                let old = arg_vals.first().cloned().unwrap_or(Val::Int(0));
                let compare = arg_vals.get(1).cloned().unwrap_or(Val::Int(0));
                let val = arg_vals.get(2).cloned().unwrap_or(Val::Int(0));
                if old == compare {
                    if let Some(lhs) = args.first() {
                        let _ = self.assign_lvalue(lhs, val);
                    }
                }
                Ok(old)
            }
            // Memory barriers — no-ops in single-threaded software renderer
            "barrier"
            | "memoryBarrier"
            | "memoryBarrierAtomicCounter"
            | "memoryBarrierBuffer"
            | "memoryBarrierImage"
            | "memoryBarrierShared"
            | "groupMemoryBarrier" => Ok(Val::Float(0.0)),

            // ── User-defined function ────────────────────────────────────
            _ => {
                // Check if it's a struct constructor first.
                if let Some(fields) = self.struct_defs.get(name).cloned() {
                    let mut map = BTreeMap::new();
                    for (i, (_fty, fname)) in fields.iter().enumerate() {
                        let v = arg_vals.get(i).cloned().unwrap_or(Val::Float(0.0));
                        map.insert(fname.clone(), v);
                    }
                    return Ok(Val::Struct(map));
                }
                // Look up user function (avoid borrow conflict by cloning)
                let func = self.funcs.get(name).cloned();
                if let Some(func) = func {
                    self.push_scope();
                    for ((_, pname), val) in func.params.iter().zip(arg_vals.iter()) {
                        self.declare(pname, val.clone());
                    }
                    let result = self.exec_stmts(&func.body);
                    self.pop_scope();
                    match result {
                        Err(Signal::Return(Some(v))) => Ok(v),
                        Err(Signal::Return(None)) => Ok(Val::Float(0.0)),
                        Err(other) => Err(other),
                        Ok(()) => Ok(Val::Float(0.0)),
                    }
                } else {
                    Ok(Val::Float(0.0)) // unknown call — return 0
                }
            }
        }
    }

    /// Helper for `dFdx` / `dFdy`: given an optional expression and a derivative
    /// prefix (`"_dvdx_"` or `"_dvdy_"`), try to resolve the derivative.
    ///
    /// * `Expr::Var(name)` → look up `_dvdx_{name}` in env.
    /// * `Expr::Field { expr: Var(name), field }` (swizzle) → look up the
    ///   derivative of the base var and apply the same swizzle.
    /// * Anything else → `Val::Float(0.0)` (fallback, original behaviour).
    fn resolve_deriv(expr: Option<&Expr>, prefix: &str, env: &Env<'_>) -> Val {
        let Some(expr) = expr else {
            return Val::Float(0.0);
        };
        match expr {
            Expr::Var(name) => {
                let key = alloc::format!("{}{}", prefix, name);
                env.get(&key)
            }
            Expr::Field { expr: base, field } => {
                if let Expr::Var(base_name) = base.as_ref() {
                    let key = alloc::format!("{}{}", prefix, base_name);
                    let base_deriv = env.get(&key);
                    // Apply the swizzle/field to the derivative.
                    Self::swizzle_val(&base_deriv, field)
                } else {
                    Val::Float(0.0)
                }
            }
            _ => Val::Float(0.0),
        }
    }

    /// Apply a GLSL field access / swizzle string to a `Val`.
    fn swizzle_val(val: &Val, field: &str) -> Val {
        let comps = match val {
            Val::Vec4(v) => [v[0], v[1], v[2], v[3]],
            Val::Vec3(v) => [v[0], v[1], v[2], 0.0],
            Val::Vec2(v) => [v[0], v[1], 0.0, 0.0],
            Val::Float(f) => [*f, *f, *f, *f],
            _ => return val.clone(),
        };
        let idx = |c: char| -> f32 {
            match c {
                'x' | 'r' | 's' => comps[0],
                'y' | 'g' | 't' => comps[1],
                'z' | 'b' | 'p' => comps[2],
                'w' | 'a' | 'q' => comps[3],
                _ => 0.0,
            }
        };
        let chars: Vec<char> = field.chars().collect();
        match chars.len() {
            1 => Val::Float(idx(chars[0])),
            2 => Val::Vec2([idx(chars[0]), idx(chars[1])]),
            3 => Val::Vec3([idx(chars[0]), idx(chars[1]), idx(chars[2])]),
            4 => Val::Vec4([idx(chars[0]), idx(chars[1]), idx(chars[2]), idx(chars[3])]),
            _ => val.clone(),
        }
    }

    fn exec_stmts(&mut self, stmts: &[Stmt]) -> Result<(), Signal> {
        for s in stmts {
            self.exec_stmt(s)?;
        }
        Ok(())
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<(), Signal> {
        match stmt {
            Stmt::VarDecl { ty, name, init } => {
                let v = if let Some(e) = init.as_ref() {
                    self.eval_expr(e).unwrap_or(Val::Float(0.0))
                } else if let Some(fields) = self.struct_defs.get(ty.as_str()).cloned() {
                    // Zero-initialize struct.
                    let map: BTreeMap<String, Val> = fields
                        .into_iter()
                        .map(|(_fty, fn_)| (fn_, Val::Float(0.0)))
                        .collect();
                    Val::Struct(map)
                } else {
                    Val::Float(0.0)
                };
                self.declare(name, v);
                Ok(())
            }
            Stmt::Expr(e) => {
                self.eval_expr(e)?;
                Ok(())
            }
            Stmt::Block(stmts) => {
                self.push_scope();
                let r = self.exec_stmts(stmts);
                self.pop_scope();
                r
            }
            Stmt::If { cond, then, els } => {
                if self.eval_expr(cond)?.as_bool() {
                    self.exec_stmt(then)
                } else if let Some(e) = els {
                    self.exec_stmt(e)
                } else {
                    Ok(())
                }
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.push_scope();
                if let Some(s) = init {
                    self.exec_stmt(s)?;
                }
                let mut iters = 0usize;
                loop {
                    if iters > 65536 {
                        break;
                    } // guard against infinite loops
                    iters += 1;
                    if let Some(c) = cond {
                        if !self.eval_expr(c)?.as_bool() {
                            break;
                        }
                    }
                    match self.exec_stmt(body) {
                        Err(Signal::Break) => break,
                        Err(Signal::Continue) => {}
                        Err(other) => {
                            self.pop_scope();
                            return Err(other);
                        }
                        Ok(()) => {}
                    }
                    if let Some(s) = step {
                        self.eval_expr(s)?;
                    }
                }
                self.pop_scope();
                Ok(())
            }
            Stmt::While { cond, body } => {
                let mut iters = 0usize;
                loop {
                    if iters > 65536 {
                        break;
                    }
                    iters += 1;
                    if !self.eval_expr(cond)?.as_bool() {
                        break;
                    }
                    match self.exec_stmt(body) {
                        Err(Signal::Break) => break,
                        Err(Signal::Continue) => {}
                        Err(other) => return Err(other),
                        Ok(()) => {}
                    }
                }
                Ok(())
            }
            Stmt::DoWhile { body, cond } => {
                let mut iters = 0usize;
                loop {
                    if iters > 65536 {
                        break;
                    }
                    iters += 1;
                    match self.exec_stmt(body) {
                        Err(Signal::Break) => break,
                        Err(Signal::Continue) => {}
                        Err(other) => return Err(other),
                        Ok(()) => {}
                    }
                    if !self.eval_expr(cond)?.as_bool() {
                        break;
                    }
                }
                Ok(())
            }
            Stmt::Return(e) => Err(Signal::Return(
                e.as_ref()
                    .map(|ex| self.eval_expr(ex).unwrap_or(Val::Float(0.0))),
            )),
            Stmt::Break => Err(Signal::Break),
            Stmt::Continue => Err(Signal::Continue),
            Stmt::Discard => Err(Signal::Discard),
        }
    }
}

// ─── Val extra helpers ────────────────────────────────────────────────────────

impl Val {
    fn max_val(&self, other: &Val) -> Val {
        binary_float_op_vals(self, other, |a, b| if a > b { a } else { b })
    }
    fn min_val(&self, other: &Val) -> Val {
        binary_float_op_vals(self, other, |a, b| if a < b { a } else { b })
    }
}

fn unary_float_op(args: &[Val], f: impl Fn(f32) -> f32 + Copy) -> Val {
    match args.first() {
        Some(Val::Float(v)) => Val::Float(f(*v)),
        Some(Val::Vec2(v)) => Val::Vec2([f(v[0]), f(v[1])]),
        Some(Val::Vec3(v)) => Val::Vec3([f(v[0]), f(v[1]), f(v[2])]),
        Some(Val::Vec4(v)) => Val::Vec4([f(v[0]), f(v[1]), f(v[2]), f(v[3])]),
        Some(other) => Val::Float(f(other.as_float())),
        None => Val::Float(0.0),
    }
}

fn binary_float_op(args: &[Val], f: impl Fn(f32, f32) -> f32) -> Val {
    let a = args.first().cloned().unwrap_or(Val::Float(0.0));
    let b = args.get(1).cloned().unwrap_or(Val::Float(0.0));
    binary_float_op_vals(&a, &b, f)
}

fn binary_float_op_vals(a: &Val, b: &Val, f: impl Fn(f32, f32) -> f32) -> Val {
    let bscalar = b.as_float();
    match a {
        Val::Float(v) => Val::Float(f(*v, bscalar)),
        Val::Vec2(v) => {
            let [bx, by] = match b {
                Val::Vec2(q) => *q,
                _ => [bscalar; 2],
            };
            Val::Vec2([f(v[0], bx), f(v[1], by)])
        }
        Val::Vec3(v) => {
            let [bx, by, bz] = match b {
                Val::Vec3(q) => *q,
                _ => [bscalar; 3],
            };
            Val::Vec3([f(v[0], bx), f(v[1], by), f(v[2], bz)])
        }
        Val::Vec4(v) => {
            let [bx, by, bz, bw] = match b {
                Val::Vec4(q) => *q,
                _ => [bscalar; 4],
            };
            Val::Vec4([f(v[0], bx), f(v[1], by), f(v[2], bz), f(v[3], bw)])
        }
        other => Val::Float(f(other.as_float(), bscalar)),
    }
}

fn vec_length(v: &Val) -> f32 {
    let d = vec_dot(v, v);
    libm::sqrtf(d)
}

fn vec_dot(a: &Val, b: &Val) -> f32 {
    match (a, b) {
        (Val::Float(x), Val::Float(y)) => x * y,
        (Val::Vec2(u), Val::Vec2(v)) => u[0] * v[0] + u[1] * v[1],
        (Val::Vec3(u), Val::Vec3(v)) => u[0] * v[0] + u[1] * v[1] + u[2] * v[2],
        (Val::Vec4(u), Val::Vec4(v)) => u[0] * v[0] + u[1] * v[1] + u[2] * v[2] + u[3] * v[3],
        _ => a.as_float() * b.as_float(),
    }
}

fn to_vec3(v: &Val) -> [f32; 3] {
    match v {
        Val::Vec3(a) => *a,
        Val::Vec4(a) => [a[0], a[1], a[2]],
        Val::Vec2(a) => [a[0], a[1], 0.0],
        other => [other.as_float(); 3],
    }
}

// ─── Texture sampling helpers ─────────────────────────────────────────────────

/// Convert a cube-map direction vector to a (face_index, u, v) tuple.
/// Face indices: 0=+X, 1=-X, 2=+Y, 3=-Y, 4=+Z, 5=-Z.
fn cube_dir_to_face_uv(x: f32, y: f32, z: f32) -> (usize, f32, f32) {
    let ax = libm::fabsf(x);
    let ay = libm::fabsf(y);
    let az = libm::fabsf(z);
    if ax >= ay && ax >= az {
        if x > 0.0 {
            (0, (-z / ax + 1.0) * 0.5, (y / ax + 1.0) * 0.5)
        } else {
            (1, (z / ax + 1.0) * 0.5, (y / ax + 1.0) * 0.5)
        }
    } else if ay >= ax && ay >= az {
        if y > 0.0 {
            (2, (x / ay + 1.0) * 0.5, (-z / ay + 1.0) * 0.5)
        } else {
            (3, (x / ay + 1.0) * 0.5, (z / ay + 1.0) * 0.5)
        }
    } else {
        if z > 0.0 {
            (4, (x / az + 1.0) * 0.5, (y / az + 1.0) * 0.5)
        } else {
            (5, (-x / az + 1.0) * 0.5, (y / az + 1.0) * 0.5)
        }
    }
}

/// Sample a specific mip level from a `TextureOwned`, or the base level if `lod` is out of range.
fn sample_owned_at_lod(owned: &TextureOwned, uv: Vec2, lod: i32) -> Vec4 {
    let lod = lod.max(0) as usize;
    let (pixels, w, h) = if lod == 0 || owned.mip_levels.is_empty() {
        (&owned.pixels, owned.width, owned.height)
    } else {
        let idx = (lod - 1).min(owned.mip_levels.len() - 1);
        let (mw, mh, mpx) = &owned.mip_levels[idx];
        (mpx, *mw, *mh)
    };
    let tex = Texture {
        pixels,
        width: w,
        height: h,
        wrap_s: owned.wrap_s,
        wrap_t: owned.wrap_t,
        min_filter: owned.min_filter,
        mag_filter: owned.mag_filter,
        border_color: [0.0; 4],
    };
    tex.sample(uv)
}

/// Compute the mip LOD from explicit dPdx/dPdy gradients.
fn lod_from_gradients(ddx: Vec2, ddy: Vec2) -> f32 {
    let ddx2 = ddx.x * ddx.x + ddx.y * ddx.y;
    let ddy2 = ddy.x * ddy.x + ddy.y * ddy.y;
    if ddx2 < 1e-20 && ddy2 < 1e-20 {
        return 0.0;
    }
    let max2 = if ddx2 > ddy2 { ddx2 } else { ddy2 };
    0.5 * libm::log2f(max2)
}

/// Trilinear sample between two mip levels.
fn sample_owned_trilinear(owned: &TextureOwned, uv: Vec2, lod: f32) -> Vec4 {
    let lod = lod.max(0.0);
    let lod0 = libm::floorf(lod) as i32;
    let frac = lod - lod0 as f32;
    let c0 = sample_owned_at_lod(owned, uv, lod0);
    if frac < 1e-4 {
        return c0;
    }
    let c1 = sample_owned_at_lod(owned, uv, lod0 + 1);
    Vec4::new(
        c0.x + (c1.x - c0.x) * frac,
        c0.y + (c1.y - c0.y) * frac,
        c0.z + (c1.z - c0.z) * frac,
        c0.w + (c1.w - c0.w) * frac,
    )
}

/// Unpack a raw pixel BGRA32 as integer components (B=chan0, G=chan1, R=chan2, A=chan3).
/// Returns components as signed i32 bytes (-128..127).
fn unpack_int(p: u32) -> [i32; 4] {
    let b = (p & 0xFF) as i8 as i32;
    let g = ((p >> 8) & 0xFF) as i8 as i32;
    let r = ((p >> 16) & 0xFF) as i8 as i32;
    let a = ((p >> 24) & 0xFF) as i8 as i32;
    [r, g, b, a]
}

/// Unpack a raw pixel BGRA32 as unsigned integer components (B=chan0, G=chan1, R=chan2, A=chan3).
fn unpack_uint(p: u32) -> [u32; 4] {
    let b = p & 0xFF;
    let g = (p >> 8) & 0xFF;
    let r = (p >> 16) & 0xFF;
    let a = (p >> 24) & 0xFF;
    [r, g, b, a]
}

/// Sample a TextureOwned and return the appropriate Val type based on the texture format.
fn sample_owned_typed(owned: &TextureOwned, uv: Vec2) -> Val {
    let tex = Texture {
        pixels: &owned.pixels,
        width: owned.width,
        height: owned.height,
        wrap_s: owned.wrap_s,
        wrap_t: owned.wrap_t,
        min_filter: owned.min_filter,
        mag_filter: owned.mag_filter,
        border_color: [0.0; 4],
    };
    match owned.format {
        TextureFormat::Float => {
            let rgba = tex.sample(uv);
            Val::Vec4([rgba.x, rgba.y, rgba.z, rgba.w])
        }
        TextureFormat::Int => {
            // Fetch the nearest texel and unpack as integers
            let rgba = tex.sample_nearest(uv);
            // Approximate: convert normalized floats back to [-128, 127] range
            let ix = (rgba.x * 255.0 - 128.0) as i32;
            let iy = (rgba.y * 255.0 - 128.0) as i32;
            let iz = (rgba.z * 255.0 - 128.0) as i32;
            let iw = (rgba.w * 255.0 - 128.0) as i32;
            Val::IVec4([ix, iy, iz, iw])
        }
        TextureFormat::UInt => {
            let rgba = tex.sample_nearest(uv);
            let ux = (rgba.x * 255.0) as u32;
            let uy = (rgba.y * 255.0) as u32;
            let uz = (rgba.z * 255.0) as u32;
            let uw = (rgba.w * 255.0) as u32;
            Val::UVec4([ux, uy, uz, uw])
        }
        TextureFormat::Depth => {
            // Pixels are stored as f32::to_bits(depth).
            // Use nearest-neighbor fetch via the Texture struct, but recover the
            // raw u32 bits rather than the BGRA-decoded float channels.
            // wrap_coord is available on Texture<'_> via sample_nearest — we
            // reconstruct the index the same way.
            let x = {
                let u = uv.x;
                let w = owned.width;
                let u = match owned.wrap_s {
                    crate::gl::WrapMode::Repeat => u - libm::floorf(u),
                    crate::gl::WrapMode::MirroredRepeat => {
                        let fi = libm::floorf(u) as i32;
                        let frac = u - fi as f32;
                        if fi & 1 == 0 { frac } else { 1.0 - frac }
                    }
                    _ => u.clamp(0.0, 1.0),
                };
                ((u * w as f32) as u32).min(w.saturating_sub(1))
            };
            let y = {
                let v = uv.y;
                let h = owned.height;
                let v = match owned.wrap_t {
                    crate::gl::WrapMode::Repeat => v - libm::floorf(v),
                    crate::gl::WrapMode::MirroredRepeat => {
                        let fi = libm::floorf(v) as i32;
                        let frac = v - fi as f32;
                        if fi & 1 == 0 { frac } else { 1.0 - frac }
                    }
                    _ => v.clamp(0.0, 1.0),
                };
                ((v * h as f32) as u32).min(h.saturating_sub(1))
            };
            let idx = y as usize * owned.width as usize + x as usize;
            let d = f32::from_bits(*owned.pixels.get(idx).unwrap_or(&0));
            Val::Vec4([d, d, d, 1.0])
        }
    }
}

/// Sample a specific layer of a 2D texture array. `layer` is the 0-based array layer index.
fn sample_layer(owned: &TextureOwned, uv: Vec2, layer: usize) -> Val {
    let pixels = owned
        .layers
        .get(layer)
        .map(|l| l.as_slice())
        .unwrap_or(&owned.pixels);
    let tex = Texture {
        pixels,
        width: owned.width,
        height: owned.height,
        wrap_s: owned.wrap_s,
        wrap_t: owned.wrap_t,
        min_filter: owned.min_filter,
        mag_filter: owned.mag_filter,
        border_color: [0.0; 4],
    };
    let rgba = tex.sample(uv);
    Val::Vec4([rgba.x, rgba.y, rgba.z, rgba.w])
}

fn set_swizzle(base: &mut Val, field: &str, val: &Val) {
    let val_comps: Vec<f32> = match val {
        Val::Vec4(v) => v.to_vec().iter().map(|&x| x).collect(),
        Val::Vec3(v) => v.to_vec().iter().map(|&x| x).collect(),
        Val::Vec2(v) => v.to_vec().iter().map(|&x| x).collect(),
        other => vec![other.as_float()],
    };
    let indices: Vec<usize> = field.chars().filter_map(swizzle_idx).collect();
    match base {
        Val::Vec4(v) => {
            for (i, &idx) in indices.iter().enumerate() {
                if idx < 4 {
                    v[idx] = val_comps.get(i).copied().unwrap_or(0.0);
                }
            }
        }
        Val::Vec3(v) => {
            for (i, &idx) in indices.iter().enumerate() {
                if idx < 3 {
                    v[idx] = val_comps.get(i).copied().unwrap_or(0.0);
                }
            }
        }
        Val::Vec2(v) => {
            for (i, &idx) in indices.iter().enumerate() {
                if idx < 2 {
                    v[idx] = val_comps.get(i).copied().unwrap_or(0.0);
                }
            }
        }
        _ => {}
    }
}

fn build_vec2(args: &[Val]) -> Val {
    match args {
        [Val::Vec2(v)] => Val::Vec2(*v),
        [a] => {
            let f = a.as_float();
            Val::Vec2([f, f])
        }
        [a, b] => Val::Vec2([a.as_float(), b.as_float()]),
        _ => Val::Vec2([0.0; 2]),
    }
}
fn build_vec3(args: &[Val]) -> Val {
    match args {
        [Val::Vec3(v)] => Val::Vec3(*v),
        [Val::Vec2(v), c] => Val::Vec3([v[0], v[1], c.as_float()]),
        [a, Val::Vec2(v)] => Val::Vec3([a.as_float(), v[0], v[1]]),
        [a] => {
            let f = a.as_float();
            Val::Vec3([f, f, f])
        }
        [a, b, c] => Val::Vec3([a.as_float(), b.as_float(), c.as_float()]),
        _ => Val::Vec3([0.0; 3]),
    }
}
fn build_vec4(args: &[Val]) -> Val {
    match args {
        [Val::Vec4(v)] => Val::Vec4(*v),
        [Val::Vec3(v), w] => Val::Vec4([v[0], v[1], v[2], w.as_float()]),
        [Val::Vec2(v), z, w] => Val::Vec4([v[0], v[1], z.as_float(), w.as_float()]),
        [Val::Vec2(a), Val::Vec2(b)] => Val::Vec4([a[0], a[1], b[0], b[1]]),
        [a] => {
            let f = a.as_float();
            Val::Vec4([f, f, f, f])
        }
        [a, b, c, d] => Val::Vec4([a.as_float(), b.as_float(), c.as_float(), d.as_float()]),
        _ => Val::Vec4([0.0; 4]),
    }
}
fn build_mat2(args: &[Val]) -> Val {
    if args.len() == 1 {
        let f = args[0].as_float();
        return Val::Mat2([[f, 0.0], [0.0, f]]);
    }
    let fs: Vec<f32> = args
        .iter()
        .flat_map(|a| match a {
            Val::Vec2(v) => v.to_vec().clone(),
            Val::Float(f) => vec![*f],
            other => vec![other.as_float()],
        })
        .collect();
    Val::Mat2([
        [
            fs.get(0).copied().unwrap_or(1.0),
            fs.get(1).copied().unwrap_or(0.0),
        ],
        [
            fs.get(2).copied().unwrap_or(0.0),
            fs.get(3).copied().unwrap_or(1.0),
        ],
    ])
}
fn build_mat3(args: &[Val]) -> Val {
    if args.len() == 1 {
        let f = args[0].as_float();
        return Val::Mat3([[f, 0.0, 0.0], [0.0, f, 0.0], [0.0, 0.0, f]]);
    }
    let fs: Vec<f32> = args
        .iter()
        .flat_map(|a| match a {
            Val::Vec3(v) => v.to_vec().clone(),
            Val::Float(f) => vec![*f],
            other => vec![other.as_float()],
        })
        .collect();
    let g = |i: usize| {
        fs.get(i)
            .copied()
            .unwrap_or(if i % 4 == 0 { 1.0 } else { 0.0 })
    };
    Val::Mat3([[g(0), g(1), g(2)], [g(3), g(4), g(5)], [g(6), g(7), g(8)]])
}
fn build_mat4(args: &[Val]) -> Val {
    if args.len() == 1 {
        let f = args[0].as_float();
        let mut m = [[0.0f32; 4]; 4];
        for i in 0..4 {
            m[i][i] = f;
        }
        return Val::Mat4(m);
    }
    let fs: Vec<f32> = args
        .iter()
        .flat_map(|a| match a {
            Val::Vec4(v) => v.to_vec().clone(),
            Val::Float(f) => vec![*f],
            other => vec![other.as_float()],
        })
        .collect();
    let g = |i: usize| {
        fs.get(i)
            .copied()
            .unwrap_or(if i % 5 == 0 { 1.0 } else { 0.0 })
    };
    Val::Mat4([
        [g(0), g(1), g(2), g(3)],
        [g(4), g(5), g(6), g(7)],
        [g(8), g(9), g(10), g(11)],
        [g(12), g(13), g(14), g(15)],
    ])
}

// ─── GlslVertex and GlslVarying ───────────────────────────────────────────────

/// A vertex represented as up to 16 generic float-vec4 attribute slots.
/// Slot 0 = position, slots 1..15 = user attributes in binding order.
#[derive(Clone, Copy, Debug)]
pub struct GlslVertex {
    pub attribs: [[f32; 4]; 16],
    /// Value injected as `gl_VertexID` in the vertex shader (GAP-009).
    pub vertex_id: i32,
    /// Value injected as `gl_InstanceID` in the vertex shader (GAP-009).
    pub instance_id: i32,
}

impl Default for GlslVertex {
    fn default() -> Self {
        Self {
            attribs: [[0.0; 4]; 16],
            vertex_id: 0,
            instance_id: 0,
        }
    }
}

/// Up to 16 vec4 varyings passed from vertex to fragment stage.
/// `front_facing` carries the winding sign for `gl_FrontFacing` (GAP-010).
/// `dvdx`/`dvdy` are screen-space partial derivatives of each slot, computed
/// by the rasterizer for `dFdx` / `dFdy` support (GAP-007).
#[derive(Clone, Copy, Debug)]
pub struct GlslVarying {
    pub slots: [[f32; 4]; 16],
    /// Packed `gl_FrontFacing`: positive area → true (front face in CCW convention).
    pub front_facing: bool,
    /// Screen-space X derivative of each varying slot (d(slot)/dx). Set by
    /// `finalize_pixel_derivatives`; zero until that call.
    pub dvdx: [[f32; 4]; 16],
    /// Screen-space Y derivative of each varying slot (d(slot)/dy). Set by
    /// `finalize_pixel_derivatives`; zero until that call.
    pub dvdy: [[f32; 4]; 16],
}

// Per-triangle derivative constants, stored in a global so that
// `init_triangle_derivatives` (called once per triangle) can pass them to
// `finalize_pixel_derivatives` (called once per pixel).
// SAFETY: single-threaded software renderer; no real concurrency.
#[derive(Clone, Copy)]
struct TriangleDeriv {
    dn_dx: [[f32; 4]; 16], // d(slot * inv_w) / dx — constant per triangle
    dn_dy: [[f32; 4]; 16], // d(slot * inv_w) / dy — constant per triangle
    d_inv_w_dx: f32,
    d_inv_w_dy: f32,
}

impl TriangleDeriv {
    const fn zero() -> Self {
        Self {
            dn_dx: [[0.0; 4]; 16],
            dn_dy: [[0.0; 4]; 16],
            d_inv_w_dx: 0.0,
            d_inv_w_dy: 0.0,
        }
    }
}

// SAFETY: graphos-gl is a single-threaded software renderer.
// Wrapping in UnsafeCell + SyncWrapper lets us store it as a static.
struct SyncUnsafeCell<T>(core::cell::UnsafeCell<T>);
// SAFETY: single-threaded; no races.
unsafe impl<T> Sync for SyncUnsafeCell<T> {}
impl<T: Copy> SyncUnsafeCell<T> {
    const fn new(val: T) -> Self {
        Self(core::cell::UnsafeCell::new(val))
    }
    #[inline]
    fn get(&self) -> T {
        unsafe { *self.0.get() }
    }
    #[inline]
    fn set(&self, val: T) {
        unsafe {
            *self.0.get() = val;
        }
    }
}

static TRIANGLE_DERIV: SyncUnsafeCell<TriangleDeriv> = SyncUnsafeCell::new(TriangleDeriv::zero());

impl Varying for GlslVarying {
    fn weighted_sum(a: Self, wa: f32, b: Self, wb: f32, c: Self, wc: f32) -> Self {
        let mut out = Self {
            slots: [[0.0; 4]; 16],
            front_facing: a.front_facing,
            dvdx: [[0.0; 4]; 16],
            dvdy: [[0.0; 4]; 16],
        };
        for i in 0..16 {
            for j in 0..4 {
                out.slots[i][j] = a.slots[i][j] * wa + b.slots[i][j] * wb + c.slots[i][j] * wc;
            }
        }
        out
    }
    fn scale(self, s: f32) -> Self {
        let mut out = self;
        for i in 0..16 {
            for j in 0..4 {
                out.slots[i][j] *= s;
            }
        }
        out
    }
    #[inline]
    fn set_front_facing(&mut self, front: bool) {
        self.front_facing = front;
    }

    fn init_triangle_derivatives(
        vy0: &Self,
        vy1: &Self,
        vy2: &Self,
        dw0dx: f32,
        dw1dx: f32,
        dw2dx: f32,
        dw0dy: f32,
        dw1dy: f32,
        dw2dy: f32,
        d_inv_w_dx: f32,
        d_inv_w_dy: f32,
    ) {
        let mut td = TriangleDeriv {
            d_inv_w_dx,
            d_inv_w_dy,
            ..TriangleDeriv::zero()
        };
        for s in 0..16 {
            for c in 0..4 {
                td.dn_dx[s][c] =
                    dw0dx * vy0.slots[s][c] + dw1dx * vy1.slots[s][c] + dw2dx * vy2.slots[s][c];
                td.dn_dy[s][c] =
                    dw0dy * vy0.slots[s][c] + dw1dy * vy1.slots[s][c] + dw2dy * vy2.slots[s][c];
            }
        }
        TRIANGLE_DERIV.set(td);
    }

    fn finalize_pixel_derivatives(&mut self, one_over_w: f32) {
        if one_over_w.abs() < 1e-10 {
            return;
        }
        let td = TRIANGLE_DERIV.get();
        for s in 0..16 {
            for c in 0..4 {
                // Quotient rule: d(N/D)/dx = (dN/dx - (N/D)*dD/dx) / D
                // N/D = self.slots[s][c], D = one_over_w
                self.dvdx[s][c] = (td.dn_dx[s][c] - self.slots[s][c] * td.d_inv_w_dx) / one_over_w;
                self.dvdy[s][c] = (td.dn_dy[s][c] - self.slots[s][c] * td.d_inv_w_dy) / one_over_w;
            }
        }
    }
}

// ─── GlslShader ───────────────────────────────────────────────────────────────

/// A compiled+parsed GLSL program that implements the [`Shader`] trait.
///
/// Created via [`GlslShader::compile`].  The shader holds cloned copies of
/// the parsed ASTs and the uniform+texture snapshots so it can be used as an
/// immutable value across draw calls.
/// Parse UBO blocks from a GLSL shader source string (GAP-011).
/// Returns the list of `UboBlock` declarations found.
pub fn parse_ubo_blocks(src: &str) -> Vec<UboBlock> {
    Parser::new(src).parse_shader().ubo_blocks
}

pub struct GlslShader {
    vert_funcs: BTreeMap<String, FuncDef>,
    frag_funcs: BTreeMap<String, FuncDef>,
    vert_attr_names: Vec<String>, // names of `attribute` inputs in vert, in declaration order
    vert_out_names: Vec<String>,  // names of `out` varyings declared in vert
    frag_in_names: Vec<String>,   // names of `in` varyings declared in frag
    frag_out_bindings: Vec<(u8, String)>, // attachment index -> fragment output variable name
    uniforms: BTreeMap<String, Val>,
    textures: Vec<Option<TextureOwned>>,
    /// User-defined struct type definitions shared between vertex and fragment shaders.
    struct_defs: BTreeMap<String, Vec<(String, String)>>,
}

/// Texture format tag — controls how sampled pixel bits are interpreted.
#[derive(Clone, PartialEq, Debug)]
pub enum TextureFormat {
    /// Standard normalized BGRA32 → `[0.0, 1.0]^4` floats.
    Float,
    /// Raw packed-integer BGRA32 → `ivec4` components.
    Int,
    /// Raw packed-integer BGRA32 → `uvec4` components.
    UInt,
    /// Depth-component texture: pixels are `f32::to_bits(depth)`.
    /// `texture()` returns `vec4(depth, depth, depth, 1.0)`.
    Depth,
}

/// Owned copy of texture data for use inside GlslShader.
#[derive(Clone)]
pub struct TextureOwned {
    pub pixels: Vec<u32>,
    pub width: u32,
    pub height: u32,
    pub wrap_s: crate::gl::WrapMode,
    pub wrap_t: crate::gl::WrapMode,
    pub min_filter: crate::gl::FilterMode,
    pub mag_filter: crate::gl::FilterMode,
    /// Pixel format — controls return type of `texture()` calls.
    pub format: TextureFormat,
    /// Additional mip levels (index 0 = level 1, etc.): `(width, height, pixels)`.
    pub mip_levels: Vec<(u32, u32, Vec<u32>)>,
    /// Cube map faces `[+X, -X, +Y, -Y, +Z, -Z]`; each face has the same
    /// dimensions as the base level. Empty vecs → face not uploaded.
    pub cube_faces: [Vec<u32>; 6],
    /// 2D array layers (index 0 = layer 0, etc.). Each layer has `width * height` pixels.
    /// When non-empty, `texture(sampler, vec3(s,t,layer))` selects the layer by index.
    pub layers: Vec<Vec<u32>>,
}

impl GlslShader {
    /// Parse and prepare a GLSL shader pair.
    ///
    /// * `vert_src` — vertex shader GLSL source (UTF-8).
    /// * `frag_src` — fragment shader GLSL source (UTF-8).
    /// * `uniform_vals` — uniform name → value map built from [`Context::uniforms`].
    /// * `textures` — per-unit texture snapshots (slot index = `sampler2D` binding).
    pub fn compile(
        vert_src: &str,
        frag_src: &str,
        uniform_vals: BTreeMap<String, Val>,
        textures: Vec<Option<TextureOwned>>,
        frag_out_bindings: Vec<(u8, String)>,
    ) -> Self {
        let vert_ast = Parser::new(vert_src).parse_shader();
        let frag_ast = Parser::new(frag_src).parse_shader();

        let vert_funcs: BTreeMap<String, FuncDef> = vert_ast
            .functions
            .into_iter()
            .map(|f| (f.name.clone(), f))
            .collect();
        let frag_funcs: BTreeMap<String, FuncDef> = frag_ast
            .functions
            .into_iter()
            .map(|f| (f.name.clone(), f))
            .collect();

        let vert_attr_names = vert_ast.attributes.into_iter().map(|(_, n)| n).collect();
        let mut vert_out_names: Vec<String> =
            vert_ast.outputs.into_iter().map(|(_, n)| n).collect();
        for (_, name) in vert_ast.varyings_out {
            if !vert_out_names.iter().any(|existing| existing == &name) {
                vert_out_names.push(name);
            }
        }
        let mut frag_in_names: Vec<String> =
            frag_ast.attributes.into_iter().map(|(_, n)| n).collect();
        for (_, name) in frag_ast.varyings_in {
            if !frag_in_names.iter().any(|existing| existing == &name) {
                frag_in_names.push(name);
            }
        }

        // Merge struct defs from both shaders.
        let mut struct_defs = vert_ast.struct_defs;
        for (k, v) in frag_ast.struct_defs {
            struct_defs.entry(k).or_insert(v);
        }

        Self {
            vert_funcs,
            frag_funcs,
            vert_attr_names,
            vert_out_names,
            frag_in_names,
            frag_out_bindings,
            uniforms: uniform_vals,
            textures,
            struct_defs,
        }
    }

    fn make_textures(&self) -> Vec<Option<Texture<'_>>> {
        self.textures
            .iter()
            .map(|t| {
                t.as_ref().map(|to| Texture {
                    pixels: &to.pixels,
                    width: to.width,
                    height: to.height,
                    wrap_s: to.wrap_s,
                    wrap_t: to.wrap_t,
                    min_filter: to.min_filter,
                    mag_filter: to.mag_filter,
                    border_color: [0.0; 4],
                })
            })
            .collect()
    }

    /// Return a `Texture` for a specific mip level (0 = base).
    fn make_texture_lod(
        textures: &[Option<TextureOwned>],
        slot: usize,
        lod: i32,
    ) -> Option<(Texture<'_>, crate::gl::WrapMode, crate::gl::WrapMode)> {
        let owned = textures.get(slot)?.as_ref()?;
        let lod = lod.max(0) as usize;
        if lod == 0 {
            Some((
                Texture {
                    pixels: &owned.pixels,
                    width: owned.width,
                    height: owned.height,
                    wrap_s: owned.wrap_s,
                    wrap_t: owned.wrap_t,
                    min_filter: owned.min_filter,
                    mag_filter: owned.mag_filter,
                    border_color: [0.0; 4],
                },
                owned.wrap_s,
                owned.wrap_t,
            ))
        } else {
            let idx = lod - 1;
            let (mw, mh, mpx) = owned.mip_levels.get(idx)?;
            Some((
                Texture {
                    pixels: mpx,
                    width: *mw,
                    height: *mh,
                    wrap_s: owned.wrap_s,
                    wrap_t: owned.wrap_t,
                    min_filter: owned.min_filter,
                    mag_filter: owned.mag_filter,
                    border_color: [0.0; 4],
                },
                owned.wrap_s,
                owned.wrap_t,
            ))
        }
    }
}

impl Shader for GlslShader {
    type Vertex = GlslVertex;
    type Varying = GlslVarying;

    fn vertex(&self, v: &GlslVertex) -> (Vec4, GlslVarying) {
        let tex_slots = self.make_textures();
        let mut env = Env::new_with_structs(
            &self.uniforms,
            &tex_slots,
            &self.textures,
            &self.vert_funcs,
            &self.struct_defs,
        );

        // Inject vertex built-ins (GAP-009).
        env.declare("gl_VertexID", Val::Int(v.vertex_id));
        env.declare("gl_InstanceID", Val::Int(v.instance_id));

        // Load vertex attributes into environment — declare by actual name (from parsed AST)
        // so GLSL code like `a_pos` can reference attrib slot 0 directly.
        for (i, attrib) in v.attribs.iter().enumerate() {
            env.declare(&alloc::format!("_attr{}", i), Val::Vec4(*attrib));
            if let Some(name) = self.vert_attr_names.get(i) {
                env.declare(name, Val::Vec4(*attrib));
            }
        }

        // Execute main()
        if let Some(main) = self.vert_funcs.get("main") {
            let _ = env.exec_stmts(&main.body.clone());
        }

        // Read gl_Position
        let pos = match env.get("gl_Position") {
            Val::Vec4(p) => Vec4::new(p[0], p[1], p[2], p[3]),
            other => Vec4::new(other.as_float(), 0.0, 0.0, 1.0),
        };

        // Read varyings → slots
        let mut varying = GlslVarying {
            slots: [[0.0; 4]; 16],
            front_facing: true,
            dvdx: [[0.0; 4]; 16],
            dvdy: [[0.0; 4]; 16],
        };
        for (i, name) in self.vert_out_names.iter().enumerate().take(16) {
            varying.slots[i] = env.get(name).as_vec4();
        }

        (pos, varying)
    }

    fn fragment(&self, v: &GlslVarying) -> Option<Vec4> {
        self.run_fragment_outputs(v)
            .and_then(|outputs| outputs.colors.into_iter().flatten().next())
    }

    fn fragment_depth(&self, v: &GlslVarying) -> Option<f32> {
        self.run_fragment_outputs(v)
            .and_then(|outputs| outputs.depth)
    }

    fn fragment_outputs(&self, v: &GlslVarying) -> Option<FragmentOutputs> {
        self.run_fragment_outputs(v)
    }
}

impl GlslShader {
    /// Run the fragment shader, returning MRT color outputs and optional gl_FragDepth.
    fn run_fragment_outputs(&self, v: &GlslVarying) -> Option<FragmentOutputs> {
        let tex_slots = self.make_textures();
        let mut env = Env::new_with_structs(
            &self.uniforms,
            &tex_slots,
            &self.textures,
            &self.frag_funcs,
            &self.struct_defs,
        );

        // Inject fragment built-ins.
        env.declare("gl_FrontFacing", Val::Bool(v.front_facing)); // GAP-010

        // Load interpolated varyings into environment.
        for (i, name) in self.frag_in_names.iter().enumerate().take(16) {
            env.declare(name, Val::Vec4(v.slots[i]));
            // GAP-007: inject per-varying derivative variables for dFdx / dFdy.
            env.declare(&alloc::format!("_dvdx_{}", name), Val::Vec4(v.dvdx[i]));
            env.declare(&alloc::format!("_dvdy_{}", name), Val::Vec4(v.dvdy[i]));
        }
        for (i, slot) in v.slots.iter().enumerate() {
            env.declare(&alloc::format!("_vary{}", i), Val::Vec4(*slot));
        }

        let discard = if let Some(main) = self.frag_funcs.get("main") {
            let body = main.body.clone();
            matches!(env.exec_stmts(&body), Err(Signal::Discard))
        } else {
            false
        };
        if discard {
            return None;
        }

        let mut outputs = FragmentOutputs {
            colors: [None; 8],
            depth: match env.try_get("gl_FragDepth") {
                Some(Val::Float(d)) => Some(*d),
                _ => None,
            },
        };

        if self.frag_out_bindings.is_empty() {
            let [r, g, b, a] = env.get("gl_FragColor").as_vec4();
            outputs.colors[0] = Some(Vec4::new(r, g, b, a));
        } else {
            for (location, name) in &self.frag_out_bindings {
                let attachment = *location as usize;
                if attachment >= outputs.colors.len() {
                    continue;
                }
                let [r, g, b, a] = env.get(name).as_vec4();
                outputs.colors[attachment] = Some(Vec4::new(r, g, b, a));
            }
        }

        Some(outputs)
    }
}

// ─── Context integration helpers ─────────────────────────────────────────────

/// Convert a [`UniformValue`] to an interpreter [`Val`].
pub fn uniform_to_val(u: &UniformValue) -> Val {
    match u {
        UniformValue::Float(v) => Val::Float(*v),
        UniformValue::Int(v) => Val::Int(*v),
        UniformValue::UInt(v) => Val::UInt(*v),
        UniformValue::Vec2(v) => Val::Vec2(*v),
        UniformValue::Vec3(v) => Val::Vec3(*v),
        UniformValue::Vec4(v) => Val::Vec4(*v),
        UniformValue::IVec2(v) => Val::IVec2(*v),
        UniformValue::IVec3(v) => Val::IVec3(*v),
        UniformValue::IVec4(v) => Val::IVec4(*v),
        UniformValue::UVec2(v) => Val::UVec2(*v),
        UniformValue::UVec3(v) => Val::UVec3(*v),
        UniformValue::UVec4(v) => Val::UVec4(*v),
        UniformValue::Mat2(v) => Val::Mat2([[v[0], v[1]], [v[2], v[3]]]),
        UniformValue::Mat3(v) => {
            Val::Mat3([[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
        }
        UniformValue::Mat4(v) => Val::Mat4([
            [v[0], v[1], v[2], v[3]],
            [v[4], v[5], v[6], v[7]],
            [v[8], v[9], v[10], v[11]],
            [v[12], v[13], v[14], v[15]],
        ]),
        UniformValue::FloatArray(v) => Val::Array(v.iter().map(|&f| Val::Float(f)).collect()),
        UniformValue::IntArray(v) => Val::Array(v.iter().map(|&i| Val::Int(i)).collect()),
        UniformValue::UIntArray(v) => Val::Array(v.iter().map(|&u| Val::UInt(u)).collect()),
        _ => Val::Float(0.0),
    }
}

/// Build the texture slot array for a program from context texture state.
pub fn build_texture_slots(
    tex_alloc: &[bool],
    texture_images: &[Vec<crate::gl::TextureImage>],
    textures: &[crate::gl::TextureObject],
    active_textures: &[u32],
    num_slots: usize,
) -> Vec<Option<TextureOwned>> {
    (0..num_slots)
        .map(|slot| {
            let name = *active_textures.get(slot).unwrap_or(&0);
            if name == 0 || (name as usize) > tex_alloc.len() || !tex_alloc[name as usize - 1] {
                return None;
            }
            let idx = name as usize - 1;
            let imgs = &texture_images[idx];
            if imgs.is_empty() {
                return None;
            }
            let img = &imgs[0];
            let obj = &textures[idx];
            let mip_levels: Vec<(u32, u32, Vec<u32>)> = imgs[1..]
                .iter()
                .map(|m| (m.width, m.height, m.pixels.clone()))
                .collect();
            Some(TextureOwned {
                pixels: img.pixels.clone(),
                width: img.width,
                height: img.height,
                wrap_s: obj.wrap_s,
                wrap_t: obj.wrap_t,
                min_filter: obj.min_filter,
                mag_filter: obj.mag_filter,
                format: if obj.is_depth {
                    TextureFormat::Depth
                } else {
                    TextureFormat::Float
                },
                mip_levels,
                cube_faces: [
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                ],
                layers: obj.array_layers.clone(),
            })
        })
        .collect()
}

// ─── Geometry shader ──────────────────────────────────────────────────────────

/// Input/output primitive types for geometry shaders.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GeomPrimitive {
    Points,
    Lines,
    LineStrip,
    LineLoop,
    Triangles,
    TriangleStrip,
    TriangleFan,
    LinesAdjacency,
    TrianglesAdjacency,
}

impl GeomPrimitive {
    fn vertices_per_prim(self) -> usize {
        match self {
            GeomPrimitive::Points => 1,
            GeomPrimitive::Lines => 2,
            GeomPrimitive::LinesAdjacency => 4,
            GeomPrimitive::Triangles => 3,
            GeomPrimitive::TrianglesAdjacency => 6,
            _ => 3, // strips — caller handles windowing
        }
    }
}

/// A compiled geometry shader.
///
/// Created via [`GlslGeometryShader::compile`].  Use
/// [`GlslGeometryShader::process_primitive`] to run the geometry stage on
/// one input primitive and collect the emitted vertices.
pub struct GlslGeometryShader {
    funcs: BTreeMap<String, FuncDef>,
    uniforms: BTreeMap<String, Val>,
    textures: Vec<Option<TextureOwned>>,
    pub in_type: GeomPrimitive,
    pub out_type: GeomPrimitive,
    pub max_vertices: u32,
}

impl GlslGeometryShader {
    /// Compile a geometry shader.
    ///
    /// `geom_src` — GLSL ES 3.2 geometry shader source.
    pub fn compile(
        geom_src: &str,
        uniform_vals: BTreeMap<String, Val>,
        textures: Vec<Option<TextureOwned>>,
    ) -> Self {
        let ast = Parser::new(geom_src).parse_shader();
        let funcs: BTreeMap<String, FuncDef> = ast
            .functions
            .into_iter()
            .map(|f| (f.name.clone(), f))
            .collect();

        // Parse layout qualifiers from source text (simple scan)
        let in_type = parse_geom_layout_in(geom_src);
        let (out_type, max_vertices) = parse_geom_layout_out(geom_src);

        Self {
            funcs,
            uniforms: uniform_vals,
            textures,
            in_type,
            out_type,
            max_vertices,
        }
    }

    /// Run the geometry shader on one input primitive.
    ///
    /// `inputs` — slice of `(clip_position, varying)` from the vertex stage, one per input vertex.
    ///
    /// Returns a list of emitted vertex groups.  Each inner `Vec` is one strip/fan run
    /// terminated by `EndPrimitive()`.  Each element is `(clip_position, varying)`.
    pub fn process_primitive(
        &self,
        inputs: &[(Vec4, GlslVarying)],
    ) -> Vec<Vec<(Vec4, GlslVarying)>> {
        let tex_slots: Vec<Option<Texture<'_>>> = self
            .textures
            .iter()
            .map(|t| {
                t.as_ref().map(|to| Texture {
                    pixels: &to.pixels,
                    width: to.width,
                    height: to.height,
                    wrap_s: to.wrap_s,
                    wrap_t: to.wrap_t,
                    min_filter: to.min_filter,
                    mag_filter: to.mag_filter,
                    border_color: [0.0; 4],
                })
            })
            .collect();

        let mut env = Env::new(&self.uniforms, &tex_slots, &self.textures, &self.funcs);

        // Populate gl_in[] array
        let gl_in_arr: Vec<Val> = inputs
            .iter()
            .map(|(pos, vary)| Val::Vec4([pos.x, pos.y, pos.z, pos.w]))
            .collect();
        env.declare("gl_in_positions", Val::Array(gl_in_arr));

        // Per-input varying slots accessible as gl_in[i].someField
        for (i, (_, vary)) in inputs.iter().enumerate() {
            env.declare(&alloc::format!("_geom_in_{}", i), Val::Vec4(vary.slots[0]));
        }

        // Geometry output state
        let mut all_primitives: Vec<Vec<(Vec4, GlslVarying)>> = Vec::new();
        let mut current_strip: Vec<(Vec4, GlslVarying)> = Vec::new();

        // We need to intercept EmitVertex() and EndPrimitive() calls.
        // These are signalled as special signals from exec_stmts.
        // We set reserved env variables that get cleared in a loop.
        env.declare("_geom_emit_pending", Val::Bool(false));
        env.declare("_geom_end_pending", Val::Bool(false));
        env.declare("gl_Position", Val::Vec4([0.0; 4]));

        if let Some(main) = self.funcs.get("main") {
            let body = main.body.clone();
            let _ = env.exec_stmts_geom(
                &body,
                &mut current_strip,
                &mut all_primitives,
                self.max_vertices as usize,
            );
        }

        if !current_strip.is_empty() {
            all_primitives.push(current_strip);
        }
        all_primitives
    }
}

/// Parse the `layout(triangles) in;` declaration from geometry shader source.
fn parse_geom_layout_in(src: &str) -> GeomPrimitive {
    if src.contains("triangles_adjacency") {
        return GeomPrimitive::TrianglesAdjacency;
    }
    if src.contains("lines_adjacency") {
        return GeomPrimitive::LinesAdjacency;
    }
    if src.contains("layout(triangles)") {
        return GeomPrimitive::Triangles;
    }
    if src.contains("layout(lines)") {
        return GeomPrimitive::Lines;
    }
    if src.contains("layout(points)") {
        return GeomPrimitive::Points;
    }
    GeomPrimitive::Triangles // default
}

/// Parse the `layout(triangle_strip, max_vertices = N) out;` declaration.
fn parse_geom_layout_out(src: &str) -> (GeomPrimitive, u32) {
    let prim = if src.contains("triangle_strip") {
        GeomPrimitive::TriangleStrip
    } else if src.contains("line_strip") {
        GeomPrimitive::LineStrip
    } else {
        GeomPrimitive::Points
    };
    // Extract max_vertices = N
    let max_v = src
        .find("max_vertices")
        .and_then(|pos| {
            let after = &src[pos..];
            let eq = after.find('=')?;
            let num_start = after[eq + 1..].trim_start();
            let num_str: String = num_start
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            num_str.parse::<u32>().ok()
        })
        .unwrap_or(64);
    (prim, max_v)
}

impl<'a> Env<'a> {
    /// Execute statements in a geometry shader context, intercepting EmitVertex/EndPrimitive.
    fn exec_stmts_geom(
        &mut self,
        stmts: &[Stmt],
        current_strip: &mut Vec<(Vec4, GlslVarying)>,
        all_primitives: &mut Vec<Vec<(Vec4, GlslVarying)>>,
        max_vertices: usize,
    ) -> Result<(), Signal> {
        for s in stmts {
            // Check for EmitVertex / EndPrimitive as Call statements
            if let Stmt::Expr(Expr::Call { name: fname, .. }) = s {
                if fname == "EmitVertex" {
                    if current_strip.len() < max_vertices {
                        let pos_val = self.get("gl_Position");
                        let [px, py, pz, pw] = pos_val.as_vec4();
                        let pos = Vec4::new(px, py, pz, pw);
                        // Collect varyings from _geom_out_ variables
                        let mut vary = GlslVarying {
                            slots: [[0.0; 4]; 16],
                            front_facing: true,
                            dvdx: [[0.0; 4]; 16],
                            dvdy: [[0.0; 4]; 16],
                        };
                        for i in 0..16 {
                            vary.slots[i] = self.get(&alloc::format!("_geom_out_{}", i)).as_vec4();
                        }
                        current_strip.push((pos, vary));
                    }
                    continue;
                }
                if fname == "EndPrimitive" {
                    if !current_strip.is_empty() {
                        all_primitives.push(core::mem::take(current_strip));
                    }
                    continue;
                }
            }
            self.exec_stmt(s)?;
        }
        Ok(())
    }
}

// ─── Compute shader ───────────────────────────────────────────────────────────

/// A compiled compute shader.
///
/// Created via [`GlslComputeShader::compile`].  Use
/// [`GlslComputeShader::dispatch`] to execute all workgroups.
pub struct GlslComputeShader {
    funcs: BTreeMap<String, FuncDef>,
    uniforms: BTreeMap<String, Val>,
    textures: Vec<Option<TextureOwned>>,
    /// Declared local work-group size `(x, y, z)`.
    pub local_size: (u32, u32, u32),
}

impl GlslComputeShader {
    /// Compile a compute shader.
    ///
    /// `comp_src` — GLSL ES 3.1 compute shader source.
    pub fn compile(
        comp_src: &str,
        uniform_vals: BTreeMap<String, Val>,
        textures: Vec<Option<TextureOwned>>,
    ) -> Self {
        let ast = Parser::new(comp_src).parse_shader();
        let funcs: BTreeMap<String, FuncDef> = ast
            .functions
            .into_iter()
            .map(|f| (f.name.clone(), f))
            .collect();
        let local_size = parse_local_size(comp_src);
        Self {
            funcs,
            uniforms: uniform_vals,
            textures,
            local_size,
        }
    }

    /// Dispatch the compute shader with `(num_groups_x, num_groups_y, num_groups_z)` workgroups.
    ///
    /// Runs `main()` once per invocation.  Shared variables are held in a
    /// `BTreeMap` per workgroup, cleared between workgroups.
    ///
    /// Returns the final uniform/global variable state as a flat map (useful for testing).
    pub fn dispatch(
        &self,
        num_groups_x: u32,
        num_groups_y: u32,
        num_groups_z: u32,
    ) -> BTreeMap<String, Val> {
        let tex_slots: Vec<Option<Texture<'_>>> = self
            .textures
            .iter()
            .map(|t| {
                t.as_ref().map(|to| Texture {
                    pixels: &to.pixels,
                    width: to.width,
                    height: to.height,
                    wrap_s: to.wrap_s,
                    wrap_t: to.wrap_t,
                    min_filter: to.min_filter,
                    mag_filter: to.mag_filter,
                    border_color: [0.0; 4],
                })
            })
            .collect();

        let (lx, ly, lz) = self.local_size;
        let mut final_state: BTreeMap<String, Val> = BTreeMap::new();

        for gz in 0..num_groups_z {
            for gy in 0..num_groups_y {
                for gx in 0..num_groups_x {
                    // Shared memory — reset per workgroup
                    let mut shared: BTreeMap<String, Val> = BTreeMap::new();

                    for iz in 0..lz {
                        for iy in 0..ly {
                            for ix in 0..lx {
                                let mut env = Env::new(
                                    &self.uniforms,
                                    &tex_slots,
                                    &self.textures,
                                    &self.funcs,
                                );

                                // Inject built-in compute variables
                                env.declare("gl_WorkGroupSize", Val::UVec3([lx, ly, lz]));
                                env.declare(
                                    "gl_NumWorkGroups",
                                    Val::UVec3([num_groups_x, num_groups_y, num_groups_z]),
                                );
                                env.declare("gl_WorkGroupID", Val::UVec3([gx, gy, gz]));
                                env.declare("gl_LocalInvocationID", Val::UVec3([ix, iy, iz]));
                                env.declare(
                                    "gl_GlobalInvocationID",
                                    Val::UVec3([gx * lx + ix, gy * ly + iy, gz * lz + iz]),
                                );
                                env.declare(
                                    "gl_LocalInvocationIndex",
                                    Val::UInt(iz * lx * ly + iy * lx + ix),
                                );

                                // Inject shared variables from workgroup shared state
                                for (k, v) in &shared {
                                    env.declare(k, v.clone());
                                }

                                if let Some(main) = self.funcs.get("main") {
                                    let body = main.body.clone();
                                    let _ = env.exec_stmts(&body);
                                }

                                // Collect any updates to shared variables back into shared map
                                // (We do this by checking the top scope for "shared_" prefixed vars)
                                for (k, v) in
                                    env.scopes.first().map(|s| s.iter()).into_iter().flatten()
                                {
                                    if k.starts_with("shared_") {
                                        shared.insert(k.clone(), v.clone());
                                    }
                                }

                                // On last invocation, capture state for caller
                                if ix == lx - 1
                                    && iy == ly - 1
                                    && iz == lz - 1
                                    && gx == num_groups_x - 1
                                    && gy == num_groups_y - 1
                                    && gz == num_groups_z - 1
                                {
                                    for scope in &env.scopes {
                                        for (k, v) in scope {
                                            final_state.insert(k.clone(), v.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        final_state
    }
}

/// Parse `layout(local_size_x = N, local_size_y = M, local_size_z = P) in;`
fn parse_local_size(src: &str) -> (u32, u32, u32) {
    let extract = |key: &str| -> u32 {
        src.find(key)
            .and_then(|pos| {
                let after = &src[pos + key.len()..];
                let after = after.trim_start();
                if after.starts_with('=') {
                    let num: String = after[1..]
                        .trim_start()
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    num.parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(1)
    };
    (
        extract("local_size_x"),
        extract("local_size_y"),
        extract("local_size_z"),
    )
}

pub use self::internal::GlslShaderRef;
mod internal {
    // Re-export for use in gl.rs without pub-use of private TextureOwned
    pub use super::GlslShader as GlslShaderRef;
}
