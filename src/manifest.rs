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
    /// Render target slots and the colortex they map to, when the shader writes `gl_FragData`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mrt: Option<MrtInfo>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MrtInfo {
    pub targets: Vec<u32>,
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
    /// Storage images (`imageStore`/`imageLoad` targets); the host binds them as UAVs.
    pub storage_images: Vec<StorageImageBinding>,
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
    /// Byte size of the HLSL constant buffer; zero for storage buffers.
    pub size: u32,
    pub members: Vec<BufferMember>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BufferMember {
    pub name: String,
    pub hlsl_type: String,
    /// Byte offset inside the constant buffer, following HLSL packing.
    pub offset: u32,
    pub size: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct StorageImageBinding {
    pub name: String,
    /// `RWTexture…<…>` spelling naga emitted, informational for the host.
    pub hlsl_type: String,
    pub register: u32,
    pub space: u32,
    pub group: u32,
    pub binding: u32,
    /// GLSL image format (`rgba16f`); it decides the HLSL element type.
    pub format: String,
    /// `LOAD`, `STORE` or `LOAD | STORE`.
    pub access: String,
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
    StorageImage,
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
            storage_images: Vec::new(),
        },
        stage_io: collect_stage_io(module, entry),
        mrt: dialect.mrt_targets.as_ref().map(|targets| MrtInfo {
            targets: targets.clone(),
        }),
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
            ResourceKind::StorageImage => {
                let (format, access) = match module.types[variable.ty].inner {
                    TypeInner::Image {
                        class: ImageClass::Storage { format, access },
                        ..
                    } => (format!("{format:?}"), format!("{access:?}")),
                    _ => ("unknown".to_owned(), "unknown".to_owned()),
                };
                manifest.resources.storage_images.push(StorageImageBinding {
                    hlsl_type: storage_image_hlsl_type(module, variable.ty),
                    name,
                    register: resource.register,
                    space: resource.space,
                    group: binding.group,
                    binding: binding.binding,
                    format,
                    access,
                });
            }
            ResourceKind::ConstantBuffer | ResourceKind::StorageBuffer => {
                let hlsl_type = match resource.kind {
                    ResourceKind::ConstantBuffer => type_name(module, variable.ty),
                    // Lowered buffer textures are read-only storage buffers, which
                    // naga emits as a single ByteAddressBuffer.
                    ResourceKind::StorageBuffer => "ByteAddressBuffer".to_owned(),
                    _ => unreachable!("guarded by the outer match"),
                };
                let (size, members) = match resource.kind {
                    ResourceKind::ConstantBuffer => constant_buffer_layout(module, variable.ty),
                    // Storage buffer layout follows the struct's std430 rules, not
                    // HLSL constant buffer packing, so it stays out of the manifest.
                    _ => (0, Vec::new()),
                };
                // Vanilla binds uniform buffers by the GLSL block name (`LightmapInfo`), while
                // naga names the global after the instance (`lightmapInfo`). Hosts strip the
                // `constants_` prefix to get the block name, so report the structure name here.
                let name = match resource.kind {
                    ResourceKind::ConstantBuffer => {
                        default_name(module, variable.ty, resource.kind)
                    }
                    _ => name,
                };
                let buffer = BufferBinding {
                    name,
                    hlsl_type,
                    register: resource.register,
                    space: resource.space,
                    group: binding.group,
                    binding: binding.binding,
                    size,
                    members,
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

/// HLSL constant buffer packing for one value: its size and its alignment.
struct HlslField {
    size: u32,
    align: u32,
}

fn constant_buffer_layout(module: &Module, ty: Handle<Type>) -> (u32, Vec<BufferMember>) {
    let TypeInner::Struct { members, .. } = &module.types[ty].inner else {
        return (0, Vec::new());
    };
    let Some((size, fields)) = hlsl_struct_layout(module, members) else {
        return (0, Vec::new());
    };
    // The packed size of the declared members. naga may append `_end_pad` members to the
    // HLSL struct, so DXC can allocate more; the manifest value is a lower bound.
    let members = members
        .iter()
        .zip(fields)
        .enumerate()
        .map(|(index, (member, (offset, size)))| BufferMember {
            name: member
                .name
                .clone()
                .unwrap_or_else(|| format!("member{index}")),
            hlsl_type: type_name(module, member.ty),
            offset,
            size,
        })
        .collect();
    (size, members)
}

fn hlsl_struct_layout(
    module: &Module,
    members: &[naga::StructMember],
) -> Option<(u32, Vec<(u32, u32)>)> {
    let mut offset = 0u32;
    let mut fields = Vec::with_capacity(members.len());
    for member in members {
        let field = hlsl_field(module, member.ty)?;
        let start = hlsl_offset(offset, field.size, field.align);
        fields.push((start, field.size));
        offset = start.checked_add(field.size)?;
    }
    Some((offset, fields))
}

/// Places a value at `offset`, keeping it inside a 16 byte register.
fn hlsl_offset(offset: u32, size: u32, align: u32) -> u32 {
    let mut offset = offset.div_ceil(align) * align;
    if size < 16 {
        let slot = offset % 16;
        if slot + size > 16 {
            offset = offset.div_ceil(16) * 16;
        }
    }
    offset
}

fn hlsl_field(module: &Module, ty: Handle<Type>) -> Option<HlslField> {
    match &module.types[ty].inner {
        TypeInner::Scalar(scalar) | TypeInner::Atomic(scalar) => {
            let size = scalar.width.max(4) as u32;
            Some(HlslField { size, align: size })
        }
        TypeInner::Vector { size, scalar } => {
            let component = scalar.width.max(4) as u32;
            let size = component * *size as u32;
            Some(HlslField {
                size,
                // Two and four component vectors align to their own size; a three component
                // vector aligns like a scalar, which is what DXC's cbuffer packing does.
                align: if size == 12 { component } else { size.min(16) },
            })
        }
        TypeInner::Matrix { columns, .. } => Some(HlslField {
            size: 16 * *columns as u32,
            align: 16,
        }),
        TypeInner::Array { base, size, .. } => {
            let element = hlsl_field(module, *base)?;
            let stride = element.size.max(1).div_ceil(16) * 16;
            let count = match size {
                ArraySize::Constant(value) => value.get(),
                ArraySize::Pending(_) | ArraySize::Dynamic => return None,
            };
            Some(HlslField {
                size: stride.checked_mul(count)?,
                align: 16,
            })
        }
        TypeInner::Struct { members, .. } => {
            let (size, _) = hlsl_struct_layout(module, members)?;
            Some(HlslField { size, align: 16 })
        }
        _ => None,
    }
}

fn default_name(module: &Module, ty: Handle<Type>, kind: ResourceKind) -> String {
    let base = match kind {
        ResourceKind::Texture => "texture",
        ResourceKind::Sampler => "sampler",
        ResourceKind::ConstantBuffer => "constants",
        ResourceKind::StorageBuffer => "storage",
        ResourceKind::StorageImage => "image",
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
    let multisampled = matches!(class, ImageClass::Sampled { multi: true, .. })
        && matches!(dim, naga::ImageDimension::D2);
    let prefix = match dim {
        naga::ImageDimension::D1 => "Texture1D",
        naga::ImageDimension::D2 if multisampled => "Texture2DMS",
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

/// `RWTexture…<…>` spelling for a storage image, mirroring naga's HLSL backend.
fn storage_image_hlsl_type(module: &Module, ty: Handle<Type>) -> String {
    let TypeInner::Image {
        dim,
        arrayed,
        class,
    } = &module.types[ty].inner
    else {
        return "RWTexture2D<float4>".to_owned();
    };
    let ImageClass::Storage { format, .. } = class else {
        return "RWTexture2D<float4>".to_owned();
    };
    let element = match format {
        naga::StorageFormat::R16Float | naga::StorageFormat::R32Float => "float",
        naga::StorageFormat::R8Unorm | naga::StorageFormat::R16Unorm => "unorm float",
        naga::StorageFormat::R8Snorm | naga::StorageFormat::R16Snorm => "snorm float",
        naga::StorageFormat::R8Uint | naga::StorageFormat::R16Uint => "uint",
        naga::StorageFormat::R32Uint | naga::StorageFormat::R64Uint => "uint",
        naga::StorageFormat::R8Sint
        | naga::StorageFormat::R16Sint
        | naga::StorageFormat::R32Sint => "int",
        naga::StorageFormat::Rg16Float
        | naga::StorageFormat::Rg32Float
        | naga::StorageFormat::Rg11b10Ufloat
        | naga::StorageFormat::Rgba16Float
        | naga::StorageFormat::Rgba32Float => "float4",
        naga::StorageFormat::Rg8Unorm
        | naga::StorageFormat::Rg16Unorm
        | naga::StorageFormat::Rgba8Unorm
        | naga::StorageFormat::Bgra8Unorm
        | naga::StorageFormat::Rgba16Unorm
        | naga::StorageFormat::Rgb10a2Unorm => "unorm float4",
        naga::StorageFormat::Rg8Snorm
        | naga::StorageFormat::Rg16Snorm
        | naga::StorageFormat::Rgba8Snorm
        | naga::StorageFormat::Rgba16Snorm => "snorm float4",
        naga::StorageFormat::Rg8Uint
        | naga::StorageFormat::Rg16Uint
        | naga::StorageFormat::Rgba8Uint
        | naga::StorageFormat::Rgba16Uint
        | naga::StorageFormat::Rgba32Uint
        | naga::StorageFormat::Rgb10a2Uint => "uint4",
        naga::StorageFormat::Rg8Sint
        | naga::StorageFormat::Rg16Sint
        | naga::StorageFormat::Rgba8Sint
        | naga::StorageFormat::Rgba16Sint
        | naga::StorageFormat::Rgba32Sint => "int4",
        naga::StorageFormat::Rg32Uint => "int4",
        naga::StorageFormat::Rg32Sint => "uint4",
    };
    let prefix = match dim {
        naga::ImageDimension::D1 => "RWTexture1D",
        naga::ImageDimension::D2 => "RWTexture2D",
        naga::ImageDimension::D3 => "RWTexture3D",
        naga::ImageDimension::Cube => "RWTextureCube",
    };
    if *arrayed {
        format!("{prefix}Array<{element}>")
    } else {
        format!("{prefix}<{element}>")
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

#[cfg(test)]
mod tests {
    use super::*;
    use naga::{ImageClass, ImageDimension, ScalarKind, TypeInner};

    fn texture(dim: ImageDimension, arrayed: bool, class: ImageClass) -> String {
        let mut module = Module::default();
        let ty = module.types.insert(
            naga::Type {
                name: None,
                inner: TypeInner::Image {
                    dim,
                    arrayed,
                    class,
                },
            },
            naga::Span::UNDEFINED,
        );
        image_hlsl_type(&module, ty)
    }

    fn keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn manifest_keys_match_the_documented_schema() {
        use naga::ShaderStage;

        let source = r#"#version 330
/* DRAWBUFFERS:03 */
in vec2 texCoord;
uniform sampler2D ColorTex;
uniform sampler2DShadow ShadowTex;
uniform isamplerBuffer Tint;
uniform vec4 Loose;
layout(std140) uniform Globals { vec4 A; float B; };
void main() {
    gl_FragData[0] = texture(ColorTex, texCoord) * Loose;
    gl_FragData[1] = vec4(float(texture(ShadowTex, vec3(texCoord, 0.5))));
}"#;
        let options = crate::compile::CompileOptions {
            uniform_block: Some("CommonUniforms".to_owned()),
            ..Default::default()
        };
        let compiled = crate::compile::compile_with_source(
            std::path::Path::new("schema.fsh"),
            Some(source),
            ShaderStage::Fragment,
            &options,
        )
        .expect("the fixture converts");
        let manifest = &compiled.manifest;

        assert_eq!(manifest.schema, SCHEMA);
        assert_eq!(
            keys(&serde_json::to_value(manifest).unwrap()),
            vec![
                "mrt",
                "resources",
                "schema",
                "shader",
                "stage_io",
                "warnings"
            ]
        );
        assert_eq!(
            keys(&serde_json::to_value(&manifest.shader).unwrap()),
            vec!["entry", "hlsl_file", "name", "shader_model", "stage"]
        );
        assert_eq!(
            keys(&serde_json::to_value(&manifest.resources).unwrap()),
            vec![
                "constant_buffers",
                "samplers",
                "storage_buffers",
                "storage_images",
                "textures"
            ]
        );

        let resources = &manifest.resources;
        assert!(!resources.textures.is_empty());
        assert_eq!(
            keys(&serde_json::to_value(&resources.textures[0]).unwrap()),
            vec!["binding", "group", "hlsl_type", "name", "register", "space"]
        );

        let comparison = resources
            .samplers
            .iter()
            .find(|sampler| sampler.comparison)
            .expect("the comparison sampler is listed");
        assert_eq!(comparison.texture.as_deref(), Some("ShadowTex"));
        assert_eq!(
            keys(&serde_json::to_value(comparison).unwrap()),
            vec![
                "binding",
                "comparison",
                "group",
                "name",
                "register",
                "space",
                "texture"
            ]
        );

        let buffer = resources
            .constant_buffers
            .iter()
            .find(|buffer| !buffer.members.is_empty())
            .expect("a constant buffer with members is listed");
        assert_eq!(
            keys(&serde_json::to_value(buffer).unwrap()),
            vec![
                "binding",
                "group",
                "hlsl_type",
                "members",
                "name",
                "register",
                "size",
                "space",
            ]
        );
        assert_eq!(
            keys(&serde_json::to_value(&buffer.members[0]).unwrap()),
            vec!["hlsl_type", "name", "offset", "size"]
        );
        assert!(!resources.storage_buffers.is_empty());

        assert_eq!(
            keys(&serde_json::to_value(&manifest.stage_io).unwrap()),
            vec!["builtins", "inputs", "outputs"]
        );
        let input = manifest
            .stage_io
            .inputs
            .iter()
            .find(|io| io.name.starts_with("texCoord"))
            .expect("the fragment input is listed");
        assert_eq!(
            keys(&serde_json::to_value(input).unwrap()),
            vec!["hlsl_type", "interpolation", "location", "name"]
        );

        assert_eq!(
            manifest.mrt.as_ref().map(|mrt| mrt.targets.clone()),
            Some(vec![0, 3])
        );
        assert!(
            manifest
                .warnings
                .iter()
                .any(|warning| warning.contains("texel buffer")),
            "{:?}",
            manifest.warnings
        );
    }
    #[test]
    fn names_sampled_texture_shapes() {
        let float = ImageClass::Sampled {
            kind: ScalarKind::Float,
            multi: false,
        };
        assert_eq!(
            texture(ImageDimension::D2, true, float),
            "Texture2DArray<float4>"
        );
        assert_eq!(
            texture(ImageDimension::D3, false, float),
            "Texture3D<float4>"
        );
        assert_eq!(
            texture(
                ImageDimension::D2,
                false,
                ImageClass::Sampled {
                    kind: ScalarKind::Sint,
                    multi: false,
                }
            ),
            "Texture2D<int4>"
        );
    }

    #[test]
    fn names_multisampled_textures() {
        let multi = ImageClass::Sampled {
            kind: ScalarKind::Float,
            multi: true,
        };
        assert_eq!(
            texture(ImageDimension::D2, false, multi),
            "Texture2DMS<float4>"
        );
        assert_eq!(
            texture(ImageDimension::D2, true, multi),
            "Texture2DMSArray<float4>"
        );
    }
}
