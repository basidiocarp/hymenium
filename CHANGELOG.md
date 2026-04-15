# Changelog

All notable changes to Hymenium are documented in this file.

## [Unreleased]

## [0.1.2] - 2026-04-14

### Added

- **Context compression pipeline**: Hymenium now exposes a pluggable
  `ContextEngine` with budget-aware pruning, focus-topic biasing, and
  tool-pair sanitization.

### Fixed

- **Dispatch overflow handling**: dispatch now retries with compressed context
  when the rendered parent-task surface exceeds budget instead of re-expanding
  the original payload.
- **Verifier reliability**: the context compression handoff verifier now runs
  to completion under `set -e`.

## [0.1.1] - 2026-04-14

### Fixed

- **CLI version surface**: `hymenium --version` now works, so external installers
  and verification steps can validate the binary without falling through to a
  subcommand error.

## [0.1.0] - 2026-04-11

### Added

- **Handoff parser**: reads structured handoff markdown and extracts metadata,
  scope, steps, verification blocks, and paste markers into typed values.
- **Decomposition engine**: splits large handoffs into focused child handoffs
  using project grouping, union-find dependency merging, and effort-based
  packing.
- **Workflow engine**: state machine for phase transitions with typed phase
  status, started/completed timestamps, and retry tracking.
- **Phase gating**: `GateCondition` evaluator with `CodeDiffExists`,
  `VerificationPassed`, `AuditClean`, and `FindingsResolved` conditions.
  Enforced before any dispatch.
- **Workflow templates**: declarative `impl_audit_default` pattern with
  implement and audit phases, entry/exit gates, and agent role/tier
  assignments. `TemplateRegistry` for named template lookup.
- **Dispatch layer**: `CanopyClient` trait with `CliCanopyClient` (shells out
  to canopy CLI) and `MockCanopyClient` (in-memory for testing). Orchestration
  creates tasks, assigns agents by tier, and guards against empty templates
  and empty slugs.
- **Progress monitoring**: `check_progress` evaluates heartbeat timeout, code
  diff existence, and paste marker progress. `handle_signal` routes signals
  to recovery actions.
- **Retry and recovery**: `decide_recovery` with progressive escalation — plain
  retry, narrowed scope, tier escalation, and operator escalation. Configurable
  via `RetryPolicy`.
- **SQLite store**: placeholder for workflow state persistence.
- **121 tests**: parser correctness, gate evaluation, retry decision trees,
  decomposition edge cases, dispatch orchestration, and monitor signal
  handling.
