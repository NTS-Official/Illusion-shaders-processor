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
use crate::stage_io::{self, StageOutput};

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
    pub register_layout: RegisterLayout,
    /// Explicit `name -> register` overrides, keyed by constant-buffer struct name and texture
    /// name; a split sampler takes its texture's entry.
    pub bindings: BTreeMap<String, u32>,
    /// Vertex attribute names in the order the host's input layout lists them; empty for other
    /// stages.
    pub vertex_attributes: Vec<String>,
    /// Appends `gl_Position.y = -gl_Position.y;` after each clip-space write, matching the D3D
    /// convention the host renders with.
    pub flip_y: bool,
    /// Fragment input whose `.y` is flipped for the lightmap pass; `None` keeps the shader as is.
    pub flip_texcoord: Option<String>,
    /// Collects loose `uniform <type> <name>;` declarations into this `std140` block; shader packs
    /// need it because naga rejects uniforms outside a block.
    pub uniform_block: Option<String>,
    /// Rewrite fixed-function GLSL builtins that shader packs still use.
    pub legacy_builtins: bool,
    /// Vertex stage outputs in declaration order; the fragment stage pads its inputs to this
    /// interface. D3D12 links stages by register and component mask, so both sides have to pack
    /// the interface the same way (see `stage_io`).
    pub vertex_outputs: Vec<StageOutput>,
}

/// How bindings are numbered in the emitted HLSL.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RegisterLayout {
    /// Independent `b`/`t`/`s` counters driven by the `*_register_base` options.
    #[default]
    SpirvBases,
    /// One index per resource, in the host's binding order: constant buffers first, then
    /// textures (each sampler shares its texture's index), then storage buffers. This is
    /// what `D3D12BindGroupEntry` produces, where the list index doubles as the register.
    HostIndex,
}

impl RegisterLayout {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "spirv" | "spirv-bases" => Some(Self::SpirvBases),
            "host" | "host-index" => Some(Self::HostIndex),
            _ => None,
        }
    }
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
            // Read-only storage buffers land in the `t` register space, so they
            // must not overlap the textures that start at `t0`.
            storage_register_base: 32,
            register_layout: RegisterLayout::SpirvBases,
            bindings: BTreeMap::new(),
            vertex_attributes: Vec::new(),
            flip_y: false,
            flip_texcoord: None,
            uniform_block: None,
            legacy_builtins: false,
            vertex_outputs: Vec::new(),
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
    #[error("GLSL frontend rejected the shader: {0}")]
    Frontend(String),
    #[error("shader failed validation: {0}")]
    Validation(String),
    #[error("HLSL backend failed: {0}")]
    Backend(String),
    #[error("entry point `{0}` was not produced by the frontend")]
    MissingEntryPoint(String),
    #[error("stage interface: {0}")]
    StageInterface(String),
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
        "comp" | "csh" => Some(ShaderStage::Compute),
        _ => None,
    }
}

impl Error {
    /// Stable tag for the C ABI error response; the host may branch on it.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Preprocess(_) => "preprocess",
            Error::Dialect(_) => "dialect",
            Error::Frontend(_) => "glsl-frontend",
            Error::Validation(_) => "validation",
            Error::Backend(_) => "hlsl-backend",
            Error::MissingEntryPoint(_) => "missing-entry-point",
            Error::StageInterface(_) => "stage-io",
            Error::Io { .. } => "io",
        }
    }
}

/// Maps a span in the normalised GLSL back to the file and line it came from,
/// so frontend, validation and backend errors point at the shader the author
/// edited instead of at byte offsets in the flattened text.
struct SourceMap {
    text: String,
    /// One entry per normalised line (0-based): the preprocessed line (1-based).
    lines: Vec<u32>,
    /// One entry per preprocessed line (0-based).
    origins: Vec<preprocess::Origin>,
}

impl SourceMap {
    fn new(text: String, lines: Vec<u32>, origins: Vec<preprocess::Origin>) -> Self {
        Self {
            text,
            lines,
            origins,
        }
    }

