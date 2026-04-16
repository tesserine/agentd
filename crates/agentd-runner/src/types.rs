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

/// Static configuration for a session, derived from a profile.
///
/// Describes the profile identity, container image, methodology, command, and
/// caller-resolved environment variables. Validated by
/// [`run_session`](crate::run_session) before any resources are allocated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    /// Stable daemon-instance identifier used in runner-managed Podman
    /// resource names so startup reconciliation can scope ownership to one
    /// daemon instance.
    pub daemon_instance_id: String,
    /// Profile identity string. Doubles as the in-container unix username, a
    /// component of the container name, and the value of the `PROFILE_NAME`
    /// environment variable. Must pass
    /// [`validate_profile_name`](crate::validate_profile_name).
    pub profile_name: String,
    /// Container image reference. The image must provide `/bin/sh`, `git`,
    /// and the setup/session binaries required by the configured profile
    /// command in `PATH`, including `useradd` and `gosu`.
    pub base_image: String,
    /// Host-side path to the methodology directory. Mounted read-only into
    /// the container at `/agentd/methodology`. Must contain `manifest.toml`.
    pub methodology_dir: PathBuf,
    /// Additional host bind mounts declared by the selected profile.
    pub mounts: Vec<BindMount>,
    /// Command array executed directly from the cloned repository after
    /// workspace setup. Not a shell command unless the profile explicitly
    /// configures one (for example, `["/bin/sh", "-lc", "..."]`).
    pub command: Vec<String>,
    /// Caller-resolved environment variables injected into the container.
    /// Non-empty values are passed via ephemeral podman secrets; empty values
    /// are passed as direct `--env` assignments.
    pub environment: Vec<ResolvedEnvironmentVariable>,
}

/// A host bind mount declared by a session profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindMount {
    /// Host-side source path to bind into the container.
    pub source: PathBuf,
    /// Absolute in-container target path for the bind mount. Must not contain
    /// `.` or `..` components or `,`.
    pub target: PathBuf,
    /// Whether the mount is read-only inside the container.
    pub read_only: bool,
}

/// A name-value pair representing an environment variable whose value has
/// already been resolved by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEnvironmentVariable {
    /// Variable name. Must not be empty, contain `,` or `=`, or collide with
    /// runner-managed names (currently `PROFILE_NAME`).
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
    /// Optional work unit identifier exposed to the session command through
    /// the runner-managed `AGENTD_WORK_UNIT` environment variable when set.
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
    Success { exit_code: i32 },
    /// The session failed without a more specific semantic classification.
    GenericFailure { exit_code: i32 },
    /// The caller invoked the runtime incorrectly.
    UsageError { exit_code: i32 },
    /// Actionable work exists, but execution is blocked on prerequisites.
    Blocked { exit_code: i32 },
    /// No actionable work is currently available.
    NothingReady { exit_code: i32 },
    /// Work was attempted but required completion checks failed.
    WorkFailed { exit_code: i32 },
    /// The runtime or environment failed independently of the work item.
    InfrastructureFailure { exit_code: i32 },
    /// The configured command was found but could not be executed.
    CommandNotExecutable { exit_code: i32 },
    /// The configured command was not found.
    CommandNotFound { exit_code: i32 },
    /// The process terminated from signal `signal`, preserving `128 + signal`.
    TerminatedBySignal { exit_code: i32, signal: i32 },
    /// The session exceeded its timeout and was force-removed.
    TimedOut,
}

impl SessionOutcome {
    /// Interpret a process exit code according to the shared commons contract.
    pub fn from_exit_code(exit_code: i32) -> Self {
        match exit_code {
            0 => Self::Success { exit_code },
            1 => Self::GenericFailure { exit_code },
            2 => Self::UsageError { exit_code },
            3 => Self::Blocked { exit_code },
            4 => Self::NothingReady { exit_code },
            5 => Self::WorkFailed { exit_code },
            6 => Self::InfrastructureFailure { exit_code },
            126 => Self::CommandNotExecutable { exit_code },
            127 => Self::CommandNotFound { exit_code },
            129.. => Self::TerminatedBySignal {
                exit_code,
                signal: exit_code - 128,
            },
            _ => Self::GenericFailure { exit_code },
        }
    }

