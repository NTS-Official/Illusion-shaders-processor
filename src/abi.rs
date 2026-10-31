//! The C ABI of the conversion library, which is what release builds ship.
//!
//! The host passes one JSON request and gets one JSON response back, so no
//! Rust type crosses the boundary and no ownership is shared. That keeps the
//! ABI to three functions while the request and response schemas stay free to
//! grow; [`ABI_VERSION`] bumps only on an incompatible schema change.
//!
//! ```c
//! uint32_t illusion_shader_abi_version(void);
//! int32_t  illusion_shader_convert(const char *request_json, char **out_response_json);
//! void     illusion_shader_free(char *string);
//! ```

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char};
use std::path::PathBuf;

use naga::ShaderStage;
use naga::back::hlsl::ShaderModel;
use serde::Deserialize;
use serde_json::json;

use crate::compile::{self, CompileOptions, CompiledShader, RegisterLayout};
use crate::compile::{Error as CompileError, stage_from_path};

/// Bumped whenever the request or response schema changes incompatibly.
pub const ABI_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConvertRequest {
    /// Shader path: names the shader and anchors relative `#include`s.
    path: String,
    /// Optional GLSL text; when absent the file at `path` is read.
    #[serde(default)]
    source: Option<String>,
    /// `vertex`/`fragment`/`compute`; derived from the extension when absent.
    #[serde(default)]
    stage: Option<String>,
    #[serde(default = "default_entry")]
    entry: String,
    #[serde(default)]
    include_roots: Vec<String>,
    #[serde(default)]
    defines: BTreeMap<String, Option<String>>,
    /// `6_0` .. `6_9`; defaults to `6_0`.
    #[serde(default)]
    shader_model: Option<String>,
    #[serde(default)]
    space: Option<u8>,
    /// `spirv` (default) or `host`. `host` numbers registers the way the host's
    /// `D3D12BindGroupEntry` list does, which is what rendering needs.
    #[serde(default)]
    register_layout: Option<String>,
    /// Exact register per resource name; the host knows the merged entry list and
    /// overrides the sequential `host` numbering with these numbers.
    #[serde(default)]
    bindings: BTreeMap<String, u32>,
    /// Vertex attribute names in input-layout order, so the emitted `LOC<n>` semantics line up
    /// with the vertex format elements.
    #[serde(default)]
    vertex_attributes: Vec<String>,
    /// Flipped D3D clip space: the converter appends `gl_Position.y = -gl_Position.y;` after every
    /// clip-space write instead of the host patching the HLSL text.
    #[serde(default)]
    flip_y: bool,
    /// Global fragment input whose `.y` is flipped (lightmap pass); omitted when absent.
    #[serde(default)]
    flip_texcoord: Option<String>,
    /// Collect loose uniforms into this `std140` block (shader packs).
    uniform_block: Option<String>,
    /// Rewrite fixed-function GLSL builtins that shader packs still use.
    #[serde(default)]
    legacy_builtins: bool,
    /// Vertex stage outputs in declaration order; a fragment conversion pads its inputs to this
    /// interface so D3D12 packs both signatures the same way. Omitted for other stages.
    #[serde(default)]
    vertex_outputs: Vec<crate::stage_io::StageOutput>,
}

fn default_entry() -> String {
    "main".to_owned()
}

/// Returns the ABI version the loaded library implements.
#[unsafe(no_mangle)]
pub extern "C" fn illusion_shader_abi_version() -> u32 {
    ABI_VERSION
}

/// Converts the shader described by `request_json`.
///
/// Returns `0` and stores a success object in `*out_response_json`, or returns
/// `1` and stores `{"ok": false, "kind": ..., "message": ...}` there. Returns
/// `2` when the arguments are unusable (null pointer, not UTF-8, interior NUL);
/// `*out_response_json` is untouched then.
///
/// The response belongs to the library: free it with
/// [`illusion_shader_free`] once copied.
///
/// # Safety
///
/// `request` must be a NUL-terminated C string and `out_response_json` must
/// point to writable storage for one `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn illusion_shader_convert(
    request: *const c_char,
    out_response_json: *mut *mut c_char,
) -> i32 {
    if request.is_null() || out_response_json.is_null() {
        return 2;
    }
    let Ok(request_text) = (unsafe { CStr::from_ptr(request) }).to_str() else {
        return 2;
    };
    let (code, response) = std::panic::catch_unwind(|| convert_json(request_text))
        .unwrap_or_else(|_| (1, error_json("panic", "the conversion panicked")));
    let Ok(response) = CString::new(response) else {
        return 2;
    };
    unsafe { *out_response_json = response.into_raw() };
    code
}

/// Frees a string returned by [`illusion_shader_convert`].
///
/// # Safety
///
/// `text` must be a pointer this library returned, or null, and must not be
/// freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn illusion_shader_free(text: *mut c_char) {
    if text.is_null() {
        return;
    }
    drop(unsafe { CString::from_raw(text) });
}

