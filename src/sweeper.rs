//! Runtime sweeper: heartbeat timeout, orphan failure, reconciliation, and GC.
//!
//! The sweeper runs as a background thread at [`SWEEP_INTERVAL`] and performs
//! four phases on each tick:
//!
//! 1. **Heartbeat timeout** — runtimes that have not sent a heartbeat within
//!    [`HEARTBEAT_TIMEOUT`] are marked [`RuntimeStatus::Offline`].
//! 2. **Orphan failure** — active workflow phases owned by newly-offline runtimes
//!    are transitioned to failed with reason `"runtime went offline"`.
//! 3. **Status reconciliation** — ensures no active phase claims a runtime that
//!    is currently offline, guarding against races or missed transitions.
//! 4. **GC** — runtime entries that have been offline longer than
//!    [`GC_RETENTION`] are removed entirely.
//!
//! The sweeper is non-blocking: if the runtime registry and phase states are
//! both empty, the sweep completes cleanly with no writes.

use crate::store::StoreError;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use thiserror::Error;
use tracing::{trace, warn};

// ---------------------------------------------------------------------------
// Constants (configurable via environment variables at construction time)
// ---------------------------------------------------------------------------

/// How often the sweeper wakes up and runs all four phases.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// How long a runtime may go without a heartbeat before it is marked offline.
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);

/// How long an offline runtime is retained before it is garbage-collected.
pub const GC_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error type for sweeper operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SweeperError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("sweeper thread panicked")]
    ThreadPanic,
}

// ---------------------------------------------------------------------------
// Runtime registry types
// ---------------------------------------------------------------------------

/// Liveness status of a registered runtime.
///
/// A runtime starts [`Online`] when it registers. The sweeper transitions it
/// to [`Offline`] after a missed-heartbeat threshold. Entries remain in the
/// registry until they reach the GC retention threshold, at which point they
/// are removed permanently.
///
/// [`Online`]: RuntimeStatus::Online
/// [`Offline`]: RuntimeStatus::Offline
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeStatus {
    /// The runtime is sending heartbeats within the expected interval.
    Online,
    /// The runtime has not sent a heartbeat within [`HEARTBEAT_TIMEOUT`].
    Offline,
}

impl RuntimeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeStatus::Online => "online",
            RuntimeStatus::Offline => "offline",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "online" => Some(RuntimeStatus::Online),
            "offline" => Some(RuntimeStatus::Offline),
            _ => None,
        }
    }
}

/// A runtime entry in the registry.
#[derive(Debug, Clone)]
pub struct RuntimeEntry {
    pub runtime_id: String,
    pub status: RuntimeStatus,
    pub last_heartbeat: DateTime<Utc>,
    pub registered_at: DateTime<Utc>,
    /// When the runtime transitioned to offline; `None` if still online.
    pub went_offline_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// RuntimeRegistry — thin wrapper around a SQLite connection
// ---------------------------------------------------------------------------

/// Persistent registry of runtimes and their liveness state.
///
/// Uses the same `SQLite` database as the workflow store, sharing the file but
/// opening a separate connection so the sweeper thread can hold it without
/// borrowing `WorkflowStore`.
pub struct RuntimeRegistry {
    conn: Connection,
}

impl std::fmt::Debug for RuntimeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeRegistry").finish_non_exhaustive()
    }
}

impl RuntimeRegistry {
    /// Open the registry at the given `SQLite` database path, running migrations.
    pub fn open(db_path: impl Into<PathBuf>) -> Result<Self, SweeperError> {
        let path: PathBuf = db_path.into();
        let conn = Connection::open(&path)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let registry = Self { conn };
        registry.migrate()?;
        Ok(registry)
    }

