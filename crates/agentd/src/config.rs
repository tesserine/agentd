//! TOML configuration parsing and validation for the daemon and profile registry.
//!
//! Bridges operator-facing configuration to daemon dispatch and the runner's
//! [`SessionSpec`] model. Validation here is stricter than the runner's own
//! validation — it enforces uniqueness (no duplicate profile or credential
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

use agentd_runner::{
    BindMount, MountTargetValidationError, RunnerError, validate_environment_name,
    validate_mount_overlap, validate_mount_target, validate_profile_name, validate_repo_url,
};
use croner::parser::{CronParser, Seconds, Year};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Validated daemon and profile registry parsed from a TOML configuration file.
///
/// Guarantees that daemon runtime paths are present, all profile names are
/// unique and valid, all required fields are non-empty, and all credential
/// names are valid environment variable names. Relative daemon runtime paths
/// and `methodology_dir` paths are resolved against the configuration file's
/// parent directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    daemon: DaemonConfig,
    profiles: Vec<ProfileConfig>,
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

    /// Returns all configured profiles.
    pub fn profiles(&self) -> &[ProfileConfig] {
        &self.profiles
    }

    /// Returns the daemon-wide runtime paths.
    pub fn daemon(&self) -> &DaemonConfig {
        &self.daemon
    }

    /// Looks up a profile by name. Returns `None` if no profile matches.
    pub fn profile(&self, name: &str) -> Option<&ProfileConfig> {
        self.profiles.iter().find(|profile| profile.name == name)
    }

    fn parse(contents: &str, base_dir: Option<&Path>) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(contents)?;
        let daemon = DaemonConfig::parse(raw.daemon, base_dir)?;

        if raw.profiles.is_empty() {
            return Err(ConfigError::NoProfiles);
        }

        let mut seen_profiles = HashSet::new();
        let mut profiles = Vec::with_capacity(raw.profiles.len());

        for raw_profile in raw.profiles {
            validate_lookup_key("name", &raw_profile.name, None, None)?;
            if validate_profile_name(&raw_profile.name).is_err() {
                return Err(ConfigError::InvalidProfileName {
                    name: raw_profile.name,
                });
            }

            if !seen_profiles.insert(raw_profile.name.clone()) {
                return Err(ConfigError::DuplicateProfileName {
                    name: raw_profile.name,
                });
            }

            validate_non_empty(
                "base_image",
                &raw_profile.base_image,
                Some(raw_profile.name.as_str()),
                None,
            )?;
            validate_non_empty(
                "methodology_dir",
                &raw_profile.methodology_dir,
                Some(raw_profile.name.as_str()),
                None,
            )?;
            let repo = match raw_profile.repo {
                Some(value) => {
                    validate_lookup_key("repo", &value, Some(raw_profile.name.as_str()), None)?;
                    validate_repo_url(&value).map_err(|error| ConfigError::InvalidRepo {
                        profile: raw_profile.name.clone(),
                        message: error.to_string(),
                    })?;
                    Some(value)
                }
                None => None,
            };
            let schedule = match raw_profile.schedule {
                Some(value) => {
                    validate_lookup_key("schedule", &value, Some(raw_profile.name.as_str()), None)?;
                    validate_schedule(&value).map_err(|_| ConfigError::InvalidSchedule {
                        profile: raw_profile.name.clone(),
                        schedule: value.clone(),
                    })?;
                    Some(value)
                }
                None => None,
            };
            let repo_token_source = match raw_profile.repo_token_source {
                Some(value) if value.is_empty() => None,
                Some(value) => {
                    validate_lookup_key(
                        "repo_token_source",
                        &value,
                        Some(raw_profile.name.as_str()),
                        None,
                    )?;
                    Some(value)
                }
                None => None,
            };

            if raw_profile.command.is_empty() {
                return Err(ConfigError::EmptyCommand {
                    profile: raw_profile.name.clone(),
                });
            }

            let mut command = Vec::with_capacity(raw_profile.command.len());
            for element in raw_profile.command {
                validate_non_empty("command", &element, Some(raw_profile.name.as_str()), None)?;
                command.push(element);
            }

            let mut seen_mount_targets = HashSet::new();
            let mut mounts = Vec::with_capacity(raw_profile.mounts.len());
            for raw_mount in raw_profile.mounts {
                validate_lookup_key(
                    "mounts.source",
                    &raw_mount.source,
                    Some(raw_profile.name.as_str()),
                    None,
                )?;
                validate_lookup_key(
                    "mounts.target",
                    &raw_mount.target,
                    Some(raw_profile.name.as_str()),
                    None,
                )?;

                let source = PathBuf::from(&raw_mount.source);
                if !source.is_absolute() {
                    return Err(ConfigError::MountSourceMustBeAbsolute {
                        profile: raw_profile.name.clone(),
                        source,
                    });
                }

                let target = PathBuf::from(&raw_mount.target);
                if let Err(error) = validate_mount_target(&target, &raw_profile.name) {
                    return Err(ConfigError::InvalidMountTarget {
                        profile: raw_profile.name.clone(),
                        error,
                    });
                }

                if !seen_mount_targets.insert(target.clone()) {
                    return Err(ConfigError::DuplicateMountTarget {
                        profile: raw_profile.name.clone(),
                        target,
                    });
                }

                mounts.push(ProfileMountConfig {
                    source,
                    target,
                    read_only: raw_mount.read_only,
                });
            }
            let runner_mounts = mounts
                .iter()
                .map(ProfileMountConfig::to_runner_mount)
                .collect::<Vec<_>>();
            if let Err(error) = validate_mount_overlap(&runner_mounts) {
                return Err(ConfigError::OverlappingMountTargets {
                    profile: raw_profile.name.clone(),
                    first: error.first,
                    second: error.second,
                });
            }

            let mut seen_credentials = HashSet::new();
            let mut credentials = Vec::with_capacity(raw_profile.credentials.len());
            for raw_credential in raw_profile.credentials {
                validate_lookup_key(
                    "credentials.name",
                    &raw_credential.name,
                    Some(raw_profile.name.as_str()),
                    None,
                )?;
                if validate_environment_name(&raw_credential.name).is_err() {
                    return Err(ConfigError::InvalidCredentialName {
                        profile: raw_profile.name.clone(),
                        name: raw_credential.name,
                    });
                }
                validate_non_empty(
                    "credentials.source",
                    &raw_credential.source,
                    Some(raw_profile.name.as_str()),
                    Some(raw_credential.name.as_str()),
                )?;

                if !seen_credentials.insert(raw_credential.name.clone()) {
                    return Err(ConfigError::DuplicateCredentialName {
                        profile: raw_profile.name.clone(),
                        name: raw_credential.name,
                    });
                }

                credentials.push(CredentialConfig {
                    name: raw_credential.name,
                    source: raw_credential.source,
                });
            }

            if schedule.is_some() && repo.is_none() {
                return Err(ConfigError::ScheduleRequiresRepo {
                    profile: raw_profile.name.clone(),
                });
            }

            let methodology_dir = resolve_methodology_dir(base_dir, &raw_profile.methodology_dir);
            profiles.push(ProfileConfig {
                name: raw_profile.name,
                base_image: raw_profile.base_image,
                methodology_dir,
                mounts,
                repo,
                schedule,
                repo_token_source,
                credentials,
                command,
            });
        }

        Ok(Self { daemon, profiles })
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