    fn locate(&self, span: naga::Span) -> Option<String> {
        if !span.is_defined() {
            return None;
        }
        let line = span.location(&self.text).line_number as usize;
        let preprocessed = *self.lines.get(line.checked_sub(1)?)?;
        if preprocessed == 0 {
            return None;
        }
        let origin = self.origins.get(preprocessed as usize - 1)?;
        Some(format!("{}:{}", origin.path.display(), origin.line))
    }

    fn locate_spans<E>(&self, error: &naga::WithSpan<E>) -> Option<String> {
        // Spans are pushed outermost first, so the last one that resolves is the
        // most precise statement about where the shader went wrong.
        error
            .spans()
            .filter_map(|(span, _)| self.locate(*span))
            .last()
    }

    /// Describes a validation failure with its location and the whole error
    /// chain, because `ValidationError`'s own `Display` stops at "is invalid".
    fn describe_validation(&self, error: &naga::WithSpan<naga::valid::ValidationError>) -> String {
        let mut text = match self.locate_spans(error) {
            Some(location) => format!("{location}: {error}"),
            None => error.to_string(),
        };
        let mut source = std::error::Error::source(error);
        while let Some(inner) = source {
            text.push_str(&format!(": {inner}"));
            source = inner.source();
        }
        text
    }

    /// Describes frontend diagnostics with the location of the first error.
    fn describe_frontend(&self, errors: &naga::front::glsl::ParseErrors) -> String {
        let Some(first) = errors.errors.first() else {
            return "the frontend produced no diagnostics".to_owned();
        };
        let message = first.to_string();
        let mut text = match self.locate(first.meta) {
            Some(location) => format!("{location}: {message}"),
            None => message,
        };
        let rest = errors.errors.len().saturating_sub(1);
        if rest > 0 {
            text.push_str(&format!(" (+{rest} more error(s))"));
        }
        text
    }
}

pub fn compile(
    path: &Path,
    stage: ShaderStage,
    options: &CompileOptions,
) -> Result<CompiledShader, Error> {
    compile_with_source(path, None, stage, options)
}

/// Compiles a shader, optionally from source text instead of the file at
/// `path`. Includes still resolve relative to `path`.
pub fn compile_with_source(
    path: &Path,
    source: Option<&str>,
    stage: ShaderStage,
    options: &CompileOptions,
) -> Result<CompiledShader, Error> {
    let preprocess_options = preprocess::PreprocessOptions {
        include_roots: options.include_roots.clone(),
        defines: stage_defines(&options.defines, stage),
    };
    let preprocessed = match source {
        Some(source) => preprocess::preprocess_source(path, source, &preprocess_options)?,
        None => preprocess::preprocess(path, &preprocess_options)?,
    };

    let tokens = lexer::tokenize(&preprocessed.text);
    let attribute_order: &[String] = if stage == ShaderStage::Vertex {
        &options.vertex_attributes
    } else {
        &[]
    };
    let (tokens, dialect_info) = dialect::normalize_with_config(
        tokens,
        &dialect::NormalizeOptions {
            attribute_order,
            flip_y: options.flip_y,
            flip_texcoord: options.flip_texcoord.as_deref(),
            uniform_block: options.uniform_block.as_deref(),
            legacy_builtins: options.legacy_builtins,
        },
    )?;
    let (normalized_glsl, source_map) = {
        let (text, lines) = lexer::render_with_lines(&tokens);
        let map = SourceMap::new(text.clone(), lines, preprocessed.origins);
        (text, map)
    };

    let mut frontend = naga::front::glsl::Frontend::default();
    let parse_options = naga::front::glsl::Options {
        stage,
        defines: Default::default(),
    };
    let mut module = frontend
        .parse(&parse_options, &normalized_glsl)
        .map_err(|errors| Error::Frontend(source_map.describe_frontend(&errors)))?;

    // Implicit-LOD sampling is only legal in fragment shaders; GLSL lets vertex
    // shaders get away with it, and the hardware samples the base level there.
    if stage != ShaderStage::Fragment {
        clamp_sample_levels(&mut module);
    }

    let padded_inputs = if stage == ShaderStage::Fragment {
        stage_io::pad_fragment_inputs(&mut module, &options.entry_point, &options.vertex_outputs)
            .map_err(Error::StageInterface)?
    } else {
        0
    };

    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::all());
    let info = validator
        .validate(&module)
        .map_err(|error| Error::Validation(source_map.describe_validation(&error)))?;

    let (binding_map, resources) = assign_bindings(&module, options, &dialect_info);

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
        .map_err(|error| Error::Backend(error.to_string()))?;

    let entry = module
        .entry_points
        .iter()
        .find(|entry| entry.name == options.entry_point && entry.stage == stage)
        .ok_or_else(|| Error::MissingEntryPoint(options.entry_point.clone()))?;

    let mut warnings = dialect_info.warnings.clone();
    if padded_inputs > 0 {
        warnings.push(format!(
            "padded {padded_inputs} fragment input(s) so the stage interface packs like the vertex stage"
        ));
    }
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

