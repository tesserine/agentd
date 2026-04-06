//! Session lifecycle management for agentd.

use getrandom::fill as fill_random_bytes;
use std::collections::VecDeque;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const AGENT_NAME_ENV: &str = "AGENT_NAME";
const ATTACHED_STDERR_TAIL_LIMIT: usize = 64 * 1024;
const ATTACHED_STDERR_TRUNCATION_NOTICE: &str = "[stderr truncated to last 65536 bytes]\n";
const METHODOLOGY_MOUNT_PATH: &str = "/agentd/methodology";
const METHODOLOGY_STAGE_LINK_NAME: &str = "methodology";
const PODMAN_INFRASTRUCTURE_ERROR_EXIT_CODE: i32 = 125;
const REPO_DIR: &str = "/agentd/workspace/repo";
const SUPPORTED_REPO_URL_FORMS: &str = "https://, http://, or git://";
const SUPPORTED_REPO_URL_PREFIXES: [&str; 3] = ["https://", "http://", "git://"];
const SESSION_SECRET_PREFIX: &str = "agentd-secret-";
const SESSION_STAGE_PREFIX: &str = "agentd-session-stage-";

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
            RunnerError::InvalidAgentName => write!(f, "agent_name must not be empty"),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SecretBinding {
    secret_name: String,
    target_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionResources {
    container_name: String,
    methodology_staging_dir: PathBuf,
    methodology_mount_source: PathBuf,
    secret_bindings: Vec<SecretBinding>,
}

pub fn run_session(
    spec: SessionSpec,
    invocation: SessionInvocation,
) -> Result<SessionOutcome, RunnerError> {
    validate_spec(&spec)?;
    validate_invocation(&invocation)?;
    let session_id = unique_suffix()?;

    let container_name = format!("agentd-{}-{}", sanitize_name(&spec.agent_name), session_id);
    let manifest_path = spec.methodology_dir.join("manifest.toml");
    if !manifest_path.is_file() {
        return Err(RunnerError::MissingMethodologyManifest {
            path: manifest_path,
        });
    }

    let resources = prepare_session_resources(&container_name, &spec, &session_id)?;

    if let Err(error) = create_container(&resources, &spec, &invocation) {
        if let Err(cleanup_error) = cleanup_session_resources(&resources) {
            log_cleanup_failure("container creation", &cleanup_error);
        }
        return Err(error);
    }

    let start_result = match invocation.timeout {
        Some(timeout) => run_container_with_timeout(
            &resources.container_name,
            &resources.secret_bindings,
            timeout,
        ),
        None => run_container_to_completion(&resources.container_name, &resources.secret_bindings),
    };

    let cleanup_result = cleanup_session_resources(&resources);

    match (start_result, cleanup_result) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            log_cleanup_failure("session execution", &cleanup_error);
            Err(error)
        }
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
        match validate_environment_name(&variable.name) {
            Ok(()) => {}
            Err(EnvironmentNameValidationError::Invalid) => {
                return Err(RunnerError::InvalidEnvironmentName {
                    name: variable.name.clone(),
                });
            }
            Err(EnvironmentNameValidationError::Reserved) => {
                return Err(RunnerError::ReservedEnvironmentName {
                    name: variable.name.clone(),
                });
            }
        }
    }

    Ok(())
}

fn validate_invocation(invocation: &SessionInvocation) -> Result<(), RunnerError> {
    let repo_url = invocation.repo_url.as_str();
    if repo_url.trim().is_empty() || repo_url != repo_url.trim() {
        return Err(unsupported_repo_url_error());
    }

    if has_repo_url_userinfo(repo_url) {
        return Err(credential_bearing_repo_url_error());
    }

    if !is_supported_repo_url(repo_url) {
        return Err(unsupported_repo_url_error());
    }

    Ok(())
}

fn is_supported_repo_url(repo_url: &str) -> bool {
    if repo_url.contains(['?', '#']) {
        return false;
    }

    repo_url_authority(repo_url)
        .zip(repo_url_path(repo_url))
        .map(|(authority, path)| {
            !authority.is_empty()
                && !authority.starts_with('/')
                && path.starts_with('/')
                && path.len() > 1
        })
        .unwrap_or(false)
}

fn has_repo_url_userinfo(repo_url: &str) -> bool {
    repo_url_authority(repo_url)
        .map(|authority| authority.contains('@'))
        .unwrap_or(false)
}

