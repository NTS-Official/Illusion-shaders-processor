//! GLSL -> HLSL conversion for the Illusion shader pipeline.
//!
//! The crate owns the conversion stage only: it turns Minecraft GLSL (a
//! flattened translation unit) into HLSL plus a manifest that names every
//! resource register and stage interface slot. The renderer consumes the
//! manifest; it never parses the HLSL itself.
//!
//! Pipeline: `preprocess` -> `dialect` -> naga frontend/validator -> HLSL
//! backend (with `static_samplers`) -> `manifest`.

pub mod compile;
pub mod dialect;
pub mod lexer;
pub mod manifest;
pub mod preprocess;
pub mod rt_light_contract;

pub use compile::{
    CompileOptions, CompiledShader, Error as CompileError, compile, stage_from_path, write_outputs,
};
