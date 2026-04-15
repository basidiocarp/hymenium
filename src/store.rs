//! `SQLite` persistence for workflow state.
//!
//! The `WorkflowStore` is the single authority for durable workflow records.
//! It records the full lifecycle — instances, phase states, and transitions —
//! so Hymenium can be queried as the source of execution truth.
//!
//! Default database path: `~/.local/share/hymenium/hymenium.db`
//! Override with the `HYMENIUM_DB` environment variable.

use crate::outcome::WorkflowOutcome;
use crate::workflow::engine::{PhaseState, PhaseStatus, WorkflowInstance, WorkflowStatus};
use crate::workflow::template::{AgentRole, WorkflowTemplate};
use crate::workflow::WorkflowId;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error type for workflow store operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("workflow not found: {0}")]
    NotFound(String),

    #[error("invalid stored value '{value}' for field '{field}': {reason}")]
    InvalidValue {
        field: &'static str,
        value: String,
        reason: String,
    },

    #[error("nested transactions are not supported")]
    NestedTransaction,
}

// ---------------------------------------------------------------------------
// WorkflowStore
// ---------------------------------------------------------------------------

/// Durable `SQLite` store for workflow instances, phase states, and transitions.
pub struct WorkflowStore {
    /// Path to the `SQLite` database file.
    pub db_path: PathBuf,
    conn: Connection,
}

impl std::fmt::Debug for WorkflowStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowStore")
            .field("db_path", &self.db_path)
            .finish_non_exhaustive()
    }
}