/// Configuration for a single profile in the registry.
///
/// Fields are private; use the accessor methods to read values. The profile
/// name has been validated as a legal unix username and is unique within the
/// [`Config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileConfig {
    name: String,
    base_image: String,
    methodology_dir: PathBuf,
    mounts: Vec<ProfileMountConfig>,
    repo: Option<String>,
    schedule: Option<String>,
    repo_token_source: Option<String>,
    credentials: Vec<CredentialConfig>,
    command: Vec<String>,
}

impl ProfileConfig {
    /// Profile identity string, validated as a legal unix username.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Container image reference for this profile's sessions.
    pub fn base_image(&self) -> &str {
        &self.base_image
    }

    /// Path to the methodology directory. Always absolute when constructed
    /// via [`Config::load`]; preserved as-is (potentially relative) when
    /// constructed via the [`FromStr`] impl.
    pub fn methodology_dir(&self) -> &Path {
        &self.methodology_dir
    }

    /// Additional bind mounts declared for this profile.
    pub fn mounts(&self) -> &[ProfileMountConfig] {
        &self.mounts
    }

    /// Optional default repository URL for sessions launched from this profile.
    pub fn repo(&self) -> Option<&str> {
        self.repo.as_deref()
    }

    /// Optional five-field cron expression evaluated in daemon-local time.
    pub fn schedule(&self) -> Option<&str> {
        self.schedule.as_deref()
    }

