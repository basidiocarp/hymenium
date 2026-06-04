use hymenium::dispatch::{CapabilityRequirements, TaskPacket};

// Mirrors the schema's `required` array exactly. Optional fields
// (context_budget, response_format, request_heartbeat) are intentionally
// absent — they are validated by `task_packet_no_extra_fields_beyond_septa_contract`.
const SEPTA_REQUIRED_FIELDS: &[&str] = &[
    "schema_version",
    "task_id",
    "workflow_id",
    "phase_id",
    "goal",
    "constraints",
    "dependencies",
    "acceptance_criteria",
    "capability_requirements",
    "escalation_conditions",
];

fn make_packet() -> TaskPacket {
    TaskPacket::new(
        "01JNQWF0000000000000000001",
        "implement",
        "Implement the handoff parser",
        vec!["Write scope limited to hymenium/src/".to_string()],
        vec!["cargo test passes".to_string()],
        CapabilityRequirements {
            tier: "sonnet".to_string(),
            tools: vec!["bash".to_string(), "read".to_string()],
        },
    )
}

#[test]
fn task_packet_covers_all_septa_required_fields() {
    let packet = make_packet();
    let json = serde_json::to_string(&packet).expect("serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("round-trip parse");
    let obj = value.as_object().expect("root is object");

    for field in SEPTA_REQUIRED_FIELDS {
        assert!(
            obj.contains_key(*field),
            "missing required septa field: {field}"
        );
    }
}

#[test]
fn task_packet_schema_version_matches_septa_contract() {
    // septa task-packet-v1.schema.json specifies const "1.0"
    let packet = make_packet();
    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&packet).unwrap()).unwrap();
    assert_eq!(value["schema_version"], "1.0");
}

#[test]
fn task_packet_capability_requirements_has_septa_required_subfields() {
    let packet = make_packet();
    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&packet).unwrap()).unwrap();
    let cap = &value["capability_requirements"];
    assert!(
        cap.get("tier").is_some(),
        "capability_requirements.tier missing"
    );
    assert!(
        cap.get("tools").is_some(),
        "capability_requirements.tools missing"
    );
    // septa capability_requirements disallows additionalProperties
    let cap_obj = cap.as_object().unwrap();
    assert_eq!(
        cap_obj.len(),
        2,
        "capability_requirements has unexpected extra fields: {:?}",
        cap_obj.keys().collect::<Vec<_>>()
    );
}

#[test]
fn task_packet_omits_context_budget_when_not_set() {
    // context_budget is optional in septa; skip_serializing_if = "Option::is_none" must be wired
    let packet = make_packet();
    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&packet).unwrap()).unwrap();
    assert!(
        value.get("context_budget").is_none(),
        "context_budget should be absent when None"
    );
}

#[test]
fn task_packet_no_extra_fields_beyond_septa_contract() {
    // septa schema uses additionalProperties: false at the root
    let packet = make_packet();
    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&packet).unwrap()).unwrap();
    let obj = value.as_object().unwrap();

    let allowed_fields: std::collections::HashSet<&str> = [
        "schema_version",
        "task_id",
        "workflow_id",
        "phase_id",
        "goal",
        "constraints",
        "dependencies",
        "acceptance_criteria",
        "capability_requirements",
        "context_budget",
        "escalation_conditions",
        "response_format",
        "request_heartbeat",
    ]
    .into_iter()
    .collect();

    for key in obj.keys() {
        assert!(
            allowed_fields.contains(key.as_str()),
            "unexpected field in serialized TaskPacket not allowed by septa schema: {key}"
        );
    }
}
