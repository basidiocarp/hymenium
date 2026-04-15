//! Hymenium: Handoff workflow orchestration for multi-agent systems.
//!
//! This crate provides the core workflow engine for decomposing, dispatching,
//! monitoring, and coordinating work across distributed agents.

pub mod commands;
pub mod context;
pub mod decompose;
pub mod dispatch;
pub mod failure;
pub mod monitor;
pub mod outcome;
pub mod outcomes;
pub mod parser;
pub mod retry;
pub mod store;
pub mod workflow;
