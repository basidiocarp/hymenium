# Hymenium

Workflow orchestration engine. Automates multi-agent task execution patterns like implementer/auditor.

Named after the hymenium — the fertile spore-bearing surface of a mushroom, the layer where the actual reproductive work happens, sitting on top of supporting structures and coordinating the output.

Part of the [Basidiocarp ecosystem](https://github.com/basidiocarp).

---

## The Problem

AI agents need orchestrated workflows — implement, verify, audit, fix, close — but today this protocol is driven by prose instructions in `CLAUDE.md` and `AGENTS.md`. Agents can misread or skip steps, phase transitions happen on the honor system, and manual orchestration does not scale past one or two concurrent handoffs. When something stalls, the only recovery path is a human noticing and intervening.

## The Solution

Hymenium automates workflow patterns as first-class engine behavior. It reads handoff documents, decomposes large ones into focused child handoffs, creates Canopy tasks, assigns agents by capability tier, enforces phase gates before any transition, monitors progress against completeness conditions, and handles failures by closing stalled agents and relaunching with narrowed scope.

The implementer/auditor workflow is the first built-in template. Hymenium enforces the rule that the auditor phase cannot start until the implementer has produced a real code diff and verification results — a gate condition that prose instructions leave to agent discipline.

---

## The Ecosystem

| Tool | Purpose |
|------|---------|
| **[annulus](https://github.com/basidiocarp/annulus)** | Cross-ecosystem operator utilities |
| **[hymenium](https://github.com/basidiocarp/hymenium)** | Workflow orchestration engine (this project) |
| **[canopy](https://github.com/basidiocarp/canopy)** | Multi-agent coordination ledger |
| **[cap](https://github.com/basidiocarp/cap)** | Web dashboard for the ecosystem |
| **[cortina](https://github.com/basidiocarp/cortina)** | Lifecycle signal capture and session attribution |
| **[hyphae](https://github.com/basidiocarp/hyphae)** | Persistent agent memory |
| **[lamella](https://github.com/basidiocarp/lamella)** | Skills, hooks, and plugins for coding agents |
| **[mycelium](https://github.com/basidiocarp/mycelium)** | Token-optimized command output |
| **[rhizome](https://github.com/basidiocarp/rhizome)** | Code intelligence via tree-sitter and LSP |
| **[spore](https://github.com/basidiocarp/spore)** | Shared transport and editor primitives |
| **[stipe](https://github.com/basidiocarp/stipe)** | Ecosystem installer and manager |
| **[volva](https://github.com/basidiocarp/volva)** | Execution-host runtime layer |

> **Boundary:** `hymenium` owns the workflow lifecycle — phase gating, dispatch decisions, handoff decomposition, progress monitoring, and retry/recovery. `canopy` owns the coordination ledger that Hymenium reads and writes through. `cortina` owns lifecycle capture. `hyphae` owns memory. `volva` owns agent sessions. `stipe` owns installation.

---

## Quick Start

```bash
# Build or install
cargo install --path .

# Dispatch a handoff to available agents
hymenium dispatch .handoffs/my-feature.md

# Check workflow status (all active workflows)
hymenium status

# Check status of a specific workflow
hymenium status <workflow-id>

# Cancel a running workflow
hymenium cancel <workflow-id>

# Reconcile workflow phases against Canopy task statuses (idempotent)
hymenium reconcile <workflow-id>

# Decompose a large handoff (stub — not yet implemented)
hymenium decompose .handoffs/my-large-feature.md
```

---

## How It Works

```text
Handoff docs / operator         Hymenium                       Canopy + agents
───────────────────────         ────────                       ───────────────
.handoffs/*.md          ──►     parse + decompose
                                     │
                                workflow engine
                                     │
                         gate check: impl phase ready?  ──►  create canopy task
                                     │                        assign implementer
                                     │
                         poll: impl done + diff + verify? ──► gate check: audit ready?
                                     │                        create canopy task
                                     │                        assign auditor
                                     │
                         poll: audit clean?              ──►  close tasks
                                     │                        update handoff
                         stall?  retry.rs closes agent,
                                 relaunches with narrow scope
```

1. **Parse** — reads structured handoff markdown and extracts metadata, scope, and verification requirements.
2. **Decompose** — splits large handoffs into focused child handoffs that fit a single agent's scope.
3. **Dispatch** — creates Canopy tasks and assigns agents based on the workflow template and capability tier.
4. **Gate** — enforces phase transition preconditions; the auditor phase does not start until the implementer phase satisfies all gate conditions.
5. **Monitor** — polls Canopy for task completeness, evaluates gate conditions, and escalates when progress stops.
6. **Recover** — detects stalled agents via heartbeat timeout, closes them through Canopy, and relaunches with a narrowed scope.

---

## What Hymenium Owns

- Workflow lifecycle state — which phase a workflow is in and what transitions are allowed
- Phase gating — enforcing that phase preconditions are met before dispatching the next agent
- Dispatch decisions — reading a handoff, choosing an agent tier, and creating Canopy tasks
- Handoff decomposition — splitting a large handoff into focused child handoffs
- Progress monitoring — polling Canopy state and evaluating completeness gates
- Retry and recovery — detecting stalled agents and relaunching with corrected scope

## What Hymenium Does Not Own

- Task storage and coordination records — handled by `canopy`
- Lifecycle signals and session capture — handled by `cortina`
- Long-term memory and retrieval — handled by `hyphae`
- Agent session management and execution hosting — handled by `volva`
- Installation and ecosystem repair — handled by `stipe`

---

## Key Features

- **Handoff parsing** — reads structured handoff markdown and extracts scope, metadata, and verification requirements into typed values; invalid documents are rejected at intake, not discovered at dispatch time.
- **Automatic decomposition** — splits handoffs that exceed a single agent's scope into focused child handoffs; each child gets a coherent slice of the original work.
- **Workflow templates** — first-class declarative patterns interpreted by the engine; adding a new workflow type does not require engine changes.
- **Phase gating** — enforces transition preconditions in one place (`gate.rs`); the auditor cannot start until the implementer has produced a real diff and verification results.
- **Progress monitoring** — polls Canopy state against completeness conditions; escalates to the operator when a workflow exceeds its timeout without advancing.
- **Retry and recovery** — detects heartbeat timeouts, closes stalled agents through Canopy, and re-enters dispatch with a narrowed scope.

---

## Architecture

```text
hymenium (single binary)
├── src/commands/    CLI subcommand handlers (dispatch, status, cancel, reconcile, ...)
├── src/parser/      handoff intake (markdown → ParsedHandoff)
├── src/decompose/   split large handoffs into focused child handoffs
├── src/workflow/    template engine and phase gate enforcement
├── src/dispatch/    Canopy task creation and agent assignment
├── src/monitor/     progress polling and escalation
├── src/retry.rs     stall detection and recovery
└── src/store.rs     SQLite workflow state persistence
```

```text
hymenium dispatch  <path>           parse and dispatch a handoff to available agents
hymenium status    [<workflow-id>]  report workflow phase and gate conditions (all if omitted)
hymenium cancel    <workflow-id>    cancel a running workflow
hymenium reconcile <workflow-id>    reconcile workflow phases against Canopy task statuses
hymenium decompose <path>           NOT YET IMPLEMENTED — stub only
```

---

## Configuration

```toml
# ~/.config/hymenium/config.toml

[canopy]
socket = ""         # MCP socket path; falls back to CLI if unset

[monitor]
poll_interval_secs = 30
stall_timeout_secs = 300

[log]
level = "warn"
```

---

## Development

```bash
cargo build --release
cargo test
cargo clippy
cargo fmt
```

- Unit tests cover parser, decomposer, gate logic, and retry decisions without live Canopy.
- Integration tests (marked `#[ignore]`) exercise the full Canopy round-trip. Run with `cargo test --ignored` against a running Canopy instance.

## License

MIT — see [LICENSE](LICENSE).
