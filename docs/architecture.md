# Hymenium Architecture

Hymenium is a single-crate Rust binary with CLI, MCP, and SQLite store layers.
Its core job is to automate multi-agent workflow patterns: it reads handoff
documents, decomposes large ones into focused child handoffs, dispatches agents
through Canopy, enforces phase gates before transitions, monitors progress, and
recovers from stalled agents. This document covers the system boundary, request
flow, and data model.

---

## Design Principles

- **Single orchestration authority** -- Hymenium is the authoritative source for
  workflow lifecycle, phase transitions, dispatch decisions, and escalation.
  Other tools (Canopy, Cortina, Hyphae) own their respective domains but defer
  to Hymenium for orchestration semantics.
- **Workflow-first, not agent-first** -- durable workflow state drives what
  agents do, not the other way around. Agents are assigned by the engine, not
  self-selected.
- **Phase gates are non-negotiable** -- the auditor phase cannot start until
  the implementer phase satisfies all gate conditions. This is enforced in
  `gate.rs`, not left to caller discipline.
- **Templates are data, not code** -- workflow patterns are declarative
  definitions that the engine interprets. Adding a new workflow type should not
  require engine changes.
- **Canopy boundary is clean** -- Hymenium reads and writes Canopy state
  through MCP tools or CLI only, never via direct database access. This lets
  Canopy's schema evolve without breaking Hymenium's internals.
- **Progressive recovery** -- stalled agents are closed and relaunched with
  narrowed scope or escalated tier, not retried blindly.
- **Parse at the boundary** -- handoff markdown is parsed into typed values at
  intake. Downstream code works with `ParsedHandoff`, `ParsedStep`, and
  `HandoffMetadata`, never raw strings.

---

## System Boundary

### Hymenium owns

- Workflow lifecycle state -- which phase a workflow is in and what transitions
  are allowed
- Phase gating -- enforcing that phase preconditions are met before dispatching
  the next agent
- Dispatch decisions -- reading a handoff, choosing an agent tier, and creating
  Canopy tasks
- Handoff decomposition -- splitting a large handoff into focused child
  handoffs based on project, effort, and dependency structure
- Progress monitoring -- polling Canopy state and evaluating completeness gates
- Retry and recovery -- detecting stalled agents and relaunching with corrected
  scope or escalated tier

### Canopy owns

- The coordination ledger -- task storage, assignment history, and coordination
  records that make multi-agent work explicit and auditable.
- Evidence references and verification state
- The operator surface -- queue views, attention surfaces, and read models that
  let operators understand task state and evidence without reconstructing facts
  from logs

### Cortina owns

- Lifecycle signal capture and session attribution
- Hook event recording and structured runtime signals

### Hyphae owns

- Long-term memory, recall, and indexed document retrieval
- Session context and cross-session knowledge

### Volva owns

- Agent session management and execution hosting
- Backend orchestration at the runtime seam

### Stipe owns

- Installation, setup, update, and ecosystem repair

---

## Workspace Structure

```text
src/
├── main.rs              CLI entry point and MCP server
├── parser/              Handoff document parser
│   ├── markdown.rs      Parse structured handoff markdown
│   └── metadata.rs      Extract handoff metadata block
├── decompose/           Split large handoffs into focused child handoffs
│   ├── algorithm.rs     Group-by-project, union-find merge, chunk packing
│   ├── effort.rs        Effort parsing and tier assignment
│   └── render.rs        Render child handoff markdown
├── workflow/            Workflow template engine
│   ├── template.rs      Workflow pattern definitions and registry
│   ├── engine.rs        Workflow execution state machine
│   └── gate.rs          Phase gating rules and evaluator trait
├── dispatch/            Create Canopy tasks, assign agents by tier
│   ├── cli.rs           CliCanopyClient (shells out to canopy CLI)
│   ├── mock.rs          MockCanopyClient (in-memory for testing)
│   └── orchestrate.rs   dispatch_workflow orchestration logic
├── monitor/             Progress monitoring and escalation
│   ├── progress.rs      check_progress and stall detection
│   └── handler.rs       handle_signal routes signals to recovery
├── retry.rs             Stalled agent detection and recovery
└── store.rs             SQLite workflow state persistence
```

Hymenium compiles into a single binary.

- **`parser/`**: Owns structured handoff intake. All downstream code receives
  parsed types, not raw strings.
- **`decompose/`**: Splits large handoffs by project grouping, dependency
  merging (union-find), and effort-based packing. Assigns agent tiers and
  renders child handoff markdown.
- **`workflow/`**: The core state machine. `engine.rs` drives phase
  transitions, `template.rs` defines declarative workflow patterns, and
  `gate.rs` enforces phase preconditions.
- **`dispatch/`**: The only module that writes to Canopy. `orchestrate.rs`
  creates tasks and assigns agents, `cli.rs` shells out to the Canopy CLI.
- **`monitor/`**: Polls Canopy state, evaluates completeness gates, and
  escalates when workflows stall. `handler.rs` routes progress signals to
  recovery actions.
- **`retry.rs`**: Decides recovery actions based on stall reason and retry
  count: plain retry, narrowed scope, tier escalation, or operator escalation.
- **`store.rs`**: SQLite persistence for workflow state that belongs to
  Hymenium, not to Canopy.

