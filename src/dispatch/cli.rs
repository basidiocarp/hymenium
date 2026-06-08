use super::{
    CanopyClient, CompletenessReport, DispatchError, ImportResult, TaskDetail, TaskOptions,
};
use crate::workflow::template::AgentRole;
#[cfg(unix)]
use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Wire mirror types for canopy-task-detail-v1
// ---------------------------------------------------------------------------

/// Deserialize-only mirror of the nested `task` object in `canopy-task-detail-v1`.
///
/// Field names match canopy's wire format exactly. The flat `TaskDetail` struct
/// uses different names (`agent_id`, `parent_id`) — `map_wire_to_detail` handles
/// the renaming so both the socket and CLI-fallback paths share the same mapping.
#[derive(serde::Deserialize)]
struct TaskWireMirror {
    task_id: String,
    title: String,
    status: String,
    #[serde(default)]
    owner_agent_id: Option<String>,
    #[serde(default)]
    parent_task_id: Option<String>,
    #[serde(default)]
    required_capabilities: Vec<String>,
    #[serde(default)]
    has_code_diff: bool,
    #[serde(default)]
    has_verification_passed: bool,
}

/// Deserialize-only mirror of the top-level `canopy-task-detail-v1` payload.
///
/// Only the fields hymenium reads are present. Unknown fields are ignored via
/// the default serde behaviour so fixture additions do not break parsing.
#[derive(serde::Deserialize)]
struct TaskDetailWireMirror {
    task: TaskWireMirror,
    #[serde(default)]
    completion_signal: Option<serde_json::Value>,
}

/// Map the nested wire mirror into the flat `TaskDetail` struct used internally.
///
/// Centralised here so the socket path and the CLI-fallback path produce identical
/// output rather than each having its own field mapping.
fn map_wire_to_detail(wire: TaskDetailWireMirror) -> TaskDetail {
    TaskDetail {
        task_id: wire.task.task_id,
        title: wire.task.title,
        status: wire.task.status,
        agent_id: wire.task.owner_agent_id,
        parent_id: wire.task.parent_task_id,
        required_capabilities: wire.task.required_capabilities,
        has_code_diff: wire.task.has_code_diff,
        has_verification_passed: wire.task.has_verification_passed,
        completion_signal: wire.completion_signal.and_then(|v| {
            if v.is_null() {
                None
            } else {
                serde_json::from_value(v).ok()
            }
        }),
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC envelope shapes from canopy's socket server
// ---------------------------------------------------------------------------

/// Deserialize-only mirror of canopy's JSON-RPC 2.0 response envelope.
///
/// Canopy's socket server (`socket_server.rs`) rewrites handler errors into a
/// top-level JSON-RPC error object: `{"jsonrpc":"2.0","id":..,"error":{"code",-32000,"message":"..."}}`.
/// Successful responses carry `{"jsonrpc":"2.0","id":..,"result":<payload>}`.
/// The two fields are mutually exclusive in practice, but both are `Option` so
/// that a partial or unexpected envelope does not cause a hard deserialize
/// failure — the parser below handles the missing-field cases explicitly.
#[cfg(unix)]
#[derive(serde::Deserialize)]
struct CanopyRpcResult {
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Socket path resolution
// ---------------------------------------------------------------------------

/// Resolve the unix-socket path for the canopy JSON-RPC server.
///
/// Resolution order:
/// 1. `HYMENIUM_CANOPY_SOCKET` env var (explicit override)
/// 2. Canopy descriptor file: `<config_dir("canopy")>/canopy.endpoint.json` → `.endpoint`
/// 3. Default: `<data_dir("basidiocarp")>/canopy/canopy.sock`
///
/// Returns `None` on any resolution failure (missing env, unreadable descriptor,
/// or platform path unavailable). The caller falls back to the CLI path when
/// `None` is returned.
#[cfg(unix)]
fn resolve_canopy_socket() -> Option<PathBuf> {
    // 1. Explicit env override.
    if let Ok(p) = std::env::var("HYMENIUM_CANOPY_SOCKET") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }

    // 2. Canopy descriptor file.
    if let Ok(config_dir) = spore::paths::config_dir("canopy") {
        let descriptor = config_dir.join("canopy.endpoint.json");
        if let Ok(content) = std::fs::read_to_string(&descriptor) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(endpoint) = v.get("endpoint").and_then(serde_json::Value::as_str) {
                    if !endpoint.is_empty() {
                        return Some(PathBuf::from(endpoint));
                    }
                }
            }
        }
    }

    // 3. Default socket path.
    if let Ok(data_dir) = spore::paths::data_dir("basidiocarp") {
        return Some(data_dir.join("canopy").join("canopy.sock"));
    }

    None
}