fn repo_url_authority(repo_url: &str) -> Option<&str> {
    let prefix = SUPPORTED_REPO_URL_PREFIXES
        .iter()
        .find(|prefix| repo_url.starts_with(**prefix))?;

    let remainder = &repo_url[prefix.len()..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    Some(&remainder[..authority_end])
}

fn repo_url_path(repo_url: &str) -> Option<&str> {
    let prefix = SUPPORTED_REPO_URL_PREFIXES
        .iter()
        .find(|prefix| repo_url.starts_with(**prefix))?;

    let remainder = &repo_url[prefix.len()..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    Some(&remainder[authority_end..])
}

fn unsupported_repo_url_error() -> RunnerError {
    RunnerError::InvalidRepoUrl {
        message: format!(
            "must be a supported public remote repository URL ({SUPPORTED_REPO_URL_FORMS})"
        ),
    }
}

fn credential_bearing_repo_url_error() -> RunnerError {
    RunnerError::InvalidRepoUrl {
        message: "must not embed credentials in the URL; credential-bearing URLs are not accepted until #32 lands".to_string(),
    }
}

fn create_container(
    resources: &SessionResources,
    spec: &SessionSpec,
    invocation: &SessionInvocation,
) -> Result<(), RunnerError> {
    run_podman_command(build_create_container_args(resources, spec, invocation)).map(|_| ())
}

fn build_container_script(spec: &SessionSpec, invocation: &SessionInvocation) -> String {
    let mut script = String::from(
        "set -eu\n\
         mkdir -p /agentd/workspace\n\
         rm -rf /agentd/workspace/repo\n\
         git clone --no-hardlinks -- ",
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

fn run_container_to_completion(
    container_name: &str,
    secret_bindings: &[SecretBinding],
) -> Result<SessionOutcome, RunnerError> {
    let mut start = start_attached_container(container_name)?;
    let wait_result =
        wait_for_container_exit(&mut start.child, container_name, secret_bindings, None);

    match wait_result {
        Ok(Some(status)) => {
            let (args, stderr) = finalize_attached_start(start)?;
            classify_attached_start_result(args, container_name, status, stderr)
        }
        Ok(None) => unreachable!("container wait without timeout should not return a timeout"),
        Err(error) => {
            finalize_attached_start(start)?;
            Err(error)
        }
    }
}

fn run_container_with_timeout(
    container_name: &str,
    secret_bindings: &[SecretBinding],
    timeout: Duration,
) -> Result<SessionOutcome, RunnerError> {
    let mut start = start_attached_container(container_name)?;
    let wait_result = wait_for_container_exit(
        &mut start.child,
        container_name,
        secret_bindings,
        Some(timeout),
    );

    match wait_result {
        Ok(Some(status)) => {
            let (args, stderr) = finalize_attached_start(start)?;
            classify_attached_start_result(args, container_name, status, stderr)
        }
        Ok(None) => {
            let cleanup_result = cleanup_container(container_name);
            let finalize_result = finalize_attached_start(start).map(|_| ());

            cleanup_result?;
            finalize_result?;
            Ok(SessionOutcome::TimedOut)
        }
        Err(error) => {
            finalize_attached_start(start)?;
            Err(error)
        }
    }
}

fn finalize_attached_start(
    mut start: AttachedPodmanStart,
) -> Result<(Vec<String>, String), RunnerError> {
    start.child.wait()?;
    let stderr = finish_captured_stderr(start.stderr_thread)?;
    Ok((start.args, stderr))
}

fn log_cleanup_failure(stage: &str, error: &RunnerError) {
    let mut stderr = std::io::stderr().lock();
    let _ = log_cleanup_failure_to(&mut stderr, stage, error);
}

fn log_cleanup_failure_to<W>(
    writer: &mut W,
    stage: &str,
    error: &RunnerError,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "cleanup after {stage} failed: {error}")
}

fn wait_for_container_exit(
    child: &mut Child,
    container_name: &str,
    secret_bindings: &[SecretBinding],
    timeout: Option<Duration>,
) -> Result<Option<ExitStatus>, RunnerError> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let mut secrets_released = secret_bindings.is_empty();

    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }

        if !secrets_released && inspect_container_status(container_name)? == "running" {
            cleanup_podman_secrets(secret_bindings)?;
            secrets_released = true;
        }

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if let Some(status) = child.try_wait()? {
                return Ok(Some(status));
            }
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
    .map(|_| ())
}

fn run_podman_command(args: Vec<String>) -> Result<String, RunnerError> {
    run_podman_command_with_input(args, None)
}

fn run_podman_command_with_input(
    args: Vec<String>,
    stdin_data: Option<&[u8]>,
) -> Result<String, RunnerError> {
    let mut command = Command::new("podman");
    command
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin_data.is_some() {
        command.stdin(Stdio::piped());
    }

    let mut child = command.spawn()?;
    let write_error = if let Some(stdin_data) = stdin_data {
        let write_result = {
            let mut stdin = child
                .stdin
                .take()
                .expect("podman stdin should be piped when input is provided");
            stdin.write_all(stdin_data)
        };
        write_result.err()
    } else {
        None
    };

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(RunnerError::PodmanCommandFailed {
            args,
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    if let Some(write_error) = write_error {
        return Err(RunnerError::Io(write_error));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
    container_name: &str,
    status: ExitStatus,
    stderr: String,
) -> Result<SessionOutcome, RunnerError> {
    classify_attached_start_result_with_inspector(args, status, stderr, || {
        inspect_terminal_container_outcome(container_name)
    })
}

fn classify_attached_start_result_with_inspector<F>(
    args: Vec<String>,
    status: ExitStatus,
    stderr: String,
    inspect_terminal_outcome: F,
) -> Result<SessionOutcome, RunnerError>
where
    F: FnOnce() -> Option<SessionOutcome>,
{
    if status.code() == Some(PODMAN_INFRASTRUCTURE_ERROR_EXIT_CODE) {
        if let Some(outcome) = inspect_terminal_outcome() {
            return Ok(outcome);
        }

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

fn inspect_terminal_container_outcome(container_name: &str) -> Option<SessionOutcome> {
    let output = run_podman_command(vec![
        "inspect".to_string(),
        "--type".to_string(),
        "container".to_string(),
        "--format".to_string(),
        "{{.State.Status}} {{.State.ExitCode}}".to_string(),
        container_name.to_string(),
    ])
    .ok()?;
    let (status, exit_code) = parse_container_state(&output)?;

    if matches!(status, "exited" | "stopped") {
        return Some(exit_code_to_outcome(exit_code));
    }

    None
}

fn parse_container_state(output: &str) -> Option<(&str, i32)> {
    let mut parts = output.split_whitespace();
    let status = parts.next()?;
    let exit_code = parts.next()?.parse().ok()?;
    Some((status, exit_code))
}

fn exit_code_to_outcome(exit_code: i32) -> SessionOutcome {
    match exit_code {
        0 => SessionOutcome::Succeeded,
        exit_code => SessionOutcome::Failed { exit_code },
    }
}

fn inspect_container_status(container_name: &str) -> Result<String, RunnerError> {
    run_podman_command(vec![
        "inspect".to_string(),
        "--type".to_string(),
        "container".to_string(),
        "--format".to_string(),
        "{{.State.Status}}".to_string(),
        container_name.to_string(),
    ])
    .map(|output| output.trim().to_string())
}

fn runner_managed_environment(spec: &SessionSpec) -> [(&str, &str); 1] {
    [(AGENT_NAME_ENV, &spec.agent_name)]
}

pub fn validate_environment_name(name: &str) -> Result<(), EnvironmentNameValidationError> {
    if name.is_empty() || name.contains('=') || name.contains(',') {
        return Err(EnvironmentNameValidationError::Invalid);
    }
    if is_reserved_environment_name(name) {
        return Err(EnvironmentNameValidationError::Reserved);
    }

    Ok(())
}

fn is_reserved_environment_name(name: &str) -> bool {
    matches!(name, AGENT_NAME_ENV)
}

fn prepare_session_resources(
    container_name: &str,
    spec: &SessionSpec,
    session_id: &str,
) -> Result<SessionResources, RunnerError> {
    let methodology_staging_dir =
        create_methodology_staging_dir(&spec.methodology_dir, session_id)?;
    let methodology_mount_source = methodology_staging_dir.join(METHODOLOGY_STAGE_LINK_NAME);
    let mut resources = SessionResources {
        container_name: container_name.to_string(),
        methodology_staging_dir,
        methodology_mount_source,
        secret_bindings: Vec::new(),
    };

    for (index, variable) in spec.environment.iter().enumerate() {
        let secret_name = format!("{SESSION_SECRET_PREFIX}{session_id}-{index}");
        if let Err(error) = create_podman_secret(&secret_name, &variable.value) {
            let _ = cleanup_session_resources(&resources);
            return Err(error);
        }
        resources.secret_bindings.push(SecretBinding {
            secret_name,
            target_name: variable.name.clone(),
        });
    }

    Ok(resources)
}

fn create_methodology_staging_dir(
    methodology_dir: &Path,
    session_id: &str,
) -> Result<PathBuf, RunnerError> {
    let canonical_methodology_dir = methodology_dir.canonicalize()?;
    let staging_dir = safe_staging_root().join(format!("{SESSION_STAGE_PREFIX}{session_id}"));
    fs::create_dir_all(&staging_dir)?;
    let staged_link = staging_dir.join(METHODOLOGY_STAGE_LINK_NAME);

    if let Err(error) = create_directory_symlink(&canonical_methodology_dir, &staged_link) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(error);
    }

    Ok(staging_dir)
}

fn safe_staging_root() -> PathBuf {
    let temp_dir = std::env::temp_dir();
    if !path_requires_mount_staging_alias(&temp_dir) {
        return temp_dir;
    }

    #[cfg(unix)]
    {
        PathBuf::from("/tmp")
    }

    #[cfg(not(unix))]
    {
        temp_dir
    }
}

fn path_requires_mount_staging_alias(path: &Path) -> bool {
    path.to_string_lossy().contains(',')
}

fn create_podman_secret(secret_name: &str, value: &str) -> Result<(), RunnerError> {
    run_podman_command_with_input(
        vec![
            "secret".to_string(),
            "create".to_string(),
            secret_name.to_string(),
            "-".to_string(),
        ],
        Some(value.as_bytes()),
    )
    .map(|_| ())
}

fn cleanup_session_resources(resources: &SessionResources) -> Result<(), RunnerError> {
    let container_result = cleanup_container(&resources.container_name);
    let secret_result = cleanup_podman_secrets(&resources.secret_bindings);
    let staging_result = cleanup_methodology_staging_dir(&resources.methodology_staging_dir);

    container_result?;
    secret_result?;
    staging_result
}

fn cleanup_podman_secrets(secret_bindings: &[SecretBinding]) -> Result<(), RunnerError> {
    if secret_bindings.is_empty() {
        return Ok(());
    }

    let mut args = vec![
        "secret".to_string(),
        "rm".to_string(),
        "--ignore".to_string(),
    ];
    args.extend(
        secret_bindings
            .iter()
            .map(|binding| binding.secret_name.clone()),
    );
    run_podman_command(args).map(|_| ())
}

fn cleanup_methodology_staging_dir(path: &Path) -> Result<(), RunnerError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RunnerError::Io(error)),
    }
}

fn build_create_container_args(
    resources: &SessionResources,
    spec: &SessionSpec,
    invocation: &SessionInvocation,
) -> Vec<String> {
    let mut args = vec![
        "create".to_string(),
        "--name".to_string(),
        resources.container_name.clone(),
        "--mount".to_string(),
        format!(
            "type=bind,src={},target={},ro=true,relabel=shared",
            resources.methodology_mount_source.display(),
            METHODOLOGY_MOUNT_PATH
        ),
    ];

    for binding in &resources.secret_bindings {
        args.push("--secret".to_string());
        args.push(format!(
            "{},type=env,target={}",
            binding.secret_name, binding.target_name
        ));
    }

    for (name, value) in runner_managed_environment(spec) {
        args.push("--env".to_string());
        args.push(format!("{name}={value}"));
    }

    args.push(spec.base_image.clone());
    args.push("sh".to_string());
    args.push("-lc".to_string());
    args.push(build_container_script(spec, invocation));

    args
}

#[cfg(unix)]
fn create_directory_symlink(source: &Path, destination: &Path) -> Result<(), RunnerError> {
    std::os::unix::fs::symlink(source, destination).map_err(RunnerError::Io)
}

#[cfg(windows)]
fn create_directory_symlink(source: &Path, destination: &Path) -> Result<(), RunnerError> {
    std::os::windows::fs::symlink_dir(source, destination).map_err(RunnerError::Io)
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
    let mut host_stderr = std::io::stderr().lock();
    forward_and_capture_stderr_to(&mut stderr, &mut host_stderr)
}

fn forward_and_capture_stderr_to<T, U>(mut stderr: T, mut host_stderr: U) -> std::io::Result<String>
where
    T: Read,
    U: Write,
{
    let mut collected = StderrTailBuffer::new(ATTACHED_STDERR_TAIL_LIMIT);
    let mut buffer = [0_u8; 4096];

    loop {
        let bytes_read = stderr.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        let chunk = &buffer[..bytes_read];
        host_stderr.write_all(chunk)?;
        host_stderr.flush()?;
        collected.push(chunk);
    }

    Ok(collected.into_string())
}

struct StderrTailBuffer {
    bytes: VecDeque<u8>,
    limit: usize,
    truncated: bool,
}

impl StderrTailBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(limit),
            limit,
            truncated: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.len() >= self.limit {
            self.bytes.clear();
            self.bytes
                .extend(chunk[chunk.len() - self.limit..].iter().copied());
            self.truncated = true;
            return;
        }

        let overflow = self
            .bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.limit);
        if overflow > 0 {
            self.bytes.drain(..overflow);
            self.truncated = true;
        }

        self.bytes.extend(chunk.iter().copied());
    }

    fn into_string(self) -> String {
        let stderr =
            String::from_utf8_lossy(&self.bytes.into_iter().collect::<Vec<_>>()).into_owned();
        if self.truncated {
            return format!("{ATTACHED_STDERR_TRUNCATION_NOTICE}{stderr}");
        }

        stderr
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

fn unique_suffix() -> Result<String, RunnerError> {
    unique_suffix_with(|bytes| fill_random_bytes(bytes).map_err(std::io::Error::other))
}

fn unique_suffix_with<F>(fill_random: F) -> Result<String, RunnerError>
where
    F: FnOnce(&mut [u8]) -> std::io::Result<()>,
{
    let mut bytes = [0_u8; 16];
    fill_random(&mut bytes)?;
    Ok(hex_encode(&bytes))
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
            '\u{0008}' => encoded.push_str("\\b"),
            '\u{000C}' => encoded.push_str("\\f"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            other if other.is_control() => {
                encoded.push_str(&format!("\\u{:04X}", other as u32));
            }
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
    use std::env;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    const VALID_REMOTE_REPO_URL: &str = "https://example.com/agentd.git";

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
    fn validate_spec_rejects_environment_names_containing_commas() {
        let error = validate_spec(&SessionSpec {
            agent_name: "agent".to_string(),
            base_image: "image".to_string(),
            methodology_dir: PathBuf::from("/tmp/methodology"),
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "TOKEN,EXTRA".to_string(),
                value: "secret".to_string(),
            }],
        })
        .expect_err("comma-delimited environment names should be rejected");

        match error {
            RunnerError::InvalidEnvironmentName { name } => {
                assert_eq!(name, "TOKEN,EXTRA");
            }
            other => panic!("expected InvalidEnvironmentName, got {other:?}"),
        }
    }

    #[test]
    fn validate_spec_rejects_environment_names_containing_equals_signs() {
        let error = validate_spec(&SessionSpec {
            agent_name: "agent".to_string(),
            base_image: "image".to_string(),
            methodology_dir: PathBuf::from("/tmp/methodology"),
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "TOKEN=EXTRA".to_string(),
                value: "secret".to_string(),
            }],
        })
        .expect_err("environment names containing '=' should be rejected");

        match error {
            RunnerError::InvalidEnvironmentName { name } => {
                assert_eq!(name, "TOKEN=EXTRA");
            }
            other => panic!("expected InvalidEnvironmentName, got {other:?}"),
        }
    }

    #[test]
    fn validate_invocation_accepts_supported_remote_repo_urls() {
        for repo_url in [
            "https://example.com/agentd.git",
            "http://example.com/agentd.git",
            "git://example.com/agentd.git",
        ] {
            validate_invocation(&SessionInvocation {
                repo_url: repo_url.to_string(),
                work_unit: None,
                timeout: None,
            })
            .unwrap_or_else(|error| panic!("expected {repo_url} to be accepted, got {error}"));
        }
    }

    #[test]
    fn validate_invocation_rejects_non_remote_repo_urls() {
        for repo_url in [
            "",
            " ",
            " repo ",
            "repo",
            "./repo",
            "../repo.git",
            "/srv/test-repo.git",
            "file:///srv/test-repo.git",
            "ssh://git@example.com/agentd.git",
            "git@example.com:agentd.git",
            "https://user:token@example.com/repo.git",
            "https://",
            "http://",
            "git://",
            "https://github.com",
            "http:///repo.git",
            "https://?ref=main",
            "https://#readme",
            "https://example.com/repo.git?token=secret",
            "https://example.com/repo.git#readme",
            "example.com:agentd.git",
            "git@example.com",
            "@example.com:agentd.git",
            "git@:agentd.git",
        ] {
            let error = validate_invocation(&SessionInvocation {
                repo_url: repo_url.to_string(),
                work_unit: None,
                timeout: None,
            })
            .expect_err("non-remote repo URL should be rejected");

            assert!(
                matches!(error, RunnerError::InvalidRepoUrl { .. }),
                "expected InvalidRepoUrl for {repo_url}, got {error:?}"
            );
        }
    }

    #[test]
    fn validate_invocation_rejects_credential_bearing_repo_urls() {
        let error = validate_invocation(&SessionInvocation {
            repo_url: "https://user:token@example.com/repo.git".to_string(),
            work_unit: None,
            timeout: None,
        })
        .expect_err("credential-bearing repo URLs should be rejected");

        let message = error.to_string();
        assert!(
            message.contains("credential-bearing URLs are not accepted"),
            "expected credential-bearing URL rejection message, got {message}"
        );
        assert!(
            message.contains("#32"),
            "expected credential-bearing URL rejection to reference #32, got {message}"
        );
    }

    #[test]
    fn run_session_rejects_invalid_repo_url_before_methodology_validation() {
        let error = run_session(
            SessionSpec {
                agent_name: "agent".to_string(),
                base_image: "image".to_string(),
                methodology_dir: PathBuf::from("/tmp/does-not-exist"),
                agent_command: vec!["codex".to_string(), "exec".to_string()],
                environment: Vec::new(),
            },
            SessionInvocation {
                repo_url: "/srv/test-repo.git".to_string(),
                work_unit: None,
                timeout: None,
            },
        )
        .expect_err("invalid repo URL should be rejected before setup");

        assert!(
            matches!(error, RunnerError::InvalidRepoUrl { .. }),
            "expected InvalidRepoUrl, got {error:?}"
        );
    }

    #[test]
    fn run_session_rejects_credential_bearing_repo_url_before_methodology_validation() {
        let error = run_session(
            SessionSpec {
                agent_name: "agent".to_string(),
                base_image: "image".to_string(),
                methodology_dir: PathBuf::from("/tmp/does-not-exist"),
                agent_command: vec!["codex".to_string(), "exec".to_string()],
                environment: Vec::new(),
            },
            SessionInvocation {
                repo_url: "https://user:token@example.com/repo.git".to_string(),
                work_unit: None,
                timeout: None,
            },
        )
        .expect_err("credential-bearing repo URL should be rejected before setup");

        assert!(
            matches!(error, RunnerError::InvalidRepoUrl { .. }),
            "expected InvalidRepoUrl, got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("credential-bearing URLs are not accepted until #32 lands"),
            "expected credential-bearing URL message, got {error}"
        );
    }

    #[test]
    fn create_container_args_include_shared_relabel_for_methodology_mount() {
        let args = build_create_container_args(
            &SessionResources {
                container_name: "agentd-agent-session".to_string(),
                methodology_staging_dir: PathBuf::from("/tmp/staging"),
                methodology_mount_source: PathBuf::from("/tmp/staging/methodology"),
                secret_bindings: Vec::new(),
            },
            &SessionSpec {
                agent_name: "agent".to_string(),
                base_image: "image".to_string(),
                methodology_dir: PathBuf::from("/tmp/methodology"),
                agent_command: vec!["codex".to_string(), "exec".to_string()],
                environment: Vec::new(),
            },
            &SessionInvocation {
                repo_url: VALID_REMOTE_REPO_URL.to_string(),
                work_unit: None,
                timeout: None,
            },
        );

        let mount_value = argument_value(&args.join(" "), "--mount")
            .expect("podman create should receive a methodology mount");

        assert!(
            mount_value.contains("relabel=shared"),
            "methodology bind mount should include shared SELinux relabeling: {mount_value}"
        );
    }

    #[test]
    fn build_container_script_terminates_git_clone_options_before_repo_url() {
        let script = build_container_script(
            &SessionSpec {
                agent_name: "agent".to_string(),
                base_image: "image".to_string(),
                methodology_dir: PathBuf::from("/tmp/methodology"),
                agent_command: vec!["codex".to_string(), "exec".to_string()],
                environment: Vec::new(),
            },
            &SessionInvocation {
                repo_url: "-repo.git".to_string(),
                work_unit: None,
                timeout: None,
            },
        );

        assert!(
            script.contains("git clone --no-hardlinks -- '-repo.git' '/agentd/workspace/repo'"),
            "git clone should terminate options before the repo URL: {script}"
        );
    }

    #[test]
    fn toml_string_escapes_control_characters_into_valid_toml() {
        let original = "before\x08middle\x0cafter\0tail";
        let encoded = toml_string(original);

        assert!(
            encoded.contains("\\b"),
            "backspace should use the TOML named escape: {encoded:?}"
        );
        assert!(
            encoded.contains("\\f"),
            "form feed should use the TOML named escape: {encoded:?}"
        );
        assert!(
            encoded.contains("\\u0000"),
            "null should use a unicode escape: {encoded:?}"
        );

        let document = format!("value = {encoded}\n");
        let parsed: toml::Value =
            toml::from_str(&document).expect("escaped string should parse as TOML");

        assert_eq!(
            parsed.get("value").and_then(toml::Value::as_str),
            Some(original),
            "TOML parsing should round-trip the original control characters"
        );
    }

    #[test]
    fn wait_for_container_exit_checks_child_status_again_after_timeout_boundary() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.write_script(
            r#"#!/bin/sh
set -eu

log_root="${AGENTD_FAKE_PODMAN_LOG_DIR:?}"
command_name="$1"
shift

case "$command_name" in
    inspect)
        sleep 0.05
        printf 'running\n'
        ;;
    secret)
        subcommand="$1"
        shift
        case "$subcommand" in
            rm)
                printf 'rm %s\n' "$*" >> "$log_root/secret-commands.log"
                ;;
            *)
                echo "unexpected podman secret subcommand: $subcommand" >&2
                exit 98
                ;;
        esac
        ;;
    *)
        echo "unexpected podman command: $command_name" >&2
        exit 99
        ;;
