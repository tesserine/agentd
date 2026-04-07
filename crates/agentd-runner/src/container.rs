use crate::podman::{run_podman_command, run_podman_command_until};
use crate::resources::sanitize_name;
use crate::resources::{SecretBinding, SessionResources, cleanup_podman_secrets};
use crate::types::{RunnerError, SessionInvocation, SessionOutcome, SessionSpec};
use crate::validation::runner_managed_environment;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const ATTACHED_STDERR_TAIL_LIMIT: usize = 64 * 1024;
const ATTACHED_STDERR_TRUNCATION_NOTICE: &str = "[stderr truncated to last 65536 bytes]\n";
const HOME_ROOT_DIR: &str = "/home";
const METHODOLOGY_MOUNT_PATH: &str = "/agentd/methodology";
const PODMAN_INFRASTRUCTURE_ERROR_EXIT_CODE: i32 = 125;

pub(crate) fn create_container(
    resources: &SessionResources,
    spec: &SessionSpec,
    invocation: &SessionInvocation,
) -> Result<(), RunnerError> {
    run_podman_command(build_create_container_args(resources, spec, invocation)).map(|_| ())
}

pub(crate) fn run_container_to_completion(
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
            cleanup_and_finalize_attached_start_after_wait_error(container_name, start);
            Err(error)
        }
    }
}

pub(crate) fn run_container_with_timeout(
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
        Ok(None) => match cleanup_container(container_name) {
            Ok(()) => {
                finalize_attached_start(start).map(|_| ())?;
                Ok(SessionOutcome::TimedOut)
            }
            Err(error) => {
                log_cleanup_failure("session execution", &error);
                if let Err(kill_error) = start.child.kill() {
                    log_attached_start_kill_failure("session execution", &kill_error);
                }
                if let Err(finalize_error) = finalize_attached_start(start).map(|_| ()) {
                    log_attached_start_finalization_failure("session execution", &finalize_error);
                }
                Ok(SessionOutcome::TimedOut)
            }
        },
        Err(error) => {
            cleanup_and_finalize_attached_start_after_wait_error(container_name, start);
            Err(error)
        }
    }
}

pub(crate) fn cleanup_container(container_name: &str) -> Result<(), RunnerError> {
    run_podman_command(vec![
        "rm".to_string(),
        "--force".to_string(),
        "--ignore".to_string(),
        container_name.to_string(),
    ])
    .map(|_| ())
}

pub(crate) fn log_cleanup_failure(stage: &str, error: &RunnerError) {
    let mut stderr = std::io::stderr().lock();
    let _ = log_cleanup_failure_to(&mut stderr, stage, error);
}

pub(crate) fn log_attached_start_finalization_failure(stage: &str, error: &RunnerError) {
    let mut stderr = std::io::stderr().lock();
    let _ = log_attached_start_finalization_failure_to(&mut stderr, stage, error);
}

pub(crate) fn log_attached_start_kill_failure(stage: &str, error: &std::io::Error) {
    let mut stderr = std::io::stderr().lock();
    let _ = log_attached_start_kill_failure_to(&mut stderr, stage, error);
}

