use super::{
    CanopyClient, CompletenessReport, DispatchError, ImportResult, TaskDetail, TaskOptions,
};

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
            args.push(role.to_string());
        }
        if let Some(ref tier) = options.required_tier {
            args.push("--required-tier".to_string());
            args.push(tier.to_string());
        }
        if options.verification_required {
            args.push("--verification-required".to_string());
        }
        // Pass capability requirements as a comma-separated list matching canopy's
        // --required-capabilities flag (value_delimiter = ',').
        if !options.required_capabilities.is_empty() {
            args.push("--required-capabilities".to_string());
            args.push(options.required_capabilities.join(","));
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
            args.push(role.to_string());
        }
        if let Some(ref tier) = options.required_tier {
            args.push("--required-tier".to_string());
            args.push(tier.to_string());
        }
        if options.verification_required {
            args.push("--verification-required".to_string());
        }
        // Pass capability requirements as a comma-separated list matching canopy's
        // --required-capabilities flag (value_delimiter = ',').
        if !options.required_capabilities.is_empty() {
            args.push("--required-capabilities".to_string());
            args.push(options.required_capabilities.join(","));
        }
        args
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
        self.run(&args)
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
        self.run(&args)
    }

    fn assign_task(&self, task_id: &str, agent_id: &str) -> Result<(), DispatchError> {
        self.run(&["task", "assign", task_id, "--agent", agent_id])?;
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
}
