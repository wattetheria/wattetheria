//! Integration test for trust-level capability enforcement.

use wattetheria_kernel::capabilities::{CapabilityPolicy, TrustLevel};

#[test]
fn capability_policy_enforces_levels() {
    let policy = CapabilityPolicy::default();
    assert!(policy.is_allowed(TrustLevel::Trusted, "wallet.send"));
    assert!(!policy.is_allowed(TrustLevel::Untrusted, "wallet.send"));
    assert!(
        policy
            .assert_allowed(TrustLevel::Verified, "p2p.publish")
            .is_ok()
    );
}
