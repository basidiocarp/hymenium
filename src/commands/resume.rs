//! Resume a workflow paused at a HandoffToUser checkpoint.

use anyhow::{Context, Result};
use crate::store::WorkflowStore;
use crate::workflow::WorkflowId;

/// Resume the workflow with `id` from a HandoffToUser pause.
pub fn run(workflow_id: &str, store: &WorkflowStore) -> Result<()> {
    let id = WorkflowId(workflow_id.to_string());
    let mut instance = store
        .get_workflow(&id)
        .with_context(|| format!("could not load workflow {workflow_id}"))?
        .ok_or_else(|| anyhow::anyhow!("workflow {workflow_id} not found"))?;

    instance
        .resume_from_user_input()
        .with_context(|| format!("could not resume workflow {workflow_id}"))?;

    let phase_order = instance.current_phase_idx;
    store
        .with_transaction::<_, _, crate::store::StoreError>(|s| {
            s.update_workflow_status(&id, &instance.status, None)?;
            if let Some(state) = instance.current_phase() {
                s.upsert_phase_state(&id, state, phase_order)?;
            }
            Ok(())
        })
        .with_context(|| format!("could not persist resume for {workflow_id}"))?;

    println!("Workflow {workflow_id} resumed.");
    Ok(())
}
