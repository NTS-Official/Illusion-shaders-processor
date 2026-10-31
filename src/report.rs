//! Machine-readable reports for the `audit` and `verify` commands.
//!
//! The schemas are part of the output contract: CI reads them, so the field names and the
//! `schema` strings must change only with a new version. Human output on stdout is a
//! convenience; the report file is the stable interface.

use serde::Serialize;

use crate::verify::{Reflection, Verification};

/// Schema id of the file written by `audit --report`.
pub const AUDIT_SCHEMA: &str = "illusion.shader-audit/1";
/// Schema id of the file written by `verify --report`.
pub const VERIFY_SCHEMA: &str = "illusion.shader-verify/1";

#[derive(Debug, Serialize)]
pub struct AuditFailure {
    pub shader: String,
    pub error: String,
}

#[derive(Debug, Serialize)]
pub struct AuditReport {
    pub schema: &'static str,
    pub root: String,
    pub total: usize,
    pub compiled: usize,
    pub failed: usize,
    pub failures: Vec<AuditFailure>,
}

impl AuditReport {
    pub fn new(root: &str, total: usize) -> Self {
        Self {
            schema: AUDIT_SCHEMA,
            root: root.to_owned(),
            total,
            compiled: 0,
            failed: 0,
            failures: Vec::new(),
        }
    }

    pub fn add_compiled(&mut self) {
        self.compiled += 1;
    }