esac
"#,
        );

        let mut child = Command::new("sh")
            .args(["-c", "sleep 0.02"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("child process should start");

        let result = fixture.run_with_fake_podman_env(|| {
            wait_for_container_exit(
                &mut child,
                "container",
                &[SecretBinding {
                    secret_name: "secret".to_string(),
                    target_name: "GITHUB_TOKEN".to_string(),
                }],
                Some(Duration::from_millis(10)),
            )
        });

        let status = result
            .expect("wait should succeed")
            .expect("completed child should win over timeout");
        assert_eq!(status.code(), Some(0));
    }

    #[test]
    fn attached_start_classifies_exit_code_125_as_runner_error() {
        let error = classify_attached_start_result_with_inspector(
            vec![
                "start".to_string(),
                "--attach".to_string(),
                "container".to_string(),
            ],
            exit_status(125),
            "podman start failed".to_string(),
            || None,
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
    fn attached_start_preserves_agent_exit_code_125_when_inspection_reports_terminal_exit() {
        let outcome = classify_attached_start_result_with_inspector(
            vec![
                "start".to_string(),
                "--attach".to_string(),
                "container".to_string(),
            ],
            exit_status(125),
            String::new(),
            || Some(SessionOutcome::Failed { exit_code: 125 }),
        )
        .expect("inspected terminal exit code should win over podman attach status");

        assert_eq!(outcome, SessionOutcome::Failed { exit_code: 125 });
    }

    #[test]
    fn attached_start_classifies_nonzero_exit_as_session_failure() {
        let outcome = classify_attached_start_result(
            vec![
                "start".to_string(),
                "--attach".to_string(),
                "container".to_string(),
            ],
            "container",
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
            "container",
            exit_status(0),
            String::new(),
        )
        .expect("successful attached starts should remain successful session outcomes");

        assert_eq!(outcome, SessionOutcome::Succeeded);
    }

    #[test]
    fn attached_start_stderr_retains_only_bounded_tail() {
        let payload = "x".repeat((64 * 1024) + 128);
        let mut forwarded = Vec::new();

        let captured =
            forward_and_capture_stderr_to(std::io::Cursor::new(payload.as_bytes()), &mut forwarded)
                .expect("stderr forwarding should succeed");

        let expected_tail = "x".repeat(64 * 1024);
        assert!(captured.starts_with("[stderr truncated to last 65536 bytes]\n"));
        assert!(captured.ends_with(&expected_tail));
        assert_eq!(
            captured.len(),
            "[stderr truncated to last 65536 bytes]\n".len() + expected_tail.len()
        );
        assert_eq!(forwarded, payload.as_bytes());
    }

    #[test]
    fn run_session_does_not_pass_resolved_environment_values_via_podman_create_arguments() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.write_script(
            r#"#!/bin/sh
set -eu

log_root="${AGENTD_FAKE_PODMAN_LOG_DIR:?}"
command_name="$1"
shift

case "$command_name" in
    secret)
        subcommand="$1"
        shift
        case "$subcommand" in
            create)
                printf 'create %s\n' "$*" >> "$log_root/secret-commands.log"
                cat > "$log_root/secret-value.log"
                ;;
            rm)
                printf 'rm %s\n' "$*" >> "$log_root/secret-commands.log"
                ;;
            *)
                echo "unexpected podman secret subcommand: $subcommand" >&2
                exit 98
                ;;
        esac
        ;;
    create)
        printf '%s\n' "$*" > "$log_root/create-args.log"
        printf 'created' > "$log_root/container-state"
        ;;
    start)
        printf 'running' > "$log_root/container-state"
        exit 0
        ;;
    rm)
        exit 0
        ;;
    inspect)
        format_value=""
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --type)
                    shift 2
                    ;;
                --format)
                    format_value="$2"
                    shift 2
                    ;;
                *)
                    shift
                    ;;
            esac
        done
        state="$(cat "$log_root/container-state" 2>/dev/null || printf 'created')"
        case "$format_value" in
            "{{.State.Status}}")
                printf '%s\n' "$state"
                ;;
            "{{.State.Status}} {{.State.ExitCode}}")
                printf '%s 0\n' "$state"
                ;;
            *)
                exit 97
                ;;
        esac
        ;;
    *)
        echo "unexpected podman command: $command_name" >&2
        exit 99
        ;;
