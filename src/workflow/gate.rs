//! Phase entry/exit gate evaluation.
//!
//! Evaluates conditions to determine if a workflow can enter or exit a phase.
//! Gates enforce state transition preconditions before dispatch occurs.

use crate::workflow::WorkflowId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Error type for gate operations.
#[derive(Debug, Error)]
pub enum GateError {
    #[error("gate evaluation failed: {0}")]
    EvaluationError(String),

    #[error("gate condition not met: {condition} (context: {context})")]
    ConditionNotMet { condition: String, context: String },
}

/// Result type for gate operations.
pub type GateResult<T> = Result<T, GateError>;

/// A gate condition that can be evaluated during phase transitions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum GateCondition {
    CodeDiffExists,
    VerificationPassed,
    AuditClean,
    FindingsResolved,
    Custom(String),
}

impl std::fmt::Display for GateCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateCondition::CodeDiffExists => write!(f, "code_diff_exists"),
            GateCondition::VerificationPassed => write!(f, "verification_passed"),
            GateCondition::AuditClean => write!(f, "audit_clean"),
            GateCondition::FindingsResolved => write!(f, "findings_resolved"),
            GateCondition::Custom(s) => write!(f, "{}", s),
        }
    }
}

/// Parse a string condition into a typed `GateCondition`.
pub fn parse_gate_condition(s: &str) -> GateCondition {
    match s {
        "code_diff_exists" => GateCondition::CodeDiffExists,
        "verification_passed" => GateCondition::VerificationPassed,
        "audit_clean" => GateCondition::AuditClean,
        "findings_resolved" => GateCondition::FindingsResolved,
        other => GateCondition::Custom(other.to_string()),
    }
}

/// Context passed to gate evaluators to support condition evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateContext {
    pub workflow_id: WorkflowId,
    pub phase_id: String,
    /// Additional metadata that evaluators can use to determine condition status.
    pub metadata: HashMap<String, String>,
}

impl GateContext {
    /// Create a new gate context with empty metadata.
    pub fn new(workflow_id: WorkflowId, phase_id: impl Into<String>) -> Self {
        Self {
            workflow_id,
            phase_id: phase_id.into(),
            metadata: HashMap::new(),
        }
    }

    /// Add metadata to the gate context.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Result of evaluating a single gate condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionEvaluation {
    pub condition: GateCondition,
    pub passed: bool,
    pub reason: String,
}

/// Detailed result of evaluating a complete gate (entry or exit).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateEvaluation {
    pub phase_id: String,
    pub conditions: Vec<ConditionEvaluation>,
    /// True only if ALL conditions passed.
    pub all_passed: bool,
}

impl GateEvaluation {
    /// Check if all conditions in this gate passed.
    pub fn passed(&self) -> bool {
        self.all_passed && self.conditions.iter().all(|c| c.passed)
    }

    /// Get a summary of which conditions failed, if any.
    pub fn failures(&self) -> Vec<String> {
        self.conditions
            .iter()
            .filter(|c| !c.passed)
            .map(|c| c.condition.to_string())
            .collect()
    }
}

/// Evaluates gate conditions for phase transitions.
pub trait GateEvaluator {
    /// Evaluate a single gate condition.
    fn evaluate(
        &self,
        condition: &GateCondition,
        context: &GateContext,
    ) -> GateResult<ConditionEvaluation>;

    /// Evaluate all conditions in a set, returning true only if ALL pass.
    fn evaluate_all(
        &self,
        conditions: &[GateCondition],
        context: &GateContext,
    ) -> GateResult<GateEvaluation> {
        let mut evals = Vec::new();
        for condition in conditions {
            evals.push(self.evaluate(condition, context)?);
        }
        let all_passed = evals.iter().all(|e| e.passed);
        Ok(GateEvaluation {
            phase_id: context.phase_id.clone(),
            conditions: evals,
            all_passed,
        })
    }
}

