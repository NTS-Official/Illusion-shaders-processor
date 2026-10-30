//! GLSL dialect normalisation, Minecraft 3.30 -> naga-acceptable Vulkan GLSL.
//!
//! naga's GLSL frontend rejects several constructs the vanilla corpus uses.
//! Every rewrite here is a mechanical, local edit that preserves the shader's
//! semantics; nothing is inferred and no fallback regex post-processing of the
//! emitted HLSL is performed.
//!
//! Rewrites, in order:
//! 1. `const vec2[] a = vec2[](1, 2);` -> sized array (naga needs a length).
//! 2. `uniform Globals { ... };` -> `layout(binding = N) uniform Globals { ... };`
//! 3. `uniform sampler2D S;` -> `uniform texture2D S;` + `uniform sampler S_sampler;`
//!    with distinct bindings (naga rejects a shared binding).
//! 4. `vec4 f(sampler2D src, ...)` -> `vec4 f(texture2D src, sampler src_sampler, ...)`
//!    plus a matching extra argument at every call site.
//! 5. `texture(S, uv)` -> `texture(sampler2D(S, S_sampler), uv)`.

use std::collections::{BTreeMap, BTreeSet};

use crate::lexer::{self, Kind, Token};

/// Bindings for split combined samplers start here so they never collide with
/// the uniform blocks that are numbered from zero.
const TEXTURE_BINDING_BASE: u32 = 16;
const SAMPLER_BINDING_BASE: u32 = 17;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageType {
    Dim2,
    Cube,
}

impl ImageType {
    fn texture_type(self) -> &'static str {
        match self {
            Self::Dim2 => "texture2D",
            Self::Cube => "textureCube",
        }
    }

    fn constructor(self) -> &'static str {
        match self {
            Self::Dim2 => "sampler2D",
            Self::Cube => "samplerCube",
        }
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

#[derive(Clone, Debug, Default)]
pub struct DialectInfo {
    pub combined_samplers: Vec<CombinedSampler>,
    pub uniform_blocks: Vec<UniformBlock>,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported GLSL construct: {0}")]
    Unsupported(String),
    #[error("malformed GLSL: {0}")]
    Malformed(String),
}

pub fn normalize(mut tokens: Vec<Token>) -> Result<(Vec<Token>, DialectInfo), Error> {
    let mut info = DialectInfo::default();

    tokens = cast_vertex_index(tokens);
    tokens = inline_const_array_sizes(tokens);
    size_unsized_arrays(&mut tokens)?;
    add_block_bindings(&mut tokens, &mut info);
    split_combined_samplers(&mut tokens, &mut info)?;

    let mut sampler_map: BTreeMap<String, String> = info
        .combined_samplers
        .iter()
        .map(|sampler| (sampler.texture.clone(), sampler.sampler.clone()))
        .collect();
    let mut constructors: BTreeMap<String, String> = info
        .combined_samplers
        .iter()
        .map(|sampler| {
            (
                sampler.texture.clone(),
                sampler.image_type.constructor().to_owned(),
            )
        })
        .collect();
    let mut split_functions = BTreeSet::new();
    split_sampler_parameters(
        &mut tokens,
        &mut sampler_map,
        &mut constructors,
        &mut split_functions,
    )?;

    tokens = inject_sampler_arguments(tokens, &sampler_map, &split_functions)?;
    tokens = wrap_texture_calls(tokens, &sampler_map, &constructors)?;

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
    while let Some(uniform) = find_ident(tokens, search, "uniform") {
        search = uniform + 1;
        let Some(type_index) = lexer::next_significant(tokens, uniform + 1) else {
            return Ok(());
        };
        let image_type = match tokens[type_index].text.as_str() {
            "sampler2D" => ImageType::Dim2,
            "samplerCube" => ImageType::Cube,
            "isampler2D" | "usampler2D" | "isamplerCube" | "usamplerCube" | "sampler2DArray"
            | "isampler2DArray" => {
                return Err(Error::Unsupported(format!(
                    "opaque uniform type `{}` is not supported yet",
                    tokens[type_index].text
                )));
            }
            "samplerBuffer" | "isamplerBuffer" | "usamplerBuffer" => {
                return Err(Error::Unsupported(
                    "buffer textures are not supported yet".to_owned(),
                ));
            }
            _ => continue,
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
        let texture_binding = TEXTURE_BINDING_BASE + 2 * index;
        let sampler_binding = SAMPLER_BINDING_BASE + 2 * index;
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
            ident(image_type.texture_type(), line),
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
            ident("sampler", line),
            space(line),
            ident(&sampler_name, line),
            symbol(";", line),
        ];
        tokens.splice(uniform..=semicolon, replacement);
        info.combined_samplers.push(CombinedSampler {
            texture: name,
            sampler: sampler_name,
            image_type,
            texture_binding,
            sampler_binding,
        });
        index += 1;
        search = uniform + 1;
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
                item.kind == Kind::Ident
                    && matches!(item.text.as_str(), "sampler2D" | "samplerCube")
            });
            let mut sampler_name = None;

            match sampler_type {
                Some(type_index) => {
                    let image_type = if part_tokens[type_index].text == "samplerCube" {
                        ImageType::Cube
                    } else {
                        ImageType::Dim2
                    };
                    let name = part_tokens
                        .iter()
                        .skip(type_index + 1)
                        .find(|item| item.kind == Kind::Ident)
                        .map(|item| item.text.clone())
                        .ok_or_else(|| {
                            Error::Malformed("sampler parameter without a name".to_owned())
                        })?;
                    let typed = format!("{name}_sampler");
                    part_tokens[type_index] = ident(image_type.texture_type(), line);
                    sampler_map.insert(name.clone(), typed.clone());
                    constructors.insert(name, image_type.constructor().to_owned());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(source: &str) -> String {
        let (tokens, _) = normalize(lexer::tokenize(source)).expect("normalize");
        lexer::render(&tokens)
    }

    #[test]
    fn combined_sampler_is_split_with_distinct_bindings() {
        let text = normalized("uniform sampler2D Sampler0;\n");
        assert!(text.contains("layout(binding = 16) uniform texture2D Sampler0;"));
        assert!(text.contains("layout(binding = 17) uniform sampler Sampler0_sampler;"));
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
}