esac
"#,
        );

        let methodology_dir = fixture.create_methodology_dir("runner-methodology");
        let outcome = fixture.run_with_fake_podman(SessionSpec {
            agent_name: "agent".to_string(),
            base_image: "image".to_string(),
            methodology_dir,
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "GITHUB_TOKEN".to_string(),
                value: "test-token".to_string(),
            }],
        });

        assert_eq!(
            outcome.expect("session should succeed with fake podman"),
            SessionOutcome::Succeeded
        );

        let create_args = fixture.read_log("create-args.log");
        assert!(
            !create_args.contains("GITHUB_TOKEN=test-token"),
            "resolved environment values must not appear in podman create args: {create_args}"
        );
        assert!(
            create_args.contains("--secret"),
            "resolved environment should be injected via podman secrets: {create_args}"
        );

        let secret_args = fixture.read_log("secret-commands.log");
        assert!(
            secret_args.contains("create"),
            "podman secret create should be invoked before container create: {secret_args}"
        );
        assert_eq!(fixture.read_log("secret-value.log"), "test-token");
    }

    #[test]
    fn run_session_reuses_one_session_identifier_for_container_stage_and_secret_names() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        let agent_name = "a".repeat(190);
        fixture.write_script(
            r#"#!/bin/sh
set -eu

log_root="${AGENTD_FAKE_PODMAN_LOG_DIR:?}"
command_name="$1"
shift

case "$command_name" in
    secret)
        subcommand="$1"
        shift
        case "$subcommand" in
            create)
                printf 'create %s\n' "$*" >> "$log_root/secret-commands.log"
                cat > /dev/null
                ;;
            rm)
                printf 'rm %s\n' "$*" >> "$log_root/secret-commands.log"
                ;;
            *)
                echo "unexpected podman secret subcommand: $subcommand" >&2
                exit 98
                ;;
        esac
        ;;
    create)
        printf '%s\n' "$*" > "$log_root/create-args.log"
        printf 'created' > "$log_root/container-state"
        ;;
    start)
        printf 'running' > "$log_root/container-state"
        exit 0
        ;;
    rm)
        exit 0
        ;;
    inspect)
        format_value=""
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --type)
                    shift 2
                    ;;
                --format)
                    format_value="$2"
                    shift 2
                    ;;
                *)
                    shift
                    ;;
            esac
        done
        state="$(cat "$log_root/container-state" 2>/dev/null || printf 'created')"
        case "$format_value" in
            "{{.State.Status}}")
                printf '%s\n' "$state"
                ;;
            "{{.State.Status}} {{.State.ExitCode}}")
                printf '%s 0\n' "$state"
                ;;
            *)
                exit 97
                ;;
        esac
        ;;
    *)
        echo "unexpected podman command: $command_name" >&2
        exit 99
        ;;
