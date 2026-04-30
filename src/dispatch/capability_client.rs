//! Capability-aware Canopy dispatch client.
//!
//! Resolves `workflow.dispatch.v1` via Spore before dispatching. When a live
//! endpoint is available, this client sends a `dispatch-request-v1` payload
//! directly to Canopy's typed intake instead of building raw CLI flag calls.
//!
//! # Fallback hierarchy
//!
//! 1. **Typed endpoint (preferred)**: if `workflow.dispatch.v1` is resolved to a
//!    CLI transport with a known command, send `dispatch-request-v1` JSON on stdin.
//! 2. **CLI compatibility adapter (fallback only)**: the inner `fallback` client is
//!    used when the typed endpoint is absent or non-CLI. A `tracing::warn` is
//!    emitted so dogfood runs can prove whether the typed path is being used.
//!
//! The CLI fallback remains tested and isolated. New system-to-system dispatch
//! should prefer the typed endpoint; do not add further CLI-only integration paths.
//!
//! # Capability types
//!
//! This module embeds the capability registry and runtime lease types from
//! the `capability-registry-v1` and `capability-runtime-lease-v1` Septa
//! contracts. These were inlined here because spore v0.4.11 (the ecosystem
//! workspace pin) does not yet export a `capability` module; the types were
//! added in a later commit. This keeps hymenium at the ecosystem pin without
//! waiting for a spore release.

use super::{
    CanopyClient, CompletenessReport, DispatchError, ImportResult, TaskDetail, TaskOptions,
};
#[cfg(unix)]
use crate::dispatch::cli::libc_kill;
use crate::dispatch::cli::{resolve_canopy_binary, CANOPY_ALLOWED_ENV, CANOPY_TIMEOUT};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Inline capability types (capability-registry-v1 and capability-runtime-lease-v1)
// ---------------------------------------------------------------------------

/// Transport kind used to call a capability endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum TransportKind {
    /// MCP over stdin/stdout.
    Stdio,
    /// Unix domain socket.
    UnixSocket,
    /// TCP endpoint.
    Tcp,
    /// Subprocess invocation via CLI.
    Cli,
}

/// Who manages a registry entry installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum CapabilityManager {
    Stipe,
    Manual,
    #[serde(rename = "self")]
    SelfManaged,
}

/// Last known health state for a registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RegistryHealthStatus {
    Ok,
    Degraded,
    Missing,
}

/// Health hint stored in the installed registry entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryHealthHint {
    pub status: RegistryHealthStatus,
    pub message: Option<String>,
}

/// One entry in the installed capability registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub tool: String,
    pub version: String,
    pub manager: CapabilityManager,
    pub capability_ids: Vec<String>,
    pub contract_ids: Vec<String>,
    pub transport: TransportKind,
    pub binary_path: Option<String>,
    pub health: Option<RegistryHealthHint>,
}

/// Parsed `capability-registry-v1` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRegistry {
    pub schema_version: String,
    pub written_at_unix: u64,
    pub entries: Vec<RegistryEntry>,
}

/// Self-reported health status in a runtime lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum LeaseHealthStatus {
    Ok,
    Degraded,
}

/// Health hint in a runtime lease.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseHealthHint {
    pub status: LeaseHealthStatus,
    pub message: Option<String>,
}

/// Parsed `capability-runtime-lease-v1` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeLease {
    pub schema_version: String,
    pub tool: String,
    pub capability_id: String,
    pub transport: TransportKind,
    pub pid: u32,
    pub leased_at_unix: u64,
    pub expires_at_unix: Option<u64>,
    pub endpoint: Option<String>,
    pub command: Option<String>,
    pub version: Option<String>,
    pub health: Option<LeaseHealthHint>,
}

impl RuntimeLease {
    /// Returns `true` when this lease has passed its `expires_at_unix` deadline.
    fn is_expired(&self) -> bool {
        let Some(expires) = self.expires_at_unix else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        now > expires
    }
}

/// A resolved endpoint candidate for a requested capability id.
#[derive(Debug, Clone)]
pub struct EndpointCandidate {
    pub tool: String,
    pub transport: TransportKind,
    pub endpoint: Option<String>,
    pub command: Option<PathBuf>,
    pub version: Option<String>,
    /// Whether this candidate came from a live runtime lease (true) or the
    /// installed registry fallback (false).
    pub from_lease: bool,
}

// ---------------------------------------------------------------------------
// Capability resolution
// ---------------------------------------------------------------------------

