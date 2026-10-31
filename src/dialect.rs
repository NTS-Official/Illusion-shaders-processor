//! GLSL dialect normalisation, Minecraft 3.30 -> naga-acceptable Vulkan GLSL.
//!
//! naga's GLSL frontend rejects several constructs the vanilla corpus uses.
//! Every rewrite here is a mechanical, local edit that preserves the shader's
//! semantics; nothing is inferred and no fallback regex post-processing of the
//! emitted HLSL is performed.
//!
//! Rewrites, in order:
//! 1. `gl_VertexIndex` -> `int(gl_VertexIndex)` (naga models it as `u32`).
//! 2. `const int n = <expr>; a[n]` -> `a[(<expr>)]` (array sizes must be constant).
//! 3. `const vec2[] a = vec2[](1, 2);` -> sized array (naga needs a length).
//! 4. `uniform isamplerBuffer X;` -> a read-only storage buffer of 32-bit elements,
//!    and `texelFetch(X, i).r` -> `X.data[(i)]`.
//! 5. `uniform Globals { ... };` -> `layout(binding = N) uniform Globals { ... };`
//! 6. `uniform sampler2D S;` -> `uniform texture2D S;` + `uniform sampler S_sampler;`
//!    with distinct bindings (naga rejects a shared binding).
//! 7. `vec4 f(sampler2D src, ...)` -> `vec4 f(texture2D src, sampler src_sampler, ...)`
//!    plus a matching extra argument at every call site.
//! 8. `texture(S, uv)` -> `texture(sampler2D(S, S_sampler), uv)`.

use std::collections::{BTreeMap, BTreeSet};

use crate::lexer::{self, Kind, Token};

/// Binding floors for the constructs that need one: each pass starts at its own base and then
/// skips past every binding the source already declares, so pack sources with explicit bindings
/// keep them.
const TEXTURE_BINDING_BASE: u32 = 16;
/// Lowered buffer textures become storage buffers; keep their bindings clear of
/// both uniform blocks and split samplers.
const STORAGE_BINDING_BASE: u32 = 48;

/// Storage images keep their own numbering above the buffer textures, because naga requires a
/// binding on every image and the host may bind them as UAVs one day.
const STORAGE_IMAGE_BINDING_BASE: u32 = 64;

/// Highest `binding = N` already written into the token stream.
///
/// Naga rejects two resources that share `(group, binding)`, and a shader pack numbers its own
/// textures and samplers, so a fixed base is not enough: every allocator must start above what the
/// shader (and the earlier passes) already use.
fn highest_binding(tokens: &[Token]) -> Option<u32> {
    let mut highest: Option<u32> = None;
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Ident || token.text != "binding" {
            continue;
        }
        let Some(equals) = lexer::next_significant(tokens, index + 1) else {
            continue;
        };
        if !lexer::is_symbol(&tokens[equals], "=") {
            continue;
        }
        let Some(value) = lexer::next_significant(tokens, equals + 1) else {
            continue;
        };
        if tokens[value].kind != Kind::Number {
            continue;
        }
        if let Ok(number) = tokens[value].text.parse::<u32>() {
            highest = Some(highest.map_or(number, |current: u32| current.max(number)));
        }
    }
    highest
}

/// First binding at or above `base` that no resource in `tokens` uses yet.
fn first_free_binding(tokens: &[Token], base: u32) -> u32 {
    highest_binding(tokens).map_or(base, |highest| base.max(highest + 1))
}

const TEXTURE_FUNCTIONS: &[&str] = &[
    "texture",
    "textureLod",
    "textureGrad",
    "textureBias",
    "textureProj",
    "textureOffset",
    "textureLodOffset",
    "textureGradOffset",
    "textureProjOffset",
    "textureProjLod",
    "textureGather",
];

/// Shape and component kind of a GLSL combined sampler. HLSL spells both out
/// (`Texture2DArray<float4>`, `Texture2D<int4>`), so the dialect has to keep them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageType {
    shape: ImageShape,
    scalar: ImageScalar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageShape {
    Dim1,
    Dim1Array,
    Dim2,
    Dim2Array,
    Dim2Ms,
    Dim2MsArray,
    Dim3,
    Cube,
    CubeArray,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageScalar {
    Float,
    Sint,
    Uint,
    /// A depth-comparison sampler (`sampler2DShadow`): HLSL keeps a `Texture2D<float>` plus a
    /// `SamplerComparisonState`, and every fetch becomes `SampleCmp`.
    Shadow,
}

impl ImageShape {
    fn parse(suffix: &str) -> Option<Self> {
        Some(match suffix {
            "1D" => Self::Dim1,
            "1DArray" => Self::Dim1Array,
            "2D" => Self::Dim2,
            "2DArray" => Self::Dim2Array,
            "2DMS" => Self::Dim2Ms,
            "2DMSArray" => Self::Dim2MsArray,
            "3D" => Self::Dim3,
            "Cube" => Self::Cube,
            "CubeArray" => Self::CubeArray,
            _ => return None,
        })
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Dim1 => "1D",
            Self::Dim1Array => "1DArray",
            Self::Dim2 => "2D",
            Self::Dim2Array => "2DArray",
            Self::Dim2Ms => "2DMS",
            Self::Dim2MsArray => "2DMSArray",
            Self::Dim3 => "3D",
            Self::Cube => "Cube",
            Self::CubeArray => "CubeArray",
        }
    }
}

impl ImageScalar {
    fn texture_prefix(self) -> &'static str {
        match self {
            Self::Float | Self::Shadow => "",
            Self::Sint => "i",
            Self::Uint => "u",
        }
    }

    fn sampler_prefix(self) -> &'static str {
        match self {
            Self::Float | Self::Shadow => "sampler",
            Self::Sint => "isampler",
            Self::Uint => "usampler",
        }
    }
}

impl ImageType {
    /// Parses a GLSL sampler type name (`sampler2DArray`, `isampler3D`, `sampler2DShadow`, ...).
    /// Returns `None` for texel-buffer types (`samplerBuffer`), which the dialect lowers
    /// separately.
    pub fn parse(word: &str) -> Option<Self> {
        if let Some(shadow) = word.strip_prefix("sampler")
            && let Some(suffix) = shadow.strip_suffix("Shadow")
            && let Some(shape) = ImageShape::parse(suffix)
        {
            return Some(Self {
                shape,
                scalar: ImageScalar::Shadow,
            });
        }
        let (scalar, suffix) = if let Some(suffix) = word.strip_prefix("usampler") {
            (ImageScalar::Uint, suffix)
        } else if let Some(suffix) = word.strip_prefix("isampler") {
            (ImageScalar::Sint, suffix)
        } else {
            (ImageScalar::Float, word.strip_prefix("sampler")?)
        };
        Some(Self {
            shape: ImageShape::parse(suffix)?,
            scalar,
        })
    }

    fn texture_type(self) -> String {
        format!(
            "{}texture{}",
            self.scalar.texture_prefix(),
            self.shape.suffix()
        )
    }

    /// The separable sampler declaration that replaces the combined sampler. A comparison
    /// sampler is `samplerShadow`: it carries the comparison flag but no image shape, so the
    /// split pass cannot mistake the replacement for another combined sampler.
    fn sampler_type(self) -> String {
        if self.scalar == ImageScalar::Shadow {
            return "samplerShadow".to_owned();
        }
        "sampler".to_owned()
    }

