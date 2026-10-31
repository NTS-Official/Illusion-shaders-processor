//! The same input and the same options must produce byte-identical HLSL and
//! manifest: hosts cache the artifacts, and CI diffs them.
//!
//! This guards accidental `HashMap` iteration order in the dialect or manifest
//! stages. Report ordering (`audit` / `verify`) is covered separately.

use std::path::PathBuf;

use illusion_shader::compile::{self, CompileOptions, RegisterLayout};
use illusion_shader::manifest;

const GOLDEN: &str = "tests/fixtures/golden";

fn convert(file: &str, options: &CompileOptions) -> (String, String) {
    let path = PathBuf::from(GOLDEN).join(file);
    let stage = compile::stage_from_path(&path).expect("fixture has a known extension");
    let shader = compile::compile(&path, stage, options)
        .unwrap_or_else(|error| panic!("{file} does not convert: {error}"));
    let json = manifest::to_json(&shader.manifest).expect("manifest serialises");
    (shader.hlsl, json)
}

#[test]
fn conversion_is_reproducible() {
    let host = CompileOptions {
        register_layout: RegisterLayout::HostIndex,
        bindings: [("Globals".to_owned(), 3), ("Sampler0".to_owned(), 7)]
            .into_iter()
            .collect(),
        ..CompileOptions::default()
    };

    let pack = CompileOptions {
        uniform_block: Some("CommonUniforms".to_owned()),
        legacy_builtins: true,
        ..CompileOptions::default()
    };

    let vertex = CompileOptions {
        vertex_attributes: vec!["Position".to_owned(), "UV0".to_owned(), "Color".to_owned()],
        ..CompileOptions::default()
    };

    let cases: [(&str, &CompileOptions); 3] = [
        ("host_bindings.fsh", &host),
        ("pack_fragment.fsh", &pack),
        ("vertex_inputs.vsh", &vertex),
    ];

    for (file, options) in cases {
        let first = convert(file, options);
        let second = convert(file, options);
        assert_eq!(first.0, second.0, "{file}: HLSL differs between runs");
        assert_eq!(first.1, second.1, "{file}: manifest differs between runs");
    }
}
