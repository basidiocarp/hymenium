use chrono::{DateTime, Utc};

use crate::dispatch::CanopyClient;
use crate::workflow::engine::{PhaseStatus, WorkflowInstance};

use super::{MonitorConfig, MonitorError, ProgressSignal, StallReason};

/// Evaluate the health of the current active phase.
///
/// Accepts `now` as a parameter so callers control the clock, making this
/// function fully deterministic in tests.
pub fn check_progress(
    workflow: &WorkflowInstance,
    canopy: &dyn CanopyClient,
    config: &MonitorConfig,
    now: DateTime<Utc>,
) -> Result<ProgressSignal, MonitorError> {
    let phase = workflow
        .phase_states
        .get(workflow.current_phase_idx)
        .ok_or_else(|| {
            MonitorError::InvalidState(format!(
                "no phase at index {}",
                workflow.current_phase_idx
            ))
        })?;

    if phase.status != PhaseStatus::Active {
        return Err(MonitorError::PhaseNotActive(format!(
            "phase {} is {:?}, not Active",
            phase.phase_id, phase.status
        )));
    }

    let task_id = phase.canopy_task_id.as_deref().ok_or_else(|| {
        MonitorError::InvalidState(format!(
            "phase {} has no canopy_task_id",
            phase.phase_id
        ))
    })?;

    let task = canopy
        .get_task(task_id)
        .map_err(|e| MonitorError::CanopyError(e.to_string()))?;

    // Terminal states in canopy take priority.
    if task.status == "completed" {
        return Ok(ProgressSignal::PhaseComplete {
            phase_id: phase.phase_id.clone(),
        });
    }
    if task.status == "failed" {
        return Ok(ProgressSignal::Failed {
            phase_id: phase.phase_id.clone(),
            error: format!("canopy task {} failed", task_id),
        });
    }

    // Heartbeat check: if the task is still pending/assigned and the heartbeat
    // timeout has elapsed, the agent likely never started.
    // An Active phase with no started_at is an invalid state — surface it rather
    // than silently defaulting to `now` (which would suppress the timeout).
    let started = phase.started_at.ok_or_else(|| {
        MonitorError::InvalidState(format!(
            "phase {} is Active but has no started_at timestamp",
            phase.phase_id
        ))
    })?;
    let elapsed = now.signed_duration_since(started);
    let heartbeat_elapsed =
        elapsed > chrono::Duration::from_std(config.heartbeat_timeout).unwrap_or(chrono::Duration::MAX);

    if heartbeat_elapsed && (task.status == "pending" || task.status == "assigned") {
        return Ok(ProgressSignal::Stalled {
            phase_id: phase.phase_id.clone(),
            since: started,
            reason: StallReason::HeartbeatTimeout,
        });
    }

    // Completeness check.
    let completeness = canopy
        .check_completeness(&workflow.handoff_path)
        .map_err(|e| MonitorError::CanopyError(e.to_string()))?;

    if completeness.complete {
        return Ok(ProgressSignal::PhaseComplete {
            phase_id: phase.phase_id.clone(),
        });
    }

    let progress_elapsed =
        elapsed > chrono::Duration::from_std(config.progress_timeout).unwrap_or(chrono::Duration::MAX);

    if progress_elapsed && completeness.completed_items == 0 {
        return Ok(ProgressSignal::Stalled {
            phase_id: phase.phase_id.clone(),
            since: started,
            reason: StallReason::NoCodeDiff,
        });
    }

    if progress_elapsed && completeness.completed_items > 0 && !completeness.complete {
        return Ok(ProgressSignal::Stalled {
            phase_id: phase.phase_id.clone(),
            since: started,
            reason: StallReason::NoPasteMarkerProgress,
        });
    }

    Ok(ProgressSignal::Healthy {
        phase_id: phase.phase_id.clone(),
        last_activity: now,
    })
}