/// Resolve a capability id to a preferred endpoint candidate.
///
/// Checks runtime leases first (live endpoints), then falls back to the
/// installed registry. Returns the first non-stale match, or `None` when
/// neither source has the capability.
fn resolve_capability(
    capability_id: &str,
    registry_path: &Path,
    lease_dir: &Path,
) -> anyhow::Result<Option<EndpointCandidate>> {
    // Step 1: check live leases first.
    let leases = load_leases(lease_dir);
    for lease in &leases {
        if lease.capability_id == capability_id && !lease.is_expired() {
            return Ok(Some(EndpointCandidate {
                tool: lease.tool.clone(),
                transport: lease.transport.clone(),
                endpoint: lease.endpoint.clone(),
                command: lease.command.as_deref().map(PathBuf::from),
                version: lease.version.clone(),
                from_lease: true,
            }));
        }
    }

    // Step 2: fall back to the installed registry.
    let Some(registry) = load_registry(registry_path)? else {
        return Ok(None);
    };

    for entry in &registry.entries {
        if entry.capability_ids.iter().any(|id| id == capability_id) {
            if matches!(
                entry.health.as_ref().map(|h| &h.status),
                Some(RegistryHealthStatus::Missing)
            ) {
                continue;
            }
            return Ok(Some(EndpointCandidate {
                tool: entry.tool.clone(),
                transport: entry.transport.clone(),
                endpoint: None,
                command: entry.binary_path.as_deref().map(PathBuf::from),
                version: Some(entry.version.clone()),
                from_lease: false,
            }));
        }
    }

    Ok(None)
}

fn load_leases(dir: &Path) -> Vec<RuntimeLease> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|content| serde_json::from_str::<RuntimeLease>(&content).ok())
        .collect()
}

fn load_registry(path: &Path) -> anyhow::Result<Option<CapabilityRegistry>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(serde_json::from_str(&content)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!(
            "failed to read capability registry at {}: {e}",
            path.display()
        )),
    }
}

// ---------------------------------------------------------------------------
// Request builder
// ---------------------------------------------------------------------------

/// Capability ID for Canopy's dispatch intake endpoint.
pub const DISPATCH_CAPABILITY: &str = "workflow.dispatch.v1";

