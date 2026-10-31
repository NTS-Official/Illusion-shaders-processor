//! GLSL preprocessor: `#include`, conditionals, object-like and function-like macros.
//!
//! naga takes a single translation unit and has no include support, so the
//! source has to be flattened before it reaches the frontend. Everything the
//! vanilla shader corpus uses is handled here: `#include <minecraft:x.glsl>`,
//! `#ifdef` / `#ifndef` / `#if defined(...)` / `#elif` / `#else`, object-like
//! `#define`, and function-like `#define` (`#define f(a, b) ...`, which shader
//! packs need). `#`/`##` are not supported.
//!
//! The output keeps one line per input line (includes are inlined) and records
//! the origin of every output line so diagnostics can point back at the file
//! the line came from. A macro invocation has to stay on one line: the corpus
//! only splits `#define` bodies with `\`, which is spliced before parsing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::lexer::{self, Kind, Token};

/// naga's GLSL frontend only accepts 440/450/460, so vanilla `#version 330`
/// sources are lifted to 450 before parsing.
pub const NORMALIZED_VERSION: &str = "#version 450 core";

/// Guards against a macro that expands to itself.
const MAX_EXPANSION_DEPTH: usize = 32;

#[derive(Clone, Debug, Default)]
pub struct PreprocessOptions {
    /// Directories searched for `#include <...>` files. Each root is probed for
    /// `<root>/include/<name>`, `<root>/<name>` and `<root>/shaders/include/<name>`.
    pub include_roots: Vec<PathBuf>,
    /// Predefined macros: `None` is a flag, `Some(value)` expands in code.
    pub defines: BTreeMap<String, Option<String>>,
}

#[derive(Clone, Debug)]
pub struct Origin {
    pub path: PathBuf,
    pub line: u32,
}

#[derive(Debug)]
pub struct Preprocessed {
    pub text: String,
    pub origins: Vec<Origin>,
    pub included: Vec<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}:{line}: include {spec} not found in any include root")]
    IncludeNotFound {
        path: PathBuf,
        line: u32,
        spec: String,
    },
    #[error("{path}:{line}: include cycle through {target}")]
    IncludeCycle {
        path: PathBuf,
        line: u32,
        target: PathBuf,
    },
    #[error("{path}:{line}: unsupported directive #{name}")]
    UnknownDirective {
        path: PathBuf,
        line: u32,
        name: String,
    },
    #[error("{path}:{line}: #{name} without a matching opener")]
    StrayDirective {
        path: PathBuf,
        line: u32,
        name: String,
    },
    #[error("{path}:{line}: cannot evaluate #{name} expression `{expr}`: {reason}")]
    Expression {
        path: PathBuf,
        line: u32,
        name: String,
        expr: String,
        reason: String,
    },
    #[error("{path}:{line}: unterminated conditional block")]
    UnterminatedConditional { path: PathBuf, line: u32 },
    #[error("{path}:{line}: unterminated comment")]
    UnterminatedComment { path: PathBuf, line: u32 },
    #[error("{path}:{line}: #define {name} has an unterminated parameter list")]
    MalformedDefine {
        path: PathBuf,
        line: u32,
        name: String,
    },
}

#[derive(Clone, Debug)]
enum MacroDef {
    Flag,
    Value(String),
    Function { params: Vec<String>, body: String },
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    parent_active: bool,
    active: bool,
    taken: bool,
}

pub fn preprocess(path: &Path, options: &PreprocessOptions) -> Result<Preprocessed, Error> {
    let source = std::fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })?;
    preprocess_source(path, &source, options)
}

/// Preprocesses source text whose includes still resolve relative to `path`.
/// The C ABI uses this so a host can pass the shader text instead of a file.
pub fn preprocess_source(
    path: &Path,
    source: &str,
    options: &PreprocessOptions,
) -> Result<Preprocessed, Error> {
    let mut preprocessor = Preprocessor {
        macros: options
            .defines
            .iter()
            .map(|(name, value)| {
                let def = match value {
                    Some(value) => MacroDef::Value(value.clone()),
                    None => MacroDef::Flag,
                };
                (name.clone(), def)
            })
            .collect(),
        external: options.defines.keys().cloned().collect(),
        include_roots: options.include_roots.clone(),
        stack: Vec::new(),
        out: String::new(),
        origins: Vec::new(),
        included: Vec::new(),
        emitted_version: false,
    };

    let entry = std::path::absolute(path).map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })?;
    preprocessor.stack.push(entry);
    preprocessor.process_text(path, source)?;
    preprocessor.stack.pop();

    if !preprocessor.emitted_version {
        preprocessor
            .out
            .insert_str(0, &format!("{NORMALIZED_VERSION}\n"));
        preprocessor.origins.insert(
            0,
            Origin {
                path: path.to_owned(),
                line: 1,
            },
        );
    }

    Ok(Preprocessed {
        text: preprocessor.out,
        origins: preprocessor.origins,
        included: preprocessor.included,
    })
}

