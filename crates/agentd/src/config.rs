//! TOML configuration parsing and validation for the daemon and agent registry.
//!
//! Bridges operator-facing configuration to daemon dispatch and the runner's
//! [`SessionSpec`] model. Validation here is stricter than the runner's own
//! validation — it enforces uniqueness (no duplicate agent or credential
//! names), non-empty fields, and whitespace hygiene in addition to the
//! runner's format and reservation rules. Relative daemon runtime paths and
//! `methodology_dir` are resolved against the configuration file location when
//! loaded from disk.
//!
//! [`SessionSpec`]: agentd_runner::SessionSpec

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use agentd_runner::{RunnerError, validate_agent_name, validate_environment_name};
use serde::Deserialize;

/// Validated daemon and agent registry parsed from a TOML configuration file.
///
/// Guarantees that daemon runtime paths are present, all agent names are
/// unique and valid, all required fields are non-empty, and all credential
/// names are valid environment variable names. Relative daemon runtime paths
/// and `methodology_dir` paths are resolved against the configuration file's
/// parent directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    daemon: DaemonConfig,
    agents: Vec<AgentConfig>,
}

impl Config {
    /// Reads and parses a TOML configuration file at `path`.
    ///
    /// Relative daemon runtime paths and `methodology_dir` values in the file
    /// are resolved against the directory containing `path`. Returns
    /// [`ConfigError`] on I/O failure, parse failure, or any validation
    /// violation.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        let base_dir = absolute_config_dir(path)?;
        Self::parse(&contents, base_dir.as_deref())
    }

    /// Returns all configured agents.
    pub fn agents(&self) -> &[AgentConfig] {
        &self.agents
    }

    /// Returns the daemon-wide runtime paths.
    pub fn daemon(&self) -> &DaemonConfig {
        &self.daemon
    }

    /// Looks up an agent by name. Returns `None` if no agent matches.
    pub fn agent(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|agent| agent.name == name)
    }

    fn parse(contents: &str, base_dir: Option<&Path>) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(contents)?;
        validate_non_empty("daemon.socket_path", &raw.daemon.socket_path, None, None)?;
        validate_non_empty("daemon.pid_file", &raw.daemon.pid_file, None, None)?;

        if raw.agents.is_empty() {
            return Err(ConfigError::NoAgents);
        }

        let mut seen_agents = HashSet::new();
        let mut agents = Vec::with_capacity(raw.agents.len());

        for raw_agent in raw.agents {
            validate_lookup_key("name", &raw_agent.name, None, None)?;
            if validate_agent_name(&raw_agent.name).is_err() {
                return Err(ConfigError::InvalidAgentName {
                    name: raw_agent.name,
                });
            }

            if !seen_agents.insert(raw_agent.name.clone()) {
                return Err(ConfigError::DuplicateAgentName {
                    name: raw_agent.name,
                });
            }

            validate_non_empty(
                "base_image",
                &raw_agent.base_image,
                Some(raw_agent.name.as_str()),
                None,
            )?;
            validate_non_empty(
                "methodology_dir",
                &raw_agent.methodology_dir,
                Some(raw_agent.name.as_str()),
                None,
            )?;
            let repo_token_source = match raw_agent.repo_token_source {
                Some(value) if value.is_empty() => None,
                Some(value) => {
                    validate_lookup_key(
                        "repo_token_source",
                        &value,
                        Some(raw_agent.name.as_str()),
                        None,
                    )?;
                    Some(value)
                }
                None => None,
            };

            if raw_agent.runa.command.is_empty() {
                return Err(ConfigError::EmptyCommand {
                    agent: raw_agent.name.clone(),
                });
            }

            let mut command = Vec::with_capacity(raw_agent.runa.command.len());
            for element in raw_agent.runa.command {
                validate_non_empty(
                    "runa.command",
                    &element,
                    Some(raw_agent.name.as_str()),
                    None,
                )?;
                command.push(element);
            }

            let mut seen_credentials = HashSet::new();
            let mut credentials = Vec::with_capacity(raw_agent.credentials.len());
            for raw_credential in raw_agent.credentials {
                validate_lookup_key(
                    "credentials.name",
                    &raw_credential.name,
                    Some(raw_agent.name.as_str()),
                    None,
                )?;
                if validate_environment_name(&raw_credential.name).is_err() {
                    return Err(ConfigError::InvalidCredentialName {
                        agent: raw_agent.name.clone(),
                        name: raw_credential.name,
                    });
                }
                validate_non_empty(
                    "credentials.source",
                    &raw_credential.source,
                    Some(raw_agent.name.as_str()),
                    Some(raw_credential.name.as_str()),
                )?;

                if !seen_credentials.insert(raw_credential.name.clone()) {
                    return Err(ConfigError::DuplicateCredentialName {
                        agent: raw_agent.name.clone(),
                        name: raw_credential.name,
                    });
                }

                credentials.push(CredentialConfig {
                    name: raw_credential.name,
                    source: raw_credential.source,
                });
            }

            let methodology_dir = resolve_methodology_dir(base_dir, &raw_agent.methodology_dir);
            agents.push(AgentConfig {
                name: raw_agent.name,
                base_image: raw_agent.base_image,
                methodology_dir,
                repo_token_source,
                credentials,
                runa: RunaConfig { command },
            });
        }

        Ok(Self {
            daemon: DaemonConfig {
                socket_path: resolve_config_path(base_dir, &raw.daemon.socket_path),
                pid_file: resolve_config_path(base_dir, &raw.daemon.pid_file),
            },
            agents,
        })
    }
}

