use super::{CanopyClient, CompletenessReport, DispatchError, ImportResult, TaskDetail, TaskOptions};

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

impl CanopyClient for CliCanopyClient {
    fn create_task(
        &self,
        title: &str,
        description: &str,
        project_root: &str,
        options: &TaskOptions,
    ) -> Result<String, DispatchError> {
        let mut args = vec![
            "task",
            "create",
            "--title",
            title,
            "--description",
            description,
            "--project-root",
            project_root,
        ];
        let role_str;
        if let Some(ref role) = options.required_role {
            role_str = role.to_string();
            args.push("--required-role");
            args.push(&role_str);
        }
        let tier_str;
        if let Some(ref tier) = options.required_tier {
            tier_str = tier.to_string();
            args.push("--required-tier");
            args.push(&tier_str);
        }
        if options.verification_required {
            args.push("--verification-required");
        }
        self.run(&args)
    }

    fn create_subtask(
        &self,
        parent_id: &str,
        title: &str,
        description: &str,
        options: &TaskOptions,
    ) -> Result<String, DispatchError> {
        let mut args = vec![
            "task",
            "create",
            "--parent",
            parent_id,
            "--title",
            title,
            "--description",
            description,
        ];
        let role_str;
        if let Some(ref role) = options.required_role {
            role_str = role.to_string();
            args.push("--required-role");
            args.push(&role_str);
        }
        let tier_str;
        if let Some(ref tier) = options.required_tier {
            tier_str = tier.to_string();
            args.push("--required-tier");
            args.push(&tier_str);
        }
        if options.verification_required {
            args.push("--verification-required");
        }
        self.run(&args)
    }

    fn assign_task(&self, task_id: &str, agent_id: &str) -> Result<(), DispatchError> {
        self.run(&["task", "assign", task_id, "--agent", agent_id])?;
        Ok(())
    }

    fn get_task(&self, task_id: &str) -> Result<TaskDetail, DispatchError> {
        let json = self.run(&["task", "get", task_id, "--json"])?;
        serde_json::from_str(&json).map_err(|e| {
            DispatchError::CanopyError(format!("failed to parse task detail: {e}"))
        })
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
        serde_json::from_str(&json).map_err(|e| {
            DispatchError::CanopyError(format!("failed to parse import result: {e}"))
        })
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
}
