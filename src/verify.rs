//! Compile the emitted HLSL with DXC and compare its reflection with the manifest.
//!
//! `dxc -Fc` writes the DXIL assembly, whose `!dx.resources` metadata lists every binding as
//! `{kind, type, name, space, lower bound, ...}`. The manifest is only trusted as far as DXC
//! confirms it: a register the manifest announces but DXIL does not hold was pruned as unused
//! (warning), while a register DXIL holds that the manifest does not declare is a real
//! disagreement (failure).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use naga::ShaderStage;
use naga::back::hlsl::ShaderModel;

use crate::compile::CompiledShader;
use crate::manifest::{Resources, StageIo};

static TEMPORARY_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BindingKind {
    Srv,
    Uav,
    Cbv,
    Sampler,
}

impl BindingKind {
    fn from_group(index: usize) -> Self {
        match index {
            0 => Self::Srv,
            1 => Self::Uav,
            2 => Self::Cbv,
            _ => Self::Sampler,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Srv => "SRV",
            Self::Uav => "UAV",
            Self::Cbv => "CBV",
            Self::Sampler => "sampler",
        }
    }

    fn register_prefix(self) -> char {
        match self {
            Self::Srv => 't',
            Self::Uav => 'u',
            Self::Cbv => 'b',
            Self::Sampler => 's',
        }
    }
}

/// One binding as DXC recorded it in `!dx.resources`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflectionBinding {
    pub kind: BindingKind,
    pub space: u32,
    pub register: u32,
    /// The LLVM type name, e.g. `class.Texture2D<vector<float, 4> >`.
    pub element: String,
}

#[derive(Clone, Debug, Default)]
pub struct Reflection {
    pub bindings: Vec<ReflectionBinding>,
    /// Entry-point inputs as the `ISG1` signature records them.
    pub inputs: Vec<SignatureParameter>,
    /// Entry-point outputs as the `OSG1` signature records them.
    pub outputs: Vec<SignatureParameter>,
    /// Constant buffer layouts from `!dx.typeAnnotations` (`-Zi` must be on).
    pub layouts: Vec<BufferLayout>,
}

/// One constant buffer as DXC's debug annotations describe it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BufferLayout {
    /// Struct name without its `struct.`/`hostlayout.struct.` prefix.
    pub name: String,
    pub size: u32,
    pub members: Vec<LayoutMember>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutMember {
    pub name: String,
    pub offset: u32,
}

/// One entry of a DXIL input or output signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureParameter {
    /// Semantic name without its index, e.g. `LOC` or `SV_Position`.
    pub semantic: String,
    pub index: u32,
    /// Register the parameter occupies; for `LOC<n>` this is `n`.
    pub register: u32,
    pub component_type: ComponentType,
    /// Written components, 1..=4.
    pub components: u8,
    /// `SV_*` parameters are not part of the manifest's stage IO.
    pub system: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentType {
    Float32,
    UInt32,
    SInt32,
    Unknown(u32),
}

impl ComponentType {
    fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::UInt32,
            2 => Self::SInt32,
            3 => Self::Float32,
            other => Self::Unknown(other),
        }
    }

    fn label(self) -> String {
        match self {
            Self::Float32 => "float32".to_owned(),
            Self::UInt32 => "uint32".to_owned(),
            Self::SInt32 => "int32".to_owned(),
            Self::Unknown(raw) => format!("component type {raw}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Failure,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct Verification {
    /// Bindings DXC reported for the shader.
    pub bindings: usize,
    /// Manifest resources DXC dropped because the shader never uses them.
    pub pruned: usize,
    /// Entry-point inputs / outputs in the DXIL signatures.
    pub inputs: usize,
    pub outputs: usize,
    /// Manifest stage parameters DXC dropped because the entry point ignores them.
    pub pruned_io: usize,
    pub findings: Vec<Finding>,
}

impl Verification {
    pub fn is_ok(&self) -> bool {
        !self
            .findings
            .iter()
            .any(|finding| finding.severity == Severity::Failure)
    }

    pub fn failures(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Failure)
    }
}

#[derive(Debug)]
pub enum Error {
    MissingDxc(PathBuf),
    Spawn {
        dxc: PathBuf,
        source: std::io::Error,
    },
    Compile {
        status: Option<i32>,
        diagnostics: String,
    },
    Reflection(String),
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDxc(path) => write!(
                formatter,
                "`{}` is not a DXC executable; pass --dxc <path>",
                path.display()
            ),
            Self::Spawn { dxc, source } => {
                write!(formatter, "cannot run `{}`: {source}", dxc.display())
            }
            Self::Compile {
                status,
                diagnostics,
            } => {
                let code = status
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_owned());
                write!(formatter, "dxc failed ({code}): {diagnostics}")
            }
            Self::Reflection(message) => write!(formatter, "dxc reflection: {message}"),
            Self::Io(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug)]
pub struct DxcOptions {
    pub dxc: PathBuf,
    pub profile: String,
    pub entry_point: String,
}

impl DxcOptions {
    pub fn new(
        dxc: impl Into<PathBuf>,
        stage: ShaderStage,
        model: ShaderModel,
        entry_point: impl Into<String>,
    ) -> Self {
        Self {
            dxc: dxc.into(),
            profile: profile(stage, model),
            entry_point: entry_point.into(),
        }
    }
}

/// The `-T` profile an shader stage compiles under.
pub fn profile(stage: ShaderStage, model: ShaderModel) -> String {
    let prefix = match stage {
        ShaderStage::Vertex => "vs",
        ShaderStage::Fragment => "ps",
        ShaderStage::Compute => "cs",
        ShaderStage::Task => "as",
        ShaderStage::Mesh => "ms",
        ShaderStage::RayGeneration
        | ShaderStage::AnyHit
        | ShaderStage::ClosestHit
        | ShaderStage::Miss => "lib",
    };
    format!("{prefix}_{}", model.to_str())
}

