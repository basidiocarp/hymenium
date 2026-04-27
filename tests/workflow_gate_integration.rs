//! Integration tests proving the auditor phase gate evaluates real evidence state.
//!
//! These tests use `EvidenceGateEvaluator` backed by `TaskDetail` records to
//! verify that:
//! - the gate blocks when evidence fields are absent
//! - the gate passes when both evidence fields are present
//! - partial evidence (diff only, no verification) is correctly detected as blocked

use hymenium::dispatch::TaskDetail;
use hymenium::workflow::gate::{EvidenceGateEvaluator, GateCondition, GateEvaluator};
use hymenium::workflow::{GateContext, WorkflowId};

fn make_task(has_code_diff: bool, has_verification_passed: bool) -> TaskDetail {
    TaskDetail {
        task_id: "test-task".to_string(),
        title: "Test implementer task".to_string(),
        status: "completed".to_string(),
        agent_id: None,
        parent_id: None,
        required_capabilities: Vec::new(),
        has_code_diff,
        has_verification_passed,
    }
}

fn make_context() -> GateContext {
    GateContext::new(WorkflowId("wf-test".to_string()), "audit-phase")
}

#[test]
fn workflow_gate_blocks_audit_without_code_diff() {
    let task = make_task(false, false);
    let evaluator = EvidenceGateEvaluator::new(task);
    let ctx = make_context();
    let result = evaluator
        .evaluate(&GateCondition::CodeDiffExists, &ctx)
        .expect("evaluate");
    assert!(
        !result.passed,
        "gate should block when task has no code diff"
    );
    assert!(
        result.reason.contains("no code diff"),
        "reason should explain why blocked: got '{}'",
        result.reason
    );
}

#[test]
fn workflow_gate_blocks_audit_without_verification() {
    let task = make_task(true, false);
    let evaluator = EvidenceGateEvaluator::new(task);
    let ctx = make_context();
    let result = evaluator
        .evaluate(&GateCondition::VerificationPassed, &ctx)
        .expect("evaluate");
    assert!(
        !result.passed,
        "gate should block when task has no verification evidence"
    );
    assert!(
        result.reason.contains("no passing verification"),
        "reason should explain: got '{}'",
        result.reason
    );
}

#[test]
fn workflow_gate_blocks_audit_without_real_diff_and_verification() {
    // The main named test from the verification contract.
    // With both fields false, both conditions must fail.
    let task = make_task(false, false);
    let evaluator = EvidenceGateEvaluator::new(task);
    let ctx = make_context();

    let conditions = [GateCondition::CodeDiffExists, GateCondition::VerificationPassed];
    let eval = evaluator
        .evaluate_all(&conditions, &ctx)
        .expect("evaluate_all");
    assert!(
        !eval.passed(),
        "gate must block when task has neither diff nor verification"
    );
    assert_eq!(eval.failures().len(), 2, "both conditions should fail");
}

#[test]
fn workflow_gate_passes_audit_with_both_evidence_types() {
    let task = make_task(true, true);
    let evaluator = EvidenceGateEvaluator::new(task);
    let ctx = make_context();

    let conditions = [GateCondition::CodeDiffExists, GateCondition::VerificationPassed];
    let eval = evaluator
        .evaluate_all(&conditions, &ctx)
        .expect("evaluate_all");
    assert!(
        eval.passed(),
        "gate must pass when task has both diff and verification evidence"
    );
    assert_eq!(eval.failures().len(), 0, "no conditions should fail");
}

#[test]
fn workflow_gate_partial_evidence_diff_only_still_blocks() {
    // Chaos test: partial evidence (diff present, verification absent) must block.
    // Proves the gate is field-checking, not just returning true.
    let task = make_task(true, false);
    let evaluator = EvidenceGateEvaluator::new(task);
    let ctx = make_context();

    let conditions = [GateCondition::CodeDiffExists, GateCondition::VerificationPassed];
    let eval = evaluator
        .evaluate_all(&conditions, &ctx)
        .expect("evaluate_all");
    assert!(
        !eval.passed(),
        "gate must block when only diff is present (verification still missing)"
    );
    assert_eq!(
        eval.failures().len(),
        1,
        "only VerificationPassed should fail"
    );
    assert!(
        eval.failures()[0].contains("verification"),
        "the failing condition should be verification_passed, got '{}'",
        eval.failures()[0]
    );
}