    fn constructor(self) -> String {
        if self.scalar == ImageScalar::Shadow {
            return format!("sampler{}Shadow", self.shape.suffix());
        }
        format!("{}{}", self.scalar.sampler_prefix(), self.shape.suffix())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CombinedSampler {
    pub texture: String,
    pub sampler: String,
    pub image_type: ImageType,
    pub texture_binding: u32,
    pub sampler_binding: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniformBlock {
    pub name: String,
    pub binding: u32,
}

/// A GLSL buffer texture (`samplerBuffer`) rewritten as a read-only storage
/// buffer, because neither Naga nor HLSL has a type for texel buffers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferTexture {
    /// Name of the original `uniform isamplerBuffer` variable.
    pub uniform: String,
    /// Instance name of the generated `buffer` block.
    pub instance: String,
    /// Block binding, in the storage-buffer range.
    pub binding: u32,
    /// Element type of the generated array (`int`, `uint` or `float`).
    pub element: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct DialectInfo {
    pub combined_samplers: Vec<CombinedSampler>,
    pub uniform_blocks: Vec<UniformBlock>,
    pub buffer_textures: Vec<BufferTexture>,
    /// Slot-to-colortex mapping from a `DRAWBUFFERS` or `RENDERTARGETS` directive; set when the
    /// shader writes `gl_FragData`.
    pub mrt_targets: Option<Vec<u32>>,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported GLSL construct: {0}")]
    Unsupported(String),
    #[error("malformed GLSL: {0}")]
    Malformed(String),
}

pub fn normalize(tokens: Vec<Token>) -> Result<(Vec<Token>, DialectInfo), Error> {
    normalize_with_options(tokens, &[], false, None)
}

/// Like [`normalize`], but numbers the vertex inputs after `attribute_order`.
pub fn normalize_with_order(
    tokens: Vec<Token>,
    attribute_order: &[String],
) -> Result<(Vec<Token>, DialectInfo), Error> {
    normalize_with_options(tokens, attribute_order, false, None)
}

/// Like [`normalize_with_order`], and flips clip-space Y when `flip_y` is set.
pub fn normalize_with_options(
    tokens: Vec<Token>,
    attribute_order: &[String],
    flip_y: bool,
    flip_texcoord: Option<&str>,
) -> Result<(Vec<Token>, DialectInfo), Error> {
    normalize_with_config(
        tokens,
        &NormalizeOptions {
            attribute_order,
            flip_y,
            flip_texcoord,
            uniform_block: None,
            legacy_builtins: false,
        },
    )
}

/// Every input the normaliser needs. Kept as a struct because hosts supply the fields one by one.
#[derive(Clone, Debug, Default)]
pub struct NormalizeOptions<'a> {
    /// Vertex attributes in host layout order; empty keeps each declaration's own order.
    pub attribute_order: &'a [String],
    /// Flip Y in clip space (D3D convention) by rewriting `gl_Position` assignments.
    pub flip_y: bool,
    /// Flip the named fragment input by materialising a flipped local copy.
    pub flip_texcoord: Option<&'a str>,
    /// Collect loose `uniform <type> <name>;` declarations into this `std140` block.
    pub uniform_block: Option<&'a str>,
    /// Rewrite fixed-function GLSL builtins (`gl_Vertex`, `gl_ModelViewMatrix`, ...) that shader
    /// packs still use. Off by default: vanilla shaders never refer to them.
    pub legacy_builtins: bool,
}

pub fn normalize_with_config(
    mut tokens: Vec<Token>,
    options: &NormalizeOptions<'_>,
) -> Result<(Vec<Token>, DialectInfo), Error> {
    let mut info = DialectInfo::default();

    let (tokens_without_point_size, dropped_point_size) = drop_point_size(tokens);
    tokens = tokens_without_point_size;
    if dropped_point_size > 0 {
        info.warnings.push(format!(
            "dropped {dropped_point_size} gl_PointSize assignment(s): D3D12 has no point size"
        ));
    }
    tokens = cast_vertex_index(tokens);
    tokens = inline_const_array_sizes(tokens);
    size_unsized_arrays(&mut tokens)?;
    if options.legacy_builtins {
        rewrite_legacy_builtins(&mut tokens, &mut info)?;
        lower_gl_fog(&mut tokens, &mut info)?;
        inject_compat_uniforms(&mut tokens, &mut info)?;
        inject_compat_attributes(&mut tokens, &mut info)?;
    }
    tokens = assign_io_locations(tokens, options.attribute_order);
    if options.flip_y {
        tokens = flip_clip_space(tokens);
    }
    if let Some(name) = options.flip_texcoord {
        tokens = flip_named_input(tokens, name)?;
    }
    lower_buffer_textures(&mut tokens, &mut info)?;
    rewrite_frag_data(&mut tokens, &mut info)?;
    if options.legacy_builtins {
        lower_bool_uniforms(&mut tokens, &mut info)?;
    }
    if let Some(block_name) = options.uniform_block {
        collect_loose_uniforms(&mut tokens, &mut info, block_name)?;
    }
    add_block_bindings(&mut tokens, &mut info);
    split_combined_samplers(&mut tokens, &mut info)?;
    ensure_image_bindings(&mut tokens, &mut info)?;

    let mut sampler_map: BTreeMap<String, String> = info
        .combined_samplers
        .iter()
        .map(|sampler| (sampler.texture.clone(), sampler.sampler.clone()))
        .collect();
    let mut constructors: BTreeMap<String, String> = info
        .combined_samplers
        .iter()
        .map(|sampler| (sampler.texture.clone(), sampler.image_type.constructor()))
        .collect();
    let mut split_functions = BTreeSet::new();
    split_sampler_parameters(
        &mut tokens,
        &mut sampler_map,
        &mut constructors,
        &mut split_functions,
    )?;

    tokens = lower_legacy_sampling(tokens, &sampler_map, &constructors)?;
    tokens = inject_sampler_arguments(tokens, &sampler_map, &split_functions)?;
    tokens = wrap_texture_calls(tokens, &sampler_map, &constructors)?;
    rewrite_buffer_texture_fetches(&mut tokens, &info.buffer_textures)?;
    lower_modf(&mut tokens)?;

    Ok((tokens, info))
}

fn token(kind: Kind, text: &str, line: u32) -> Token {
    Token::new(kind, text, line)
}

fn space(line: u32) -> Token {
    token(Kind::Space, " ", line)
}

fn symbol(text: &str, line: u32) -> Token {
    token(Kind::Symbol, text, line)
}

fn ident(text: &str, line: u32) -> Token {
    token(Kind::Ident, text, line)
}

fn number(value: u32, line: u32) -> Token {
    token(Kind::Number, &value.to_string(), line)
}

fn find_ident(tokens: &[Token], from: usize, text: &str) -> Option<usize> {
    let mut index = from;
    while index < tokens.len() {
        if lexer::is_ident(&tokens[index], text) {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn matching_close(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, item) in tokens.iter().enumerate().skip(open) {
        if item.kind != Kind::Symbol {
            continue;
        }
        match item.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn matching_open(tokens: &[Token], close: usize) -> Option<usize> {
    let mut depth = 0usize;
    for index in (0..=close).rev() {
        let item = &tokens[index];
        if item.kind != Kind::Symbol {
            continue;
        }
        match item.text.as_str() {
            ")" | "]" | "}" => depth += 1,
            "(" | "[" | "{" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits `[a, b, (c, d)]` into `[[a, b], [(c, d)]]` at depth-zero commas.
fn split_top_level(tokens: &[Token]) -> Vec<&[Token]> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, item) in tokens.iter().enumerate() {
        if item.kind != Kind::Symbol {
            continue;
        }
        match item.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            "," if depth == 0 => {
                parts.push(&tokens[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&tokens[start..]);
    parts
}

fn single_ident(tokens: &[Token]) -> Option<&str> {
    let mut found = None;
    for item in tokens {
        if item.is_trivia() {
            continue;
        }
        if item.kind != Kind::Ident || found.is_some() {
            return None;
        }
        found = Some(item.text.as_str());
    }
    found
}

fn size_unsized_arrays(tokens: &mut Vec<Token>) -> Result<(), Error> {
    let mut index = 0usize;
    while index < tokens.len() {
        if !lexer::is_symbol(&tokens[index], "[") {
            index += 1;
            continue;
        }
        let Some(close) = lexer::next_significant(tokens, index + 1) else {
            break;
        };
        if !lexer::is_symbol(&tokens[close], "]") {
            index += 1;
            continue;
        }
        // Only handle the `T[] name = T[](a, b, ...);` form.
        let Some(name_index) = lexer::next_significant(tokens, close + 1) else {
            break;
        };
        if tokens[name_index].kind != Kind::Ident {
            index = close + 1;
            continue;
        }
        let Some(assign) = lexer::next_significant(tokens, name_index + 1) else {
            break;
        };
        if !lexer::is_symbol(&tokens[assign], "=") {
            index = close + 1;
            continue;
        }
        let Some(rhs_bracket) = lexer::next_significant(tokens, assign + 1).and_then(|next| {
            // Skip the constructor type name, then `[`.
            lexer::next_significant(tokens, next + 1)
        }) else {
            break;
        };
        if !lexer::is_symbol(&tokens[rhs_bracket], "[") {
            index = close + 1;
            continue;
        }
        let Some(rhs_close) = lexer::next_significant(tokens, rhs_bracket + 1) else {
            break;
        };
        if !lexer::is_symbol(&tokens[rhs_close], "]") {
            index = close + 1;
            continue;
        }
        let Some(paren) = lexer::next_significant(tokens, rhs_close + 1) else {
            break;
        };
        if !lexer::is_symbol(&tokens[paren], "(") {
            index = close + 1;
            continue;
        }
        let Some(paren_close) = matching_close(tokens, paren) else {
            return Err(Error::Malformed(
                "unterminated array initialiser".to_owned(),
            ));
        };
        let items = split_top_level(&tokens[paren + 1..paren_close]);
        let count = items
            .iter()
            .filter(|part| single_ident(part).is_some() || part.iter().any(|t| !t.is_trivia()))
            .count();
        if count == 0 {
            return Err(Error::Unsupported(
                "array initialiser without elements".to_owned(),
            ));
        }
        let line = tokens[index].line;
        tokens.splice(close..close, [number(count as u32, line)]);
        let shifted_close = rhs_close + 1;
        tokens.splice(shifted_close..shifted_close, [number(count as u32, line)]);
        index = paren_close + 2;
    }
    Ok(())
}

/// Reports whether the source already declares `uniform <name> { … }`.
fn declares_uniform_block(tokens: &[Token], name: &str) -> bool {
    let mut index = 0usize;
    while index < tokens.len() {
        if !lexer::is_ident(&tokens[index], "uniform") {
            index += 1;
            continue;
        }
        let Some(named) = lexer::next_significant(tokens, index + 1) else {
            return false;
        };
        if !lexer::is_ident(&tokens[named], name) {
            index = named;
            continue;
        }
        let Some(brace) = lexer::next_significant(tokens, named + 1) else {
            return false;
        };
        if lexer::is_symbol(&tokens[brace], "{") {
            return true;
        }
        index = named;
    }
    false
}

/// Rewrites `bool` uniform members into `int` plus a shadowing global.
///
/// naga rejects a `bool` inside a uniform block (validation reports a dangling local variable), and
/// a D3D12 constant buffer cannot hold `bool` anyway. The host fills `0`/`1`; every shader then
/// reads a global `bool name;` that `main` assigns from the integer before anything else runs.
fn lower_bool_uniforms(tokens: &mut Vec<Token>, info: &mut DialectInfo) -> Result<(), Error> {
    let depths = brace_depths(tokens);
    let mut names: Vec<String> = Vec::new();
    let mut index = 0usize;
    while index < tokens.len() {
        if !lexer::is_ident(&tokens[index], "uniform") || depths[index] != 0 {
            index += 1;
            continue;
        }
        let Some(first) = lexer::next_significant(tokens, index + 1) else {
            return Ok(());
        };
        let Some(second) = lexer::next_significant(tokens, first + 1) else {
            return Ok(());
        };
        if lexer::is_ident(&tokens[first], "bool") {
            if tokens[second].kind == Kind::Ident {
                let name = tokens[second].text.clone();
                tokens[first].text = "int".to_owned();
                tokens[second].text = format!("{name}_value");
                names.push(name);
            }
            index = second + 1;
            continue;
        }
        if lexer::is_symbol(&tokens[second], "{") {
            let Some(close) = matching_close(tokens, second) else {
                return Err(Error::Unsupported(
                    "uniform block has no closing brace".to_owned(),
                ));
            };
            let mut cursor = second + 1;
            while cursor < close {
                if !lexer::is_ident(&tokens[cursor], "bool") || depths[cursor] != depths[second] + 1
                {
                    cursor += 1;
                    continue;
                }
                let Some(named) = lexer::next_significant(tokens, cursor + 1) else {
                    return Ok(());
                };
                if tokens[named].kind == Kind::Ident {
                    let name = tokens[named].text.clone();
                    tokens[cursor].text = "int".to_owned();
                    tokens[named].text = format!("{name}_value");
                    names.push(name);
                }
                cursor = named + 1;
            }
            index = close + 1;
            continue;
        }
        index = second + 1;
    }
    if names.is_empty() {
        return Ok(());
    }
    let line = tokens.first().map_or(1, |item| item.line);
    let mut globals: Vec<Token> = Vec::with_capacity(names.len() * 4);
    for name in &names {
        let mut declaration = lexer::tokenize(&format!("bool {name};"));
        for item in &mut declaration {
            item.line = line;
        }
        globals.extend(declaration);
        globals.push(space(line));
    }
    let insert_at = declaration_insertion_point(tokens);
    tokens.splice(insert_at..insert_at, globals);

    let Some(main) = find_ident(tokens, 0, "main") else {
        return Err(Error::Unsupported(
            "a bool uniform needs a `main` to initialise it".to_owned(),
        ));
    };
    let open = lexer::next_significant(tokens, main + 1)
        .ok_or_else(|| Error::Unsupported("`main` is not declared as a function".to_owned()))?;
    let close = matching_close(tokens, open)
        .ok_or_else(|| Error::Unsupported("`main` has no argument list".to_owned()))?;
    let brace = lexer::next_significant(tokens, close + 1)
        .ok_or_else(|| Error::Unsupported("`main` has no body".to_owned()))?;
    if !lexer::is_symbol(&tokens[brace], "{") {
        return Err(Error::Unsupported("`main` has no body".to_owned()));
    }
    let mut assignments: Vec<Token> = Vec::with_capacity(names.len() * 8);
    for name in &names {
        let mut statement = lexer::tokenize(&format!("{name} = {name}_value != 0;"));
        for item in &mut statement {
            item.line = line;
        }
        assignments.extend(statement);
        assignments.push(space(line));
    }
    tokens.splice(brace + 1..brace + 1, assignments);
    info.warnings.push(format!(
        "lowered {} bool uniform(s) to int: D3D12 constant buffers cannot hold bool",
        names.len()
    ));
    Ok(())
}

fn brace_depths(tokens: &[Token]) -> Vec<usize> {
    let mut depths = Vec::with_capacity(tokens.len());
    let mut depth = 0usize;
    for item in tokens {
        if item.kind == Kind::Symbol && item.text == "}" {
            depth = depth.saturating_sub(1);
        }
        depths.push(depth);
        if item.kind == Kind::Symbol && item.text == "{" {
            depth += 1;
        }
    }
    depths
}
/// Moves loose `uniform <type> <name>;` declarations into one `std140` block so naga accepts them.
///
/// naga rejects uniforms that are not part of a block, while shader packs declare hundreds of
/// loose uniforms that the host fills by name. Collecting them keeps the shader compilable, at the
/// price of the host having to fill one block in declaration order (see the contract, §6.1).
/// Rejects image uniforms early: the host bindings carry SRV/CBV slots but no UAV, so a shader
/// that declares an `image*` uniform cannot be bound even if it converts.
/// Numbers every storage image so the host can bind it as a UAV.
///
/// A GLSL storage image is `layout(binding = N, <format>) uniform image…`. Naga needs both
/// qualifiers, and the host binds by the manifest's register, so the dialect owns the binding
/// number: it rewrites the layout to `binding = STORAGE_IMAGE_BINDING_BASE + i` and keeps the
/// shader's format. Shader packs routinely omit the format (OpenGL takes it from the image unit
/// at dispatch), so a missing format becomes `rgba32f` — `float4` in HLSL — with a warning.
fn ensure_image_bindings(tokens: &mut Vec<Token>, info: &mut DialectInfo) -> Result<(), Error> {
    // Collect first, edit afterwards: splicing tokens invalidates the indices the scan walks, and
    // re-scanning the same declaration would give it a second binding number (the two slots then
    // collide in naga's binding check).
    let mut declarations: Vec<(usize, usize, usize)> = Vec::new();
    let mut search = 0;
    while let Some(uniform) = find_ident(tokens, search, "uniform") {
        let mut cursor = uniform + 1;
        while let Some(index) = lexer::next_significant(tokens, cursor) {
            if tokens[index].kind == Kind::Ident
                && matches!(
                    tokens[index].text.as_str(),
                    "writeonly" | "readonly" | "coherent" | "volatile" | "restrict"
                )
            {
                cursor = index + 1;
            } else {
                break;
            }
        }
        let Some(type_index) = lexer::next_significant(tokens, cursor) else {
            break;
        };
        if tokens[type_index].kind != Kind::Ident
            || !is_storage_image_type(&tokens[type_index].text)
        {
            search = uniform + 1;
            continue;
        }
        let Some(name_index) = lexer::next_significant(tokens, type_index + 1) else {
            return Err(Error::Malformed(format!(
                "`{}` is not followed by an image name",
                tokens[type_index].text
            )));
        };
        if tokens[name_index].kind != Kind::Ident {
            return Err(Error::Malformed(format!(
                "`{}` is not followed by an image name",
                tokens[type_index].text
            )));
        }
        declarations.push((uniform, type_index, name_index));
        search = name_index + 1;
    }

    // Edit back to front: a splice at the first declaration would move every later index, but the
    // binding numbers must still follow declaration order.
    let images_base = first_free_binding(tokens, STORAGE_IMAGE_BINDING_BASE);
    let count = declarations.len();
    for (slot, (uniform, type_index, name_index)) in declarations.iter().copied().enumerate().rev()
    {
        debug_assert!(slot < count);
        let binding = images_base + slot as u32;
        let image_type = tokens[type_index].text.clone();
        let line = tokens[name_index].line;
        let name = tokens[name_index].text.clone();
        let start = statement_start(&tokens[..uniform]);
        let mut defaulted_format = false;
        match layout_group(tokens, start, uniform) {
            Some((open, close)) => {
                let has_format = tokens[open + 1..close].iter().any(|token| {
                    token.kind == Kind::Ident
                        && token.text != "binding"
                        && !token.text.starts_with("local_size_")
                });
                let close = if has_format {
                    close
                } else {
                    defaulted_format = true;
                    let insert = vec![
                        ident(storage_image_format(&image_type), line),
                        symbol(",", line),
                        space(line),
                    ];
                    let length = insert.len();
                    tokens.splice(open + 1..open + 1, insert);
                    close + length
                };
                match location_inside_named(tokens, open, close, "binding") {
                    Some(position) => tokens[position] = number(binding, line),
                    None => {
                        let insert = vec![
                            ident("binding", line),
                            space(line),
                            symbol("=", line),
                            space(line),
                            number(binding, line),
                            symbol(",", line),
                            space(line),
                        ];
                        tokens.splice(open + 1..open + 1, insert);
                    }
                }
            }
            None => {
                defaulted_format = true;
                let declaration = lexer::tokenize(&format!(
                    "layout(binding = {binding}, {}) ",
                    storage_image_format(&image_type)
                ));
                tokens.splice(uniform..uniform, declaration);
            }
        }
        let note = if defaulted_format {
            ", and the shader named no format, so it became `rgba32f`"
        } else {
            ""
        };
        info.warnings.push(format!(
            "`{name}` is a storage image of `{image_type}`; it becomes a UAV (`{}`), which the host cannot bind yet{note}",
            storage_image_hlsl_type(&image_type)
        ));
    }
    Ok(())
}

/// Storage image declaration type: `image*`, `uimage*` (unsigned) or `iimage*` (signed).
fn is_storage_image_type(glsl_type: &str) -> bool {
    glsl_type.starts_with("image")
        || glsl_type.starts_with("uimage")
        || glsl_type.starts_with("iimage")
}

/// Best-effort HLSL spelling of a storage image, for the warning text only: naga emits the
/// authoritative type in the HLSL and the manifest.
fn storage_image_hlsl_type(glsl_type: &str) -> String {
    let (stem, element) = match glsl_type.strip_prefix("uimage") {
        Some(rest) => (rest, "uint4"),
        None => match glsl_type.strip_prefix("iimage") {
            Some(rest) => (rest, "int4"),
            None => (
                glsl_type.strip_prefix("image").unwrap_or(glsl_type),
                "float4",
            ),
        },
    };
    let dim = match stem {
        "1D" => "1D",
        "2D" => "2D",
        "3D" => "3D",
        "Cube" => "Cube",
        "2DArray" | "2DMSArray" => "2DArray",
        "1DArray" => "1DArray",
        _ => "2D",
    };
    format!("RWTexture{dim}<{element}>")
}

/// Shader packs call GLSL's `modf(x, out i)`, which naga's frontend does not implement. The dialect
/// provides four `trunc`-based overloads and renames the calls; nothing is injected unless the
/// shader calls `modf`, so every other shader keeps byte-identical output.
const MODF_PRELUDE: &str = "\
vec4 dxr_modf(vec4 x, out vec4 i) { i = trunc(x); return x - i; }
vec3 dxr_modf(vec3 x, out vec3 i) { i = trunc(x); return x - i; }
vec2 dxr_modf(vec2 x, out vec2 i) { i = trunc(x); return x - i; }
float dxr_modf(float x, out float i) { i = trunc(x); return x - i; }
";

fn lower_modf(tokens: &mut Vec<Token>) -> Result<(), Error> {
    let mut calls = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Ident || token.text != "modf" {
            continue;
        }
        let Some(paren) = lexer::next_significant(tokens, index + 1) else {
            continue;
        };
        if lexer::is_symbol(&tokens[paren], "(") {
            calls.push(index);
        }
    }
    if calls.is_empty() {
        return Ok(());
    }
    for index in calls {
        tokens[index].text = "dxr_modf".to_owned();
    }
    let insertion = version_line_end(tokens)
        .ok_or_else(|| Error::Malformed("the source has no `#version` line".to_owned()))?;
    tokens.splice(insertion..insertion, lexer::tokenize(MODF_PRELUDE));
    Ok(())
}

/// Index just past the `#version` line: declarations must follow it.
///
/// The lexer keeps a directive line in one `Directive` token (`#version 450 core`) without its
/// newline, so a snippet spliced right after the token would glue onto the profile name.
fn version_line_end(tokens: &[Token]) -> Option<usize> {
    let version = tokens.iter().position(|token| {
        token.kind == Kind::Directive && token.text.split_whitespace().next() == Some("#version")
    })?;
    let newline = (version..tokens.len()).find(|&index| tokens[index].text.contains('\n'))?;
    Some(newline + 1)
}

/// Default format for an image whose declaration names none: the format must match the sampled
/// type, so `uimage`/`iimage` cannot borrow the float default.
fn storage_image_format(glsl_type: &str) -> &'static str {
    if glsl_type.starts_with("uimage") {
        "rgba32ui"
    } else if glsl_type.starts_with("iimage") {
        "rgba32i"
    } else {
        "rgba32f"
    }
}
fn collect_loose_uniforms(
    tokens: &mut Vec<Token>,
    info: &mut DialectInfo,
    block_name: &str,
) -> Result<(), Error> {
    if block_name.is_empty()
        || !block_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        || block_name.starts_with(|c: char| c.is_ascii_digit())
    {
        return Err(Error::Unsupported(format!(
            "`{block_name}` is not a usable uniform block name"
        )));
    }
    if info
        .uniform_blocks
        .iter()
        .any(|block| block.name == block_name)
        || declares_uniform_block(tokens, block_name)
    {
        return Err(Error::Unsupported(format!(
            "uniform block `{block_name}` is declared twice"
        )));
    }

    let mut spans: Vec<(usize, usize, Vec<Token>, Option<String>)> = Vec::new();
    let mut brace_depth = 0usize;
    let mut index = 0usize;
    while index < tokens.len() {
        if tokens[index].kind == Kind::Symbol {
            match tokens[index].text.as_str() {
                "{" => brace_depth += 1,
                "}" => brace_depth = brace_depth.saturating_sub(1),
                _ => {}
            }
            index += 1;
            continue;
        }
        if brace_depth > 0 || !lexer::is_ident(&tokens[index], "uniform") {
            index += 1;
            continue;
        }
        let Some(type_index) = lexer::next_significant(tokens, index + 1) else {
            break;
        };
        if tokens[type_index].kind != Kind::Ident {
            index += 1;
            continue;
        }
        let type_text = tokens[type_index].text.clone();
        let Some(name_index) = lexer::next_significant(tokens, type_index + 1) else {
            break;
        };
        // A `{` here means this is already a block; skip it.
        if tokens[name_index].kind != Kind::Ident {
            index += 1;
            continue;
        }
        let Some(after_name) = lexer::next_significant(tokens, name_index + 1) else {
            break;
        };
        let mut member_end = name_index;
        let mut cursor = after_name;
        if lexer::is_symbol(&tokens[after_name], "[") {
            let Some(close) = matching_close(tokens, after_name) else {
                return Err(Error::Malformed(
                    "unterminated uniform array declaration".to_owned(),
                ));
            };
            member_end = close;
            cursor = lexer::next_significant(tokens, close + 1).unwrap_or(tokens.len());
        }
        let mut initializer = None;
        let semicolon = if lexer::is_symbol(&tokens[cursor], "=") {
            let Some(end) = statement_end(tokens, cursor) else {
                return Err(Error::Malformed(
                    "unterminated uniform initialiser".to_owned(),
                ));
            };
            initializer = Some(lexer::render(&tokens[cursor + 1..end]).trim().to_owned());
            end
        } else if lexer::is_symbol(&tokens[cursor], ";") {
            cursor
        } else {
            index = cursor.max(index + 1);
            continue;
        };
        if type_text.contains("sampler")
            || type_text.contains("image")
            || type_text.contains("texture")
        {
            index = semicolon + 1;
            continue;
        }
        let member: Vec<Token> = tokens[type_index..=member_end].to_vec();
        spans.push((index, semicolon, member, initializer));
        index = semicolon + 1;
    }

    if spans.is_empty() {
        return Ok(());
    }

    let binding = info.uniform_blocks.len() as u32;
    let line = tokens[spans[0].0].line;
    let newline = || Token::new(Kind::Space, "\n", line);
    let mut block = vec![
        ident("layout", line),
        symbol("(", line),
        ident("std140", line),
        symbol(",", line),
        space(line),
        ident("binding", line),
        space(line),
        symbol("=", line),
        space(line),
        number(binding, line),
        symbol(")", line),
        space(line),
        ident("uniform", line),
        space(line),
        ident(block_name, line),
        space(line),
        symbol("{", line),
        newline(),
    ];
    for (_, _, member, initializer) in &spans {
        block.extend(member.iter().cloned());
        block.push(symbol(";", line));
        block.push(newline());
        if let Some(initializer) = initializer {
            info.warnings.push(format!(
                "dropped uniform initialiser `{initializer}` in block `{block_name}`"
            ));
        }
    }
    block.extend([symbol("}", line), symbol(";", line), newline()]);

    let first = spans[0].0;
    for (start, end, _, _) in spans.iter().rev() {
        tokens.drain(*start..=*end);
    }
    tokens.splice(first..first, block);
    info.uniform_blocks.push(UniformBlock {
        name: block_name.to_owned(),
        binding,
    });
    info.warnings.push(format!(
        "moved {} loose uniform(s) into block `{block_name}`",
        spans.len()
    ));
    Ok(())
}

fn add_block_bindings(tokens: &mut Vec<Token>, info: &mut DialectInfo) {
    let mut search = 0usize;
    while let Some(uniform) = find_ident(tokens, search, "uniform") {
        search = uniform + 1;
        let Some(name_index) = lexer::next_significant(tokens, uniform + 1) else {
            return;
        };
        if tokens[name_index].kind != Kind::Ident {
            continue;
        }
        let Some(brace) = lexer::next_significant(tokens, name_index + 1) else {
            return;
        };
        if !lexer::is_symbol(&tokens[brace], "{") {
            continue;
        }

        let block_name = tokens[name_index].text.clone();
        if info
            .uniform_blocks
            .iter()
            .any(|block| block.name == block_name)
        {
            continue;
        }
        let binding = info.uniform_blocks.len() as u32;
        let line = tokens[uniform].line;

        let layout_parens = lexer::prev_significant(tokens, uniform).and_then(|previous| {
            if !lexer::is_symbol(&tokens[previous], ")") {
                return None;
            }
            let open = matching_open(tokens, previous)?;
            let before = lexer::prev_significant(tokens, open)?;
            lexer::is_ident(&tokens[before], "layout").then_some((open, previous))
        });

        match layout_parens {
            Some((open, close)) => {
                let has_binding =
                    (open + 1..close).any(|index| lexer::is_ident(&tokens[index], "binding"));
                if !has_binding {
                    let suffix = [
                        symbol(",", line),
                        space(line),
                        ident("binding", line),
                        space(line),
                        symbol("=", line),
                        space(line),
                        number(binding, line),
                    ];
                    tokens.splice(close..close, suffix);
                }
            }
            None => {
                let prefix = [
                    ident("layout", line),
                    symbol("(", line),
                    ident("binding", line),
                    space(line),
                    symbol("=", line),
                    space(line),
                    number(binding, line),
                    symbol(")", line),
                    space(line),
                ];
                tokens.splice(uniform..uniform, prefix);
            }
        }
        info.uniform_blocks.push(UniformBlock {
            name: block_name,
            binding,
        });
    }
}

fn split_combined_samplers(tokens: &mut Vec<Token>, info: &mut DialectInfo) -> Result<(), Error> {
    let mut search = 0usize;
    let mut index = 0u32;
    let split_base = first_free_binding(tokens, TEXTURE_BINDING_BASE);
    // Each split replaces one declaration with two, so the token count grows by a bounded amount.
    // Exceeding this budget means a pass is re-splitting its own output; fail instead of looping.
    let budget = tokens.len() * 4 + 4096;
    while let Some(uniform) = find_ident(tokens, search, "uniform") {
        search = uniform + 1;
        let Some(type_index) = lexer::next_significant(tokens, uniform + 1) else {
            return Ok(());
        };
        let image_type = match ImageType::parse(&tokens[type_index].text) {
            Some(image_type) => image_type,
            None if tokens[type_index].text.starts_with("samplerBuffer")
                || tokens[type_index].text.starts_with("isamplerBuffer")
                || tokens[type_index].text.starts_with("usamplerBuffer") =>
            {
                return Err(Error::Unsupported(format!(
                    "buffer texture `{}` was not lowered by `lower_buffer_textures`",
                    tokens[type_index + 1].text
                )));
            }

            None => continue,
        };
        if tokens[type_index].kind != Kind::Ident {
            continue;
        }
        let Some(name_index) = lexer::next_significant(tokens, type_index + 1) else {
            return Ok(());
        };
        if tokens[name_index].kind != Kind::Ident {
            continue;
        }
        let Some(semicolon) = lexer::next_significant(tokens, name_index + 1) else {
            return Ok(());
        };
        if !lexer::is_symbol(&tokens[semicolon], ";") {
            return Err(Error::Unsupported(
                "combined sampler declared with multiple declarators".to_owned(),
            ));
        }

        let name = tokens[name_index].text.clone();
        let sampler_name = format!("{name}_sampler");
        let texture_binding = split_base + 2 * index;
        let sampler_binding = split_base + 2 * index + 1;
        let line = tokens[uniform].line;

        let replacement = [
            ident("layout", line),
            symbol("(", line),
            ident("binding", line),
            space(line),
            symbol("=", line),
            space(line),
            number(texture_binding, line),
            symbol(")", line),
            space(line),
            ident("uniform", line),
            space(line),
            ident(&image_type.texture_type(), line),
            space(line),
            ident(&name, line),
            symbol(";", line),
            space(line),
            ident("layout", line),
            symbol("(", line),
            ident("binding", line),
            space(line),
            symbol("=", line),
            space(line),
            number(sampler_binding, line),
            symbol(")", line),
            space(line),
            ident("uniform", line),
            space(line),
            ident(&image_type.sampler_type(), line),
            space(line),
            ident(&sampler_name, line),
            symbol(";", line),
        ];
        let replacement_len = replacement.len();
        tokens.splice(uniform..=semicolon, replacement);
        if tokens.len() > budget {
            return Err(Error::Unsupported(
                "combined sampler splitting is not converging".to_owned(),
            ));
        }
        info.combined_samplers.push(CombinedSampler {
            texture: name,
            sampler: sampler_name,
            image_type,
            texture_binding,
            sampler_binding,
        });
        index += 1;
        // Continue after the replacement: its own `uniform` keywords must not be re-split.
        search = uniform + replacement_len;
    }
    Ok(())
}

fn split_sampler_parameters(
    tokens: &mut Vec<Token>,
    sampler_map: &mut BTreeMap<String, String>,
    constructors: &mut BTreeMap<String, String>,
    split_functions: &mut BTreeSet<String>,
) -> Result<(), Error> {
    let mut search = 0usize;
    while let Some(candidate) = lexer::next_significant(tokens, search) {
        search = candidate + 1;
        if tokens[candidate].kind != Kind::Ident {
            continue;
        }
        let Some(open) = lexer::next_significant(tokens, candidate + 1) else {
            break;
        };
        if !lexer::is_symbol(&tokens[open], "(") {
            continue;
        }
        let Some(close) = matching_close(tokens, open) else {
            return Err(Error::Malformed("unterminated parameter list".to_owned()));
        };
        let Some(brace) = lexer::next_significant(tokens, close + 1) else {
            break;
        };
        if !lexer::is_symbol(&tokens[brace], "{") {
            continue;
        }

        let line = tokens[open].line;
        let parts = split_top_level(&tokens[open + 1..close]);
        let mut rewritten: Vec<Token> = Vec::new();
        let mut changed = false;

        for part in &parts {
            let part_tokens = trim_leading_trivia(part);
            if part_tokens.is_empty() {
                continue;
            }
            if !rewritten.is_empty() {
                rewritten.push(symbol(",", line));
                rewritten.push(space(line));
            }

            let mut part_tokens = part_tokens.to_vec();
            let sampler_type = part_tokens.iter().position(|item| {
                item.kind == Kind::Ident && ImageType::parse(&item.text).is_some()
            });
            let mut sampler_name = None;

            match sampler_type {
                Some(type_index) => {
                    let image_type = ImageType::parse(&part_tokens[type_index].text)
                        .expect("the position above only matches parseable sampler types");
                    let name = part_tokens
                        .iter()
                        .skip(type_index + 1)
                        .find(|item| item.kind == Kind::Ident)
                        .map(|item| item.text.clone())
                        .ok_or_else(|| {
                            Error::Malformed("sampler parameter without a name".to_owned())
                        })?;
                    let typed = format!("{name}_sampler");
                    part_tokens[type_index] = ident(&image_type.texture_type(), line);
                    sampler_map.insert(name.clone(), typed.clone());
                    constructors.insert(name, image_type.constructor());
                    sampler_name = Some(typed);
                    changed = true;
                }
                None => {
                    if part_tokens
                        .iter()
                        .any(|item| item.kind == Kind::Ident && item.text.contains("sampler"))
                    {
                        return Err(Error::Unsupported(format!(
                            "opaque parameter `{}` is not supported",
                            lexer::render(part).trim()
                        )));
                    }
                }
            }

            rewritten.extend(part_tokens);
            if let Some(name) = sampler_name {
                rewritten.push(symbol(",", line));
                rewritten.push(space(line));
                rewritten.push(ident("sampler", line));
                rewritten.push(space(line));
                rewritten.push(ident(&name, line));
            }
        }

        if changed {
            split_functions.insert(tokens[candidate].text.clone());
            let trailing = tokens
                .get(open + 1..close)
                .and_then(|slice| slice.last())
                .filter(|item| item.kind == Kind::Space)
                .cloned();
            if let Some(trailing) = trailing {
                rewritten.push(trailing);
            }
            tokens.splice(open + 1..close, rewritten);
            search = open + 1;
        }
    }
    Ok(())
}

/// Appends `gl_Position.y = -gl_Position.y;` after every assignment to `gl_Position`. This reaches
/// the D3D clip-space convention in the GLSL we own, so the HLSL text never needs post-processing.
fn flip_clip_space(tokens: Vec<Token>) -> Vec<Token> {
    let mut out = Vec::with_capacity(tokens.len() + 16);
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        out.push(token.clone());
        if lexer::is_ident(token, "gl_Position")
            && lexer::next_significant(&tokens, index + 1)
                .is_some_and(|next| lexer::is_symbol(&tokens[next], "="))
            && let Some(end) = statement_end(&tokens, index + 1)
        {
            for inner in &tokens[index + 1..=end] {
                out.push(inner.clone());
            }
            let line = tokens[end].line;
            out.extend([
                space(line),
                ident("gl_Position", line),
                symbol(".", line),
                ident("y", line),
                space(line),
                symbol("=", line),
                space(line),
                symbol("-", line),
                ident("gl_Position", line),
                symbol(".", line),
                ident("y", line),
                symbol(";", line),
            ]);
            index = end + 1;
            continue;
        }
        index += 1;
    }
    out
}

/// Reads the lightmap coordinate with a flipped `y`, for passes whose old chain patched the
/// fragment shader. Naga rejects writes to `in` variables, so the pass copies the input into a
/// local with a flipped `y` and rewrites every read inside `main` to that local.
fn flip_named_input(tokens: Vec<Token>, name: &str) -> Result<Vec<Token>, Error> {
    let input_type = declared_input_type(&tokens, name).ok_or_else(|| {
        Error::Unsupported(format!(
            "flip texcoord: there is no global input named `{name}`"
        ))
    })?;
    let components = match input_type.as_str() {
        "vec2" => 2,
        "vec3" => 3,
        "vec4" => 4,
        other => {
            return Err(Error::Unsupported(format!(
                "flip texcoord: `{name}` is `{other}`; only vec2, vec3 and vec4 can be flipped"
            )));
        }
    };
    let brace = find_ident(&tokens, 0, "main")
        .and_then(|index| {
            let open = lexer::next_significant(&tokens, index + 1)?;
            if !lexer::is_symbol(&tokens[open], "(") {
                return None;
            }
            let close = matching_close(&tokens, open)?;
            let brace = lexer::next_significant(&tokens, close + 1)?;
            lexer::is_symbol(&tokens[brace], "{").then_some(brace)
        })
        .ok_or_else(|| Error::Unsupported("flip texcoord: cannot find `void main()`".to_owned()))?;
    let body_end = matching_close(&tokens, brace)
        .ok_or_else(|| Error::Unsupported("flip texcoord: main body is not closed".to_owned()))?;

    let local = format!("{name}Flipped");
    let line = tokens[brace].line;
    let mut declaration = vec![
        space(line),
        ident(&input_type, line),
        space(line),
        ident(&local, line),
        space(line),
        symbol("=", line),
        space(line),
        ident(&input_type, line),
        symbol("(", line),
        ident(name, line),
        symbol(".", line),
        ident("x", line),
    ];
    for component in ["y", "z", "w"].into_iter().take(components - 1) {
        declaration.extend([
            symbol(",", line),
            space(line),
            token(Kind::Number, "1.0", line),
            space(line),
            symbol("-", line),
            space(line),
            ident(name, line),
            symbol(".", line),
            ident(component, line),
        ]);
    }
    declaration.extend([symbol(")", line), symbol(";", line)]);

    let mut out = Vec::with_capacity(tokens.len() + declaration.len());
    out.extend_from_slice(&tokens[..=brace]);
    out.extend_from_slice(&declaration);
    for item in &tokens[brace + 1..body_end] {
        if lexer::is_ident(item, name) {
            out.push(ident(&local, item.line));
        } else {
            out.push(item.clone());
        }
    }
    out.extend_from_slice(&tokens[body_end..]);
    Ok(out)
}

/// Type of the global `in` declaration called `name`, e.g. `vec2`.
fn declared_input_type(tokens: &[Token], name: &str) -> Option<String> {
    let mut index = 0;
    while index < tokens.len() {
        if lexer::is_ident(&tokens[index], "in") {
            let ty = lexer::next_significant(tokens, index + 1)?;
            let identifier = lexer::next_significant(tokens, ty + 1)?;
            if lexer::is_ident(&tokens[identifier], name) {
                return Some(tokens[ty].text.clone());
            }
        }
        index += 1;
    }
    None
}
/// Index of the `;` that closes the statement starting at `from`, ignoring nested groups.
fn statement_end(tokens: &[Token], from: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (offset, token) in tokens[from..].iter().enumerate() {
        if token.kind != lexer::Kind::Symbol {
            continue;
        }
        match token.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            ";" if depth == 0 => return Some(from + offset),
            _ => {}
        }
    }
    None
}

/// naga models `gl_VertexIndex` as an unsigned integer, so `int i = gl_VertexIndex & 7;` fails
/// validation with `InvalidStoreTypes`. The cast keeps every use well typed.
fn cast_vertex_index(tokens: Vec<Token>) -> Vec<Token> {
    let mut out = Vec::with_capacity(tokens.len());
    for token in tokens {
        if token.kind == Kind::Ident && token.text == "gl_VertexIndex" {
            let line = token.line;
            out.push(ident("int", line));
            out.push(ident("(", line));
            out.push(token);
            out.push(ident(")", line));
            continue;
        }
        out.push(token);
    }
    out
}

/// Drops `gl_PointSize = …;` statements: D3D12 has no point size, and naga's HLSL backend rejects
/// the builtin outright, which would fail the whole shader for one dead store.
fn drop_point_size(tokens: Vec<Token>) -> (Vec<Token>, usize) {
    let mut out = Vec::with_capacity(tokens.len());
    let mut dropped = 0;
    let mut index = 0;
    while index < tokens.len() {
        let assignment = lexer::is_ident(&tokens[index], "gl_PointSize")
            && lexer::next_significant(&tokens, index + 1)
                .is_some_and(|next| lexer::is_symbol(&tokens[next], "="));
        if assignment && let Some(end) = statement_end(&tokens, index + 1) {
            dropped += 1;
            index = end + 1;
            continue;
        }
        out.push(tokens[index].clone());
        index += 1;
    }
    (out, dropped)
}

/// naga requires array sizes to be constant expressions, so `const int n = 8 / 4; vec4 a[n];`
/// has to become `vec4 a[(8 / 4)];`.
fn inline_const_array_sizes(tokens: Vec<Token>) -> Vec<Token> {
    let mut values: BTreeMap<String, Vec<Token>> = BTreeMap::new();
    let mut index = 0usize;
    while index < tokens.len() {
        if lexer::is_ident(&tokens[index], "const") {
            let type_index = lexer::next_significant(&tokens, index + 1);
            let name_index =
                type_index.and_then(|type_index| lexer::next_significant(&tokens, type_index + 1));
            let equals_index =
                name_index.and_then(|name_index| lexer::next_significant(&tokens, name_index + 1));
            if let (Some(type_index), Some(name_index), Some(equals_index)) =
                (type_index, name_index, equals_index)
            {
                let scalar = lexer::is_ident(&tokens[type_index], "int")
                    || lexer::is_ident(&tokens[type_index], "uint");
                let assign =
                    tokens[equals_index].kind == Kind::Symbol && tokens[equals_index].text == "=";
                if scalar && assign && tokens[name_index].kind == Kind::Ident {
                    let mut depth = 0usize;
                    let mut end_index = None;
                    let mut cursor = equals_index + 1;
                    while cursor < tokens.len() {
                        if tokens[cursor].kind == Kind::Symbol {
                            match tokens[cursor].text.as_str() {
                                "(" | "[" | "{" => depth += 1,
                                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                                ";" if depth == 0 => {
                                    end_index = Some(cursor);
                                    break;
                                }
                                _ => {}
                            }
                        }
                        cursor += 1;
                    }
                    if let Some(end_index) = end_index {
                        let expression = trim_leading_trivia(&tokens[equals_index + 1..end_index]);
                        let expression = trim_trailing_trivia(expression);
                        values.insert(tokens[name_index].text.clone(), expression.to_vec());
                        index = end_index;
                    }
                }
            }
        }
        index += 1;
    }

    if values.is_empty() {
        return tokens;
    }

    let mut out = Vec::with_capacity(tokens.len());
    let mut index = 0usize;
    while index < tokens.len() {
        if tokens[index].kind == Kind::Symbol && tokens[index].text == "[" {
            let name_index = lexer::next_significant(&tokens, index + 1);
            let close_index =
                name_index.and_then(|name_index| lexer::next_significant(&tokens, name_index + 1));
            if let (Some(name_index), Some(close_index)) = (name_index, close_index) {
                let closed =
                    tokens[close_index].kind == Kind::Symbol && tokens[close_index].text == "]";
                if closed
                    && tokens[name_index].kind == Kind::Ident
                    && let Some(expression) = values.get(&tokens[name_index].text)
                {
                    let line = tokens[index].line;
                    out.push(tokens[index].clone());
                    if expression.len() > 1 {
                        out.push(ident("(", line));
                    }
                    out.extend(expression.iter().cloned());
                    if expression.len() > 1 {
                        out.push(ident(")", line));
                    }
                    out.push(tokens[close_index].clone());
                    index = close_index + 1;
                    continue;
                }
            }
        }
        out.push(tokens[index].clone());
        index += 1;
    }
    out
}

fn trim_trailing_trivia(tokens: &[Token]) -> &[Token] {
    let mut end = tokens.len();
    while end > 0 && tokens[end - 1].kind == Kind::Space {
        end -= 1;
    }
    &tokens[..end]
}
fn trim_leading_trivia(tokens: &[Token]) -> &[Token] {
    let mut start = 0usize;
    while start < tokens.len() && tokens[start].is_trivia() {
        start += 1;
    }
    &tokens[start..]
}
fn inject_sampler_arguments(
    tokens: Vec<Token>,
    sampler_map: &BTreeMap<String, String>,
    split_functions: &BTreeSet<String>,
) -> Result<Vec<Token>, Error> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut index = 0usize;
    while index < tokens.len() {
        let item = &tokens[index];
        if item.kind == Kind::Ident
            && split_functions.contains(&item.text)
            && let Some(open) = lexer::next_significant(&tokens, index + 1)
            && lexer::is_symbol(&tokens[open], "(")
        {
            let close = matching_close(&tokens, open)
                .ok_or_else(|| Error::Malformed("unterminated call".to_owned()))?;
            out.extend_from_slice(&tokens[index..=open]);
            let mut argument_start = open + 1;
            let mut depth = 0usize;
            for cursor in open + 1..close {
                let current = &tokens[cursor];
                if current.kind == Kind::Symbol {
                    match current.text.as_str() {
                        "(" | "[" | "{" => depth += 1,
                        ")" | "]" | "}" => depth = depth.saturating_sub(1),
                        "," if depth == 0 => {
                            emit_argument(
                                &mut out,
                                &tokens[argument_start..cursor],
                                sampler_map,
                                current.line,
                            );
                            out.push(current.clone());
                            argument_start = cursor + 1;
                        }
                        _ => {}
                    }
                }
            }
            let line = tokens[close].line;
            emit_argument(&mut out, &tokens[argument_start..close], sampler_map, line);
            index = close;
            continue;
        }
        out.push(item.clone());
        index += 1;
    }
    Ok(out)
}

fn emit_argument(
    out: &mut Vec<Token>,
    argument: &[Token],
    sampler_map: &BTreeMap<String, String>,
    line: u32,
) {
    let sampler = single_ident(argument).and_then(|name| sampler_map.get(name));
    out.extend_from_slice(argument);
    if let Some(sampler) = sampler {
        // Insert before trailing trivia so the comma stays next to the argument.
        let trailing = argument
            .iter()
            .rev()
            .take_while(|token| token.is_trivia())
            .count();
        let insert_at = out.len() - trailing;
        let injected = [symbol(",", line), space(line), ident(sampler, line)];
        out.splice(insert_at..insert_at, injected);
    }
}

fn wrap_texture_calls(
    tokens: Vec<Token>,
    sampler_map: &BTreeMap<String, String>,
    constructors: &BTreeMap<String, String>,
) -> Result<Vec<Token>, Error> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len() + 16);
    let mut index = 0usize;
    while index < tokens.len() {
        let item = &tokens[index];
        if item.kind == Kind::Ident
            && TEXTURE_FUNCTIONS.contains(&item.text.as_str())
            && let Some(open) = lexer::next_significant(&tokens, index + 1)
            && lexer::is_symbol(&tokens[open], "(")
        {
            let close = matching_close(&tokens, open)
                .ok_or_else(|| Error::Malformed("unterminated texture call".to_owned()))?;
            // First argument span.
            let mut depth = 0usize;
            let mut first_end = close;
            for (offset, current) in tokens[open + 1..close].iter().enumerate() {
                if current.kind != Kind::Symbol {
                    continue;
                }
                match current.text.as_str() {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth = depth.saturating_sub(1),
                    "," if depth == 0 => {
                        first_end = open + 1 + offset;
                        break;
                    }
                    _ => {}
                }
            }
            let first = &tokens[open + 1..first_end];
            if let Some((name, sampler)) = single_ident(first)
                .and_then(|name| sampler_map.get(name).map(|sampler| (name, sampler)))
            {
                out.extend_from_slice(&tokens[index..=open]);
                let line = tokens[open].line;
                let constructor = constructors
                    .get(name)
                    .map(String::as_str)
                    .unwrap_or("sampler2D");
                if item.text != "textureGather"
                    && (constructor.starts_with("isampler") || constructor.starts_with("usampler"))
                {
                    return Err(Error::Unsupported(format!(
                        "`{}` samples the integer image `{name}`, which naga rejects: integer \
                         textures only support `texelFetch`",
                        item.text
                    )));
                }
                out.push(ident(constructor, line));
                out.push(symbol("(", line));
                for token in first {
                    if !token.is_trivia() {
                        out.push(token.clone());
                    }
                }
                out.push(symbol(",", line));
                out.push(space(line));
                out.push(ident(sampler, line));
                out.push(symbol(")", line));
                index = first_end;
                continue;
            }
        }
        out.push(item.clone());
        index += 1;
    }
    Ok(out)
}

/// Rewrites the fixed-function era sampling calls that shader packs still use.
///
/// `texture2DLod(x, uv, lod)` becomes `textureLod(sampler2D(x, x_sampler), uv, lod)`, and
/// `shadow2D(map, p)` becomes `vec4(texture(sampler2DShadow(map, map_sampler), p))` so the
/// legacy `.x`/`.z` swizzles keep working (naga has no `shadow2D`).
fn lower_legacy_sampling(
    tokens: Vec<Token>,
    sampler_map: &BTreeMap<String, String>,
    constructors: &BTreeMap<String, String>,
) -> Result<Vec<Token>, Error> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len() + 32);
    let mut index = 0usize;
    while index < tokens.len() {
        let item = &tokens[index];
        let legacy = matches!(
            item.text.as_str(),
            "texture2DLod"
                | "textureCubeLod"
                | "texture2DProjLod"
                | "texture2D"
                | "texture3D"
                | "textureCube"
                | "texture2DProj"
                | "texture2DGrad"
                | "textureCubeGrad"
                | "texture2DProjGrad"
                | "shadow2D"
        );
        if item.kind != Kind::Ident || !legacy {
            out.push(item.clone());
            index += 1;
            continue;
        }
        // `texture2D` is also the type of a split sampler, so only calls are rewritten.
        let Some(open) = lexer::next_significant(&tokens, index + 1)
            .filter(|open| lexer::is_symbol(&tokens[*open], "("))
        else {
            out.push(item.clone());
            index += 1;
            continue;
        };
        let close = matching_close(&tokens, open)
            .ok_or_else(|| Error::Malformed(format!("unterminated `{}` call", item.text)))?;
        let arguments = split_top_level(&tokens[open + 1..close]);
        let Some(texture) = arguments.first().and_then(|first| single_ident(first)) else {
            // Not a plain sampler variable (e.g. a field of a block): leave the call alone.
            out.push(item.clone());
            index += 1;
            continue;
        };
        let sampler = sampler_map
            .get(texture)
            .cloned()
            .unwrap_or_else(|| format!("{texture}_sampler"));
        let constructor = if item.text == "shadow2D" {
            "sampler2DShadow".to_owned()
        } else {
            constructors
                .get(texture)
                .cloned()
                .unwrap_or_else(|| "sampler2D".to_owned())
        };
        let (wrapper, call) = match item.text.as_str() {
            "shadow2D" => (Some("vec4"), "texture"),
            "texture2DLod" | "textureCubeLod" => (None, "textureLod"),
            "texture2DProjLod" => (None, "textureProjLod"),
            "texture2D" | "texture3D" | "textureCube" => (None, "texture"),
            "texture2DProj" => (None, "textureProj"),
            "texture2DGrad" | "textureCubeGrad" => (None, "textureGrad"),
            _ => (None, "textureProjGrad"),
        };
        let line = item.line;
        if let Some(wrapper) = wrapper {
            out.push(ident(wrapper, line));
            out.push(symbol("(", line));
        }
        out.push(ident(call, line));
        out.push(symbol("(", line));
        out.push(ident(&constructor, line));
        out.push(symbol("(", line));
        out.push(ident(texture, line));
        out.push(symbol(",", line));
        out.push(space(line));
        out.push(ident(&sampler, line));
        out.push(symbol(")", line));
        for argument in arguments.iter().skip(1) {
            out.push(symbol(",", line));
            out.push(space(line));
            out.extend(argument.iter().filter(|token| !token.is_trivia()).cloned());
        }
        out.push(symbol(")", line));
        if wrapper.is_some() {
            out.push(symbol(")", line));
        }
        index = close + 1;
    }
    Ok(out)
}

/// Rewrites GLSL buffer textures (`uniform isamplerBuffer CloudFaces;`) as a
/// read-only storage buffer. Neither naga nor HLSL has a texel-buffer type, so
/// the declaration becomes an `int[]` block plus a helper that reads one texel
/// out of the low byte of a 32-bit word. Buffer textures in the corpus are
/// `R8_SINT` views, so a texel is one byte of the same bytes the host binds as a
/// raw view; reading the word as an int would fold four neighbouring texels.
/// The block gets a named instance so the emitted HLSL variable has a
/// predictable name for the host to bind.
fn lower_buffer_textures(tokens: &mut Vec<Token>, info: &mut DialectInfo) -> Result<(), Error> {
    let buffers_base = first_free_binding(tokens, STORAGE_BINDING_BASE);
    let mut search = 0;
    while let Some(uniform) = find_ident(tokens, search, "uniform") {
        let Some(first) = lexer::next_significant(tokens, uniform + 1) else {
            break;
        };
        let type_index = if matches!(tokens[first].text.as_str(), "lowp" | "mediump" | "highp") {
            match lexer::next_significant(tokens, first + 1) {
                Some(index) => index,
                None => break,
            }
        } else {
            first
        };
        let element = match tokens[type_index].text.as_str() {
            "samplerBuffer" => "float",
            "isamplerBuffer" => "int",
            "usamplerBuffer" => "uint",
            _ => {
                search = uniform + 1;
                continue;
            }
        };
        let Some(name_index) = lexer::next_significant(tokens, type_index + 1) else {
            break;
        };
        let Some(semicolon) = lexer::next_significant(tokens, name_index + 1) else {
            break;
        };
        if tokens[name_index].kind != Kind::Ident || !lexer::is_symbol(&tokens[semicolon], ";") {
            return Err(Error::Malformed(format!(
                "`{}` is not declared as a plain `uniform <buffer sampler> <name>;`",
                tokens[name_index].text
            )));
        }
        let line = tokens[name_index].line;
        let uniform_name = tokens[name_index].text.clone();
        if element == "float" {
            return Err(Error::Unsupported(format!(
                "`{uniform_name}` is a float buffer texture; buffer textures are read one byte per texel, which only integer buffer textures define"
            )));
        }
        let block = format!("{uniform_name}Buffer");
        // The instance keeps the uniform's name: naga names the global after the instance, and the
        // manifest and the host bind storage buffers by that name (the host's uniform reflection
        // only knows `X`, so a `X_buf` instance would be unfindable there).
        let instance = uniform_name.clone();
        let binding = buffers_base + info.buffer_textures.len() as u32;
        let declaration = vec![
            ident("layout", line),
            symbol("(", line),
            ident("std430", line),
            symbol(",", line),
            space(line),
            ident("binding", line),
            space(line),
            symbol("=", line),
            space(line),
            number(binding, line),
            symbol(")", line),
            space(line),
            ident("readonly", line),
            space(line),
            ident("buffer", line),
            space(line),
            ident(&block, line),
            space(line),
            symbol("{", line),
            space(line),
            ident(element, line),
            space(line),
            ident("data", line),
            symbol("[", line),
            symbol("]", line),
            symbol(";", line),
            space(line),
            symbol("}", line),
            space(line),
            ident(&instance, line),
            symbol(";", line),
        ];
        let declaration_len = declaration.len();
        tokens.splice(uniform..=semicolon, declaration);
        // The texel value lives in one byte of the word; the helper keeps the index expression
        // evaluated once and sign-extends `int` texels the way `R8_SINT` would.
        let mut helper = lexer::tokenize(&format!(
            "{element} dxr_texel_{uniform_name}(int index) {{ \
             int word = {instance}.data[index >> 2]; \
             int value = (word >> ((index & 3) * 8)) & 0xFF; \
             return value << 24 >> 24; }}"
        ));
        for item in &mut helper {
            item.line = line;
        }
        helper.push(space(line));
        tokens.splice(uniform + declaration_len..uniform + declaration_len, helper);
        info.warnings.push(format!(
            "`{uniform_name}` is a texel buffer of `{element}`; lowered to a read-only storage buffer read one byte per texel"
        ));
        info.buffer_textures.push(BufferTexture {
            uniform: uniform_name,
            instance,
            binding,
            element,
        });
        search = uniform + 1;
    }
    Ok(())
}

/// Rewrites `texelFetch(CloudFaces, index).r` as `dxr_texel_CloudFaces((index))`.
fn rewrite_buffer_texture_fetches(
    tokens: &mut Vec<Token>,
    textures: &[BufferTexture],
) -> Result<(), Error> {
    if textures.is_empty() {
        return Ok(());
    }
    let mut search = 0;
    while let Some(fetch) = find_ident(tokens, search, "texelFetch") {
        let Some(open) = lexer::next_significant(tokens, fetch + 1) else {
            break;
        };
        if !lexer::is_symbol(&tokens[open], "(") {
            search = fetch + 1;
            continue;
        }
        let Some(image_index) = lexer::next_significant(tokens, open + 1) else {
            break;
        };
        let Some(texture) = textures
            .iter()
            .find(|texture| lexer::is_ident(&tokens[image_index], &texture.uniform))
        else {
            search = fetch + 1;
            continue;
        };
        let Some(comma) = lexer::next_significant(tokens, image_index + 1) else {
            break;
        };
        if !lexer::is_symbol(&tokens[comma], ",") {
            return Err(Error::Malformed(format!(
                "texelFetch on `{}` is missing its index argument",
                texture.uniform
            )));
        }
        let Some(close) = matching_close(tokens, open) else {
            return Err(Error::Malformed("unterminated texelFetch".to_owned()));
        };
        if tokens[comma + 1..close]
            .iter()
            .any(|item| lexer::is_symbol(item, ","))
        {
            return Err(Error::Unsupported(format!(
                "texelFetch on buffer texture `{}` takes exactly one index argument",
                texture.uniform
            )));
        }
        let line = tokens[fetch].line;
        let index = trim_trailing_trivia(trim_leading_trivia(&tokens[comma + 1..close])).to_vec();
        let mut replacement = vec![
            ident(&format!("dxr_texel_{}", texture.uniform), line),
            symbol("(", line),
            symbol("(", line),
        ];
        replacement.extend(index);
        replacement.extend([symbol(")", line), symbol(")", line)]);

        let Some(dot) = lexer::next_significant(tokens, close + 1) else {
            return Err(Error::Unsupported(format!(
                "`texelFetch` on buffer texture `{}` must select a component",
                texture.uniform
            )));
        };
        if !lexer::is_symbol(&tokens[dot], ".") {
            return Err(Error::Unsupported(format!(
                "`texelFetch` on buffer texture `{}` must select a component",
                texture.uniform
            )));
        }
        let Some(component) = lexer::next_significant(tokens, dot + 1) else {
            return Err(Error::Malformed("dangling `.` after texelFetch".to_owned()));
        };
        if !matches!(tokens[component].text.as_str(), "r" | "x") {
            return Err(Error::Unsupported(format!(
                "buffer texture `{}` reads `.{}`; only `.r`/`.x` are supported",
                texture.uniform, tokens[component].text
            )));
        }
        tokens.splice(fetch..=component, replacement);
        search = fetch + 1;
    }
    Ok(())
}

/// Rewrite `gl_FragData[i]` into explicit MRT outputs.
///
/// `i` is a slot index, not a colortex number: the slot list comes from the pack's `DRAWBUFFERS` or
/// `RENDERTARGETS` comment directive, and the slot-to-colortex mapping is recorded in the manifest
/// so the host can build the render target views. Both directives may appear; the one written later
/// in the file wins.
fn rewrite_frag_data(tokens: &mut Vec<Token>, info: &mut DialectInfo) -> Result<(), Error> {
    let targets = comment_directive_targets(tokens)?;
    let mut used: BTreeSet<u32> = BTreeSet::new();
    let mut search = 0usize;
    while let Some(index) = find_ident(tokens, search, "gl_FragData") {
        let open = lexer::next_significant(tokens, index + 1).ok_or_else(|| {
            Error::Unsupported("gl_FragData must be indexed with the render target slot".to_owned())
        })?;
        if !lexer::is_symbol(&tokens[open], "[") {
            return Err(Error::Unsupported(
                "gl_FragData must be indexed with the render target slot".to_owned(),
            ));
        }
        let slot_token = lexer::next_significant(tokens, open + 1).ok_or_else(|| {
            Error::Unsupported("gl_FragData[...] needs a constant slot index".to_owned())
        })?;
        let close = lexer::next_significant(tokens, slot_token + 1).ok_or_else(|| {
            Error::Unsupported("gl_FragData[...] needs a constant slot index".to_owned())
        })?;
        if !lexer::is_symbol(&tokens[close], "]") || tokens[slot_token].kind != Kind::Number {
            return Err(Error::Unsupported(
                "gl_FragData[...] needs a constant slot index".to_owned(),
            ));
        }
        let slot: u32 = tokens[slot_token].text.parse().map_err(|_| {
            Error::Unsupported("gl_FragData[...] needs a constant slot index".to_owned())
        })?;
        let Some(targets) = targets.as_ref() else {
            return Err(Error::Unsupported(
                "gl_FragData requires a DRAWBUFFERS or RENDERTARGETS comment directive".to_owned(),
            ));
        };
        if slot as usize >= targets.len() {
            return Err(Error::Unsupported(format!(
                "gl_FragData[{slot}] has no slot: the directive declares {} target(s)",
                targets.len()
            )));
        }
        used.insert(slot);
        let replacement = ident(&format!("iris_FragData{slot}"), tokens[index].line);
        tokens.splice(index..=close, [replacement]);
        search = index + 1;
    }
    if used.is_empty() {
        return Ok(());
    }
    let line = tokens.first().map_or(1, |item| item.line);
    let mut declarations = Vec::with_capacity(used.len() * 12);
    for slot in &used {
        declarations.extend(layout_prefix(*slot, line));
        declarations.push(ident("out", line));
        declarations.push(space(line));
        declarations.push(ident("vec4", line));
        declarations.push(space(line));
        declarations.push(ident(&format!("iris_FragData{slot}"), line));
        declarations.push(symbol(";", line));
        declarations.push(space(line));
    }
    let insert_at = declaration_insertion_point(tokens);
    tokens.splice(insert_at..insert_at, declarations);
    info.mrt_targets = targets;
    Ok(())
}

/// Position right after the `#version` directive, where injected declarations belong.
fn declaration_insertion_point(tokens: &[Token]) -> usize {
    for (index, item) in tokens.iter().enumerate() {
        if item.kind == Kind::Directive && item.text.trim_start().starts_with("#version") {
            let mut cursor = index + 1;
            while cursor < tokens.len() && tokens[cursor].kind == Kind::Space {
                let ends_line = tokens[cursor].text.contains('\n');
                cursor += 1;
                if ends_line {
                    break;
                }
            }
            return cursor;
        }
    }
    0
}

/// Rewrites the fixed-function GLSL builtins a shader pack still writes.
///
/// D3D12 has no fixed-function state, so every value comes from an attribute or uniform the host
/// fills. Off unless asked: vanilla shaders never reference these names.
/// Lowers the fixed-function `gl_Fog` structure (Iris shader packs still read it) to loose
/// uniforms: `gl_Fog.start` becomes `gl_Fog_start` plus a `uniform float gl_Fog_start;`.
fn lower_gl_fog(tokens: &mut Vec<Token>, info: &mut DialectInfo) -> Result<(), Error> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut used: Vec<(&'static str, &'static str)> = Vec::new();
    let mut references = 0usize;
    let mut index = 0usize;
    while index < tokens.len() {
        let item = &tokens[index];
        if item.kind != Kind::Ident || item.text != "gl_Fog" {
            out.push(item.clone());
            index += 1;
            continue;
        }
        let Some(dot) = lexer::next_significant(tokens, index + 1)
            .filter(|dot| lexer::is_symbol(&tokens[*dot], "."))
        else {
            return Err(Error::Unsupported(
                "`gl_Fog` must be used as `gl_Fog.<field>`".to_owned(),
            ));
        };
        let field_index = lexer::next_significant(tokens, dot + 1)
            .ok_or_else(|| Error::Malformed("`gl_Fog.` is missing its field name".to_owned()))?;
        let field = &tokens[field_index];
        let (ty, name) = match field.text.as_str() {
            "color" => ("vec4", "gl_Fog_color"),
            "density" => ("float", "gl_Fog_density"),
            "start" => ("float", "gl_Fog_start"),
            "end" => ("float", "gl_Fog_end"),
            "scale" => ("float", "gl_Fog_scale"),
            other => {
                return Err(Error::Unsupported(format!(
                    "gl_Fog.{other} has no equivalent: only color, density, start, end and scale are defined"
                )));
            }
        };
        if !used.iter().any(|(_, existing)| *existing == name) {
            used.push((ty, name));
        }
        out.push(ident(name, item.line));
        references += 1;
        index = field_index + 1;
    }
    *tokens = out;
    if references == 0 {
        return Ok(());
    }
    let insert_at = declaration_insertion_point(tokens);
    let mut declarations: Vec<Token> = Vec::new();
    for (ty, name) in &used {
        declarations.push(ident("uniform", 1));
        declarations.push(space(1));
        declarations.push(ident(ty, 1));
        declarations.push(space(1));
        declarations.push(ident(name, 1));
        declarations.push(symbol(";", 1));
        declarations.push(Token::new(Kind::Space, "\n", 1));
    }
    tokens.splice(insert_at..insert_at, declarations);
    info.warnings.push(format!(
        "lowered {references} gl_Fog field reference(s) to loose uniforms"
    ));
    Ok(())
}
/// Declares the core-profile compatibility uniforms Iris injects (`projectionMatrix`,
/// `modelViewMatrix`, `normalMatrix`) when a pack uses them without declaring them.
/// Returns true when the identifier at `index` sits in a statement that already carries
/// `qualifier` (`uniform`, `in`, …). Declarations look like `uniform mat4 projectionMatrix;`,
/// so the qualifier is up to a few tokens back rather than immediately before the name.
fn declared_with(tokens: &[Token], index: usize, qualifier: &str) -> bool {
    let mut cursor = index;
    for _ in 0..3 {
        let Some(previous) = lexer::prev_significant(tokens, cursor) else {
            return false;
        };
        let text = tokens[previous].text.as_str();
        if text == qualifier {
            return true;
        }
        if matches!(text, ";" | "{" | "}") {
            return false;
        }
        cursor = previous;
    }
    false
}
fn inject_compat_uniforms(tokens: &mut Vec<Token>, info: &mut DialectInfo) -> Result<(), Error> {
    const NAMES: [(&str, &str); 3] = [
        ("projectionMatrix", "mat4"),
        ("modelViewMatrix", "mat4"),
        ("normalMatrix", "mat3"),
    ];
    let mut missing: Vec<(&str, &str)> = Vec::new();
    for (name, ty) in NAMES {
        let mut used = false;
        let mut declared = false;
        for index in 0..tokens.len() {
            if tokens[index].kind != Kind::Ident || tokens[index].text != name {
                continue;
            }
            used = true;
            if declared_with(tokens, index, "uniform") {
                declared = true;
                break;
            }
        }
        if used && !declared {
            missing.push((name, ty));
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let insert_at = declaration_insertion_point(tokens);
    let mut declarations: Vec<Token> = Vec::new();
    for (name, ty) in &missing {
        declarations.push(ident("uniform", 1));
        declarations.push(space(1));
        declarations.push(ident(ty, 1));
        declarations.push(space(1));
        declarations.push(ident(name, 1));
        declarations.push(symbol(";", 1));
        declarations.push(Token::new(Kind::Space, "\n", 1));
    }
    tokens.splice(insert_at..insert_at, declarations);
    info.warnings.push(format!(
        "declared {} compatibility uniform(s): {}",
        missing.len(),
        missing
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    ));
    Ok(())
}
/// Declares the core-profile vertex attributes Iris injects (`vaPosition`, `vaNormal`,
/// `vaColor`, `vaUV0..2`) when a pack uses them without declaring them.
fn inject_compat_attributes(tokens: &mut Vec<Token>, info: &mut DialectInfo) -> Result<(), Error> {
    const NAMES: [(&str, &str); 6] = [
        ("vaPosition", "vec3"),
        ("vaNormal", "vec3"),
        ("vaColor", "vec4"),
        ("vaUV0", "vec2"),
        ("vaUV1", "ivec2"),
        ("vaUV2", "ivec2"),
    ];
    let mut missing: Vec<(&str, &str)> = Vec::new();
    for (name, ty) in NAMES {
        let mut used = false;
        let mut declared = false;
        for index in 0..tokens.len() {
            if tokens[index].kind != Kind::Ident || tokens[index].text != name {
                continue;
            }
            used = true;
            if declared_with(tokens, index, "in") {
                declared = true;
                break;
            }
        }
        if used && !declared {
            missing.push((name, ty));
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let insert_at = declaration_insertion_point(tokens);
    let mut declarations: Vec<Token> = Vec::new();
    for (name, ty) in &missing {
        declarations.push(ident("in", 1));
        declarations.push(space(1));
        declarations.push(ident(ty, 1));
        declarations.push(space(1));
        declarations.push(ident(name, 1));
        declarations.push(symbol(";", 1));
        declarations.push(Token::new(Kind::Space, "\n", 1));
    }
    tokens.splice(insert_at..insert_at, declarations);
    info.warnings.push(format!(
        "declared {} compatibility attribute(s): {}",
        missing.len(),
        missing
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    ));
    Ok(())
}
fn rewrite_legacy_builtins(tokens: &mut Vec<Token>, info: &mut DialectInfo) -> Result<(), Error> {
    let mut declarations: BTreeSet<&'static str> = BTreeSet::new();
    let mut rewritten = 0usize;
    let mut needs_offset_matrix = false;
    let mut index = 0usize;
    while index < tokens.len() {
        if tokens[index].kind != Kind::Ident {
            index += 1;
            continue;
        }
        let line = tokens[index].line;
        let (consumed, replacement, needed): (usize, String, &'static [&'static str]) = match tokens
            [index]
            .text
            .as_str()
        {
            "gl_Vertex" => (
                1,
                "vec4(iris_Position, 1.0)".to_owned(),
                &["in vec3 iris_Position;"],
            ),
            "gl_Color" => (
                1,
                "(iris_Color * iris_ColorModulator)".to_owned(),
                &["in vec4 iris_Color;", "uniform vec4 iris_ColorModulator;"],
            ),
            "gl_VertexID" => (1, "gl_VertexIndex".to_owned(), &[]),
            "gl_InstanceID" => (1, "gl_InstanceIndex".to_owned(), &[]),
            "gl_Normal" => (1, "iris_Normal".to_owned(), &["in vec3 iris_Normal;"]),
            "attribute" => (1, "in".to_owned(), &[]),
            "gl_MultiTexCoord0" => (
                1,
                "vec4(iris_UV0, 0.0, 1.0)".to_owned(),
                &["in vec2 iris_UV0;"],
            ),
            "gl_MultiTexCoord1" | "gl_MultiTexCoord2" => (
                1,
                "vec4(iris_UV2, 0.0, 1.0)".to_owned(),
                &["in ivec2 iris_UV2;"],
            ),
            "gl_MultiTexCoord3" => (1, "mc_midTexCoord".to_owned(), &["in vec4 mc_midTexCoord;"]),
            "gl_ModelViewProjectionMatrix" => {
                needs_offset_matrix = true;
                (
                    1,
                    "(iris_ProjMat * iris_ModelViewMat * iris_ModelOffsetMatrix())".to_owned(),
                    &[
                        "uniform mat4 iris_ModelViewMat;",
                        "uniform mat4 iris_ProjMat;",
                        "uniform vec3 iris_ModelOffset;",
                    ],
                )
            }
            "gl_ModelViewMatrix" => {
                needs_offset_matrix = true;
                (
                    1,
                    "iris_ModelViewMat * iris_ModelOffsetMatrix()".to_owned(),
                    &[
                        "uniform mat4 iris_ModelViewMat;",
                        "uniform vec3 iris_ModelOffset;",
                    ],
                )
            }
            "gl_ProjectionMatrix" => (
                1,
                "iris_ProjMat".to_owned(),
                &["uniform mat4 iris_ProjMat;"],
            ),
            "gl_NormalMatrix" => (
                1,
                "iris_NormalMat".to_owned(),
                &["uniform mat3 iris_NormalMat;"],
            ),
            "gl_ModelViewMatrixInverse" => (
                1,
                "iris_ModelViewMatInverse".to_owned(),
                &["uniform mat4 iris_ModelViewMatInverse;"],
            ),
            "gl_ProjectionMatrixInverse" => (
                1,
                "iris_ProjMatInverse".to_owned(),
                &["uniform mat4 iris_ProjMatInverse;"],
            ),
            "gl_TextureMatrix" => texture_matrix_reference(tokens, index)?,
            "ftransform" => {
                let open = lexer::next_significant(tokens, index + 1).ok_or_else(|| {
                    Error::Unsupported("ftransform must be called as ftransform()".to_owned())
                })?;
                let close = lexer::next_significant(tokens, open + 1).ok_or_else(|| {
                    Error::Unsupported("ftransform() has no closing parenthesis".to_owned())
                })?;
                if !lexer::is_symbol(&tokens[open], "(") || !lexer::is_symbol(&tokens[close], ")") {
                    return Err(Error::Unsupported(
                        "ftransform must be called as ftransform()".to_owned(),
                    ));
                }
                needs_offset_matrix = true;
                (
                    close - index + 1,
                    "(iris_ProjMat * iris_ModelViewMat * iris_ModelOffsetMatrix() * vec4(iris_Position, 1.0))"
                        .to_owned(),
                    &[
                        "in vec3 iris_Position;",
                        "uniform mat4 iris_ModelViewMat;",
                        "uniform mat4 iris_ProjMat;",
                        "uniform vec3 iris_ModelOffset;",
                    ],
                )
            }
            _ => {
                index += 1;
                continue;
            }
        };
        let mut replacement_tokens = lexer::tokenize(&replacement);
        for item in &mut replacement_tokens {
            item.line = line;
        }
        tokens.splice(index..index + consumed, replacement_tokens);
        for declaration in needed {
            declarations.insert(declaration);
        }
        rewritten += 1;
        index += 1;
    }
    if declarations.is_empty() {
        return Ok(());
    }
    let line = tokens.first().map_or(1, |item| item.line);
    let mut injected: Vec<Token> = Vec::with_capacity(declarations.len() * 8);
    for declaration in &declarations {
        let mut declaration_tokens = lexer::tokenize(declaration);
        for item in &mut declaration_tokens {
            item.line = line;
        }
        injected.extend(declaration_tokens);
        injected.push(space(line));
    }
    if needs_offset_matrix {
        let mut helper = lexer::tokenize(
            "mat4 iris_ModelOffsetMatrix() { return mat4(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, iris_ModelOffset.x, iris_ModelOffset.y, iris_ModelOffset.z, 1.0); }",
        );
        for item in &mut helper {
            item.line = line;
        }
        injected.extend(helper);
        injected.push(space(line));
    }
    let insert_at = declaration_insertion_point(tokens);
    tokens.splice(insert_at..insert_at, injected);
    if rewritten > 0 {
        info.warnings.push(format!(
            "rewrote {rewritten} legacy GLSL builtin reference(s)"
        ));
    }
    Ok(())
}

/// Expands `gl_TextureMatrix[n]` into the host uniform that carries that layer.
fn texture_matrix_reference(
    tokens: &[Token],
    index: usize,
) -> Result<(usize, String, &'static [&'static str]), Error> {
    let open = lexer::next_significant(tokens, index + 1).ok_or_else(|| {
        Error::Unsupported("gl_TextureMatrix must be indexed with a constant layer".to_owned())
    })?;
    let slot_token = lexer::next_significant(tokens, open + 1).ok_or_else(|| {
        Error::Unsupported("gl_TextureMatrix must be indexed with a constant layer".to_owned())
    })?;
    let close = lexer::next_significant(tokens, slot_token + 1).ok_or_else(|| {
        Error::Unsupported("gl_TextureMatrix must be indexed with a constant layer".to_owned())
    })?;
    if !lexer::is_symbol(&tokens[open], "[")
        || !lexer::is_symbol(&tokens[close], "]")
        || tokens[slot_token].kind != Kind::Number
    {
        return Err(Error::Unsupported(
            "gl_TextureMatrix must be indexed with a constant layer".to_owned(),
        ));
    }
    let layer: u32 = tokens[slot_token].text.parse().map_err(|_| {
        Error::Unsupported("gl_TextureMatrix must be indexed with a constant layer".to_owned())
    })?;
    match layer {
        0 => Ok((
            close - index + 1,
            "iris_TextureMat".to_owned(),
            &["uniform mat4 iris_TextureMat;"],
        )),
        1 => Ok((
            close - index + 1,
            "iris_LightmapTextureMatrix".to_owned(),
            &["uniform mat4 iris_LightmapTextureMatrix;"],
        )),
        other => Err(Error::Unsupported(format!(
            "gl_TextureMatrix[{other}] has no equivalent: only layers 0 and 1 are defined"
        ))),
    }
}

fn comment_directive_targets(tokens: &[Token]) -> Result<Option<Vec<u32>>, Error> {
    let mut chosen: Option<(usize, Vec<u32>)> = None;
    for (index, item) in tokens.iter().enumerate() {
        if item.kind != Kind::Comment {
            continue;
        }
        let Some((kind, value)) = split_comment_directive(&item.text) else {
            continue;
        };
        let slots = match kind {
            "DRAWBUFFERS" => parse_drawbuffers(value)?,
            _ => parse_rendertargets(value)?,
        };
        if slots.is_empty() {
            return Err(Error::Unsupported(format!(
                "{kind} declares no render targets"
            )));
        }
        chosen = Some((index, slots));
    }
    Ok(chosen.map(|(_, slots)| slots))
}

fn split_comment_directive(text: &str) -> Option<(&str, &str)> {
    for kind in ["DRAWBUFFERS", "RENDERTARGETS"] {
        if let Some(start) = text.find(kind) {
            let rest = &text[start + kind.len()..];
            let rest = rest.trim_start();
            let rest = rest.strip_prefix(':')?;
            let end = rest.find("*/").unwrap_or(rest.len());
            return Some((kind, rest[..end].trim()));
        }
    }
    None
}

fn parse_drawbuffers(value: &str) -> Result<Vec<u32>, Error> {
    let mut slots = Vec::new();
    for character in value.chars().filter(|character| !character.is_whitespace()) {
        let Some(digit) = character.to_digit(10) else {
            return Err(Error::Unsupported(format!(
                "DRAWBUFFERS only supports one digit per slot, found `{character}`"
            )));
        };
        slots.push(digit);
    }
    Ok(slots)
}

fn parse_rendertargets(value: &str) -> Result<Vec<u32>, Error> {
    let mut slots = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let slot: u32 = item.parse().map_err(|_| {
            Error::Unsupported(format!("RENDERTARGETS has a non numeric slot `{item}`"))
        })?;
        slots.push(slot);
    }
    Ok(slots)
}

/// Give every global `in`/`out` declaration an explicit location.
///
/// Vanilla 26.3 ships interface variables without `layout(location = ...)` and lets shaderc's
/// `auto_map_locations` number them. Naga has no such mode: every implicit variable lands at
/// location 0, so validation fails with a location collision. Numbering by declaration order also
/// keeps the vertex outputs and the fragment inputs aligned, which is what the host relies on.
///
/// Vertex inputs are the exception: the host builds its input layout from the vertex format, so a
/// location selects bytes from that format and not from the shader's declaration order. When
/// `attribute_order` is non-empty it therefore also overrides the locations the shader declares.
fn assign_io_locations(mut tokens: Vec<Token>, attribute_order: &[String]) -> Vec<Token> {
    tokens = split_io_declarators(tokens);
    let declared = declared_input_names(&tokens);
    let ranked = ranked_locations(&declared, attribute_order);
    let mut used: BTreeSet<u32> = declared_locations(&tokens, true).into_iter().collect();
    let mut used_outputs: BTreeSet<u32> = declared_locations(&tokens, false).into_iter().collect();
    let mut next_input = 0u32;
    let mut outputs = 0u32;
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut index = 0usize;
    while index < tokens.len() {
        let item = tokens[index].clone();
        if item.kind == Kind::Symbol {
            match item.text.as_str() {
                "{" => brace_depth += 1,
                "}" => brace_depth = brace_depth.saturating_sub(1),
                "(" => paren_depth += 1,
                ")" => paren_depth = paren_depth.saturating_sub(1),
                _ => {}
            }
            index += 1;
            continue;
        }
        let global = brace_depth == 0 && paren_depth == 0;
        if !global || item.kind != Kind::Ident || (item.text != "in" && item.text != "out") {
            index += 1;
            continue;
        }
        let is_input = item.text == "in";
        let line = item.line;
        let name = declared_name(&tokens, index).map(str::to_owned);
        let start = statement_start(&tokens[..index]);
        let layout = layout_group(&tokens, start, index);
        // `layout(local_size_x = 8) in;` declares the workgroup size, not a
        // stage interface; appending a location makes naga reject the shader.
        let workgroup = layout.is_some_and(|(open, close)| {
            tokens[open..close]
                .iter()
                .any(|token| token.text.starts_with("local_size_"))
        });
        if workgroup || name.is_none() {
            index += 1;
            continue;
        }
        let declared_location =
            layout.and_then(|(open, close)| location_inside(&tokens, open, close));
        let ranked_location = name.as_deref().and_then(|name| ranked.get(name).copied());
        let location = if is_input {
            ranked_location
                .or_else(|| declared_location.map(|(_, value)| value))
                .unwrap_or_else(|| {
                    let mut candidate = next_input;
                    while used.contains(&candidate) {
                        candidate += 1;
                    }
                    candidate
                })
        } else {
            declared_location
                .map(|(_, value)| value)
                .unwrap_or_else(|| next_free(&used_outputs, outputs))
        };
        match layout {
            Some((open, close)) => match declared_location {
                Some((position, _)) => {
                    tokens[position] = number(location, tokens[position].line);
                }
                None => {
                    let mut insertion = Vec::new();
                    if close > open + 1 {
                        insertion.push(symbol(",", line));
                        insertion.push(space(line));
                    }
                    insertion.extend(layout_item(location, line));
                    tokens.splice(close..close, insertion);
                }
            },
            None => {
                let prefix = layout_prefix(location, line);
                let inserted = prefix.len();
                tokens.splice(index..index, prefix);
                index += inserted;
            }
        }
        if is_input {
            used.insert(location);
            next_input = next_input.max(location + 1);
        } else {
            used_outputs.insert(location);
            outputs = outputs.max(location + 1);
        }
        index += 1;
    }
    tokens
}

fn next_free(used: &BTreeSet<u32>, from: u32) -> u32 {
    let mut candidate = from;
    while used.contains(&candidate) {
        candidate += 1;
    }
    candidate
}

/// Locations the shader already declares on its global `in`/`out` statements.
fn declared_locations(tokens: &[Token], inputs: bool) -> Vec<u32> {
    let mut locations = Vec::new();
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut index = 0usize;
    while index < tokens.len() {
        let item = &tokens[index];
        if item.kind == Kind::Symbol {
            match item.text.as_str() {
                "{" => brace_depth += 1,
                "}" => brace_depth = brace_depth.saturating_sub(1),
                "(" => paren_depth += 1,
                ")" => paren_depth = paren_depth.saturating_sub(1),
                _ => {}
            }
            index += 1;
            continue;
        }
        let want = if inputs { "in" } else { "out" };
        if brace_depth == 0 && paren_depth == 0 && lexer::is_ident(item, want) {
            let start = statement_start(&tokens[..index]);
            if let Some((open, close)) = layout_group(tokens, start, index)
                && let Some((_, value)) = location_inside(tokens, open, close)
            {
                locations.push(value);
            }
        }
        index += 1;
    }
    locations
}

/// Rewrites `flat out vec3 a, b;` into one statement per name so every name gets its own location.
fn split_io_declarators(mut tokens: Vec<Token>) -> Vec<Token> {
    let mut index = 0usize;
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    while index < tokens.len() {
        let item = tokens[index].clone();
        if item.kind == Kind::Symbol {
            match item.text.as_str() {
                "{" => brace_depth += 1,
                "}" => brace_depth = brace_depth.saturating_sub(1),
                "(" => paren_depth += 1,
                ")" => paren_depth = paren_depth.saturating_sub(1),
                _ => {}
            }
            index += 1;
            continue;
        }
        let global = brace_depth == 0 && paren_depth == 0;
        if !global || item.kind != Kind::Ident || (item.text != "in" && item.text != "out") {
            index += 1;
            continue;
        }
        let Some(end) = statement_end(&tokens, index) else {
            index += 1;
            continue;
        };
        let Some((prefix, declarators)) = io_declarators(&tokens, index, end) else {
            index = end + 1;
            continue;
        };
        if declarators.len() < 2 {
            index = end + 1;
            continue;
        }
        let start = statement_start(&tokens[..index]);
        let line = item.line;
        let mut replacement = Vec::new();
        for (name, array) in declarators {
            replacement.extend(prefix.iter().cloned());
            replacement.push(space(line));
            replacement.push(name);
            replacement.extend(array);
            replacement.push(symbol(";", line));
            replacement.push(space(line));
        }
        let inserted = replacement.len();
        tokens.splice(start..=end, replacement);
        index = start + inserted;
    }
    tokens
}

/// One name of an interface declaration plus its array suffix, if any.
type Declarator = (Token, Vec<Token>);

/// Qualifiers and type of an interface declaration, plus its declarators.
fn io_declarators(
    tokens: &[Token],
    keyword: usize,
    end: usize,
) -> Option<(Vec<Token>, Vec<Declarator>)> {
    let mut cursor = keyword + 1;
    loop {
        let next = lexer::next_significant(tokens, cursor)?;
        if next >= end {
            return None;
        }
        if lexer::is_ident(&tokens[next], "layout") {
            let open = (next + 1..end).find(|at| lexer::is_symbol(&tokens[*at], "("))?;
            let close = matching_close(tokens, open)?;
            cursor = close + 1;
            continue;
        }
        if tokens[next].kind == Kind::Ident && is_qualifier_or_precision(&tokens[next].text) {
            cursor = next + 1;
            continue;
        }
        cursor = next;
        break;
    }
    let ty = lexer::next_significant(tokens, cursor)?;
    if ty >= end || tokens[ty].kind != Kind::Ident {
        return None;
    }
    let mut prefix: Vec<Token> = tokens[statement_start(&tokens[..keyword])..=ty].to_vec();
    strip_layout_groups(&mut prefix);
    let mut declarators = Vec::new();
    let mut at = lexer::next_significant(tokens, ty + 1)?;
    loop {
        if at >= end || tokens[at].kind != Kind::Ident {
            return None;
        }
        let name = tokens[at].clone();
        let mut array = Vec::new();
        let mut probe = at + 1;
        if let Some(open) = lexer::next_significant(tokens, probe)
            && open < end
            && lexer::is_symbol(&tokens[open], "[")
            && let Some(close) = matching_close(tokens, open)
        {
            array.extend(tokens[open..=close].iter().cloned());
            probe = close + 1;
        }
        declarators.push((name, array));
        let significant = lexer::next_significant(tokens, probe)?;
        if lexer::is_symbol(&tokens[significant], ";") {
            break;
        }
        if !lexer::is_symbol(&tokens[significant], ",") {
            return None;
        }
        at = lexer::next_significant(tokens, significant + 1)?;
    }
    Some((prefix, declarators))
}

fn strip_layout_groups(tokens: &mut Vec<Token>) {
    let mut index = 0usize;
    while index < tokens.len() {
        if lexer::is_ident(&tokens[index], "layout")
            && let Some(open) =
                (index + 1..tokens.len()).find(|at| lexer::is_symbol(&tokens[*at], "("))
            && let Some(close) = matching_close(tokens, open)
        {
            tokens.drain(index..=close);
            continue;
        }
        index += 1;
    }
}

fn is_qualifier_or_precision(text: &str) -> bool {
    is_qualifier(text) || matches!(text, "invariant" | "precise")
}

fn layout_prefix(location: u32, line: u32) -> Vec<Token> {
    vec![
        ident("layout", line),
        symbol("(", line),
        ident("location", line),
        space(line),
        symbol("=", line),
        space(line),
        number(location, line),
        symbol(")", line),
        space(line),
    ]
}

fn layout_item(location: u32, line: u32) -> Vec<Token> {
    vec![
        ident("location", line),
        space(line),
        symbol("=", line),
        space(line),
        number(location, line),
    ]
}

/// Parentheses of the `layout(...)` qualifier in the statement that ends at `end`, if any.
fn layout_group(tokens: &[Token], start: usize, end: usize) -> Option<(usize, usize)> {
    let mut index = start;
    while index < end {
        if lexer::is_ident(&tokens[index], "layout") {
            let open = (index + 1..end).find(|cursor| lexer::is_symbol(&tokens[*cursor], "("))?;
            let mut depth = 0usize;
            for (offset, token) in tokens[open..end].iter().enumerate() {
                if lexer::is_symbol(token, "(") {
                    depth += 1;
                } else if lexer::is_symbol(token, ")") {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some((open, open + offset));
                    }
                }
            }
            return None;
        }
        index += 1;
    }
    None
}