fn build_container_script(spec: &SessionSpec, invocation: &SessionInvocation) -> String {
    let username = sanitize_name(&spec.agent_name);
    let home_dir = format!("{HOME_ROOT_DIR}/{username}");
    let repo_dir = format!("{home_dir}/repo");
    let user_group = format!("{username}:{username}");
    let mut script = String::from("set -eu\nuseradd --create-home --home-dir ");
    script.push_str(&shell_quote(&home_dir));
    script.push_str(" --shell /bin/sh --user-group ");
    script.push_str(&shell_quote(&username));
    script.push_str("\nrm -rf ");
    script.push_str(&shell_quote(&repo_dir));
    script.push_str("\nGIT_TERMINAL_PROMPT=0 git clone --no-hardlinks -- ");
    script.push_str(&shell_quote(&invocation.repo_url));
    script.push(' ');
    script.push_str(&shell_quote(&repo_dir));
    script.push_str("\ncd ");
    script.push_str(&shell_quote(&repo_dir));
    script.push_str("\nruna init --methodology ");
    script.push_str(&shell_quote(&format!(
        "{METHODOLOGY_MOUNT_PATH}/manifest.toml"
    )));
    script.push_str("\ncat >> .runa/config.toml <<'EOF'\n[agent]\ncommand = ");
    script.push_str(&toml_array(&spec.agent_command));
    script.push_str("\nEOF\nchown -R ");
    script.push_str(&shell_quote(&user_group));
    script.push(' ');
    script.push_str(&shell_quote(&home_dir));
    script.push_str("\nexport HOME=");
    script.push_str(&shell_quote(&home_dir));
    script.push_str("\nexec gosu ");
    script.push_str(&shell_quote(&user_group));
    script.push_str(" runa run");

    if let Some(work_unit) = &invocation.work_unit {
        script.push(' ');
        script.push_str("--work-unit ");
        script.push_str(&shell_quote(work_unit));
    }

    script
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
    let mut secret_bindings = resources.secret_bindings.iter();

    for variable in &spec.environment {
        if variable.value.is_empty() {
            args.push("--env".to_string());
            args.push(format!("{}=", variable.name));
            continue;
        }

        let binding = secret_bindings
            .next()
            .expect("non-empty environment values should have matching secret bindings");
        debug_assert_eq!(binding.target_name, variable.name);

        args.push("--secret".to_string());
        args.push(format!(
            "{},type=env,target={}",
            binding.secret_name, binding.target_name
        ));
    }
    debug_assert!(
        secret_bindings.next().is_none(),
        "all secret bindings should be consumed when building create args"
    );

    for (name, value) in runner_managed_environment(spec) {
        args.push("--env".to_string());
        args.push(format!("{name}={value}"));
    }

    args.push("--user".to_string());
    args.push("0:0".to_string());
    args.push("--entrypoint".to_string());
    args.push("/bin/sh".to_string());
    args.push(spec.base_image.clone());
    args.push("-lc".to_string());
    args.push(build_container_script(spec, invocation));

    args
}

fn cleanup_and_finalize_attached_start_after_wait_error(
    container_name: &str,
    start: AttachedPodmanStart,
) {
    if let Err(error) = cleanup_container(container_name) {
        log_cleanup_failure("session execution", &error);
    }

    if let Err(error) = finalize_attached_start(start).map(|_| ()) {
        log_attached_start_finalization_failure("session execution", &error);
    }
}

fn finalize_attached_start(
    mut start: AttachedPodmanStart,
) -> Result<(Vec<String>, String), RunnerError> {
    start.child.wait()?;
    let stderr = finish_captured_stderr(start.stderr_thread)?;
    Ok((start.args, stderr))
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

fn log_attached_start_finalization_failure_to<W>(
    writer: &mut W,
    stage: &str,
    error: &RunnerError,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "attached start finalization after {stage} failed: {error}"
    )
}

fn log_attached_start_kill_failure_to<W>(
    writer: &mut W,
    stage: &str,
    error: &std::io::Error,
) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "attached start kill after {stage} failed: {error}")
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

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if let Some(status) = child.try_wait()? {
                return Ok(Some(status));
            }
            return Ok(None);
        }

        if !secrets_released {
            let running = match deadline {
                Some(deadline) => match inspect_container_status_until(container_name, deadline)? {
                    Some(status) => status == "running",
                    None => {
                        if let Some(status) = child.try_wait()? {
                            return Ok(Some(status));
                        }
                        return Ok(None);
                    }
                },
                None => inspect_container_status(container_name)? == "running",
            };

            if running {
                match cleanup_podman_secrets(secret_bindings) {
                    Ok(()) => {}
                    Err(error) => log_cleanup_failure("secret release", &error),
                }
                secrets_released = true;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

struct AttachedPodmanStart {
    args: Vec<String>,
    child: Child,
    stderr_thread: thread::JoinHandle<std::io::Result<String>>,
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

fn inspect_container_status_until(
    container_name: &str,
    deadline: Instant,
) -> Result<Option<String>, RunnerError> {
    run_podman_command_until(
        vec![
            "inspect".to_string(),
            "--type".to_string(),
            "container".to_string(),
            "--format".to_string(),
            "{{.State.Status}}".to_string(),
            container_name.to_string(),
        ],
        deadline,
    )
    .map(|output| output.map(|output| output.trim().to_string()))
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
    let host_stderr = std::io::stderr();
    forward_and_capture_stderr_to(&mut stderr, host_stderr)
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

#[cfg(test)]
mod tests;