struct Preprocessor {
    macros: BTreeMap<String, MacroDef>,
    /// Macros the caller set; a shader's own `#define`/`#undef` of these is ignored, because a
    /// loader owns pack options (`COLORED_LIGHTING`, `SHADOW_QUALITY`) and a pack still carries a
    /// default `#define` for them.
    external: BTreeSet<String>,
    include_roots: Vec<PathBuf>,
    stack: Vec<PathBuf>,
    out: String,
    origins: Vec<Origin>,
    included: Vec<PathBuf>,
    emitted_version: bool,
}

impl Preprocessor {
    fn process_file(&mut self, path: &Path) -> Result<(), Error> {
        let source = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
        self.process_text(path, &source)
    }

    fn process_text(&mut self, path: &Path, source: &str) -> Result<(), Error> {
        let mut frames: Vec<Frame> = Vec::new();
        let raw_lines: Vec<&str> = source.lines().collect();
        let last_line = raw_lines.len() as u32;
        let mut index = 0usize;
        // A `#define` inside `/* ... */` is comment text, not a directive; shader packs rely on
        // that when they comment out a feature toggle.
        let mut in_comment = false;

        while index < raw_lines.len() {
            let line_number = index as u32 + 1;
            let mut line = raw_lines[index]
                .strip_suffix('\r')
                .unwrap_or(raw_lines[index])
                .to_owned();
            while line.trim_end().ends_with('\\') {
                let trimmed = line.trim_end();
                line.truncate(trimmed.len() - 1);
                index += 1;
                let Some(next) = raw_lines.get(index) else {
                    break;
                };
                line.push(' ');
                line.push_str(next.strip_suffix('\r').unwrap_or(next));
            }
            let started_in_comment = in_comment;
            in_comment = scan_comment_state(&line, in_comment);
            let trimmed = line.trim_start();
            if started_in_comment {
                if Self::is_active(&frames) {
                    self.push_line(&line, path, line_number);
                }
            } else if let Some(rest) = trimmed.strip_prefix('#') {
                self.directive(path, line_number, rest, &mut frames)?;
            } else if Self::is_active(&frames) {
                let expanded = self.expand(&line);
                self.push_line(&expanded, path, line_number);
            }
            index += 1;
        }

        if !frames.is_empty() {
            return Err(Error::UnterminatedConditional {
                path: path.to_owned(),
                line: last_line,
            });
        }
        if in_comment {
            return Err(Error::UnterminatedComment {
                path: path.to_owned(),
                line: last_line,
            });
        }
        Ok(())
    }