/// The `location = <n>` pair inside a `layout(...)` group.
fn location_inside(tokens: &[Token], open: usize, close: usize) -> Option<(usize, u32)> {
    let value_index = location_inside_named(tokens, open, close, "location")?;
    tokens[value_index]
        .text
        .parse::<u32>()
        .ok()
        .map(|value| (value_index, value))
}

/// Index of the number that follows `name =` inside a `layout(...)` group.
fn location_inside_named(tokens: &[Token], open: usize, close: usize, name: &str) -> Option<usize> {
    let index = (open..close).find(|cursor| lexer::is_ident(&tokens[*cursor], name))?;
    let equals = (index + 1..close).find(|cursor| lexer::is_symbol(&tokens[*cursor], "="))?;
    (equals + 1..close).find(|cursor| tokens[*cursor].kind == Kind::Number)
}

/// Names of the global `in` declarations, in declaration order.
fn declared_input_names(tokens: &[Token]) -> Vec<String> {
    let mut names = Vec::new();
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    for (index, item) in tokens.iter().enumerate() {
        if item.kind == Kind::Symbol {
            match item.text.as_str() {
                "{" => brace_depth += 1,
                "}" => brace_depth = brace_depth.saturating_sub(1),
                "(" => paren_depth += 1,
                ")" => paren_depth = paren_depth.saturating_sub(1),
                _ => {}
            }
            continue;
        }
        let global = brace_depth == 0 && paren_depth == 0;
        if !global || item.kind != Kind::Ident || item.text != "in" {
            continue;
        }
        if let Some(name) = declared_name(tokens, index) {
            names.push(name.to_owned());
        }
    }
    names
}

