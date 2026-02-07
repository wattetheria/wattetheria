//! JSON Schema conformance helpers for protocol payload validation.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[must_use]
pub fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("schemas")
}

pub fn load_schema(name: &str) -> Result<Value> {
    let path = schema_dir().join(name);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read schema path {}", path.display()))?;
    serde_json::from_str(&raw).context("parse schema json")
}

pub fn validate(schema_name: &str, payload: &Value) -> Result<()> {
    let schema = load_schema(schema_name)?;
    // Compile and execute JSON Schema validation at runtime.
    let compiled = jsonschema::validator_for(&schema).context("compile schema")?;
    if let Err(error) = compiled.validate(payload) {
        anyhow::bail!("schema validation failed: {error}");
    }
    Ok(())
}