/// Finds DXC: an explicit path, then `PATH`, then the Vulkan SDK that ships `dxc.exe`.
pub fn find_dxc(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return path.is_file().then(|| path.to_path_buf());
    }
    if let Some(path) = which("dxc") {
        return Some(path);
    }
    let sdk = std::env::var_os("VULKAN_SDK")?;
    let candidate = PathBuf::from(sdk).join("Bin").join(executable("dxc"));
    candidate.is_file().then_some(candidate)
}

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(executable(program)))
        .find(|candidate| candidate.is_file())
}

fn executable(program: &str) -> String {
    if cfg!(windows) {
        format!("{program}.exe")
    } else {
        program.to_owned()
    }
}

/// Runs DXC on `hlsl` and returns what its assembly says about the resource bindings.
pub fn compile_and_reflect(
    hlsl: &str,
    name: &str,
    options: &DxcOptions,
) -> Result<Reflection, Error> {
    let directory = temporary_directory();
    std::fs::create_dir_all(&directory)
        .map_err(|error| Error::Io(format!("cannot create {}: {error}", directory.display())))?;

    let result = reflect_in(&directory, hlsl, name, options);
    let _ = std::fs::remove_dir_all(&directory);
    result
}

fn reflect_in(
    directory: &Path,
    hlsl: &str,
    name: &str,
    options: &DxcOptions,
) -> Result<Reflection, Error> {
    let stem = sanitized(name);
    let source = directory.join(format!("{stem}.hlsl"));
    let object = directory.join(format!("{stem}.dxil"));
    let assembly = directory.join(format!("{stem}.asm"));
    std::fs::write(&source, hlsl.as_bytes())
        .map_err(|error| Error::Io(format!("cannot write {}: {error}", source.display())))?;

    let output = Command::new(&options.dxc)
        .arg("-T")
        .arg(&options.profile)
        .arg("-E")
        .arg(&options.entry_point)
        .arg("-Fo")
        .arg(&object)
        .arg("-Fc")
        .arg(&assembly)
        .arg("-Zi")
        .arg("-Wno-parentheses-equality")
        .arg(&source)
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::MissingDxc(options.dxc.clone())
            } else {
                Error::Spawn {
                    dxc: options.dxc.clone(),
                    source: error,
                }
            }
        })?;

    if !output.status.success() {
        let diagnostics = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(Error::Compile {
            status: output.status.code(),
            diagnostics,
        });
    }

    let text = std::fs::read_to_string(&assembly)
        .map_err(|error| Error::Io(format!("cannot read {}: {error}", assembly.display())))?;
    let object_bytes = std::fs::read(&object)
        .map_err(|error| Error::Io(format!("cannot read {}: {error}", object.display())))?;
    let mut reflection = reflection_from_assembly(&text)?;
    let signatures = reflection_from_container(&object_bytes)?;
    reflection.inputs = signatures.inputs;
    reflection.outputs = signatures.outputs;
    reflection.layouts = buffer_layouts(&text)?;
    Ok(reflection)
}

/// Reads `!dx.typeAnnotations` (only emitted with `-Zi`) into constant buffer layouts.
pub fn buffer_layouts(assembly: &str) -> Result<Vec<BufferLayout>, Error> {
    let nodes = metadata_nodes(assembly);
    let Some(list) = nodes.get("dx.typeAnnotations") else {
        return Ok(Vec::new());
    };
    let mut layouts = Vec::new();
    for reference in node_references(list).into_iter().flatten() {
        let body = node_value(&nodes, &reference)?;
        collect_layouts(&nodes, body, &mut layouts)?;
    }
    Ok(layouts)
}

fn collect_layouts(
    nodes: &BTreeMap<String, String>,
    body: &str,
    layouts: &mut Vec<BufferLayout>,
) -> Result<(), Error> {
    let items = node_items(body);
    let mut index = 0;
    while index + 1 < items.len() {
        let Some(name) = type_reference(&items[index]) else {
            index += 1;
            continue;
        };
        let Some(reference) = node_reference(&items[index + 1]) else {
            index += 1;
            continue;
        };
        let layout_body = node_value(nodes, &reference)?;
        if let Some(layout) = parse_layout(name, layout_body, nodes)? {
            layouts.push(layout);
        }
        index += 2;
    }
    Ok(())
}

/// `%hostlayout.struct.Globals undef` -> `Globals`.
fn type_reference(item: &str) -> Option<String> {
    let item = item.trim();
    let name = item.strip_prefix('%')?.trim();
    let name = name.strip_suffix("undef").unwrap_or(name).trim();
    let mut name = name;
    while let Some(rest) = name.strip_prefix("hostlayout.") {
        name = rest;
    }
    let name = name.strip_prefix("struct.").unwrap_or(name);
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

fn node_reference(item: &str) -> Option<String> {
    let item = item.trim();
    let id = item.strip_prefix('!')?;
    (!id.is_empty() && id.chars().all(|character| character.is_ascii_digit()))
        .then(|| id.to_owned())
}

/// Splits `!{i32 0, %struct.X undef, !7, ...}` into its top-level items.
fn node_items(text: &str) -> Vec<String> {
    let inner = text
        .trim()
        .strip_prefix("!{")
        .and_then(|text| text.strip_suffix('}'))
        .unwrap_or(text);
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (index, character) in inner.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(inner[start..index].trim().to_owned());
                start = index + 1;
            }
            _ => {}
        }
    }
    let tail = inner[start..].trim();
    if !tail.is_empty() {
        items.push(tail.to_owned());
    }
    items
}

fn parse_layout(
    name: String,
    body: &str,
    nodes: &BTreeMap<String, String>,
) -> Result<Option<BufferLayout>, Error> {
    let items = node_items(body);
    let Some(size) = items.first().and_then(|item| i32_value(item)) else {
        return Ok(None);
    };
    let mut members = Vec::new();
    for item in &items {
        let Some(reference) = node_reference(item) else {
            continue;
        };
        let member_body = node_value(nodes, &reference)?;
        if let Some(member) = parse_layout_member(member_body) {
            members.push(member);
        }
    }
    Ok(Some(BufferLayout {
        name,
        size: size.max(0) as u32,
        members,
    }))
}