// ---------------------------------------------------------------------------
// Socket JSON-RPC client
// ---------------------------------------------------------------------------

/// Send a `canopy_task` JSON-RPC request over a unix domain socket and parse
/// the `TaskDetailWireMirror` from the response.
///
/// Writes a single newline-delimited request, reads one newline-delimited
/// response, and handles canopy's JSON-RPC 2.0 error envelope where
/// task-not-found is returned as a top-level `error` object
/// (`{"jsonrpc":"2.0","id":..,"error":{"code":-32000,"message":"..."}}`)
/// rather than a value inside `result`.
///
/// Returns `Err(DispatchError::CanopyError)` on any I/O, framing, or
/// application-level error so the caller can fall back to the CLI path.
#[cfg(unix)]
fn socket_get_task(
    socket_path: &std::path::Path,
    task_id: &str,
) -> Result<TaskDetailWireMirror, DispatchError> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path).map_err(|e| {
        DispatchError::CanopyError(format!(
            "canopy socket connect failed ({}): {e}",
            socket_path.display()
        ))
    })?;

    // Set read and write timeouts matching the global canopy timeout so the
    // socket path cannot block longer than the CLI path would in either
    // direction.
    let timeout = canopy_timeout();
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| DispatchError::CanopyError(format!("canopy socket set_read_timeout: {e}")))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| DispatchError::CanopyError(format!("canopy socket set_write_timeout: {e}")))?;

    // Build the request via serde_json so task_id values containing `"`, `\`,
    // or newlines cannot produce malformed JSON or inject a second framing line.
    let request_value = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "canopy_task",
        "params": { "task_id": task_id }
    });
    let request = serde_json::to_string(&request_value)
        .map_err(|e| DispatchError::CanopyError(format!("canopy socket request serialize: {e}")))?
        + "\n";
    stream
        .write_all(request.as_bytes())
        .map_err(|e| DispatchError::CanopyError(format!("canopy socket write: {e}")))?;
    stream
        .flush()
        .map_err(|e| DispatchError::CanopyError(format!("canopy socket flush: {e}")))?;

    let mut response_line = String::new();
    let mut reader = std::io::BufReader::new(&stream);
    reader
        .read_line(&mut response_line)
        .map_err(|e| DispatchError::CanopyError(format!("canopy socket read: {e}")))?;

    parse_canopy_rpc_response(&response_line)
}

/// Parse a raw JSON-RPC response line from canopy into a `TaskDetailWireMirror`.
///
/// Canopy's socket server uses standard JSON-RPC 2.0 error envelopes for
/// application-level failures such as task-not-found:
///
/// ```json
/// {"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"task detail: not found"}}
/// ```
///
/// Successful responses carry the task payload in `result`:
///
/// ```json
/// {"jsonrpc":"2.0","id":1,"result":{<TaskDetailWire payload>}}
/// ```
///
/// Resolution order:
/// 1. Top-level `error` present → surface `error.message` as `DispatchError::CanopyError`.
///    This is the primary path for task-not-found and other handler errors.
/// 2. Top-level `result` present:
///    a. Check for a nested `result.error` string as a defensive path (canopy does not emit this shape, but the check is harmless).
///    b. Deserialize `result` as `TaskDetailWireMirror`.
/// 3. Neither field present → parse error.
#[cfg(unix)]
fn parse_canopy_rpc_response(line: &str) -> Result<TaskDetailWireMirror, DispatchError> {
    let rpc: CanopyRpcResult = serde_json::from_str(line.trim())
        .map_err(|e| DispatchError::CanopyError(format!("canopy socket response parse: {e}")))?;

    // Primary path: top-level JSON-RPC error envelope (real canopy not-found shape).
    if let Some(err_obj) = &rpc.error {
        let message = err_obj
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| err_obj.to_string(), ToString::to_string);
        return Err(DispatchError::CanopyError(format!("canopy: {message}")));
    }

    let result = rpc.result.ok_or_else(|| {
        DispatchError::CanopyError(
            "canopy socket response missing both 'result' and 'error' fields".to_string(),
        )
    })?;

    // Secondary/defensive path: nested result.error string.
    // Canopy does not emit this shape, but the check costs nothing and guards
    // against future protocol surprises.
    if let Some(err_str) = result.get("error").and_then(serde_json::Value::as_str) {
        return Err(DispatchError::CanopyError(format!("canopy: {err_str}")));
    }

    serde_json::from_value::<TaskDetailWireMirror>(result)
        .map_err(|e| DispatchError::CanopyError(format!("canopy task detail deserialize: {e}")))
}