esac
"#,
        );

        let methodology_dir = fixture.create_methodology_dir("runner-methodology");
        let outcome = fixture.run_with_fake_podman(SessionSpec {
            agent_name: agent_name.clone(),
            base_image: "image".to_string(),
            methodology_dir,
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "GITHUB_TOKEN".to_string(),
                value: "test-token".to_string(),
            }],
        });

        assert_eq!(
            outcome.expect("session should succeed with fake podman"),
            SessionOutcome::Succeeded
        );

        let create_args = fixture.read_log("create-args.log");
        let container_name = argument_value(&create_args, "--name")
            .expect("podman create should receive a container name");
        let mount_value = argument_value(&create_args, "--mount")
            .expect("podman create should receive a methodology mount");
        let mount_source = mount_src_value(&mount_value).expect("mount should include src");
        let stage_dir_name = Path::new(&mount_source)
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .expect("mount source should live under the runner staging directory");
        let secret_args = fixture.read_log("secret-commands.log");
        let secret_name = secret_args
            .split_whitespace()
            .nth(1)
            .expect("secret create should include a secret name");

        let container_prefix = format!("agentd-{agent_name}-");
        let container_suffix = container_name
            .strip_prefix(&container_prefix)
            .expect("container name should include agent prefix");
        let stage_suffix = stage_dir_name
            .strip_prefix(SESSION_STAGE_PREFIX)
            .expect("staging dir should include session stage prefix");

        assert_eq!(stage_suffix, container_suffix);
        assert_eq!(
            secret_name,
            format!("{SESSION_SECRET_PREFIX}{container_suffix}-0")
        );
        assert_eq!(container_suffix.len(), 32);
        assert!(
            container_suffix.chars().all(|character| {
                character.is_ascii_digit() || ('a'..='f').contains(&character)
            })
        );
    }

    #[test]
    fn run_session_releases_session_secrets_after_container_reaches_running_state() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.write_script(
            r#"#!/bin/sh
set -eu

log_root="${AGENTD_FAKE_PODMAN_LOG_DIR:?}"
command_name="$1"
shift

case "$command_name" in
    secret)
        subcommand="$1"
        shift
        case "$subcommand" in
            create)
                printf 'create %s\n' "$*" >> "$log_root/secret-commands.log"
                cat > "$log_root/secret-value.log"
                ;;
            rm)
                printf 'rm %s\n' "$*" >> "$log_root/secret-commands.log"
                : > "$log_root/secret-removed"
                ;;
            *)
                echo "unexpected podman secret subcommand: $subcommand" >&2
                exit 98
                ;;
        esac
        ;;
    create)
        printf '%s\n' "$*" > "$log_root/create-args.log"
        printf 'created' > "$log_root/container-state"
        ;;
    start)
        printf 'running' > "$log_root/container-state"
        deadline=$(( $(date +%s) + 3 ))
        while [ ! -f "$log_root/secret-removed" ]; do
            if [ "$(date +%s)" -ge "$deadline" ]; then
                echo "secret was not removed while container was running" >&2
                exit 42
            fi
            sleep 0.1
        done
        exit 0
        ;;
    rm)
        exit 0
        ;;
    inspect)
        format_value=""
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --type)
                    shift 2
                    ;;
                --format)
                    format_value="$2"
                    shift 2
                    ;;
                *)
                    shift
                    ;;
            esac
        done
        state="$(cat "$log_root/container-state" 2>/dev/null || printf 'created')"
        case "$format_value" in
            "{{.State.Status}}")
                printf '%s\n' "$state"
                ;;
            "{{.State.Status}} {{.State.ExitCode}}")
                printf '%s 0\n' "$state"
                ;;
            *)
                echo "unexpected podman inspect format: $format_value" >&2
                exit 97
                ;;
        esac
        ;;
    *)
        echo "unexpected podman command: $command_name" >&2
        exit 99
        ;;
