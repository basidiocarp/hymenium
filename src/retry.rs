//! Retry policy and recovery actions.
//!
//! Implements retry decisions for stalled or failed workflow phases. Given a
//! [`ProgressSignal`] from the monitor and the current retry count, this module
//! decides whether to retry (optionally with a narrower scope or escalated
//! agent tier), escalate to a human operator, or cancel.

use serde::{Deserialize, Serialize};

use crate::monitor::{ProgressSignal, StallReason};
use crate::workflow::template::AgentTier;

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// Configuration for retry behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of retries before escalating.
    pub max_retries: u32,

    /// Whether to narrow the task scope on retry (e.g. split into smaller work).
    pub narrow_scope_on_retry: bool,

    /// Whether to escalate the agent tier on retry (e.g. Sonnet -> Opus).
    pub escalate_tier_on_retry: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            narrow_scope_on_retry: true,
            escalate_tier_on_retry: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Recovery action
// ---------------------------------------------------------------------------

/// The action the orchestrator should take in response to a stall or failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RecoveryAction {
    /// Retry the phase, optionally with narrowed scope or escalated tier.
    Retry {
        narrowed_scope: Option<String>,
        new_tier: Option<AgentTier>,
    },

    /// Escalate to a human operator.
    Escalate { reason: String },

    /// No recovery needed; cancel the recovery workflow.
    Cancel { reason: String },
}

// ---------------------------------------------------------------------------
// Tier escalation
// ---------------------------------------------------------------------------

/// Return the next higher agent tier.
///
/// Haiku -> Sonnet -> Opus -> Opus (ceiling). Any -> Sonnet.
pub fn next_tier(current: &AgentTier) -> AgentTier {
    match current {
        AgentTier::Haiku | AgentTier::Any => AgentTier::Sonnet,
        AgentTier::Sonnet | AgentTier::Opus => AgentTier::Opus,
    }
}

// ---------------------------------------------------------------------------
// Recovery decision
// ---------------------------------------------------------------------------

/// Decide the recovery action for a given progress signal and retry state.
///
/// The caller is responsible for executing the returned action (e.g. closing
/// the stalled agent, re-dispatching with new parameters, or notifying the
/// operator).
pub fn decide_recovery(
    signal: &ProgressSignal,
    retry_count: u32,
    policy: &RetryPolicy,
) -> RecoveryAction {
    match signal {
        // Signals that should not trigger recovery at all.
        ProgressSignal::Healthy { .. }
        | ProgressSignal::PhaseComplete { .. }
        | ProgressSignal::GateSatisfied { .. } => RecoveryAction::Cancel {
            reason: "no recovery needed".to_string(),
        },

        // Heartbeat timeout: the agent likely never started. Always retry
        // immediately on the first attempt.
        ProgressSignal::Stalled {
            reason: StallReason::HeartbeatTimeout,
            ..
        } => {
            if retry_count >= policy.max_retries {
                return RecoveryAction::Escalate {
                    reason: format!(
                        "heartbeat timeout after {} retries — retry limit exceeded",
                        retry_count
                    ),
                };
            }
            RecoveryAction::Retry {
                narrowed_scope: None,
                new_tier: None,
            }
        }

        // Status chatter: agent is active but not producing real work.
        ProgressSignal::Stalled {
            reason: StallReason::StatusChatterOnly,
            ..
        } => {
            if retry_count >= policy.max_retries {
                return RecoveryAction::Escalate {
                    reason: format!(
                        "status chatter only after {} retries — retry limit exceeded",
                        retry_count
                    ),
                };
            }
            let narrowed = if policy.narrow_scope_on_retry {
                Some("narrow scope to reduce chatter".to_string())
            } else {
                None
            };
            RecoveryAction::Retry {
                narrowed_scope: narrowed,
                new_tier: None,
            }
        }

        // No code diff: agent started but produced nothing.
        ProgressSignal::Stalled {
            reason: StallReason::NoCodeDiff,
            ..
        } => decide_progressive_recovery(retry_count, policy, "no code diff"),

        // Partial progress stopped: some items done, rest stalled.
        ProgressSignal::Stalled {
            reason: StallReason::NoPasteMarkerProgress,
            ..
        } => decide_progressive_recovery(retry_count, policy, "partial progress stalled"),

        // Failed canopy task.
        ProgressSignal::Failed { .. } => {
            if retry_count >= policy.max_retries {
                RecoveryAction::Escalate {
                    reason: format!(
                        "phase failed after {} retries — retry limit exceeded",
                        retry_count
                    ),
                }
            } else {
                RecoveryAction::Retry {
                    narrowed_scope: None,
                    new_tier: None,
                }
            }
        }
    }
}

