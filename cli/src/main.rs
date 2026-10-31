//! `shader-convert` - compile Minecraft GLSL to HLSL and emit the manifest.
//!
//! ```text
//! shader-convert convert <file.vsh|file.fsh> [--out <dir>] [--include <dir>]...
//!                         [-D NAME[=VALUE]]... [--stage vert|frag]
//!                         [--dump-glsl] [--shader-model 6_0]
//! shader-convert audit <shader-dir> [--include <dir>]... [-D NAME[=VALUE]]...
//!                         [--defines-file <json>] [--report <file>]
//!                         [--shader-model 6_0] [--skip <substring>]...
//! shader-convert verify <shader-dir> [--include <dir>]... [-D NAME[=VALUE]]...
//!                         [--defines-file <json>] [--dxc <path>] [--shader-model 6_0]
//!                         [--report <file>] [--skip <substring>]...
//! ```
//!
//! `convert` writes `<name>.hlsl` and `<name>.manifest.json` into the output
//! directory. `audit` compiles every shader under a directory and reports the
//! failures, which is how the conversion corpus is kept honest. `verify` goes one
//! step further: it hands the HLSL to DXC and checks that the registers the
//! manifest promises are the ones DXC actually bound. Options a subcommand does not
//! list are rejected instead of silently ignored.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use naga::ShaderStage;
use naga::back::hlsl::ShaderModel;

use illusion_shader::StageOutput;
use illusion_shader::compile::{self, CompileOptions, RegisterLayout};
use illusion_shader::report::{AuditReport, VerifyReport};
use illusion_shader::{dialect, lexer, preprocess, verify};

const USAGE: &str = "\
shader-convert convert <file> --out <dir> [--include <dir>]... [-D NAME[=VALUE]]... [--defines-file <json>] [--stage vert|frag] [--dump-glsl] [--shader-model 6_0] [--register-layout spirv|host] [--binding NAME=REGISTER]... [--vertex-attribute NAME[,NAME]...] [--vertex-output LOC<n>=<hlsl_type>]... [--flip-y] [--flip-texcoord NAME] [--uniform-block NAME] [--legacy-builtins]
shader-convert audit <dir> [--include <dir>]... [-D NAME[=VALUE]]... [--defines-file <json>] [--report <file>] [--shader-model 6_0] [--skip <substring>]...
shader-convert verify <dir> [--include <dir>]... [-D NAME[=VALUE]]... [--defines-file <json>] [--dxc <path>] [--shader-model 6_0] [--report <file>] [--skip <substring>]...
shader-convert normalize <file> [--out <dir>] [--include <dir>]... [-D NAME[=VALUE]]... [--defines-file <json>] [--stage vert|frag] [--vertex-attribute NAME[,NAME]...] [--vertex-output LOC<n>=<hlsl_type>]... [--flip-y] [--flip-texcoord NAME] [--uniform-block NAME] [--legacy-builtins]";

/// Options a subcommand accepts; an unknown combination is a typo, not a no-op.
const CONVERT_OPTIONS: &[&str] = &[
    "--out",
    "--stage",
    "--dump-glsl",
    "--include",
    "-D",
    "--define",
    "--defines-file",
    "--shader-model",
    "--register-layout",
    "--binding",
    "--vertex-attribute",
    "--vertex-output",
    "--flip-y",
    "--flip-texcoord",
    "--uniform-block",
    "--legacy-builtins",
];

const NORMALIZE_OPTIONS: &[&str] = &[
    "--out",
    "--stage",
    "--include",
    "-D",
    "--define",
    "--defines-file",
    "--vertex-attribute",
    "--vertex-output",
    "--flip-y",
    "--flip-texcoord",
    "--uniform-block",
    "--legacy-builtins",
];

const AUDIT_OPTIONS: &[&str] = &[
    "--report",
    "--skip",
    "--include",
    "-D",
    "--define",
    "--defines-file",
    "--shader-model",
    "--register-layout",
    "--binding",
    "--vertex-attribute",
    "--vertex-output",
    "--flip-y",
    "--flip-texcoord",
    "--uniform-block",
    "--legacy-builtins",
];

const VERIFY_OPTIONS: &[&str] = &[
    "--report",
    "--skip",
    "--dxc",
    "--include",
    "-D",
    "--define",
    "--defines-file",
    "--shader-model",
    "--register-layout",
    "--binding",
    "--vertex-attribute",
    "--vertex-output",
    "--flip-y",
    "--flip-texcoord",
    "--uniform-block",
    "--legacy-builtins",
];

