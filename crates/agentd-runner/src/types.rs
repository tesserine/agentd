use std::fmt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    pub agent_name: String,
    pub base_image: String,
    pub methodology_dir: PathBuf,
    pub agent_command: Vec<String>,
    pub environment: Vec<ResolvedEnvironmentVariable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEnvironmentVariable {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInvocation {
    pub repo_url: String,
    pub work_unit: Option<String>,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutcome {
    Succeeded,
    Failed { exit_code: i32 },
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentNameValidationError {
    Invalid,
    Reserved,
}

#[derive(Debug)]
pub enum RunnerError {
    MissingMethodologyManifest {
        path: PathBuf,
    },
    InvalidAgentName,
    InvalidBaseImage,
    InvalidRepoUrl {
        message: String,
    },
    InvalidAgentCommand,
    InvalidEnvironmentName {
        name: String,
    },
    ReservedEnvironmentName {
        name: String,
    },
    Io(std::io::Error),
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
                "agent_name must already be a unix username starting with a lowercase letter, containing only lowercase letters, digits, or '-', be at most 32 characters, and not be one of the reserved system names root, nobody, daemon, bin, sys, man, or mail"
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
