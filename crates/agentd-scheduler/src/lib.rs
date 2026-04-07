//! Job scheduling primitives for agentd.
//!
//! This crate will own scheduling policy and timing — determining when agents
//! run and with what mission context, then handing identity and invocation
//! parameters to `agentd-runner` for execution. Currently a placeholder
//! pending scheduler implementation.
//!
//! See `ARCHITECTURE.md` section "Scheduler" for the design-level treatment.