---

## Request Flow

When a handoff is submitted for workflow execution:

1. **Parse** (`parser/markdown.rs`)
   Reads structured handoff markdown and extracts title, metadata, scope,
   steps, verification blocks, and paste markers into typed values. Invalid
   documents are rejected here, not discovered at dispatch time.

2. **Decompose** (`decompose/`)
   If the handoff exceeds a single agent's scope, splits it into focused child
   handoffs. Groups steps by project, merges dependency-connected steps via
   union-find, packs chunks at effort and step-count boundaries, builds an
   inter-piece dependency graph, and assigns agent tiers based on effort.

3. **Template lookup** (`workflow/template.rs`)
   Selects the workflow template (e.g. `impl_audit_default`) from the registry.
   The template defines the phase sequence, entry/exit gates, and agent
   role/tier assignments.

4. **Start workflow** (`workflow/engine.rs`)
   Creates a `WorkflowInstance` with phase states initialized from the
   template. The first phase starts as `Pending`; it becomes `Active` when
   dispatch succeeds.

5. **Gate check** (`workflow/gate.rs`)
   Before dispatching into a phase, evaluates all entry gate conditions. If any
   condition fails, the workflow blocks at the current phase. Gate conditions
   include `CodeDiffExists`, `VerificationPassed`, `AuditClean`, and
   `FindingsResolved`.

6. **Dispatch** (`dispatch/orchestrate.rs`)
   Creates Canopy tasks for the phase, assigns agents by tier, and tracks the
   dispatch in the workflow state. Builds the agent name following the
   `<role>/<repo>/<handoff-slug>/<run>` convention.

7. **Monitor** (`monitor/progress.rs`)
   Polls Canopy task state against the workflow's completeness conditions.
   Evaluates heartbeat timeout, code diff existence, and paste marker progress.
   Emits `ProgressSignal` values: `Healthy`, `Stalled`, `PhaseComplete`,
   `GateSatisfied`, or `Failed`.

8. **Recover** (`retry.rs`)
   When a stall or failure signal arrives, decides the recovery action based on
   the stall reason, retry count, and `RetryPolicy`. First stall retries plain,
   second narrows scope, third escalates to the operator. Tier escalation
   (Haiku -> Sonnet -> Opus) is configurable.

9. **Phase transition** (`workflow/engine.rs`)
   When the current phase's exit gate passes, the engine advances to the next
   phase. The cycle repeats: gate check, dispatch, monitor, recover.

---

## Data Model

### WorkflowInstance

```rust
pub struct WorkflowInstance {
    pub id: WorkflowId,
    pub template_name: String,
    pub handoff_path: String,
    pub phase_states: Vec<PhaseState>,
    pub created_at: DateTime<Utc>,
}
```

### PhaseState

```rust
pub struct PhaseState {
    pub phase_id: String,
    pub status: PhaseStatus,          // Pending, Active, Completed, Failed, Skipped
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub retry_count: u32,
}
```

### GateCondition

```rust
pub enum GateCondition {
    CodeDiffExists,
    VerificationPassed,
    AuditClean,
    FindingsResolved,
    Custom(String),
}
```

### RecoveryAction

```rust
pub enum RecoveryAction {
    Retry { narrowed_scope: Option<String>, new_tier: Option<AgentTier> },
    Escalate { reason: String },
    Cancel { reason: String },
}
```

### ProgressSignal

```rust
pub enum ProgressSignal {
    Healthy { phase_id, last_activity },
    Stalled { phase_id, since, reason: StallReason },
    PhaseComplete { phase_id },
    GateSatisfied { gate },
    Failed { phase_id, error },
}
```

---

## Testing

```bash
cargo test                  # All unit and integration tests
cargo test parser           # Parser tests only
cargo test workflow         # Workflow engine tests only
cargo test --ignored        # Canopy round-trip tests (requires running Canopy)
```

| Category | Count | What's Tested |
|----------|-------|---------------|
| Parser tests | 11 | Handoff parsing, step extraction, metadata, paste markers, fixtures |
| Decompose tests | 27 | Grouping, dependency merge, effort parsing, chunk packing, rendering |
| Workflow tests | 24 | Phase transitions, gate evaluation, template registry, mock evaluator |
| Dispatch tests | 21 | CanopyClient trait, mock client, dispatch orchestration, error guards |
| Monitor tests | 16 | Progress checking, stall detection, signal handling |
| Retry tests | 18 | Recovery decisions, progressive escalation, tier escalation, policy defaults |

Integration tests (marked `#[ignore]`) exercise the full Canopy round-trip:
dispatch, poll, status update, and close-out. Run with `cargo test --ignored`
against a running Canopy instance.

---

## Key Dependencies

- **`rusqlite`** -- local workflow state storage. Hymenium's database holds
  workflow lifecycle records distinct from Canopy's task ledger.
- **`clap`** -- the CLI contract for `hymenium run`, `status`, `retry`, and
  `serve`.
- **`spore`** -- shared ecosystem transport, config, and path primitives.
- **`chrono`** -- timestamps for phase transitions, heartbeat detection, and
  stall duration calculation.
- **`thiserror`** -- typed error enums for parser, decomposer, dispatch,
  monitor, and gate modules.
