//! Integration test for hashcash mint/verify admission checks.

use wattetheria_kernel::hashcash;

#[test]
fn hashcash_admission_cost_works() {
    let stamp = hashcash::mint("agent-x", 12, 250_000).expect("should find stamp");
    assert!(hashcash::verify(&stamp, "agent-x", 12));
    assert!(!hashcash::verify(&stamp, "agent-y", 12));
}
