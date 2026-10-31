//! Robustness tests: broken GLSL surfaces as an error, never as a panic.
//!
//! The converter runs inside the game process, so a panic in a rewrite pass would take the render
//! thread down instead of failing a single pipeline. Every case here is input a shader pack or a
//! truncated file can actually produce: unbalanced braces, cut-off directives, sampler parameters
//! without a name, recursive macros.

use std::path::Path;

use illusion_shader::compile::{CompileOptions, compile_with_source};
use naga::ShaderStage;

/// Error tags documented in `docs/shader-convert-abi.md`; a new tag has to be added there too.
const KINDS: &[&str] = &[
    "preprocess",
    "dialect",
    "glsl-frontend",
    "validation",
    "hlsl-backend",
    "missing-entry-point",
    "io",
];

const CASES: &[(&str, &str)] = &[
    ("empty", ""),
    ("comments only", "// nothing here\n/* and nothing here */\n"),
    ("version only", "#version 330\n"),
    (
        "unclosed brace",
        "#version 450\nout vec4 color;\nvoid main() {\n  color = vec4(1.0);\n",
    ),
    (
        "unclosed parenthesis",
        "#version 450\nvoid main() {\n  float x = max(1.0, 2.0;\n}\n",
    ),
    (
        "unterminated parameter list",
        "#version 450\nvec4 shade(vec4 c, float\nvoid main() { }\n",
    ),
    (
        "sampler parameter without a name",
        "#version 450\nuniform sampler2D tex;\nvec4 shade(sampler2D) { return vec4(0.0); }\nvoid main() { }\n",
    ),
    (
        "unclosed struct",
        "#version 450\nstruct Light {\n  vec4 colour;\nvoid main() { }\n",
    ),
    (
        "truncated fragdata index",
        "#version 450\nvoid main() {\n  gl_FragData[\n}\n",
    ),
    (
        "non-constant fragdata index",
        "#version 450\nuniform int slot;\nvoid main() { gl_FragData[slot] = vec4(0.0); }\n",
    ),
    (
        "truncated sample call",
        "#version 450\nuniform sampler2D tex;\nvoid main() { gl_FragColor = texture2D(; }\n",
    ),
    (
        "if without endif",
        "#version 450\n#if defined(MC_VERSION)\nvoid main() { }\n",
    ),
    ("stray endif", "#version 450\n#endif\nvoid main() { }\n"),
    (
        "truncated function-like macro",
        "#version 450\n#define SCALE(x\nvoid main() { }\n",
    ),
    (
        "recursive macros",
        "#version 450\n#define A B\n#define B A\nfloat x = A;\nvoid main() { }\n",
    ),
    (
        "macro used as array size",
        "#version 450\n#define COUNT\nfloat x[COUNT];\nvoid main() { }\n",
    ),
    (
        "missing include",
        "#version 450\n#include \"absent.glsl\"\nvoid main() { }\n",
    ),
    (
        "unterminated include",
        "#version 450\n#include \"absent.glsl\nvoid main() { }\n",
    ),
    (
        "unclosed comment",
        "#version 450\nvoid main() { }\n/* trailing\n",
    ),
    (
        "texture matrix out of range",
        "#version 120\nvoid main() {\n  gl_Position = gl_TextureMatrix[5] * gl_Vertex;\n}\n",
    ),
    (
        "legacy varying without main body",
        "#version 120\nvarying vec4 tint;\nvoid main() {\n  gl_FragColor = tint;\n",
    ),
    (
        "duplicate layout location",
        "#version 450\nlayout(location = 0) in vec4 a;\nlayout(location = 0) in vec4 b;\nvoid main() { }\n",
    ),
    (
        "position written twice, second one cut",
        "#version 450\nvoid main() {\n  gl_Position = vec4(0.0);\n  gl_Position =\n}\n",
    ),
];

/// Compiles one broken case; `Ok` carries how it failed, `Err` means it converted as if valid.
fn convert(source: &str) -> Result<String, String> {
    let path = Path::new("malformed.fsh");
    match compile_with_source(
        path,
        Some(source),
        ShaderStage::Fragment,
        &CompileOptions::default(),
    ) {
        Ok(_) => Err("malformed source converted successfully".to_owned()),
        Err(error) => {
            let kind = error.kind();
            assert!(
                KINDS.contains(&kind),
                "error kind `{kind}` is missing from docs/shader-convert-abi.md"
            );
            Ok(format!("{kind}: {error}"))
        }
    }
}

#[test]
fn malformed_input_reports_an_error() {
    let failures: Vec<String> = CASES
        .iter()
        .filter_map(|(name, source)| match convert(source) {
            Ok(_) => None,
            Err(reason) => Some(format!("{name} -> {reason}")),
        })
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn deep_expression_nesting_does_not_overflow() {
    let mut source = String::from("#version 450\nout vec4 color;\nvoid main() {\n  color = vec4(");
    source.push_str(&"(".repeat(64));
    source.push_str("1.0");
    source.push_str(&")".repeat(64));
    source.push_str(");\n}\n");
    // Either outcome is acceptable; the test exists to prove no panic or stack overflow.
    let _ = compile_with_source(
        Path::new("nested.fsh"),
        Some(&source),
        ShaderStage::Fragment,
        &CompileOptions::default(),
    );
}

/// A directive inside `/* ... */` is comment text, not a directive: packs comment out feature
/// toggles, and honouring them switches branches the author disabled.
#[test]
fn directives_inside_block_comments_are_comment_text() {
    let source = concat!(
        "#version 450\n",
        "out vec4 color;\n",
        "/*\n",
        "#define FEATURE 1\n",
        "*/\n",
        "#if defined(FEATURE)\n",
        "void main() { color = vec4(1.0); }\n",
        "#else\n",
        "void main() { color = vec4(0.0); }\n",
        "#endif\n",
    );
    let compiled = compile_with_source(
        Path::new("comment.fsh"),
        Some(source),
        ShaderStage::Fragment,
        &CompileOptions::default(),
    )
    .expect("a shader with a commented-out toggle converts");
    assert!(
        compiled.hlsl.contains("(0.0)") && !compiled.hlsl.contains("(1.0)"),
        "the #else branch must win:\n{}",
        compiled.hlsl
    );
}

/// Qualifier-only declarations are nonsense GLSL that the dialect leaves alone and naga drops.
/// Converting them must not panic; the exact outcome is not part of the contract.
#[test]
fn stray_qualifiers_do_not_panic() {
    for source in [
        "#version 450\nuniform;\nvoid main() { }\n",
        "#version 450\nuniform bool;\nvoid main() { }\n",
        "#version 450\nin;\nvoid main() { }\n",
    ] {
        if let Err(error) = compile_with_source(
            Path::new("stray.fsh"),
            Some(source),
            ShaderStage::Fragment,
            &CompileOptions::default(),
        ) {
            let kind = error.kind();
            assert!(
                KINDS.contains(&kind),
                "error kind `{kind}` is missing from docs/shader-convert-abi.md"
            );
        }
    }
}