    pub fn add_failed(&mut self, shader: &str, error: String) {
        self.failed += 1;
        self.failures.push(AuditFailure {
            shader: shader.to_owned(),
            error,
        });
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[derive(Debug, Serialize)]
pub struct VerifyEntry {
    pub shader: String,
    pub ok: bool,
    pub bindings: usize,
    pub inputs: usize,
    pub outputs: usize,
    pub pruned: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub findings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl VerifyEntry {
    fn passed(shader: &str, verification: &Verification, warnings: &[String]) -> Self {
        Self {
            shader: shader.to_owned(),
            ok: true,
            bindings: verification.bindings,
            inputs: verification.inputs,
            outputs: verification.outputs,
            pruned: verification.pruned,
            warnings: warnings.to_vec(),
            findings: Vec::new(),
            error: None,
        }
    }

    fn failed(shader: &str, verification: &Verification, warnings: &[String]) -> Self {
        Self {
            shader: shader.to_owned(),
            ok: false,
            bindings: verification.bindings,
            inputs: verification.inputs,
            outputs: verification.outputs,
            pruned: verification.pruned,
            warnings: warnings.to_vec(),
            findings: verification
                .failures()
                .map(|finding| finding.message.clone())
                .collect(),
            error: None,
        }
    }

    fn broken(shader: &str, error: String) -> Self {
        Self {
            shader: shader.to_owned(),
            ok: false,
            bindings: 0,
            inputs: 0,
            outputs: 0,
            pruned: 0,
            warnings: Vec::new(),
            findings: Vec::new(),
            error: Some(error),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct VerifyReport {
    pub schema: &'static str,
    pub root: String,
    pub dxc: String,
    pub shader_model: String,
    pub total: usize,
    pub verified: usize,
    pub failed: usize,
    pub entries: Vec<VerifyEntry>,
}

impl VerifyReport {
    pub fn new(root: &str, dxc: &str, shader_model: &str, total: usize) -> Self {
        Self {
            schema: VERIFY_SCHEMA,
            root: root.to_owned(),
            dxc: dxc.to_owned(),
            shader_model: shader_model.to_owned(),
            total,
            verified: 0,
            failed: 0,
            entries: Vec::new(),
        }
    }

    pub fn add_passed(&mut self, shader: &str, verification: &Verification, warnings: &[String]) {
        self.verified += 1;
        self.entries
            .push(VerifyEntry::passed(shader, verification, warnings));
    }

    pub fn add_failed(&mut self, shader: &str, verification: &Verification, warnings: &[String]) {
        self.failed += 1;
        self.entries
            .push(VerifyEntry::failed(shader, verification, warnings));
    }

    /// A shader that never reached the comparison (conversion or DXC failed).
    pub fn add_error(&mut self, shader: &str, error: String) {
        self.failed += 1;
        self.entries.push(VerifyEntry::broken(shader, error));
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Flattens a reflection into the counters the CLI prints, so tests can compare numbers
/// without depending on DXC.
pub fn reflection_counts(reflection: &Reflection) -> (usize, usize) {
    (reflection.inputs.len(), reflection.outputs.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_report_keeps_schema_and_counts() {
        let mut report = AuditReport::new("shaders", 3);
        report.add_compiled();
        report.add_compiled();
        report.add_failed("core/broken.fsh", "boom".to_owned());
        let json: serde_json::Value =
            serde_json::from_str(&report.to_json_pretty().unwrap()).unwrap();
        assert_eq!(json["schema"], AUDIT_SCHEMA);
        assert_eq!(json["total"], 3);
        assert_eq!(json["compiled"], 2);
        assert_eq!(json["failed"], 1);
        assert_eq!(json["failures"][0]["shader"], "core/broken.fsh");
        assert_eq!(json["failures"][0]["error"], "boom");
    }

    #[test]
    fn verify_report_records_entries_in_order() {
        let mut report = VerifyReport::new("shaders", "dxc", "6_0", 2);
        report.add_error("core/one.vsh", "conversion failed".to_owned());
        let json: serde_json::Value =
            serde_json::from_str(&report.to_json_pretty().unwrap()).unwrap();
        assert_eq!(json["schema"], VERIFY_SCHEMA);
        assert_eq!(json["dxc"], "dxc");
        assert_eq!(json["shader_model"], "6_0");
        assert_eq!(json["verified"], 0);
        assert_eq!(json["failed"], 1);
        assert_eq!(json["entries"][0]["shader"], "core/one.vsh");
        assert_eq!(json["entries"][0]["ok"], false);
        assert_eq!(json["entries"][0]["error"], "conversion failed");
        // A broken entry has no findings, so the field stays an empty array.
        assert_eq!(json["entries"][0]["findings"], serde_json::json!([]));
    }

    #[test]
    fn verify_report_keeps_manifest_warnings() {
        let mut report = VerifyReport::new("shaders", "dxc", "6_0", 1);
        report.add_passed(
            "core/debug_point.vsh",
            &Verification::default(),
            &["dropped 1 gl_PointSize assignment(s)".to_owned()],
        );
        let json: serde_json::Value =
            serde_json::from_str(&report.to_json_pretty().unwrap()).unwrap();
        assert_eq!(json["entries"][0]["ok"], true);
        assert_eq!(
            json["entries"][0]["warnings"][0],
            "dropped 1 gl_PointSize assignment(s)"
        );
    }
    fn object_keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn audit_report_keys_match_the_documented_schema() {
        let mut report = AuditReport::new("shaders", 2);
        report.add_compiled();
        report.add_failed("core/broken.fsh", "preprocess: missing include".to_owned());
        let json: serde_json::Value =
            serde_json::from_str(&report.to_json_pretty().unwrap()).unwrap();
        assert_eq!(
            object_keys(&json),
            vec!["compiled", "failed", "failures", "root", "schema", "total"]
        );
        assert_eq!(object_keys(&json["failures"][0]), vec!["error", "shader"]);
    }

    #[test]
    fn verify_report_keys_match_the_documented_schema() {
        let mut report = VerifyReport::new("shaders", "dxc", "6_0", 2);
        report.add_passed(
            "core/ok.fsh",
            &Verification::default(),
            &["dropped 1 gl_PointSize assignment(s)".to_owned()],
        );
        report.add_error("core/broken.fsh", "conversion failed".to_owned());
        let json: serde_json::Value =
            serde_json::from_str(&report.to_json_pretty().unwrap()).unwrap();
        assert_eq!(
            object_keys(&json),
            vec![
                "dxc",
                "entries",
                "failed",
                "root",
                "schema",
                "shader_model",
                "total",
                "verified",
            ]
        );
        assert_eq!(
            object_keys(&json["entries"][0]),
            vec![
                "bindings", "findings", "inputs", "ok", "outputs", "pruned", "shader", "warnings",
            ]
        );
        assert_eq!(
            object_keys(&json["entries"][1]),
            vec![
                "bindings", "error", "findings", "inputs", "ok", "outputs", "pruned", "shader",
            ]
        );
    }
    #[test]
    fn reflection_counts_follow_the_signature_vectors() {
        let reflection = Reflection::default();
        assert_eq!(reflection_counts(&reflection), (0, 0));
    }
}
