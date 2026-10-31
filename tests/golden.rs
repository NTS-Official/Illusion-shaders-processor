//! Snapshot tests for the converter's output contract.
//!
//! Each fixture pins the generated HLSL and manifest, so a change to the dialect or to the
//! manifest layout shows up as a reviewable diff instead of a silent contract change. Run with
//! `UPDATE_GOLDEN=1` to refresh the snapshots after an intentional change.

use std::path::{Path, PathBuf};

use illusion_shader::RegisterLayout;
use illusion_shader::compile::{CompileOptions, compile_with_source};
use naga::ShaderStage;

struct Case {
    file: &'static str,
    stage: ShaderStage,
    /// Host vertex-format order; empty for fragment shaders.
    attributes: &'static [&'static str],
    flip_y: bool,
    /// Fragment input whose `y` is flipped, for the lightmap pass.
    flip_texcoord: Option<&'static str>,
    /// Explicit register overrides, as the host passes them from its binding entries.
    bindings: &'static [(&'static str, u32)],
    /// Collected std140 block for shader-pack source with loose uniforms.
    uniform_block: Option<&'static str>,
    legacy_builtins: bool,
}

const CASES: &[Case] = &[
    Case {
        file: "vector_select.fsh",
        stage: ShaderStage::Fragment,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "mrt.fsh",
        stage: ShaderStage::Fragment,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "legacy_sampling.fsh",
        stage: ShaderStage::Fragment,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "vertex_inputs.vsh",
        stage: ShaderStage::Vertex,
        attributes: &["Position", "UV0", "Color"],
        flip_y: true,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "constant_buffer.fsh",
        stage: ShaderStage::Fragment,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "texel_buffer.vsh",
        stage: ShaderStage::Vertex,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "image_query.fsh",
        stage: ShaderStage::Fragment,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "flip_texcoord.fsh",
        stage: ShaderStage::Fragment,
        attributes: &[],
        flip_y: false,
        flip_texcoord: Some("texCoord"),
        bindings: &[],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "shadow_sampler.fsh",
        stage: ShaderStage::Fragment,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "host_bindings.fsh",
        stage: ShaderStage::Fragment,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        // The host numbers its entries, so a buffer and its texture do not start at zero.
        bindings: &[("Globals", 3), ("Sampler0", 7)],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "host_inputs_declared.vsh",
        stage: ShaderStage::Vertex,
        attributes: &["Position", "UV0", "Color"],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: None,
        legacy_builtins: false,
    },
    Case {
        file: "uniform_block.fsh",
        stage: ShaderStage::Fragment,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        // Shader-pack source keeps its uniforms loose; the host fills one std140 block.
        uniform_block: Some("CommonUniforms"),
        legacy_builtins: false,
    },
    Case {
        file: "legacy_builtins.vsh",
        stage: ShaderStage::Vertex,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        // Fixed-function builtins expand to `iris_*` attributes and uniforms.
        uniform_block: Some("CommonUniforms"),
        legacy_builtins: true,
    },
    Case {
        file: "pack_fragment.fsh",
        stage: ShaderStage::Fragment,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: Some("CommonUniforms"),
        legacy_builtins: true,
    },
    Case {
        file: "pack_vertex.vsh",
        stage: ShaderStage::Vertex,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        uniform_block: Some("CommonUniforms"),
        legacy_builtins: true,
    },
    Case {
        file: "compute_image.csh",
        stage: ShaderStage::Compute,
        attributes: &[],
        flip_y: false,
        flip_texcoord: None,
        bindings: &[],
        // A storage image with no format qualifier becomes a `rgba32f` UAV.
        uniform_block: None,
        legacy_builtins: false,
    },
];

fn fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
}

#[test]
fn generated_output_matches_the_snapshots() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let directory = fixture_directory();
    let mut mismatches = Vec::new();

    for case in CASES {
        let source_path = directory.join(case.file);
        let source = std::fs::read_to_string(&source_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", source_path.display()));
        let options = CompileOptions {
            register_layout: RegisterLayout::HostIndex,
            vertex_attributes: case
                .attributes
                .iter()
                .map(|name| name.to_string())
                .collect(),
            flip_y: case.flip_y,
            flip_texcoord: case.flip_texcoord.map(str::to_owned),
            bindings: case
                .bindings
                .iter()
                .map(|(name, register)| (name.to_string(), *register))
                .collect(),
            uniform_block: case.uniform_block.map(str::to_owned),
            legacy_builtins: case.legacy_builtins,
            ..CompileOptions::default()
        };
        let compiled = compile_with_source(&source_path, Some(&source), case.stage, &options)
            .unwrap_or_else(|error| panic!("{} does not convert: {error}", case.file));

        for (suffix, actual) in [
            ("hlsl", compiled.hlsl.clone()),
            (
                "manifest.json",
                illusion_shader::manifest::to_json(&compiled.manifest).expect("manifest json"),
            ),
        ] {
            let golden_path = directory.join(format!("{}.{suffix}", case.file));
            if update {
                std::fs::write(&golden_path, &actual).unwrap_or_else(|error| {
                    panic!("cannot write {}: {error}", golden_path.display())
                });
                continue;
            }
            let expected = std::fs::read_to_string(&golden_path).unwrap_or_else(|error| {
                panic!(
                    "cannot read {} ({error}); run the test with UPDATE_GOLDEN=1 to create it",
                    golden_path.display()
                )
            });
            if expected != actual {
                mismatches.push(format!(
                    "{}: {} differs; rerun with UPDATE_GOLDEN=1 after reviewing the change",
                    case.file, suffix
                ));
            }
        }
    }

    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// A fragment stage that skips an input the vertex stage writes cannot be linked by D3D12: the
/// runtime packs each stage's inputs on its own, so `LOC1` alone lands in a different register
/// mask than `LOC0` + `LOC1`. The converter pads the fragment inputs to the vertex interface.
#[test]
fn a_padded_interface_matches_the_snapshot() {
    use illusion_shader::StageOutput;

    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let directory = fixture_directory();
    let file = "stage_pad.fsh";
    let source_path = directory.join(file);
    let source = std::fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", source_path.display()));
    let options = CompileOptions {
        register_layout: RegisterLayout::HostIndex,
        vertex_outputs: vec![
            StageOutput {
                location: 0,
                hlsl_type: "float".to_owned(),
                interpolation: Some("Perspective".to_owned()),
            },
            StageOutput {
                location: 1,
                hlsl_type: "float2".to_owned(),
                interpolation: Some("Perspective".to_owned()),
            },
        ],
        ..CompileOptions::default()
    };
    let compiled =
        compile_with_source(&source_path, Some(&source), ShaderStage::Fragment, &options)
            .unwrap_or_else(|error| panic!("{file} does not convert: {error}"));

    for (suffix, actual) in [
        ("hlsl", compiled.hlsl.clone()),
        (
            "manifest.json",
            illusion_shader::manifest::to_json(&compiled.manifest).expect("manifest json"),
        ),
    ] {
        let golden_path = directory.join(format!("{file}.{suffix}"));
        if update {
            std::fs::write(&golden_path, &actual)
                .unwrap_or_else(|error| panic!("cannot write {}: {error}", golden_path.display()));
            continue;
        }
        let expected = std::fs::read_to_string(&golden_path).unwrap_or_else(|error| {
            panic!(
                "cannot read {} ({error}); run the test with UPDATE_GOLDEN=1 to create it",
                golden_path.display()
            )
        });
        assert_eq!(expected, actual, "{file}: {suffix} differs");
    }
}
