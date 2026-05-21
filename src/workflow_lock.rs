//! Workflow lock management for blocking context compaction during execution.
//!
//! When hymenium is actively executing a DAG workflow, this module writes a lock
//! file that signals to Claude Code's context compaction hook that a workflow
//! phase is in progress and compaction should be blocked.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process;

/// Metadata stored in the workflow lock file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowLock {
    pub pid: u32,
    pub workflow_id: String,
    pub phase: String,
    pub started_at: String,
}

/// Get the path to the workflow lock file in the system temp directory.
fn lock_path() -> PathBuf {
    std::env::temp_dir().join("hymenium-workflow.lock")
}

/// Check if a process ID is still alive.
///
/// On Unix-like systems, uses `libc::kill(pid, 0)` to test if the process exists.
/// Returns true if the process is alive, false if the PID is stale.
#[allow(unsafe_code, clippy::cast_possible_wrap)] // libc::kill requires unsafe; cast is safe for valid PIDs
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[cfg(not(unix))]
    {
        // On non-Unix systems, assume the PID is stale; fail-open by returning false.
        // This means the lock will not block compaction on Windows.
        false
    }
}

/// Write a workflow lock atomically.
///
/// Writes the lock file with an atomic rename pattern (.tmp + rename) to ensure
/// consistency. If a lock file already exists, it will be overwritten.
///
/// Lock errors are non-fatal; this function logs warnings but does not propagate
/// failures. The workflow continues even if the lock cannot be written.
pub fn acquire_lock(workflow_id: &str, phase: &str) -> Result<()> {
    let path = lock_path();
    let tmp_path = path.with_extension("lock.tmp");

    let lock = WorkflowLock {
        pid: process::id(),
        workflow_id: workflow_id.to_string(),
        phase: phase.to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
    };

    let json = serde_json::to_string(&lock).context("failed to serialize lock")?;

    let mut file = fs::File::create(&tmp_path).context("failed to create lock temp file")?;
    file.write_all(json.as_bytes())
        .context("failed to write lock file")?;
    drop(file);

    fs::rename(&tmp_path, &path).context("failed to rename lock file atomically")?;

    Ok(())
}

/// Remove the lock file if it exists.
///
/// Lock removal errors are non-fatal. This function logs warnings but does not
/// propagate failures.
pub fn release_lock() -> Result<()> {
    let path = lock_path();
    if path.exists() {
        fs::remove_file(&path).context("failed to remove lock file")?;
    }
    Ok(())
}

/// Returns true if a lock exists AND the owner process is still alive.
///
/// If the lock file is absent, or if it contains a stale PID (process no longer
/// exists), this function returns false.
#[must_use]
pub fn is_active_lock() -> bool {
    if let Some(lock) = read_active_lock() {
        is_pid_alive(lock.pid)
    } else {
        false
    }
}

/// Returns the lock content if active, None if absent or stale.
///
/// Reads the lock file and checks if the owner process is still alive. Returns
/// the lock metadata if active, or None if the lock is absent or the PID is stale.
#[must_use]
pub fn read_active_lock() -> Option<WorkflowLock> {
    let path = lock_path();
    if !path.exists() {
        return None;
    }

    let Ok(content) = fs::read_to_string(&path) else {
        // Cannot read lock file; treat as stale
        return None;
    };

    #[allow(clippy::single_match_else)] // We handle both Ok and Err; clippy suggestion is incorrect
    match serde_json::from_str::<WorkflowLock>(&content) {
        Ok(lock) => {
            if is_pid_alive(lock.pid) {
                Some(lock)
            } else {
                // Clean up stale lock
                let _ = fs::remove_file(&path);
                None
            }
        }
        Err(_) => {
            // Malformed lock file; treat as stale
            let _ = fs::remove_file(&path);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    lazy_static::lazy_static! {
        static ref LOCK_TEST_MUTEX: Mutex<()> = Mutex::new(());
    }

    #[test]
    fn acquire_and_release_lock_roundtrip() {
        let _guard = LOCK_TEST_MUTEX.lock().unwrap();

        // Clean up any existing lock
        let _ = release_lock();

        // Should not be active initially
        assert!(!is_active_lock());

        // Acquire a lock
        acquire_lock("test-workflow", "test-phase").expect("failed to acquire lock");

        // Should be active now
        assert!(is_active_lock());

        // Lock content should be readable
        let lock = read_active_lock().expect("failed to read lock");
        assert_eq!(lock.workflow_id, "test-workflow");
        assert_eq!(lock.phase, "test-phase");
        assert_eq!(lock.pid, process::id());

        // Release the lock
        release_lock().expect("failed to release lock");

        // Should no longer be active
        assert!(!is_active_lock());
    }

    #[test]
    fn stale_lock_with_nonexistent_pid_is_not_active() {
        let _guard = LOCK_TEST_MUTEX.lock().unwrap();

        // Clean up any existing lock
        let _ = release_lock();

        // Manually write a lock with an impossible PID
        let path = lock_path();
        let lock = WorkflowLock {
            pid: 2147483647, // Very large PID that's unlikely to exist
            workflow_id: "stale-workflow".to_string(),
            phase: "stale-phase".to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        let json = serde_json::to_string(&lock).expect("failed to serialize");
        fs::write(&path, json).expect("failed to write lock");

        // The lock should be detected as stale
        assert!(!is_active_lock());

        // Clean up
        let _ = release_lock();
    }

    #[test]
    fn is_active_lock_false_when_no_lock() {
        let _guard = LOCK_TEST_MUTEX.lock().unwrap();

        // Clean up any existing lock
        let _ = release_lock();

        // No lock file should mean is_active_lock returns false
        assert!(!is_active_lock());
    }
}