/// Build a `dispatch-request-v1` JSON payload from task-creation arguments.
///
/// Adapts the internal `CanopyClient::create_task` interface into the Septa
/// `dispatch-request-v1` wire format.  Fields that have no direct mapping are
/// given safe defaults.
pub fn build_dispatch_request(
    _title: &str,
    _description: &str,
    project_root: &str,
    options: &TaskOptions,
) -> String {
    let workflow_template = options
        .workflow_id
        .as_deref()
        .unwrap_or("impl-audit")
        .to_string();
    let target_repo = Path::new(project_root)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(project_root)
        .to_string();

    json!({
        "schema_version": "1.0",
        "handoff_path": "",
        "workflow_template": workflow_template,
        "project_root": project_root,
        "target_repo": target_repo,
        "priority": "medium",
        "depends_on": []
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// CapabilityCanopyClient
// ---------------------------------------------------------------------------

/// A capability-aware canopy dispatch client.
///
/// Resolves `workflow.dispatch.v1` through Spore before sending dispatch
/// requests.  When a typed endpoint is available it sends a
/// `dispatch-request-v1` payload to Canopy's dispatch intake; otherwise it
/// falls back to the wrapped compatibility adapter with an observable warning.
pub struct CapabilityCanopyClient<C: CanopyClient> {
    /// Path to the ecosystem capability registry.
    registry_path: PathBuf,
    /// Directory where runtime capability lease files are stored.
    lease_dir: PathBuf,
    /// Compatibility fallback adapter — used when the typed endpoint is unavailable.
    ///
    /// `CliCanopyClient` is the production fallback. Tests substitute
    /// `MockCanopyClient` to verify fallback behavior without a live canopy
    /// instance.  This is a **compatibility adapter**: do not treat it as the
    /// preferred integration path.
    fallback: C,
}

impl<C: CanopyClient> CapabilityCanopyClient<C> {
    /// Create a client using the default Spore registry and lease paths.
    ///
    /// In production this resolves to the ecosystem-standard locations written
    /// by `stipe init`.
    pub fn new(fallback: C) -> Self {
        Self {
            registry_path: spore::paths::data_dir("basidiocarp")
                .join("capability-registry.json"),
            lease_dir: spore::paths::data_dir("basidiocarp").join("leases"),
            fallback,
        }
    }

    /// Create a client with explicit paths — used in tests to point at
    /// temporary fixtures rather than the live ecosystem registry.
    pub fn with_paths(registry_path: PathBuf, lease_dir: PathBuf, fallback: C) -> Self {
        Self {
            registry_path,
            lease_dir,
            fallback,
        }
    }

    /// Resolve `workflow.dispatch.v1` from the registry or live leases.
    ///
    /// Returns `None` when the capability cannot be found; logs at debug or
    /// warn level depending on whether the absence was expected or an error.
    fn resolve_dispatch_endpoint(&self) -> Option<EndpointCandidate> {
        match resolve_capability(DISPATCH_CAPABILITY, &self.registry_path, &self.lease_dir) {
            Ok(Some(candidate)) => Some(candidate),
            Ok(None) => {
                tracing::debug!(
                    capability = DISPATCH_CAPABILITY,
                    "capability not found in registry or leases"
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    capability = DISPATCH_CAPABILITY,
                    "capability resolution error; falling back to CLI compatibility adapter"
                );
                None
            }
        }
    }

    /// Send a `dispatch-request-v1` JSON payload to the resolved CLI command.
    ///
    /// Invokes `<command> dispatch submit -` with the JSON on stdin and parses
    /// the `task_id` field from the `DispatchResponse` on stdout.
    ///
    /// Security properties enforced here (same as `CliCanopyClient::run`):
    /// - The child environment is cleared and only the allowlisted variables
    ///   are restored, preventing secret leakage.
    /// - A 30-second wall-clock timeout kills the child if it hangs.
    fn send_dispatch_request(command: &Path, request_json: &str) -> Result<String, DispatchError> {
        // Enforce an absolute path to prevent PATH hijacking by relative binary
        // paths. Absolute paths are validated for existence and used directly;
        // relative paths are resolved through `which` so the caller gets a clear
        // error without silently invoking PATH-ordered impostor binaries.
        let bin = if command.is_absolute() {
            if !command.exists() {
                return Err(DispatchError::CanopyError(format!(
                    "dispatch endpoint not found: {}",
                    command.display()
                )));
            }
            command.to_path_buf()
        } else {
            resolve_canopy_binary(command.to_str().unwrap_or("canopy"))?
        };

        let env_pairs: Vec<(String, String)> = CANOPY_ALLOWED_ENV
            .iter()
            .filter_map(|key| std::env::var(key).ok().map(|val| (key.to_string(), val)))
            .collect();

        let mut child = std::process::Command::new(&bin)
            .args(["dispatch", "submit", "-"])
            .env_clear()
            .envs(env_pairs)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                DispatchError::CanopyError(format!(
                    "failed to start dispatch endpoint {}: {e}",
                    bin.display()
                ))
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(request_json.as_bytes())
                .map_err(|e| DispatchError::CanopyError(format!("write dispatch request: {e}")))?;
        }

        // Enforce a wall-clock timeout with a background kill thread.
        //
        // A cancellation channel lets the main thread signal the killer before
        // it fires, preventing a PID-reuse race: after wait_with_output() the
        // child PID is freed and could be reused by an unrelated process.
        let timeout = CANOPY_TIMEOUT;
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
            .map_err(|e| DispatchError::CanopyError(format!("dispatch endpoint failed: {e}")))?;

        // Cancel the killer before it fires (safe even if it already ran).
        let _ = cancel_tx.send(());
        let _ = killer.join();

        if !output.status.success() {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt as _;
                if output.status.signal() == Some(libc::SIGKILL) {
                    return Err(DispatchError::CanopyError(
                        "canopy dispatch timed out after 30s".to_string(),
                    ));
                }
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DispatchError::CanopyError(stderr.trim().to_string()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .and_then(|v| {
                v.get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(String::from)
            })
            .ok_or_else(|| {
                DispatchError::CanopyError(format!(
                    "dispatch endpoint returned unexpected output: {stdout}"
                ))
            })
    }
}

impl<C: CanopyClient> CanopyClient for CapabilityCanopyClient<C> {
    fn create_task(
        &self,
        title: &str,
        description: &str,
        project_root: &str,
        options: &TaskOptions,
    ) -> Result<String, DispatchError> {
        // Prefer the typed dispatch endpoint over the CLI compatibility adapter.
        if let Some(candidate) = self.resolve_dispatch_endpoint() {
            match candidate.transport {
                TransportKind::Cli => {
                    if let Some(ref command) = candidate.command {
                        let req = build_dispatch_request(title, description, project_root, options);
                        tracing::debug!(
                            command = %command.display(),
                            "sending dispatch-request-v1 to typed capability endpoint"
                        );
                        return Self::send_dispatch_request(command, &req);
                    }
                }
                _ => {
                    tracing::warn!(
                        transport = ?candidate.transport,
                        "workflow.dispatch.v1 resolved with unsupported transport; \
                         falling back to CLI compatibility adapter"
                    );
                }
            }
        }

        // CLI compatibility adapter — fallback only.
        tracing::warn!(
            "workflow.dispatch.v1 endpoint unavailable; \
             falling back to CLI compatibility adapter"
        );
        self.fallback
            .create_task(title, description, project_root, options)
    }

    fn create_subtask(
        &self,
        parent_id: &str,
        title: &str,
        description: &str,
        options: &TaskOptions,
    ) -> Result<String, DispatchError> {
        // No typed endpoint for subtask creation yet; delegate to compatibility adapter.
        self.fallback
            .create_subtask(parent_id, title, description, options)
    }

    fn assign_task(
        &self,
        task_id: &str,
        agent_id: &str,
        assigned_by: &str,
    ) -> Result<(), DispatchError> {
        self.fallback.assign_task(task_id, agent_id, assigned_by)
    }

    fn get_task(&self, task_id: &str) -> Result<TaskDetail, DispatchError> {
        self.fallback.get_task(task_id)
    }

    fn check_completeness(&self, handoff_path: &str) -> Result<CompletenessReport, DispatchError> {
        self.fallback.check_completeness(handoff_path)
    }

    fn import_handoff(
        &self,
        path: &str,
        assign_to: Option<&str>,
    ) -> Result<ImportResult, DispatchError> {
        self.fallback.import_handoff(path, assign_to)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{MockCanopyClient, TaskOptions};
    use std::fs;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn make_client_with_paths(
        registry_path: PathBuf,
        lease_dir: PathBuf,
    ) -> CapabilityCanopyClient<MockCanopyClient> {
        CapabilityCanopyClient::with_paths(registry_path, lease_dir, MockCanopyClient::new())
    }

    // ─── build_dispatch_request ───────────────────────────────────────────────

    #[test]
    fn build_dispatch_request_includes_required_fields() {
        let options = TaskOptions {
            workflow_id: Some("impl-audit".to_string()),
            ..Default::default()
        };
        let json_str = build_dispatch_request("title", "desc", "/workspace/canopy", &options);
        let v: serde_json::Value = serde_json::from_str(&json_str).expect("valid JSON");

        assert_eq!(v["schema_version"], "1.0");
        assert_eq!(v["workflow_template"], "impl-audit");
        assert_eq!(v["project_root"], "/workspace/canopy");
        assert_eq!(v["target_repo"], "canopy");
        assert_eq!(v["priority"], "medium");
        assert_eq!(v["depends_on"], serde_json::json!([]));
    }

    #[test]
    fn build_dispatch_request_defaults_template_when_none() {
        let options = TaskOptions::default();
        let json_str = build_dispatch_request("t", "d", "/workspace", &options);
        let v: serde_json::Value = serde_json::from_str(&json_str).expect("valid JSON");
        assert_eq!(v["workflow_template"], "impl-audit");
    }

    // ─── fallback when no registry ────────────────────────────────────────────

    #[test]
    fn capability_client_falls_back_to_cli_when_no_registry() {
        let dir = temp_dir();
        let client = make_client_with_paths(
            dir.path().join("no-registry.json"),
            dir.path().join("no-leases"),
        );

        // MockCanopyClient is the fallback — it returns "mock-task-1"
        let task_id = client
            .create_task("test task", "desc", "/tmp", &TaskOptions::default())
            .expect("fallback should succeed");

        assert!(
            task_id.starts_with("mock-task"),
            "expected mock-task id, got: {task_id}"
        );
    }

    // ─── fallback when registry has no matching capability ────────────────────

    #[test]
    fn capability_client_falls_back_when_capability_absent_from_registry() {
        let dir = temp_dir();
        // Write a registry that does NOT include workflow.dispatch.v1
        let reg = serde_json::json!({
            "schema_version": "1.0",
            "written_at_unix": 1_700_000_000_u64,
            "entries": [{
                "tool": "hyphae",
                "version": "0.1.0",
                "manager": "stipe",
                "capability_ids": ["memory.store.v1"],
                "contract_ids": [],
                "transport": "cli",
                "binary_path": "/usr/local/bin/hyphae",
                "health": null
            }]
        });
        let reg_path = dir.path().join("registry.json");
        fs::write(&reg_path, serde_json::to_string(&reg).unwrap()).unwrap();

        let client = make_client_with_paths(reg_path, dir.path().join("no-leases"));
        let task_id = client
            .create_task("task", "desc", "/tmp", &TaskOptions::default())
            .expect("fallback should succeed");

        assert!(
            task_id.starts_with("mock-task"),
            "expected mock-task fallback, got: {task_id}"
        );
    }

    // ─── fallback when lease is stale ─────────────────────────────────────────

    #[test]
    fn capability_client_falls_back_on_stale_lease() {
        let dir = temp_dir();
        let lease_dir = dir.path().join("leases");
        fs::create_dir_all(&lease_dir).unwrap();

        // Write an expired lease for workflow.dispatch.v1
        let expired_lease = RuntimeLease {
            schema_version: "1.0".to_string(),
            tool: "canopy".to_string(),
            capability_id: DISPATCH_CAPABILITY.to_string(),
            transport: TransportKind::Cli,
            pid: 99999,
            leased_at_unix: 1,
            expires_at_unix: Some(1), // always in the past
            endpoint: None,
            command: Some("/usr/local/bin/canopy".to_string()),
            version: None,
            health: None,
        };
        fs::write(
            lease_dir.join("canopy-dispatch.json"),
            serde_json::to_string(&expired_lease).unwrap(),
        )
        .unwrap();

        // No registry → only the expired lease, which is stale
        let client = make_client_with_paths(dir.path().join("no-registry.json"), lease_dir);
        let task_id = client
            .create_task("task", "desc", "/tmp", &TaskOptions::default())
            .expect("stale lease should trigger fallback");

        assert!(
            task_id.starts_with("mock-task"),
            "expected mock-task fallback, got: {task_id}"
        );
    }

    // ─── capability endpoint is found ─────────────────────────────────────────

    #[test]
    fn capability_client_uses_registry_entry_to_build_dispatch_request() {
        let dir = temp_dir();
        // Write a registry that advertises workflow.dispatch.v1 pointing at
        // a fake command path.  We don't actually invoke the command in this
        // unit test; we verify that resolve_dispatch_endpoint returns a candidate.
        let reg = serde_json::json!({
            "schema_version": "1.0",
            "written_at_unix": 1_700_000_000_u64,
            "entries": [{
                "tool": "canopy",
                "version": "0.5.0",
                "manager": "stipe",
                "capability_ids": [DISPATCH_CAPABILITY],
                "contract_ids": ["dispatch-request-v1"],
                "transport": "cli",
                "binary_path": "/nonexistent/canopy-bin",
                "health": null
            }]
        });
        let reg_path = dir.path().join("registry.json");
        fs::write(&reg_path, serde_json::to_string(&reg).unwrap()).unwrap();

        let client = make_client_with_paths(reg_path, dir.path().join("no-leases"));

        // The endpoint is resolved but the command doesn't exist — the
        // CanopyError propagates from send_dispatch_request.
        let result = client.create_task("task", "desc", "/tmp", &TaskOptions::default());

        // The test verifies we attempted the typed endpoint path (not the mock
        // fallback).  A missing binary returns CanopyError, not a mock-task id.
        match result {
            Err(DispatchError::CanopyError(msg)) => {
                assert!(
                    msg.contains("nonexistent") || msg.contains("failed"),
                    "expected endpoint error, got: {msg}"
                );
            }
            Ok(id) if id.starts_with("mock-task") => {
                panic!("expected typed endpoint attempt, got mock fallback")
            }
            Ok(_) => {} // unexpected success is acceptable (binary happened to exist)
            Err(e) => panic!("unexpected error variant: {e}"),
        }
    }

    // ─── other methods delegate to fallback ───────────────────────────────────

    #[test]
    fn non_create_methods_delegate_to_fallback() {
        let dir = temp_dir();
        let client = make_client_with_paths(
            dir.path().join("no-registry.json"),
            dir.path().join("no-leases"),
        );

        // Pre-create a task through the fallback to avoid NotFound.
        let task_id = client
            .create_task("parent", "desc", "/tmp", &TaskOptions::default())
            .expect("create");

        let subtask_id = client
            .create_subtask(&task_id, "child", "desc", &TaskOptions::default())
            .expect("create_subtask");
        assert!(subtask_id.starts_with("mock-task"));

        client
            .assign_task(&subtask_id, "agent-1", "hymenium")
            .expect("assign_task");

        let detail = client.get_task(&subtask_id).expect("get_task");
        assert_eq!(detail.agent_id.as_deref(), Some("agent-1"));

        let report = client
            .check_completeness("/handoff.md")
            .expect("check_completeness");
        assert!(report.complete);

        let import = client
            .import_handoff("/path/to/handoff.md", None)
            .expect("import_handoff");
        assert!(!import.task_id.is_empty());
    }
}