/// Progressive recovery: first retry is plain, second narrows scope and
/// optionally escalates tier, third and beyond escalates to operator.
fn decide_progressive_recovery(
    retry_count: u32,
    policy: &RetryPolicy,
    context: &str,
) -> RecoveryAction {
    if retry_count >= policy.max_retries {
        return RecoveryAction::Escalate {
            reason: format!(
                "{context} after {retry_count} retries — retry limit exceeded"
            ),
        };
    }

    if retry_count == 0 { RecoveryAction::Retry {
        narrowed_scope: None,
        new_tier: None,
    } } else {
        let narrowed = if policy.narrow_scope_on_retry {
            Some("narrow".to_string())
        } else {
            None
        };
        let tier = if policy.escalate_tier_on_retry {
            // Currently assumes Sonnet as the base tier (the default in
            // impl_audit_default). When decide_recovery gains a
            // current_tier parameter, this should use next_tier(current_tier).
            Some(next_tier(&AgentTier::Sonnet))
        } else {
            None
        };
        RecoveryAction::Retry {
            narrowed_scope: narrowed,
            new_tier: tier,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::StallReason;
    use chrono::Utc;

    fn stalled_signal(reason: StallReason) -> ProgressSignal {
        ProgressSignal::Stalled {
            phase_id: "implement".to_string(),
            since: Utc::now(),
            reason,
        }
    }

    // -- decide_recovery tests ------------------------------------------------

    #[test]
    fn first_stall_retries_plain() {
        let signal = stalled_signal(StallReason::NoCodeDiff);
        let policy = RetryPolicy::default();
        let action = decide_recovery(&signal, 0, &policy);

        match action {
            RecoveryAction::Retry {
                narrowed_scope,
                new_tier,
            } => {
                assert!(narrowed_scope.is_none());
                assert!(new_tier.is_none());
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn second_stall_narrows_scope() {
        let signal = stalled_signal(StallReason::NoCodeDiff);
        let policy = RetryPolicy::default(); // narrow_scope_on_retry = true
        let action = decide_recovery(&signal, 1, &policy);

        match action {
            RecoveryAction::Retry { narrowed_scope, .. } => {
                assert!(narrowed_scope.is_some(), "expected narrowed scope on second retry");
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn third_stall_escalates() {
        let signal = stalled_signal(StallReason::NoCodeDiff);
        let policy = RetryPolicy::default(); // max_retries = 2
        let action = decide_recovery(&signal, 2, &policy);

        assert!(matches!(action, RecoveryAction::Escalate { .. }));
    }

    #[test]
    fn heartbeat_timeout_retries_immediately() {
        let signal = stalled_signal(StallReason::HeartbeatTimeout);
        let policy = RetryPolicy::default();
        let action = decide_recovery(&signal, 0, &policy);

        assert!(matches!(action, RecoveryAction::Retry { .. }));
    }

    #[test]
    fn heartbeat_timeout_escalates_at_limit() {
        let signal = stalled_signal(StallReason::HeartbeatTimeout);
        let policy = RetryPolicy::default();
        let action = decide_recovery(&signal, 2, &policy);

        assert!(matches!(action, RecoveryAction::Escalate { .. }));
    }

    #[test]
    fn status_chatter_retries_with_narrowed_scope() {
        let signal = stalled_signal(StallReason::StatusChatterOnly);
        let policy = RetryPolicy::default();
        let action = decide_recovery(&signal, 0, &policy);

        match action {
            RecoveryAction::Retry { narrowed_scope, .. } => {
                assert!(narrowed_scope.is_some());
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn failed_signal_retries_under_limit() {
        let signal = ProgressSignal::Failed {
            phase_id: "implement".to_string(),
            error: "canopy task failed".to_string(),
        };
        let policy = RetryPolicy::default();
        let action = decide_recovery(&signal, 0, &policy);
        assert!(matches!(action, RecoveryAction::Retry { .. }));
    }

    #[test]
    fn failed_signal_escalates_at_limit() {
        let signal = ProgressSignal::Failed {
            phase_id: "implement".to_string(),
            error: "canopy task failed".to_string(),
        };
        let policy = RetryPolicy::default();
        let action = decide_recovery(&signal, 2, &policy);
        assert!(matches!(action, RecoveryAction::Escalate { .. }));
    }

    #[test]
    fn healthy_signal_cancels() {
        let signal = ProgressSignal::Healthy {
            phase_id: "implement".to_string(),
            last_activity: Utc::now(),
        };
        let policy = RetryPolicy::default();
        let action = decide_recovery(&signal, 0, &policy);
        assert!(matches!(action, RecoveryAction::Cancel { .. }));
    }

    #[test]
    fn phase_complete_cancels() {
        let signal = ProgressSignal::PhaseComplete {
            phase_id: "implement".to_string(),
        };
        let policy = RetryPolicy::default();
        let action = decide_recovery(&signal, 0, &policy);
        assert!(matches!(action, RecoveryAction::Cancel { .. }));
    }

    #[test]
    fn gate_satisfied_cancels() {
        let signal = ProgressSignal::GateSatisfied {
            gate: "code_diff_exists".to_string(),
        };
        let policy = RetryPolicy::default();
        let action = decide_recovery(&signal, 0, &policy);
        assert!(matches!(action, RecoveryAction::Cancel { .. }));
    }

    #[test]
    fn paste_marker_progress_follows_progressive_recovery() {
        let signal = stalled_signal(StallReason::NoPasteMarkerProgress);
        let policy = RetryPolicy::default();

        // First: plain retry
        let action = decide_recovery(&signal, 0, &policy);
        assert!(matches!(action, RecoveryAction::Retry { narrowed_scope: None, .. }));

        // Second: narrowed scope
        let action = decide_recovery(&signal, 1, &policy);
        match action {
            RecoveryAction::Retry { narrowed_scope, .. } => {
                assert!(narrowed_scope.is_some());
            }
            other => panic!("expected Retry, got {other:?}"),
        }

        // Third: escalate
        let action = decide_recovery(&signal, 2, &policy);
        assert!(matches!(action, RecoveryAction::Escalate { .. }));
    }

    // -- next_tier tests ------------------------------------------------------

    #[test]
    fn tier_escalation_chain() {
        assert_eq!(next_tier(&AgentTier::Haiku), AgentTier::Sonnet);
        assert_eq!(next_tier(&AgentTier::Sonnet), AgentTier::Opus);
        assert_eq!(next_tier(&AgentTier::Opus), AgentTier::Opus);
        assert_eq!(next_tier(&AgentTier::Any), AgentTier::Sonnet);
    }

    // -- RetryPolicy defaults -------------------------------------------------

    #[test]
    fn default_policy_values() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 2);
        assert!(policy.narrow_scope_on_retry);
        assert!(!policy.escalate_tier_on_retry);
    }

    // -- escalate_tier_on_retry -----------------------------------------------

    #[test]
    fn tier_escalation_on_retry_when_enabled() {
        let signal = stalled_signal(StallReason::NoCodeDiff);
        let policy = RetryPolicy {
            max_retries: 3,
            narrow_scope_on_retry: true,
            escalate_tier_on_retry: true,
        };
        let action = decide_recovery(&signal, 1, &policy);
        match action {
            RecoveryAction::Retry { new_tier, .. } => {
                assert_eq!(new_tier, Some(AgentTier::Opus));
            }
            other => panic!("expected Retry with tier escalation, got {other:?}"),
        }
    }

    // -- max_retries = 0 escalates immediately --------------------------------

    #[test]
    fn zero_max_retries_escalates_immediately() {
        let policy = RetryPolicy {
            max_retries: 0,
            ..RetryPolicy::default()
        };
        for reason in [
            StallReason::NoCodeDiff,
            StallReason::HeartbeatTimeout,
            StallReason::NoPasteMarkerProgress,
            StallReason::StatusChatterOnly,
        ] {
            let signal = stalled_signal(reason);
            let action = decide_recovery(&signal, 0, &policy);
            assert!(
                matches!(action, RecoveryAction::Escalate { .. }),
                "expected Escalate for {:?}",
                signal
            );
        }
    }

    // -- StatusChatterOnly via handle_signal ----------------------------------

    #[test]
    fn status_chatter_via_handle_signal_returns_retry() {
        use crate::workflow::engine::WorkflowInstance;
        use crate::workflow::template::impl_audit_default;
        use crate::workflow::WorkflowId;
        use crate::workflow::engine::PhaseStatus;

        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-chatter".to_string()),
            template,
            "/test/handoff.md",
        );
        wf.phase_states[0].status = PhaseStatus::Active;
        wf.phase_states[0].started_at = Some(Utc::now());

        let signal = ProgressSignal::Stalled {
            phase_id: "implement".to_string(),
            since: Utc::now(),
            reason: StallReason::StatusChatterOnly,
        };
        let policy = RetryPolicy::default();

        let action = crate::monitor::handle_signal(&signal, &mut wf, 0, &policy)
            .expect("should succeed");
        match action {
            RecoveryAction::Retry { narrowed_scope, .. } => {
                assert!(narrowed_scope.is_some(), "StatusChatterOnly should narrow scope");
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }
}
