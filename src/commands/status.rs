//! `hymenium status [<workflow_id>]` command handler.

use crate::store::{StoreError, WorkflowStore};
use crate::workflow::engine::WorkflowInstance;
use crate::workflow::WorkflowId;
use thiserror::Error;

/// Errors that can occur during the status command.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StatusCommandError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("workflow not found: {0}")]
    NotFound(String),
}

/// Run the `status` command for a specific workflow ID.
pub fn run_single(
    workflow_id: &str,
    store: &WorkflowStore,
    json: bool,
) -> Result<(), StatusCommandError> {
    let id = WorkflowId(workflow_id.to_string());
    let inst = store
        .get_workflow(&id)?
        .ok_or_else(|| StatusCommandError::NotFound(workflow_id.to_string()))?;

    if json {
        print_json(&inst);
    } else {
        print_human(&inst);
    }

    Ok(())
}

/// Run the `status` command with no workflow ID — lists all active workflows.
///
/// # Panics
///
/// Panics if `serde_json` fails to serialize a `serde_json::Value` — this is
/// considered a programming error because `Value` serialization is infallible
/// for well-formed values constructed from primitive JSON types.
pub fn run_list(store: &WorkflowStore, json: bool) -> Result<(), StatusCommandError> {
    let instances = store.list_active_workflows()?;

    if instances.is_empty() {
        println!("No active workflows.");
        return Ok(());
    }

    if json {
        let entries: Vec<_> = instances.iter().map(workflow_summary_json).collect();
        let out = serde_json::to_string_pretty(&entries)
            .expect("serde_json::Value serialization is infallible");
        println!("{}", out);
    } else {
        for inst in &instances {
            let phase_name = inst.current_phase().map_or("-", |p| p.phase_id.as_str());
            let agent = inst
                .current_phase()
                .and_then(|p| p.agent_id.as_deref())
                .unwrap_or("-");
            println!(
                "{} | {} | {} | phase: {} | agent: {}",
                inst.workflow_id, inst.template.template_id, inst.status, phase_name, agent,
            );
        }
    }

    Ok(())
}

fn print_human(inst: &WorkflowInstance) {
    println!("Workflow: {}", inst.workflow_id);
    println!("  Template:   {}", inst.template.template_id);
    println!("  Handoff:    {}", inst.handoff_path);
    println!("  Status:     {}", inst.status);
    if let Some(blocked) = &inst.blocked_on {
        println!("  Blocked on: {}", blocked);
    }
    println!("  Created:    {}", inst.created_at.to_rfc3339());
    println!("  Updated:    {}", inst.updated_at.to_rfc3339());
    println!("  Phases:");
    for state in &inst.phase_states {
        let agent = state.agent_id.as_deref().unwrap_or("-");
        println!(
            "    {} ({}) — {} | agent: {}",
            state.phase_id, state.role, state.status, agent
        );
    }
}

fn print_json(inst: &WorkflowInstance) {
    let val = workflow_summary_json(inst);
    let out =
        serde_json::to_string_pretty(&val).expect("serde_json::Value serialization is infallible");
    println!("{}", out);
}

fn workflow_summary_json(inst: &WorkflowInstance) -> serde_json::Value {
    let phases: Vec<_> = inst
        .phase_states
        .iter()
        .map(|p| {
            serde_json::json!({
                "phase_id": p.phase_id,
                "role": p.role.to_string(),
                "status": p.status.to_string(),
                "agent_id": p.agent_id,
                "started_at": p.started_at.map(|t| t.to_rfc3339()),
                "completed_at": p.completed_at.map(|t| t.to_rfc3339()),
                "canopy_task_id": p.canopy_task_id,
            })
        })
        .collect();

    serde_json::json!({
        "schema_version": "1.0",
        "workflow_id": inst.workflow_id.0,
        "handoff_path": inst.handoff_path,
        "template_id": inst.template.template_id,
        "status": inst.status.to_string(),
        "blocked_on": inst.blocked_on,
        "current_phase": inst.current_phase().map(|p| p.phase_id.as_str()),
        "phases": phases,
        "created_at": inst.created_at.to_rfc3339(),
        "updated_at": inst.updated_at.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::engine::WorkflowInstance;
    use crate::workflow::template::impl_audit_default;
    use crate::workflow::WorkflowId;

    /// Regression test: status JSON must emit septa workflow-status-v1 role names.
    ///
    /// The `workflow-status-v1` schema restricts `phases[].role` to the 9 reset
    /// role names. This test proves that `impl_audit_default()` phases serialize
    /// to allowed role strings and never to legacy names like `"implementer"` or
    /// `"auditor"`.
    #[test]
    fn status_json_emits_reset_role_names() {
        let allowed_roles = [
            "Spec Author",
            "Workflow Planner",
            "Packet Compiler",
            "Decomposition Checker",
            "Workflow Coordinator",
            "Worker",
            "Output Verifier",
            "Repair Worker",
            "Final Verifier",
        ];

        let template = impl_audit_default();
        let instance = WorkflowInstance::new(
            WorkflowId("test-status-roles".to_string()),
            template,
            "/handoffs/test.md",
        );

        let json_val = workflow_summary_json(&instance);
        let phases = json_val["phases"]
            .as_array()
            .expect("phases must be an array");

        assert_eq!(phases.len(), 2, "impl-audit must have 2 phases");

        for phase in phases {
            let role = phase["role"].as_str().expect("role must be a string");
            assert!(
                allowed_roles.contains(&role),
                "role '{}' is not in the septa workflow-status-v1 allowed set; \
                 legacy names like 'implementer' and 'auditor' are forbidden",
                role
            );
        }

        // Assert specific phase-to-role mapping.
        assert_eq!(phases[0]["phase_id"], "implement");
        assert_eq!(phases[0]["role"], "Worker");
        assert_eq!(phases[1]["phase_id"], "audit");
        assert_eq!(phases[1]["role"], "Output Verifier");
    }
}
