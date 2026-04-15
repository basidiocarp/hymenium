//! Structured task packets conforming to the `task-packet-v1` septa contract.
//!
//! A `TaskPacket` is the concrete unit of work delivered to a Worker agent.
//! It carries everything needed to execute a phase: goal, constraints,
//! dependencies, acceptance criteria, capability requirements, context budget,
//! and escalation conditions.
//!
//! Producer: Hymenium (Packet Compiler role).
//! Consumer: Worker agent.

use serde::{Deserialize, Serialize};
use ulid::Ulid;

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

/// Capability requirements for the Worker executing a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRequirements {
    /// Minimum agent tier required to execute this task.
    pub tier: String,
    /// Tool names the Worker must have access to.
    pub tools: Vec<String>,
}

/// Optional context budget limits for a task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Maximum token budget for this task execution.
    pub max_tokens: u64,
    /// Maximum number of turns allowed for this task execution.
    pub max_turns: u64,
}

/// Structured task packet conforming to `task-packet-v1`.
///
/// Carries the complete context a Worker needs to execute a phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPacket {
    pub schema_version: String,
    /// Unique task identifier (ULID).
    pub task_id: String,
    /// Parent workflow identifier (ULID).
    pub workflow_id: String,
    /// Which phase within the workflow this packet belongs to.
    pub phase_id: String,
    /// Concrete description of what success looks like for this task.
    pub goal: String,
    /// Hard rules the Worker must respect.
    pub constraints: Vec<String>,
    /// Task IDs or artifact references this packet depends on.
    pub dependencies: Vec<String>,
    /// Exact conditions the Output Verifier must see satisfied.
    pub acceptance_criteria: Vec<String>,
    /// Capability requirements for the Worker.
    pub capability_requirements: CapabilityRequirements,
    /// Optional context budget limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_budget: Option<ContextBudget>,
    /// Conditions under which the Worker must escalate rather than retry.
    pub escalation_conditions: Vec<String>,
}

impl TaskPacket {
    /// Build a new `TaskPacket`, generating a fresh ULID for `task_id`.
    pub fn new(
        workflow_id: impl Into<String>,
        phase_id: impl Into<String>,
        goal: impl Into<String>,
        constraints: Vec<String>,
        acceptance_criteria: Vec<String>,
        required_capabilities: CapabilityRequirements,
    ) -> Self {
        Self {
            schema_version: "1.0".to_string(),
            task_id: Ulid::new().to_string(),
            workflow_id: workflow_id.into(),
            phase_id: phase_id.into(),
            goal: goal.into(),
            constraints,
            dependencies: Vec::new(),
            acceptance_criteria,
            capability_requirements: required_capabilities,
            context_budget: None,
            escalation_conditions: vec![
                "Handoff spec is ambiguous or contradicts itself".to_string(),
                "Required dependency task is not yet complete".to_string(),
                "Three consecutive test failures after repair attempts".to_string(),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_packet_serializes_to_json() {
        let packet = TaskPacket::new(
            "01JNQWF0000000000000000001",
            "implement",
            "Implement the handoff parser",
            vec!["Write scope limited to hymenium/src/".to_string()],
            vec!["cargo test passes".to_string()],
            CapabilityRequirements {
                tier: "sonnet".to_string(),
                tools: vec!["bash".to_string(), "read".to_string()],
            },
        );

        let json = serde_json::to_string(&packet).expect("serialize");
        assert!(json.contains("acceptance_criteria"));
        assert!(!json.contains("context_budget") || !json.contains("\"context_budget\":null"));
        assert!(json.contains("capability_requirements"));
        assert!(!json.contains("required_capabilities")); // field name is capability_requirements
    }

    #[test]
    fn task_packet_generates_unique_ids() {
        let p1 = TaskPacket::new(
            "01JNQWF0000000000000000001",
            "implement",
            "goal",
            vec![],
            vec![],
            CapabilityRequirements {
                tier: "sonnet".to_string(),
                tools: vec![],
            },
        );
        let p2 = TaskPacket::new(
            "01JNQWF0000000000000000001",
            "audit",
            "goal",
            vec![],
            vec![],
            CapabilityRequirements {
                tier: "sonnet".to_string(),
                tools: vec![],
            },
        );
        assert_ne!(p1.task_id, p2.task_id);
    }

    #[test]
    fn task_packet_schema_version_is_one() {
        let p = TaskPacket::new(
            "01JNQWF0000000000000000001",
            "implement",
            "goal",
            vec![],
            vec![],
            CapabilityRequirements {
                tier: "haiku".to_string(),
                tools: vec![],
            },
        );
        assert_eq!(p.schema_version, "1.0");
    }
}
