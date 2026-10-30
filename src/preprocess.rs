//! GLSL preprocessor: `#include`, conditionals, object-like macros.
//!
//! naga takes a single translation unit and has no include support, so the
//! source has to be flattened before it reaches the frontend. Everything the
//! vanilla shader corpus uses is handled here: `#include <minecraft:x.glsl>`,
//! `#ifdef` / `#ifndef` / `#if defined(...)` / `#elif` / `#else`, and
//! object-like `#define` (no function-like macros exist in the corpus).
//!
//! The output keeps one line per input line (includes are inlined) and records
//! the origin of every output line so diagnostics can point back at the file
//! the line came from.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::lexer::{self, Kind};

/// naga's GLSL frontend only accepts 440/450/460, so vanilla `#version 330`
/// sources are lifted to 450 before parsing.
pub const NORMALIZED_VERSION: &str = "#version 450 core";

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
}

#[derive(Clone, Debug)]
enum MacroDef {
    Flag,
    Value(String),
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
        let last_line = source.lines().count() as u32;

        for (index, raw) in source.lines().enumerate() {
            let line_number = index as u32 + 1;
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix('#') {
                self.directive(path, line_number, rest, &mut frames)?;
                continue;
            }
            if !Self::is_active(&frames) {
                continue;
            }
            let expanded = self.expand(line);
            self.push_line(&expanded, path, line_number);
        }

        if !frames.is_empty() {
            return Err(Error::UnterminatedConditional {
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
                    self.define(arg);
                }
                Ok(())
            }
            "undef" => {
                if Self::is_active(frames) {
                    let (name, _) = split_word(arg.trim_start());
                    self.macros.remove(name);
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

    fn define(&mut self, arg: &str) {
        let (name, value) = split_word(arg.trim_start());
        let value = strip_trailing_comment(value).trim();
        if value.is_empty() {
            self.macros.insert(name.to_owned(), MacroDef::Flag);
        } else {
            self.macros
                .insert(name.to_owned(), MacroDef::Value(value.to_owned()));
        }
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

        if !is_angle && let Some(parent) = current.parent() {
            let candidate = parent.join(body);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        for root in &self.include_roots {
            for candidate in [
                root.join("include").join(name),
                root.join(name),
                root.join("shaders").join("include").join(name),
            ] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    fn expand(&self, line: &str) -> String {
        let mut text = line.to_owned();
        for _ in 0..8 {
            let tokens = lexer::tokenize(&text);
            let mut changed = false;
            let mut out = String::with_capacity(text.len());
            for token in &tokens {
                if token.kind == Kind::Ident
                    && let Some(MacroDef::Value(value)) = self.macros.get(&token.text)
                {
                    out.push_str(value);
                    changed = true;
                    continue;
                }
                out.push_str(&token.text);
            }
            if !changed {
                break;
            }
            text = out;
        }
        text
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
        let replaced = self.replace_defined(expr);
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
        match self.macros.get(name) {
            Some(MacroDef::Value(value)) => parse_integer(value).unwrap_or(1),
            Some(MacroDef::Flag) => 1,
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

fn strip_trailing_comment(text: &str) -> &str {
    let line_comment = text.find("//").unwrap_or(text.len());
    let block_comment = text.find("/*").unwrap_or(text.len());
    &text[..line_comment.min(block_comment)]
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
}