/// Parses a configuration from a TOML string without file-path context.
///
/// Unlike [`Config::load`], relative `methodology_dir` values are preserved
/// as-is because there is no file path to resolve them against.
impl FromStr for Config {
    type Err = ConfigError;

    fn from_str(contents: &str) -> Result<Self, Self::Err> {
        Self::parse(contents, None)
    }
}

/// Configuration for a single agent in the registry.
///
/// Fields are private; use the accessor methods to read values. The agent
/// name has been validated as a legal unix username and is unique within the
/// [`Config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    name: String,
    base_image: String,
    methodology_dir: PathBuf,
    repo_token_source: Option<String>,
    credentials: Vec<CredentialConfig>,
    runa: RunaConfig,
}

impl AgentConfig {
    /// Agent identity string, validated as a legal unix username.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Container image reference for this agent's sessions.
    pub fn base_image(&self) -> &str {
        &self.base_image
    }

    /// Path to the methodology directory. Always absolute when constructed
    /// via [`Config::load`]; preserved as-is (potentially relative) when
    /// constructed via the [`FromStr`] impl.
    pub fn methodology_dir(&self) -> &Path {
        &self.methodology_dir
    }

    /// Optional environment variable name resolved by the daemon at dispatch
    /// time to authenticate the runner-managed `git clone` only. This value is
    /// not injected into the agent runtime environment.
    pub fn repo_token_source(&self) -> Option<&str> {
        self.repo_token_source.as_deref()
    }

    /// Declared credentials for this agent. Each credential's name is a
    /// valid environment variable name, unique within this agent.
    pub fn credentials(&self) -> &[CredentialConfig] {
        &self.credentials
    }

    /// Runa runtime configuration for this agent.
    pub fn runa(&self) -> &RunaConfig {
        &self.runa
    }
}

/// A declared credential for an agent.
///
/// The `name` becomes the environment variable name inside the container.
/// The `source` is the name of an environment variable that the daemon reads
/// from its own process environment before passing the resolved value to the
/// runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialConfig {
    name: String,
    source: String,
}

impl CredentialConfig {
    /// Environment variable name for this credential inside the container.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Environment variable name that the daemon later resolves from its own
    /// process environment.
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Runa runtime configuration for an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunaConfig {
    command: Vec<String>,
}

impl RunaConfig {
    /// Static command array written into `.runa/config.toml` as the
    /// `[agent] command` value. Not a shell command — each element becomes
    /// a TOML string in the array. Must contain at least one element.
    pub fn command(&self) -> &[String] {
        &self.command
    }
}

/// Daemon-wide paths for the operator socket and PID file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    socket_path: PathBuf,
    pid_file: PathBuf,
}

impl DaemonConfig {
    /// Filesystem path for the daemon's local Unix socket.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Filesystem path for the daemon's PID file and advisory lock.
    pub fn pid_file(&self) -> &Path {
        &self.pid_file
    }
}

