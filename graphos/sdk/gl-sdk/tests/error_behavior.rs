// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
/// Epic 1 — Error behavior tests
///
/// Verifies that the GL context generates the correct GL errors for invalid
/// operations and that `glGetError` returns and clears errors per the ES 3.0 spec.
use graphos_gl::{Context, GlError, ShaderKind};

// ── glGetError basic contract ─────────────────────────────────────────────────

#[test]
fn no_error_on_fresh_context() {
    let mut ctx = Context::new();
    assert_eq!(ctx.get_error(), None, "fresh context must have no error");
}

#[test]
fn get_error_clears_after_read() {
    let mut ctx = Context::new();
    // Trigger an error by using a non-existent program name.
    ctx.use_program(99999);
    let first = ctx.get_error();
    let second = ctx.get_error();
    assert!(first.is_some(), "invalid use_program must set an error");
    assert!(second.is_none(), "get_error must clear the error flag");
}

// ── GL_INVALID_OPERATION ─────────────────────────────────────────────────────

#[test]
fn link_program_with_uncompiled_shaders_fails() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);
    assert!(
        !ctx.program_link_status(prog),
        "program with uncompiled shaders must not link"
    );
    let log = ctx.program_info_log(prog).unwrap_or("");
    assert!(
        !log.is_empty(),
        "a non-empty info log must be set on link failure"
    );
}

#[test]
fn use_program_with_unlinked_program_sets_error() {
    let mut ctx = Context::new();
    let prog = ctx.create_program();
    // Never linked — using it must be GL_INVALID_OPERATION.
    ctx.use_program(prog);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

// ── Shader compilation ────────────────────────────────────────────────────────

#[test]
fn valid_vertex_shader_compiles_successfully() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    ctx.shader_source_str(
        vs,
        "#version 300 es\n\
         in vec4 a_pos;\n\
         void main() { gl_Position = a_pos; }\n",
    );
    ctx.compile_shader(vs);
    assert!(
        ctx.shader_compile_status(vs),
        "valid vertex shader must compile"
    );
}

// ── Object lifecycle ──────────────────────────────────────────────────────────

#[test]
fn create_and_delete_shader_round_trip() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    assert_ne!(vs, 0, "create_shader must return a non-zero name");
    ctx.delete_shaders(&[vs]);
    assert!(
        ctx.get_error().is_none(),
        "delete_shaders must not set an error"
    );
}

#[test]
fn create_and_delete_program_round_trip() {
    let mut ctx = Context::new();
    let prog = ctx.create_program();
    assert_ne!(prog, 0, "create_program must return a non-zero name");
    ctx.delete_programs(&[prog]);
    assert!(ctx.get_error().is_none());
}

#[test]
fn create_buffer_and_bind_round_trip() {
    let mut ctx = Context::new();
    let buf = ctx.gen_buffer();
    assert_ne!(buf, 0);
    ctx.bind_array_buffer(buf);
    assert!(ctx.get_error().is_none());
}