esac
"#,
        );

        let methodology_dir = fixture.create_methodology_dir("runner-methodology");
        let outcome = fixture.run_with_fake_podman(SessionSpec {
            agent_name: "agent".to_string(),
            base_image: "image".to_string(),
            methodology_dir,
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "GITHUB_TOKEN".to_string(),
                value: "test-token".to_string(),
            }],
        });

        assert_eq!(
            outcome.expect("session should succeed with fake podman"),
            SessionOutcome::Succeeded
        );

        let secret_args = fixture.read_log("secret-commands.log");
        assert!(
            secret_args.contains("create"),
            "podman secret create should be invoked: {secret_args}"
        );
        assert!(
            secret_args.contains("rm"),
            "podman secret rm should run after the container reaches running: {secret_args}"
        );
    }

    #[test]
    fn run_container_to_completion_reaps_attached_child_when_wait_for_container_exit_errors() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.write_script(
            r#"#!/bin/sh
set -eu

log_root="${AGENTD_FAKE_PODMAN_LOG_DIR:?}"
command_name="$1"
shift

case "$command_name" in
    start)
        printf '%s\n' "$$" > "$log_root/start.pid"
        sleep 0.3
        exit 0
        ;;
    inspect)
        echo "inspect failed while attached start was still running" >&2
        exit 41
        ;;
    *)
        echo "unexpected podman command: $command_name" >&2
        exit 99
        ;;
esac
"#,
        );

        let started_at = Instant::now();
        let error = fixture
            .run_with_fake_podman_env(|| {
                run_container_to_completion(
                    "container",
                    &[SecretBinding {
                        secret_name: "secret".to_string(),
                        target_name: "GITHUB_TOKEN".to_string(),
                    }],
                )
            })
            .expect_err("inspect failure should surface as a runner error");
        let elapsed = started_at.elapsed();

        match error {
            RunnerError::PodmanCommandFailed { args, status, .. } => {
                assert_eq!(
                    args,
                    vec![
                        "inspect".to_string(),
                        "--type".to_string(),
                        "container".to_string(),
                        "--format".to_string(),
                        "{{.State.Status}}".to_string(),
                        "container".to_string(),
                    ]
                );
                assert_eq!(status.code(), Some(41));
            }
            other => panic!("expected PodmanCommandFailed, got {other:?}"),
        }

        assert!(
            elapsed >= Duration::from_millis(100),
            "attached start should be awaited before returning wait errors, returned after {elapsed:?}"
        );

        let pid = fixture
            .read_log("start.pid")
            .trim()
            .parse::<u32>()
            .expect("fake podman start should record its pid");
        assert_process_is_reaped(pid);
    }

    #[test]
    fn run_container_with_timeout_reaps_attached_child_when_wait_for_container_exit_errors() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.write_script(
            r#"#!/bin/sh
set -eu

log_root="${AGENTD_FAKE_PODMAN_LOG_DIR:?}"
command_name="$1"
shift

case "$command_name" in
    start)
        printf '%s\n' "$$" > "$log_root/start.pid"
        sleep 0.3
        exit 0
        ;;
    inspect)
        echo "inspect failed while attached start was still running" >&2
        exit 43
        ;;
    *)
        echo "unexpected podman command: $command_name" >&2
        exit 99
        ;;
