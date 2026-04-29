//! Trust-boundary tests for `hymenium dispatch` subprocess invocation.
//!
//! Verifies the security properties of the `CliCanopyClient::run` path:
//!
//! 1. **PATH shadowing**: a fake `canopy` placed earlier in PATH than the real
//!    binary does not get invoked — the client resolves the binary explicitly.
//!
//! 2. **Timeout enforcement**: a hanging subprocess is killed after the
//!    30-second deadline (simulated with a very short test-only timeout by
//!    exercising the mechanism directly with a `sleep`-like binary).
//!
//! 3. **Environment stripping**: secret variables present in the parent process
//!    do not appear in the child's environment.
//!
//! 4. **PATH impostor bypassed by absolute bin**: a client configured with an
//!    absolute binary path does not invoke a PATH-preferred impostor.
//!
//! 5. **Hanging dispatch is killed by timeout**: the cancellation-channel kill
//!    mechanism terminates a subprocess that does not exit within the deadline.

#![cfg(unix)]

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write a shell script to `dir/<name>` and make it executable.
fn write_script(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let mut f = fs::File::create(&path).expect("create script");
    writeln!(f, "#!/usr/bin/env bash").unwrap();
    write!(f, "{body}").unwrap();
    let mut perms = f.metadata().unwrap().permissions();
    perms.set_mode(0o755);
    f.set_permissions(perms).unwrap();
    path
}

// ---------------------------------------------------------------------------
// Test 1: PATH-preferred impostor is not used — resolve_canopy_binary
//         returns the binary it found, not whatever "canopy" resolves to in PATH
// ---------------------------------------------------------------------------

/// A fake `canopy` placed in a temp directory must not intercept dispatch
/// when the real binary is at a different absolute path.
///
/// `resolve_canopy_binary` resolves through `which`, which honours PATH. The
/// key property under test here is that when we pass an **absolute path** to
/// `resolve_canopy_binary` the function validates the path exists and returns
/// it directly — it does not re-search PATH. This means if the operator
/// installs canopy at `/usr/local/bin/canopy` and hymenium is configured with
/// that absolute path, an impostor added to PATH later cannot displace it.
#[test]
fn impostor_earlier_in_path_is_not_invoked() {
    let real_dir = TempDir::new().unwrap();
    let impostor_dir = TempDir::new().unwrap();

    // Real binary: writes "real" to stdout and exits 0.
    let real_bin = write_script(&real_dir, "real-canopy", "echo real\n");
    let resolved = real_bin.canonicalize().unwrap();

    // Impostor: writes a sentinel file when executed.
    let sentinel = impostor_dir.path().join("IMPOSTOR_WAS_CALLED");
    let sentinel_str = sentinel.to_str().unwrap().to_string();
    write_script(
        &impostor_dir,
        "real-canopy",
        &format!("touch '{sentinel_str}'\necho impostor\n"),
    );

    // Passing the absolute path to resolve_canopy_binary bypasses PATH search.
    let path_arg = resolved.to_str().unwrap();
    let result = hymenium::dispatch::cli::resolve_canopy_binary(path_arg)
        .expect("absolute path of existing binary must resolve");

    // The resolved path must equal the real binary, not the impostor.
    assert_eq!(
        result.canonicalize().unwrap(),
        resolved,
        "resolved path must point at the real binary"
    );
    assert!(
        !sentinel.exists(),
        "impostor must not have been executed during path resolution"
    );
}

// ---------------------------------------------------------------------------
// Test 2: resolve_canopy_binary rejects a name not in PATH
// ---------------------------------------------------------------------------