/// Rank of every declared input among the attributes the host's vertex format supplies.
fn ranked_locations(declared: &[String], attribute_order: &[String]) -> BTreeMap<String, u32> {
    let mut ranked = BTreeMap::new();
    let mut rank = 0u32;
    for attribute in attribute_order {
        if declared.iter().any(|name| name == attribute) && !ranked.contains_key(attribute) {
            ranked.insert(attribute.clone(), rank);
            rank += 1;
        }
    }
    ranked
}
/// Name declared by the interface declaration that starts at `from`, if it has one.
fn declared_name(tokens: &[Token], from: usize) -> Option<&str> {
    let mut name = None;
    for item in &tokens[from + 1..] {
        if item.kind == Kind::Symbol {
            match item.text.as_str() {
                ";" | "{" | "}" => break,
                _ => {}
            }
        }
        if item.kind == Kind::Ident && !is_qualifier(&item.text) {
            name = Some(item.text.as_str());
        }
    }
    name
}

fn is_qualifier(text: &str) -> bool {
    matches!(
        text,
        "flat"
            | "smooth"
            | "noperspective"
            | "centroid"
            | "sample"
            | "patch"
            | "highp"
            | "mediump"
            | "lowp"
            | "invariant"
            | "precise"
    )
}

/// Index of the first token of the statement that ends at the end of `tokens`.
fn statement_start(tokens: &[Token]) -> usize {
    let mut index = tokens.len();
    while index > 0 {
        let item = &tokens[index - 1];
        if item.kind == Kind::Symbol && matches!(item.text.as_str(), ";" | "{" | "}") {
            return index;
        }
        index -= 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(source: &str) -> String {
        let (tokens, _) = normalize(lexer::tokenize(source)).expect("normalize");
        lexer::render(&tokens)
    }

    fn normalized_with_block(source: &str, block: &str) -> (String, DialectInfo) {
        let (tokens, info) = normalize_with_config(
            lexer::tokenize(source),
            &NormalizeOptions {
                uniform_block: Some(block),
                ..NormalizeOptions::default()
            },
        )
        .expect("normalize");
        (lexer::render(&tokens), info)
    }

    #[test]
    fn collects_loose_uniforms_into_one_block() {
        let (text, info) = normalized_with_block(
            "uniform float far;\nuniform vec4 fogColor = vec4(1.0);\nout vec4 color;\n\
             void main() { color = fogColor * far; }\n",
            "CommonUniforms",
        );
        assert!(
            text.contains("layout(std140, binding = 0) uniform CommonUniforms {"),
            "{text}"
        );
        assert!(text.contains("float far;"), "{text}");
        assert!(text.contains("vec4 fogColor;"), "{text}");
        assert!(!text.contains("uniform vec4 fogColor"), "{text}");
        assert_eq!(info.uniform_blocks.len(), 1);
        assert_eq!(info.uniform_blocks[0].name, "CommonUniforms");
        assert_eq!(info.uniform_blocks[0].binding, 0);
        assert!(
            info.warnings
                .iter()
                .any(|warning| warning.contains("moved 2 loose uniform(s)")),
            "{:?}",
            info.warnings
        );
        assert!(
            info.warnings
                .iter()
                .any(|warning| warning.contains("dropped uniform initialiser")),
            "{:?}",
            info.warnings
        );
    }

    #[test]
    fn leaves_loose_uniforms_alone_without_a_block_name() {
        let text = normalized("uniform float far;\n");
        assert!(text.contains("uniform float far;"), "{text}");
    }

    #[test]
    fn lowers_bool_uniform_members() {
        let mut info = DialectInfo::default();
        let mut tokens = lexer::tokenize(
            "layout(std140, binding = 0) uniform Globals { bool heavyFog; vec4 tint; } common;\n\
             void main() { }\n",
        );
        lower_bool_uniforms(&mut tokens, &mut info).expect("bool member must lower");
        let text = lexer::render(&tokens);
        assert!(text.contains("int heavyFog_value;"), "{text}");
        assert!(text.contains("bool heavyFog;"), "{text}");
        assert!(text.contains("heavyFog = heavyFog_value != 0;"), "{text}");
        assert!(
            info.warnings
                .iter()
                .any(|warning| warning.contains("lowered 1 bool uniform(s)")),
            "{:?}",
            info.warnings
        );
    }

    #[test]
    fn lowers_loose_bool_uniforms() {
        let mut info = DialectInfo::default();
        let mut tokens = lexer::tokenize("uniform bool isElytraFlying;\nvoid main() { }\n");
        lower_bool_uniforms(&mut tokens, &mut info).expect("loose bool must lower");
        let text = lexer::render(&tokens);
        assert!(text.contains("uniform int isElytraFlying_value;"), "{text}");
        assert!(text.contains("bool isElytraFlying;"), "{text}");
        assert!(
            text.contains("isElytraFlying = isElytraFlying_value != 0;"),
            "{text}"
        );
    }

    #[test]
    fn rejects_a_duplicate_uniform_block_name() {
        let error = normalize_with_config(
            lexer::tokenize(
                "layout(std140, binding = 0) uniform CommonUniforms { float far; } common;\n\
                 uniform float near;\n",
            ),
            &NormalizeOptions {
                uniform_block: Some("CommonUniforms"),
                ..NormalizeOptions::default()
            },
        )
        .expect_err("duplicate block name must be rejected");
        assert!(error.to_string().contains("declared twice"), "{error}");
    }

    #[test]
    fn combined_sampler_is_split_with_distinct_bindings() {
        let text = normalized("uniform sampler2D Sampler0;\n");
        assert!(text.contains("layout(binding = 16) uniform texture2D Sampler0;"));
        assert!(text.contains("layout(binding = 17) uniform sampler Sampler0_sampler;"));
    }

    #[test]
    fn assigns_missing_io_locations_in_declaration_order() {
        let text = normalized(
            "in vec3 Position;\nin vec4 Color;\nout vec4 vertexColor;\nlayout(location = 7) in vec2 Explicit;\nvoid main() { gl_Position = vec4(Position, 1.0); vertexColor = Color; }\n",
        );
        assert!(
            text.contains("layout(location = 0) in vec3 Position"),
            "{text}"
        );
        assert!(
            text.contains("layout(location = 1) in vec4 Color"),
            "{text}"
        );
        assert!(
            text.contains("layout(location = 0) out vec4 vertexColor"),
            "{text}"
        );
        assert!(
            text.contains("layout(location = 7) in vec2 Explicit"),
            "{text}"
        );
    }

    #[test]
    fn splits_multi_declarator_interface_blocks() {
        let text = normalized(
            "flat out vec3 upVec, sunVec;\nvoid main() { gl_Position = vec4(upVec + sunVec, 1.0); }\n",
        );
        assert!(
            text.contains("layout(location = 0) out vec3 upVec;")
                || text.contains("flat layout(location = 0) out vec3 upVec;"),
            "{text}"
        );
        assert!(
            text.contains("layout(location = 1) out vec3 sunVec;")
                || text.contains("flat layout(location = 1) out vec3 sunVec;"),
            "{text}"
        );
    }

    #[test]
    fn skips_locations_the_shader_declares_later() {
        let text = normalized(
            "out vec2 Generated;\nlayout(location = 0) out vec2 Declared;\nvoid main() { gl_Position = vec4(Generated + Declared, 0.0, 1.0); }\n",
        );
        assert!(
            text.contains("layout(location = 0) out vec2 Declared"),
            "{text}"
        );
        assert!(
            text.contains("layout(location = 1) out vec2 Generated"),
            "{text}"
        );
    }

    #[test]
    fn declared_locations_yield_to_the_attribute_order() {
        let source = "layout(location = 0) in vec3 Position;\nlayout(location = 1) in vec4 Color;\nlayout(location = 2) in vec2 UV0;\nvoid main() { gl_Position = vec4(Position + Color.xyz + vec3(UV0, 0.0), 1.0); }\n";
        let order = ["Position".to_owned(), "UV0".to_owned(), "Color".to_owned()];
        let (tokens, _) = normalize_with_order(lexer::tokenize(source), &order).expect("normalize");
        let text = lexer::render(&tokens);
        assert!(
            text.contains("layout(location = 0) in vec3 Position"),
            "{text}"
        );
        assert!(text.contains("layout(location = 1) in vec2 UV0"), "{text}");
        assert!(
            text.contains("layout(location = 2) in vec4 Color"),
            "{text}"
        );
    }
    #[test]
    fn flips_clip_space_after_position_assignment() {
        let source = "void main() { gl_Position = vec4(1.0); if (gl_Position.x > 0.0) { gl_Position.y = 2.0; } }";
        let (tokens, _) =
            normalize_with_options(lexer::tokenize(source), &[], true, None).expect("normalize");
        let text = lexer::render(&tokens);
        assert!(
            text.contains("gl_Position = vec4(1.0); gl_Position.y = -gl_Position.y;"),
            "{text}"
        );
        assert_eq!(text.matches("-gl_Position.y").count(), 1, "{text}");
    }
    #[test]
    fn rewrites_legacy_texture_functions() {
        let source = "uniform sampler2D tex;\nout vec4 color;\nvoid main() {\n    color = texture2D(tex, vec2(0.5));\n}\n";
        let (tokens, _) =
            normalize_with_config(crate::lexer::tokenize(source), &NormalizeOptions::default())
                .unwrap();
        let text = crate::lexer::render(&tokens);
        assert!(!text.contains("texture2D("), "{text}");
        assert!(
            text.contains("texture(sampler2D(tex, tex_sampler)"),
            "{text}"
        );
    }

    #[test]
    fn lowers_gl_fog_fields() {
        let source = "#version 450 core\nout vec4 color;\nvoid main() {\n    color = vec4(gl_Fog.start, gl_Fog.scale, 0.0, 1.0);\n}\n";
        let options = NormalizeOptions {
            legacy_builtins: true,
            ..Default::default()
        };
        let (tokens, info) =
            normalize_with_config(crate::lexer::tokenize(source), &options).unwrap();
        let text = crate::lexer::render(&tokens);
        assert!(text.contains("gl_Fog_start"), "{text}");
        assert!(text.contains("uniform float gl_Fog_start;"), "{text}");
        assert!(
            info.warnings
                .iter()
                .any(|warning| warning.contains("gl_Fog")),
            "{info:?}"
        );
    }

    #[test]
    fn declares_compatibility_uniforms_and_attributes() {
        let source = "#version 450 core\nout vec4 color;\nvoid main() {\n    color = projectionMatrix * vec4(vaPosition, 1.0);\n}\n";
        let options = NormalizeOptions {
            legacy_builtins: true,
            ..Default::default()
        };
        let (tokens, info) =
            normalize_with_config(crate::lexer::tokenize(source), &options).unwrap();
        let text = crate::lexer::render(&tokens);
        assert!(text.contains("uniform mat4 projectionMatrix;"), "{text}");
        assert!(text.contains("in vec3 vaPosition;"), "{text}");
        assert!(!info.warnings.is_empty());
    }
    #[test]
    fn rewrites_legacy_builtins() {
        let source = "#version 330
out vec4 color;
void main() {
    gl_Position = ftransform();
    color = gl_Color + gl_Vertex + vec4(gl_Normal, 1.0) + gl_TextureMatrix[1] * gl_MultiTexCoord1;
}
";
        let (tokens, info) = normalize_with_config(
            lexer::tokenize(source),
            &NormalizeOptions {
                legacy_builtins: true,
                ..NormalizeOptions::default()
            },
        )
        .expect("normalize");
        let text = lexer::render(&tokens);
        for name in [
            "gl_Vertex",
            "gl_Color",
            "gl_Normal",
            "gl_MultiTexCoord",
            "gl_TextureMatrix",
            "ftransform",
        ] {
            assert!(!text.contains(name), "{name} survived: {text}");
        }
        assert!(text.contains("in vec3 iris_Position;"), "{text}");
        assert!(
            text.contains("uniform mat4 iris_LightmapTextureMatrix;"),
            "{text}"
        );
        assert!(text.contains("vec4(iris_UV2, 0.0, 1.0)"), "{text}");
        assert!(
            text.contains("(iris_Color * iris_ColorModulator)"),
            "{text}"
        );
        assert!(text.contains("uniform vec4 iris_ColorModulator;"), "{text}");
        assert!(
            text.contains("iris_ModelOffsetMatrix() * vec4(iris_Position, 1.0)"),
            "{text}"
        );
        assert!(text.contains("uniform vec3 iris_ModelOffset;"), "{text}");
        assert!(text.contains("mat4 iris_ModelOffsetMatrix()"), "{text}");
        assert!(
            info.warnings
                .iter()
                .any(|warning| warning.contains("rewrote 6 legacy GLSL builtin")),
            "{:?}",
            info.warnings
        );
    }

    #[test]
    fn rewrites_legacy_attribute_keyword() {
        let source = "#version 120
attribute vec3 Position;
void main() { gl_Position = vec4(Position, 1.0); }
";
        let (tokens, _) = normalize_with_config(
            lexer::tokenize(source),
            &NormalizeOptions {
                legacy_builtins: true,
                ..NormalizeOptions::default()
            },
        )
        .expect("normalize");
        let text = lexer::render(&tokens);
        assert!(!text.contains("attribute"), "{text}");
        assert!(text.contains("in vec3 Position;"), "{text}");
    }
    #[test]
    fn rewrites_inverse_matrices() {
        let source = "#version 330
out vec4 color;
void main() {
    color = gl_ModelViewMatrixInverse[3] + gl_ProjectionMatrixInverse[3];
}
";
        let (tokens, _) = normalize_with_config(
            lexer::tokenize(source),
            &NormalizeOptions {
                legacy_builtins: true,
                ..NormalizeOptions::default()
            },
        )
        .expect("normalize");
        let text = lexer::render(&tokens);
        assert!(!text.contains("gl_ModelViewMatrixInverse"), "{text}");
        assert!(!text.contains("gl_ProjectionMatrixInverse"), "{text}");
        assert!(text.contains("iris_ModelViewMatInverse[3]"), "{text}");
        assert!(text.contains("iris_ProjMatInverse[3]"), "{text}");
        assert!(
            text.contains("uniform mat4 iris_ModelViewMatInverse;"),
            "{text}"
        );
        assert!(text.contains("uniform mat4 iris_ProjMatInverse;"), "{text}");
    }
    #[test]
    fn numbers_storage_images_and_defaults_their_format() {
        let source = "#version 450 core\nwriteonly uniform image3D Volume;\nvoid main() {}\n";
        let (tokens, info) = super::normalize_with_config(
            crate::lexer::tokenize(source),
            &super::NormalizeOptions::default(),
        )
        .expect("storage images convert");
        let text = crate::lexer::render(&tokens);
        assert!(
            text.contains("layout(binding = 64, rgba32f)"),
            "the dialect owns the binding and the format: {text}"
        );
        assert!(
            info.warnings
                .iter()
                .any(|warning| warning.contains("storage image") && warning.contains("rgba32f")),
            "{:?}",
            info.warnings
        );
    }
    #[test]
    fn rejects_unknown_texture_matrix_layers() {
        let source = "#version 330
void main() {
    gl_Position = gl_TextureMatrix[2] * vec4(0.0);
}
";
        let error = normalize_with_config(
            lexer::tokenize(source),
            &NormalizeOptions {
                legacy_builtins: true,
                ..NormalizeOptions::default()
            },
        )
        .expect_err("unsupported layer");
        assert!(
            error
                .to_string()
                .contains("only layers 0 and 1 are defined"),
            "{error}"
        );
    }
    #[test]
    fn splits_comparison_samplers_once() {
        let source = "uniform sampler2DShadow ShadowMap;\nout vec4 c;\n\
                      void main() { c = texture(ShadowMap, vec3(vec2(0.5), 0.25)); }";
        let (tokens, info) = normalize(lexer::tokenize(source)).expect("normalize");
        let text = lexer::render(&tokens);
        assert_eq!(
            info.combined_samplers.len(),
            1,
            "the split sampler must not be split again: {text}"
        );
        assert!(
            text.contains("uniform texture2D ShadowMap;"),
            "expected a separable texture declaration: {text}"
        );
        assert!(
            text.contains("uniform samplerShadow ShadowMap_sampler;"),
            "expected a comparison sampler declaration: {text}"
        );
        assert!(
            text.contains("texture(sampler2DShadow(ShadowMap, ShadowMap_sampler),"),
            "expected the comparison constructor: {text}"
        );
        // Normalising the result again must be a fixed point.
        let (again, info) = normalize(tokens).expect("normalize twice");
        assert_eq!(info.combined_samplers.len(), 0, "{}", lexer::render(&again));
    }

    #[test]
    fn flips_the_named_fragment_input() {
        let source = "in vec2 texCoord;\nvoid main() {\n    vec4 c = vec4(1.0);\n}";
        let (tokens, _) =
            normalize_with_options(lexer::tokenize(source), &[], false, Some("texCoord"))
                .expect("normalize");
        let text = lexer::render(&tokens);
        assert!(
            text.contains("vec2 texCoordFlipped = vec2(texCoord.x, 1.0 - texCoord.y);"),
            "{text}"
        );
        let main = text.find("void main() {").expect("main");
        let body = &text[main..];
        assert!(body.contains("vec4 c = vec4(1.0);"), "{text}");
        assert!(!body.contains("texCoord.x * 16.0"), "{text}");
    }
    #[test]
    fn rejects_flip_texcoord_without_the_input() {
        let source = "in vec2 uv;\nvoid main() { vec4 c = vec4(1.0); }";
        let error = normalize_with_options(lexer::tokenize(source), &[], false, Some("texCoord"))
            .expect_err("missing input");
        assert!(
            error
                .to_string()
                .contains("no global input named `texCoord`"),
            "{error}"
        );
    }
    #[test]
    fn drops_point_size_assignments() {
        let source =
            "out vec4 color;\nvoid main() {\n    gl_PointSize = 4.0;\n    color = vec4(1.0);\n}\n";
        let (tokens, info) = normalize(lexer::tokenize(source)).expect("normalize");
        let text = lexer::render(&tokens);
        assert!(!text.contains("gl_PointSize"), "{text}");
        assert!(text.contains("color = vec4(1.0);"), "{text}");
        assert_eq!(info.warnings.len(), 1);
        assert!(
            info.warnings[0].contains("gl_PointSize"),
            "{:?}",
            info.warnings
        );
    }

    #[test]
    fn vertex_inputs_follow_the_host_attribute_order() {
        let source = "in vec3 Position;\nin vec4 Color;\nin vec2 UV0;\nout vec4 vertexColor;\nvoid main() { gl_Position = vec4(Position, 1.0); vertexColor = Color; }\n";
        let order = ["Position".to_owned(), "UV0".to_owned(), "Color".to_owned()];
        let (tokens, _) = normalize_with_order(lexer::tokenize(source), &order).expect("normalize");
        let text = lexer::render(&tokens);
        assert!(
            text.contains("layout(location = 0) in vec3 Position"),
            "{text}"
        );
        assert!(text.contains("layout(location = 1) in vec2 UV0"), "{text}");
        assert!(
            text.contains("layout(location = 2) in vec4 Color"),
            "{text}"
        );
    }

    #[test]
    fn uniform_block_gets_binding() {
        let text = normalized("layout(std140) uniform Globals { float x; };\n");
        assert!(text.contains("layout(std140, binding = 0)"));
    }

    #[test]
    fn texture_call_is_wrapped() {
        let text = normalized("uniform sampler2D S;\nvec4 f() { return texture(S, vec2(0.0)); }\n");
        assert!(text.contains("texture(sampler2D(S, S_sampler), vec2(0.0))"));
    }

    #[test]
    fn sampler_parameter_is_split_at_definition_and_call() {
        let text = normalized(
            "uniform sampler2D T;\nvec4 pick(sampler2D src, vec2 uv) { return texture(src, uv); }\nvec4 g() { return pick(T, vec2(1.0)); }\n",
        );
        assert!(text.contains("vec4 pick(texture2D src, sampler src_sampler, vec2 uv)"));
        assert!(text.contains("texture(sampler2D(src, src_sampler), uv)"));
        assert!(text.contains("pick(T, T_sampler, vec2(1.0))"));
    }

    #[test]
    fn unsized_array_gets_element_count() {
        let text = normalized("const vec2[] a = vec2[](-1.0, -1.0, 1.0, 1.0);\n");
        assert!(text.contains("const vec2[4] a = vec2[4](-1.0, -1.0, 1.0, 1.0)"));
    }
    #[test]
    fn casts_vertex_index() {
        let tokens = lexer::tokenize("void main() { int i = gl_VertexIndex & 7; }");
        let (tokens, info) = normalize(tokens).unwrap();
        assert!(info.warnings.is_empty());
        let text = lexer::render(&tokens);
        assert!(text.contains("int(gl_VertexIndex)"), "{text}");
    }

    #[test]
    fn inlines_const_array_sizes() {
        let tokens = lexer::tokenize(
            "void main() { const int targetCount = 8 / 4; vec4 samples[targetCount]; }",
        );
        let (tokens, _) = normalize(tokens).unwrap();
        let text = lexer::render(&tokens);
        assert!(text.contains("samples[(8 / 4)]"), "{text}");
    }

    #[test]
    fn lowers_buffer_texture_to_storage_buffer() {
        let tokens = lexer::tokenize(
            "uniform isamplerBuffer CloudFaces;\nvoid main() { int v = texelFetch(CloudFaces, 3).r; }\n",
        );
        let (tokens, info) = normalize(tokens).unwrap();
        assert_eq!(info.buffer_textures.len(), 1);
        assert_eq!(info.buffer_textures[0].binding, STORAGE_BINDING_BASE);
        let text = lexer::render(&tokens);
        assert!(
            text.contains(
                "layout(std430, binding = 48) readonly buffer CloudFacesBuffer { int data[]; } CloudFaces;"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "int dxr_texel_CloudFaces(int index) { int word = CloudFaces.data[index >> 2]; int value = (word >> ((index & 3) * 8)) & 0xFF; return value << 24 >> 24; }"
            ),
            "{text}"
        );
        assert!(text.contains("dxr_texel_CloudFaces((3))"), "{text}");
    }

    #[test]
    fn float_buffer_textures_are_reported() {
        let error = normalize(lexer::tokenize(
            "uniform samplerBuffer Values;\nvoid main() { vec4 v = texelFetch(Values, 3); }\n",
        ))
        .expect_err("float buffer textures must fail");
        assert!(matches!(error, Error::Unsupported(_)), "{error:?}");
        assert!(
            error.to_string().contains("float buffer texture"),
            "{error}"
        );
    }

    #[test]
    fn splits_array_and_integer_sampler_types() {
        let text = normalized(
            "uniform sampler2DArray Layers;\nuniform isampler2D Data;\nlayout(location = 0) out vec4 color;\nvoid main() { color = texture(Layers, vec3(0.5)); }\n",
        );
        assert!(text.contains("uniform texture2DArray Layers;"), "{text}");
        assert!(text.contains("uniform sampler Layers_sampler;"), "{text}");
        assert!(text.contains("uniform itexture2D Data;"), "{text}");
        assert!(text.contains("uniform sampler Data_sampler;"), "{text}");
        assert!(
            text.contains("texture(sampler2DArray(Layers, Layers_sampler), vec3(0.5))"),
            "{text}"
        );
    }

    #[test]
    fn integer_images_accept_texel_fetch() {
        let text = normalized(
            "uniform isampler2D Data;\nlayout(location = 0) out ivec4 value;\nvoid main() { value = texelFetch(Data, ivec2(1, 2), 0); }\n",
        );
        assert!(text.contains("uniform itexture2D Data;"), "{text}");
        assert!(text.contains("texelFetch(Data, ivec2(1, 2), 0)"), "{text}");
    }

    #[test]
    fn sampling_an_integer_image_is_reported() {
        let error = normalize(lexer::tokenize(
            "uniform isampler2D Data;\nlayout(location = 0) out vec4 color;\nvoid main() { color = vec4(texture(Data, vec2(0.5))); }\n",
        ))
        .expect_err("sampling an integer image must fail");
        assert!(matches!(error, Error::Unsupported(_)), "{error:?}");
        assert!(error.to_string().contains("texelFetch"), "{error}");
    }

    #[test]
    fn rewrites_legacy_sampling_calls() {
        let source = "uniform sampler2D colortex;\nuniform sampler2DShadow shadowtex0;\nlayout(location = 0) out vec4 color;\nvoid main() {\n    vec4 lod = texture2DLod(colortex, vec2(0.5), 0.0);\n    float shadow = shadow2D(shadowtex0, vec3(0.5)).x;\n    color = lod * shadow;\n}\n";
        let text = normalized(source);
        assert!(
            text.contains("textureLod(sampler2D(colortex, colortex_sampler), vec2(0.5), 0.0)"),
            "{text}"
        );
        assert!(
            text.contains(
                "vec4(texture(sampler2DShadow(shadowtex0, shadowtex0_sampler), vec3(0.5)))"
            ),
            "{text}"
        );
    }

    #[test]
    fn rewrites_frag_data_into_mrt_outputs() {
        let source = "#version 330\n/* DRAWBUFFERS:0364 */\nvoid main() {\n    gl_FragData[0] = vec4(1.0);\n    gl_FragData[3] = vec4(0.0);\n}\n";
        let (tokens, info) = normalize(lexer::tokenize(source)).expect("normalize");
        let text = lexer::render(&tokens);
        assert!(
            text.contains("layout(location = 0) out vec4 iris_FragData0;"),
            "{text}"
        );
        assert!(
            text.contains("layout(location = 3) out vec4 iris_FragData3;"),
            "{text}"
        );
        assert!(text.contains("iris_FragData0 = vec4(1.0);"), "{text}");
        assert_eq!(info.mrt_targets, Some(vec![0, 3, 6, 4]));
    }

    #[test]
    fn takes_the_later_mrt_directive() {
        let source = "#version 330\n/* RENDERTARGETS: 0,5 */\n/* DRAWBUFFERS:27 */\nvoid main() {\n    gl_FragData[1] = vec4(1.0);\n}\n";
        let (_, info) = normalize(lexer::tokenize(source)).expect("normalize");
        assert_eq!(info.mrt_targets, Some(vec![2, 7]));
    }

    #[test]
    fn rejects_frag_data_without_a_directive() {
        let source = "#version 330\nvoid main() {\n    gl_FragData[0] = vec4(1.0);\n}\n";
        let error = normalize(lexer::tokenize(source)).expect_err("must fail");
        assert!(
            error.to_string().contains("DRAWBUFFERS or RENDERTARGETS"),
            "{error}"
        );
    }

    #[test]
    fn rejects_frag_data_slot_outside_the_directive() {
        let source = "#version 330\n/* DRAWBUFFERS:03 */\nvoid main() {\n    gl_FragData[3] = vec4(1.0);\n}\n";
        let error = normalize(lexer::tokenize(source)).expect_err("must fail");
        assert!(error.to_string().contains("has no slot"), "{error}");
    }
}
