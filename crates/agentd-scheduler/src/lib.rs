//! Job scheduling primitives for agentd.
//!
//! This crate will own scheduling policy and timing — determining when agents
//! run and with what mission context, then dispatching run requests through
//! the daemon's Unix socket using the same intake path as manual invocation.
//! The scheduler does not call `agentd-runner` directly. Currently a
//! placeholder pending scheduler implementation.
//!
//! See `ARCHITECTURE.md` section "Session Lifecycle" for the design-level
//! treatment.