/// Gate evaluator that passes every condition unconditionally.
///
/// Used during reconciliation where a completed Canopy task implies the
/// assigned agent already satisfied all gate conditions as part of its work.
/// Hymenium trusts the Canopy completion signal as the gate outcome.
#[derive(Debug, Clone, Default)]
pub struct PermissiveGateEvaluator;

impl GateEvaluator for PermissiveGateEvaluator {
    fn evaluate(
        &self,
        condition: &GateCondition,
        _context: &GateContext,
    ) -> GateResult<ConditionEvaluation> {
        Ok(ConditionEvaluation {
            condition: condition.clone(),
            passed: true,
            reason: "condition satisfied (permissive reconciliation)".to_string(),
        })
    }
}

/// Mock gate evaluator for testing.
/// Allows setting which conditions pass or fail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockGateEvaluator {
    passing: HashMap<String, bool>,
}

impl MockGateEvaluator {
    /// Create a new mock evaluator with all conditions failing by default.
    pub fn new() -> Self {
        Self {
            passing: HashMap::new(),
        }
    }

    /// Set whether a condition should pass or fail.
    pub fn set_condition(mut self, condition: impl Into<String>, passes: bool) -> Self {
        self.passing.insert(condition.into(), passes);
        self
    }

    /// Set multiple conditions at once.
    pub fn with_conditions(mut self, conditions: impl IntoIterator<Item = (String, bool)>) -> Self {
        self.passing.extend(conditions);
        self
    }
}

impl Default for MockGateEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl GateEvaluator for MockGateEvaluator {
    fn evaluate(
        &self,
        condition: &GateCondition,
        _context: &GateContext,
    ) -> GateResult<ConditionEvaluation> {
        let condition_str = condition.to_string();
        let passed = self.passing.get(&condition_str).copied().unwrap_or(false);

        Ok(ConditionEvaluation {
            condition: condition.clone(),
            passed,
            reason: if passed {
                format!("{} satisfied", condition_str)
            } else {
                format!("{} not satisfied", condition_str)
            },
        })
    }
}

/// A gate evaluator backed by real `TaskDetail` evidence fields.
///
/// Evaluates `CodeDiffExists` and `VerificationPassed` by inspecting the
/// provided task detail, rather than returning hardcoded values. Useful for
/// integration tests that want to prove the gate actually checks evidence state.
#[derive(Debug, Clone)]
pub struct EvidenceGateEvaluator {
    task: crate::dispatch::TaskDetail,
}

impl EvidenceGateEvaluator {
    pub fn new(task: crate::dispatch::TaskDetail) -> Self {
        Self { task }
    }
}

