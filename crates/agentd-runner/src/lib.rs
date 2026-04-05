//! Session lifecycle management for agentd.

use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const AGENT_NAME_ENV: &str = "AGENT_NAME";
const METHODOLOGY_MOUNT_PATH: &str = "/agentd/methodology";
const PODMAN_INFRASTRUCTURE_ERROR_EXIT_CODE: i32 = 125;
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
        None => run_container_to_completion(&container_name),
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
        if is_reserved_environment_name(&variable.name) {
            return Err(RunnerError::ReservedEnvironmentName {
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
    ];

    for variable in &spec.environment {
        args.push("--env".to_string());
        args.push(format!("{}={}", variable.name, variable.value));
    }
    for (name, value) in runner_managed_environment(spec) {
        args.push("--env".to_string());
        args.push(format!("{name}={value}"));
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

fn run_container_to_completion(container_name: &str) -> Result<SessionOutcome, RunnerError> {
    let mut start = start_attached_container(container_name)?;
    let status = start.child.wait()?;
    let stderr = finish_captured_stderr(start.stderr_thread)?;

    classify_attached_start_result(start.args, status, stderr)
}

fn run_container_with_timeout(
    container_name: &str,
    timeout: Duration,
) -> Result<SessionOutcome, RunnerError> {
    let mut start = start_attached_container(container_name)?;

    match wait_for_child(&mut start.child, timeout)? {
        Some(status) => {
            let stderr = finish_captured_stderr(start.stderr_thread)?;
            classify_attached_start_result(start.args, status, stderr)
        }
        None => {
            cleanup_container(container_name)?;
            let _ = start.child.wait();
            let _ = finish_captured_stderr(start.stderr_thread)?;
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

struct AttachedPodmanStart {
    args: Vec<String>,
    child: Child,
    stderr_thread: thread::JoinHandle<std::io::Result<String>>,
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

fn start_attached_container(container_name: &str) -> Result<AttachedPodmanStart, RunnerError> {
    let args = vec![
        "start".to_string(),
        "--attach".to_string(),
        container_name.to_string(),
    ];
    let mut child = Command::new("podman")
        .args(&args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child
        .stderr
        .take()
        .expect("podman stderr should be piped when capturing attached startup errors");

    Ok(AttachedPodmanStart {
        args,
        child,
        stderr_thread: thread::spawn(move || forward_and_capture_stderr(stderr)),
    })
}

fn classify_attached_start_result(
    args: Vec<String>,
    status: ExitStatus,
    stderr: String,
) -> Result<SessionOutcome, RunnerError> {
    if status.code() == Some(PODMAN_INFRASTRUCTURE_ERROR_EXIT_CODE) {
        return Err(RunnerError::PodmanCommandFailed {
            args,
            status,
            stderr,
        });
    }

    Ok(container_status_to_outcome(status))
}

fn container_status_to_outcome(status: ExitStatus) -> SessionOutcome {
    match status.code().unwrap_or(1) {
        0 => SessionOutcome::Succeeded,
        exit_code => SessionOutcome::Failed { exit_code },
    }
}

fn runner_managed_environment(spec: &SessionSpec) -> [(&str, &str); 1] {
    [(AGENT_NAME_ENV, &spec.agent_name)]
}

fn is_reserved_environment_name(name: &str) -> bool {
    matches!(name, AGENT_NAME_ENV)
}

fn finish_captured_stderr(
    stderr_thread: thread::JoinHandle<std::io::Result<String>>,
) -> Result<String, RunnerError> {
    stderr_thread
        .join()
        .map_err(|panic_payload| {
            let message = match panic_payload.downcast::<String>() {
                Ok(message) => *message,
                Err(panic_payload) => match panic_payload.downcast::<&'static str>() {
                    Ok(message) => (*message).to_string(),
                    Err(_) => "unknown panic".to_string(),
                },
            };

            RunnerError::Io(std::io::Error::other(format!(
                "stderr forwarding thread panicked: {message}"
            )))
        })?
        .map_err(RunnerError::Io)
}

fn forward_and_capture_stderr<T>(mut stderr: T) -> std::io::Result<String>
where
    T: Read,
{
    let mut collected = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut host_stderr = std::io::stderr().lock();

    loop {
        let bytes_read = stderr.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        let chunk = &buffer[..bytes_read];
        host_stderr.write_all(chunk)?;
        host_stderr.flush()?;
        collected.extend_from_slice(chunk);
    }

    Ok(String::from_utf8_lossy(&collected).into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_spec_rejects_reserved_environment_names() {
        let error = validate_spec(&SessionSpec {
            agent_name: "agent".to_string(),
            base_image: "image".to_string(),
            methodology_dir: PathBuf::from("/tmp/methodology"),
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "AGENT_NAME".to_string(),
                value: "spoofed".to_string(),
            }],
        })
        .expect_err("reserved runner environment names should be rejected");

        match error {
            RunnerError::ReservedEnvironmentName { name } => {
                assert_eq!(name, "AGENT_NAME");
            }
            other => panic!("expected ReservedEnvironmentName, got {other:?}"),
        }
    }

    #[test]
    fn attached_start_classifies_exit_code_125_as_runner_error() {
        let error = classify_attached_start_result(
            vec![
                "start".to_string(),
                "--attach".to_string(),
                "container".to_string(),
            ],
            exit_status(125),
            "podman start failed".to_string(),
        )
        .expect_err("podman infrastructure failures should surface as runner errors");

        match error {
            RunnerError::PodmanCommandFailed {
                args,
                status,
                stderr,
            } => {
                assert_eq!(
                    args,
                    vec![
                        "start".to_string(),
                        "--attach".to_string(),
                        "container".to_string(),
                    ]
                );
                assert_eq!(status.code(), Some(125));
                assert_eq!(stderr, "podman start failed");
            }
            other => panic!("expected PodmanCommandFailed, got {other:?}"),
        }
    }

    #[test]
    fn attached_start_classifies_nonzero_exit_as_session_failure() {
        let outcome = classify_attached_start_result(
            vec![
                "start".to_string(),
                "--attach".to_string(),
                "container".to_string(),
            ],
            exit_status(23),
            String::new(),
        )
        .expect("agent exit codes should remain session outcomes");

        assert_eq!(outcome, SessionOutcome::Failed { exit_code: 23 });
    }

    #[test]
    fn attached_start_classifies_zero_exit_as_success() {
        let outcome = classify_attached_start_result(
            vec![
                "start".to_string(),
                "--attach".to_string(),
                "container".to_string(),
            ],
            exit_status(0),
            String::new(),
        )
        .expect("successful attached starts should remain successful session outcomes");

        assert_eq!(outcome, SessionOutcome::Succeeded);
    }

    #[cfg(unix)]
    fn exit_status(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        ExitStatusExt::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn exit_status(code: i32) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;

        ExitStatusExt::from_raw(code as u32)
    }
}
