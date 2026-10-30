//! The manifest the conversion tool hands to the renderer.
//!
//! The manifest is the contract between the converter and the host: it names
//! every resource with the register it occupies in the emitted HLSL, plus the
//! stage interface the pipeline state has to match. The host never parses the
//! HLSL for this information.

use naga::{
    ArraySize, Binding, EntryPoint, Handle, ImageClass, Module, ResourceBinding, Scalar, Type,
    TypeInner,
};
use serde::Serialize;

use crate::dialect::DialectInfo;

pub const SCHEMA: &str = "illusion.shader-manifest/1";

#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
    pub schema: String,
    pub shader: ShaderInfo,
    pub resources: Resources,
    pub stage_io: StageIo,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ShaderInfo {
    pub name: String,
    pub stage: String,
    pub entry: String,
    pub hlsl_file: String,
    pub shader_model: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Resources {
    pub textures: Vec<TextureBinding>,
    pub samplers: Vec<SamplerBinding>,
    pub constant_buffers: Vec<BufferBinding>,
    pub storage_buffers: Vec<BufferBinding>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TextureBinding {
    pub name: String,
    pub hlsl_type: String,
    pub register: u32,
    pub space: u32,
    pub group: u32,
    pub binding: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct SamplerBinding {
    pub name: String,
    pub register: u32,
    pub space: u32,
    pub group: u32,
    pub binding: u32,
    pub comparison: bool,
    pub texture: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BufferBinding {
    pub name: String,
    pub hlsl_type: String,
    pub register: u32,
    pub space: u32,
    pub group: u32,
    pub binding: u32,
    pub members: Vec<BufferMember>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BufferMember {
    pub name: String,
    pub hlsl_type: String,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct StageIo {
    pub inputs: Vec<Io>,
    pub outputs: Vec<Io>,
    pub builtins: Vec<BuiltinIo>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Io {
    pub name: String,
    pub location: u32,
    pub hlsl_type: String,
    pub interpolation: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BuiltinIo {
    pub name: String,
    pub builtin: String,
    pub hlsl_type: String,
}

#[derive(Clone, Debug)]
pub struct ManifestOptions {
    pub shader_name: String,
    pub stage: String,
    pub entry: String,
    pub hlsl_file: String,
    pub shader_model: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Texture,
    Sampler,
    ConstantBuffer,
    StorageBuffer,
}

#[derive(Clone, Copy, Debug)]
pub struct ResourceRegister {
    pub handle: Handle<naga::GlobalVariable>,
    pub kind: ResourceKind,
    pub register: u32,
    pub space: u32,
}

pub fn build(
    module: &Module,
    entry: &EntryPoint,
    resources: &[ResourceRegister],
    dialect: &DialectInfo,
    options: ManifestOptions,
    warnings: Vec<String>,
) -> Manifest {
    let mut manifest = Manifest {
        schema: SCHEMA.to_owned(),
        shader: ShaderInfo {
            name: options.shader_name,
            stage: options.stage,
            entry: options.entry,
            hlsl_file: options.hlsl_file,
            shader_model: options.shader_model,
        },
        resources: Resources {
            textures: Vec::new(),
            samplers: Vec::new(),
            constant_buffers: Vec::new(),
            storage_buffers: Vec::new(),
        },
        stage_io: collect_stage_io(module, entry),
        warnings,
    };

    for resource in resources {
        let variable = &module.global_variables[resource.handle];
        let Some(binding) = variable.binding else {
            continue;
        };
        let name = variable
            .name
            .clone()
            .unwrap_or_else(|| default_name(module, variable.ty, resource.kind));

        match resource.kind {
            ResourceKind::Texture => manifest.resources.textures.push(TextureBinding {
                name,
                hlsl_type: image_hlsl_type(module, variable.ty),
                register: resource.register,
                space: resource.space,
                group: binding.group,
                binding: binding.binding,
            }),
            ResourceKind::Sampler => {
                let comparison = matches!(
                    module.types[variable.ty].inner,
                    TypeInner::Sampler { comparison: true }
                );
                let texture = dialect
                    .combined_samplers
                    .iter()
                    .find(|sampler| sampler.sampler == name)
                    .map(|sampler| sampler.texture.clone());
                manifest.resources.samplers.push(SamplerBinding {
                    name,
                    register: resource.register,
                    space: resource.space,
                    group: binding.group,
                    binding: binding.binding,
                    comparison,
                    texture,
                });
            }
            ResourceKind::ConstantBuffer | ResourceKind::StorageBuffer => {
                let buffer = BufferBinding {
                    name,
                    hlsl_type: type_name(module, variable.ty),
                    register: resource.register,
                    space: resource.space,
                    group: binding.group,
                    binding: binding.binding,
                    members: buffer_members(module, variable.ty),
                };
                match resource.kind {
                    ResourceKind::ConstantBuffer => {
                        manifest.resources.constant_buffers.push(buffer)
                    }
                    _ => manifest.resources.storage_buffers.push(buffer),
                }
            }
        }
    }

    manifest
}

fn collect_stage_io(module: &Module, entry: &EntryPoint) -> StageIo {
    let mut io = StageIo::default();

    for argument in &entry.function.arguments {
        let Some(binding) = &argument.binding else {
            continue;
        };
        let name = argument
            .name
            .clone()
            .unwrap_or_else(|| format!("arg{}", io.inputs.len() + io.builtins.len()));
        match binding {
            Binding::BuiltIn(builtin) => io.builtins.push(BuiltinIo {
                name,
                builtin: format!("{builtin:?}"),
                hlsl_type: type_name(module, argument.ty),
            }),
            Binding::Location {
                location,
                interpolation,
                ..
            } => io.inputs.push(Io {
                name,
                location: *location,
                hlsl_type: type_name(module, argument.ty),
                interpolation: interpolation.map(|value| format!("{value:?}")),
            }),
        }
    }

    if let Some(result) = &entry.function.result {
        push_output(
            module,
            &mut io,
            &result.ty,
            result.binding.as_ref(),
            "result",
        );
    }
    io
}

fn push_output(
    module: &Module,
    io: &mut StageIo,
    ty: &Handle<Type>,
    binding: Option<&Binding>,
    name: &str,
) {
    if let Some(binding) = binding {
        match binding {
            Binding::BuiltIn(builtin) => io.builtins.push(BuiltinIo {
                name: name.to_owned(),
                builtin: format!("{builtin:?}"),
                hlsl_type: type_name(module, *ty),
            }),
            Binding::Location {
                location,
                interpolation,
                ..
            } => io.outputs.push(Io {
                name: name.to_owned(),
                location: *location,
                hlsl_type: type_name(module, *ty),
                interpolation: interpolation.map(|value| format!("{value:?}")),
            }),
        }
        return;
    }

    let TypeInner::Struct { members, .. } = &module.types[*ty].inner else {
        return;
    };
    for (index, member) in members.iter().enumerate() {
        let member_name = member
            .name
            .clone()
            .unwrap_or_else(|| format!("member{index}"));
        push_output(
            module,
            io,
            &member.ty,
            member.binding.as_ref(),
            &member_name,
        );
    }
}

fn buffer_members(module: &Module, ty: Handle<Type>) -> Vec<BufferMember> {
    let TypeInner::Struct { members, .. } = &module.types[ty].inner else {
        return Vec::new();
    };
    members
        .iter()
        .enumerate()
        .map(|(index, member)| BufferMember {
            name: member
                .name
                .clone()
                .unwrap_or_else(|| format!("member{index}")),
            hlsl_type: type_name(module, member.ty),
        })
        .collect()
}

fn default_name(module: &Module, ty: Handle<Type>, kind: ResourceKind) -> String {
    let base = match kind {
        ResourceKind::Texture => "texture",
        ResourceKind::Sampler => "sampler",
        ResourceKind::ConstantBuffer => "constants",
        ResourceKind::StorageBuffer => "storage",
    };
    format!("{base}_{}", type_name(module, ty))
}

pub fn image_hlsl_type(module: &Module, ty: Handle<Type>) -> String {
    let TypeInner::Image {
        dim,
        arrayed,
        class,
    } = &module.types[ty].inner
    else {
        return "Texture2D<float4>".to_owned();
    };
    let base = match class {
        ImageClass::Sampled {
            kind: naga::ScalarKind::Float,
            ..
        } => "float4",
        ImageClass::Sampled {
            kind: naga::ScalarKind::Sint,
            ..
        } => "int4",
        ImageClass::Sampled {
            kind: naga::ScalarKind::Uint,
            ..
        } => "uint4",
        ImageClass::Depth { .. } => "float",
        _ => "float4",
    };
    let prefix = match dim {
        naga::ImageDimension::D1 => "Texture1D",
        naga::ImageDimension::D2 => "Texture2D",
        naga::ImageDimension::D3 => "Texture3D",
        naga::ImageDimension::Cube => "TextureCube",
    };
    if *arrayed {
        format!("{prefix}Array<{base}>")
    } else {
        format!("{prefix}<{base}>")
    }
}

pub fn type_name(module: &Module, ty: Handle<Type>) -> String {
    match &module.types[ty].inner {
        TypeInner::Scalar(scalar) => scalar_name(*scalar).to_owned(),
        TypeInner::Vector { size, scalar } => format!("{}{}", scalar_name(*scalar), *size as u8),
        TypeInner::Matrix {
            columns,
            rows,
            scalar,
        } => {
            format!("{}{}x{}", scalar_name(*scalar), *columns as u8, *rows as u8)
        }
        TypeInner::Atomic(scalar) => scalar_name(*scalar).to_owned(),
        TypeInner::Array { base, size, .. } => {
            format!("{}[{}]", type_name(module, *base), array_len(size))
        }
        TypeInner::Struct { .. } => module.types[ty]
            .name
            .clone()
            .unwrap_or_else(|| "unknown_struct".to_owned()),
        TypeInner::Image { .. } => image_hlsl_type(module, ty),
        TypeInner::Sampler { comparison } => {
            if *comparison {
                "SamplerComparisonState".to_owned()
            } else {
                "SamplerState".to_owned()
            }
        }
        _ => "unknown".to_owned(),
    }
}

fn array_len(size: &ArraySize) -> String {
    match size {
        ArraySize::Constant(value) => value.to_string(),
        ArraySize::Pending(_) | ArraySize::Dynamic => String::new(),
    }
}

fn scalar_name(scalar: Scalar) -> &'static str {
    match (scalar.kind, scalar.width) {
        (naga::ScalarKind::Float, 2) => "half",
        (naga::ScalarKind::Float, _) => "float",
        (naga::ScalarKind::Sint, _) => "int",
        (naga::ScalarKind::Uint, _) => "uint",
        (naga::ScalarKind::Bool, _) => "bool",
        (naga::ScalarKind::AbstractInt, _) => "int",
        (naga::ScalarKind::AbstractFloat, _) => "float",
    }
}

/// JSON is the on-disk form; callers decide where it goes.
pub fn to_json(manifest: &Manifest) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(manifest)
}

/// Convenience for tests and diagnostics: the register/binding pair as text.
pub fn binding_label(binding: &ResourceBinding) -> String {
    format!("group{}binding{}", binding.group, binding.binding)
}