impl GateEvaluator for EvidenceGateEvaluator {
    fn evaluate(
        &self,
        condition: &GateCondition,
        _context: &GateContext,
    ) -> GateResult<ConditionEvaluation> {
        let (passed, reason) = match condition {
            GateCondition::CodeDiffExists => (
                self.task.has_code_diff,
                if self.task.has_code_diff {
                    "code diff is present".to_string()
                } else {
                    "no code diff recorded for this task".to_string()
                },
            ),
            GateCondition::VerificationPassed => (
                self.task.has_verification_passed,
                if self.task.has_verification_passed {
                    "verification evidence is present and passed".to_string()
                } else {
                    "no passing verification evidence for this task".to_string()
                },
            ),
            // For conditions not yet backed by evidence fields, default to
            // conservative pass to avoid blocking non-evidence gates.
            GateCondition::AuditClean | GateCondition::FindingsResolved => (
                true,
                "structural gate — not evidence-backed".to_string(),
            ),
            GateCondition::Custom(name) => (
                false,
                format!("unknown custom condition '{name}' — defaulting to blocked"),
            ),
        };
        Ok(ConditionEvaluation {
            condition: condition.clone(),
            passed,
            reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_known_conditions() {
        assert_eq!(
            parse_gate_condition("code_diff_exists"),
            GateCondition::CodeDiffExists
        );
        assert_eq!(
            parse_gate_condition("verification_passed"),
            GateCondition::VerificationPassed
        );
        assert_eq!(
            parse_gate_condition("audit_clean"),
            GateCondition::AuditClean
        );
        assert_eq!(
            parse_gate_condition("findings_resolved"),
            GateCondition::FindingsResolved
        );
    }

    #[test]
    fn test_parse_unknown_condition_becomes_custom() {
        let result = parse_gate_condition("custom_condition");
        match result {
            GateCondition::Custom(s) => assert_eq!(s, "custom_condition"),
            _ => panic!("expected Custom variant"),
        }
    }

    #[test]
    fn test_gate_condition_display() {
        assert_eq!(
            format!("{}", GateCondition::CodeDiffExists),
            "code_diff_exists"
        );
        assert_eq!(
            format!("{}", GateCondition::VerificationPassed),
            "verification_passed"
        );
        assert_eq!(
            format!("{}", GateCondition::Custom("foo".to_string())),
            "foo"
        );
    }

    #[test]
    fn test_gate_context_creation() {
        let wf_id = WorkflowId("test-123".to_string());
        let context = GateContext::new(wf_id.clone(), "phase1");
        assert_eq!(context.workflow_id, wf_id);
        assert_eq!(context.phase_id, "phase1");
        assert!(context.metadata.is_empty());
    }

    #[test]
    fn test_gate_context_with_metadata() {
        let wf_id = WorkflowId("test-123".to_string());
        let context = GateContext::new(wf_id, "phase1")
            .with_metadata("key1", "value1")
            .with_metadata("key2", "value2");
        assert_eq!(
            context.metadata.get("key1").map(String::as_str),
            Some("value1")
        );
        assert_eq!(
            context.metadata.get("key2").map(String::as_str),
            Some("value2")
        );
    }

    #[test]
    fn test_mock_evaluator_all_conditions_pass() {
        let evaluator = MockGateEvaluator::new()
            .set_condition("code_diff_exists", true)
            .set_condition("verification_passed", true);

        let conditions = vec![
            GateCondition::CodeDiffExists,
            GateCondition::VerificationPassed,
        ];
        let context = GateContext::new(WorkflowId("test".to_string()), "phase1");

        let result = evaluator
            .evaluate_all(&conditions, &context)
            .expect("should evaluate");
        assert!(result.passed());
        assert_eq!(result.conditions.len(), 2);
    }

    #[test]
    fn test_mock_evaluator_one_condition_fails() {
        let evaluator = MockGateEvaluator::new()
            .set_condition("code_diff_exists", true)
            .set_condition("verification_passed", false);

        let conditions = vec![
            GateCondition::CodeDiffExists,
            GateCondition::VerificationPassed,
        ];
        let context = GateContext::new(WorkflowId("test".to_string()), "phase1");

        let result = evaluator
            .evaluate_all(&conditions, &context)
            .expect("should evaluate");
        assert!(!result.passed());
        let failures = result.failures();
        assert!(failures.iter().any(|f| f.contains("verification_passed")));
    }

    #[test]
    fn test_mock_evaluator_default_all_fail() {
        let evaluator = MockGateEvaluator::new();

        let conditions = vec![GateCondition::CodeDiffExists, GateCondition::AuditClean];
        let context = GateContext::new(WorkflowId("test".to_string()), "phase1");

        let result = evaluator
            .evaluate_all(&conditions, &context)
            .expect("should evaluate");
        assert!(!result.passed());
    }

    #[test]
    fn test_condition_evaluation_reasons() {
        let evaluator = MockGateEvaluator::new().set_condition("code_diff_exists", true);
        let context = GateContext::new(WorkflowId("test".to_string()), "phase1");

        let eval = evaluator
            .evaluate(&GateCondition::CodeDiffExists, &context)
            .expect("should evaluate");
        assert!(eval.passed);
        assert_eq!(eval.reason, "code_diff_exists satisfied");
    }
}
