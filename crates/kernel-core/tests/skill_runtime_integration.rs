//! Integration test for sandboxed skill runtime capability checks.

use wattetheria_kernel::capabilities::{CapabilityPolicy, TrustLevel};
use wattetheria_kernel::skill_runtime::{EchoSkill, SkillManifest, SkillRuntime};

#[test]
fn skill_runtime_sandbox_guards_untrusted_calls() {
    let mut runtime = SkillRuntime::new(CapabilityPolicy::default());
    runtime.register(
        SkillManifest {
            name: "echo".to_string(),
            version: "0.1.0".to_string(),
            required_capabilities: vec!["p2p.publish".to_string()],
        },
        EchoSkill,
    );

    assert!(
        runtime
            .invoke(
                "echo",
                "0.1.0",
                TrustLevel::Untrusted,
                serde_json::json!({"value":1})
            )
            .is_err()
    );

    assert!(
        runtime
            .invoke(
                "echo",
                "0.1.0",
                TrustLevel::Verified,
                serde_json::json!({"value":1})
            )
            .is_ok()
    );
}