impl WorkflowStore {
    /// Open (or create) the workflow database at `path`, running schema migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                    Some(format!("could not create db directory: {e}")),
                )
            })?;
        }
        let conn = Connection::open(path)?;
        // Enable foreign key enforcement.
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let store = Self {
            db_path: path.to_owned(),
            conn,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Return the default database path, honoring the `HYMENIUM_DB` override.
    ///
    /// Follows XDG conventions: `$XDG_DATA_HOME/hymenium/hymenium.db` on Linux,
    /// or `~/.local/share/hymenium/hymenium.db` as a fallback.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("HYMENIUM_DB") {
            return PathBuf::from(p);
        }
        // Try XDG_DATA_HOME first, then HOME-relative fallback.
        let base = std::env::var("XDG_DATA_HOME").map_or_else(
            |_| {
                std::env::var("HOME").map_or_else(
                    |_| PathBuf::from("."),
                    |h| PathBuf::from(h).join(".local").join("share"),
                )
            },
            PathBuf::from,
        );
        base.join("hymenium").join("hymenium.db")
    }

    // -----------------------------------------------------------------------
    // Schema migrations
    // -----------------------------------------------------------------------

    fn migrate(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS workflows (
                workflow_id   TEXT PRIMARY KEY,
                template_id   TEXT NOT NULL,
                handoff_path  TEXT NOT NULL,
                status        TEXT NOT NULL,
                current_phase TEXT,
                blocked_on    TEXT,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                template_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS phase_states (
                workflow_id    TEXT NOT NULL,
                phase_id       TEXT NOT NULL,
                role           TEXT NOT NULL,
                status         TEXT NOT NULL,
                agent_id       TEXT,
                started_at     TEXT,
                completed_at   TEXT,
                canopy_task_id TEXT,
                retry_count    INTEGER NOT NULL DEFAULT 0,
                phase_order    INTEGER NOT NULL,
                failure_reason TEXT,
                PRIMARY KEY (workflow_id, phase_id),
                FOREIGN KEY (workflow_id) REFERENCES workflows(workflow_id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS workflow_transitions (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_id     TEXT NOT NULL,
                from_phase      TEXT,
                to_phase        TEXT,
                transitioned_at TEXT NOT NULL,
                reason          TEXT,
                FOREIGN KEY (workflow_id) REFERENCES workflows(workflow_id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS workflow_outcomes (
                workflow_id  TEXT PRIMARY KEY,
                outcome_json TEXT NOT NULL,
                created_at   TEXT NOT NULL
            );
            ",
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Insert a new workflow instance (must not already exist).
    pub fn insert_workflow(&self, inst: &WorkflowInstance) -> Result<(), StoreError> {
        let template_json = serde_json::to_string(&inst.template)?;
        let current_phase = inst.current_phase().map(|p| p.phase_id.as_str());

        self.conn.execute(
            "INSERT INTO workflows
                (workflow_id, template_id, handoff_path, status, current_phase,
                 blocked_on, created_at, updated_at, template_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                inst.workflow_id.0,
                inst.template.template_id,
                inst.handoff_path,
                inst.status.to_string(),
                current_phase,
                inst.blocked_on,
                inst.created_at.to_rfc3339(),
                inst.updated_at.to_rfc3339(),
                template_json,
            ],
        )?;

        for (order, state) in inst.phase_states.iter().enumerate() {
            self.upsert_phase_state(&inst.workflow_id, state, order)?;
        }

        Ok(())
    }

    /// Load a workflow instance by ID, returning `None` if not found.
    pub fn get_workflow(&self, id: &WorkflowId) -> Result<Option<WorkflowInstance>, StoreError> {
        type Row = (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
        );

        let row: Result<Row, _> = self.conn.query_row(
            "SELECT workflow_id, template_id, handoff_path, status, current_phase,
                    blocked_on, created_at, updated_at, template_json
             FROM workflows WHERE workflow_id = ?1",
            params![id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,         // workflow_id
                    row.get::<_, String>(2)?,         // handoff_path
                    row.get::<_, String>(3)?,         // status
                    row.get::<_, Option<String>>(4)?, // current_phase
                    row.get::<_, Option<String>>(5)?, // blocked_on
                    row.get::<_, String>(6)?,         // created_at
                    row.get::<_, String>(7)?,         // updated_at
                    row.get::<_, String>(8)?,         // template_json
                ))
            },
        );

        let (
            wf_id,
            handoff_path,
            status_str,
            _current_phase,
            blocked_on,
            created_at_str,
            updated_at_str,
            template_json,
        ) = match row {
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(StoreError::Sqlite(e)),
            Ok(r) => r,
        };

        let status = parse_workflow_status(&status_str)?;
        let template: WorkflowTemplate = serde_json::from_str(&template_json)?;
        let created_at = parse_datetime(&created_at_str, "created_at")?;
        let updated_at = parse_datetime(&updated_at_str, "updated_at")?;

        let phase_states = self.load_phase_states(&WorkflowId(wf_id.clone()))?;

        // Restore current_phase_idx from loaded phase states.
        let current_phase_idx = phase_states
            .iter()
            .position(|p| p.status == PhaseStatus::Active)
            .or_else(|| {
                // If no active phase, find the last completed one and use next.
                // If none completed, default to 0.
                phase_states
                    .iter()
                    .rposition(|p| p.status == PhaseStatus::Completed)
                    .map(|i| (i + 1).min(phase_states.len().saturating_sub(1)))
            })
            .unwrap_or(0);

        Ok(Some(WorkflowInstance {
            workflow_id: WorkflowId(wf_id),
            template,
            handoff_path,
            status,
            blocked_on,
            current_phase_idx,
            phase_states,
            transitions: Vec::new(),
            created_at,
            updated_at,
        }))
    }

    /// Update the top-level status and `blocked_on` for a workflow.
    pub fn update_workflow_status(
        &self,
        id: &WorkflowId,
        status: &WorkflowStatus,
        blocked_on: Option<&str>,
    ) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE workflows SET status = ?1, blocked_on = ?2, updated_at = ?3
             WHERE workflow_id = ?4",
            params![
                status.to_string(),
                blocked_on,
                Utc::now().to_rfc3339(),
                id.0,
            ],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound(id.0.clone()));
        }
        Ok(())
    }

    /// List all workflows that are not in a terminal state.
    pub fn list_active_workflows(&self) -> Result<Vec<WorkflowInstance>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT workflow_id FROM workflows
             WHERE status NOT IN ('completed', 'failed', 'cancelled')
             ORDER BY created_at DESC",
        )?;

        let ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;

        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(inst) = self.get_workflow(&WorkflowId(id))? {
                results.push(inst);
            }
        }
        Ok(results)
    }

    /// Record a phase transition event in the audit log.
    pub fn record_transition(
        &self,
        id: &WorkflowId,
        from: Option<&str>,
        to: Option<&str>,
        reason: Option<&str>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO workflow_transitions (workflow_id, from_phase, to_phase, transitioned_at, reason)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.0,
                from,
                to,
                Utc::now().to_rfc3339(),
                reason,
            ],
        )?;
        Ok(())
    }

    /// Upsert a single phase state record.
    pub fn upsert_phase_state(
        &self,
        workflow_id: &WorkflowId,
        state: &PhaseState,
        order: usize,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO phase_states
                (workflow_id, phase_id, role, status, agent_id, started_at,
                 completed_at, canopy_task_id, retry_count, phase_order, failure_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(workflow_id, phase_id) DO UPDATE SET
                role = excluded.role,
                status = excluded.status,
                agent_id = excluded.agent_id,
                started_at = excluded.started_at,
                completed_at = excluded.completed_at,
                canopy_task_id = excluded.canopy_task_id,
                retry_count = excluded.retry_count,
                phase_order = excluded.phase_order,
                failure_reason = excluded.failure_reason",
            params![
                workflow_id.0,
                state.phase_id,
                state.role.to_string(),
                state.status.to_string(),
                state.agent_id,
                state.started_at.map(|t| t.to_rfc3339()),
                state.completed_at.map(|t| t.to_rfc3339()),
                state.canopy_task_id,
                state.retry_count,
                i64::try_from(order).unwrap_or(i64::MAX),
                state.failure_reason,
            ],
        )?;
        Ok(())
    }

    /// Persist a terminal workflow outcome.
    ///
    /// Uses `INSERT OR REPLACE` so calling this a second time (e.g. on retry
    /// of the emit step) is safe — the latest outcome wins.
    pub fn insert_outcome(&self, outcome: &WorkflowOutcome) -> Result<(), StoreError> {
        let json = serde_json::to_string(outcome)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO workflow_outcomes (workflow_id, outcome_json, created_at)
             VALUES (?1, ?2, ?3)",
            params![outcome.workflow_id.0, json, Utc::now().to_rfc3339(),],
        )?;
        Ok(())
    }

    /// Load the stored outcome for a workflow, returning `None` if not found.
    pub fn get_outcome(&self, id: &WorkflowId) -> Result<Option<WorkflowOutcome>, StoreError> {
        let row: Result<String, _> = self.conn.query_row(
            "SELECT outcome_json FROM workflow_outcomes WHERE workflow_id = ?1",
            params![id.0],
            |r| r.get(0),
        );
        match row {
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StoreError::Sqlite(e)),
            Ok(json) => {
                let outcome = serde_json::from_str(&json)?;
                Ok(Some(outcome))
            }
        }
    }

    /// Return `true` if a terminal outcome has already been recorded for this workflow.
    pub fn outcome_exists(&self, id: &WorkflowId) -> Result<bool, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM workflow_outcomes WHERE workflow_id = ?1",
            params![id.0],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    /// Execute a sequence of operations within a single `SQLite` transaction.
    ///
    /// This ensures that either all operations succeed or all are rolled back,
    /// preventing partial state corruption when multiple persistence steps are needed.
    ///
    /// The closure can return any error type `E` as long as `StoreError` is convertible
    /// to `E` (typically via `#[from]`). This allows the caller's typed error enum to
    /// be returned directly from the closure without intermediate conversions.
    ///
    /// # Example
    ///
    /// ```
    /// use hymenium::store::{WorkflowStore, StoreError};
    /// # use std::path::PathBuf;
    /// # use std::time::{SystemTime, UNIX_EPOCH};
    ///
    /// let nanos = SystemTime::now()
    ///     .duration_since(UNIX_EPOCH)
    ///     .unwrap()
    ///     .subsec_nanos();
    /// let path = std::env::temp_dir().join(format!("hymenium_doctest_{}.db", nanos));
    /// let store = WorkflowStore::open(&path).expect("open store");
    /// let result = store.with_transaction::<_, _, StoreError>(|_s| Ok(42));
    /// assert!(result.is_ok());
    /// ```
    pub fn with_transaction<F, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Self) -> Result<T, E>,
        E: From<StoreError>,
    {
        if !self.conn.is_autocommit() {
            return Err(E::from(StoreError::NestedTransaction));
        }

        self.conn
            .execute_batch("BEGIN")
            .map_err(StoreError::from)
            .map_err(E::from)?;
        match f(self) {
            Ok(value) => {
                self.conn
                    .execute_batch("COMMIT")
                    .map_err(StoreError::from)
                    .map_err(E::from)?;
                Ok(value)
            }
            Err(e) => {
                if let Err(rollback_err) = self.conn.execute_batch("ROLLBACK") {
                    eprintln!("warning: rollback failed after error: {rollback_err}");
                }
                Err(e)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn load_phase_states(&self, workflow_id: &WorkflowId) -> Result<Vec<PhaseState>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT phase_id, role, status, agent_id, started_at, completed_at,
                    canopy_task_id, retry_count, failure_reason
             FROM phase_states
             WHERE workflow_id = ?1
             ORDER BY phase_order ASC",
        )?;

        let rows = stmt
            .query_map(params![workflow_id.0], |row| {
                Ok((
                    row.get::<_, String>(0)?,         // phase_id
                    row.get::<_, String>(1)?,         // role
                    row.get::<_, String>(2)?,         // status
                    row.get::<_, Option<String>>(3)?, // agent_id
                    row.get::<_, Option<String>>(4)?, // started_at
                    row.get::<_, Option<String>>(5)?, // completed_at
                    row.get::<_, Option<String>>(6)?, // canopy_task_id
                    row.get::<_, u32>(7)?,            // retry_count
                    row.get::<_, Option<String>>(8)?, // failure_reason
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut result = Vec::with_capacity(rows.len());
        for (
            phase_id,
            role_str,
            status_str,
            agent_id,
            started_str,
            completed_str,
            canopy_id,
            retry_count,
            failure_reason,
        ) in rows
        {
            let role = parse_agent_role(&role_str)?;
            let phase_status = parse_phase_status(&status_str)?;
            let started_at = started_str
                .map(|s| parse_datetime(&s, "started_at"))
                .transpose()?;
            let completed_at = completed_str
                .map(|s| parse_datetime(&s, "completed_at"))
                .transpose()?;

            result.push(PhaseState {
                phase_id,
                role,
                status: phase_status,
                agent_id,
                canopy_task_id: canopy_id,
                started_at,
                completed_at,
                failure_reason,
                retry_count,
            });
        }

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Parse helpers
// ---------------------------------------------------------------------------

fn parse_workflow_status(s: &str) -> Result<WorkflowStatus, StoreError> {
    match s {
        "pending" => Ok(WorkflowStatus::Pending),
        "dispatched" => Ok(WorkflowStatus::Dispatched),
        "in_progress" => Ok(WorkflowStatus::InProgress),
        "blocked_on_gate" => Ok(WorkflowStatus::BlockedOnGate),
        "awaiting_repair" => Ok(WorkflowStatus::AwaitingRepair),
        "completed" => Ok(WorkflowStatus::Completed),
        "failed" => Ok(WorkflowStatus::Failed),
        "cancelled" => Ok(WorkflowStatus::Cancelled),
        other => Err(StoreError::InvalidValue {
            field: "status",
            value: other.to_string(),
            reason: "unknown workflow status".to_string(),
        }),
    }
}

fn parse_phase_status(s: &str) -> Result<PhaseStatus, StoreError> {
    match s {
        "pending" => Ok(PhaseStatus::Pending),
        "active" => Ok(PhaseStatus::Active),
        "completed" => Ok(PhaseStatus::Completed),
        "failed" => Ok(PhaseStatus::Failed),
        "skipped" => Ok(PhaseStatus::Skipped),
        other => Err(StoreError::InvalidValue {
            field: "phase status",
            value: other.to_string(),
            reason: "unknown phase status".to_string(),
        }),
    }
}

fn parse_agent_role(s: &str) -> Result<AgentRole, StoreError> {
    // Deserialize via serde_json round-trip to reuse the canonical serde renames.
    let json = serde_json::Value::String(s.to_string());
    serde_json::from_value(json).map_err(|_| StoreError::InvalidValue {
        field: "role",
        value: s.to_string(),
        reason: "unknown agent role".to_string(),
    })
}

fn parse_datetime(s: &str, field: &'static str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| StoreError::InvalidValue {
            field,
            value: s.to_string(),
            reason: e.to_string(),
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::template::impl_audit_default;

    fn temp_store() -> WorkflowStore {
        // Use an in-memory SQLite database for unit tests.
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        let store = WorkflowStore {
            db_path: PathBuf::from(":memory:"),
            conn,
        };
        store.migrate().expect("migrate");
        store
    }

    fn make_instance(id: &str) -> WorkflowInstance {
        WorkflowInstance::new(
            WorkflowId(id.to_string()),
            impl_audit_default(),
            "/path/to/handoff.md",
        )
    }

    #[test]
    fn test_round_trip_insert_get() {
        let store = temp_store();
        let inst = make_instance("01JNQWF0000000000000000001");

        store.insert_workflow(&inst).expect("insert");

        let loaded = store
            .get_workflow(&inst.workflow_id)
            .expect("get")
            .expect("should exist");

        assert_eq!(loaded.workflow_id, inst.workflow_id);
        assert_eq!(loaded.status, WorkflowStatus::Pending);
        assert_eq!(loaded.handoff_path, "/path/to/handoff.md");
        assert_eq!(loaded.phase_states.len(), 2);
        assert_eq!(loaded.phase_states[0].phase_id, "implement");
        assert_eq!(loaded.phase_states[1].phase_id, "audit");
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let store = temp_store();
        let result = store
            .get_workflow(&WorkflowId("not-a-real-id".to_string()))
            .expect("query should not error");
        assert!(result.is_none());
    }

    #[test]
    fn test_update_workflow_status() {
        let store = temp_store();
        let inst = make_instance("01JNQWF0000000000000000002");
        store.insert_workflow(&inst).expect("insert");

        store
            .update_workflow_status(
                &inst.workflow_id,
                &WorkflowStatus::BlockedOnGate,
                Some("exit gate: code_diff_exists not satisfied"),
            )
            .expect("update");

        let loaded = store
            .get_workflow(&inst.workflow_id)
            .expect("get")
            .expect("should exist");

        assert_eq!(loaded.status, WorkflowStatus::BlockedOnGate);
        assert_eq!(
            loaded.blocked_on.as_deref(),
            Some("exit gate: code_diff_exists not satisfied")
        );
    }

    #[test]
    fn test_list_active_excludes_terminal() {
        let store = temp_store();

        let active = make_instance("01JNQWF0000000000000000003");
        store.insert_workflow(&active).expect("insert active");

        let mut done = make_instance("01JNQWF0000000000000000004");
        done.status = WorkflowStatus::Completed;
        store.insert_workflow(&done).expect("insert completed");

        let active_list = store.list_active_workflows().expect("list");
        assert_eq!(active_list.len(), 1);
        assert_eq!(active_list[0].workflow_id.0, "01JNQWF0000000000000000003");
    }

    #[test]
    fn test_record_transition() {
        let store = temp_store();
        let inst = make_instance("01JNQWF0000000000000000005");
        store.insert_workflow(&inst).expect("insert");

        store
            .record_transition(
                &inst.workflow_id,
                Some("implement"),
                Some("audit"),
                Some("gates satisfied"),
            )
            .expect("record");

        // Verify row exists.
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM workflow_transitions WHERE workflow_id = ?1",
                params![inst.workflow_id.0],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 1);
    }

    // -- outcome persistence -------------------------------------------------

    #[test]
    fn test_insert_outcome_round_trip() {
        use crate::outcome::WorkflowOutcome;
        use crate::workflow::engine::WorkflowStatus;

        let store = temp_store();
        let mut inst = make_instance("01JNQWF0000000000000000006");
        inst.status = WorkflowStatus::Completed;
        store.insert_workflow(&inst).expect("insert workflow");

        let outcome = WorkflowOutcome::build(&inst, None, Utc::now());
        store.insert_outcome(&outcome).expect("insert outcome");

        // Verify the row is present.
        assert!(
            store.outcome_exists(&inst.workflow_id).expect("check"),
            "outcome should exist after insert"
        );

        // Verify we can read the JSON back and it has the required keys.
        let json_str: String = store
            .conn
            .query_row(
                "SELECT outcome_json FROM workflow_outcomes WHERE workflow_id = ?1",
                params![inst.workflow_id.0],
                |r| r.get(0),
            )
            .expect("read outcome_json");

        let value: serde_json::Value = serde_json::from_str(&json_str).expect("parse outcome json");
        assert_eq!(value["schema_version"], "1.0");
        assert_eq!(value["terminal_status"], "completed");
        assert!(value["attempt_count"].as_i64().unwrap_or(0) >= 1);
    }

    #[test]
    fn test_outcome_not_exists_before_insert() {
        let store = temp_store();
        let inst = make_instance("01JNQWF0000000000000000007");
        store.insert_workflow(&inst).expect("insert workflow");

        assert!(
            !store.outcome_exists(&inst.workflow_id).expect("check"),
            "outcome should not exist before insert"
        );
    }

    #[test]
    fn test_insert_outcome_replace_is_safe() {
        use crate::outcome::WorkflowOutcome;
        use crate::workflow::engine::WorkflowStatus;

        let store = temp_store();
        let mut inst = make_instance("01JNQWF0000000000000000008");
        inst.status = WorkflowStatus::Completed;
        store.insert_workflow(&inst).expect("insert workflow");

        let outcome = WorkflowOutcome::build(&inst, None, Utc::now());
        store.insert_outcome(&outcome).expect("first insert");
        // Inserting again (INSERT OR REPLACE) should not error.
        store
            .insert_outcome(&outcome)
            .expect("second insert (replace)");

        // Still exactly one row.
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM workflow_outcomes WHERE workflow_id = ?1",
                params![inst.workflow_id.0],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 1);
    }
}
