// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
/// Epic 1 — API state tests
///
/// Verifies that the GL context correctly tracks and reports state for
/// the ES 3.0 areas addressed in the conformance roadmap.
use graphos_gl::{Context, ShaderKind};

// ── Extension reporting (GAP-002) ────────────────────────────────────────────

#[test]
fn only_implemented_extensions_advertised() {
    let ctx = Context::new();
    let exts = ctx.supported_extensions();
    assert!(
        exts.contains(&"GL_KHR_debug"),
        "GL_KHR_debug must be advertised"
    );
    assert!(
        !exts.contains(&"GL_KHR_robustness"),
        "KHR_robustness not implemented"
    );
    assert!(
        !exts.contains(&"GL_EXT_disjoint_timer_query"),
        "EXT_disjoint_timer_query not implemented"
    );
    assert!(
        !exts.contains(&"GL_EXT_color_buffer_float"),
        "EXT_color_buffer_float not implemented"
    );
}

#[test]
fn get_num_extensions_matches_list() {
    let ctx = Context::new();
    let n = ctx.get_num_extensions();
    let list = ctx.supported_extensions();
    assert_eq!(
        n as usize,
        list.len(),
        "get_num_extensions must match supported_extensions length"
    );
}

#[test]
fn get_stringi_extension_returns_correct_names() {
    let ctx = Context::new();
    let n = ctx.get_num_extensions();
    for i in 0..n {
        assert!(
            ctx.get_stringi_extension(i).is_some(),
            "index {i} must return Some"
        );
    }
    assert!(
        ctx.get_stringi_extension(n).is_none(),
        "out-of-range index must return None"
    );
}

// ── glGetActiveUniform type reporting (GAP-001) ──────────────────────────────

#[test]
fn get_active_uniforms_iv_returns_correct_type() {
    let mut ctx = Context::new();

    let vs = ctx.create_shader(ShaderKind::Vertex);
    ctx.shader_source_str(
        vs,
        "#version 300 es\n\
         uniform mat4 u_mvp;\n\
         uniform vec3 u_color;\n\
         in vec4 a_pos;\n\
         void main() { gl_Position = u_mvp * a_pos; }\n",
    );
    ctx.compile_shader(vs);

    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source_str(
        fs,
        "#version 300 es\n\
         precision mediump float;\n\
         uniform vec3 u_color;\n\
         out vec4 fragColor;\n\
         void main() { fragColor = vec4(u_color, 1.0); }\n",
    );
    ctx.compile_shader(fs);

    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);
    assert!(ctx.program_link_status(prog), "program must link");

    // Find uniform indices by name (sentinel: get_active_uniform returns None past the end).
    let mut mvp_index = None;
    let mut color_index = None;
    for i in 0u32.. {
        match ctx.get_active_uniform(prog, i) {
            Some((_, ref name)) if name == "u_mvp" => {
                mvp_index = Some(i);
            }
            Some((_, ref name)) if name == "u_color" => {
                color_index = Some(i);
            }
            None => break,
            _ => {}
        }
    }

    let mvp_i = mvp_index.expect("u_mvp uniform must be reported");
    let color_i = color_index.expect("u_color uniform must be reported");

    let mut out = [0i32; 1];
    ctx.get_active_uniforms_iv(prog, &[mvp_i], 0x8A3A, &mut out); // GL_UNIFORM_TYPE
    assert_eq!(
        out[0], 0x8B5C,
        "u_mvp must report GL_FLOAT_MAT4 (0x8B5C), got 0x{:04X}",
        out[0]
    );

    ctx.get_active_uniforms_iv(prog, &[color_i], 0x8A3A, &mut out);
    assert_eq!(
        out[0], 0x8B51,
        "u_color must report GL_FLOAT_VEC3 (0x8B51), got 0x{:04X}",
        out[0]
    );
}

// ── Program link — stage interface type matching (GAP-012) ───────────────────

#[test]
fn mismatched_varying_type_prevents_link() {
    let mut ctx = Context::new();

    let vs = ctx.create_shader(ShaderKind::Vertex);
    ctx.shader_source_str(
        vs,
        "#version 300 es\n\
         in vec4 a_pos;\n\
         out vec4 vColor;\n\
         void main() { gl_Position = a_pos; vColor = vec4(1.0); }\n",
    );
    ctx.compile_shader(vs);

    // Fragment reads vColor as vec2 — type mismatch with vec4.
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source_str(
        fs,
        "#version 300 es\n\
         precision mediump float;\n\
         in vec2 vColor;\n\
         out vec4 fragColor;\n\
         void main() { fragColor = vec4(vColor, 0.0, 1.0); }\n",
    );
    ctx.compile_shader(fs);

    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);

    assert!(
        !ctx.program_link_status(prog),
        "program with vec4/vec2 varying type mismatch must NOT link (GAP-012)"
    );
}

#[test]
fn matching_varying_types_link_successfully() {
    let mut ctx = Context::new();

    let vs = ctx.create_shader(ShaderKind::Vertex);
    ctx.shader_source_str(
        vs,
        "#version 300 es\n\
         in vec4 a_pos;\n\
         out vec4 vColor;\n\
         void main() { gl_Position = a_pos; vColor = vec4(1.0); }\n",
    );
    ctx.compile_shader(vs);

    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source_str(
        fs,
        "#version 300 es\n\
         precision mediump float;\n\
         in vec4 vColor;\n\
         out vec4 fragColor;\n\
         void main() { fragColor = vColor; }\n",
    );
    ctx.compile_shader(fs);

    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);

    assert!(
        ctx.program_link_status(prog),
        "matching varying types must link successfully"
    );
}
