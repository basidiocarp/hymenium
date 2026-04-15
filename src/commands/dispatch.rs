//! `hymenium dispatch <path>` command handler.

use crate::dispatch::{dispatch_workflow, CliCanopyClient};
use crate::parser::markdown::parse_handoff;
use crate::store::{StoreError, WorkflowStore};
use crate::workflow::engine::WorkflowInstance;
use crate::workflow::template::{impl_audit_default, TemplateRegistry};
use crate::workflow::WorkflowId;
use std::path::Path;
use thiserror::Error;
use ulid::Ulid;

/// Errors that can occur during the dispatch command.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DispatchCommandError {
    #[error("could not read handoff file '{path}': {source}")]
    ReadFile {
        path: String,
        source: std::io::Error,
    },

    #[error("could not parse handoff: {0}")]
    Parse(#[from] crate::parser::ParseError),

    #[error("dispatch failed: {0}")]
    Dispatch(#[from] crate::dispatch::DispatchError),

    #[error("store error: {0}")]
    Store(#[from] StoreError),
}

/// Run the `dispatch` command: parse the handoff, create a workflow instance,
/// dispatch it via Canopy, persist it, and print the workflow ID.
///
/// # Panics
///
/// Panics if the built-in `impl-audit` template fails validation, which should
/// never happen outside of a programming error in the template definition.
pub fn run(path: &Path, store: &WorkflowStore) -> Result<WorkflowInstance, DispatchCommandError> {
    let source = std::fs::read_to_string(path).map_err(|e| DispatchCommandError::ReadFile {
        path: path.display().to_string(),
        source: e,
    })?;

    let handoff = parse_handoff(&source)?;

    // Load the impl-audit template from the registry.
    let mut registry = TemplateRegistry::new();
    registry
        .register(impl_audit_default())
        .expect("built-in template must be valid");
    let template = registry
        .get("impl-audit")
        .expect("impl-audit template must exist in registry")
        .clone();

    let workflow_id = WorkflowId(Ulid::new().to_string());

    let canopy = CliCanopyClient::new("canopy");
    let instance = dispatch_workflow(&handoff, &template, &workflow_id, &canopy)?;

    // Insert the workflow row first so the FK on workflow_transitions is satisfied.
    store.insert_workflow(&instance)?;

    // Record the initial transition after the parent row exists.
    store.record_transition(
        &instance.workflow_id,
        None,
        instance.phase_states.first().map(|p| p.phase_id.as_str()),
        Some("initial dispatch"),
    )?;

    Ok(instance)
}

#[cfg(test)]
mod tests {
    use crate::dispatch::{dispatch_workflow, MockCanopyClient};
    use crate::parser::{ParsedHandoff, ParsedStep};
    use crate::store::WorkflowStore;
    use crate::workflow::template::impl_audit_default;
    use crate::workflow::WorkflowId;

    fn temp_store() -> WorkflowStore {
        // Use a unique file path per test invocation so tests don't collide.
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let db_path = std::env::temp_dir().join(format!("hymenium_dispatch_test_{}.db", nanos));
        WorkflowStore::open(&db_path).expect("open store")
    }

    fn minimal_handoff() -> ParsedHandoff {
        ParsedHandoff {
            title: "Regression Test Handoff".to_string(),
            metadata: None,
            problem: "Test the dispatch store ordering".to_string(),
            state: vec![],
            intent: "Prove insert_workflow precedes record_transition".to_string(),
            steps: vec![ParsedStep {
                number: 1,
                title: "Do the thing".to_string(),
                project: None,
                effort: None,
                depends_on: Vec::new(),
                description: "Step description".to_string(),
                files_to_modify: Vec::new(),
                verification: None,
                checklist: Vec::new(),
            }],
            completion_protocol: None,
            context: None,
        }
    }

    /// Regression test: `insert_workflow` must precede `record_transition`.
    ///
    /// With `PRAGMA foreign_keys = ON`, inserting into `workflow_transitions`
    /// before the parent row exists in `workflows` produces a FK violation.
    /// This test proves the ordering is correct by running both writes through
    /// a store with FK enforcement active.
    #[test]
    fn dispatch_store_writes_satisfy_fk_constraint() {
        let store = temp_store();
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let handoff = minimal_handoff();
        let workflow_id = WorkflowId("01TEST000000000000000FK001".to_string());

        let instance = dispatch_workflow(&handoff, &template, &workflow_id, &mock)
            .expect("dispatch_workflow should succeed");

        // This is the exact write sequence from commands/dispatch.rs `run()`.
        // A reversed order (record_transition first) would fail with a FK error.
        store
            .insert_workflow(&instance)
            .expect("insert_workflow must succeed before record_transition");
        store
            .record_transition(
                &instance.workflow_id,
                None,
                instance.phase_states.first().map(|p| p.phase_id.as_str()),
                Some("initial dispatch"),
            )
            .expect("record_transition must succeed after insert_workflow");

        // Verify both rows were persisted.
        let loaded = store
            .get_workflow(&instance.workflow_id)
            .expect("get should not error")
            .expect("workflow should exist");
        assert_eq!(loaded.workflow_id, instance.workflow_id);
        assert_eq!(loaded.phase_states.len(), 2);
    }
}