/// `{i32 6, !"CameraBlockPos", i32 3, i32 0, ...}` -> name and offset.
fn parse_layout_member(body: &str) -> Option<LayoutMember> {
    let items = node_items(body);
    let name = items
        .iter()
        .find_map(|item| {
            item.strip_prefix("!\"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .map(|name| name.to_owned())?;
    let marker = items.iter().position(|item| item.trim() == "i32 3")?;
    let offset = items.get(marker + 1).and_then(|item| i32_value(item))?;
    Some(LayoutMember {
        name,
        offset: offset.max(0) as u32,
    })
}

fn i32_value(item: &str) -> Option<i64> {
    item.trim().strip_prefix("i32 ")?.trim().parse().ok()
}

/// Reads the entry-point signatures out of a DXIL container.
///
/// The container header is `DXBC` + 16-byte hash + version + size + part count, followed by one
/// `u32` offset per part; each part starts with a four-character code and its size.
pub fn reflection_from_container(bytes: &[u8]) -> Result<Reflection, Error> {
    if bytes.len() < 32 || &bytes[0..4] != b"DXBC" {
        return Err(Error::Reflection(
            "the object is not a DXIL container".to_owned(),
        ));
    }
    let count = u32_at(bytes, 28)?;
    let mut reflection = Reflection::default();
    for index in 0..count as usize {
        let offset = u32_at(bytes, 32 + index * 4)? as usize;
        let header = bytes
            .get(offset..offset + 8)
            .ok_or_else(|| Error::Reflection(format!("part {index} header is out of range")))?;
        let input = match &header[0..4] {
            b"ISG1" => true,
            b"OSG1" => false,
            _ => continue,
        };
        let size = u32_at(header, 4)? as usize;
        let data = bytes
            .get(offset + 8..offset + 8 + size)
            .ok_or_else(|| Error::Reflection(format!("part {index} body is out of range")))?;
        let parameters = signature_parameters(data)?;
        if input {
            reflection.inputs = parameters;
        } else {
            reflection.outputs = parameters;
        }
    }
    Ok(reflection)
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| Error::Reflection(format!("offset {offset} is out of range")))?;
    Ok(u32::from_le_bytes(
        slice.try_into().expect("four bytes were sliced"),
    ))
}

/// Parses one `ISG1`/`OSG1` part: element count, table offset, then the elements and their
/// semantics as NUL-terminated strings after them.
fn signature_parameters(data: &[u8]) -> Result<Vec<SignatureParameter>, Error> {
    let count = u32_at(data, 0)? as usize;
    let offset = u32_at(data, 4)? as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    // DXIL v1 elements are 32 bytes (stream + name + index + system + component + register +
    // packed mask + min precision); the older ISGN layout is 24 and starts at the name offset.
    for stride in [ELEMENT_V1, ELEMENT_V0] {
        if let Some(parameters) = parse_elements(data, count, offset, stride) {
            return Ok(parameters);
        }
    }
    Err(Error::Reflection(format!(
        "signature with {count} elements at offset {offset} does not match a known layout"
    )))
}

const ELEMENT_V1: usize = 32;
const ELEMENT_V0: usize = 24;

fn parse_elements(
    data: &[u8],
    count: usize,
    offset: usize,
    stride: usize,
) -> Option<Vec<SignatureParameter>> {
    let table = offset.checked_add(count.checked_mul(stride)?)?;
    if table > data.len() {
        return None;
    }
    let name_field = if stride == ELEMENT_V1 { 1 } else { 0 };
    let mut parameters = Vec::with_capacity(count);
    for index in 0..count {
        let base = offset + index * stride;
        let field = |slot: usize| u32_at(data, base + slot * 4).ok();
        let name_at = field(name_field)? as usize;
        if name_at < table || name_at >= data.len() {
            return None;
        }
        let rest = &data[name_at..];
        let end = rest.iter().position(|byte| *byte == 0)?;
        let semantic = std::str::from_utf8(&rest[..end]).ok()?.to_owned();
        let system = field(name_field + 2)?;
        let packed = field(name_field + 5)?;
        parameters.push(SignatureParameter {
            semantic,
            index: field(name_field + 1)?,
            register: field(name_field + 4)?,
            component_type: ComponentType::from_raw(field(name_field + 3)?),
            components: (packed & 0x0f).count_ones() as u8,
            system: system != 0,
        });
    }
    Some(parameters)
}

/// Reads the resource bindings out of DXIL assembly.
pub fn reflection_from_assembly(assembly: &str) -> Result<Reflection, Error> {
    let nodes = metadata_nodes(assembly);
    let Some(resources) = nodes.get("dx.resources").map(String::as_str) else {
        // A shader that touches no resources has no `!dx.resources` at all.
        return Ok(Reflection::default());
    };
    let groups = resource_groups(&nodes, resources)?;
    let mut reflection = Reflection::default();
    for (index, group) in node_references(groups).iter().enumerate() {
        let Some(group) = group else {
            continue;
        };
        let kind = BindingKind::from_group(index);
        let entries = node_value(&nodes, group)?;
        for entry in node_references(entries).into_iter().flatten() {
            let text = node_value(&nodes, &entry)?;
            reflection.bindings.push(parse_binding(kind, text)?);
        }
    }
    Ok(reflection)
}

/// `!dx.resources = !{!4}` points at the node holding the four group lists; older layouts
/// inline them. Follow the indirection only when the value holds a single node reference.
fn resource_groups<'a>(
    nodes: &'a BTreeMap<String, String>,
    value: &'a str,
) -> Result<&'a str, Error> {
    let references = node_references(value);
    match references.as_slice() {
        [Some(id)] => node_value(nodes, id),
        _ => Ok(value),
    }
}