    fn directive(
        &mut self,
        path: &Path,
        line: u32,
        rest: &str,
        frames: &mut Vec<Frame>,
    ) -> Result<(), Error> {
        let rest = rest.trim_start();
        let (name, arg) = split_word(rest);
        match name {
            "version" => {
                if !self.emitted_version {
                    self.push_line(NORMALIZED_VERSION, path, line);
                    self.emitted_version = true;
                }
                Ok(())
            }
            // Vulkan-only hints; the HLSL backend derives this from the interface.
            "extension" | "pragma" => Ok(()),
            "define" => {
                if Self::is_active(frames) {
                    self.define(path, line, arg)?;
                }
                Ok(())
            }
            "undef" => {
                if Self::is_active(frames) {
                    let (name, _) = split_word(arg.trim_start());
                    if !self.external.contains(name) {
                        self.macros.remove(name);
                    }
                }
                Ok(())
            }
            "include" => {
                if Self::is_active(frames) {
                    self.include(path, line, arg)?;
                }
                Ok(())
            }
            "if" => {
                let parent_active = Self::is_active(frames);
                let value = if parent_active {
                    self.eval(path, line, "if", arg)? != 0
                } else {
                    false
                };
                frames.push(Frame {
                    parent_active,
                    active: parent_active && value,
                    taken: value,
                });
                Ok(())
            }
            "ifdef" | "ifndef" => {
                let parent_active = Self::is_active(frames);
                let (macro_name, _) = split_word(arg.trim_start());
                let defined = self.macros.contains_key(macro_name);
                let value = if name == "ifdef" { defined } else { !defined };
                frames.push(Frame {
                    parent_active,
                    active: parent_active && value,
                    taken: value,
                });
                Ok(())
            }
            "elif" => {
                let frame = frames.last_mut().ok_or_else(|| Error::StrayDirective {
                    path: path.to_owned(),
                    line,
                    name: name.to_owned(),
                })?;
                let value = if frame.parent_active && !frame.taken {
                    self.eval(path, line, "elif", arg)? != 0
                } else {
                    false
                };
                frame.active = frame.parent_active && !frame.taken && value;
                frame.taken |= value;
                Ok(())
            }
            "else" => {
                let frame = frames.last_mut().ok_or_else(|| Error::StrayDirective {
                    path: path.to_owned(),
                    line,
                    name: name.to_owned(),
                })?;
                frame.active = frame.parent_active && !frame.taken;
                frame.taken = true;
                Ok(())
            }
            "endif" => {
                frames.pop().ok_or_else(|| Error::StrayDirective {
                    path: path.to_owned(),
                    line,
                    name: name.to_owned(),
                })?;
                Ok(())
            }
            _ if !Self::is_active(frames) => Ok(()),
            _ => Err(Error::UnknownDirective {
                path: path.to_owned(),
                line,
                name: name.to_owned(),
            }),
        }
    }

    fn define(&mut self, path: &Path, line: u32, arg: &str) -> Result<(), Error> {
        let arg = arg.trim_start();
        let name_end = arg
            .find(|ch: char| ch.is_whitespace() || ch == '(')
            .unwrap_or(arg.len());
        let (name, rest) = arg.split_at(name_end);
        if self.external.contains(name) {
            return Ok(());
        }
        if let Some(after_paren) = rest.strip_prefix('(') {
            let Some(close) = matching_paren(after_paren) else {
                return Err(Error::MalformedDefine {
                    path: path.to_owned(),
                    line,
                    name: name.to_owned(),
                });
            };
            let params = after_paren[..close]
                .split(',')
                .map(str::trim)
                .filter(|param| !param.is_empty())
                .map(str::to_owned)
                .collect();
            let body = strip_trailing_comment(&after_paren[close + 1..])
                .trim()
                .to_owned();
            self.macros
                .insert(name.to_owned(), MacroDef::Function { params, body });
            return Ok(());
        }
        let value = strip_trailing_comment(rest).trim();
        if value.is_empty() {
            self.macros.insert(name.to_owned(), MacroDef::Flag);
        } else {
            self.macros
                .insert(name.to_owned(), MacroDef::Value(value.to_owned()));
        }
        Ok(())
    }

    fn include(&mut self, path: &Path, line: u32, arg: &str) -> Result<(), Error> {
        let spec = parse_include_spec(arg).ok_or_else(|| Error::IncludeNotFound {
            path: path.to_owned(),
            line,
            spec: arg.trim().to_owned(),
        })?;
        let target = self
            .resolve_include(path, &spec)
            .ok_or_else(|| Error::IncludeNotFound {
                path: path.to_owned(),
                line,
                spec: spec.clone(),
            })?;

        let absolute = std::path::absolute(&target).unwrap_or_else(|_| target.clone());
        if self.stack.contains(&absolute) {
            return Err(Error::IncludeCycle {
                path: path.to_owned(),
                line,
                target: absolute,
            });
        }

        self.included.push(target.clone());
        self.stack.push(absolute);
        let result = self.process_file(&target);
        self.stack.pop();
        result
    }

