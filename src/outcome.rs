//! Workflow outcome records.
//!
//! Defines [`WorkflowOutcome`], which is the terminal summary emitted when a
//! workflow reaches a final state. The wire shape matches the
//! `septa/workflow-outcome-v1.schema.json` contract exactly.
//!
//! # Mapping: `FailureKind` → `TerminalFailureType`
//!
//! [`FailureKind`] is the internal detection-site enum (why a phase failed).
//! [`TerminalFailureType`] is the septa wire category (why the whole workflow
//! ended). The mapping is deterministic:
//!
//! | Internal `FailureKind`    | Wire `TerminalFailureType`  | Rationale |
//! |---------------------------|-----------------------------|-----------|
//! | `SpecAmbiguity`           | `Unknown`                   | Ambiguity is an input quality issue, not a gate/exec category |
//! | `TaskTooLarge`            | `GateViolation`             | Decomposition gate failed before dispatch |
//! | `MissingDependency`       | `GateViolation`             | Entry gate blocked; dependency not ready |
//! | `ExecutionIncomplete`     | `StallTimeout`              | Agent ran but stalled without completing |
//! | `ScopeViolation`          | `AuditRejected`             | Audit surface — agent violated scope bounds |
//! | `ContractMismatch`        | `VerificationFailed`        | Verifier rejected output for schema non-conformance |
//! | `MinorDefect`             | `VerificationFailed`        | Verifier found a defect; repair loop exhausted |

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::failure::FailureKind;
use crate::workflow::engine::{PhaseStatus, WorkflowInstance};
use crate::workflow::template::AgentRole;
use crate::workflow::WorkflowId;

// ---------------------------------------------------------------------------
// Wire types (septa workflow-outcome-v1)
// ---------------------------------------------------------------------------

/// Runtime and session identity context for a workflow outcome.
///
/// Matches the `runtime_identity` nested object in
/// `septa/workflow-outcome-v1.schema.json`. All fields are optional.
/// Fields that are `None` are omitted from the serialised JSON.
///
/// This identity is observational — it records the execution context that
/// was present when the outcome was produced so retrospective route analysis
/// can account for host, worktree, or session differences. It does NOT alter
/// routing policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeIdentity {
    /// Execution-host runtime session identity when the producer can expose one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_session_id: Option<String>,
    /// Canonical repository or workspace root for the workflow scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    /// Git worktree identifier when the workflow is scoped to one worktree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    /// Execution host or adapter identity, e.g. `"volva:anthropic"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_ref: Option<String>,
    /// Workspace or multi-repo scoping identifier when the workflow spans
    /// more than one project root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// Layer where the root cause of a failure was identified.
///
/// Matches the `root_cause_layer` enum in `septa/workflow-outcome-v1.schema.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RootCauseLayer {
    Spec,
    Decomposition,
    Execution,
    Verification,
    Infrastructure,
}

/// Terminal state of a workflow, matching the septa `terminal_status` enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Completed,
    Failed,
    Cancelled,
}

/// Terminal failure category, matching the septa `failure_type` enum.
///
/// See the module-level doc for the mapping from [`FailureKind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalFailureType {
    GateViolation,
    VerificationFailed,
    StallTimeout,
    OperatorCancelled,
    AuditRejected,
    InfrastructureError,
    Unknown,
}

impl FailureKind {
    /// Map an internal failure kind to the wire terminal failure type.
    ///
    /// See the module-level table in [`crate::outcome`] for rationale.
    pub fn to_terminal_failure_type(self) -> TerminalFailureType {
        match self {
            FailureKind::SpecAmbiguity => TerminalFailureType::Unknown,
            FailureKind::TaskTooLarge | FailureKind::MissingDependency => {
                TerminalFailureType::GateViolation
            }
            FailureKind::ExecutionIncomplete => TerminalFailureType::StallTimeout,
            FailureKind::ScopeViolation => TerminalFailureType::AuditRejected,
            FailureKind::ContractMismatch | FailureKind::MinorDefect => {
                TerminalFailureType::VerificationFailed
            }
        }
    }
}

/// Status of a single phase in the route taken by a workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStepStatus {
    Completed,
    Failed,
    Skipped,
}

/// One step in the execution route of a workflow outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteStep {
    pub phase_id: String,
    /// The serialised wire name (e.g. `"Worker"`, `"Output Verifier"`).
    pub role: AgentRole,
    pub status: RouteStepStatus,
}

