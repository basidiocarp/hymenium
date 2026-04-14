# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Hymenium is a workflow orchestration engine for multi-agent task execution. It is a single Rust binary with a CLI and an MCP server that reads handoff documents, runs workflow templates, and dispatches agents through Canopy. Hymenium owns the workflow lifecycle — phase gating, dispatch decisions, handoff decomposition, progress monitoring, and retry/recovery — while deferring coordination state to Canopy, lifecycle signals to Cortina, long-term memory to Hyphae, and agent sessions to Volva.

---

## What Hymenium Does NOT Do

- Does not store tasks or coordination records (Canopy owns the ledger; Hymenium reads and writes through Canopy's MCP surface)
- Does not capture lifecycle events or session signals (Cortina owns hook capture and attribution)
- Does not host agent execution or manage runtime sessions (Volva owns that)
- Does not hold long-term memory or indexed documents (Hyphae owns memory and retrieval)
- Does not handle installation, update, or doctor flows (Stipe owns the install lifecycle)
- Does not provide operator utilities like statuslines (Annulus owns that)

---

## Failure Modes

- **Canopy unavailable**: workflow execution pauses; Hymenium logs the interruption and retries on reconnect rather than corrupting in-flight state.
- **Handoff parse failure**: reports the parse error with the offending file path, does not dispatch, and returns control to the caller with a clear message.
- **Agent stalled**: detected via heartbeat timeout; Hymenium closes the stalled agent, narrows the scope if needed, and relaunches with a fresh dispatch.
- **Workflow template not found**: rejects dispatch with a named error identifying the missing template; does not fall back to an ad hoc workflow.
- **Phase gate not met**: blocks transition and emits a structured gate failure; the workflow stays at the current phase until the gate condition is satisfied.

---

## State Locations

| What | Path |
|------|------|
| Workflow database | `~/.local/share/hymenium/hymenium.db` (or `HYMENIUM_DB` env) |
| Config file | `~/.config/hymenium/config.toml` (or `HYMENIUM_CONFIG` env) |
| Log output | stderr (level via `HYMENIUM_LOG` env) |

---

## Build & Test Commands

```bash
cargo build --release
cargo install --path .

cargo test                              # All unit and integration tests
cargo test parser                       # Parser tests only
cargo test workflow                     # Workflow engine tests only
cargo test --ignored                    # Canopy round-trip tests (requires running Canopy)

cargo clippy
cargo fmt --check
cargo fmt
```

---

## Architecture

```text
src/
├── main.rs              # CLI entry point and MCP server
├── parser/              # Handoff document parser
│   ├── markdown.rs      # Parse structured handoff markdown
│   └── metadata.rs      # Extract handoff metadata block
├── decompose/           # Split large handoffs into focused child handoffs
│   ├── algorithm.rs     # Group-by-project, union-find merge, chunk packing
│   ├── effort.rs        # Effort parsing and tier assignment
│   └── render.rs        # Render child handoff markdown
├── workflow/            # Workflow template engine
│   ├── template.rs      # Workflow pattern definitions
│   ├── engine.rs        # Workflow execution state machine
│   └── gate.rs          # Phase gating rules
├── dispatch/            # Create Canopy tasks, assign agents by tier
│   ├── cli.rs           # CliCanopyClient (shells out to canopy CLI)
│   ├── mock.rs          # MockCanopyClient (in-memory for testing)
│   └── orchestrate.rs   # dispatch_workflow orchestration logic
├── monitor/             # Progress monitoring and escalation
│   ├── progress.rs      # check_progress and stall detection
│   └── handler.rs       # handle_signal routes signals to recovery
├── retry.rs             # Stalled agent detection and recovery
└── store.rs             # SQLite workflow state persistence
```

- **parser/**: owns structured handoff intake; all downstream code receives parsed types, not raw strings.
- **decompose/**: splits large handoffs into focused child handoffs using project grouping, dependency merging, and effort-based packing.
- **workflow/**: the core state machine; `engine.rs` drives transitions, `gate.rs` enforces phase preconditions before any dispatch happens.
- **dispatch/**: the only module that writes to Canopy; `orchestrate.rs` creates tasks and assigns agents, `cli.rs` handles the Canopy CLI boundary.
- **monitor/**: polls Canopy state, evaluates completeness gates, and escalates when workflows stall beyond the configured timeout.
- **retry.rs**: detects heartbeat timeouts, closes stalled agents through Canopy, and re-enters the dispatch flow with a narrowed scope.
- **store.rs**: SQLite persistence for workflow state that belongs to Hymenium, not to Canopy.

---

## Core Abstraction

```rust
pub trait WorkflowEngine {
    fn start(&self, template: &WorkflowTemplate, handoff: &ParsedHandoff) -> Result<WorkflowId>;
    fn advance(&self, id: WorkflowId) -> Result<PhaseTransition>;
    fn status(&self, id: WorkflowId) -> Result<WorkflowStatus>;
    fn recover(&self, id: WorkflowId) -> Result<RecoveryAction>;
}
```

`WorkflowEngine` is the central interface. `engine.rs` implements it by delegating to `gate.rs` for preconditions, `dispatch.rs` for agent creation, and `monitor.rs` for completeness checks. Tests stub this trait to exercise gate and retry logic without live Canopy connectivity.

---

## Key Design Decisions

- **Separate binary from Canopy** — orchestration and the coordination ledger have different failure modes; tying them together would make Canopy unavailability block all workflow state reads.
- **SQLite for workflow state** — local durability, same pattern as the rest of the ecosystem; Hymenium's database holds workflow lifecycle records that are distinct from Canopy's task ledger.
- **MCP and CLI dual surface** — same pattern as Canopy; orchestrators can drive Hymenium programmatically or from shell scripts without picking a protocol.
- **Reads Canopy via MCP/CLI, never via direct database access** — keeps the contract boundary clean and lets Canopy's schema evolve without breaking Hymenium's internals.
- **Workflow templates are data, not code** — templates in `templates/` are declarative patterns that the engine interprets; adding a new workflow pattern does not require changing engine logic.
- **Phase gates are non-negotiable** — the auditor phase cannot start until the implementer phase satisfies all gate conditions; this is enforced in `gate.rs`, not left to caller discipline.

---

## Key Files

| File | Purpose |
|------|---------|
| `src/workflow/engine.rs` | Central state machine; owns phase transitions and gate evaluation |
| `src/workflow/gate.rs` | Phase gating rules; the single enforcement point for transition preconditions |
| `src/dispatch.rs` | Canopy task creation and agent assignment; the only outbound write surface |
| `src/monitor.rs` | Completeness polling, escalation thresholds, and stall detection |
| `src/retry.rs` | Stalled agent close-and-relaunch logic |
| `src/parser/markdown.rs` | Handoff document intake; maps raw markdown to `ParsedHandoff` |
| `src/store.rs` | SQLite schema, migrations, and workflow state persistence |

---

## Communication Contracts

### Outbound (this project sends)

| Contract | Target | Protocol | Schema |
|----------|--------|----------|--------|
| `workflow-status-v1` | Cap / Annulus | MCP tool or CLI | `septa/workflow-status-v1.schema.json` |
| `dispatch-request-v1` | Canopy | MCP tool `canopy_task_create` and `canopy_task_assign` | `septa/dispatch-request-v1.schema.json` |

**Source files:**
- `src/dispatch.rs` — `dispatch_handoff()` — builds and sends `dispatch-request-v1` to Canopy via MCP
- `src/monitor.rs` — `emit_status()` — builds and sends `workflow-status-v1` for operator visibility

Breaking change impact: if `dispatch-request-v1` shape changes, Canopy will reject task creation calls and workflows will stall at dispatch. If `workflow-status-v1` changes, Cap will misrender operator views.

### Inbound (this project receives)

| Contract | Source | Protocol | Schema |
|----------|--------|----------|--------|
| `canopy-task-detail-v1` | Canopy | MCP tool `canopy_get_task` or CLI `canopy api task` | `septa/canopy-task-detail-v1.schema.json` |
| Handoff documents | `.handoffs/` directory | File read | Structured markdown (see `src/parser/`) |

**Receiver source files:**
- `src/monitor.rs` — `poll_canopy()` — reads `canopy-task-detail-v1` to evaluate completeness gates
- `src/parser/markdown.rs` — `parse_handoff()` — reads raw handoff markdown and returns `ParsedHandoff`

### Shared Dependencies

- **spore**: check `../ecosystem-versions.toml` before upgrading. Pin must stay in sync across all consumers.
- **rusqlite**: schema changes affect workflow state durability; keep version aligned with ecosystem pin in `ecosystem-versions.toml`.
- **Canopy MCP surface**: Hymenium depends on Canopy's MCP tool names and argument shapes; test against a running Canopy instance before releasing.

---

## Configuration

Config file: `~/.config/hymenium/config.toml` (override with `HYMENIUM_CONFIG` env).

| Variable | Default | Description |
|----------|---------|-------------|
| `HYMENIUM_LOG` | `warn` | Log level for stderr output |
| `HYMENIUM_DB` | `~/.local/share/hymenium/hymenium.db` | Workflow state database path |
| `HYMENIUM_CONFIG` | `~/.config/hymenium/config.toml` | Config file path |
| `HYMENIUM_CANOPY_SOCKET` | `` | Canopy MCP socket path (falls back to CLI if unset) |

---

## Testing Strategy

- Unit tests cover parser correctness, decomposer split logic, gate precondition evaluation, and retry decision trees — all without live Canopy.
- Integration tests (marked `#[ignore]`) exercise the full Canopy round-trip: dispatch, poll, status update, and close-out. Run with `cargo test --ignored` against a running Canopy instance.
- Workflow template tests verify that each template in `templates/` parses correctly and produces a valid initial state.
- Fixtures in `tests/fixtures/` use real handoff markdown samples, not synthetic ones.