esac
"#,
        );

        let started_at = Instant::now();
        let error = fixture
            .run_with_fake_podman_env(|| {
                run_container_with_timeout(
                    "container",
                    &[SecretBinding {
                        secret_name: "secret".to_string(),
                        target_name: "GITHUB_TOKEN".to_string(),
                    }],
                    Duration::from_secs(5),
                )
            })
            .expect_err("inspect failure should surface as a runner error");
        let elapsed = started_at.elapsed();

        match error {
            RunnerError::PodmanCommandFailed { args, status, .. } => {
                assert_eq!(
                    args,
                    vec![
                        "inspect".to_string(),
                        "--type".to_string(),
                        "container".to_string(),
                        "--format".to_string(),
                        "{{.State.Status}}".to_string(),
                        "container".to_string(),
                    ]
                );
                assert_eq!(status.code(), Some(43));
            }
            other => panic!("expected PodmanCommandFailed, got {other:?}"),
        }

        assert!(
            elapsed >= Duration::from_millis(100),
            "attached start should be awaited before returning wait errors, returned after {elapsed:?}"
        );

        let pid = fixture
            .read_log("start.pid")
            .trim()
            .parse::<u32>()
            .expect("fake podman start should record its pid");
        assert_process_is_reaped(pid);
    }

    #[test]
    fn run_container_with_timeout_reaps_attached_child_when_cleanup_container_fails_after_timeout()
    {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.write_script(
            r#"#!/bin/sh
set -eu

log_root="${AGENTD_FAKE_PODMAN_LOG_DIR:?}"
command_name="$1"
shift

case "$command_name" in
    start)
        printf '%s\n' "$$" > "$log_root/start.pid"
        sleep 0.3
        exit 0
        ;;
    rm)
        echo "rm failed after timeout" >&2
        exit 47
        ;;
    inspect)
        printf 'created\n'
        ;;
    *)
        echo "unexpected podman command: $command_name" >&2
        exit 99
        ;;
esac
"#,
        );

        let started_at = Instant::now();
        let error = fixture
            .run_with_fake_podman_env(|| {
                run_container_with_timeout("container", &[], Duration::from_millis(50))
            })
            .expect_err("timeout cleanup failure should surface as a runner error");
        let elapsed = started_at.elapsed();

        match error {
            RunnerError::PodmanCommandFailed { args, status, .. } => {
                assert_eq!(
                    args,
                    vec![
                        "rm".to_string(),
                        "--force".to_string(),
                        "--ignore".to_string(),
                        "container".to_string(),
                    ]
                );
                assert_eq!(status.code(), Some(47));
            }
            other => panic!("expected PodmanCommandFailed, got {other:?}"),
        }

        assert!(
            elapsed >= Duration::from_millis(100),
            "attached start should be awaited before returning timeout cleanup errors, returned after {elapsed:?}"
        );

        let pid = fixture
            .read_log("start.pid")
            .trim()
            .parse::<u32>()
            .expect("fake podman start should record its pid");
        assert_process_is_reaped(pid);
    }

    #[test]
    fn cleanup_failure_logs_include_container_creation_stage() {
        let mut output = Vec::new();

        log_cleanup_failure_to(
            &mut output,
            "container creation",
            &RunnerError::InvalidAgentName,
        )
        .expect("cleanup failure log should be written");

        assert_eq!(
            String::from_utf8(output).expect("cleanup log should be utf-8"),
            "cleanup after container creation failed: agent_name must not be empty\n"
        );
    }

    #[test]
    fn cleanup_failure_logs_include_session_execution_stage() {
        let mut output = Vec::new();

        log_cleanup_failure_to(
            &mut output,
            "session execution",
            &RunnerError::InvalidBaseImage,
        )
        .expect("cleanup failure log should be written");

        assert_eq!(
            String::from_utf8(output).expect("cleanup log should be utf-8"),
            "cleanup after session execution failed: base_image must not be empty\n"
        );
    }

    #[test]
    fn run_session_returns_create_error_when_cleanup_after_create_also_fails() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.write_script(
            r#"#!/bin/sh
set -eu

command_name="$1"
shift

case "$command_name" in
    secret)
        subcommand="$1"
        shift
        case "$subcommand" in
            create)
                cat > /dev/null
                exit 0
                ;;
            rm)
                echo "secret cleanup failed after create failure" >&2
                exit 29
                ;;
            *)
                echo "unexpected podman secret subcommand: $subcommand" >&2
                exit 98
                ;;
        esac
        ;;
    create)
        echo "container create failed" >&2
        exit 31
        ;;
    rm)
        exit 0
        ;;
    *)
        echo "unexpected podman command: $command_name" >&2
        exit 99
        ;;
esac
"#,
        );

        let methodology_dir = fixture.create_methodology_dir("runner-methodology");
        let error = fixture
            .run_with_fake_podman_env(|| {
                run_session(
                    SessionSpec {
                        agent_name: "agent".to_string(),
                        base_image: "image".to_string(),
                        methodology_dir,
                        agent_command: vec!["codex".to_string(), "exec".to_string()],
                        environment: vec![ResolvedEnvironmentVariable {
                            name: "GITHUB_TOKEN".to_string(),
                            value: "test-token".to_string(),
                        }],
                    },
                    SessionInvocation {
                        repo_url: VALID_REMOTE_REPO_URL.to_string(),
                        work_unit: None,
                        timeout: None,
                    },
                )
            })
            .expect_err("create failure should remain the returned error");

        match error {
            RunnerError::PodmanCommandFailed { args, status, .. } => {
                assert_eq!(args.first().map(String::as_str), Some("create"));
                assert_eq!(status.code(), Some(31));
            }
            other => panic!("expected PodmanCommandFailed, got {other:?}"),
        }
    }

    #[test]
    fn run_session_returns_run_error_when_cleanup_after_run_also_fails() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.write_script(
            r#"#!/bin/sh
set -eu

log_root="${AGENTD_FAKE_PODMAN_LOG_DIR:?}"
command_name="$1"
shift

case "$command_name" in
    secret)
        subcommand="$1"
        shift
        case "$subcommand" in
            create)
                cat > /dev/null
                exit 0
                ;;
            rm)
                rm_count_file="$log_root/secret-rm-count"
                rm_count=0
                if [ -f "$rm_count_file" ]; then
                    rm_count="$(cat "$rm_count_file")"
                fi
                rm_count=$((rm_count + 1))
                printf '%s' "$rm_count" > "$rm_count_file"
                if [ "$rm_count" -gt 1 ]; then
                    echo "secret cleanup failed after run failure" >&2
                    exit 29
                fi
                exit 0
                ;;
            *)
                echo "unexpected podman secret subcommand: $subcommand" >&2
                exit 98
                ;;
        esac
        ;;
    create)
        printf 'created' > "$log_root/container-state"
        exit 0
        ;;
    start)
        printf 'running' > "$log_root/container-state"
        echo "attached start failed" >&2
        exit 125
        ;;
    rm)
        exit 0
        ;;
    inspect)
        format_value=""
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --type)
                    shift 2
                    ;;
                --format)
                    format_value="$2"
                    shift 2
                    ;;
                *)
                    shift
                    ;;
            esac
        done
        case "$format_value" in
            "{{.State.Status}}")
                printf 'running\n'
                ;;
            "{{.State.Status}} {{.State.ExitCode}}")
                printf 'running 0\n'
                ;;
            *)
                echo "unexpected podman inspect format: $format_value" >&2
                exit 97
                ;;
        esac
        ;;
    *)
        echo "unexpected podman command: $command_name" >&2
        exit 99
        ;;
