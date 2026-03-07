//! Capability-gated runtime shell for skills and plugin actions.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::capabilities::{CapabilityPolicy, TrustLevel};

#[derive(Debug, Clone)]
pub struct SkillManifest {
    pub name: String,
    pub version: String,
    pub required_capabilities: Vec<String>,
}

pub trait SkillHandler: Send + Sync {
    fn run(&self, input: Value) -> Result<Value>;
}

#[derive(Debug, Default)]
pub struct EchoSkill;

impl SkillHandler for EchoSkill {
    fn run(&self, input: Value) -> Result<Value> {
        Ok(serde_json::json!({"echo": input}))
    }
}

#[derive(Debug, Clone)]
pub struct ProcessSkill {
    program: PathBuf,
}

impl ProcessSkill {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl SkillHandler for ProcessSkill {
    fn run(&self, input: Value) -> Result<Value> {
        let payload = serde_json::to_vec(&input).context("serialize process skill input")?;

        let mut child = Command::new(&self.program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn process skill {}", self.program.display()))?;

        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(&payload)
                .context("write process skill stdin")?;
        }

        let output = child
            .wait_with_output()
            .context("wait for process skill output")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "process skill exited with status {:?}: {}",
                output.status.code(),
                stderr.trim()
            );
        }

        let stdout = String::from_utf8(output.stdout).context("decode process skill stdout")?;
        serde_json::from_str(stdout.trim()).context("parse process skill stdout json")
    }
}

#[derive(Default)]
pub struct SkillRuntime {
    manifests: BTreeMap<String, SkillManifest>,
    handlers: BTreeMap<String, Arc<dyn SkillHandler>>,
    policy: CapabilityPolicy,
}

impl SkillRuntime {
    #[must_use]
    pub fn new(policy: CapabilityPolicy) -> Self {
        Self {
            manifests: BTreeMap::new(),
            handlers: BTreeMap::new(),
            policy,
        }
    }

    pub fn register<H: SkillHandler + 'static>(&mut self, manifest: SkillManifest, handler: H) {
        let key = key_of(&manifest.name, &manifest.version);
        self.manifests.insert(key.clone(), manifest);
        self.handlers.insert(key, Arc::new(handler));
    }

    pub fn invoke(
        &self,
        name: &str,
        version: &str,
        trust: TrustLevel,
        input: Value,
    ) -> Result<Value> {
        let key = key_of(name, version);
        let manifest = self
            .manifests
            .get(&key)
            .with_context(|| format!("skill not found: {key}"))?;

        for capability in &manifest.required_capabilities {
            self.policy.assert_allowed(trust, capability)?;
        }

        let handler = self
            .handlers
            .get(&key)
            .with_context(|| format!("handler not found: {key}"))?;

        handler.run(input)
    }
}

fn key_of(name: &str, version: &str) -> String {
    format!("{name}@{version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_enforces_capabilities() {
        let mut runtime = SkillRuntime::new(CapabilityPolicy::default());
        runtime.register(
            SkillManifest {
                name: "echo".to_string(),
                version: "0.1.0".to_string(),
                required_capabilities: vec!["p2p.publish".to_string()],
            },
            EchoSkill,
        );

        let denied = runtime.invoke(
            "echo",
            "0.1.0",
            TrustLevel::Untrusted,
            serde_json::json!({"hello":"world"}),
        );
        assert!(denied.is_err());

        let allowed = runtime
            .invoke(
                "echo",
                "0.1.0",
                TrustLevel::Trusted,
                serde_json::json!({"hello":"world"}),
            )
            .unwrap();
        assert_eq!(allowed["echo"]["hello"], "world");
    }

    #[cfg(unix)]
    #[test]
    fn process_skill_executes_json_contract() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("skill-process.sh");
        fs::write(
            &script_path,
            r#"#!/usr/bin/env sh
INPUT=$(cat)
printf '{"handled":true,"input":%s}\n' "$INPUT"
"#,
        )
        .unwrap();

        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();

        let mut runtime = SkillRuntime::new(CapabilityPolicy::default());
        runtime.register(
            SkillManifest {
                name: "proc-echo".to_string(),
                version: "0.1.0".to_string(),
                required_capabilities: vec!["fs.read".to_string()],
            },
            ProcessSkill::new(script_path),
        );

        let out = runtime
            .invoke(
                "proc-echo",
                "0.1.0",
                TrustLevel::Untrusted,
                serde_json::json!({"hello":"world"}),
            )
            .unwrap();

        assert_eq!(out["handled"], true);
        assert_eq!(out["input"]["hello"], "world");
    }
}