fn convert_json(request_text: &str) -> (i32, String) {
    let request: ConvertRequest = match serde_json::from_str(request_text) {
        Ok(request) => request,
        Err(error) => return (1, error_json("request", &error.to_string())),
    };
    match convert(&request) {
        Ok(compiled) => (
            0,
            json!({
                "ok": true,
                "hlsl": compiled.hlsl,
                "manifest": compiled.manifest,
                "warnings": compiled.warnings,
            })
            .to_string(),
        ),
        Err(error) => (1, error_json(error.kind(), &error.to_string())),
    }
}

fn convert(request: &ConvertRequest) -> Result<CompiledShader, CompileError> {
    let path = PathBuf::from(&request.path);
    let stage = match &request.stage {
        Some(name) => stage_from_name(name).ok_or_else(|| {
            CompileError::Frontend(format!(
                "unknown stage `{name}`; expected vertex, fragment or compute"
            ))
        })?,
        None => stage_from_path(&path).ok_or_else(|| {
            CompileError::Frontend(format!(
                "cannot derive the stage from `{}`; pass `stage` in the request",
                request.path
            ))
        })?,
    };
    let shader_model = match &request.shader_model {
        Some(name) => parse_shader_model(name).ok_or_else(|| {
            CompileError::Frontend(format!(
                "unknown shader model `{name}`; expected 6_0 .. 6_9"
            ))
        })?,
        None => ShaderModel::V6_0,
    };
    let register_layout = match &request.register_layout {
        Some(name) => RegisterLayout::from_name(name).ok_or_else(|| {
            CompileError::Frontend(format!(
                "unknown register layout `{name}`; expected spirv or host"
            ))
        })?,
        None => RegisterLayout::SpirvBases,
    };
    let options = CompileOptions {
        include_roots: request.include_roots.iter().map(PathBuf::from).collect(),
        defines: request.defines.clone(),
        entry_point: request.entry.clone(),
        shader_model,
        space: request.space.unwrap_or(0),
        register_layout,
        bindings: request.bindings.clone(),
        vertex_attributes: request.vertex_attributes.clone(),
        flip_y: request.flip_y,
        flip_texcoord: request.flip_texcoord.clone(),
        uniform_block: request.uniform_block.clone(),
        legacy_builtins: request.legacy_builtins,
        vertex_outputs: request.vertex_outputs.clone(),
        ..Default::default()
    };
    compile::compile_with_source(&path, request.source.as_deref(), stage, &options)
}

fn error_json(kind: &str, message: &str) -> String {
    json!({ "ok": false, "kind": kind, "message": message }).to_string()
}

fn stage_from_name(name: &str) -> Option<ShaderStage> {
    match name {
        "vertex" | "vert" | "vs" => Some(ShaderStage::Vertex),
        "fragment" | "frag" | "ps" | "pixel" => Some(ShaderStage::Fragment),
        "compute" | "comp" | "cs" => Some(ShaderStage::Compute),
        _ => None,
    }
}

