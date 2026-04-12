# Changelog

All notable changes to Hymenium are documented in this file.

## [Unreleased]

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
