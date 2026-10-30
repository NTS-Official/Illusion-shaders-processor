//! `shader-convert` - compile Minecraft GLSL to HLSL and emit the manifest.
//!
//! ```text
//! shader-convert convert <file.vsh|file.fsh> [--out <dir>] [--include <dir>]...
//!                         [-D NAME[=VALUE]]... [--stage vert|frag]
//!                         [--dump-glsl] [--shader-model 6_0]
//! shader-convert audit <shader-dir> [--include <dir>]... [-D NAME[=VALUE]]...
//!                         [--report <file>] [--shader-model 6_0]
//! ```
//!
//! `convert` writes `<name>.hlsl` and `<name>.manifest.json` into the output
//! directory. `audit` compiles every shader under a directory and reports the
//! failures, which is how the conversion corpus is kept honest.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use naga::back::hlsl::ShaderModel;

use Illusion_shaders_extension::compile::{self, CompileOptions};
use Illusion_shaders_extension::{dialect, lexer, preprocess};

const USAGE: &str = "\
shader-convert convert <file> --out <dir> [--include <dir>]... [-D NAME[=VALUE]]... [--stage vert|frag] [--dump-glsl]
shader-convert audit <dir> [--include <dir>]... [-D NAME[=VALUE]]... [--report <file>] [--shader-model 6_0]
shader-convert normalize <file> [-D NAME[=VALUE]]... [--out <dir>]";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = arguments.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };

    match command.as_str() {
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        "convert" => match parse(rest).and_then(|parsed| convert(&parsed)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => fail(&message),
        },
        "normalize" => match parse(rest).and_then(|parsed| normalize(&parsed)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => fail(&message),
        },
        "audit" => match parse(rest).and_then(|parsed| audit(&parsed)) {
            Ok(0) => ExitCode::SUCCESS,
            Ok(failed) => {
                eprintln!("{failed} shader(s) failed");
                ExitCode::FAILURE
            }
            Err(message) => fail(&message),
        },
        other => fail(&format!("unknown command `{other}`\n{USAGE}")),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::FAILURE
}

#[derive(Debug, Default)]
struct Parsed {
    inputs: Vec<PathBuf>,
    out: Option<PathBuf>,
    includes: Vec<PathBuf>,
    defines: BTreeMap<String, Option<String>>,
    stage: Option<String>,
    dump_glsl: bool,
    report: Option<PathBuf>,
    shader_model: Option<ShaderModel>,
    defines_file: BTreeMap<String, BTreeMap<String, Option<String>>>,
}

fn parse(arguments: &[String]) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        match argument {
            "--out" => {
                parsed.out = Some(PathBuf::from(next_value(arguments, &mut index, argument)?))
            }
            "--include" => parsed
                .includes
                .push(PathBuf::from(next_value(arguments, &mut index, argument)?)),
            "-D" | "--define" => {
                let define = next_value(arguments, &mut index, argument)?;
                match define.split_once('=') {
                    Some((name, value)) => parsed
                        .defines
                        .insert(name.to_owned(), Some(value.to_owned())),
                    None => parsed.defines.insert(define, None),
                };
            }
            "--stage" => parsed.stage = Some(next_value(arguments, &mut index, argument)?),
            "--shader-model" => {
                let model = next_value(arguments, &mut index, argument)?;
                parsed.shader_model = Some(parse_shader_model(&model)?);
            }
            "--dump-glsl" => parsed.dump_glsl = true,
            "--report" => {
                parsed.report = Some(PathBuf::from(next_value(arguments, &mut index, argument)?))
            }
            "--defines-file" => {
                let path = next_value(arguments, &mut index, argument)?;
                parsed.defines_file = load_defines_file(Path::new(&path))?;
            }
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
            other => parsed.inputs.push(PathBuf::from(other)),
        }
        index += 1;
    }
    if parsed.inputs.is_empty() {
        return Err(USAGE.to_owned());
    }
    Ok(parsed)
}

