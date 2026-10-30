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

use crate::compile::{self, CompileOptions, CompiledShader};
use crate::compile::{Error as CompileError, stage_from_path};

/// Bumped whenever the request or response schema changes incompatibly.
pub const ABI_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
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
            CompileError::Frontend(format!("unknown shader model `{name}`; expected 6_0 .. 6_9"))
        })?,
        None => ShaderModel::V6_0,
    };
    let options = CompileOptions {
        include_roots: request.include_roots.iter().map(PathBuf::from).collect(),
        defines: request.defines.clone(),
        entry_point: request.entry.clone(),
        shader_model,
        space: request.space.unwrap_or(0),
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
    fn rejects_malformed_requests() {
        let (code, response) = call_abi("{\"path\": 1}");
        assert_eq!(code, 1);
        assert_eq!(response["kind"], "request");
    }
}