/// Terminal outcome record for a workflow.
///
/// Serialises to the `workflow-outcome-v1` septa wire shape. All field names
/// are `snake_case` to match the schema; `AgentRole` carries its own
/// `#[serde(rename = ...)]` attrs so role strings come out as human-readable
/// names like `"Worker"` and `"Output Verifier"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowOutcome {
    /// Always `"1.0"`.
    pub schema_version: String,
    pub workflow_id: WorkflowId,
    pub template_id: String,
    pub handoff_path: String,
    pub terminal_status: TerminalStatus,
    /// `null` when `terminal_status` is `completed`.
    pub failure_type: Option<TerminalFailureType>,
    /// Total phase dispatches across the workflow lifetime (≥ 1).
    pub attempt_count: u32,
    /// Ordered trace of phases that ran.
    pub route_taken: Vec<RouteStep>,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    /// Optional verifier confidence score (0.0–1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Layer where the root cause was identified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_cause_layer: Option<RootCauseLayer>,
    /// Optional runtime and session identity context.
    ///
    /// Use [`WorkflowOutcome::with_runtime_identity`] to attach identity after
    /// building. Omitted from JSON when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_identity: Option<RuntimeIdentity>,
}

impl WorkflowOutcome {
    /// Build an outcome from a [`WorkflowInstance`], an optional typed failure,
    /// and the wall-clock completion time.
    ///
    /// `attempt_count` is derived from the sum of `retry_count + 1` across all
    /// phase states (each dispatch is one attempt). Minimum is 1.
    pub fn build(
        instance: &WorkflowInstance,
        failure: Option<&crate::failure::TypedFailure>,
        now: DateTime<Utc>,
    ) -> Self {
        use crate::workflow::engine::WorkflowStatus;

        let terminal_status = match &instance.status {
            WorkflowStatus::Completed => TerminalStatus::Completed,
            WorkflowStatus::Cancelled => TerminalStatus::Cancelled,
            _ => TerminalStatus::Failed,
        };

        let failure_type = failure
            .map(|f| f.kind.to_terminal_failure_type())
            .or_else(|| {
                // If the workflow failed but no TypedFailure was provided,
                // emit Unknown rather than leaving it null on a failed outcome.
                if terminal_status == TerminalStatus::Failed {
                    Some(TerminalFailureType::Unknown)
                } else {
                    None
                }
            });

        // Total dispatches: each phase contributes (retry_count + 1) attempts.
        let attempt_count = instance
            .phase_states
            .iter()
            .map(|p| p.retry_count + 1)
            .sum::<u32>()
            .max(1);

        let route_taken = instance
            .phase_states
            .iter()
            .filter(|p| p.status != PhaseStatus::Pending)
            .map(|p| RouteStep {
                phase_id: p.phase_id.clone(),
                role: p.role.clone(),
                status: match p.status {
                    PhaseStatus::Completed => RouteStepStatus::Completed,
                    PhaseStatus::Skipped => RouteStepStatus::Skipped,
                    // Failed and any other non-terminal states (Active edge case):
                    // treat as failed to keep the record unambiguous.
                    _ => RouteStepStatus::Failed,
                },
            })
            .collect();

        Self {
            schema_version: "1.0".to_string(),
            workflow_id: instance.workflow_id.clone(),
            template_id: instance.template.template_id.clone(),
            handoff_path: instance.handoff_path.clone(),
            terminal_status,
            failure_type,
            attempt_count,
            route_taken,
            started_at: instance.created_at,
            completed_at: now,
            confidence: None,
            root_cause_layer: None,
            runtime_identity: None,
        }
    }