fn next_value(arguments: &[String], index: &mut usize, name: &str) -> Result<String, String> {
    *index += 1;
    arguments
        .get(*index)
        .cloned()
        .ok_or_else(|| format!("`{name}` needs a value"))
}
fn parse_shader_model(text: &str) -> Result<ShaderModel, String> {
    match text {
        "5_0" => Ok(ShaderModel::V5_0),
        "5_1" => Ok(ShaderModel::V5_1),
        "6_0" => Ok(ShaderModel::V6_0),
        "6_1" => Ok(ShaderModel::V6_1),
        "6_2" => Ok(ShaderModel::V6_2),
        "6_3" => Ok(ShaderModel::V6_3),
        "6_4" => Ok(ShaderModel::V6_4),
        "6_5" => Ok(ShaderModel::V6_5),
        "6_6" => Ok(ShaderModel::V6_6),
        "6_7" => Ok(ShaderModel::V6_7),
        "6_8" => Ok(ShaderModel::V6_8),
        "6_9" => Ok(ShaderModel::V6_9),
        other => Err(format!("unknown shader model `{other}`")),
    }
}

fn convert(parsed: &Parsed) -> Result<(), String> {
    let path = &parsed.inputs[0];
    let stage = resolve_stage(path, parsed.stage.as_deref())?;
    let options = compile_options(parsed, path);

    let compiled = compile::compile(path, stage, &options)
        .map_err(|error| format!("{}: {error}", path.display()))?;

    let out = parsed.out.clone().unwrap_or_else(|| PathBuf::from("out"));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (hlsl_path, manifest_path) =
        compile::write_outputs(&out, &name, &compiled).map_err(|error| error.to_string())?;

    if parsed.dump_glsl {
        let glsl_path = out.join(format!("{name}.glsl"));
        std::fs::write(&glsl_path, compiled.normalized_glsl.as_bytes())
            .map_err(|error| format!("cannot write {}: {error}", glsl_path.display()))?;
    }

    for warning in &compiled.warnings {
        eprintln!("warning: {}: {warning}", path.display());
    }
    println!(
        "ok   {}  textures={} samplers={} cbuffers={} -> {} + {}",
        path.display(),
        compiled.manifest.resources.textures.len(),
        compiled.manifest.resources.samplers.len(),
        compiled.manifest.resources.constant_buffers.len(),
        hlsl_path.display(),
        manifest_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    );
    Ok(())
}

fn audit(parsed: &Parsed) -> Result<usize, String> {
    let root = &parsed.inputs[0];
    if !root.is_dir() {
        return Err(format!("`{}` is not a directory", root.display()));
    }
    let mut shaders = Vec::new();
    collect_shaders(root, &mut shaders)?;
    shaders.sort();
    if shaders.is_empty() {
        return Err(format!("no .vsh/.fsh files under `{}`", root.display()));
    }

    let mut failures = Vec::new();
    let mut compiled_count = 0usize;
    for path in &shaders {
        let stage = match compile::stage_from_path(path) {
            Some(stage) => stage,
            None => continue,
        };
        let options = compile_options(parsed, path);
        match compile::compile(path, stage, &options) {
            Ok(compiled) => {
                compiled_count += 1;
                println!(
                    "ok   {}  textures={} samplers={} cbuffers={}",
                    relative(root, path),
                    compiled.manifest.resources.textures.len(),
                    compiled.manifest.resources.samplers.len(),
                    compiled.manifest.resources.constant_buffers.len(),
                );
            }
            Err(error) => {
                let message = error.to_string();
                let first = message
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                let first = truncate(&first, 160);
                println!("fail {}  {first}", relative(root, path));
                failures
                    .push(serde_json::json!({ "shader": relative(root, path), "error": message }));
            }
        }
    }

    println!(
        "\n{} compiled, {} failed, {} total",
        compiled_count,
        failures.len(),
        shaders.len()
    );

    if let Some(report) = &parsed.report {
        let json = serde_json::json!({
            "schema": "illusion.shader-audit/1",
            "root": root.display().to_string(),
            "total": shaders.len(),
            "compiled": compiled_count,
            "failed": failures.len(),
            "failures": failures,
        });
        let text = serde_json::to_string_pretty(&json).map_err(|error| error.to_string())?;
        std::fs::write(report, text)
            .map_err(|error| format!("cannot write {}: {error}", report.display()))?;
    }

    Ok(failures.len())
}