esac
"#,
        );

        let methodology_dir = fixture.create_methodology_dir("runner-methodology");
        let error = fixture
            .run_with_fake_podman_env(|| {
                run_session(
                    SessionSpec {
                        agent_name: "agent".to_string(),
                        base_image: "image".to_string(),
                        methodology_dir,
                        agent_command: vec!["codex".to_string(), "exec".to_string()],
                        environment: vec![ResolvedEnvironmentVariable {
                            name: "GITHUB_TOKEN".to_string(),
                            value: "test-token".to_string(),
                        }],
                    },
                    SessionInvocation {
                        repo_url: VALID_REMOTE_REPO_URL.to_string(),
                        work_unit: None,
                        timeout: None,
                    },
                )
            })
            .expect_err("run failure should remain the returned error");

        match error {
            RunnerError::PodmanCommandFailed { args, status, .. } => {
                assert_eq!(args.first().map(String::as_str), Some("start"));
                assert_eq!(status.code(), Some(125));
            }
            other => panic!("expected PodmanCommandFailed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn podman_commands_with_input_reap_failed_children_before_returning() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.write_script(
            r#"#!/bin/sh
set -eu

log_root="${AGENTD_FAKE_PODMAN_LOG_DIR:?}"
printf '%s\n' "$$" > "$log_root/podman.pid"
exit 17
"#,
        );

        let stdin_data = vec![b'x'; 8 * 1024 * 1024];
        let error = fixture
            .run_with_fake_podman_env(|| {
                run_podman_command_with_input(
                    vec![
                        "secret".to_string(),
                        "create".to_string(),
                        "secret-name".to_string(),
                        "-".to_string(),
                    ],
                    Some(&stdin_data),
                )
            })
            .expect_err("failed podman commands should surface an error");

        match error {
            RunnerError::PodmanCommandFailed { status, .. } => {
                assert_eq!(status.code(), Some(17));
            }
            other => panic!("expected PodmanCommandFailed, got {other:?}"),
        }

        let pid = fixture
            .read_log("podman.pid")
            .trim()
            .parse::<u32>()
            .expect("fake podman script should record its pid");
        assert_process_is_reaped(pid);
    }

    fn fake_podman_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct FakePodmanFixture {
        root: PathBuf,
        log_dir: PathBuf,
        bin_dir: PathBuf,
    }

    impl FakePodmanFixture {
        fn new() -> Self {
            let root = unique_temp_dir("agentd-runner-fake-podman");
            let log_dir = root.join("logs");
            let bin_dir = root.join("bin");
            fs::create_dir_all(&log_dir).expect("log dir should be created");
            fs::create_dir_all(&bin_dir).expect("bin dir should be created");

            Self {
                root,
                log_dir,
                bin_dir,
            }
        }

        fn write_script(&self, body: &str) {
            let script_path = self.bin_dir.join("podman");
            fs::write(&script_path, body).expect("fake podman script should be written");

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let mut permissions = fs::metadata(&script_path)
                    .expect("fake podman script metadata should be available")
                    .permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&script_path, permissions)
                    .expect("fake podman script should be executable");
            }
        }

        fn create_methodology_dir(&self, name: &str) -> PathBuf {
            let methodology_dir = self.root.join(name);
            fs::create_dir_all(&methodology_dir).expect("methodology dir should be created");
            fs::write(methodology_dir.join("manifest.toml"), "name = \"test\"\n")
                .expect("methodology manifest should be written");
            methodology_dir
        }

        fn run_with_fake_podman(&self, spec: SessionSpec) -> Result<SessionOutcome, RunnerError> {
            self.run_with_fake_podman_env(|| {
                run_session(
                    spec,
                    SessionInvocation {
                        repo_url: VALID_REMOTE_REPO_URL.to_string(),
                        work_unit: None,
                        timeout: None,
                    },
                )
            })
        }

        fn run_with_fake_podman_env<T>(&self, run: impl FnOnce() -> T) -> T {
            let original_path =
                env::var_os("PATH").expect("PATH should exist for fake podman tests");
            let fake_path = env::join_paths(
                std::iter::once(self.bin_dir.clone()).chain(env::split_paths(&original_path)),
            )
            .expect("fake PATH should be constructed");

            // Test-only PATH mutation is serialized by fake_podman_lock.
            unsafe {
                env::set_var("PATH", &fake_path);
                env::set_var("AGENTD_FAKE_PODMAN_LOG_DIR", &self.log_dir);
            }

            let result = run();

            // Test-only PATH mutation is serialized by fake_podman_lock.
            unsafe {
                env::set_var("PATH", original_path);
                env::remove_var("AGENTD_FAKE_PODMAN_LOG_DIR");
            }

            result
        }

        fn read_log(&self, name: &str) -> String {
            fs::read_to_string(self.log_dir.join(name)).unwrap_or_default()
        }
    }

    fn argument_value(command_line: &str, flag: &str) -> Option<String> {
        let mut parts = command_line.split_whitespace();
        while let Some(part) = parts.next() {
            if part == flag {
                return parts.next().map(str::to_string);
            }
        }

        None
    }

    fn mount_src_value(mount: &str) -> Option<String> {
        mount
            .split(',')
            .find_map(|component| component.strip_prefix("src=").map(str::to_string))
    }

    impl Drop for FakePodmanFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    fn exit_status(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        ExitStatusExt::from_raw(code << 8)
    }

    #[cfg(unix)]
    fn assert_process_is_reaped(pid: u32) {
        let output = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("ps should run");

        if !output.status.success() {
            return;
        }

        let status = String::from_utf8(output.stdout).expect("ps output should be utf-8");
        assert!(
            !status.trim().starts_with('Z'),
            "expected process {pid} to be reaped, got state {:?}",
            status.trim()
        );
    }

    #[cfg(windows)]
    fn exit_status(code: i32) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;

        ExitStatusExt::from_raw(code as u32)
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after the unix epoch")
                .as_nanos()
        ))
    }
}