    /// Attach runtime identity context to this outcome, returning `self`.
    ///
    /// Call after [`WorkflowOutcome::build`] when the execution environment
    /// context is available. Existing callers of `build` are not affected
    /// because identity defaults to `None`.
    #[must_use]
    pub fn with_runtime_identity(mut self, identity: RuntimeIdentity) -> Self {
        self.runtime_identity = Some(identity);
        self
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::{FailureKind, TypedFailure};
    use crate::workflow::engine::{PhaseStatus, WorkflowInstance};
    use crate::workflow::template::impl_audit_default;
    use crate::workflow::WorkflowId;

    fn make_instance(id: &str) -> WorkflowInstance {
        WorkflowInstance::new(
            WorkflowId(id.to_string()),
            impl_audit_default(),
            "/handoffs/test.md",
        )
    }

    fn completed_instance() -> WorkflowInstance {
        use crate::workflow::engine::WorkflowStatus;
        let mut inst = make_instance("01JNQWF0000000000000000010");
        inst.phase_states[0].status = PhaseStatus::Completed;
        inst.phase_states[1].status = PhaseStatus::Completed;
        inst.status = WorkflowStatus::Completed;
        inst
    }

    fn failed_instance() -> WorkflowInstance {
        use crate::workflow::engine::WorkflowStatus;
        let mut inst = make_instance("01JNQWF0000000000000000011");
        inst.phase_states[0].status = PhaseStatus::Failed;
        inst.status = WorkflowStatus::Failed;
        inst
    }

    // -- FailureKind → TerminalFailureType mapping ---------------------------

    #[test]
    fn spec_ambiguity_maps_to_unknown() {
        assert_eq!(
            FailureKind::SpecAmbiguity.to_terminal_failure_type(),
            TerminalFailureType::Unknown
        );
    }

    #[test]
    fn task_too_large_maps_to_gate_violation() {
        assert_eq!(
            FailureKind::TaskTooLarge.to_terminal_failure_type(),
            TerminalFailureType::GateViolation
        );
    }

    #[test]
    fn missing_dependency_maps_to_gate_violation() {
        assert_eq!(
            FailureKind::MissingDependency.to_terminal_failure_type(),
            TerminalFailureType::GateViolation
        );
    }

    #[test]
    fn execution_incomplete_maps_to_stall_timeout() {
        assert_eq!(
            FailureKind::ExecutionIncomplete.to_terminal_failure_type(),
            TerminalFailureType::StallTimeout
        );
    }

    #[test]
    fn scope_violation_maps_to_audit_rejected() {
        assert_eq!(
            FailureKind::ScopeViolation.to_terminal_failure_type(),
            TerminalFailureType::AuditRejected
        );
    }

    #[test]
    fn contract_mismatch_maps_to_verification_failed() {
        assert_eq!(
            FailureKind::ContractMismatch.to_terminal_failure_type(),
            TerminalFailureType::VerificationFailed
        );
    }

    #[test]
    fn minor_defect_maps_to_verification_failed() {
        assert_eq!(
            FailureKind::MinorDefect.to_terminal_failure_type(),
            TerminalFailureType::VerificationFailed
        );
    }

    // -- WorkflowOutcome::build ----------------------------------------------

    #[test]
    fn build_completed_outcome_has_null_failure_type() {
        let inst = completed_instance();
        let now = Utc::now();
        let outcome = WorkflowOutcome::build(&inst, None, now);
        assert_eq!(outcome.terminal_status, TerminalStatus::Completed);
        assert!(outcome.failure_type.is_none());
    }

    #[test]
    fn build_failed_outcome_with_typed_failure() {
        let inst = failed_instance();
        let failure = TypedFailure::new(FailureKind::ContractMismatch);
        let now = Utc::now();
        let outcome = WorkflowOutcome::build(&inst, Some(&failure), now);
        assert_eq!(outcome.terminal_status, TerminalStatus::Failed);
        assert_eq!(
            outcome.failure_type,
            Some(TerminalFailureType::VerificationFailed)
        );
    }

    #[test]
    fn build_failed_outcome_without_typed_failure_uses_unknown() {
        let inst = failed_instance();
        let now = Utc::now();
        let outcome = WorkflowOutcome::build(&inst, None, now);
        assert_eq!(outcome.terminal_status, TerminalStatus::Failed);
        assert_eq!(outcome.failure_type, Some(TerminalFailureType::Unknown));
    }

    #[test]
    fn attempt_count_minimum_one() {
        let inst = make_instance("01JNQWF0000000000000000012");
        let now = Utc::now();
        let outcome = WorkflowOutcome::build(&inst, None, now);
        assert!(outcome.attempt_count >= 1);
    }

    #[test]
    fn route_taken_excludes_pending_phases() {
        let inst = make_instance("01JNQWF0000000000000000013");
        let now = Utc::now();
        let outcome = WorkflowOutcome::build(&inst, None, now);
        // All phases are Pending in a fresh instance; route_taken should be empty.
        assert!(outcome.route_taken.is_empty());
    }

    #[test]
    fn route_taken_includes_completed_phase() {
        let inst = completed_instance();
        let now = Utc::now();
        let outcome = WorkflowOutcome::build(&inst, None, now);
        assert_eq!(outcome.route_taken.len(), 2);
        assert_eq!(outcome.route_taken[0].phase_id, "implement");
        assert_eq!(outcome.route_taken[0].status, RouteStepStatus::Completed);
    }

    // -- Serialisation -------------------------------------------------------

    #[test]
    fn outcome_serialises_required_fields() {
        let inst = completed_instance();
        let now = Utc::now();
        let outcome = WorkflowOutcome::build(&inst, None, now);
        let json = serde_json::to_value(&outcome).expect("serialize");

        // Required schema fields
        for field in &[
            "schema_version",
            "workflow_id",
            "template_id",
            "handoff_path",
            "terminal_status",
            "failure_type",
            "attempt_count",
            "route_taken",
            "started_at",
            "completed_at",
        ] {
            assert!(
                json.get(field).is_some(),
                "missing required field '{field}' in serialised outcome"
            );
        }
        assert_eq!(json["schema_version"], "1.0");
        assert_eq!(json["terminal_status"], "completed");
        assert!(json["failure_type"].is_null());
    }

    #[test]
    fn route_step_role_serialises_as_human_readable() {
        let inst = completed_instance();
        let now = Utc::now();
        let outcome = WorkflowOutcome::build(&inst, None, now);
        let json = serde_json::to_value(&outcome).expect("serialize");
        let first_step = &json["route_taken"][0];
        // AgentRole::Worker serialises as "Worker" per its serde rename attr.
        assert_eq!(first_step["role"], "Worker");
        // AgentRole::OutputVerifier serialises as "Output Verifier".
        let second_step = &json["route_taken"][1];
        assert_eq!(second_step["role"], "Output Verifier");
    }

    #[test]
    fn root_cause_layer_serialises_snake_case() {
        let cases = [
            (RootCauseLayer::Spec, "spec"),
            (RootCauseLayer::Decomposition, "decomposition"),
            (RootCauseLayer::Execution, "execution"),
            (RootCauseLayer::Verification, "verification"),
            (RootCauseLayer::Infrastructure, "infrastructure"),
        ];
        for (variant, expected) in &cases {
            let json = serde_json::to_value(variant).unwrap();
            assert_eq!(
                json, *expected,
                "RootCauseLayer::{expected:?} should serialise as \"{expected}\""
            );
        }
    }

    // -- RuntimeIdentity serialisation ----------------------------------------

    #[test]
    fn runtime_identity_present_in_json_when_set() {
        let inst = completed_instance();
        let now = Utc::now();
        let identity = RuntimeIdentity {
            runtime_session_id: Some("sess_abc123".to_string()),
            project_root: Some("/home/user/project".to_string()),
            worktree_id: Some("main".to_string()),
            host_ref: Some("volva:anthropic".to_string()),
            workspace_id: None,
        };
        let outcome = WorkflowOutcome::build(&inst, None, now).with_runtime_identity(identity);
        let json = serde_json::to_value(&outcome).expect("serialize");
        let identity_json = json
            .get("runtime_identity")
            .expect("runtime_identity must be present when set");
        assert_eq!(
            identity_json
                .get("runtime_session_id")
                .and_then(|v| v.as_str()),
            Some("sess_abc123"),
            "runtime_session_id must round-trip"
        );
        assert_eq!(
            identity_json.get("host_ref").and_then(|v| v.as_str()),
            Some("volva:anthropic"),
            "host_ref must round-trip"
        );
        // workspace_id is None so it should be absent from JSON
        assert!(
            identity_json.get("workspace_id").is_none(),
            "None fields must be omitted from JSON"
        );
    }

    #[test]
    fn runtime_identity_absent_from_json_when_not_set() {
        let inst = completed_instance();
        let now = Utc::now();
        let outcome = WorkflowOutcome::build(&inst, None, now);
        let json = serde_json::to_value(&outcome).expect("serialize");
        assert!(
            json.get("runtime_identity").is_none(),
            "runtime_identity must be absent from JSON when not set"
        );
    }

    #[test]
    fn terminal_failure_type_serialises_snake_case() {
        let json = serde_json::to_value(&TerminalFailureType::GateViolation).unwrap();
        assert_eq!(json, "gate_violation");
        let json = serde_json::to_value(&TerminalFailureType::VerificationFailed).unwrap();
        assert_eq!(json, "verification_failed");
        let json = serde_json::to_value(&TerminalFailureType::StallTimeout).unwrap();
        assert_eq!(json, "stall_timeout");
        let json = serde_json::to_value(&TerminalFailureType::AuditRejected).unwrap();
        assert_eq!(json, "audit_rejected");
        let json = serde_json::to_value(&TerminalFailureType::Unknown).unwrap();
        assert_eq!(json, "unknown");
    }
}
