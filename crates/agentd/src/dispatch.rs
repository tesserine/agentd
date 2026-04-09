use std::fmt;

use agentd_runner::{
    ResolvedEnvironmentVariable, RunnerError, SessionInvocation, SessionOutcome, SessionSpec,
    run_session,
};

use crate::config::{Config, ConfigError};

/// Parameters for a daemon run request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub agent: String,
    pub repo_url: String,
    pub work_unit: Option<String>,
}

/// Errors produced while mapping a run request into a runner session.
#[derive(Debug)]
pub enum DispatchError {
    UnknownAgent {
        agent: String,
    },
    MissingCredentialSource {
        agent: String,
        credential: String,
        source: String,
    },
    Config(ConfigError),
    Runner(RunnerError),
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownAgent { agent } => write!(f, "unknown agent '{agent}'"),
            Self::MissingCredentialSource {
                agent,
                credential,
                source,
            } => write!(
                f,
                "agent '{agent}' credential '{credential}' requires daemon environment variable '{source}'"
            ),
            Self::Config(error) => write!(f, "{error}"),
            Self::Runner(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Runner(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ConfigError> for DispatchError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<RunnerError> for DispatchError {
    fn from(error: RunnerError) -> Self {
        Self::Runner(error)
    }
}

/// Trait boundary used by daemon dispatch so tests can replace the runner.
pub trait SessionExecutor {
    fn run_session(
        &self,
        spec: SessionSpec,
        invocation: SessionInvocation,
    ) -> Result<SessionOutcome, RunnerError>;
}

/// Production executor that forwards directly into `agentd-runner`.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunnerSessionExecutor;

impl SessionExecutor for RunnerSessionExecutor {
    fn run_session(
        &self,
        spec: SessionSpec,
        invocation: SessionInvocation,
    ) -> Result<SessionOutcome, RunnerError> {
        run_session(spec, invocation)
    }
}

/// Resolve a named agent plus run request into a runner session and run it.
pub fn dispatch_run(
    config: &Config,
    request: &RunRequest,
    executor: &impl SessionExecutor,
) -> Result<SessionOutcome, DispatchError> {
    let agent = config
        .agent(&request.agent)
        .ok_or_else(|| DispatchError::UnknownAgent {
            agent: request.agent.clone(),
        })?;
    let daemon_instance_id = config.daemon().daemon_instance_id()?;

    let environment = agent
        .credentials()
        .iter()
        .map(|credential| {
            let value = std::env::var(credential.source()).map_err(|_| {
                DispatchError::MissingCredentialSource {
                    agent: agent.name().to_string(),
                    credential: credential.name().to_string(),
                    source: credential.source().to_string(),
                }
            })?;

            Ok(ResolvedEnvironmentVariable {
                name: credential.name().to_string(),
                value,
            })
        })
        .collect::<Result<Vec<_>, DispatchError>>()?;

    let repo_token = agent
        .repo_token_source()
        .and_then(|source| std::env::var(source).ok())
        .filter(|value| !value.is_empty());

    executor
        .run_session(
            SessionSpec {
                daemon_instance_id,
                agent_name: agent.name().to_string(),
                base_image: agent.base_image().to_string(),
                methodology_dir: agent.methodology_dir().to_path_buf(),
                agent_command: agent.runa().command().to_vec(),
                environment,
            },
            SessionInvocation {
                repo_url: request.repo_url.clone(),
                repo_token,
                work_unit: request.work_unit.clone(),
                timeout: None,
            },
        )
        .map_err(DispatchError::from)
}