/// Parse a `TaskDetailWireMirror` from a JSON string (used for the CLI-fallback
/// path where the raw stdout from `canopy api task --task-id` is passed in).
///
/// The CLI stdout is the same nested `TaskDetailWireMirror` shape as the socket
/// result payload, so the same struct and mapping apply.
fn parse_task_detail_from_json(json: &str) -> Result<TaskDetailWireMirror, DispatchError> {
    // Check for an inline error field at the top level (mirrors the socket
    // error-in-result encoding for CLI consumers).
    let value: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| DispatchError::CanopyError(format!("failed to parse task detail: {e}")))?;

    if let Some(err_str) = value.get("error").and_then(serde_json::Value::as_str) {
        return Err(DispatchError::CanopyError(format!("canopy: {err_str}")));
    }

    serde_json::from_value::<TaskDetailWireMirror>(value)
        .map_err(|e| DispatchError::CanopyError(format!("failed to parse task detail: {e}")))
}

// ---------------------------------------------------------------------------
// CliCanopyClient
// ---------------------------------------------------------------------------

pub(crate) fn canopy_timeout() -> Duration {
    std::env::var("HYMENIUM_CANOPY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(Duration::from_secs(30), Duration::from_secs)
}

/// Environment variables forwarded to canopy subprocesses.
///
/// All other variables are stripped to prevent secret leakage and PATH
/// hijacking by environment-injected values the orchestrator did not set.
///
/// # Note
///
/// This is public so integration tests can mirror the environment-stripping
/// behaviour exactly. It is not part of the stable public API.
pub const CANOPY_ALLOWED_ENV: &[&str] = &["PATH", "HOME", "LANG", "TMPDIR"];

/// Resolve the absolute path to the `canopy` binary.
///
/// Uses the `which` crate to search PATH. Returns an actionable error if the
/// binary cannot be found so the caller can surface a clear diagnosis.
///
/// # Note
///
/// This is public so integration tests can verify the resolution contract
/// directly. It is not part of the stable public API.
pub fn resolve_canopy_binary(name: &str) -> Result<PathBuf, DispatchError> {
    // If the caller passed an absolute path, validate it exists and use it
    // directly without a PATH search.
    let p = std::path::Path::new(name);
    if p.is_absolute() {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
        return Err(DispatchError::CanopyError(format!(
            "canopy binary not found at explicit path: {name}"
        )));
    }

    which::which(name).map_err(|_| {
        DispatchError::CanopyError(format!(
            "canopy binary not found in PATH; install canopy or set an explicit path (searched for '{name}')"
        ))
    })
}

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
    ///
    /// Security properties enforced here:
    /// - The binary path is resolved explicitly via `resolve_canopy_binary` so
    ///   that a PATH-preferred impostor cannot intercept dispatch payloads.
    /// - The child environment is cleared and only the allowlisted variables
    ///   are restored, preventing secret leakage.
    /// - A configurable wall-clock timeout (default 30s, override via `HYMENIUM_CANOPY_TIMEOUT_SECS`) kills the child and waits for it to
    ///   exit so a hanging canopy process cannot block orchestration
    ///   indefinitely.
    fn run(&self, args: &[&str]) -> Result<String, DispatchError> {
        let bin = resolve_canopy_binary(&self.canopy_bin)?;

        // Collect the allowed env values before spawning so the closure does
        // not borrow across the spawn boundary.
        let env_pairs: Vec<(String, String)> = CANOPY_ALLOWED_ENV
            .iter()
            .filter_map(|key| std::env::var(key).ok().map(|val| (key.to_string(), val)))
            .collect();

        let child = std::process::Command::new(&bin)
            .args(args)
            .env_clear()
            .envs(env_pairs)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| DispatchError::CanopyError(format!("failed to spawn canopy: {e}")))?;

        // Enforce a wall-clock timeout by having a background thread kill the
        // child if it does not finish within the configured timeout.
        //
        // A cancellation channel lets the main thread signal the killer before
        // it fires, preventing a PID-reuse race: after wait_with_output() the
        // child PID is freed and could be reused by an unrelated process.
        let timeout = canopy_timeout();
        // SAFETY: `child.id()` returns the OS PID; we use it only to send a
        // signal, which is safe from any thread.
        let child_id = child.id();
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
        let killer = std::thread::spawn(move || {
            // recv_timeout returns Err(Timeout) if the deadline elapsed without
            // a cancellation, or Ok(()) if the main thread sent the signal.
            if cancel_rx.recv_timeout(timeout).is_err() {
                // Timeout elapsed and no cancellation received — kill the child.
                #[cfg(unix)]
                libc_kill(child_id);
                #[cfg(not(unix))]
                let _ = child_id;
            }
            // If Ok(()) was received, the child already exited — do nothing.
        });

        let output = child
            .wait_with_output()
            .map_err(|e| DispatchError::CanopyError(format!("canopy dispatch failed: {e}")))?;

        // Cancel the killer before it fires (safe even if it already ran).
        let _ = cancel_tx.send(());
        let _ = killer.join();

        // Distinguish timeout from a normal non-zero exit.
        if !output.status.success() {
            // On Unix, SIGKILL produces signal status rather than a normal
            // exit code. Treat that as a timeout.
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt as _;
                if output.status.signal() == Some(libc::SIGKILL) {
                    return Err(DispatchError::CanopyError(
                        "canopy dispatch timed out".to_string(),
                    ));
                }
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DispatchError::CanopyError(stderr.trim().to_string()));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// Send SIGKILL to a process by PID on Unix systems.
///
/// Called from the timeout thread. Best-effort: if the process has already
/// exited, the kill call is harmless.
///
/// Centralises the single `unsafe` block so callers remain safe code.
///
/// # Note
///
/// This is public so integration tests can exercise the timeout kill mechanism
/// directly. It is not part of the stable public API.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn libc_kill(pid: u32) {
    // SAFETY: kill(2) is always safe to call; sending SIGKILL to an
    // already-exited process returns ESRCH which we ignore.
    //
    // PIDs on Unix are always positive and within the i32 range, so the cast
    // is safe. POSIX guarantees PID_MAX <= 2^22 on Linux and <= 99999 on macOS.
    #[allow(clippy::cast_possible_wrap)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
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
        // Try the unix-socket JSON-RPC path first (lower latency, no subprocess).
        // Any socket failure — env missing, descriptor absent, connect refused,
        // parse error — falls through to the CLI fallback.
        #[cfg(unix)]
        if let Some(socket_path) = resolve_canopy_socket() {
            match socket_get_task(&socket_path, task_id) {
                Ok(wire) => return Ok(map_wire_to_detail(wire)),
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        socket = %socket_path.display(),
                        "canopy socket get_task failed; falling back to CLI"
                    );
                }
            }
        }

        // CLI fallback: `canopy api task --task-id <id>`.
        // Parses the same nested TaskDetailWireMirror shape as the socket path.
        let json = self.run(&["api", "task", "--task-id", task_id])?;
        let wire = parse_task_detail_from_json(&json)?;
        Ok(map_wire_to_detail(wire))
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

    fn cancel_task(&self, task_id: &str) -> Result<(), DispatchError> {
        self.run(&["task", "cancel", task_id, "--cancelled-by", "hymenium"])?;
        Ok(())
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

    // -- completion_signal wire-parse contract ----------------------------------
    //
    // map_wire_to_detail owns the only conversion from the opaque wire
    // `completion_signal` Value into the typed Option<CompletionSignal>. These
    // tests lock that contract: JSON null and parse failure both collapse to
    // None (forward-compatible), a valid object round-trips, and the
    // "absent should_continue -> stop" semantic is preserved through the wire.

    fn wire_with_signal(signal_json: &str) -> TaskDetailWireMirror {
        let envelope = format!(
            r#"{{"task":{{"task_id":"01TASK","title":"t","status":"completed"}},"completion_signal":{signal_json}}}"#
        );
        serde_json::from_str(&envelope).expect("wire envelope should deserialize")
    }

    #[test]
    fn map_wire_to_detail_maps_json_null_signal_to_none() {
        let detail = map_wire_to_detail(wire_with_signal("null"));
        assert!(
            detail.completion_signal.is_none(),
            "explicit JSON null must map to None, not Some(empty)"
        );
    }

    #[test]
    fn map_wire_to_detail_maps_absent_signal_to_none() {
        // No completion_signal key at all -> serde default -> None.
        let wire: TaskDetailWireMirror = serde_json::from_str(
            r#"{"task":{"task_id":"01TASK","title":"t","status":"completed"}}"#,
        )
        .expect("wire without signal should deserialize");
        let detail = map_wire_to_detail(wire);
        assert!(detail.completion_signal.is_none());
    }

    #[test]
    fn map_wire_to_detail_maps_malformed_signal_to_none() {
        // should_continue is the wrong type; from_value fails and we fall back
        // to None rather than erroring the whole task-detail parse.
        let detail = map_wire_to_detail(wire_with_signal(r#"{"should_continue":"yes"}"#));
        assert!(
            detail.completion_signal.is_none(),
            "a malformed signal must degrade to None, never abort the read"
        );
    }

    #[test]
    fn map_wire_to_detail_parses_should_continue_false_as_wants_stop() {
        let detail = map_wire_to_detail(wire_with_signal(r#"{"should_continue":false}"#));
        let signal = detail
            .completion_signal
            .expect("a present object signal must parse to Some");
        assert!(signal.wants_stop(), "should_continue=false -> wants_stop");
    }

    #[test]
    fn map_wire_to_detail_parses_should_continue_true_as_no_stop() {
        let detail = map_wire_to_detail(wire_with_signal(
            r#"{"should_continue":true,"next_action":{"directive":"ship it"}}"#,
        ));
        let signal = detail
            .completion_signal
            .expect("a present object signal must parse to Some");
        assert!(
            !signal.wants_stop(),
            "should_continue=true -> workflow continues"
        );
    }

    #[test]
    fn map_wire_to_detail_empty_signal_object_wants_stop() {
        // A present-but-empty object means should_continue is absent, which the
        // schema defines as stop. This mirrors CompletionSignal::wants_stop.
        let detail = map_wire_to_detail(wire_with_signal("{}"));
        let signal = detail
            .completion_signal
            .expect("an empty object is still a present signal");
        assert!(
            signal.wants_stop(),
            "absent should_continue (empty object) -> stop"
        );
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

    // -- round-trip test against the real septa fixture ---------------------------

    /// Deserialize the canonical septa `canopy-task-detail-v1` fixture into
    /// `TaskDetailWireMirror`, map it to `TaskDetail`, and assert the expected
    /// field values.
    ///
    /// This test exercises the full parse-and-map path without any mocks, proving
    /// that the wire mirror and mapping function are compatible with the
    /// authoritative contract fixture.
    #[test]
    fn septa_fixture_round_trip_parses_and_maps_to_task_detail() {
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../septa/fixtures/canopy-task-detail-v1.example.json");

        // The septa fixture lives in a sibling repo that is NOT checked out in
        // hymenium's single-repo CI. Skip the round-trip when it is absent — the
        // workspace integration suite covers the cross-repo contract. Locally
        // (septa present as a sibling) this still exercises the authoritative
        // contract fixture without mocks.
        if !fixture_path.exists() {
            eprintln!(
                "skipping septa fixture round-trip: {} not present (sibling septa repo absent)",
                fixture_path.display()
            );
            return;
        }

        let content = std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
            panic!(
                "failed to read septa fixture at {}: {e}",
                fixture_path.display()
            )
        });

        // Parse into the wire mirror (simulates what both socket and CLI paths do).
        let wire: TaskDetailWireMirror = serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("fixture deserialization failed: {e}"));

        let detail = map_wire_to_detail(wire);

        assert_eq!(
            detail.task_id, "01JNQABC0123456789GHFEDCBA",
            "task_id must match fixture"
        );
        assert_eq!(detail.status, "open", "status must match fixture");
        assert!(
            !detail.has_code_diff,
            "has_code_diff must be false per fixture"
        );
        assert!(
            !detail.has_verification_passed,
            "has_verification_passed must be false per fixture"
        );
    }

    /// Prove that `map_wire_to_detail` correctly renames `owner_agent_id` →
    /// `agent_id` and `parent_task_id` → `parent_id` when both fields are
    /// present (non-None).  The septa fixture omits these fields, so the
    /// round-trip test only exercises the absent-field path; this test covers
    /// the remap path explicitly.
    #[test]
    fn map_wire_to_detail_remaps_agent_id_and_parent_id_when_present() {
        let json = r#"{
            "task": {
                "task_id": "01TASK",
                "title": "remap test",
                "status": "in_progress",
                "owner_agent_id": "agent-abc",
                "parent_task_id": "parent-xyz",
                "has_code_diff": true,
                "has_verification_passed": true
            }
        }"#;

        let wire: TaskDetailWireMirror =
            serde_json::from_str(json).expect("inline fixture must deserialize");
        let detail = map_wire_to_detail(wire);

        assert_eq!(
            detail.agent_id,
            Some("agent-abc".to_string()),
            "owner_agent_id must be remapped to agent_id"
        );
        assert_eq!(
            detail.parent_id,
            Some("parent-xyz".to_string()),
            "parent_task_id must be remapped to parent_id"
        );
        assert!(detail.has_code_diff, "has_code_diff must be true");
        assert!(
            detail.has_verification_passed,
            "has_verification_passed must be true"
        );
    }

    // -- parse_canopy_rpc_response envelope tests ----------------------------------

    /// Prove that the real canopy not-found envelope — a top-level JSON-RPC error
    /// object with no `result` field — is detected and surfaces the message text,
    /// not misread as a parse error.
    #[cfg(unix)]
    #[test]
    fn parse_canopy_rpc_response_top_level_error_envelope_is_canopy_error() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"task detail: not found"}}"#;
        let result = parse_canopy_rpc_response(raw);
        match result {
            Err(DispatchError::CanopyError(msg)) => {
                assert!(
                    msg.contains("not found"),
                    "expected error message to contain 'not found', got: {msg}"
                );
            }
            Ok(_) => panic!("expected Err(CanopyError), got Ok"),
            Err(other) => panic!("expected CanopyError, got: {other:?}"),
        }
    }

    /// Prove that a well-formed success envelope deserializes correctly end-to-end.
    #[cfg(unix)]
    #[test]
    fn parse_canopy_rpc_response_success_envelope_deserializes_task() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"result":{"task":{"task_id":"01TEST","title":"socket test","status":"open"},"completion_signal":null}}"#;
        let result = parse_canopy_rpc_response(raw);
        match result {
            Ok(wire) => {
                assert_eq!(wire.task.task_id, "01TEST");
                assert_eq!(wire.task.status, "open");
            }
            Err(e) => panic!("expected Ok, got: {e:?}"),
        }
    }

    /// Prove that the defensive nested result.error path still works even though
    /// canopy does not emit this shape on the socket transport.
    #[cfg(unix)]
    #[test]
    fn parse_canopy_rpc_response_nested_result_error_is_canopy_error() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"result":{"error":"legacy app error"}}"#;
        let result = parse_canopy_rpc_response(raw);
        match result {
            Err(DispatchError::CanopyError(msg)) => {
                assert!(
                    msg.contains("legacy app error"),
                    "expected message to contain error string, got: {msg}"
                );
            }
            Ok(_) => panic!("expected Err(CanopyError), got Ok"),
            Err(other) => panic!("expected CanopyError, got: {other:?}"),
        }
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