/// Returns `true` when the signal indicates a stalled phase.
pub fn is_stalled(signal: &ProgressSignal) -> bool {
    matches!(signal, ProgressSignal::Stalled { .. })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::engine::PhaseStatus;
    use crate::workflow::template::impl_audit_default;
    use crate::workflow::WorkflowId;
    use chrono::Utc;

    use super::super::test_helpers::{
        complete_report, incomplete_report, make_workflow, task_with_status, TestCanopyClient,
    };

    // -- check_progress tests -------------------------------------------------

    #[test]
    fn completed_canopy_task_yields_phase_complete() {
        let wf = make_workflow();
        let canopy = TestCanopyClient::new(task_with_status("completed"), incomplete_report(0, 3));
        let config = MonitorConfig::default();

        let signal = check_progress(&wf, &canopy, &config, Utc::now())
            .expect("should succeed");
        assert!(matches!(signal, ProgressSignal::PhaseComplete { .. }));
    }

    #[test]
    fn failed_canopy_task_yields_failed() {
        let wf = make_workflow();
        let canopy = TestCanopyClient::new(task_with_status("failed"), incomplete_report(0, 3));
        let config = MonitorConfig::default();

        let signal = check_progress(&wf, &canopy, &config, Utc::now())
            .expect("should succeed");
        assert!(matches!(signal, ProgressSignal::Failed { .. }));
    }

    #[test]
    fn stale_pending_task_yields_heartbeat_timeout() {
        let mut wf = make_workflow();
        // Set started_at far in the past.
        wf.phase_states[0].started_at = Some(Utc::now() - chrono::Duration::hours(1));

        let canopy = TestCanopyClient::new(task_with_status("pending"), incomplete_report(0, 3));
        let config = MonitorConfig::default();

        let signal = check_progress(&wf, &canopy, &config, Utc::now())
            .expect("should succeed");
        match signal {
            ProgressSignal::Stalled { reason, .. } => {
                assert!(matches!(reason, StallReason::HeartbeatTimeout));
            }
            other => panic!("expected Stalled(HeartbeatTimeout), got {other:?}"),
        }
    }

    #[test]
    fn zero_completeness_after_timeout_yields_no_code_diff() {
        let mut wf = make_workflow();
        wf.phase_states[0].started_at = Some(Utc::now() - chrono::Duration::hours(1));

        // Task is in_progress (not pending/assigned), so heartbeat check passes.
        let canopy = TestCanopyClient::new(task_with_status("in_progress"), incomplete_report(0, 3));
        let config = MonitorConfig::default();

        let signal = check_progress(&wf, &canopy, &config, Utc::now())
            .expect("should succeed");
        match signal {
            ProgressSignal::Stalled { reason, .. } => {
                assert!(matches!(reason, StallReason::NoCodeDiff));
            }
            other => panic!("expected Stalled(NoCodeDiff), got {other:?}"),
        }
    }

    #[test]
    fn partial_completeness_after_timeout_yields_no_paste_marker_progress() {
        let mut wf = make_workflow();
        wf.phase_states[0].started_at = Some(Utc::now() - chrono::Duration::hours(1));

        let canopy = TestCanopyClient::new(task_with_status("in_progress"), incomplete_report(1, 3));
        let config = MonitorConfig::default();

        let signal = check_progress(&wf, &canopy, &config, Utc::now())
            .expect("should succeed");
        match signal {
            ProgressSignal::Stalled { reason, .. } => {
                assert!(matches!(reason, StallReason::NoPasteMarkerProgress));
            }
            other => panic!("expected Stalled(NoPasteMarkerProgress), got {other:?}"),
        }
    }

    #[test]
    fn recent_in_progress_task_yields_healthy() {
        let wf = make_workflow();
        let canopy = TestCanopyClient::new(task_with_status("in_progress"), incomplete_report(1, 3));
        let config = MonitorConfig::default();

        let signal = check_progress(&wf, &canopy, &config, Utc::now())
            .expect("should succeed");
        assert!(matches!(signal, ProgressSignal::Healthy { .. }));
    }

    #[test]
    fn completeness_satisfied_yields_phase_complete() {
        let wf = make_workflow();
        let canopy = TestCanopyClient::new(task_with_status("in_progress"), complete_report());
        let config = MonitorConfig::default();

        let signal = check_progress(&wf, &canopy, &config, Utc::now())
            .expect("should succeed");
        assert!(matches!(signal, ProgressSignal::PhaseComplete { .. }));
    }

    #[test]
    fn no_canopy_task_id_errors() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-no-task".to_string()),
            template,
            "/test/handoff.md",
        );
        wf.phase_states[0].status = PhaseStatus::Active;
        // canopy_task_id left as None.

        let canopy = TestCanopyClient::new(task_with_status("pending"), incomplete_report(0, 3));
        let config = MonitorConfig::default();

        let result = check_progress(&wf, &canopy, &config, Utc::now());
        assert!(result.is_err());
        match result {
            Err(MonitorError::InvalidState(msg)) => {
                assert!(msg.contains("canopy_task_id"), "error was: {msg}");
            }
            other => panic!("expected InvalidState, got {other:?}"),
        }
    }

    #[test]
    fn pending_phase_errors() {
        let template = impl_audit_default();
        let wf = WorkflowInstance::new(
            WorkflowId("test-pending".to_string()),
            template,
            "/test/handoff.md",
        );
        // Phase 0 is Pending by default.

        let canopy = TestCanopyClient::new(task_with_status("pending"), incomplete_report(0, 3));
        let config = MonitorConfig::default();

        let result = check_progress(&wf, &canopy, &config, Utc::now());
        assert!(result.is_err());
        assert!(matches!(result, Err(MonitorError::PhaseNotActive(_))));
    }

    // -- Edge case: Active phase with no started_at -------------------------

    #[test]
    fn active_phase_without_started_at_errors() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-no-start".to_string()),
            template,
            "/test/handoff.md",
        );
        wf.phase_states[0].status = PhaseStatus::Active;
        wf.phase_states[0].canopy_task_id = Some("task-1".to_string());
        // started_at intentionally left as None

        let canopy = TestCanopyClient::new(task_with_status("in_progress"), incomplete_report(0, 3));
        let config = MonitorConfig::default();

        let result = check_progress(&wf, &canopy, &config, Utc::now());
        assert!(result.is_err());
        match result {
            Err(MonitorError::InvalidState(msg)) => {
                assert!(msg.contains("started_at"), "error was: {msg}");
            }
            other => panic!("expected InvalidState about started_at, got {other:?}"),
        }
    }

    // -- is_stalled tests -----------------------------------------------------

    #[test]
    fn is_stalled_identifies_stall_signals() {
        let stalled = ProgressSignal::Stalled {
            phase_id: "implement".to_string(),
            since: Utc::now(),
            reason: StallReason::HeartbeatTimeout,
        };
        assert!(is_stalled(&stalled));

        let healthy = ProgressSignal::Healthy {
            phase_id: "implement".to_string(),
            last_activity: Utc::now(),
        };
        assert!(!is_stalled(&healthy));

        let complete = ProgressSignal::PhaseComplete {
            phase_id: "implement".to_string(),
        };
        assert!(!is_stalled(&complete));

        let failed = ProgressSignal::Failed {
            phase_id: "implement".to_string(),
            error: "boom".to_string(),
        };
        assert!(!is_stalled(&failed));
    }
}