    /// Optional environment variable name resolved by the daemon at dispatch
    /// time to authenticate the runner-managed `git clone` only. This value is
    /// not injected into the session runtime environment.
    pub fn repo_token_source(&self) -> Option<&str> {
        self.repo_token_source.as_deref()
    }

    /// Declared credentials for this profile. Each credential's name is a
    /// valid environment variable name, unique within this profile.
    pub fn credentials(&self) -> &[CredentialConfig] {
        &self.credentials
    }

    /// Static session command executed from the cloned repository.
    pub fn command(&self) -> &[String] {
        &self.command
    }
}

/// A validated profile-declared bind mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileMountConfig {
    source: PathBuf,
    target: PathBuf,
    read_only: bool,
}

impl ProfileMountConfig {
    /// Host-side source path for this mount.
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Absolute in-container target path for this mount.
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Whether the mount is read-only inside the container.
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub(crate) fn to_runner_mount(&self) -> BindMount {
        BindMount {
            source: self.source.clone(),
            target: self.target.clone(),
            read_only: self.read_only,
        }
    }
}

/// A declared credential for a profile.
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

/// Daemon-wide paths for the operator socket and PID file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    socket_path: PathBuf,
    pid_file: PathBuf,
}

impl DaemonConfig {
    /// Reads and parses only the daemon-wide runtime paths from a TOML
    /// configuration file at `path`.
    ///
    /// Relative daemon runtime paths are resolved against the directory
    /// containing `path`. The profile registry is ignored entirely, but other
    /// top-level sections must match the daemon config schema.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        let base_dir = absolute_config_dir(path)?;
        Self::parse_from_str(&contents, base_dir.as_deref())
    }

    /// Filesystem path for the daemon's local Unix socket.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Filesystem path for the daemon's PID file and advisory lock.
    pub fn pid_file(&self) -> &Path {
        &self.pid_file
    }

    /// Stable daemon-instance identifier derived from the configured runtime
    /// path pair. Used to scope runner-managed Podman resource ownership to a
    /// single daemon instance.
    ///
    /// Returns an error when either runtime path is relative. [`Config::load`]
    /// resolves daemon runtime paths to absolute paths, but [`Config::from_str`]
    /// preserves relative values as-is because it has no file base.
    pub fn daemon_instance_id(&self) -> Result<String, ConfigError> {
        validate_absolute_daemon_runtime_path("daemon.socket_path", &self.socket_path)?;
        validate_absolute_daemon_runtime_path("daemon.pid_file", &self.pid_file)?;

        let mut hasher = Sha256::new();
        hasher.update(b"socket=");
        hasher.update(normalize_path_lexically(&self.socket_path).as_bytes());
        hasher.update(b"\npid=");
        hasher.update(normalize_path_lexically(&self.pid_file).as_bytes());
        hasher.update(b"\n");

        let digest = hasher.finalize();
        Ok(hex_encode(&digest[..4]))
    }

    fn parse(raw: RawDaemonConfig, base_dir: Option<&Path>) -> Result<Self, ConfigError> {
        validate_non_empty("daemon.socket_path", &raw.socket_path, None, None)?;
        validate_non_empty("daemon.pid_file", &raw.pid_file, None, None)?;

        Ok(Self {
            socket_path: resolve_config_path(base_dir, &raw.socket_path),
            pid_file: resolve_config_path(base_dir, &raw.pid_file),
        })
    }

    fn parse_from_str(contents: &str, base_dir: Option<&Path>) -> Result<Self, ConfigError> {
        let raw: RawDaemonOnlyConfig = toml::from_str(contents)?;
        Self::parse(raw.daemon, base_dir)
    }
}

