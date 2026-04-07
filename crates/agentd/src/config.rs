use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use agentd_runner::{RunnerError, validate_agent_name, validate_environment_name};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    agents: Vec<AgentConfig>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        let base_dir = absolute_config_dir(path)?;
        Self::parse(&contents, base_dir.as_deref())
    }

    pub fn agents(&self) -> &[AgentConfig] {
        &self.agents
    }

    pub fn agent(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|agent| agent.name == name)
    }

    fn parse(contents: &str, base_dir: Option<&Path>) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(contents)?;

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
                credentials,
                runa: RunaConfig { command },
            });
        }

        Ok(Self { agents })
    }
}

impl FromStr for Config {
    type Err = ConfigError;

    fn from_str(contents: &str) -> Result<Self, Self::Err> {
        Self::parse(contents, None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    name: String,
    base_image: String,
    methodology_dir: PathBuf,
    credentials: Vec<CredentialConfig>,
    runa: RunaConfig,
}

impl AgentConfig {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn base_image(&self) -> &str {
        &self.base_image
    }

    pub fn methodology_dir(&self) -> &Path {
        &self.methodology_dir
    }

    pub fn credentials(&self) -> &[CredentialConfig] {
        &self.credentials
    }

    pub fn runa(&self) -> &RunaConfig {
        &self.runa
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialConfig {
    name: String,
    source: String,
}

impl CredentialConfig {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source(&self) -> &str {
        &self.source
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunaConfig {
    command: Vec<String>,
}

impl RunaConfig {
    pub fn command(&self) -> &[String] {
        &self.command
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    NoAgents,
    DuplicateAgentName {
        name: String,
    },
    InvalidAgentName {
        name: String,
    },
    DuplicateCredentialName {
        agent: String,
        name: String,
    },
    InvalidCredentialName {
        agent: String,
        name: String,
    },
    EmptyField {
        field: &'static str,
        agent: Option<String>,
        credential: Option<String>,
    },
    FieldHasOuterWhitespace {
        field: &'static str,
        agent: Option<String>,
        credential: Option<String>,
    },
    EmptyCommand {
        agent: String,
    },
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
    agents: Vec<RawAgentConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentConfig {
    name: String,
    base_image: String,
    methodology_dir: String,
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
    match base_dir {
        Some(base_dir) => base_dir.join(methodology_dir),
        None => PathBuf::from(methodology_dir),
    }
}
