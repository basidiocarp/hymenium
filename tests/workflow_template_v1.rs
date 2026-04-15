//! Round-trip test for the septa workflow-template-v1 fixture.
//!
//! Deserializes the shared fixture from disk, checks field invariants, and
//! re-serializes to confirm the JSON round-trips without error.

use hymenium::workflow::template::{load_from_json, AgentRole, ProcessRole};

#[test]
fn workflow_template_v1_round_trip() {
    // Load the fixture from disk via CARGO_MANIFEST_DIR so the path works
    // regardless of where `cargo test` is invoked from.
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");
    let fixture_path = std::path::Path::new(&manifest_dir)
        .parent()
        .expect("manifest dir should have a parent (workspace root)")
        .join("septa/fixtures/workflow-template-v1.example.json");

    let json = std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
        panic!(
            "failed to read fixture at {}: {}",
            fixture_path.display(),
            e
        )
    });

    let template = load_from_json(&json).expect("fixture should deserialize into WorkflowTemplate");

    // Phase 0: implement — ProcessRole::Implementer, agent_role = Worker
    assert_eq!(
        template.phases[0].role,
        ProcessRole::Implementer,
        "phase[0].role should be Implementer"
    );
    assert_eq!(
        template.phases[0].agent_role,
        Some(AgentRole::Worker),
        "phase[0].agent_role should be Worker"
    );

    // Phase 1: audit — ProcessRole::Auditor, agent_role = OutputVerifier
    assert_eq!(
        template.phases[1].role,
        ProcessRole::Auditor,
        "phase[1].role should be Auditor"
    );
    assert_eq!(
        template.phases[1].agent_role,
        Some(AgentRole::OutputVerifier),
        "phase[1].agent_role should be OutputVerifier"
    );

    // Re-serialize and parse again to confirm round-trip stability.
    let re_serialized =
        serde_json::to_string(&template).expect("template should serialize to JSON");
    load_from_json(&re_serialized).expect("re-serialized JSON should deserialize without error");
}