/// Options a subcommand accepts; an unknown combination is a typo, not a no-op.
fn allowed_options(command: &str) -> Option<&'static [&'static str]> {
    match command {
        "convert" => Some(CONVERT_OPTIONS),
        "normalize" => Some(NORMALIZE_OPTIONS),
        "audit" => Some(AUDIT_OPTIONS),
        "verify" => Some(VERIFY_OPTIONS),
        _ => None,
    }
}

/// Prefixes a diagnostic with the shader path unless it already names the file,
/// which preprocess errors and the located frontend/validation errors do.
fn locate(path: &Path, message: &str) -> String {
    let shown = path.display().to_string();
    if message.contains(&shown) {
        message.to_owned()
    } else {
        format!("{shown}: {message}")
    }
}

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
        "convert" => match parse(command, rest).and_then(|parsed| convert(&parsed)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => fail(&message),
        },
        "normalize" => match parse(command, rest).and_then(|parsed| normalize(&parsed)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => fail(&message),
        },
        "audit" => match parse(command, rest).and_then(|parsed| audit(&parsed)) {
            Ok(0) => ExitCode::SUCCESS,
            Ok(failed) => {
                eprintln!("{failed} shader(s) failed");
                ExitCode::FAILURE
            }
            Err(message) => fail(&message),
        },
        "verify" => match parse(command, rest).and_then(|parsed| verify_corpus(&parsed)) {
            Ok(0) => ExitCode::SUCCESS,
            Ok(failed) => {
                eprintln!("{failed} shader(s) failed verification");
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
    dxc: Option<PathBuf>,
    register_layout: Option<RegisterLayout>,
    bindings: BTreeMap<String, u32>,
    vertex_attributes: Vec<String>,
    vertex_outputs: Vec<StageOutput>,
    flip_y: bool,
    flip_texcoord: Option<String>,
    uniform_block: Option<String>,
    legacy_builtins: bool,
    skip: Vec<String>,
}

fn parse(command: &str, arguments: &[String]) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if argument.starts_with('-') {
            seen.insert(argument);
        }
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
            "--dxc" => {
                parsed.dxc = Some(PathBuf::from(next_value(arguments, &mut index, argument)?))
            }
            "--register-layout" => {
                let name = next_value(arguments, &mut index, argument)?;
                parsed.register_layout =
                    Some(RegisterLayout::from_name(&name).ok_or_else(|| {
                        format!("unknown register layout `{name}`; expected spirv or host")
                    })?);
            }
            "--binding" => {
                let value = next_value(arguments, &mut index, argument)?;
                let (name, register) = value
                    .split_once('=')
                    .ok_or_else(|| format!("`--binding` needs NAME=REGISTER, got `{value}`"))?;
                let register = register
                    .parse::<u32>()
                    .map_err(|error| format!("`--binding {value}`: {error}"))?;
                parsed.bindings.insert(name.to_owned(), register);
            }
            "--flip-y" => {
                parsed.flip_y = true;
                index += 1;
            }
            "--flip-texcoord" => {
                parsed.flip_texcoord = Some(next_value(arguments, &mut index, argument)?);
            }
            "--legacy-builtins" => parsed.legacy_builtins = true,
            "--skip" => parsed
                .skip
                .push(next_value(arguments, &mut index, argument)?),
            "--uniform-block" => {
                parsed.uniform_block = Some(next_value(arguments, &mut index, argument)?);
            }
            "--vertex-output" => {
                let value = next_value(arguments, &mut index, argument)?;
                parsed.vertex_outputs.push(parse_stage_output(&value)?);
            }
            "--vertex-attribute" => {
                let value = next_value(arguments, &mut index, argument)?;
                parsed.vertex_attributes.extend(
                    value
                        .split(',')
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned),
                );
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
    let allowed: &[&str] = allowed_options(command).unwrap_or(&[]);
    if let Some(unknown) = seen.iter().find(|name| !allowed.contains(name)) {
        return Err(format!("`{command}` does not take `{unknown}`\n{USAGE}"));
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

/// Parses `LOC<location>=<hlsl_type>`, the shape the ABI request carries as `vertex_outputs`.
fn parse_stage_output(value: &str) -> Result<StageOutput, String> {
    let (location, hlsl_type) = value
        .split_once('=')
        .ok_or_else(|| format!("`{value}` is not LOC<location>=<hlsl_type>"))?;
    let digits = location.strip_prefix("LOC").unwrap_or(location);
    let location = digits
        .parse::<u32>()
        .map_err(|error| format!("`{location}` is not a location: {error}"))?;
    let hlsl_type = hlsl_type.trim();
    if hlsl_type.is_empty() {
        return Err(format!("`{value}` has no HLSL type"));
    }
    Ok(StageOutput {
        location,
        hlsl_type: hlsl_type.to_owned(),
        interpolation: None,
    })
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
        .map_err(|error| locate(path, &error.to_string()))?;

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
    if !parsed.skip.is_empty() {
        shaders.retain(|path| {
            let text = path.display().to_string().replace('\\', "/");
            !parsed.skip.iter().any(|skip| text.contains(skip))
        });
    }
    shaders.sort();
    if shaders.is_empty() {
        return Err(format!("no .vsh/.fsh files under `{}`", root.display()));
    }

    let mut report = AuditReport::new(&root.display().to_string(), shaders.len());
    for path in &shaders {
        let stage = match compile::stage_from_path(path) {
            Some(stage) => stage,
            None => continue,
        };
        let options = compile_options(parsed, path);
        match compile::compile(path, stage, &options) {
            Ok(compiled) => {
                report.add_compiled();
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
                report.add_failed(&relative(root, path), message);
            }
        }
    }

    println!(
        "\n{} compiled, {} failed, {} total",
        report.compiled, report.failed, report.total
    );

    if let Some(path) = &parsed.report {
        let text = report.to_json_pretty().map_err(|error| error.to_string())?;
        std::fs::write(path, text)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    }

    Ok(report.failed)
}

/// Compiles every shader under a directory and checks the registers the manifest promises
/// against the ones DXC actually bound. The exit value is the number of shaders that failed.
fn verify_corpus(parsed: &Parsed) -> Result<usize, String> {
    let root = &parsed.inputs[0];
    if !root.is_dir() {
        return Err(format!("`{}` is not a directory", root.display()));
    }
    let dxc = verify::find_dxc(parsed.dxc.as_deref()).ok_or_else(|| {
        "dxc not found: pass --dxc <path>, put dxc on PATH, or point VULKAN_SDK at the Vulkan SDK"
            .to_owned()
    })?;
    println!("dxc  {}", dxc.display());

    let mut shaders = Vec::new();
    collect_shaders(root, &mut shaders)?;
    shaders.sort();
    if !parsed.skip.is_empty() {
        shaders.retain(|path| {
            let text = path.display().to_string().replace('\\', "/");
            !parsed.skip.iter().any(|skip| text.contains(skip))
        });
    }
    shaders.sort();
    if shaders.is_empty() {
        return Err(format!("no .vsh/.fsh files under `{}`", root.display()));
    }

    let model = parsed.shader_model.unwrap_or(ShaderModel::V6_0);
    let mut report = VerifyReport::new(
        &root.display().to_string(),
        &dxc.display().to_string(),
        model.to_str(),
        shaders.len(),
    );
    let mut passed = 0usize;
    let mut failures = 0usize;
    for path in &shaders {
        let Some(stage) = compile::stage_from_path(path) else {
            continue;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let options = compile_options(parsed, path);
        let compiled = match compile::compile(path, stage, &options) {
            Ok(compiled) => compiled,
            Err(error) => {
                failures += 1;
                report.add_error(&relative(root, path), error.to_string());
                println!(
                    "fail {}  conversion: {}",
                    relative(root, path),
                    truncate(&first_line(&error.to_string()), 200)
                );
                continue;
            }
        };

        let dxc_options = verify::DxcOptions::new(&dxc, stage, model, options.entry_point.clone());
        match verify::verify(&compiled, &name, &dxc_options) {
            Ok(verified) if verified.verification.is_ok() => {
                passed += 1;
                report.add_passed(
                    &relative(root, path),
                    &verified.verification,
                    &verified.warnings,
                );
                let warnings = if verified.warnings.is_empty() {
                    String::new()
                } else {
                    format!(" warnings={}", verified.warnings.len())
                };
                println!(
                    "ok   {}  bindings={} io={}/{} pruned={}{warnings}",
                    relative(root, path),
                    verified.verification.bindings,
                    verified.verification.inputs,
                    verified.verification.outputs,
                    verified.verification.pruned
                );
            }
            Ok(verified) => {
                failures += 1;
                report.add_failed(
                    &relative(root, path),
                    &verified.verification,
                    &verified.warnings,
                );
                let messages: Vec<&str> = verified
                    .verification
                    .failures()
                    .map(|finding| finding.message.as_str())
                    .collect();
                let first = messages
                    .first()
                    .map(|message| truncate(message, 160))
                    .unwrap_or_default();
                println!("fail {}  {first}", relative(root, path));
                for message in &messages {
                    println!("     {}", truncate(message, 200));
                }
            }
            Err(error) => {
                failures += 1;
                report.add_error(&relative(root, path), error.to_string());
                println!(
                    "fail {}  {}",
                    relative(root, path),
                    truncate(&first_line(&error.to_string()), 200)
                );
            }
        }
    }

    println!(
        "\n{passed} verified, {failures} failed, {} total",
        shaders.len()
    );
    if let Some(path) = &parsed.report {
        let text = report.to_json_pretty().map_err(|error| error.to_string())?;
        std::fs::write(path, text)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    }
    Ok(failures)
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_owned()
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
    .map_err(|error| locate(path, &error.to_string()))?;

    let attribute_order: &[String] = if matches!(
        resolve_stage(&parsed.inputs[0], parsed.stage.as_deref()),
        Ok(ShaderStage::Vertex)
    ) {
        &parsed.vertex_attributes
    } else {
        &[]
    };
    let tokens = lexer::tokenize(&preprocessed.text);
    let (tokens, info) = dialect::normalize_with_config(
        tokens,
        &dialect::NormalizeOptions {
            attribute_order,
            flip_y: parsed.flip_y,
            flip_texcoord: parsed.flip_texcoord.as_deref(),
            uniform_block: parsed.uniform_block.as_deref(),
            legacy_builtins: parsed.legacy_builtins,
        },
    )
    .map_err(|error| locate(path, &error.to_string()))?;
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
        register_layout: parsed.register_layout.unwrap_or_default(),
        bindings: parsed.bindings.clone(),
        vertex_attributes: parsed.vertex_attributes.clone(),
        vertex_outputs: parsed.vertex_outputs.clone(),
        flip_y: parsed.flip_y,
        flip_texcoord: parsed.flip_texcoord.clone(),
        uniform_block: parsed.uniform_block.clone(),
        legacy_builtins: parsed.legacy_builtins,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn an_option_the_command_does_not_take_is_rejected() {
        let error = parse("audit", &args(&["--out", "x", "dir"])).expect_err("rejected");
        assert!(error.contains("`audit` does not take `--out`"), "{error}");
        let error = parse("convert", &args(&["dir", "--dxc", "dxc.exe"])).expect_err("rejected");
        assert!(error.contains("`convert` does not take `--dxc`"), "{error}");
    }

    #[test]
    fn options_each_command_documents_are_accepted() {
        assert!(parse("audit", &args(&["--skip", "dh_", "dir"])).is_ok());
        assert!(parse("convert", &args(&["dir", "--out", "x"])).is_ok());
        assert!(parse("verify", &args(&["--dxc", "dxc.exe", "dir"])).is_ok());
        assert!(parse("normalize", &args(&["dir", "--flip-y"])).is_ok());
    }

    #[test]
    fn stage_outputs_are_parsed_into_locations() {
        let parsed = parse(
            "convert",
            &args(&[
                "dir",
                "--vertex-output",
                "LOC0=float",
                "--vertex-output",
                "1=float2",
            ]),
        )
        .expect("parsed");
        let outputs: Vec<(u32, &str)> = parsed
            .vertex_outputs
            .iter()
            .map(|output| (output.location, output.hlsl_type.as_str()))
            .collect();
        assert_eq!(outputs, vec![(0, "float"), (1, "float2")]);
        let error =
            parse("convert", &args(&["dir", "--vertex-output", "LOC1"])).expect_err("no type");
        assert!(
            error.contains("is not LOC<location>=<hlsl_type>"),
            "{error}"
        );
    }

    #[test]
    fn pack_compute_fixture_only_applies_to_its_shaders() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate sits in the workspace root")
            .join("tests/fixtures/pack-compute-defines.json");
        let parsed = parse(
            "audit",
            &args(&["dir", "--defines-file", &fixture.to_string_lossy()]),
        )
        .expect("parsed");
        let compute = defines_for(
            &parsed,
            Path::new("input/ComplementaryUnbound_r5.8.1/shaders/world0/shadowcomp.csh"),
        );
        assert!(
            matches!(compute.get("IRIS_FEATURE_CUSTOM_IMAGES"), Some(Some(value)) if value == "1"),
            "{compute:?}"
        );
        assert!(
            matches!(compute.get("COLORED_LIGHTING"), Some(Some(value)) if value == "128"),
            "{compute:?}"
        );
        let graphics = defines_for(
            &parsed,
            Path::new("input/ComplementaryUnbound_r5.8.1/shaders/world0/composite.fsh"),
        );
        assert!(graphics.is_empty(), "{graphics:?}");
    }
}
