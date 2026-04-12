//! Hymenium: Handoff workflow orchestration for multi-agent systems.
//!
//! This crate provides the core workflow engine for decomposing, dispatching,
//! monitoring, and coordinating work across distributed agents.

pub mod decompose;
pub mod dispatch;
pub mod monitor;
pub mod parser;
pub mod retry;
pub mod store;
pub mod workflow;