    /// Canonical snake_case semantic label for this outcome.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Success { .. } => "success",
            Self::GenericFailure { .. } => "generic_failure",
            Self::UsageError { .. } => "usage_error",
            Self::Blocked { .. } => "blocked",
            Self::NothingReady { .. } => "nothing_ready",
            Self::WorkFailed { .. } => "work_failed",
            Self::InfrastructureFailure { .. } => "infrastructure_failure",
            Self::CommandNotExecutable { .. } => "command_not_executable",
            Self::CommandNotFound { .. } => "command_not_found",
            Self::TerminatedBySignal { .. } => "terminated_by_signal",
            Self::TimedOut => "timed_out",
        }
    }

    /// Raw process exit code when this outcome came from process termination.
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Self::Success { exit_code }
            | Self::GenericFailure { exit_code }
            | Self::UsageError { exit_code }
            | Self::Blocked { exit_code }
            | Self::NothingReady { exit_code }
            | Self::WorkFailed { exit_code }
            | Self::InfrastructureFailure { exit_code }
            | Self::CommandNotExecutable { exit_code }
            | Self::CommandNotFound { exit_code } => Some(*exit_code),
            Self::TerminatedBySignal { exit_code, .. } => Some(*exit_code),
            Self::TimedOut => None,
        }
    }

    /// Signal number when this outcome represents signal-derived termination.
    pub fn signal(&self) -> Option<i32> {
        match self {
            Self::TerminatedBySignal { signal, .. } => Some(*signal),
            _ => None,
        }
    }

    /// Whether the CLI should treat this terminal outcome as process success.
    pub fn is_cli_success(&self) -> bool {
        matches!(
            self,
            Self::Success { .. } | Self::Blocked { .. } | Self::NothingReady { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::SessionOutcome;

    #[test]
    fn exit_code_128_is_generic_failure() {
        assert_eq!(
            SessionOutcome::from_exit_code(128),
            SessionOutcome::GenericFailure { exit_code: 128 }
        );
    }

    #[test]
    fn signal_derived_exit_codes_above_128_remain_signal_terminations() {
        assert_eq!(
            SessionOutcome::from_exit_code(130),
            SessionOutcome::TerminatedBySignal {
                exit_code: 130,
                signal: 2,
            }
        );
    }
}

/// Summary of what startup reconciliation removed before the daemon accepted
/// any new sessions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartupReconciliationReport {
    /// Stale runner-managed session containers matching
    /// `agentd-{daemon8}-{profile}-{session16}` that were removed during startup.
    pub removed_container_names: Vec<String>,
    /// Orphaned runner-managed secrets matching `agentd-{daemon8}-{session16}-{suffix}`
    /// that were removed during startup.
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

/// Error returned by [`validate_profile_name`](crate::validate_profile_name)
/// when a name violates naming rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileNameValidationError {
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
/// Validation errors ([`InvalidProfileName`](Self::InvalidProfileName),
/// [`InvalidBaseImage`](Self::InvalidBaseImage), etc.) are returned before
/// any resources are allocated. Resource and execution errors
/// ([`MissingMethodologyManifest`](Self::MissingMethodologyManifest),
/// [`Io`](Self::Io), [`PodmanCommandFailed`](Self::PodmanCommandFailed))
/// occur after resource allocation has begun.
#[derive(Debug)]
pub enum RunnerError {
    /// The daemon-instance identifier must be exactly eight lowercase hex
    /// characters.
    InvalidDaemonInstanceId,
    /// The methodology directory does not contain `manifest.toml`. Produced
    /// during resource allocation, after spec and invocation validation pass.
    MissingMethodologyManifest { path: PathBuf },
    /// The profile name fails unix username rules or matches a reserved system
    /// name. Produced during spec validation.
    InvalidProfileName,
    /// The base image string is empty or has surrounding whitespace. Produced
    /// during spec validation.
    InvalidBaseImage,
    /// The repository URL is not a supported remote form (`https://`,
    /// `http://`, `git://`), embeds credentials, or is paired with
    /// `repo_token` without using `https://`. Produced during invocation
    /// validation.
    InvalidRepoUrl { message: String },
    /// The command array is empty or contains an empty element.
    /// Produced during spec validation.
    InvalidCommand,
    /// An environment variable name is empty or contains `,` or `=`.
    /// Produced during spec validation.
    InvalidEnvironmentName { name: String },
    /// An environment variable name collides with a runner-managed name.
    /// Produced during spec validation.
    ReservedEnvironmentName { name: String },
    /// A configured bind mount source path is not absolute.
    InvalidMountSource { path: PathBuf },
    /// A configured bind mount target path is not absolute, contains `.` or
    /// `..` components, or contains `,`.
    InvalidMountTarget { path: PathBuf },
    /// Two configured bind mounts share the same target path.
    DuplicateMountTarget { target: PathBuf },
    /// A configured bind mount target collides with a runner-managed mount.
    ReservedMountTarget { target: PathBuf },
    /// Filesystem failure, process I/O failure, or invalid external command
    /// output received after a successful process exit.
    Io(std::io::Error),
    /// A configured bind mount source path does not exist.
    MissingMountSource { path: PathBuf },
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
            RunnerError::InvalidDaemonInstanceId => {
                write!(
                    f,
                    "daemon_instance_id must be exactly 8 lowercase hex characters"
                )
            }
            RunnerError::MissingMethodologyManifest { path } => {
                write!(
                    f,
                    "methodology directory must contain manifest.toml: {}",
                    path.display()
                )
            }
            RunnerError::InvalidProfileName => write!(
                f,
                "profile_name must already be a unix username starting with a lowercase letter, containing only lowercase letters, digits, '_', or '-', be at most 32 characters, and not be one of the reserved system names root, nobody, daemon, bin, sys, man, or mail"
            ),
            RunnerError::InvalidBaseImage => write!(f, "base_image must not be empty"),
            RunnerError::InvalidRepoUrl { message } => write!(f, "repo_url {message}"),
            RunnerError::InvalidCommand => {
                write!(f, "command must contain at least one argument")
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
            RunnerError::InvalidMountSource { path } => {
                write!(
                    f,
                    "mount source must be an absolute path: {}",
                    path.display()
                )
            }
            RunnerError::InvalidMountTarget { path } => {
                write!(
                    f,
                    "mount target must be an absolute path without '.' or '..' components or ',': {}",
                    path.display()
                )
            }
            RunnerError::DuplicateMountTarget { target } => {
                write!(f, "mount targets must be unique: {}", target.display())
            }
            RunnerError::ReservedMountTarget { target } => {
                write!(
                    f,
                    "mount target is reserved by the runner: {}",
                    target.display()
                )
            }
            RunnerError::Io(error) => write!(f, "{error}"),
            RunnerError::MissingMountSource { path } => {
                write!(f, "mount source path does not exist: {}", path.display())
            }
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