fn node_value<'a>(nodes: &'a BTreeMap<String, String>, id: &str) -> Result<&'a str, Error> {
    nodes
        .get(id)
        .map(String::as_str)
        .ok_or_else(|| Error::Reflection(format!("the assembly references unknown node `!{id}`")))
}

fn metadata_nodes(assembly: &str) -> BTreeMap<String, String> {
    let mut nodes = BTreeMap::new();
    for line in assembly.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('!') else {
            continue;
        };
        let Some((name, value)) = rest.split_once(" = ") else {
            continue;
        };
        nodes.insert(name.trim().to_owned(), value.trim().to_owned());
    }
    nodes
}

/// Splits `!{!5, null, !8}` into `[Some("5"), None, Some("8")]`.
fn node_references(text: &str) -> Vec<Option<String>> {
    let inner = text
        .strip_prefix("!{")
        .and_then(|text| text.strip_suffix('}'))
        .unwrap_or(text);
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (index, character) in inner.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(&inner[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    items.push(&inner[start..]);

    items
        .into_iter()
        .map(|item| {
            let item = item.trim();
            if item.is_empty() || item == "null" || !item.starts_with('!') {
                None
            } else {
                Some(item.trim_start_matches('!').trim().to_owned())
            }
        })
        .collect()
}

fn parse_binding(kind: BindingKind, text: &str) -> Result<ReflectionBinding, Error> {
    let numbers = i32_numbers(text);
    if numbers.len() < 3 {
        return Err(Error::Reflection(format!(
            "cannot parse the resource metadata `{text}`"
        )));
    }
    Ok(ReflectionBinding {
        kind,
        space: numbers[1].max(0) as u32,
        register: numbers[2].max(0) as u32,
        element: element_type(text),
    })
}

fn i32_numbers(text: &str) -> Vec<i32> {
    let mut numbers = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find("i32 ") {
        rest = &rest[index + 4..];
        let digits: String = rest
            .chars()
            .take_while(|character| character.is_ascii_digit() || *character == '-')
            .collect();
        if let Ok(number) = digits.parse() {
            numbers.push(number);
        }
    }
    numbers
}

/// `{i32 0, %class.Texture2D<vector<float, 4> >* undef, ...}` -> `class.Texture2D<vector<float, 4> >`.
fn element_type(text: &str) -> String {
    let Some(start) = text.find('%') else {
        return String::new();
    };
    let rest = &text[start + 1..];
    let end = rest.find('*').unwrap_or(rest.len());
    rest[..end].trim().to_owned()
}

/// Compares the constant buffer offsets the manifest promises with DXC's own layout.
///
/// DXC prunes buffers a shader never touches, so a missing layout is a warning rather than a
/// failure; the size and the offset of every declared member must match.
/// True for the padding members naga adds to satisfy HLSL constant buffer packing.
fn is_padding(name: &str) -> bool {
    name.starts_with("_pad") || name.starts_with("_end_pad")
}
fn compare_buffers(
    resources: &Resources,
    reflection: &Reflection,
    verification: &mut Verification,
) {
    for buffer in &resources.constant_buffers {
        if buffer.size == 0 || buffer.members.is_empty() {
            continue;
        }
        let Some(layout) = reflection
            .layouts
            .iter()
            .find(|layout| layout.name == buffer.hlsl_type)
        else {
            verification.findings.push(Finding {
                severity: Severity::Warning,
                message: format!(
                    "`{}` has no DXC layout - DXC pruned the buffer as unused",
                    buffer.name
                ),
            });
            continue;
        };
        if layout.size < buffer.size {
            verification.findings.push(Finding {
                severity: Severity::Failure,
                message: format!(
                    "`{}` needs {} bytes in the manifest but DXC allocates only {}",
                    buffer.name, buffer.size, layout.size
                ),
            });
        }
        // naga renames colliding HLSL members (`heldItemId2` becomes `heldItemId2_`) and adds
        // `_pad*` members of its own, so compare the declared members by position once the
        // padding is filtered out; fall back to names when the counts disagree.
        let declared: Vec<&LayoutMember> = layout
            .members
            .iter()
            .filter(|member| !is_padding(&member.name))
            .collect();
        let positional = declared.len() == buffer.members.len();
        for (index, member) in buffer.members.iter().enumerate() {
            let found = if positional {
                Some(declared[index])
            } else {
                layout
                    .members
                    .iter()
                    .find(|actual| actual.name == member.name)
            };
            match found {
                Some(actual) if actual.offset != member.offset => {
                    verification.findings.push(Finding {
                        severity: Severity::Failure,
                        message: format!(
                            "`{}`.`{}` is at offset {} in the manifest but DXC places it at {}",
                            buffer.name, member.name, member.offset, actual.offset
                        ),
                    });
                }
                Some(_) => {}
                None => verification.findings.push(Finding {
                    severity: Severity::Failure,
                    message: format!(
                        "`{}`.`{}` is missing from DXC's layout",
                        buffer.name, member.name
                    ),
                }),
            }
        }
    }
}
/// Compares what the manifest promises with what DXC actually bound.
pub fn compare(resources: &Resources, reflection: &Reflection) -> Verification {
    let mut verification = Verification {
        bindings: reflection.bindings.len(),
        ..Default::default()
    };

    let mut expected: Vec<(BindingKind, u32, u32, &str, &str)> = Vec::new();
    for texture in &resources.textures {
        expected.push((
            BindingKind::Srv,
            texture.space,
            texture.register,
            &texture.name,
            &texture.hlsl_type,
        ));
    }
    for sampler in &resources.samplers {
        expected.push((
            BindingKind::Sampler,
            sampler.space,
            sampler.register,
            &sampler.name,
            "SamplerState",
        ));
    }
    for buffer in &resources.constant_buffers {
        expected.push((
            BindingKind::Cbv,
            buffer.space,
            buffer.register,
            &buffer.name,
            &buffer.hlsl_type,
        ));
    }
    for buffer in &resources.storage_buffers {
        // The dialect lowers buffer textures to read-only buffers, which are SRVs.
        expected.push((
            BindingKind::Srv,
            buffer.space,
            buffer.register,
            &buffer.name,
            &buffer.hlsl_type,
        ));
    }

    for image in &resources.storage_images {
        expected.push((
            BindingKind::Uav,
            image.space,
            image.register,
            &image.name,
            &image.hlsl_type,
        ));
    }

    for (kind, space, register, name, hlsl_type) in &expected {
        let found = reflection.bindings.iter().find(|binding| {
            binding.kind == *kind && binding.space == *space && binding.register == *register
        });
        match found {
            Some(binding) => {
                if let (Some(wanted), Some(actual)) = (family(hlsl_type), family(&binding.element))
                    && wanted != actual
                {
                    verification.findings.push(Finding {
                        severity: Severity::Failure,
                        message: format!(
                            "`{name}` should be at {}{} (space {space}) as {wanted}, but DXIL holds {actual}",
                            kind.register_prefix(),
                            register
                        ),
                    });
                }
            }
            None => {
                verification.pruned += 1;
                verification.findings.push(Finding {
                    severity: Severity::Warning,
                    message: format!(
                        "`{name}` ({}{}, space {space}) is declared but absent from DXIL - DXC pruned it as unused",
                        kind.register_prefix(),
                        register
                    ),
                });
            }
        }
    }

    compare_buffers(resources, reflection, &mut verification);

    for binding in &reflection.bindings {
        let declared = expected.iter().any(|(kind, space, register, ..)| {
            *kind == binding.kind && *space == binding.space && *register == binding.register
        });
        if !declared {
            verification.findings.push(Finding {
                severity: Severity::Failure,
                message: format!(
                    "DXIL holds {} at {}{} (space {}) that the manifest does not declare",
                    binding.kind.label(),
                    binding.kind.register_prefix(),
                    binding.register,
                    binding.space
                ),
            });
        }
    }

    verification
}

/// The coarse type family a declaration belongs to, used to catch register swaps.
fn family(text: &str) -> Option<&'static str> {
    let text = text
        .trim_start_matches("class.")
        .split(['<', '['])
        .next()?
        .trim()
        .to_ascii_lowercase();
    let family = match text.as_str() {
        "texture1d" | "texture1d_array" => "Texture1D",
        "texture2d" | "texture2d_array" => "Texture2D",
        "texture3d" => "Texture3D",
        "texturecube" | "texturecube_array" => "TextureCube",
        "texture2dms" | "texture2dms_array" => "Texture2DMS",
        "byteaddressbuffer" => "ByteAddressBuffer",
        "structuredbuffer" => "StructuredBuffer",
        "rwbyteaddressbuffer" => "RWByteAddressBuffer",
        "rwstructuredbuffer" => "RWStructuredBuffer",
        "rwtexture1d" | "rwtexture1d_array" => "RWTexture1D",
        "rwtexture2d" | "rwtexture2d_array" => "RWTexture2D",
        "rwtexture3d" => "RWTexture3D",
        "samplerstate" => "SamplerState",
        "samplercomparisonstate" => "SamplerComparisonState",
        _ => return None,
    };
    Some(family)
}

#[derive(Clone, Debug)]
pub struct Verified {
    pub reflection: Reflection,
    pub verification: Verification,
    /// Manifest warnings (dropped constructs) so reports can surface them.
    pub warnings: Vec<String>,
}

pub fn verify(
    compiled: &CompiledShader,
    name: &str,
    options: &DxcOptions,
) -> Result<Verified, Error> {
    let reflection = compile_and_reflect(&compiled.hlsl, name, options)?;
    let mut verification = compare(&compiled.manifest.resources, &reflection);
    compare_stage_io(&compiled.manifest.stage_io, &reflection, &mut verification);
    Ok(Verified {
        reflection,
        verification,
        warnings: compiled.warnings.clone(),
    })
}

/// Checks the manifest's stage IO against the DXIL signatures: same locations, same component
/// count and component type. Parameters DXC dropped are only warnings, because the entry point
/// may legitimately not read every declared input.
fn compare_stage_io(io: &StageIo, reflection: &Reflection, verification: &mut Verification) {
    for (side, expected, actual) in [
        ("input", &io.inputs, &reflection.inputs),
        ("output", &io.outputs, &reflection.outputs),
    ] {
        for item in expected {
            let found = actual.iter().find(|parameter| {
                !parameter.system && signature_location(parameter) == item.location
            });
            let Some(parameter) = found else {
                verification.pruned_io += 1;
                verification.findings.push(Finding {
                    severity: Severity::Warning,
                    message: format!(
                        "`{}` (LOC{}) is declared but absent from the {side} signature - DXC pruned it as unused",
                        item.name, item.location
                    ),
                });
                continue;
            };
            let Some((family, components)) = hlsl_shape(&item.hlsl_type) else {
                continue;
            };
            if parameter.components != components {
                verification.findings.push(Finding {
                    severity: Severity::Failure,
                    message: format!(
                        "`{}` is {} ({} component(s)) but the {side} signature holds {} at LOC{}",
                        item.name, item.hlsl_type, components, parameter.components, item.location
                    ),
                });
            }
            if parameter.component_type != family {
                verification.findings.push(Finding {
                    severity: Severity::Failure,
                    message: format!(
                        "`{}` is {} but the {side} signature holds {} at LOC{}",
                        item.name,
                        item.hlsl_type,
                        parameter.component_type.label(),
                        item.location
                    ),
                });
            }
        }
        for parameter in actual.iter().filter(|parameter| !parameter.system) {
            let location = signature_location(parameter);
            if expected.iter().any(|item| item.location == location) {
                continue;
            }
            verification.findings.push(Finding {
                severity: Severity::Failure,
                message: format!(
                    "the {side} signature holds `{}{}` at LOC{} that the manifest does not declare",
                    parameter.semantic, parameter.index, location
                ),
            });
        }
        if side == "input" {
            verification.inputs = actual.len();
        } else {
            verification.outputs = actual.len();
        }
    }
}

/// The `LOC<n>` index a signature parameter occupies.
///
/// DXIL packs several row-major semantics into one register, so `register` is not the location
/// for outputs (and for inputs it only matches by accident). The semantic index is authoritative
/// for `LOC`-style semantics; other semantics fall back to the register.
fn signature_location(parameter: &SignatureParameter) -> u32 {
    if parameter.semantic == "LOC" {
        parameter.index
    } else {
        parameter.register
    }
}

/// The component type and count of a scalar/vector `hlsl_type`, or `None` for composites.
fn hlsl_shape(text: &str) -> Option<(ComponentType, u8)> {
    let text = text.trim();
    if text.contains('[') || text.contains('x') {
        return None;
    }
    let digits = text.chars().rev().take_while(char::is_ascii_digit).count();
    let (base, size) = text.split_at(text.len() - digits);
    let components = if digits == 0 { 1 } else { size.parse().ok()? };
    let family = match base {
        "float" => ComponentType::Float32,
        "int" => ComponentType::SInt32,
        "uint" => ComponentType::UInt32,
        _ => return None,
    };
    Some((family, components))
}

fn temporary_directory() -> PathBuf {
    let counter = TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "illusion-shader-verify-{}-{counter}",
        std::process::id()
    ))
}