/// Adds the stage macro that shader packs branch on (`VERTEX_SHADER` / `FRAGMENT_SHADER`).
///
/// Iris and OptiFine define one of them so a shared include can hold both stage paths. Without the
/// macro the other stage's half compiles too, which shows up as errors about vertex-only builtins.
fn stage_defines(
    defines: &BTreeMap<String, Option<String>>,
    stage: ShaderStage,
) -> BTreeMap<String, Option<String>> {
    let mut defines = defines.clone();
    let name = match stage {
        ShaderStage::Vertex => "VERTEX_SHADER",
        ShaderStage::Fragment => "FRAGMENT_SHADER",
        ShaderStage::Compute => "COMPUTE_SHADER",
        _ => return defines,
    };
    defines.entry(name.to_owned()).or_insert(None);
    defines
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
    dialect_info: &dialect::DialectInfo,
) -> (BindingMap, Vec<ResourceRegister>) {
    match options.register_layout {
        RegisterLayout::SpirvBases => assign_with_bases(module, options),
        RegisterLayout::HostIndex => assign_host_index(module, options, dialect_info),
    }
}

fn assign_with_bases(
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
                TypeInner::Image {
                    class: ImageClass::Storage { .. },
                    ..
                } => {
                    // A storage image is a UAV, so it shares the `storage` numbering: both end
                    // up in the `u` register class.
                    let register = storage;
                    storage += 1;
                    (ResourceKind::StorageImage, register)
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

/// Number every resource with one index, in the host's binding order.
///
/// The host builds its root signature from `D3D12BindGroupEntry` and uses that list index as the
/// register for `b`/`t`/`s` alike, so the HLSL has to use the same numbers: constant buffers
/// first, then textures (a split sampler takes its texture's index), then storage buffers.
fn assign_host_index(
    module: &naga::Module,
    options: &CompileOptions,
    dialect_info: &dialect::DialectInfo,
) -> (BindingMap, Vec<ResourceRegister>) {
    let mut cbuffer_count = 0u32;
    let mut texture_count = 0u32;
    let mut storage_buffer_count = 0u32;
    let mut storage_image_count = 0u32;
    for (_, variable) in module.global_variables.iter() {
        if variable.binding.is_none() {
            continue;
        }
        match variable.space {
            AddressSpace::Uniform => cbuffer_count += 1,
            AddressSpace::Storage { .. } => storage_buffer_count += 1,
            AddressSpace::Handle => match module.types[variable.ty].inner {
                TypeInner::Image {
                    class: ImageClass::Sampled { .. } | ImageClass::Depth { .. },
                    ..
                } => texture_count += 1,
                TypeInner::Image {
                    class: ImageClass::Storage { .. },
                    ..
                } => storage_image_count += 1,
                _ => {}
            },
            _ => {}
        }
    }

    let sampler_to_texture: BTreeMap<&str, &str> = dialect_info
        .combined_samplers
        .iter()
        .map(|combined| (combined.sampler.as_str(), combined.texture.as_str()))
        .collect();
    let mut texture_registers: BTreeMap<String, u32> = BTreeMap::new();
    let mut binding_map = BindingMap::new();
    let mut resources = Vec::new();
    let mut cbuffers = 0u32;
    let mut textures = cbuffer_count;
    let mut storage = cbuffer_count + texture_count;
    // Storage images come after the storage buffers: the host lists CBVs, then textures, then
    // storage buffers, then UAVs.
    let mut storage_images = storage + storage_buffer_count;
    let _ = storage_image_count;

    for (handle, variable) in module.global_variables.iter() {
        let Some(binding) = variable.binding else {
            continue;
        };
        let inner = &module.types[variable.ty].inner;
        let name = variable.name.clone().unwrap_or_default();
        let (kind, register) = match variable.space {
            AddressSpace::Handle => match inner {
                TypeInner::Image {
                    class: ImageClass::Sampled { .. } | ImageClass::Depth { .. },
                    ..
                } => {
                    let register = options.bindings.get(&name).copied().unwrap_or(textures);
                    texture_registers.insert(name.clone(), register);
                    textures = textures.max(register + 1);
                    (ResourceKind::Texture, register)
                }
                TypeInner::Image {
                    class: ImageClass::Storage { .. },
                    ..
                } => {
                    let register = options
                        .bindings
                        .get(&name)
                        .copied()
                        .unwrap_or(storage_images);
                    storage_images = storage_images.max(register + 1);
                    (ResourceKind::StorageImage, register)
                }
                TypeInner::Sampler { .. } => {
                    let texture = sampler_to_texture
                        .get(name.as_str())
                        .map(|texture| (*texture).to_owned())
                        .unwrap_or_else(|| name.clone());
                    let register = options
                        .bindings
                        .get(&texture)
                        .copied()
                        .or_else(|| {
                            sampler_to_texture
                                .get(name.as_str())
                                .and_then(|texture| texture_registers.get(*texture).copied())
                        })
                        .unwrap_or(textures);
                    (ResourceKind::Sampler, register)
                }
                _ => continue,
            },
            AddressSpace::Uniform => {
                let key = cbuffer_key(module, variable);
                let register = options.bindings.get(&key).copied().unwrap_or(cbuffers);
                cbuffers = cbuffers.max(register + 1);
                (ResourceKind::ConstantBuffer, register)
            }
            AddressSpace::Storage { .. } => {
                let register = options.bindings.get(&name).copied().unwrap_or(storage);
                storage = storage.max(register + 1);
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

/// The name the manifest and the host both use for a constant buffer: the struct name.
fn cbuffer_key(module: &naga::Module, variable: &naga::GlobalVariable) -> String {
    module.types[variable.ty].name.clone().unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "#version 330
layout(std140) uniform Globals { vec4 Color; };
layout(std140) uniform Extra { vec4 Tint; };
uniform sampler2D Sampler0;
out vec4 fragColor;
void main() { fragColor = Color + Tint + texture(Sampler0, vec2(0.5)); }
";

    fn compile_with(layout: RegisterLayout) -> String {
        let options = CompileOptions {
            register_layout: layout,
            ..Default::default()
        };
        let compiled = compile_with_source(
            Path::new("test.fsh"),
            Some(SOURCE),
            ShaderStage::Fragment,
            &options,
        )
        .expect("compile");
        compiled.hlsl
    }

    fn compile_source(source: &str) -> String {
        let options = CompileOptions::default();
        compile_with_source(
            Path::new("test.fsh"),
            Some(source),
            ShaderStage::Fragment,
            &options,
        )
        .expect("compile")
        .hlsl
    }

    #[test]
    fn converts_wider_sampler_shapes() {
        let hlsl = compile_source(
            r#"#version 330
uniform samplerCubeArray Sky;
uniform sampler2DMS Samples;
out vec4 fragColor;
void main() { fragColor = texture(Sky, vec4(0.0)) + texelFetch(Samples, ivec2(1, 1), 0); }
"#,
        );
        assert!(hlsl.contains("TextureCubeArray<float4>"), "{hlsl}");
        assert!(hlsl.contains("Texture2DMS<float4>"), "{hlsl}");
    }

    fn compile_compiled(source: &str) -> CompiledShader {
        let options = CompileOptions::default();
        compile_with_source(
            Path::new("test.fsh"),
            Some(source),
            ShaderStage::Fragment,
            &options,
        )
        .expect("compile")
    }

    #[test]
    fn records_constant_buffer_offsets() {
        let compiled = compile_compiled(
            r#"#version 330
layout(std140) uniform First { vec3 A; float B; mat4 M; vec2 T[2]; };
uniform sampler2D Sampler;
out vec4 fragColor;
void main() { fragColor = texture(Sampler, vec2(0.0)) + vec4(A, B) + M[0] + vec4(T[0], T[1]); }
"#,
        );
        let buffer = &compiled.manifest.resources.constant_buffers[0];
        assert_eq!(buffer.size, 112);
        let offsets: Vec<(String, u32, u32)> = buffer
            .members
            .iter()
            .map(|member| (member.name.clone(), member.offset, member.size))
            .collect();
        assert_eq!(
            offsets,
            vec![
                ("A".to_owned(), 0, 12),
                ("B".to_owned(), 12, 4),
                ("M".to_owned(), 16, 64),
                ("T".to_owned(), 80, 32),
            ]
        );
    }

    #[test]
    fn host_layout_numbers_one_index_per_resource() {
        let hlsl = compile_with(RegisterLayout::HostIndex);
        assert!(hlsl.contains(": register(b0)"), "{hlsl}");
        assert!(hlsl.contains(": register(b1)"), "{hlsl}");
        assert!(hlsl.contains(": register(t2)"), "{hlsl}");
        assert!(hlsl.contains(": register(s2)"), "{hlsl}");
    }

    #[test]
    fn spirv_layout_keeps_independent_registers() {
        let hlsl = compile_with(RegisterLayout::SpirvBases);
        assert!(hlsl.contains(": register(b0)"), "{hlsl}");
        assert!(hlsl.contains(": register(b1)"), "{hlsl}");
        assert!(hlsl.contains(": register(t0)"), "{hlsl}");
        assert!(hlsl.contains(": register(s0)"), "{hlsl}");
    }

    #[test]
    fn vertex_attributes_number_vertex_inputs() {
        const VERTEX: &str = "#version 330
in vec3 Position;
in vec4 Color;
in vec2 UV0;
out vec4 vertexColor;
void main() { gl_Position = vec4(Position, 1.0); vertexColor = Color + vec4(UV0, 0.0, 0.0); }
";
        let options = CompileOptions {
            vertex_attributes: ["Position", "UV0", "Color"]
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            ..Default::default()
        };
        let compiled = compile_with_source(
            Path::new("test.vsh"),
            Some(VERTEX),
            ShaderStage::Vertex,
            &options,
        )
        .expect("compile");
        let hlsl = &compiled.hlsl;
        // Locations follow the host layout, not the declaration order (and naga may rename a
        // parameter that collides with an existing name, hence `UV0_`).
        assert!(hlsl.contains("float3 Position : LOC0"), "{hlsl}");
        assert!(hlsl.contains("float2 UV0_ : LOC1"), "{hlsl}");
        assert!(hlsl.contains("float4 Color : LOC2"), "{hlsl}");
    }

    #[test]
    fn explicit_bindings_win_over_sequential_numbers() {
        let options = CompileOptions {
            register_layout: RegisterLayout::HostIndex,
            bindings: [("Globals".to_owned(), 3), ("Extra".to_owned(), 7)]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let compiled = compile_with_source(
            Path::new("test.fsh"),
            Some(SOURCE),
            ShaderStage::Fragment,
            &options,
        )
        .expect("compile");
        assert!(
            compiled.hlsl.contains(": register(b3)"),
            "{}",
            compiled.hlsl
        );
        assert!(
            compiled.hlsl.contains(": register(b7)"),
            "{}",
            compiled.hlsl
        );
    }
}
