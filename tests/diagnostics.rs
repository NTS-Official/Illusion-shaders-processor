//! Diagnostics name the file and line the author has to open.
//!
//! A GLSL error only carries a byte span into the flattened, preprocessed text.
//! Without the line map a pack author sees `Span { start: 182 }` and has to
//! count bytes across every `#include` to find the line that broke.

use std::path::{Path, PathBuf};

use illusion_shader::compile::{self, CompileOptions};

const FIXTURES: &str = "tests/fixtures/diagnostics";

fn error_of(file: &str) -> String {
    let path = PathBuf::from(FIXTURES).join(file);
    let stage = compile::stage_from_path(&path).expect("fixture has a known extension");
    let options = CompileOptions {
        include_roots: vec![Path::new(FIXTURES).to_path_buf()],
        ..CompileOptions::default()
    };
    match compile::compile(&path, stage, &options) {
        Ok(_) => panic!("{file} converted, but the fixture is meant to fail"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_syntax_error_names_the_shader_and_line() {
    let message = error_of("syntax_error.fsh");
    assert!(
        message.contains("syntax_error.fsh:7:"),
        "the diagnostic must point at line 7: {message}"
    );
    assert!(
        message.contains("Expected Semicolon"),
        "the frontend's own message must survive: {message}"
    );
}

#[test]
fn an_error_inside_an_include_names_the_included_file() {
    let message = error_of("include_error.fsh");
    assert!(
        message.contains("typed.glsl:5:"),
        "the diagnostic must point into the include: {message}"
    );
    assert!(
        message.contains("doesn't match the type stored in"),
        "the validation reason must survive: {message}"
    );
}

#[test]
fn a_preprocess_error_keeps_its_location_without_repeating_the_path() {
    let message = error_of("unterminated_comment.fsh");
    assert!(
        message.contains("unterminated_comment.fsh:4:"),
        "the diagnostic must point at the unclosed comment: {message}"
    );
    assert_eq!(
        message.matches("unterminated_comment.fsh").count(),
        1,
        "the path must appear once: {message}"
    );
}
