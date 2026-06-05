#[cfg(test)]
mod hook_integration_tests {
    use std::fs;
    use std::process::{self, Command};
    use std::sync::Mutex;

    static HOOK_TEST_MUTEX: Mutex<()> = Mutex::new(());

    struct LockGuard {
        path: std::path::PathBuf,
    }
    impl Drop for LockGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    #[test]
    fn test_hook_blocks_with_active_lock() {
        let _m = HOOK_TEST_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lock_path = std::env::temp_dir().join("hymenium-workflow-test-workflow.lock");
        let _cleanup = LockGuard {
            path: lock_path.clone(),
        };

        let _ = fs::remove_file(&lock_path);

        let lock_content = format!(
            r#"{{"pid":{},"workflow_id":"test-workflow","phase":"test-phase","started_at":"2025-01-01T00:00:00Z"}}"#,
            process::id()
        );
        fs::write(&lock_path, &lock_content).expect("failed to write lock");

        let output = Command::new("cargo")
            .args([
                "run",
                "--quiet",
                "--bin",
                "hymenium",
                "--",
                "hook",
                "pre-compact",
            ])
            .output()
            .expect("failed to run hook");

        let stdout = String::from_utf8_lossy(&output.stdout);

        #[cfg(unix)]
        assert!(
            stdout.contains("block"),
            "Hook should block when a workflow is active, got: '{stdout}'"
        );

        // On non-unix, is_pid_alive always returns false (fail-open), so the
        // lock is never treated as active and the hook allows compaction. Pin
        // that documented behavior at the integration level too.
        #[cfg(not(unix))]
        assert!(
            stdout.contains("allow"),
            "On non-unix the lock never blocks (fail-open); hook should allow, got: '{stdout}'"
        );
    }

    #[test]
    fn test_hook_allows_without_lock() {
        let _m = HOOK_TEST_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Remove any stale per-workflow lock files (new naming scheme: hymenium-workflow-*.lock).
        let tmp = std::env::temp_dir();
        if let Ok(entries) = fs::read_dir(&tmp) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("hymenium-workflow-") && name_str.ends_with(".lock") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }

        let output = Command::new("cargo")
            .args([
                "run",
                "--quiet",
                "--bin",
                "hymenium",
                "--",
                "hook",
                "pre-compact",
            ])
            .output()
            .expect("failed to run hook");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("allow"),
            "Hook should allow when no workflow is active, got: '{stdout}'"
        );
    }
}
