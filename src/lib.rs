//! GLSL -> HLSL conversion for the Illusion shader pipeline.
//!
//! The crate owns the conversion stage only: it turns Minecraft GLSL (a
//! flattened translation unit) into HLSL plus a manifest that names every
//! resource register and stage interface slot. The renderer consumes the
//! manifest; it never parses the HLSL itself.
//!
//! Pipeline: `preprocess` -> `dialect` -> naga frontend/validator -> HLSL
//! backend (with `static_samplers`) -> `manifest`. `verify` closes the loop by
//! compiling the HLSL with DXC and comparing its reflection with the manifest.

pub mod abi;
pub mod compile;
pub mod dialect;
pub mod lexer;
pub mod manifest;
pub mod preprocess;
pub mod report;
pub mod rt_light_contract;
pub mod stage_io;
pub mod verify;

pub use compile::{
    CompileOptions, CompiledShader, Error as CompileError, RegisterLayout, compile,
    stage_from_path, write_outputs,
};
pub use stage_io::StageOutput;
