//! Composition root for the agentd daemon.
//!
//! Exposes the [`config`] module for TOML-based profile configuration. The
//! binary crate (`main.rs`) assembles runner and scheduler from the parsed
//! configuration and starts the daemon.

pub mod config;
pub mod daemon;
pub mod dispatch;
pub mod logging;
mod scheduler;

pub use daemon::{ClientError, DaemonError, request_run, run_daemon_until_shutdown};
pub use dispatch::{
    DispatchError, RunRequest, RunnerSessionExecutor, SessionExecutor, dispatch_run,
};
pub use logging::{
    LogFormat, LoggingError, ResolvedLoggingConfig, configure_tracing, resolve_logging_config,
};