    fn resolve_include(&self, current: &Path, spec: &str) -> Option<PathBuf> {
        let (body, is_angle) = match spec
            .strip_prefix('<')
            .and_then(|rest| rest.strip_suffix('>'))
        {
            Some(body) => (body, true),
            None => (
                spec.strip_prefix('"')
                    .and_then(|rest| rest.strip_suffix('"'))?,
                false,
            ),
        };
        let name = body.rsplit_once(':').map_or(body, |(_, name)| name);
        // Shader packs write pack-absolute includes as `/program/x.glsl`; the include root is the
        // pack's `shaders` directory, so the leading slash only means "from the pack root".
        let relative = body.trim_start_matches('/');

        if !is_angle && let Some(parent) = current.parent() {
            for candidate in [parent.join(body), parent.join(relative)] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        for root in &self.include_roots {
            for candidate in [
                root.join("include").join(name),
                root.join(name),
                root.join("shaders").join("include").join(name),
                root.join(relative),
                root.join("shaders").join(relative),
            ] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    fn expand(&self, line: &str) -> String {
        let tokens = lexer::tokenize(line);
        let expanded = self.expand_tokens(&tokens, 0);
        let mut out = String::with_capacity(line.len());
        for token in expanded {
            out.push_str(&token.text);
        }
        out
    }

    /// Expands object-like and function-like macros in `tokens`; `depth` bounds recursion.
    fn expand_tokens(&self, tokens: &[Token], depth: usize) -> Vec<Token> {
        if depth > MAX_EXPANSION_DEPTH {
            return tokens.to_vec();
        }

        let mut out = Vec::with_capacity(tokens.len());
        let mut index = 0usize;
        while index < tokens.len() {
            let token = &tokens[index];
            if token.kind != Kind::Ident {
                out.push(token.clone());
                index += 1;
                continue;
            }
            match self.macros.get(&token.text) {
                Some(MacroDef::Value(value)) => {
                    let produced = retag(&lexer::tokenize(value), token.line);
                    out.extend(self.expand_tokens(&produced, depth + 1));
                    index += 1;
                }
                Some(MacroDef::Function { params, body }) => {
                    let open = lexer::next_significant(tokens, index + 1)
                        .filter(|at| lexer::is_symbol(&tokens[*at], "("));
                    let close = open.and_then(|open| matching_close(tokens, open));
                    let (Some(open), Some(close)) = (open, close) else {
                        out.push(token.clone());
                        index += 1;
                        continue;
                    };
                    let arguments = split_arguments(tokens, open + 1, close, token.line);
                    if arguments.len() != params.len() {
                        // Wrong arity: leave the call for naga to reject with its own message.
                        out.push(token.clone());
                        index += 1;
                        continue;
                    }
                    let substituted = substitute(body, params, &arguments, token.line);
                    out.extend(self.expand_tokens(&substituted, depth + 1));
                    index = close + 1;
                }
                Some(MacroDef::Flag) | None => {
                    out.push(token.clone());
                    index += 1;
                }
            }
        }
        out
    }

    fn push_line(&mut self, text: &str, path: &Path, line: u32) {
        self.out.push_str(text);
        self.out.push('\n');
        self.origins.push(Origin {
            path: path.to_owned(),
            line,
        });
    }

    fn is_active(frames: &[Frame]) -> bool {
        frames.iter().all(|frame| frame.active)
    }

    fn eval(&self, path: &Path, line: u32, name: &str, expr: &str) -> Result<i64, Error> {
        // Preprocessor directives end at the newline, but packs annotate them with `// ...` or
        // `/* ... */`, which the GLSL preprocessor treats as whitespace.
        let stripped = strip_comments(expr);
        let replaced = self.replace_defined(&stripped);
        let tokens = tokenize_expression(&replaced).map_err(|reason| Error::Expression {
            path: path.to_owned(),
            line,
            name: name.to_owned(),
            expr: expr.trim().to_owned(),
            reason,
        })?;

        let values: Vec<ExprToken> = tokens
            .into_iter()
            .map(|token| match token {
                ExprToken::Ident(ident) => ExprToken::Number(self.macro_value(&ident)),
                other => other,
            })
            .collect();

        let mut parser = ExpressionParser {
            tokens: &values,
            position: 0,
        };
        let value = parser.parse().map_err(|reason| Error::Expression {
            path: path.to_owned(),
            line,
            name: name.to_owned(),
            expr: expr.trim().to_owned(),
            reason,
        })?;
        if parser.position != values.len() {
            return Err(Error::Expression {
                path: path.to_owned(),
                line,
                name: name.to_owned(),
                expr: expr.trim().to_owned(),
                reason: "trailing tokens".to_owned(),
            });
        }
        Ok(value)
    }

    fn macro_value(&self, name: &str) -> i64 {
        self.macro_value_at(name, 0)
    }

    /// Resolves a macro to the integer `#if` sees. Values are expanded first: packs write
    /// `#define SSAO_QUALI SSAO_QUALI_DEFINE`, which must evaluate through to the number.
    fn macro_value_at(&self, name: &str, depth: usize) -> i64 {
        if depth > MAX_EXPANSION_DEPTH {
            return 0;
        }
        match self.macros.get(name) {
            Some(MacroDef::Value(value)) => {
                let trimmed = value.trim();
                if self
                    .macros
                    .get(trimmed)
                    .is_some_and(|def| !matches!(def, MacroDef::Flag))
                {
                    return self.macro_value_at(trimmed, depth + 1);
                }
                if let Some(number) = parse_integer(trimmed) {
                    return number;
                }
                let expanded = self.expand(value);
                parse_integer(expanded.trim()).unwrap_or(1)
            }
            Some(MacroDef::Function { .. }) | Some(MacroDef::Flag) => 1,
            None => 0,
        }
    }

    /// Rewrites `defined(X)` and `defined X` into `1`/`0` before evaluation.
    fn replace_defined(&self, expr: &str) -> String {
        let mut out = String::new();
        let bytes = expr.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if expr[i..].starts_with("defined") && (i == 0 || !is_word_byte(bytes[i - 1])) {
                let mut j = i + "defined".len();
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let mut paren = false;
                if bytes.get(j) == Some(&b'(') {
                    paren = true;
                    j += 1;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                }
                let name_start = j;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                let name = &expr[name_start..j];
                if paren {
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b')') {
                        j += 1;
                    }
                }
                out.push(if self.macros.contains_key(name) {
                    '1'
                } else {
                    '0'
                });
                i = j;
                continue;
            }
            let ch = expr[i..].chars().next().unwrap_or('\0');
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }
}

fn split_word(text: &str) -> (&str, &str) {
    let end = text
        .find(|ch: char| ch.is_whitespace())
        .unwrap_or(text.len());
    (&text[..end], &text[end..])
}

/// Index of the `)` closing the `(` that precedes `text` (text starts right after it).
fn matching_paren(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, byte) in text.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                if depth == 0 {
                    return Some(offset);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// Index of the token closing the group opened at `open`.
fn matching_close(tokens: &[Token], open: usize) -> Option<usize> {
    let close = match tokens.get(open)?.text.as_str() {
        "(" => ")",
        "[" => "]",
        "{" => "}",
        _ => return None,
    };
    let mut depth = 0usize;
    for (offset, token) in tokens[open..].iter().enumerate() {
        if token.kind != Kind::Symbol {
            continue;
        }
        if token.text == tokens[open].text {
            depth += 1;
        } else if token.text == close {
            depth -= 1;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

/// Splits the arguments between `from` and `to`, keeping brackets and braces intact.
fn split_arguments(tokens: &[Token], from: usize, to: usize, line: u32) -> Vec<Vec<Token>> {
    let mut arguments = Vec::new();
    let mut current: Vec<Token> = Vec::new();
    let mut depth = 0i32;
    for token in &tokens[from..to] {
        if token.kind == Kind::Symbol {
            match token.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                "," if depth == 0 => {
                    arguments.push(trim_tokens(&current));
                    current = Vec::new();
                    continue;
                }
                _ => {}
            }
        }
        current.push(retag(std::slice::from_ref(token), line).remove(0));
    }
    arguments.push(trim_tokens(&current));
    arguments
}

fn trim_tokens(tokens: &[Token]) -> Vec<Token> {
    let start = tokens
        .iter()
        .position(|token| !token.is_trivia())
        .unwrap_or(tokens.len());
    let end = tokens
        .iter()
        .rposition(|token| !token.is_trivia())
        .map_or(start, |at| at + 1);
    tokens[start..end].to_vec()
}

/// Replaces parameter names in a macro body with the argument tokens.
fn substitute(body: &str, params: &[String], arguments: &[Vec<Token>], line: u32) -> Vec<Token> {
    let body = retag(&lexer::tokenize(body), line);
    let mut out = Vec::with_capacity(body.len());
    for token in body {
        if token.kind == Kind::Ident
            && let Some(at) = params.iter().position(|param| *param == token.text)
        {
            for argument in &arguments[at] {
                out.push(argument.clone());
            }
            continue;
        }
        out.push(token);
    }
    out
}

/// Rewrites the line of every token, so diagnostics point at the invocation.
fn retag(tokens: &[Token], line: u32) -> Vec<Token> {
    tokens
        .iter()
        .map(|token| Token::new(token.kind, &token.text, line))
        .collect()
}

fn strip_trailing_comment(text: &str) -> &str {
    let line_comment = text.find("//").unwrap_or(text.len());
    let block_comment = text.find("/*").unwrap_or(text.len());
    &text[..line_comment.min(block_comment)]
}

/// Advances the block-comment state over one line. A `//` comment hides a later `/*`, so the scan
/// stops there; the preprocessor only needs to know whether a directive line is comment text.
fn scan_comment_state(line: &str, mut in_comment: bool) -> bool {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if in_comment {
            if bytes[index..].starts_with(b"*/") {
                in_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            break;
        }
        if bytes[index..].starts_with(b"/*") {
            in_comment = true;
            index += 2;
            continue;
        }
        index += 1;
    }
    in_comment
}

fn parse_include_spec(arg: &str) -> Option<String> {
    let arg = arg.trim_start();
    let end = match arg.chars().next()? {
        '<' => arg.find('>')?,
        '"' => arg[1..].find('"')? + 1,
        _ => return None,
    };
    Some(arg[..=end].to_owned())
}

fn parse_integer(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()
    } else {
        text.trim_end_matches(['u', 'U', 'l', 'L']).parse().ok()
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ExprToken {
    Number(i64),
    Ident(String),
    Op(String),
}

/// Removes `//` and `/* */` comments so a directive expression can be parsed.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '/' {
            match characters.peek() {
                Some('/') => break,
                Some('*') => {
                    characters.next();
                    let mut previous = '\0';
                    for next in characters.by_ref() {
                        if previous == '*' && next == '/' {
                            break;
                        }
                        previous = next;
                    }
                    out.push(' ');
                    continue;
                }
                _ => {}
            }
        }
        out.push(character);
    }
    out
}

fn tokenize_expression(text: &str) -> Result<Vec<ExprToken>, String> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if byte.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            let value = parse_integer(&text[start..i])
                .ok_or_else(|| format!("bad number `{}`", &text[start..i]))?;
            tokens.push(ExprToken::Number(value));
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = i;
            while i < bytes.len() && is_word_byte(bytes[i]) {
                i += 1;
            }
            tokens.push(ExprToken::Ident(text[start..i].to_owned()));
            continue;
        }
        let two = bytes.get(i + 1).map(|next| [byte, *next]);
        let op = match two {
            Some([b'&', b'&']) => Some("&&"),
            Some([b'|', b'|']) => Some("||"),
            Some([b'=', b'=']) => Some("=="),
            Some([b'!', b'=']) => Some("!="),
            Some([b'<', b'=']) => Some("<="),
            Some([b'>', b'=']) => Some(">="),
            Some([b'<', b'<']) => Some("<<"),
            Some([b'>', b'>']) => Some(">>"),
            _ => None,
        };
        if let Some(op) = op {
            tokens.push(ExprToken::Op(op.to_owned()));
            i += 2;
            continue;
        }
        if b"()!~*/%+-<>&^|".contains(&byte) {
            tokens.push(ExprToken::Op((byte as char).to_string()));
            i += 1;
            continue;
        }
        return Err(format!("unexpected character `{}`", byte as char));
    }
    Ok(tokens)
}