/// Errors produced when loading or validating a configuration file.
#[derive(Debug)]
pub enum ConfigError {
    /// Failed to read the configuration file from disk.
    Io(std::io::Error),
    /// The file contains invalid TOML or violates the expected schema.
    Parse(toml::de::Error),
    /// The configuration defines zero profiles. At least one must be declared.
    NoProfiles,
    /// Two profiles share the same name.
    DuplicateProfileName { name: String },
    /// A profile name fails the runner's [`validate_profile_name`] rules.
    InvalidProfileName { name: String },
    /// Two credentials within the same profile share a name.
    DuplicateCredentialName { profile: String, name: String },
    /// A credential name fails [`validate_environment_name`] (contains `,`
    /// or `=`, or is the reserved `PROFILE_NAME`).
    InvalidCredentialName { profile: String, name: String },
    /// A required string field is empty or whitespace-only.
    EmptyField {
        field: &'static str,
        profile: Option<String>,
        credential: Option<String>,
    },
    /// A lookup-key field has leading or trailing whitespace. Caught
    /// separately from empty because the trimmed value may be valid — the
    /// whitespace itself is the error.
    FieldHasOuterWhitespace {
        field: &'static str,
        profile: Option<String>,
        credential: Option<String>,
    },
    /// Deriving the daemon instance id requires absolute daemon runtime paths.
    RelativeDaemonRuntimePath { field: &'static str, path: PathBuf },
    /// The `command` array is empty for a profile.
    EmptyCommand { profile: String },
    /// A profile declares a default repository URL the runner would reject.
    InvalidRepo { profile: String, message: String },
    /// A profile declares an invalid cron expression.
    InvalidSchedule { profile: String, schedule: String },
    /// A scheduled profile must declare a default repo for autonomous runs.
    ScheduleRequiresRepo { profile: String },
    /// A configured mount source is not an absolute path.
    MountSourceMustBeAbsolute { profile: String, source: PathBuf },
    /// A configured mount target violates the runner's target rules.
    InvalidMountTarget {
        profile: String,
        error: MountTargetValidationError,
    },
    /// Two configured mounts in one profile share the same target path.
    DuplicateMountTarget { profile: String, target: PathBuf },
    /// Two configured mounts in one profile overlap by path components.
    OverlappingMountTargets {
        profile: String,
        first: PathBuf,
        second: PathBuf,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(error) => write!(f, "failed to read config: {error}"),
            ConfigError::Parse(error) => write!(f, "invalid config: {error}"),
            ConfigError::NoProfiles => write!(f, "config must define at least one profile"),
            ConfigError::DuplicateProfileName { name } => {
                write!(f, "duplicate profile name: {name}")
            }
            ConfigError::InvalidProfileName { name } => {
                write!(
                    f,
                    "invalid profile name '{name}'; {}",
                    RunnerError::InvalidProfileName
                )
            }
            ConfigError::DuplicateCredentialName { profile, name } => {
                write!(
                    f,
                    "profile '{profile}' defines duplicate credential name '{name}'"
                )
            }
            ConfigError::InvalidCredentialName { profile, name } => {
                write!(
                    f,
                    "profile '{profile}' defines invalid credential name '{name}'; credential names must not contain ',' or '=' and must not use reserved name 'PROFILE_NAME'"
                )
            }
            ConfigError::EmptyField {
                field,
                profile,
                credential,
            } => {
                write!(f, "field '{field}' must not be empty")?;

                if let Some(profile) = profile {
                    write!(f, " for profile '{profile}'")?;
                }
                if let Some(credential) = credential {
                    write!(f, " credential '{credential}'")?;
                }

                Ok(())
            }
            ConfigError::FieldHasOuterWhitespace {
                field,
                profile,
                credential,
            } => {
                write!(
                    f,
                    "field '{field}' must not have leading or trailing whitespace"
                )?;

                if let Some(profile) = profile {
                    write!(f, " for profile '{profile}'")?;
                }
                if let Some(credential) = credential {
                    write!(f, " credential '{credential}'")?;
                }

                Ok(())
            }
            ConfigError::RelativeDaemonRuntimePath { field, path } => {
                write!(
                    f,
                    "field '{field}' must be absolute before deriving daemon instance id: {}",
                    path.display()
                )
            }
            ConfigError::EmptyCommand { profile } => {
                write!(f, "profile '{profile}' must define a non-empty command")
            }
            ConfigError::InvalidRepo { profile, message } => {
                write!(f, "profile '{profile}' defines invalid repo: {message}")
            }
            ConfigError::InvalidSchedule { profile, schedule } => {
                write!(
                    f,
                    "profile '{profile}' defines invalid schedule '{schedule}'"
                )
            }
            ConfigError::ScheduleRequiresRepo { profile } => {
                write!(
                    f,
                    "profile '{profile}' defines schedule but no repo; scheduled profiles must define a repo"
                )
            }
            ConfigError::MountSourceMustBeAbsolute { profile, source } => {
                write!(
                    f,
                    "profile '{profile}' defines mount source that must be absolute: {}",
                    source.display()
                )
            }
            ConfigError::InvalidMountTarget { profile, error } => {
                write!(
                    f,
                    "profile '{profile}' defines invalid mount target: {error}"
                )
            }
            ConfigError::DuplicateMountTarget { profile, target } => {
                write!(
                    f,
                    "profile '{profile}' defines duplicate mount target: {}",
                    target.display()
                )
            }
            ConfigError::OverlappingMountTargets {
                profile,
                first,
                second,
            } => {
                write!(
                    f,
                    "profile '{profile}' defines overlapping mount targets: {} and {}",
                    first.display(),
                    second.display()
                )
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
    profiles: Vec<RawProfileConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDaemonOnlyConfig {
    #[serde(default)]
    daemon: RawDaemonConfig,
    #[serde(default, rename = "profiles")]
    _profiles: Option<toml::Value>,
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
struct RawProfileConfig {
    name: String,
    base_image: String,
    methodology_dir: String,
    command: Vec<String>,
    #[serde(default)]
    mounts: Vec<RawMountConfig>,
    repo: Option<String>,
    schedule: Option<String>,
    repo_token_source: Option<String>,
    #[serde(default)]
    credentials: Vec<RawCredentialConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMountConfig {
    source: String,
    target: String,
    read_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCredentialConfig {
    name: String,
    source: String,
}

fn default_daemon_socket_path() -> String {
    "/run/agentd/agentd.sock".to_string()
}

fn default_daemon_pid_file() -> String {
    "/run/agentd/agentd.pid".to_string()
}

fn normalize_path_lexically(path: &Path) -> String {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        ".".to_string()
    } else {
        normalized.to_string_lossy().into_owned()
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        encoded.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }

    encoded
}

fn validate_non_empty(
    field: &'static str,
    value: &str,
    profile: Option<&str>,
    credential: Option<&str>,
) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::EmptyField {
            field,
            profile: profile.map(str::to_owned),
            credential: credential.map(str::to_owned),
        });
    }

    Ok(())
}

fn validate_lookup_key(
    field: &'static str,
    value: &str,
    profile: Option<&str>,
    credential: Option<&str>,
) -> Result<(), ConfigError> {
    validate_non_empty(field, value, profile, credential)?;

    if value != value.trim() {
        return Err(ConfigError::FieldHasOuterWhitespace {
            field,
            profile: profile.map(str::to_owned),
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

fn validate_schedule(schedule: &str) -> Result<(), croner::errors::CronError> {
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
        .parse(schedule)
        .map(|_| ())
}

fn validate_absolute_daemon_runtime_path(
    field: &'static str,
    path: &Path,
) -> Result<(), ConfigError> {
    if path.is_absolute() {
        return Ok(());
    }

    Err(ConfigError::RelativeDaemonRuntimePath {
        field,
        path: path.to_path_buf(),
    })
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