    /// Open an in-memory registry (for tests).
    pub fn open_in_memory() -> Result<Self, SweeperError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let registry = Self { conn };
        registry.migrate()?;
        Ok(registry)
    }

    fn migrate(&self) -> Result<(), SweeperError> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS runtimes (
                runtime_id       TEXT PRIMARY KEY,
                status           TEXT NOT NULL DEFAULT 'online',
                last_heartbeat   TEXT NOT NULL,
                registered_at    TEXT NOT NULL,
                went_offline_at  TEXT
            );
            ",
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Register a runtime or refresh an existing entry.
    ///
    /// If the runtime already exists, its `last_heartbeat` is updated and its
    /// status is reset to `online`. This doubles as the heartbeat upsert.
    pub fn upsert_heartbeat(
        &self,
        runtime_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), SweeperError> {
        self.conn.execute(
            "INSERT INTO runtimes (runtime_id, status, last_heartbeat, registered_at, went_offline_at)
             VALUES (?1, 'online', ?2, ?2, NULL)
             ON CONFLICT(runtime_id) DO UPDATE SET
                 status          = 'online',
                 last_heartbeat  = excluded.last_heartbeat,
                 went_offline_at = NULL",
            params![runtime_id, now.to_rfc3339()],
        )?;
        Ok(())
    }

    /// Return all runtime entries with timestamp parse errors, ordered by registration time.
    ///
    /// Timestamp parse errors are collected and returned as a vector of error strings.
    /// The method continues processing even when timestamps are corrupt, using fallback
    /// values (current time) so that sweep operations can continue.
    fn list_all_with_errors(&self) -> Result<(Vec<RuntimeEntry>, Vec<String>), SweeperError> {
        let mut stmt = self.conn.prepare(
            "SELECT runtime_id, status, last_heartbeat, registered_at, went_offline_at
             FROM runtimes
             ORDER BY registered_at ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut entries = Vec::with_capacity(rows.len());
        let mut errors = Vec::new();

        for (id, status_str, hb_str, reg_str, offline_str) in rows {
            let status = RuntimeStatus::from_str(&status_str).unwrap_or(RuntimeStatus::Offline);

            // Parse last_heartbeat; if corrupt, log and record error but do NOT
            // substitute Utc::now(). Mark the runtime as potentially stale.
            let last_heartbeat = match parse_dt(&hb_str) {
                Ok(dt) => dt,
                Err(e) => {
                    warn!("sweeper: invalid last_heartbeat for {}: {e}", id);
                    errors.push(format!("corrupt timestamp for runtime {}: invalid last_heartbeat: {e}", id));
                    // Use a very old timestamp so the runtime will be marked offline
                    // on the next sweep, preventing orphan masking.
                    Utc::now() - chrono::Duration::days(365)
                }
            };

            let registered_at = match parse_dt(&reg_str) {
                Ok(dt) => dt,
                Err(e) => {
                    warn!("sweeper: invalid registered_at for {}: {e}", id);
                    errors.push(format!("corrupt timestamp for runtime {}: invalid registered_at: {e}", id));
                    // Use a very old timestamp as fallback
                    Utc::now() - chrono::Duration::days(365)
                }
            };

            let went_offline_at = offline_str.as_deref().and_then(|s| {
                parse_dt(s).map_err(|e| {
                    warn!("sweeper: invalid went_offline_at for {}: {e}", id);
                    errors.push(format!("corrupt timestamp for runtime {}: invalid went_offline_at: {e}", id));
                    e
                }).ok()
            });
            entries.push(RuntimeEntry {
                runtime_id: id,
                status,
                last_heartbeat,
                registered_at,
                went_offline_at,
            });
        }
        Ok((entries, errors))
    }

    /// Return all runtime entries, ordered by registration time.
    pub fn list_all(&self) -> Result<Vec<RuntimeEntry>, SweeperError> {
        let (entries, _errors) = self.list_all_with_errors()?;
        Ok(entries)
    }

    /// Return only runtimes that are currently offline.
    pub fn list_offline(&self) -> Result<Vec<RuntimeEntry>, SweeperError> {
        Ok(self
            .list_all()?
            .into_iter()
            .filter(|e| e.status == RuntimeStatus::Offline)
            .collect())
    }

    /// Mark a runtime as offline, recording when it went offline.
    fn mark_offline(&self, runtime_id: &str, now: DateTime<Utc>) -> Result<(), SweeperError> {
        self.conn.execute(
            "UPDATE runtimes SET status = 'offline', went_offline_at = ?1
             WHERE runtime_id = ?2 AND status != 'offline'",
            params![now.to_rfc3339(), runtime_id],
        )?;
        Ok(())
    }

    /// Delete runtime entries that have been offline longer than `retention`.
    fn gc_offline(&self, now: DateTime<Utc>, retention: Duration) -> Result<usize, SweeperError> {
        let threshold =
            now - chrono::Duration::from_std(retention).unwrap_or(chrono::Duration::weeks(7));
        let deleted = self.conn.execute(
            "DELETE FROM runtimes WHERE status = 'offline' AND went_offline_at < ?1",
            params![threshold.to_rfc3339()],
        )?;
        Ok(deleted)
    }

    // -----------------------------------------------------------------------
    // Orphan detection helpers
    // -----------------------------------------------------------------------

    /// Return `phase_id`s (with their `workflow_id` and `agent_id`) that are active
    /// and whose `agent_id` matches one of the given offline runtime IDs.
    ///
    /// Operates against the `phase_states` table already present in the
    /// workflow database (same file, same connection).
    ///
    /// To avoid exceeding SQLite's variable limit (999), chunks the input into
    /// batches of 999 and runs the query once per batch, merging results.
    fn active_phases_for_runtimes(
        &self,
        offline_ids: &[&str],
    ) -> Result<Vec<(String, String, String)>, SweeperError> {
        if offline_ids.is_empty() {
            return Ok(Vec::new());
        }

        const SQLITE_VARIABLE_LIMIT: usize = 999;
        let mut all_results = Vec::new();

        // Chunk the offline_ids into batches of SQLITE_VARIABLE_LIMIT.
        for chunk in offline_ids.chunks(SQLITE_VARIABLE_LIMIT) {
            if chunk.is_empty() {
                continue;
            }

            // Build a parameterized IN clause for this batch.
            let placeholders: String = chunk
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT workflow_id, phase_id, agent_id FROM phase_states
                 WHERE status = 'active' AND agent_id IN ({placeholders})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            all_results.extend(rows);
        }

        Ok(all_results)
    }

    /// Transition an active phase to failed with the given reason.
    fn fail_phase(
        &self,
        workflow_id: &str,
        phase_id: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<(), SweeperError> {
        self.conn.execute(
            "UPDATE phase_states
             SET status = 'failed', completed_at = ?1, failure_reason = ?2
             WHERE workflow_id = ?3 AND phase_id = ?4 AND status = 'active'",
            params![now.to_rfc3339(), reason, workflow_id, phase_id],
        )?;
        Ok(())
    }

    /// Reconcile: find active phases whose `agent_id` belongs to an offline
    /// runtime but was not caught by the orphan step (e.g. went offline before
    /// this process started). Returns count of phases corrected.
    fn reconcile_offline_phases(&self, now: DateTime<Utc>) -> Result<usize, SweeperError> {
        // Collect offline runtime IDs.
        let mut stmt = self
            .conn
            .prepare("SELECT runtime_id FROM runtimes WHERE status = 'offline'")?;
        let offline: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        if offline.is_empty() {
            return Ok(0);
        }
        let ids_ref: Vec<&str> = offline.iter().map(String::as_str).collect();
        let orphans = self.active_phases_for_runtimes(&ids_ref)?;
        let count = orphans.len();
        for (wf_id, ph_id, _agent) in &orphans {
            self.fail_phase(wf_id, ph_id, "runtime went offline", now)?;
        }
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// Sweep cycle
// ---------------------------------------------------------------------------

/// Run one complete sweep against `registry`.
///
/// Returns a `SweepReport` summarising each phase's outcome. The sweep is
/// designed to be called from the background thread started by [`Sweeper`],
/// but it is also callable synchronously for testing.
pub fn run_sweep(
    registry: &RuntimeRegistry,
    heartbeat_timeout: Duration,
    gc_retention: Duration,
) -> SweepReport {
    let now = Utc::now();
    let mut report = SweepReport::default();

    // -- Phase 1: heartbeat timeout ------------------------------------------
    let all_runtimes = match registry.list_all_with_errors() {
        Ok((r, ts_errors)) => {
            // Surface timestamp parse errors in the report
            for ts_err in ts_errors {
                report.errors.push(ts_err);
            }
            r
        }
        Err(e) => {
            warn!("sweeper: failed to list runtimes: {e}");
            report.errors.push(format!("list runtimes: {e}"));
            return report;
        }
    };

    let mut newly_offline: Vec<String> = Vec::new();
    for entry in &all_runtimes {
        if entry.status == RuntimeStatus::Online {
            let age = now.signed_duration_since(entry.last_heartbeat);
            let timeout = chrono::Duration::from_std(heartbeat_timeout)
                .unwrap_or(chrono::Duration::seconds(45));
            if age > timeout {
                trace!(
                    runtime_id = %entry.runtime_id,
                    last_heartbeat = %entry.last_heartbeat,
                    "sweeper: marking runtime offline (heartbeat timeout)"
                );
                if let Err(e) = registry.mark_offline(&entry.runtime_id, now) {
                    warn!("sweeper: failed to mark {} offline: {e}", entry.runtime_id);
                    report
                        .errors
                        .push(format!("mark offline {}: {e}", entry.runtime_id));
                } else {
                    newly_offline.push(entry.runtime_id.clone());
                    report.runtimes_marked_offline += 1;
                }
            }
        }
    }
    trace!(
        "sweeper: phase 1 done — {} newly offline",
        newly_offline.len()
    );

    // -- Phase 2: orphan failure ---------------------------------------------
    if !newly_offline.is_empty() {
        let ids_ref: Vec<&str> = newly_offline.iter().map(String::as_str).collect();
        match registry.active_phases_for_runtimes(&ids_ref) {
            Ok(orphans) => {
                for (wf_id, ph_id, _agent) in &orphans {
                    match registry.fail_phase(wf_id, ph_id, "runtime went offline", now) {
                        Ok(()) => {
                            trace!(
                                workflow_id = %wf_id,
                                phase_id = %ph_id,
                                "sweeper: orphaned phase failed"
                            );
                            report.phases_orphan_failed += 1;
                        }
                        Err(e) => {
                            warn!("sweeper: failed to fail orphaned phase {wf_id}/{ph_id}: {e}");
                            report
                                .errors
                                .push(format!("orphan fail {wf_id}/{ph_id}: {e}"));
                        }
                    }
                }
            }
            Err(e) => {
                warn!("sweeper: failed to query orphaned phases: {e}");
                report.errors.push(format!("orphan query: {e}"));
            }
        }
    }
    trace!(
        "sweeper: phase 2 done — {} orphans failed",
        report.phases_orphan_failed
    );

    // -- Phase 3: reconciliation ---------------------------------------------
    match registry.reconcile_offline_phases(now) {
        Ok(count) => {
            report.phases_reconciled = count;
            trace!("sweeper: phase 3 done — {count} phases reconciled");
        }
        Err(e) => {
            warn!("sweeper: reconcile failed: {e}");
            report.errors.push(format!("reconcile: {e}"));
        }
    }

    // -- Phase 4: GC ---------------------------------------------------------
    match registry.gc_offline(now, gc_retention) {
        Ok(deleted) => {
            report.runtimes_gc_deleted = deleted;
            trace!("sweeper: phase 4 done — {deleted} runtime entries gc'd");
        }
        Err(e) => {
            warn!("sweeper: gc failed: {e}");
            report.errors.push(format!("gc: {e}"));
        }
    }

    report
}

/// Summary of a single sweep cycle.
#[derive(Debug, Default, Clone)]
pub struct SweepReport {
    /// Number of online runtimes transitioned to offline this sweep.
    pub runtimes_marked_offline: usize,
    /// Number of active phases failed due to orphaned (newly-offline) runtimes.
    pub phases_orphan_failed: usize,
    /// Number of active phases corrected during reconciliation.
    pub phases_reconciled: usize,
    /// Number of offline runtime entries removed by GC.
    pub runtimes_gc_deleted: usize,
    /// Non-fatal errors encountered during the sweep.
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Sweeper — background thread handle
// ---------------------------------------------------------------------------

/// Handle to the background sweep thread.
///
/// Drop this handle (or call [`Sweeper::stop`]) to request a clean shutdown.
/// The background thread will exit after completing any in-progress sweep.
pub struct Sweeper {
    stop_flag: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Sweeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sweeper")
            .field("stopped", &self.stop_flag.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

// Note: stop_flag uses Acquire/Release ordering:
// - Acquire on reads: ensures all prior stores become visible
// - Release on writes: ensures stores are visible to readers before signaling

impl Sweeper {
    /// Start the sweeper background thread, opening the runtime registry at
    /// `db_path` with the default constants.
    ///
    /// Returns `Err` if the registry cannot be opened.
    pub fn start(db_path: PathBuf) -> Result<Self, SweeperError> {
        Self::start_with(db_path, SWEEP_INTERVAL, HEARTBEAT_TIMEOUT, GC_RETENTION)
    }

    /// Start with explicit timing parameters (useful for tests).
    pub fn start_with(
        db_path: PathBuf,
        sweep_interval: Duration,
        heartbeat_timeout: Duration,
        gc_retention: Duration,
    ) -> Result<Self, SweeperError> {
        // Open the registry once to verify connectivity before spawning.
        let registry = RuntimeRegistry::open(&db_path)?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = Arc::clone(&stop_flag);

        let handle = std::thread::Builder::new()
            .name("hymenium-sweeper".to_string())
            .spawn(move || {
                // The registry was opened in this thread's context.
                // Re-bind so the borrow checker is satisfied.
                drop(registry); // close the check connection
                let registry = match RuntimeRegistry::open(&db_path) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!("sweeper thread: failed to open registry: {e}");
                        return;
                    }
                };

                while !stop_flag_clone.load(Ordering::Acquire) {
                    let report = run_sweep(&registry, heartbeat_timeout, gc_retention);
                    if !report.errors.is_empty() {
                        for err in &report.errors {
                            warn!("sweeper: {err}");
                        }
                    }
                    trace!(
                        offline = report.runtimes_marked_offline,
                        orphaned = report.phases_orphan_failed,
                        reconciled = report.phases_reconciled,
                        gc_deleted = report.runtimes_gc_deleted,
                        "sweeper cycle complete"
                    );

                    // Sleep in small increments so we can respond to the stop flag
                    // without waiting the full interval.
                    let step = Duration::from_millis(250);
                    let mut slept = Duration::ZERO;
                    while slept < sweep_interval && !stop_flag_clone.load(Ordering::Acquire) {
                        std::thread::sleep(step);
                        slept += step;
                    }
                }
                trace!("sweeper thread: stop requested, exiting");
            })
            .map_err(|e| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                    Some(format!("thread spawn failed: {e}")),
                )
            })?;

        Ok(Self {
            stop_flag,
            handle: Some(handle),
        })
    }

    /// Signal the sweeper to stop and wait for the thread to exit.
    pub fn stop(mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Sweeper {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_dt(s: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> RuntimeRegistry {
        RuntimeRegistry::open_in_memory().expect("in-memory registry")
    }

    // -----------------------------------------------------------------------
    // RuntimeStatus helpers
    // -----------------------------------------------------------------------

    #[test]
    fn runtime_status_round_trip() {
        assert_eq!(
            RuntimeStatus::from_str("online"),
            Some(RuntimeStatus::Online)
        );
        assert_eq!(
            RuntimeStatus::from_str("offline"),
            Some(RuntimeStatus::Offline)
        );
        assert_eq!(RuntimeStatus::from_str("garbage"), None);
        assert_eq!(RuntimeStatus::Online.as_str(), "online");
        assert_eq!(RuntimeStatus::Offline.as_str(), "offline");
    }

    // -----------------------------------------------------------------------
    // Heartbeat / upsert
    // -----------------------------------------------------------------------

    #[test]
    fn sweeper_upsert_heartbeat_creates_entry() {
        let reg = registry();
        let now = Utc::now();
        reg.upsert_heartbeat("runtime-1", now).expect("upsert");

        let all = reg.list_all().expect("list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].runtime_id, "runtime-1");
        assert_eq!(all[0].status, RuntimeStatus::Online);
    }

    #[test]
    fn sweeper_upsert_heartbeat_resets_offline_entry() {
        let reg = registry();
        let now = Utc::now();
        reg.upsert_heartbeat("runtime-1", now).expect("upsert");
        reg.mark_offline("runtime-1", now).expect("mark offline");

        // Verify it is offline.
        let offline = reg.list_offline().expect("list offline");
        assert_eq!(offline.len(), 1);

        // Now send a heartbeat — should flip back to online.
        reg.upsert_heartbeat("runtime-1", Utc::now())
            .expect("heartbeat");
        let all = reg.list_all().expect("list");
        assert_eq!(all[0].status, RuntimeStatus::Online);
        assert!(all[0].went_offline_at.is_none());
    }

    // -----------------------------------------------------------------------
    // Phase 1: heartbeat timeout
    // -----------------------------------------------------------------------

    #[test]
    fn sweeper_phase1_marks_stale_runtime_offline() {
        let reg = registry();
        let stale_ts = Utc::now() - chrono::Duration::seconds(60);
        reg.upsert_heartbeat("stale-runtime", stale_ts)
            .expect("upsert");

        let report = run_sweep(&reg, Duration::from_secs(45), GC_RETENTION);
        assert_eq!(report.runtimes_marked_offline, 1);

        let offline = reg.list_offline().expect("list offline");
        assert_eq!(offline.len(), 1);
        assert_eq!(offline[0].runtime_id, "stale-runtime");
    }

    #[test]
    fn sweeper_phase1_does_not_mark_fresh_runtime_offline() {
        let reg = registry();
        reg.upsert_heartbeat("fresh-runtime", Utc::now())
            .expect("upsert");

        let report = run_sweep(&reg, Duration::from_secs(45), GC_RETENTION);
        assert_eq!(report.runtimes_marked_offline, 0);

        let offline = reg.list_offline().expect("list");
        assert!(offline.is_empty());
    }

    // -----------------------------------------------------------------------
    // Phase 2: orphan failure
    // -----------------------------------------------------------------------

    #[test]
    fn sweeper_phase2_fails_orphaned_phases() {
        let reg = registry();

        // Insert a runtime that is stale.
        let stale_ts = Utc::now() - chrono::Duration::seconds(60);
        reg.upsert_heartbeat("offline-runtime", stale_ts)
            .expect("upsert");

        // Insert a fake active phase in the phase_states table (in the same db).
        reg.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS phase_states (
                    workflow_id    TEXT NOT NULL,
                    phase_id       TEXT NOT NULL,
                    role           TEXT NOT NULL DEFAULT 'implementer',
                    status         TEXT NOT NULL,
                    agent_id       TEXT,
                    started_at     TEXT,
                    completed_at   TEXT,
                    canopy_task_id TEXT,
                    retry_count    INTEGER NOT NULL DEFAULT 0,
                    phase_order    INTEGER NOT NULL DEFAULT 0,
                    failure_reason TEXT,
                    PRIMARY KEY (workflow_id, phase_id)
                );
                INSERT OR IGNORE INTO phase_states
                    (workflow_id, phase_id, status, agent_id, phase_order)
                VALUES ('wf-1', 'implement', 'active', 'offline-runtime', 0);",
            )
            .expect("setup phase_states");

        let report = run_sweep(&reg, Duration::from_secs(45), GC_RETENTION);
        assert_eq!(
            report.runtimes_marked_offline, 1,
            "runtime should go offline"
        );
        assert_eq!(
            report.phases_orphan_failed, 1,
            "orphaned phase should be failed"
        );

        // Verify the phase_states row was updated.
        let status: String = reg
            .conn
            .query_row(
                "SELECT status FROM phase_states WHERE workflow_id = 'wf-1' AND phase_id = 'implement'",
                [],
                |r| r.get(0),
            )
            .expect("query phase");
        assert_eq!(status, "failed");

        let reason: Option<String> = reg
            .conn
            .query_row(
                "SELECT failure_reason FROM phase_states WHERE workflow_id = 'wf-1' AND phase_id = 'implement'",
                [],
                |r| r.get(0),
            )
            .expect("query reason");
        assert_eq!(reason.as_deref(), Some("runtime went offline"));
    }

    // -----------------------------------------------------------------------
    // Phase 3: reconciliation
    // -----------------------------------------------------------------------

    #[test]
    fn sweeper_phase3_reconciles_pre_existing_orphans() {
        let reg = registry();

        // A runtime that was already offline before the sweeper started.
        let stale_ts = Utc::now() - chrono::Duration::seconds(120);
        reg.upsert_heartbeat("pre-offline", stale_ts)
            .expect("upsert");
        // Mark offline directly (bypass phase1 timeout check).
        reg.mark_offline("pre-offline", stale_ts + chrono::Duration::seconds(1))
            .expect("mark offline");

        // An active phase claimed by that offline runtime.
        reg.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS phase_states (
                    workflow_id    TEXT NOT NULL,
                    phase_id       TEXT NOT NULL,
                    role           TEXT NOT NULL DEFAULT 'implementer',
                    status         TEXT NOT NULL,
                    agent_id       TEXT,
                    started_at     TEXT,
                    completed_at   TEXT,
                    canopy_task_id TEXT,
                    retry_count    INTEGER NOT NULL DEFAULT 0,
                    phase_order    INTEGER NOT NULL DEFAULT 0,
                    failure_reason TEXT,
                    PRIMARY KEY (workflow_id, phase_id)
                );
                INSERT OR IGNORE INTO phase_states
                    (workflow_id, phase_id, status, agent_id, phase_order)
                VALUES ('wf-pre', 'audit', 'active', 'pre-offline', 0);",
            )
            .expect("setup");

        // Run sweep — phase1 won't touch it (already offline), but phase3 reconcile should.
        let report = run_sweep(&reg, Duration::from_secs(45), GC_RETENTION);
        assert!(
            report.phases_reconciled >= 1,
            "reconciler should fix pre-existing orphan"
        );

        let status: String = reg
            .conn
            .query_row(
                "SELECT status FROM phase_states WHERE workflow_id = 'wf-pre'",
                [],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(status, "failed");
    }

    // -----------------------------------------------------------------------
    // Phase 4: GC
    // -----------------------------------------------------------------------

    #[test]
    fn sweeper_phase4_gc_removes_old_offline_entries() {
        let reg = registry();

        // A runtime that went offline more than GC_RETENTION ago.
        let old_ts = Utc::now() - chrono::Duration::weeks(8);
        reg.upsert_heartbeat("old-dead", old_ts).expect("upsert");
        reg.mark_offline("old-dead", old_ts + chrono::Duration::seconds(1))
            .expect("mark offline");

        // A runtime that went offline recently — should survive GC.
        let recent_ts = Utc::now() - chrono::Duration::hours(1);
        reg.upsert_heartbeat("recent-offline", recent_ts)
            .expect("upsert");
        reg.mark_offline("recent-offline", recent_ts + chrono::Duration::seconds(1))
            .expect("mark offline");

        let report = run_sweep(&reg, Duration::from_secs(45), GC_RETENTION);
        assert_eq!(
            report.runtimes_gc_deleted, 1,
            "only old entry should be GC'd"
        );

        let remaining = reg.list_all().expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].runtime_id, "recent-offline");
    }

    #[test]
    fn sweeper_phase4_gc_does_not_remove_online_entries() {
        let reg = registry();

        // An online runtime (even with old registration).
        let old_ts = Utc::now() - chrono::Duration::weeks(10);
        reg.upsert_heartbeat("always-online", old_ts)
            .expect("upsert");
        // Simulate a recent heartbeat.
        reg.upsert_heartbeat("always-online", Utc::now())
            .expect("refresh heartbeat");

        let report = run_sweep(&reg, Duration::from_secs(45), GC_RETENTION);
        assert_eq!(
            report.runtimes_gc_deleted, 0,
            "online entries must not be GC'd"
        );
    }

    // -----------------------------------------------------------------------
    // Empty state — sweep must not panic
    // -----------------------------------------------------------------------

    #[test]
    fn sweeper_empty_state_completes_cleanly() {
        let reg = registry();
        let report = run_sweep(&reg, HEARTBEAT_TIMEOUT, GC_RETENTION);
        assert_eq!(report.runtimes_marked_offline, 0);
        assert_eq!(report.phases_orphan_failed, 0);
        assert_eq!(report.phases_reconciled, 0);
        assert_eq!(report.runtimes_gc_deleted, 0);
        assert!(report.errors.is_empty());
    }

    // -----------------------------------------------------------------------
    // Sweeper background thread starts and stops cleanly
    // -----------------------------------------------------------------------

    #[test]
    fn sweeper_thread_starts_and_stops() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let path = std::env::temp_dir().join(format!("hymenium_sweeper_test_{nanos}.db"));
        let sweeper = Sweeper::start_with(
            path.clone(),
            Duration::from_millis(200),
            Duration::from_secs(45),
            GC_RETENTION,
        )
        .expect("start sweeper");
        // Give the thread one tick.
        std::thread::sleep(Duration::from_millis(300));
        sweeper.stop();
        let _ = std::fs::remove_file(&path);
    }
}
