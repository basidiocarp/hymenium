use super::{
    CanopyClient, CompletenessReport, DispatchError, ImportResult, TaskDetail, TaskOptions,
};
use crate::workflow::template::AgentRole;

// ---------------------------------------------------------------------------
// CliCanopyClient
// ---------------------------------------------------------------------------

/// Canopy client that shells out to the `canopy` CLI binary.
#[derive(Debug, Clone)]
pub struct CliCanopyClient {
    pub(super) canopy_bin: String,
}

impl CliCanopyClient {
    /// Build a new client pointing at the given canopy binary path.
    pub fn new(canopy_bin: impl Into<String>) -> Self {
        Self {
            canopy_bin: canopy_bin.into(),
        }
    }

    /// Run a canopy subcommand and return trimmed stdout on success.
    fn run(&self, args: &[&str]) -> Result<String, DispatchError> {
        let output = std::process::Command::new(&self.canopy_bin)
            .args(args)
            .output()
            .map_err(|e| DispatchError::CanopyError(format!("failed to run canopy: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DispatchError::CanopyError(stderr.trim().to_string()));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

impl CliCanopyClient {
    fn canopy_required_role(role: &AgentRole) -> &'static str {
        match role {
            AgentRole::SpecAuthor
            | AgentRole::WorkflowPlanner
            | AgentRole::PacketCompiler
            | AgentRole::DecompositionChecker
            | AgentRole::WorkflowCoordinator => "orchestrator",
            AgentRole::Worker | AgentRole::RepairWorker => "implementer",
            AgentRole::OutputVerifier | AgentRole::FinalVerifier => "validator",
        }
    }

    fn parse_created_task_id(output: &str) -> String {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(output) {
            if let Some(task_id) = value.get("task_id").and_then(serde_json::Value::as_str) {
                return task_id.to_string();
            }
        }

        output.trim().to_string()
    }

    /// Build the CLI args for `task create` (top-level task).
    ///
    /// Returns owned `String`s so the caller controls lifetimes.
    pub(crate) fn build_create_task_args(
        title: &str,
        description: &str,
        project_root: &str,
        options: &TaskOptions,
    ) -> Vec<String> {
        let mut args = vec![
            "task".to_string(),
            "create".to_string(),
            "--title".to_string(),
            title.to_string(),
            "--description".to_string(),
            description.to_string(),
            "--project-root".to_string(),
            project_root.to_string(),
        ];
        if let Some(ref role) = options.required_role {
            args.push("--required-role".to_string());
            args.push(Self::canopy_required_role(role).to_string());
        }
        if options.verification_required {
            args.push("--verification-required".to_string());
        }
        if let Some(requested_by) = &options.requested_by {
            args.push("--requested-by".to_string());
            args.push(requested_by.clone());
        }
        // Pass capability requirements as a comma-separated list matching canopy's
        // --required-capabilities flag (value_delimiter = ',').
        if !options.required_capabilities.is_empty() {
            args.push("--required-capabilities".to_string());
            args.push(options.required_capabilities.join(","));
        }
        if let Some(ref wid) = options.workflow_id {
            args.push("--workflow-id".to_string());
            args.push(wid.clone());
        }
        if let Some(ref pid) = options.phase_id {
            args.push("--phase-id".to_string());
            args.push(pid.clone());
        }
        args
    }

    /// Build the CLI args for `task create --parent` (subtask).
    ///
    /// Returns owned `String`s so the caller controls lifetimes.
    pub(crate) fn build_create_subtask_args(
        parent_id: &str,
        title: &str,
        description: &str,
        options: &TaskOptions,
    ) -> Vec<String> {
        let mut args = vec![
            "task".to_string(),
            "create".to_string(),
            "--parent".to_string(),
            parent_id.to_string(),
            "--title".to_string(),
            title.to_string(),
            "--description".to_string(),
            description.to_string(),
        ];
        if let Some(ref role) = options.required_role {
            args.push("--required-role".to_string());
            args.push(Self::canopy_required_role(role).to_string());
        }
        if options.verification_required {
            args.push("--verification-required".to_string());
        }
        if let Some(requested_by) = &options.requested_by {
            args.push("--requested-by".to_string());
            args.push(requested_by.clone());
        }
        // Pass capability requirements as a comma-separated list matching canopy's
        // --required-capabilities flag (value_delimiter = ',').
        if !options.required_capabilities.is_empty() {
            args.push("--required-capabilities".to_string());
            args.push(options.required_capabilities.join(","));
        }
        if let Some(ref wid) = options.workflow_id {
            args.push("--workflow-id".to_string());
            args.push(wid.clone());
        }
        if let Some(ref pid) = options.phase_id {
            args.push("--phase-id".to_string());
            args.push(pid.clone());
        }
        args
    }

    /// Build the CLI args for `task assign`.
    ///
    /// Canopy requires: `--task-id <id>  --assigned-to <agent>  --assigned-by <user>`
    pub(crate) fn build_assign_task_args(
        task_id: &str,
        assigned_to: &str,
        assigned_by: &str,
    ) -> Vec<String> {
        vec![
            "task".to_string(),
            "assign".to_string(),
            "--task-id".to_string(),
            task_id.to_string(),
            "--assigned-to".to_string(),
            assigned_to.to_string(),
            "--assigned-by".to_string(),
            assigned_by.to_string(),
        ]
    }
}

impl CanopyClient for CliCanopyClient {
    fn create_task(
        &self,
        title: &str,
        description: &str,
        project_root: &str,
        options: &TaskOptions,
    ) -> Result<String, DispatchError> {
        let owned = Self::build_create_task_args(title, description, project_root, options);
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        let output = self.run(&args)?;
        Ok(Self::parse_created_task_id(&output))
    }

    fn create_subtask(
        &self,
        parent_id: &str,
        title: &str,
        description: &str,
        options: &TaskOptions,
    ) -> Result<String, DispatchError> {
        let owned = Self::build_create_subtask_args(parent_id, title, description, options);
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        let output = self.run(&args)?;
        Ok(Self::parse_created_task_id(&output))
    }

    fn assign_task(
        &self,
        task_id: &str,
        agent_id: &str,
        assigned_by: &str,
    ) -> Result<(), DispatchError> {
        let owned = Self::build_assign_task_args(task_id, agent_id, assigned_by);
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        self.run(&args)?;
        Ok(())
    }

    fn get_task(&self, task_id: &str) -> Result<TaskDetail, DispatchError> {
        let json = self.run(&["task", "get", task_id, "--json"])?;
        serde_json::from_str(&json)
            .map_err(|e| DispatchError::CanopyError(format!("failed to parse task detail: {e}")))
    }

    fn check_completeness(&self, handoff_path: &str) -> Result<CompletenessReport, DispatchError> {
        let json = self.run(&["completeness", "check", handoff_path, "--json"])?;
        serde_json::from_str(&json).map_err(|e| {
            DispatchError::CanopyError(format!("failed to parse completeness report: {e}"))
        })
    }

    fn import_handoff(
        &self,
        path: &str,
        assign_to: Option<&str>,
    ) -> Result<ImportResult, DispatchError> {
        let mut args = vec!["handoff", "import", path, "--json"];
        if let Some(agent) = assign_to {
            args.push("--assign");
            args.push(agent);
        }
        let json = self.run(&args)?;
        serde_json::from_str(&json)
            .map_err(|e| DispatchError::CanopyError(format!("failed to parse import result: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_client_builds() {
        let client = CliCanopyClient::new("canopy");
        assert_eq!(client.canopy_bin, "canopy");
    }

    #[test]
    fn parse_created_task_id_extracts_json_task_id() {
        let output = r#"{"task_id":"01TASK","title":"debug"}"#;
        assert_eq!(CliCanopyClient::parse_created_task_id(output), "01TASK");
    }

    #[test]
    fn parse_created_task_id_preserves_raw_id_fallback() {
        assert_eq!(CliCanopyClient::parse_created_task_id("01RAW\n"), "01RAW");
    }

    #[test]
    fn canopy_required_role_maps_hymenium_roles_to_canopy_roles() {
        // implementer variants
        assert_eq!(
            CliCanopyClient::canopy_required_role(&AgentRole::Worker),
            "implementer"
        );
        assert_eq!(
            CliCanopyClient::canopy_required_role(&AgentRole::RepairWorker),
            "implementer"
        );
        // validator variants
        assert_eq!(
            CliCanopyClient::canopy_required_role(&AgentRole::OutputVerifier),
            "validator"
        );
        assert_eq!(
            CliCanopyClient::canopy_required_role(&AgentRole::FinalVerifier),
            "validator"
        );
        // orchestrator variants
        assert_eq!(
            CliCanopyClient::canopy_required_role(&AgentRole::WorkflowCoordinator),
            "orchestrator"
        );
        assert_eq!(
            CliCanopyClient::canopy_required_role(&AgentRole::SpecAuthor),
            "orchestrator"
        );
        assert_eq!(
            CliCanopyClient::canopy_required_role(&AgentRole::WorkflowPlanner),
            "orchestrator"
        );
        assert_eq!(
            CliCanopyClient::canopy_required_role(&AgentRole::PacketCompiler),
            "orchestrator"
        );
        assert_eq!(
            CliCanopyClient::canopy_required_role(&AgentRole::DecompositionChecker),
            "orchestrator"
        );
    }

    fn caps_options(caps: Vec<String>) -> TaskOptions {
        TaskOptions {
            required_capabilities: caps,
            ..Default::default()
        }
    }

    // -- create_task arg-builder tests ------------------------------------------

    #[test]
    fn build_create_task_args_includes_capabilities_flag_when_set() {
        let options = caps_options(vec!["rust".to_string(), "shell".to_string()]);
        let args = CliCanopyClient::build_create_task_args("t", "d", ".", &options);

        let pos = args
            .iter()
            .position(|a| a == "--required-capabilities")
            .expect("--required-capabilities should be present");
        assert_eq!(
            args.get(pos + 1).map(String::as_str),
            Some("rust,shell"),
            "capabilities value should follow the flag immediately"
        );
    }

    #[test]
    fn build_create_task_args_omits_capabilities_flag_when_empty() {
        let options = caps_options(vec![]);
        let args = CliCanopyClient::build_create_task_args("t", "d", ".", &options);

        assert!(
            !args.iter().any(|a| a == "--required-capabilities"),
            "--required-capabilities must not appear when capabilities are empty"
        );
    }

    // -- create_subtask arg-builder tests ---------------------------------------

    #[test]
    fn build_create_subtask_args_includes_capabilities_flag_when_set() {
        let options = caps_options(vec!["rust".to_string(), "shell".to_string()]);
        let args = CliCanopyClient::build_create_subtask_args("parent-1", "t", "d", &options);

        let pos = args
            .iter()
            .position(|a| a == "--required-capabilities")
            .expect("--required-capabilities should be present");
        assert_eq!(
            args.get(pos + 1).map(String::as_str),
            Some("rust,shell"),
            "capabilities value should follow the flag immediately"
        );
    }

    #[test]
    fn build_create_subtask_args_omits_capabilities_flag_when_empty() {
        let options = caps_options(vec![]);
        let args = CliCanopyClient::build_create_subtask_args("parent-1", "t", "d", &options);

        assert!(
            !args.iter().any(|a| a == "--required-capabilities"),
            "--required-capabilities must not appear when capabilities are empty"
        );
    }

    // -- create_task requested-by tests -------------------------------------------

    #[test]
    fn build_create_task_args_includes_requested_by() {
        let options = TaskOptions {
            requested_by: Some("hymenium".to_string()),
            ..Default::default()
        };
        let args = CliCanopyClient::build_create_task_args("t", "d", ".", &options);

        let pos = args
            .iter()
            .position(|a| a == "--requested-by")
            .expect("--requested-by should be present");
        assert_eq!(
            args.get(pos + 1).map(String::as_str),
            Some("hymenium"),
            "requested-by value should follow immediately"
        );
    }

    #[test]
    fn build_create_task_args_omits_requested_by_when_none() {
        let options = TaskOptions::default();
        let args = CliCanopyClient::build_create_task_args("t", "d", ".", &options);

        assert!(
            !args.iter().any(|a| a == "--requested-by"),
            "--requested-by must not appear when not set"
        );
    }

    #[test]
    fn build_create_task_args_omits_tier_flag() {
        let options = TaskOptions {
            required_tier: Some(crate::workflow::template::AgentTier::Opus),
            ..Default::default()
        };
        let args = CliCanopyClient::build_create_task_args("t", "d", ".", &options);

        // Verify tier is not rendered as a CLI flag (not supported by canopy)
        assert!(
            !args.iter().any(|a| a.contains("tier")),
            "tier-related flags must not appear in create task args"
        );
    }

    // -- assign_task args tests ---------------------------------------------------

    #[test]
    fn build_assign_task_args_uses_named_flags() {
        let args = CliCanopyClient::build_assign_task_args("task-1", "agent-1", "hymenium");

        let task_pos = args
            .iter()
            .position(|a| a == "--task-id")
            .expect("--task-id should be present");
        assert_eq!(
            args.get(task_pos + 1).map(String::as_str),
            Some("task-1"),
            "--task-id value"
        );

        let to_pos = args
            .iter()
            .position(|a| a == "--assigned-to")
            .expect("--assigned-to should be present");
        assert_eq!(
            args.get(to_pos + 1).map(String::as_str),
            Some("agent-1"),
            "--assigned-to value"
        );

        let by_pos = args
            .iter()
            .position(|a| a == "--assigned-by")
            .expect("--assigned-by should be present");
        assert_eq!(
            args.get(by_pos + 1).map(String::as_str),
            Some("hymenium"),
            "--assigned-by value"
        );
    }

    #[test]
    fn build_create_subtask_args_includes_requested_by() {
        let options = TaskOptions {
            requested_by: Some("workflow-engine".to_string()),
            ..Default::default()
        };
        let args = CliCanopyClient::build_create_subtask_args("parent-1", "t", "d", &options);

        let pos = args
            .iter()
            .position(|a| a == "--requested-by")
            .expect("--requested-by should be present");
        assert_eq!(
            args.get(pos + 1).map(String::as_str),
            Some("workflow-engine"),
            "requested-by value should follow immediately"
        );
    }

    #[test]
    fn build_create_subtask_args_omits_tier_flag() {
        let options = TaskOptions {
            required_tier: Some(crate::workflow::template::AgentTier::Sonnet),
            ..Default::default()
        };
        let args = CliCanopyClient::build_create_subtask_args("parent-1", "t", "d", &options);

        // Verify tier is not rendered as a CLI flag (not supported by canopy)
        assert!(
            !args.iter().any(|a| a.contains("tier")),
            "tier-related flags must not appear in create subtask args"
        );
    }

    // -- runtime identity: workflow_id / phase_id passing ----------------------

    #[test]
    fn build_create_subtask_args_includes_workflow_id_and_phase_id() {
        let options = TaskOptions {
            workflow_id: Some("wf-abc123".to_string()),
            phase_id: Some("implement".to_string()),
            ..Default::default()
        };
        let args = CliCanopyClient::build_create_subtask_args("parent-1", "t", "d", &options);

        let wid_pos = args
            .iter()
            .position(|a| a == "--workflow-id")
            .expect("--workflow-id should be present");
        assert_eq!(
            args.get(wid_pos + 1).map(String::as_str),
            Some("wf-abc123"),
            "--workflow-id value should follow the flag"
        );

        let pid_pos = args
            .iter()
            .position(|a| a == "--phase-id")
            .expect("--phase-id should be present");
        assert_eq!(
            args.get(pid_pos + 1).map(String::as_str),
            Some("implement"),
            "--phase-id value should follow the flag"
        );
    }

    #[test]
    fn build_create_subtask_args_omits_workflow_id_and_phase_id_when_none() {
        let options = TaskOptions::default();
        let args = CliCanopyClient::build_create_subtask_args("parent-1", "t", "d", &options);

        assert!(
            !args.iter().any(|a| a == "--workflow-id"),
            "--workflow-id must not appear when not set"
        );
        assert!(
            !args.iter().any(|a| a == "--phase-id"),
            "--phase-id must not appear when not set"
        );
    }

    #[test]
    fn build_create_task_args_omits_workflow_id_and_phase_id_when_none() {
        let options = TaskOptions::default();
        let args = CliCanopyClient::build_create_task_args("t", "d", ".", &options);

        assert!(
            !args.iter().any(|a| a == "--workflow-id"),
            "--workflow-id must not appear when not set"
        );
        assert!(
            !args.iter().any(|a| a == "--phase-id"),
            "--phase-id must not appear when not set"
        );
    }

    #[test]
    fn build_create_task_args_includes_workflow_id_and_phase_id() {
        let options = TaskOptions {
            workflow_id: Some("wf-xyz789".to_string()),
            phase_id: Some("audit".to_string()),
            ..Default::default()
        };
        let args = CliCanopyClient::build_create_task_args("t", "d", ".", &options);

        let wid_pos = args
            .iter()
            .position(|a| a == "--workflow-id")
            .expect("--workflow-id should be present");
        assert_eq!(
            args.get(wid_pos + 1).map(String::as_str),
            Some("wf-xyz789"),
            "--workflow-id value should follow the flag"
        );

        let pid_pos = args
            .iter()
            .position(|a| a == "--phase-id")
            .expect("--phase-id should be present");
        assert_eq!(
            args.get(pid_pos + 1).map(String::as_str),
            Some("audit"),
            "--phase-id value should follow the flag"
        );
    }
}
