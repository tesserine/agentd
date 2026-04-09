//! Public data types for the session lifecycle.
//!
//! Defines the inputs ([`SessionSpec`], [`SessionInvocation`]), outputs
//! ([`SessionOutcome`]), and error types ([`RunnerError`]) that
//! [`run_session`](crate::run_session) operates on. Validation error types
//! for the standalone validators are also defined here.

use std::fmt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

/// Static configuration for an agent session.
///
/// Describes the agent's identity, container image, methodology, command, and
/// caller-resolved environment variables. Validated by
/// [`run_session`](crate::run_session) before any resources are allocated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    /// Agent identity string. Doubles as the in-container unix username, a
    /// component of the container name, and the value of the `AGENT_NAME`
    /// environment variable. Must pass
    /// [`validate_agent_name`](crate::validate_agent_name).
    pub agent_name: String,
    /// Container image reference. The image must provide `/bin/sh`, `git`,
    /// `useradd`, `gosu`, and `runa` in `PATH`.
    pub base_image: String,
    /// Host-side path to the methodology directory. Mounted read-only into
    /// the container at `/agentd/methodology`. Must contain `manifest.toml`.
    pub methodology_dir: PathBuf,
    /// Command array written into `.runa/config.toml` as the
    /// `[agent] command` value. Not a shell command — each element becomes a
    /// TOML string in the array.
    pub agent_command: Vec<String>,
    /// Caller-resolved environment variables injected into the container.
    /// Non-empty values are passed via ephemeral podman secrets; empty values
    /// are passed as direct `--env` assignments.
    pub environment: Vec<ResolvedEnvironmentVariable>,
}

/// A name-value pair representing an environment variable whose value has
/// already been resolved by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEnvironmentVariable {
    /// Variable name. Must not be empty, contain `,` or `=`, or collide with
    /// runner-managed names (currently `AGENT_NAME`).
    pub name: String,
    /// Variable value. Empty values are legal and are injected as direct
    /// `--env NAME=` assignments rather than podman secrets, which reject
    /// zero-byte payloads.
    pub value: String,
}

/// Per-invocation parameters for a session launch.
///
/// Describes the repository to clone, optional clone-only repository
/// authentication, an optional work unit, and an optional timeout. Validated
/// by [`run_session`](crate::run_session) before any resources are allocated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInvocation {
    /// Remote repository URL cloned into the container workspace. Must use
    /// `https://`, `http://`, or `git://` scheme. Credential-bearing URLs
    /// are rejected. When [`Self::repo_token`] is present, this must use
    /// `https://`.
    pub repo_url: String,
    /// Optional bearer token used only for the runner-managed `git clone`
    /// request for `https://` repository URLs. This token is not passed
    /// through to the agent runtime.
    pub repo_token: Option<String>,
    /// Optional work unit identifier passed as `--work-unit` to `runa run`.
    pub work_unit: Option<String>,
    /// Optional session timeout. When set, the runner force-removes the
    /// container after this duration and returns
    /// [`SessionOutcome::TimedOut`].
    pub timeout: Option<Duration>,
}

/// Terminal outcome of a completed session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutcome {
    /// The container process exited with code 0.
    Succeeded,
    /// The container process exited with a non-zero code.
    Failed { exit_code: i32 },
    /// The session exceeded its timeout and was force-removed.
    TimedOut,
}

/// Summary of what startup reconciliation removed before the daemon accepted
/// any new sessions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartupReconciliationReport {
    /// Stale runner-managed session containers that were removed during startup.
    pub removed_container_names: Vec<String>,
    /// Orphaned `agentd-secret-*` secrets that were removed during startup.
    pub removed_secret_names: Vec<String>,
}

/// Error returned by
/// [`validate_environment_name`](crate::validate_environment_name) when a
/// name violates naming rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentNameValidationError {
    /// The name is empty or contains `,` or `=`.
    Invalid,
    /// The name collides with a runner-managed environment variable.
    Reserved,
}