fn sanitized(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '.' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Resources, TextureBinding};

    const ASSEMBLY: &str = r#"
!dx.resources = !{!3}

!3 = !{!4, null, !8, !13}
!4 = !{!5, !6}
!5 = !{i32 0, %class.Texture2D<vector<float, 4> >* undef, !"", i32 0, i32 0, i32 1, i32 2, i32 0}
!6 = !{i32 0, %class.Texture2D<vector<float, 4> >* undef, !"", i32 0, i32 3, i32 1, i32 2, i32 0}
!8 = !{!9}
!9 = !{i32 0, %global_1* undef, !"", i32 0, i32 1, i32 1, i32 48, null}
!13 = !{!14}
!14 = !{i32 0, %struct.SamplerState* undef, !"", i32 0, i32 0, i32 1, i32 0, null}
"#;

    fn texture(name: &str, register: u32) -> TextureBinding {
        TextureBinding {
            name: name.to_owned(),
            hlsl_type: "Texture2D<float4>".to_owned(),
            register,
            space: 0,
            group: 0,
            binding: 16 + 2 * register,
        }
    }

    fn constant_buffer(name: &str, register: u32) -> crate::manifest::BufferBinding {
        crate::manifest::BufferBinding {
            name: name.to_owned(),
            hlsl_type: "Globals".to_owned(),
            register,
            space: 0,
            group: 0,
            binding: register,
            size: 0,
            members: Vec::new(),
        }
    }

    fn sampler(name: &str, register: u32) -> crate::manifest::SamplerBinding {
        crate::manifest::SamplerBinding {
            name: name.to_owned(),
            register,
            space: 0,
            group: 0,
            binding: 17 + 2 * register,
            comparison: false,
            texture: None,
        }
    }

    /// The manifest that matches [`ASSEMBLY`] exactly.
    fn matching_manifest() -> Resources {
        Resources {
            textures: vec![texture("Sampler0", 0), texture("Sampler2", 3)],
            samplers: vec![sampler("Sampler0_sampler", 0)],
            constant_buffers: vec![constant_buffer("constants_Globals", 1)],
            storage_buffers: Vec::new(),
            storage_images: Vec::new(),
        }
    }

    #[test]
    fn parses_resource_metadata() {
        let reflection = reflection_from_assembly(ASSEMBLY).unwrap();
        assert_eq!(reflection.bindings.len(), 4);
        assert_eq!(reflection.bindings[0].kind, BindingKind::Srv);
        assert_eq!(reflection.bindings[0].register, 0);
        assert_eq!(reflection.bindings[1].register, 3);
        assert_eq!(reflection.bindings[2].kind, BindingKind::Cbv);
        assert_eq!(reflection.bindings[2].register, 1);
        assert_eq!(reflection.bindings[3].kind, BindingKind::Sampler);
        assert!(
            reflection.bindings[0]
                .element
                .starts_with("class.Texture2D")
        );
    }

    #[test]
    fn accepts_a_manifest_dxc_confirms() {
        let reflection = reflection_from_assembly(ASSEMBLY).unwrap();
        let verification = compare(&matching_manifest(), &reflection);
        assert!(verification.is_ok(), "{:?}", verification.findings);
        assert_eq!(verification.pruned, 0);
    }

    #[test]
    fn warns_about_pruned_resources() {
        let mut reflection = reflection_from_assembly(ASSEMBLY).unwrap();
        reflection
            .bindings
            .retain(|binding| binding.register != 3 || binding.kind != BindingKind::Srv);
        let verification = compare(&matching_manifest(), &reflection);
        assert!(verification.is_ok(), "{:?}", verification.findings);
        assert_eq!(verification.pruned, 1);
    }

    #[test]
    fn reports_swapped_registers() {
        let reflection = reflection_from_assembly(ASSEMBLY).unwrap();
        let mut resources = matching_manifest();
        resources.textures = vec![texture("Sampler0", 5)];
        let verification = compare(&resources, &reflection);
        assert!(!verification.is_ok());
        let failures: Vec<&str> = verification
            .failures()
            .map(|f| f.message.as_str())
            .collect();
        assert!(
            failures.iter().any(|message| message.contains("t0")),
            "{failures:?}"
        );
    }

    fn storage_image(
        name: &str,
        register: u32,
        hlsl_type: &str,
    ) -> crate::manifest::StorageImageBinding {
        crate::manifest::StorageImageBinding {
            name: name.to_owned(),
            hlsl_type: hlsl_type.to_owned(),
            register,
            space: 0,
            group: 1,
            binding: 64 + register,
            format: "Rgba32Float".to_owned(),
            access: "StorageAccess(STORE)".to_owned(),
        }
    }

    fn uav_reflection(element: &str) -> Reflection {
        Reflection {
            bindings: vec![ReflectionBinding {
                kind: BindingKind::Uav,
                space: 0,
                register: 0,
                element: element.to_owned(),
            }],
            ..Reflection::default()
        }
    }

    /// A manifest that declares nothing but the given storage images.
    fn images_only(images: Vec<crate::manifest::StorageImageBinding>) -> Resources {
        Resources {
            textures: Vec::new(),
            samplers: Vec::new(),
            constant_buffers: Vec::new(),
            storage_buffers: Vec::new(),
            storage_images: images,
        }
    }

    #[test]
    fn reports_swapped_uav_dimensions() {
        let resources = images_only(vec![storage_image("floodfill", 0, "RWTexture2D<float4>")]);
        let verification = compare(
            &resources,
            &uav_reflection("class.RWTexture3D<vector<float, 4> >"),
        );
        assert!(!verification.is_ok());
        let failures: Vec<&str> = verification
            .failures()
            .map(|f| f.message.as_str())
            .collect();
        assert!(
            failures
                .iter()
                .any(|message| message.contains("u0") && message.contains("RWTexture3D")),
            "{failures:?}"
        );
    }

    #[test]
    fn accepts_a_uav_whose_family_matches() {
        let resources = images_only(vec![storage_image("floodfill", 0, "RWTexture3D<float4>")]);
        let verification = compare(
            &resources,
            &uav_reflection("class.RWTexture3D<vector<float, 4> >"),
        );
        assert!(verification.is_ok(), "{:?}", verification.findings);
    }

    #[test]
    fn treats_missing_resources_as_empty() {
        let reflection = reflection_from_assembly("!0 = !{i32 6, i32 0}").unwrap();
        assert!(reflection.bindings.is_empty());
    }

    #[test]
    fn builds_stage_profiles() {
        assert_eq!(profile(ShaderStage::Vertex, ShaderModel::V6_0), "vs_6_0");
        assert_eq!(profile(ShaderStage::Fragment, ShaderModel::V6_9), "ps_6_9");
        assert_eq!(profile(ShaderStage::Compute, ShaderModel::V6_0), "cs_6_0");
    }

    struct StageParameter<'a> {
        semantic: &'a str,
        index: u32,
        register: u32,
        component: u32,
        mask: u8,
        system: u32,
    }

    /// Builds one `ISG1`/`OSG1` part with 32-byte elements and the semantics after them.
    fn signature_part(fourcc: [u8; 4], parameters: &[StageParameter<'_>]) -> Vec<u8> {
        const ELEMENT: usize = 32;
        let table = 8 + parameters.len() * ELEMENT;
        let mut strings = Vec::new();
        let mut names = Vec::new();
        for parameter in parameters {
            names.push(table + strings.len());
            strings.extend_from_slice(parameter.semantic.as_bytes());
            strings.push(0);
        }
        let mut body = (parameters.len() as u32).to_le_bytes().to_vec();
        body.extend_from_slice(&(8u32).to_le_bytes());
        for (parameter, name) in parameters.iter().zip(names) {
            let packed = u32::from(parameter.mask) | (u32::from(parameter.mask) << 8);
            for word in [
                0,
                name as u32,
                parameter.index,
                parameter.system,
                parameter.component,
                parameter.register,
                packed,
                0,
            ] {
                body.extend_from_slice(&word.to_le_bytes());
            }
        }
        body.extend_from_slice(&strings);

        let mut part = fourcc.to_vec();
        part.extend_from_slice(&(body.len() as u32).to_le_bytes());
        part.extend_from_slice(&body);
        part
    }

    fn container(parts: &[Vec<u8>]) -> Vec<u8> {
        let count = parts.len() as u32;
        let mut offsets = Vec::with_capacity(parts.len());
        let mut position = 32 + parts.len() * 4;
        for part in parts {
            offsets.push(position as u32);
            position += part.len();
        }
        let mut bytes = b"DXBC".to_vec();
        bytes.extend_from_slice(&[0u8; 16]);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(position as u32).to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        for offset in offsets {
            bytes.extend_from_slice(&offset.to_le_bytes());
        }
        for part in parts {
            bytes.extend_from_slice(part);
        }
        bytes
    }

    #[test]
    fn reads_constant_buffer_layouts() {
        let assembly = r#"
!dx.typeAnnotations = !{!5}
!5 = !{i32 0, %struct.Globals undef, !6, %hostlayout.struct.Projection undef, !9}
!6 = !{i32 48, !7, !8}
!7 = !{i32 6, !"CameraBlockPos", i32 3, i32 0, i32 7, i32 4}
!8 = !{i32 6, !"GlintAlpha", i32 3, i32 12, i32 7, i32 9}
!9 = !{i32 64, !10}
!10 = !{i32 6, !"ProjMat", i32 2, !11, i32 3, i32 0, i32 7, i32 9}
!11 = !{i32 4, i32 4, i32 1}
"#;
        let layouts = buffer_layouts(assembly).expect("parse");
        assert_eq!(layouts.len(), 2);
        assert_eq!(layouts[0].name, "Globals");
        assert_eq!(layouts[0].size, 48);
        assert_eq!(
            layouts[0].members,
            vec![
                LayoutMember {
                    name: "CameraBlockPos".to_owned(),
                    offset: 0
                },
                LayoutMember {
                    name: "GlintAlpha".to_owned(),
                    offset: 12
                }
            ]
        );
        assert_eq!(layouts[1].name, "Projection");
        assert_eq!(layouts[1].members[0].offset, 0);
    }

    #[test]
    fn reports_buffers_smaller_than_the_manifest() {
        let mut resources = Resources {
            textures: Vec::new(),
            samplers: Vec::new(),
            constant_buffers: Vec::new(),
            storage_buffers: Vec::new(),
            storage_images: Vec::new(),
        };
        resources
            .constant_buffers
            .push(crate::manifest::BufferBinding {
                name: "constants_Globals".to_owned(),
                hlsl_type: "Globals".to_owned(),
                register: 0,
                space: 0,
                group: 0,
                binding: 0,
                size: 80,
                members: vec![crate::manifest::BufferMember {
                    name: "Color".to_owned(),
                    hlsl_type: "float4".to_owned(),
                    offset: 0,
                    size: 16,
                }],
            });
        let mut reflection = Reflection::default();
        reflection.layouts.push(BufferLayout {
            name: "Globals".to_owned(),
            size: 64,
            members: vec![LayoutMember {
                name: "Color".to_owned(),
                offset: 0,
            }],
        });
        let verification = compare(&resources, &reflection);
        assert!(!verification.is_ok());
        assert!(
            verification
                .failures()
                .any(|finding| finding.message.contains("needs 80 bytes"))
        );
    }
    #[test]
    fn parses_signatures_from_the_container() {
        let input = signature_part(
            *b"ISG1",
            &[
                StageParameter {
                    semantic: "LOC",
                    index: 0,
                    register: 0,
                    component: 3,
                    mask: 0x07,
                    system: 0,
                },
                StageParameter {
                    semantic: "SV_VertexID",
                    index: 0,
                    register: 0,
                    component: 1,
                    mask: 0x01,
                    system: 1,
                },
            ],
        );
        let output = signature_part(
            *b"OSG1",
            &[StageParameter {
                semantic: "LOC",
                index: 2,
                register: 2,
                component: 3,
                mask: 0x0f,
                system: 0,
            }],
        );
        let reflection = reflection_from_container(&container(&[input, output])).unwrap();
        assert_eq!(reflection.inputs.len(), 2);
        assert_eq!(reflection.inputs[0].semantic, "LOC");
        assert_eq!(reflection.inputs[0].components, 3);
        assert_eq!(reflection.inputs[0].component_type, ComponentType::Float32);
        assert!(!reflection.inputs[0].system);
        assert!(reflection.inputs[1].system);
        assert_eq!(reflection.outputs.len(), 1);
        assert_eq!(reflection.outputs[0].register, 2);
        assert_eq!(reflection.outputs[0].components, 4);
    }

    fn stage_io(inputs: Vec<(&str, u32, &str)>) -> StageIo {
        StageIo {
            inputs: inputs
                .into_iter()
                .map(|(name, location, hlsl_type)| crate::manifest::Io {
                    name: name.to_owned(),
                    location,
                    hlsl_type: hlsl_type.to_owned(),
                    interpolation: None,
                })
                .collect(),
            outputs: Vec::new(),
            builtins: Vec::new(),
        }
    }

    fn signature_parameter(
        semantic: &str,
        register: u32,
        component: ComponentType,
        components: u8,
    ) -> SignatureParameter {
        SignatureParameter {
            semantic: semantic.to_owned(),
            index: register,
            register,
            component_type: component,
            components,
            system: false,
        }
    }

    #[test]
    fn accepts_stage_io_the_signature_confirms() {
        let reflection = Reflection {
            inputs: vec![signature_parameter("LOC", 1, ComponentType::Float32, 2)],
            ..Default::default()
        };
        let mut verification = Verification::default();
        compare_stage_io(
            &stage_io(vec![("UV0", 1, "float2")]),
            &reflection,
            &mut verification,
        );
        assert!(verification.is_ok(), "{:?}", verification.findings);
        assert_eq!(verification.inputs, 1);
    }

    #[test]
    fn reports_stage_io_shape_mismatches() {
        let reflection = Reflection {
            inputs: vec![signature_parameter("LOC", 1, ComponentType::Float32, 4)],
            ..Default::default()
        };
        let mut verification = Verification::default();
        compare_stage_io(
            &stage_io(vec![("UV0", 1, "float2")]),
            &reflection,
            &mut verification,
        );
        assert!(!verification.is_ok());
        assert!(
            verification
                .failures()
                .any(|finding| finding.message.contains("2 component")),
            "{:?}",
            verification.findings
        );
    }

    #[test]
    fn reports_undeclared_signature_parameters() {
        let reflection = Reflection {
            inputs: vec![signature_parameter("LOC", 3, ComponentType::Float32, 2)],
            ..Default::default()
        };
        let mut verification = Verification::default();
        compare_stage_io(&stage_io(Vec::new()), &reflection, &mut verification);
        assert!(!verification.is_ok());
    }

    #[test]
    fn treats_missing_stage_parameters_as_pruned() {
        let mut verification = Verification::default();
        compare_stage_io(
            &stage_io(vec![("Color", 2, "float4")]),
            &Reflection::default(),
            &mut verification,
        );
        assert!(verification.is_ok());
        assert_eq!(verification.pruned_io, 1);
    }

    #[test]
    fn matches_packed_outputs_by_semantic_index() {
        // DXIL packs two scalars into register 0: `LOC0` owns bit 0, `LOC1` owns bit 1.
        let pack = |index: u32| SignatureParameter {
            semantic: "LOC".to_owned(),
            index,
            register: 0,
            component_type: ComponentType::Float32,
            components: 1,
            system: false,
        };
        let reflection = Reflection {
            outputs: vec![pack(0), pack(1)],
            ..Default::default()
        };
        let io = StageIo {
            inputs: Vec::new(),
            outputs: vec![
                output("sphericalVertexDistance", 0, "float"),
                output("cylindricalVertexDistance", 1, "float"),
            ],
            builtins: Vec::new(),
        };
        let mut verification = Verification::default();
        compare_stage_io(&io, &reflection, &mut verification);
        assert!(verification.is_ok(), "{:?}", verification.findings);
    }

    fn output(name: &str, location: u32, hlsl_type: &str) -> crate::manifest::Io {
        crate::manifest::Io {
            name: name.to_owned(),
            location,
            hlsl_type: hlsl_type.to_owned(),
            interpolation: None,
        }
    }
}
