//! Session lifecycle management for agentd.

use std::fmt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const METHODOLOGY_MOUNT_PATH: &str = "/agentd/methodology";
const REPO_DIR: &str = "/agentd/workspace/repo";

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

#[derive(Debug)]
pub enum RunnerError {
    MissingMethodologyManifest {
        path: PathBuf,
    },
    InvalidAgentName,
    InvalidBaseImage,
    InvalidRepoUrl,
    InvalidAgentCommand,
    InvalidEnvironmentName {
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
            RunnerError::InvalidAgentName => write!(f, "agent_name must not be empty"),
            RunnerError::InvalidBaseImage => write!(f, "base_image must not be empty"),
            RunnerError::InvalidRepoUrl => write!(f, "repo_url must not be empty"),
            RunnerError::InvalidAgentCommand => {
                write!(f, "agent_command must contain at least one argument")
            }
            RunnerError::InvalidEnvironmentName { name } => write!(
                f,
                "environment variable names must not be empty and must not contain '=': {name}"
            ),
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

pub fn run_session(
    spec: SessionSpec,
    invocation: SessionInvocation,
) -> Result<SessionOutcome, RunnerError> {
    validate_spec(&spec)?;
    validate_invocation(&invocation)?;

    let container_name = format!(
        "agentd-{}-{}",
        sanitize_name(&spec.agent_name),
        unique_suffix()
    );
    let manifest_path = spec.methodology_dir.join("manifest.toml");
    if !manifest_path.is_file() {
        return Err(RunnerError::MissingMethodologyManifest {
            path: manifest_path,
        });
    }

    create_container(&container_name, &spec, &invocation)?;

    let start_result = match invocation.timeout {
        Some(timeout) => run_container_with_timeout(&container_name, timeout),
        None => Ok(container_status_to_outcome(run_container_to_completion(
            &container_name,
        )?)),
    };

    let cleanup_result = cleanup_container(&container_name);

    match (start_result, cleanup_result) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(_cleanup_error)) => Err(error),
    }
}

fn validate_spec(spec: &SessionSpec) -> Result<(), RunnerError> {
    if spec.agent_name.trim().is_empty() {
        return Err(RunnerError::InvalidAgentName);
    }
    if spec.base_image.trim().is_empty() {
        return Err(RunnerError::InvalidBaseImage);
    }
    if spec.agent_command.is_empty() || spec.agent_command.iter().any(|arg| arg.is_empty()) {
        return Err(RunnerError::InvalidAgentCommand);
    }

    for variable in &spec.environment {
        if variable.name.is_empty() || variable.name.contains('=') {
            return Err(RunnerError::InvalidEnvironmentName {
                name: variable.name.clone(),
            });
        }
    }

    Ok(())
}

fn validate_invocation(invocation: &SessionInvocation) -> Result<(), RunnerError> {
    if invocation.repo_url.trim().is_empty() {
        return Err(RunnerError::InvalidRepoUrl);
    }

    Ok(())
}

fn create_container(
    container_name: &str,
    spec: &SessionSpec,
    invocation: &SessionInvocation,
) -> Result<(), RunnerError> {
    let methodology_dir = spec.methodology_dir.canonicalize()?;
    let mut args = vec![
        "create".to_string(),
        "--name".to_string(),
        container_name.to_string(),
        "--mount".to_string(),
        format!(
            "type=bind,src={},target={},ro=true",
            methodology_dir.display(),
            METHODOLOGY_MOUNT_PATH
        ),
        "--env".to_string(),
        format!("AGENT_NAME={}", spec.agent_name),
    ];

    for variable in &spec.environment {
        args.push("--env".to_string());
        args.push(format!("{}={}", variable.name, variable.value));
    }

    args.push(spec.base_image.clone());
    args.push("sh".to_string());
    args.push("-lc".to_string());
    args.push(build_container_script(spec, invocation));

    run_podman_command(args)
}

fn build_container_script(spec: &SessionSpec, invocation: &SessionInvocation) -> String {
    let mut script = String::from(
        "set -eu\n\
         mkdir -p /agentd/workspace\n\
         rm -rf /agentd/workspace/repo\n\
         git clone --no-hardlinks ",
    );
    script.push_str(&shell_quote(&invocation.repo_url));
    script.push(' ');
    script.push_str(&shell_quote(REPO_DIR));
    script.push_str("\ncd ");
    script.push_str(&shell_quote(REPO_DIR));
    script.push_str("\nruna init --methodology ");
    script.push_str(&shell_quote(&format!(
        "{METHODOLOGY_MOUNT_PATH}/manifest.toml"
    )));
    script.push_str("\ncat >> .runa/config.toml <<'EOF'\n[agent]\ncommand = ");
    script.push_str(&toml_array(&spec.agent_command));
    script.push_str("\nEOF\nexec runa run");

    if let Some(work_unit) = &invocation.work_unit {
        script.push(' ');
        script.push_str("--work-unit ");
        script.push_str(&shell_quote(work_unit));
    }

    script
}

fn run_container_to_completion(container_name: &str) -> Result<ExitStatus, RunnerError> {
    let output = Command::new("podman")
        .args(["start", "--attach", container_name])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    Ok(output)
}

fn run_container_with_timeout(
    container_name: &str,
    timeout: Duration,
) -> Result<SessionOutcome, RunnerError> {
    let mut child = Command::new("podman")
        .args(["start", "--attach", container_name])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    match wait_for_child(&mut child, timeout)? {
        Some(status) => Ok(container_status_to_outcome(status)),
        None => {
            cleanup_container(container_name)?;
            let _ = child.wait();
            Ok(SessionOutcome::TimedOut)
        }
    }
}

fn wait_for_child(child: &mut Child, timeout: Duration) -> Result<Option<ExitStatus>, RunnerError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn cleanup_container(container_name: &str) -> Result<(), RunnerError> {
    run_podman_command(vec![
        "rm".to_string(),
        "--force".to_string(),
        "--ignore".to_string(),
        container_name.to_string(),
    ])
}

fn run_podman_command(args: Vec<String>) -> Result<(), RunnerError> {
    let output = Command::new("podman").args(&args).output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(RunnerError::PodmanCommandFailed {
        args,
        status: output.status,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn container_status_to_outcome(status: ExitStatus) -> SessionOutcome {
    match status.code().unwrap_or(1) {
        0 => SessionOutcome::Succeeded,
        exit_code => SessionOutcome::Failed { exit_code },
    }
}

fn sanitize_name(value: &str) -> String {
    let mut result = String::new();
    let mut last_was_dash = false;

    for character in value.chars() {
        let normalized = character.to_ascii_lowercase();
        if normalized.is_ascii_alphanumeric() {
            result.push(normalized);
            last_was_dash = false;
        } else if !last_was_dash {
            result.push('-');
            last_was_dash = true;
        }
    }

    result.trim_matches('-').to_string()
}

fn unique_suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after the unix epoch")
            .as_nanos()
    )
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    let mut quoted = String::from("'");
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\"'\"'");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

fn toml_array(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| toml_string(value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

fn toml_string(value: &str) -> String {
    let mut encoded = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => encoded.push_str("\\\\"),
            '"' => encoded.push_str("\\\""),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            other => encoded.push(other),
        }
    }
    encoded.push('"');
    encoded
}

fn exit_status_label(status: &ExitStatus) -> String {
    status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string())
}