fn normalize(parsed: &Parsed) -> Result<(), String> {
    let path = &parsed.inputs[0];
    let options = compile_options(parsed, path);
    let preprocessed = preprocess::preprocess(
        path,
        &preprocess::PreprocessOptions {
            include_roots: options.include_roots.clone(),
            defines: options.defines.clone(),
        },
    )
    .map_err(|error| format!("{}: {error}", path.display()))?;

    let tokens = lexer::tokenize(&preprocessed.text);
    let (tokens, info) =
        dialect::normalize(tokens).map_err(|error| format!("{}: {error}", path.display()))?;
    let text = lexer::render(&tokens);

    for warning in &info.warnings {
        eprintln!("warning: {}: {warning}", path.display());
    }
    eprintln!(
        "normalize {}: includes={} combined_samplers={} uniform_blocks={}",
        path.display(),
        preprocessed.included.len(),
        info.combined_samplers.len(),
        info.uniform_blocks.len()
    );

    match &parsed.out {
        Some(out) => {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            std::fs::create_dir_all(out)
                .map_err(|error| format!("cannot create {}: {error}", out.display()))?;
            let target = out.join(format!("{name}.normalized.glsl"));
            std::fs::write(&target, text.as_bytes())
                .map_err(|error| format!("cannot write {}: {error}", target.display()))?;
            println!("wrote {}", target.display());
        }
        None => println!("{text}"),
    }
    Ok(())
}

fn load_defines_file(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, Option<String>>>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("cannot parse {}: {error}", path.display()))
}

/// Layered defines: file-wide `default`, then the entry whose key the shader path ends with,
/// then command-line `-D` values which always win.
fn defines_for(parsed: &Parsed, shader: &Path) -> BTreeMap<String, Option<String>> {
    let mut defines = BTreeMap::new();
    if let Some(defaults) = parsed.defines_file.get("default") {
        defines.extend(defaults.clone());
    }
    let key = shader.to_string_lossy().replace('\\', "/");
    for (name, values) in &parsed.defines_file {
        if name != "default" && key.ends_with(name.as_str()) {
            defines.extend(values.clone());
        }
    }
    defines.extend(parsed.defines.clone());
    defines
}

fn compile_options(parsed: &Parsed, shader: &Path) -> CompileOptions {
    let mut includes = parsed.includes.clone();
    if includes.is_empty()
        && let Some(root) = shader_root(shader)
    {
        includes.push(root);
    }
    CompileOptions {
        include_roots: includes,
        defines: defines_for(parsed, shader),
        shader_model: parsed.shader_model.unwrap_or(ShaderModel::V6_0),
        ..Default::default()
    }
}

/// `.../shaders/core/terrain.vsh` -> `.../shaders`.
fn shader_root(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    match parent.file_name()?.to_str()? {
        "core" | "post" | "include" => parent.parent().map(Path::to_path_buf),
        _ => Some(parent.to_path_buf()),
    }
}

fn resolve_stage(path: &Path, requested: Option<&str>) -> Result<naga::ShaderStage, String> {
    match requested {
        Some("vert" | "vertex" | "vsh") => Ok(naga::ShaderStage::Vertex),
        Some("frag" | "fragment" | "fsh") => Ok(naga::ShaderStage::Fragment),
        Some("comp" | "compute") => Ok(naga::ShaderStage::Compute),
        Some(other) => Err(format!("unknown stage `{other}`")),
        None => compile::stage_from_path(path)
            .ok_or_else(|| format!("cannot infer stage from `{}`", path.display())),
    }
}

fn collect_shaders(directory: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            collect_shaders(&path, out)?;
        } else if compile::stage_from_path(&path).is_some() {
            out.push(path);
        }
    }
    Ok(())
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut truncated: String = text.chars().take(limit).collect();
    truncated.push_str("...");
    truncated
}