struct ExpressionParser<'a> {
    tokens: &'a [ExprToken],
    position: usize,
}

impl ExpressionParser<'_> {
    fn parse(&mut self) -> Result<i64, String> {
        self.logical_or()
    }

    fn peek_op(&self, op: &str) -> bool {
        matches!(self.tokens.get(self.position), Some(ExprToken::Op(found)) if found == op)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.peek_op(op) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn logical_or(&mut self) -> Result<i64, String> {
        let mut value = self.logical_and()?;
        while self.eat_op("||") {
            let rhs = self.logical_and()?;
            value = i64::from(value != 0 || rhs != 0);
        }
        Ok(value)
    }

    fn logical_and(&mut self) -> Result<i64, String> {
        let mut value = self.bit_or()?;
        while self.eat_op("&&") {
            let rhs = self.bit_or()?;
            value = i64::from(value != 0 && rhs != 0);
        }
        Ok(value)
    }

    fn bit_or(&mut self) -> Result<i64, String> {
        let mut value = self.bit_xor()?;
        while self.eat_op("|") {
            value |= self.bit_xor()?;
        }
        Ok(value)
    }

    fn bit_xor(&mut self) -> Result<i64, String> {
        let mut value = self.bit_and()?;
        while self.eat_op("^") {
            value ^= self.bit_and()?;
        }
        Ok(value)
    }

    fn bit_and(&mut self) -> Result<i64, String> {
        let mut value = self.equality()?;
        while self.eat_op("&") {
            value &= self.equality()?;
        }
        Ok(value)
    }

    fn equality(&mut self) -> Result<i64, String> {
        let mut value = self.relational()?;
        loop {
            if self.eat_op("==") {
                value = i64::from(value == self.relational()?);
            } else if self.eat_op("!=") {
                value = i64::from(value != self.relational()?);
            } else {
                return Ok(value);
            }
        }
    }

    fn relational(&mut self) -> Result<i64, String> {
        let mut value = self.shift()?;
        loop {
            if self.eat_op("<") {
                value = i64::from(value < self.shift()?);
            } else if self.eat_op("<=") {
                value = i64::from(value <= self.shift()?);
            } else if self.eat_op(">") {
                value = i64::from(value > self.shift()?);
            } else if self.eat_op(">=") {
                value = i64::from(value >= self.shift()?);
            } else {
                return Ok(value);
            }
        }
    }

    fn shift(&mut self) -> Result<i64, String> {
        let mut value = self.additive()?;
        loop {
            if self.eat_op("<<") {
                value <<= self.additive()?;
            } else if self.eat_op(">>") {
                value >>= self.additive()?;
            } else {
                return Ok(value);
            }
        }
    }

    fn additive(&mut self) -> Result<i64, String> {
        let mut value = self.multiplicative()?;
        loop {
            if self.eat_op("+") {
                value += self.multiplicative()?;
            } else if self.eat_op("-") {
                value -= self.multiplicative()?;
            } else {
                return Ok(value);
            }
        }
    }

    fn multiplicative(&mut self) -> Result<i64, String> {
        let mut value = self.unary()?;
        loop {
            if self.eat_op("*") {
                value *= self.unary()?;
            } else if self.eat_op("/") {
                let rhs = self.unary()?;
                if rhs == 0 {
                    return Err("division by zero".to_owned());
                }
                value /= rhs;
            } else if self.eat_op("%") {
                let rhs = self.unary()?;
                if rhs == 0 {
                    return Err("division by zero".to_owned());
                }
                value %= rhs;
            } else {
                return Ok(value);
            }
        }
    }

    fn unary(&mut self) -> Result<i64, String> {
        if self.eat_op("!") {
            return Ok(i64::from(self.unary()? == 0));
        }
        if self.eat_op("-") {
            return Ok(-self.unary()?);
        }
        if self.eat_op("+") {
            return self.unary();
        }
        if self.eat_op("~") {
            return Ok(!self.unary()?);
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<i64, String> {
        match self.tokens.get(self.position).cloned() {
            Some(ExprToken::Number(value)) => {
                self.position += 1;
                Ok(value)
            }
            Some(ExprToken::Ident(name)) => {
                self.position += 1;
                Err(format!("undefined macro `{name}` in expression"))
            }
            Some(ExprToken::Op(op)) if op == "(" => {
                self.position += 1;
                let value = self.logical_or()?;
                if !self.eat_op(")") {
                    return Err("missing `)`".to_owned());
                }
                Ok(value)
            }
            Some(ExprToken::Op(op)) => Err(format!("unexpected operator `{op}`")),
            None => Err("unexpected end of expression".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<PathBuf> {
        vec![PathBuf::from("tests/fixtures/shaders")]
    }

    #[test]
    fn include_and_macros_are_flattened() {
        let options = PreprocessOptions {
            include_roots: roots(),
            defines: BTreeMap::from([("ENABLED".to_owned(), None)]),
        };
        let path = PathBuf::from("tests/fixtures/shaders/core/include_entry.vsh");
        let result = preprocess(&path, &options).expect("preprocess");
        assert!(result.text.starts_with("#version 450 core\n"));
        assert!(result.text.contains("float from_include;"));
        assert!(result.text.contains("float enabled;"));
        assert!(!result.text.contains("float disabled;"));
        assert!(!result.text.contains("#include"));
        assert_eq!(result.origins.len(), result.text.lines().count());
    }

    #[test]
    fn function_macros_expand_with_arguments() {
        let preprocessor = Preprocessor {
            macros: BTreeMap::from([(
                "projMAD".to_owned(),
                MacroDef::Function {
                    params: vec!["m".to_owned(), "v".to_owned()],
                    body: "(diagonal3(m) * (v) + (m)[3].xyz)".to_owned(),
                },
            )]),
            include_roots: Vec::new(),
            stack: Vec::new(),
            out: String::new(),
            origins: Vec::new(),
            included: Vec::new(),
            external: BTreeSet::new(),
            emitted_version: false,
        };
        let expanded = preprocessor.expand("vec3 p = projMAD(shadowProjection, shadowpos);");
        assert!(
            expanded.contains(
                "(diagonal3(shadowProjection) * (shadowpos) + (shadowProjection)[3].xyz)"
            ),
            "{expanded}"
        );
    }

    #[test]
    fn function_macro_arguments_keep_nested_calls_together() {
        let preprocessor = Preprocessor {
            macros: BTreeMap::from([(
                "pair".to_owned(),
                MacroDef::Function {
                    params: vec!["a".to_owned(), "b".to_owned()],
                    body: "vec2(a, b)".to_owned(),
                },
            )]),
            include_roots: Vec::new(),
            stack: Vec::new(),
            out: String::new(),
            origins: Vec::new(),
            included: Vec::new(),
            external: BTreeSet::new(),
            emitted_version: false,
        };
        let expanded = preprocessor.expand("vec2 v = pair(min(a, b), max(c, d));");
        assert!(
            expanded.contains("vec2(min(a, b), max(c, d))"),
            "{expanded}"
        );
    }

    #[test]
    fn chained_macro_values_resolve_in_conditionals() {
        let preprocessor = Preprocessor {
            macros: BTreeMap::from([
                (
                    "SSAO_QUALI".to_owned(),
                    MacroDef::Value("SSAO_QUALI_DEFINE".to_owned()),
                ),
                (
                    "SSAO_QUALI_DEFINE".to_owned(),
                    MacroDef::Value("2".to_owned()),
                ),
            ]),
            include_roots: Vec::new(),
            stack: Vec::new(),
            out: String::new(),
            origins: Vec::new(),
            included: Vec::new(),
            external: BTreeSet::new(),
            emitted_version: false,
        };
        let value = preprocessor
            .eval(Path::new("test.fsh"), 1, "if", "SSAO_QUALI == 2")
            .unwrap();
        assert_eq!(value, 1);
    }
    #[test]
    fn continuation_lines_join_before_parsing() {
        let source = "#define twice(x) ((x) + \\\n    (x))\nfloat y = twice(2.0);\n";
        let mut preprocessor = Preprocessor {
            macros: BTreeMap::new(),
            include_roots: Vec::new(),
            stack: Vec::new(),
            out: String::new(),
            origins: Vec::new(),
            included: Vec::new(),
            external: BTreeSet::new(),
            emitted_version: false,
        };
        preprocessor
            .process_text(std::path::Path::new("inline.glsl"), source)
            .expect("preprocess");
        let squeezed: String = preprocessor.out.split_whitespace().collect();
        assert!(
            squeezed.contains("floaty=((2.0)+(2.0));"),
            "{}",
            preprocessor.out
        );
    }

    #[test]
    fn defined_expression_selects_branch() {
        let options = PreprocessOptions {
            include_roots: roots(),
            defines: BTreeMap::from([("A".to_owned(), Some("1".to_owned()))]),
        };
        let path = PathBuf::from("tests/fixtures/shaders/core/conditional_entry.fsh");
        let result = preprocess(&path, &options).expect("preprocess");
        assert!(result.text.contains("float branch_a;"));
        assert!(!result.text.contains("float branch_b;"));
        assert!(!result.text.contains("float branch_c;"));
    }

    #[test]
    fn directive_expressions_ignore_trailing_comments() {
        let options = PreprocessOptions {
            include_roots: roots(),
            defines: BTreeMap::from([("QUALITY".to_owned(), Some("0".to_owned()))]),
        };
        let path = PathBuf::from("tests/fixtures/shaders/core/comment_condition.fsh");
        let result = preprocess(&path, &options).expect("comment is whitespace");
        assert!(result.text.contains("float potato;"), "{}", result.text);
        assert!(
            result.text.contains("float potato_block;"),
            "{}",
            result.text
        );
        assert!(!result.text.contains("float fancy;"));
    }
}
