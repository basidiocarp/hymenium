use super::{
    CanopyClient, CompletenessReport, DispatchError, ImportResult, TaskDetail, TaskOptions,
};
use std::cell::RefCell;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// MockCanopyClient
// ---------------------------------------------------------------------------

/// In-memory canopy client for testing.
///
/// Tracks tasks in a `HashMap` behind a `RefCell` so tests can observe the
/// side-effects of dispatch operations without a running canopy instance.
#[derive(Debug)]
pub struct MockCanopyClient {
    tasks: RefCell<HashMap<String, TaskDetail>>,
    next_id: RefCell<u64>,
    default_completeness: CompletenessReport,
}

impl MockCanopyClient {
    /// Create a new mock client with no tasks and a default "all complete" report.
    pub fn new() -> Self {
        Self {
            tasks: RefCell::new(HashMap::new()),
            next_id: RefCell::new(1),
            default_completeness: CompletenessReport {
                complete: true,
                total_items: 0,
                completed_items: 0,
                missing: Vec::new(),
            },
        }
    }

    /// Override the completeness report returned by `check_completeness`.
    pub fn with_completeness(mut self, report: CompletenessReport) -> Self {
        self.default_completeness = report;
        self
    }

    /// Return how many tasks have been created so far.
    pub fn task_count(&self) -> usize {
        self.tasks.borrow().len()
    }

    /// Look up a task in the internal store (test helper).
    pub fn stored_task(&self, id: &str) -> Option<TaskDetail> {
        self.tasks.borrow().get(id).cloned()
    }

    fn next_task_id(&self) -> String {
        let mut id = self.next_id.borrow_mut();
        let task_id = format!("mock-task-{id}");
        *id += 1;
        task_id
    }
}

impl Default for MockCanopyClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CanopyClient for MockCanopyClient {
    fn create_task(
        &self,
        title: &str,
        description: &str,
        _project_root: &str,
        _options: &TaskOptions,
    ) -> Result<String, DispatchError> {
        let task_id = self.next_task_id();
        let detail = TaskDetail {
            task_id: task_id.clone(),
            title: title.to_string(),
            status: "pending".to_string(),
            agent_id: None,
            parent_id: None,
        };
        self.tasks.borrow_mut().insert(task_id.clone(), detail);
        let _ = description; // description not stored in mock — check title in tests
        Ok(task_id)
    }

    fn create_subtask(
        &self,
        parent_id: &str,
        title: &str,
        description: &str,
        _options: &TaskOptions,
    ) -> Result<String, DispatchError> {
        if !self.tasks.borrow().contains_key(parent_id) {
            return Err(DispatchError::TaskCreationFailed(format!(
                "parent task {parent_id} not found"
            )));
        }
        let task_id = self.next_task_id();
        let detail = TaskDetail {
            task_id: task_id.clone(),
            title: title.to_string(),
            status: "pending".to_string(),
            agent_id: None,
            parent_id: Some(parent_id.to_string()),
        };
        self.tasks.borrow_mut().insert(task_id.clone(), detail);
        let _ = description;
        Ok(task_id)
    }

    fn assign_task(&self, task_id: &str, agent_id: &str) -> Result<(), DispatchError> {
        let mut tasks = self.tasks.borrow_mut();
        let detail = tasks.get_mut(task_id).ok_or_else(|| {
            DispatchError::InvalidState(format!("task {task_id} not found"))
        })?;
        detail.agent_id = Some(agent_id.to_string());
        detail.status = "assigned".to_string();
        Ok(())
    }

    fn get_task(&self, task_id: &str) -> Result<TaskDetail, DispatchError> {
        self.tasks
            .borrow()
            .get(task_id)
            .cloned()
            .ok_or_else(|| DispatchError::InvalidState(format!("task {task_id} not found")))
    }

    fn check_completeness(
        &self,
        _handoff_path: &str,
    ) -> Result<CompletenessReport, DispatchError> {
        Ok(self.default_completeness.clone())
    }

    fn import_handoff(
        &self,
        _path: &str,
        assign_to: Option<&str>,
    ) -> Result<ImportResult, DispatchError> {
        let task_id = self.next_task_id();
        let detail = TaskDetail {
            task_id: task_id.clone(),
            title: "imported handoff".to_string(),
            status: "pending".to_string(),
            agent_id: assign_to.map(String::from),
            parent_id: None,
        };
        self.tasks.borrow_mut().insert(task_id.clone(), detail);
        Ok(ImportResult {
            task_id,
            subtask_ids: Vec::new(),
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
    fn mock_create_get_roundtrip() {
        let mock = MockCanopyClient::new();
        let id = mock
            .create_task("test task", "description", ".", &TaskOptions::default())
            .expect("create_task should succeed");

        let detail = mock.get_task(&id).expect("get_task should succeed");
        assert_eq!(detail.title, "test task");
        assert_eq!(detail.status, "pending");
        assert!(detail.agent_id.is_none());
    }

    #[test]
    fn mock_assign_updates_task() {
        let mock = MockCanopyClient::new();
        let id = mock
            .create_task("task", "desc", ".", &TaskOptions::default())
            .expect("create_task");
        mock.assign_task(&id, "agent-1").expect("assign_task");

        let detail = mock.get_task(&id).expect("get_task");
        assert_eq!(detail.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(detail.status, "assigned");
    }

    #[test]
    fn mock_subtask_tracks_parent() {
        let mock = MockCanopyClient::new();
        let parent = mock
            .create_task("parent", "desc", ".", &TaskOptions::default())
            .expect("create parent");
        let child = mock
            .create_subtask(&parent, "child", "desc", &TaskOptions::default())
            .expect("create subtask");

        let detail = mock.get_task(&child).expect("get subtask");
        assert_eq!(detail.parent_id.as_deref(), Some(parent.as_str()));
    }

    #[test]
    fn mock_subtask_missing_parent_fails() {
        let mock = MockCanopyClient::new();
        let result = mock.create_subtask("nonexistent", "child", "desc", &TaskOptions::default());
        assert!(result.is_err());
    }

    #[test]
    fn mock_check_completeness_default() {
        let mock = MockCanopyClient::new();
        let report = mock
            .check_completeness("/some/handoff.md")
            .expect("should succeed");
        assert!(report.complete);
        assert!(report.missing.is_empty());
    }

    #[test]
    fn mock_import_handoff_returns_result() {
        let mock = MockCanopyClient::new();
        let result = mock
            .import_handoff("/path/to/handoff.md", Some("agent-1"))
            .expect("import should succeed");
        assert!(!result.task_id.is_empty());

        let task = mock.get_task(&result.task_id).expect("task should exist");
        assert_eq!(task.agent_id.as_deref(), Some("agent-1"));
    }
}