/// Error returned by [`validate_agent_name`](crate::validate_agent_name)
/// when a name violates naming rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentNameValidationError {
    /// The name is not a valid unix username: must start with a lowercase
    /// letter, contain only lowercase letters, digits, `_`, or `-`, and be
    /// at most 32 characters.
    Invalid,
    /// The name matches a reserved system username (`root`, `nobody`,
    /// `daemon`, `bin`, `sys`, `man`, `mail`).
    Reserved,
}

/// Errors produced during session execution.
///
/// Validation errors ([`InvalidAgentName`](Self::InvalidAgentName),
/// [`InvalidBaseImage`](Self::InvalidBaseImage), etc.) are returned before
/// any resources are allocated. Resource and execution errors
/// ([`MissingMethodologyManifest`](Self::MissingMethodologyManifest),
/// [`Io`](Self::Io), [`PodmanCommandFailed`](Self::PodmanCommandFailed))
/// occur after resource allocation has begun.
#[derive(Debug)]
pub enum RunnerError {
    /// The methodology directory does not contain `manifest.toml`. Produced
    /// during resource allocation, after spec and invocation validation pass.
    MissingMethodologyManifest { path: PathBuf },
    /// The agent name fails unix username rules or matches a reserved system
    /// name. Produced during spec validation.
    InvalidAgentName,
    /// The base image string is empty or has surrounding whitespace. Produced
    /// during spec validation.
    InvalidBaseImage,
    /// The repository URL is not a supported remote form (`https://`,
    /// `http://`, `git://`), embeds credentials, or is paired with
    /// `repo_token` without using `https://`. Produced during invocation
    /// validation.
    InvalidRepoUrl { message: String },
    /// The agent command array is empty or contains an empty element.
    /// Produced during spec validation.
    InvalidAgentCommand,
    /// An environment variable name is empty or contains `,` or `=`.
    /// Produced during spec validation.
    InvalidEnvironmentName { name: String },
    /// An environment variable name collides with a runner-managed name.
    /// Produced during spec validation.
    ReservedEnvironmentName { name: String },
    /// Filesystem failure, process I/O failure, or invalid external command
    /// output received after a successful process exit.
    Io(std::io::Error),
    /// A podman CLI invocation returned a non-zero exit status. Captures the
    /// argument list, exit status, and stderr for diagnostics.
    PodmanCommandFailed {
        args: Vec<String>,
        status: ExitStatus,
        stderr: String,
    },
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunnerError::MissingMethodologyManifest { path } => {
                write!(
                    f,
                    "methodology directory must contain manifest.toml: {}",
                    path.display()
                )
            }
            RunnerError::InvalidAgentName => write!(
                f,
                "agent_name must already be a unix username starting with a lowercase letter, containing only lowercase letters, digits, '_', or '-', be at most 32 characters, and not be one of the reserved system names root, nobody, daemon, bin, sys, man, or mail"
            ),
            RunnerError::InvalidBaseImage => write!(f, "base_image must not be empty"),
            RunnerError::InvalidRepoUrl { message } => write!(f, "repo_url {message}"),
            RunnerError::InvalidAgentCommand => {
                write!(f, "agent_command must contain at least one argument")
            }
            RunnerError::InvalidEnvironmentName { name } => write!(
                f,
                "environment variable names must not be empty and must not contain ',' or '=': {name}"
            ),
            RunnerError::ReservedEnvironmentName { name } => {
                write!(
                    f,
                    "environment variable name is reserved by the runner: {name}"
                )
            }
            RunnerError::Io(error) => write!(f, "{error}"),
            RunnerError::PodmanCommandFailed {
                args,
                status,
                stderr,
            } => write!(
                f,
                "podman {} failed with status {}: {}",
                args.join(" "),
                exit_status_label(status),
                stderr.trim()
            ),
        }
    }
}

impl std::error::Error for RunnerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunnerError::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RunnerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

fn exit_status_label(status: &ExitStatus) -> String {
    status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string())
}
