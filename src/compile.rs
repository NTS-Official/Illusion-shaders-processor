//! Drives one shader through preprocess -> normalise -> naga -> HLSL + manifest.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use naga::back::hlsl::{self, BindTarget, BindingMap};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga::{AddressSpace, Expression, ImageClass, SampleLevel, ShaderStage, TypeInner};

use crate::dialect;
use crate::lexer;
use crate::manifest::{self, Manifest, ManifestOptions, ResourceKind, ResourceRegister};
use crate::preprocess;

#[derive(Clone, Debug)]
pub struct CompileOptions {
    pub include_roots: Vec<PathBuf>,
    pub defines: BTreeMap<String, Option<String>>,
    pub entry_point: String,
    pub shader_model: hlsl::ShaderModel,
    pub space: u8,
    pub texture_register_base: u32,
    pub sampler_register_base: u32,
    pub cbuffer_register_base: u32,
    pub storage_register_base: u32,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            include_roots: Vec::new(),
            defines: BTreeMap::new(),
            entry_point: "main".to_owned(),
            shader_model: hlsl::ShaderModel::V6_0,
            space: 0,
            texture_register_base: 0,
            sampler_register_base: 0,
            cbuffer_register_base: 0,
            storage_register_base: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompiledShader {
    pub hlsl: String,
    pub manifest: Manifest,
    pub normalized_glsl: String,
    pub included: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Preprocess(#[from] preprocess::Error),
    #[error(transparent)]
    Dialect(#[from] dialect::Error),
    #[error("GLSL frontend rejected the shader:\n{0}")]
    Frontend(String),
    #[error("shader failed validation: {0}")]
    Validation(String),
    #[error("HLSL backend failed: {0}")]
    Backend(String),
    #[error("entry point `{0}` was not produced by the frontend")]
    MissingEntryPoint(String),
    #[error("cannot write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn stage_from_path(path: &Path) -> Option<ShaderStage> {
    match path.extension()?.to_str()? {
        "vsh" | "vert" => Some(ShaderStage::Vertex),
        "fsh" | "frag" => Some(ShaderStage::Fragment),
        "comp" => Some(ShaderStage::Compute),
        _ => None,
    }
}

pub fn compile(
    path: &Path,
    stage: ShaderStage,
    options: &CompileOptions,
) -> Result<CompiledShader, Error> {
    let preprocessed = preprocess::preprocess(
        path,
        &preprocess::PreprocessOptions {
            include_roots: options.include_roots.clone(),
            defines: options.defines.clone(),
        },
    )?;

    let tokens = lexer::tokenize(&preprocessed.text);
    let (tokens, dialect_info) = dialect::normalize(tokens)?;
    let normalized_glsl = lexer::render(&tokens);

    let mut frontend = naga::front::glsl::Frontend::default();
    let parse_options = naga::front::glsl::Options {
        stage,
        defines: Default::default(),
    };
    let mut module = frontend
        .parse(&parse_options, &normalized_glsl)
        .map_err(|errors| Error::Frontend(errors.to_string()))?;

    // Implicit-LOD sampling is only legal in fragment shaders; GLSL lets vertex
    // shaders get away with it, and the hardware samples the base level there.
    if stage != ShaderStage::Fragment {
        clamp_sample_levels(&mut module);
    }

    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::all());
    let info = validator
        .validate(&module)
        .map_err(|error| Error::Validation(format!("{error:?}")))?;

    let (binding_map, resources) = assign_bindings(&module, options);

    let hlsl_options = hlsl::Options {
        shader_model: options.shader_model,
        binding_map,
        static_samplers: true,
        ..Default::default()
    };
    let pipeline_options = hlsl::PipelineOptions {
        entry_point: Some((stage, options.entry_point.clone())),
    };
    let mut hlsl_source = String::new();
    hlsl::Writer::new(&mut hlsl_source, &hlsl_options, &pipeline_options)
        .write(&module, &info, None)
        .map_err(|error| Error::Backend(format!("{error:?}")))?;

    let entry = module
        .entry_points
        .iter()
        .find(|entry| entry.name == options.entry_point && entry.stage == stage)
        .ok_or_else(|| Error::MissingEntryPoint(options.entry_point.clone()))?;

    let warnings = dialect_info.warnings.clone();
    let shader_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let manifest = manifest::build(
        &module,
        entry,
        &resources,
        &dialect_info,
        ManifestOptions {
            shader_name: shader_name.clone(),
            stage: stage_label(stage).to_owned(),
            entry: options.entry_point.clone(),
            hlsl_file: format!("{shader_name}.hlsl"),
            shader_model: options.shader_model.to_str().to_owned(),
        },
        warnings.clone(),
    );

    Ok(CompiledShader {
        hlsl: hlsl_source,
        manifest,
        normalized_glsl,
        included: preprocessed.included,
        warnings,
    })
}

fn clamp_sample_levels(module: &mut naga::Module) {
    for (_, function) in module.functions.iter_mut() {
        for (_, expression) in function.expressions.iter_mut() {
            if let Expression::ImageSample { level, .. } = expression
                && *level == SampleLevel::Auto
            {
                *level = SampleLevel::Zero;
            }
        }
    }
}
fn assign_bindings(
    module: &naga::Module,
    options: &CompileOptions,
) -> (BindingMap, Vec<ResourceRegister>) {
    let mut textures = options.texture_register_base;
    let mut samplers = options.sampler_register_base;
    let mut cbuffers = options.cbuffer_register_base;
    let mut storage = options.storage_register_base;

    let mut binding_map = BindingMap::new();
    let mut resources = Vec::new();

    for (handle, variable) in module.global_variables.iter() {
        let Some(binding) = variable.binding else {
            continue;
        };
        let inner = &module.types[variable.ty].inner;

        let (kind, register) = match variable.space {
            AddressSpace::Handle => match inner {
                TypeInner::Image {
                    class: ImageClass::Sampled { .. } | ImageClass::Depth { .. },
                    ..
                } => {
                    let register = textures;
                    textures += 1;
                    (ResourceKind::Texture, register)
                }
                TypeInner::Sampler { .. } => {
                    let register = samplers;
                    samplers += 1;
                    (ResourceKind::Sampler, register)
                }
                _ => continue,
            },
            AddressSpace::Uniform => {
                let register = cbuffers;
                cbuffers += 1;
                (ResourceKind::ConstantBuffer, register)
            }
            AddressSpace::Storage { .. } => {
                let register = storage;
                storage += 1;
                (ResourceKind::StorageBuffer, register)
            }
            _ => continue,
        };

        binding_map.insert(
            binding,
            BindTarget {
                space: options.space,
                register,
                ..Default::default()
            },
        );
        resources.push(ResourceRegister {
            handle,
            kind,
            register,
            space: u32::from(options.space),
        });
    }

    (binding_map, resources)
}
pub fn stage_label(stage: ShaderStage) -> &'static str {
    match stage {
        ShaderStage::Vertex => "vertex",
        ShaderStage::Fragment => "fragment",
        ShaderStage::Compute => "compute",
        ShaderStage::Task => "task",
        ShaderStage::Mesh => "mesh",
        ShaderStage::RayGeneration => "ray_generation",
        ShaderStage::AnyHit => "any_hit",
        ShaderStage::ClosestHit => "closest_hit",
        ShaderStage::Miss => "miss",
    }
}

/// Writes `<name>.hlsl` and `<name>.manifest.json` next to each other.
pub fn write_outputs(
    directory: &Path,
    shader_name: &str,
    compiled: &CompiledShader,
) -> Result<(PathBuf, PathBuf), Error> {
    std::fs::create_dir_all(directory).map_err(|source| Error::Io {
        path: directory.to_owned(),
        source,
    })?;

    let hlsl_path = directory.join(format!("{shader_name}.hlsl"));
    std::fs::write(&hlsl_path, compiled.hlsl.as_bytes()).map_err(|source| Error::Io {
        path: hlsl_path.clone(),
        source,
    })?;

    let manifest_path = directory.join(format!("{shader_name}.manifest.json"));
    let json = manifest::to_json(&compiled.manifest)
        .map_err(|error| Error::Backend(format!("cannot serialise manifest: {error}")))?;
    std::fs::write(&manifest_path, json.as_bytes()).map_err(|source| Error::Io {
        path: manifest_path.clone(),
        source,
    })?;

    Ok((hlsl_path, manifest_path))
}