fn parse_shader_model(name: &str) -> Option<ShaderModel> {
    match name {
        "6_0" => Some(ShaderModel::V6_0),
        "6_1" => Some(ShaderModel::V6_1),
        "6_2" => Some(ShaderModel::V6_2),
        "6_3" => Some(ShaderModel::V6_3),
        "6_4" => Some(ShaderModel::V6_4),
        "6_5" => Some(ShaderModel::V6_5),
        "6_6" => Some(ShaderModel::V6_6),
        "6_7" => Some(ShaderModel::V6_7),
        "6_8" => Some(ShaderModel::V6_8),
        "6_9" => Some(ShaderModel::V6_9),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const FRAGMENT: &str = "\
#version 330
layout(std140) uniform Globals { vec4 Color; };
out vec4 fragColor;
void main() { fragColor = Color; }
";

    const TEXTURED_FRAGMENT: &str = r"
#version 330
layout(std140) uniform Globals { vec4 Tint; };
uniform sampler2D Sampler0;
layout(location = 0) in vec2 texCoord;
out vec4 fragColor;
void main() { fragColor = texture(Sampler0, texCoord) * Tint; }
";

    const VERTEX: &str = r"
#version 330
in vec3 Position;
out vec4 vertexColor;
void main() {
    vertexColor = vec4(1.0);
    gl_Position = vec4(Position, 1.0);
}
";

    /// Calls the C entry points exactly as a host would.
    fn call_abi(request_text: &str) -> (i32, Value) {
        let request = CString::new(request_text).expect("request has no NUL");
        let mut response: *mut c_char = std::ptr::null_mut();
        let code = unsafe { illusion_shader_convert(request.as_ptr(), &mut response) };
        assert!(!response.is_null(), "the ABI must always return a string");
        let text = unsafe { CStr::from_ptr(response) }
            .to_str()
            .expect("response is UTF-8")
            .to_owned();
        unsafe { illusion_shader_free(response) };
        (code, serde_json::from_str(&text).expect("response is JSON"))
    }

    #[test]
    fn reports_abi_version() {
        assert_eq!(illusion_shader_abi_version(), ABI_VERSION);
    }

    #[test]
    fn converts_inline_source() {
        let request = json!({
            "path": "core/probe.fsh",
            "source": FRAGMENT,
            "stage": "fragment"
        });
        let (code, response) = call_abi(&request.to_string());
        assert_eq!(code, 0, "{response}");
        assert_eq!(response["ok"], true);
        assert!(
            response["hlsl"]
                .as_str()
                .expect("hlsl")
                .contains("cbuffer global"),
            "{response}"
        );
        assert_eq!(response["manifest"]["schema"], "illusion.shader-manifest/1");
    }

    #[test]
    fn reports_errors_as_json() {
        let request = json!({ "path": "core/probe.fsh", "stage": "pixelish" });
        let (code, response) = call_abi(&request.to_string());
        assert_eq!(code, 1);
        assert_eq!(response["ok"], false);
        assert_eq!(response["kind"], "glsl-frontend");
    }

    #[test]
    fn rejects_unknown_request_fields() {
        let (code, response) = call_abi(r#"{"path":"x.fsh","register_layout":"host","mystery":1}"#);
        assert_eq!(code, 1, "{response}");
        assert_eq!(response["ok"], false);
        assert_eq!(response["kind"], "request");
        assert!(
            response["message"].as_str().unwrap().contains("mystery"),
            "{response}"
        );
    }
    #[test]
    fn rejects_malformed_requests() {
        let (code, response) = call_abi("{\"path\": 1}");
        assert_eq!(code, 1);
        assert_eq!(response["kind"], "request");
    }

    /// Every documented request field must reach the compiler. A field that serde silently
    /// ignores is invisible at runtime, which is how the `bindings` field was once dropped.
    #[test]
    fn honours_every_documented_request_field() {
        let request = json!({
            "path": "core/contract.fsh",
            "source": TEXTURED_FRAGMENT,
            "stage": "fragment",
            "entry": "main",
            "include_roots": [],
            "defines": {},
            "shader_model": "6_9",
            "space": 0,
            "register_layout": "host",
            "bindings": { "Globals": 3, "Sampler0": 7 },
            "vertex_attributes": [],
            "flip_y": false,
            "flip_texcoord": "texCoord"
        });
        let (code, response) = call_abi(&request.to_string());
        assert_eq!(code, 0, "{response}");
        let hlsl = response["hlsl"].as_str().expect("hlsl");
        for expected in [
            "cbuffer global : register(b3)",
            "register(t7)",
            "register(s7)",
            "texCoordFlipped",
        ] {
            assert!(hlsl.contains(expected), "missing {expected} in:\n{hlsl}");
        }

        let vertex = json!({
            "path": "core/contract.vsh",
            "source": VERTEX,
            "stage": "vertex",
            "flip_y": true
        });
        let (code, response) = call_abi(&vertex.to_string());
        assert_eq!(code, 0, "{response}");
        let hlsl = response["hlsl"].as_str().expect("hlsl");
        assert!(hlsl.contains("gl_Position.y"), "{hlsl}");
    }

    /// The vertex stage's interface decides which fragment inputs exist: D3D12 packs each stage's
    /// inputs on its own, so a fragment that skips an input the vertex writes links only if the
    /// converter fills it in. This drives that field over the ABI.
    #[test]
    fn pads_fragment_inputs_from_the_vertex_outputs() {
        let request = json!({
            "path": "core/stage_pad.fsh",
            "source": "#version 450\nlayout(location = 1) in vec2 uv;\nlayout(location = 0) out vec4 color;\nvoid main() { color = vec4(uv, 0.0, 1.0); }\n",
            "stage": "fragment",
            "include_roots": [],
            "defines": {},
            "vertex_outputs": [
                { "location": 0, "hlsl_type": "float", "interpolation": "Perspective" },
                { "location": 1, "hlsl_type": "float2", "interpolation": "Perspective" }
            ]
        });
        let (code, response) = call_abi(&request.to_string());
        assert_eq!(code, 0, "{response}");
        let hlsl = response["hlsl"].as_str().expect("hlsl");
        for expected in ["dxr_pad0", "LOC0", "LOC1"] {
            assert!(hlsl.contains(expected), "missing {expected} in:\n{hlsl}");
        }
        let warnings = response["manifest"]["warnings"]
            .as_array()
            .expect("warnings");
        assert!(
            warnings.iter().any(|warning| warning
                .as_str()
                .is_some_and(|text| text.contains("padded 1 fragment input"))),
            "{warnings:?}"
        );
    }
}