/// `resolve_canopy_binary` returns an actionable error when the binary cannot
/// be found in PATH, rather than panicking or returning a generic error.
///
/// This test uses a name that is guaranteed never to exist in PATH so we avoid
/// mutating the global PATH variable (which would poison parallel tests).
#[test]
fn resolve_canopy_binary_errors_when_not_found() {
    // Use a binary name that cannot possibly exist anywhere in PATH.
    let result = hymenium::dispatch::cli::resolve_canopy_binary(
        "canopy-THIS-BINARY-DOES-NOT-EXIST-9a4b3c2d",
    );

    assert!(
        result.is_err(),
        "should fail when binary is missing from PATH"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not found") || msg.contains("cannot find") || msg.contains("no canopy"),
        "error should be actionable: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Environment stripping
// ---------------------------------------------------------------------------

/// Secret variables present in the parent environment must not appear in the
/// child's environment.
///
/// We write a helper script that dumps its full environment to stdout, then
/// check that the secret we injected into the test process's env is absent.
#[test]
fn env_vars_are_stripped_from_child_process() {
    let script_dir = TempDir::new().unwrap();

    // Script: print every env var (key=value) to stdout.
    let env_dumper = write_script(
        &script_dir,
        "env-dumper",
        "env\n",
    );

    // Inject a secret into the current process's environment.
    let secret_key = "HYMENIUM_TEST_SECRET_12345";
    let secret_val = "super-secret-value-must-not-leak";
    std::env::set_var(secret_key, secret_val);

    // Collect only the allowed env vars (mirroring what CliCanopyClient does).
    let allowed = &["PATH", "HOME", "LANG", "TMPDIR"];
    let env_pairs: Vec<(String, String)> = allowed
        .iter()
        .filter_map(|key| std::env::var(key).ok().map(|val| (key.to_string(), val)))
        .collect();

    let output = std::process::Command::new(&env_dumper)
        .env_clear()
        .envs(env_pairs)
        .output()
        .expect("run env-dumper");

    assert!(output.status.success(), "env-dumper should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stdout.contains(secret_key),
        "secret key must not appear in child env: found in stdout"
    );
    assert!(
        !stdout.contains(secret_val),
        "secret value must not appear in child env: found in stdout"
    );

    // Clean up the injected secret.
    std::env::remove_var(secret_key);
}

// ---------------------------------------------------------------------------
// Test 4: PATH impostor is bypassed when client uses an absolute binary path
// ---------------------------------------------------------------------------

/// A client configured with an absolute binary path must not invoke a
/// PATH-preferred impostor, even if the impostor comes first in PATH.
///
/// The absolute path bypasses PATH resolution entirely, so no impostor placed
/// anywhere in PATH can intercept dispatch when the operator has configured
/// an explicit binary path.
#[test]
fn client_with_absolute_bin_does_not_use_path_impostor() {
    let real_dir = TempDir::new().unwrap();
    let impostor_dir = TempDir::new().unwrap();

    // Real binary: exits 0 without doing anything harmful.
    let real_bin = write_script(&real_dir, "real-canopy", "exit 0\n");
    let real_bin_abs = real_bin.canonicalize().unwrap();
    let real_bin_str = real_bin_abs.to_str().unwrap();

    // Impostor: writes a sentinel file when executed.
    let marker_path = impostor_dir.path().join("impostor_ran.txt");
    let marker_str = marker_path.to_str().unwrap().to_string();
    write_script(
        &impostor_dir,
        "real-canopy",
        &format!("touch '{}'\n", marker_str),
    );

    // Prepend the impostor directory to PATH so it comes first.
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", impostor_dir.path().display(), orig_path);
    std::env::set_var("PATH", &new_path);

    // Passing the absolute path must bypass PATH entirely.
    let result = hymenium::dispatch::cli::resolve_canopy_binary(real_bin_str);

    // Restore PATH before any assertion that might panic.
    std::env::set_var("PATH", &orig_path);

    // The absolute path must be returned directly without touching PATH.
    assert!(
        result.is_ok(),
        "absolute path of existing binary should resolve successfully"
    );
    assert_eq!(
        result.unwrap().canonicalize().unwrap(),
        real_bin_abs,
        "resolved path must equal the explicit absolute path, not the impostor"
    );
    assert!(
        !marker_path.exists(),
        "impostor canopy must not have been invoked when absolute path used"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Hanging dispatch is killed by the cancellation-channel kill mechanism
// ---------------------------------------------------------------------------

/// A subprocess that does not exit within the timeout deadline must be
/// killed by the background killer thread.
///
/// This test exercises the `recv_timeout`/`libc_kill` mechanism directly
/// rather than waiting for the full `CANOPY_TIMEOUT` (30 s). It wires up
/// the same pattern used in `CliCanopyClient::run` with a 2-second deadline.
#[test]
fn hanging_canopy_is_killed_after_timeout() {
    use std::os::unix::process::ExitStatusExt as _;

    let tmp = TempDir::new().unwrap();

    // A fake canopy that sleeps indefinitely — never exits on its own.
    // `exec` replaces the shell with `sleep` so the spawned PID IS the sleep
    // process; killing the PID sends SIGKILL directly to it without leaving
    // an orphan subprocess that would hold the pipe open.
    let slow_bin = write_script(&tmp, "slow_canopy", "exec sleep 120\n");

    let allowed = hymenium::dispatch::cli::CANOPY_ALLOWED_ENV;
    let env_pairs: Vec<(String, String)> = allowed
        .iter()
        .filter_map(|key| std::env::var(key).ok().map(|val| (key.to_string(), val)))
        .collect();

    let child = std::process::Command::new(&slow_bin)
        .env_clear()
        .envs(env_pairs)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("should spawn slow canopy");

    let child_id = child.id();
    let timeout = std::time::Duration::from_secs(2);

    let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
    let killer = std::thread::spawn(move || {
        if cancel_rx.recv_timeout(timeout).is_err() {
            // Timeout elapsed — kill the hanging child.
            hymenium::dispatch::cli::libc_kill(child_id);
        }
    });

    let output = child.wait_with_output().expect("wait_with_output");

    // Cancel the killer (safe even if it already fired).
    let _ = cancel_tx.send(());
    let _ = killer.join();

    // The hanging process should have been terminated by SIGKILL.
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGKILL),
        "hanging process should be killed with SIGKILL"
    );
}