/// Errors produced when loading or validating a configuration file.
#[derive(Debug)]
pub enum ConfigError {
    /// Failed to read the configuration file from disk.
    Io(std::io::Error),
    /// The file contains invalid TOML or violates the expected schema.
    Parse(toml::de::Error),
    /// The configuration defines zero agents. At least one must be declared.
    NoAgents,
    /// Two agents share the same name.
    DuplicateAgentName { name: String },
    /// An agent name fails the runner's [`validate_agent_name`] rules.
    InvalidAgentName { name: String },
    /// Two credentials within the same agent share a name.
    DuplicateCredentialName { agent: String, name: String },
    /// A credential name fails [`validate_environment_name`] (contains `,`
    /// or `=`, or is the reserved `AGENT_NAME`).
    InvalidCredentialName { agent: String, name: String },
    /// A required string field is empty or whitespace-only.
    EmptyField {
        field: &'static str,
        agent: Option<String>,
        credential: Option<String>,
    },
    /// A lookup-key field has leading or trailing whitespace. Caught
    /// separately from empty because the trimmed value may be valid — the
    /// whitespace itself is the error.
    FieldHasOuterWhitespace {
        field: &'static str,
        agent: Option<String>,
        credential: Option<String>,
    },
    /// The `runa.command` array is empty for an agent.
    EmptyCommand { agent: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(error) => write!(f, "failed to read config: {error}"),
            ConfigError::Parse(error) => write!(f, "invalid config: {error}"),
            ConfigError::NoAgents => write!(f, "config must define at least one agent"),
            ConfigError::DuplicateAgentName { name } => {
                write!(f, "duplicate agent name: {name}")
            }
            ConfigError::InvalidAgentName { name } => {
                write!(
                    f,
                    "invalid agent name '{name}'; {}",
                    RunnerError::InvalidAgentName
                )
            }
            ConfigError::DuplicateCredentialName { agent, name } => {
                write!(
                    f,
                    "agent '{agent}' defines duplicate credential name '{name}'"
                )
            }
            ConfigError::InvalidCredentialName { agent, name } => {
                write!(
                    f,
                    "agent '{agent}' defines invalid credential name '{name}'; credential names must not contain ',' or '=' and must not use reserved name 'AGENT_NAME'"
                )
            }
            ConfigError::EmptyField {
                field,
                agent,
                credential,
            } => {
                write!(f, "field '{field}' must not be empty")?;

                if let Some(agent) = agent {
                    write!(f, " for agent '{agent}'")?;
                }
                if let Some(credential) = credential {
                    write!(f, " credential '{credential}'")?;
                }

                Ok(())
            }
            ConfigError::FieldHasOuterWhitespace {
                field,
                agent,
                credential,
            } => {
                write!(
                    f,
                    "field '{field}' must not have leading or trailing whitespace"
                )?;

                if let Some(agent) = agent {
                    write!(f, " for agent '{agent}'")?;
                }
                if let Some(credential) = credential {
                    write!(f, " credential '{credential}'")?;
                }

                Ok(())
            }
            ConfigError::EmptyCommand { agent } => {
                write!(f, "agent '{agent}' must define a non-empty runa.command")
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Io(error) => Some(error),
            ConfigError::Parse(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(error: toml::de::Error) -> Self {
        Self::Parse(error)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    daemon: RawDaemonConfig,
    #[serde(default)]
    agents: Vec<RawAgentConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDaemonConfig {
    #[serde(default = "default_daemon_socket_path")]
    socket_path: String,
    #[serde(default = "default_daemon_pid_file")]
    pid_file: String,
}

impl Default for RawDaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_daemon_socket_path(),
            pid_file: default_daemon_pid_file(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentConfig {
    name: String,
    base_image: String,
    methodology_dir: String,
    repo_token_source: Option<String>,
    #[serde(default)]
    credentials: Vec<RawCredentialConfig>,
    runa: RawRunaConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCredentialConfig {
    name: String,
    source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRunaConfig {
    command: Vec<String>,
}

fn default_daemon_socket_path() -> String {
    "/run/agentd/agentd.sock".to_string()
}

fn default_daemon_pid_file() -> String {
    "/run/agentd/agentd.pid".to_string()
}

fn validate_non_empty(
    field: &'static str,
    value: &str,
    agent: Option<&str>,
    credential: Option<&str>,
) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::EmptyField {
            field,
            agent: agent.map(str::to_owned),
            credential: credential.map(str::to_owned),
        });
    }

    Ok(())
}

fn validate_lookup_key(
    field: &'static str,
    value: &str,
    agent: Option<&str>,
    credential: Option<&str>,
) -> Result<(), ConfigError> {
    validate_non_empty(field, value, agent, credential)?;

    if value != value.trim() {
        return Err(ConfigError::FieldHasOuterWhitespace {
            field,
            agent: agent.map(str::to_owned),
            credential: credential.map(str::to_owned),
        });
    }

    Ok(())
}

fn absolute_config_dir(path: &Path) -> Result<Option<PathBuf>, ConfigError> {
    let base_dir = path.parent().unwrap_or(Path::new("."));
    let absolute_base_dir = if base_dir.is_absolute() {
        base_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(base_dir)
    };

    Ok(Some(absolute_base_dir))
}

fn resolve_methodology_dir(base_dir: Option<&Path>, methodology_dir: &str) -> PathBuf {
    resolve_config_path(base_dir, methodology_dir)
}

fn resolve_config_path(base_dir: Option<&Path>, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        return path.to_path_buf();
    }

    match base_dir {
        Some(base_dir) => base_dir.join(path),
        None => path.to_path_buf(),
    }
}
