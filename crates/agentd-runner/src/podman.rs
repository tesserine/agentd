//! Thin command layer over the podman CLI.
//!
//! All podman interaction in the runner flows through this module, providing a
//! single point of control for process spawning, stdin piping, deadline
//! enforcement, and exit-status interpretation. Tests substitute a fake podman
//! script via `PATH` manipulation rather than mocking this layer.

use crate::types::RunnerError;
use std::io::{Read, Write};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct StartedPodmanCommand {
    child: Child,
    write_error: Option<std::io::Error>,
}

pub(crate) fn run_podman_command(args: Vec<String>) -> Result<String, RunnerError> {
    run_podman_command_with_input(args, None)
}

pub(crate) fn run_podman_command_until(
    args: Vec<String>,
    deadline: Instant,
) -> Result<Option<String>, RunnerError> {
    run_podman_command_with_input_until(args, None, deadline)
}

pub(crate) fn run_podman_command_with_input(
    args: Vec<String>,
    stdin_data: Option<&[u8]>,
) -> Result<String, RunnerError> {
    let StartedPodmanCommand { child, write_error } = spawn_podman_command(&args, stdin_data)?;

    let output = child.wait_with_output()?;
    finish_podman_output(args, write_error, output)
}

pub(crate) fn run_podman_command_with_input_until(
    args: Vec<String>,
    stdin_data: Option<&[u8]>,
    deadline: Instant,
) -> Result<Option<String>, RunnerError> {
    let StartedPodmanCommand {
        mut child,
        write_error,
    } = spawn_podman_command(&args, stdin_data)?;

    loop {
        if let Some(status) = child.try_wait()? {
            let output = read_podman_output_after_exit(child, status)?;
            return finish_podman_output(args, write_error, output).map(Some);
        }

        if Instant::now() >= deadline {
            terminate_child(&mut child)?;
            return Ok(None);
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_podman_command(
    args: &[String],
    stdin_data: Option<&[u8]>,
) -> Result<StartedPodmanCommand, RunnerError> {
    let mut command = Command::new("podman");
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin_data.is_some() {
        command.stdin(Stdio::piped());
    }

    let mut child = command.spawn()?;
    let write_error = write_podman_stdin(&mut child, stdin_data);

    Ok(StartedPodmanCommand { child, write_error })
}

fn write_podman_stdin(child: &mut Child, stdin_data: Option<&[u8]>) -> Option<std::io::Error> {
    if let Some(stdin_data) = stdin_data {
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
    }
}

fn finish_podman_output(
    args: Vec<String>,
    write_error: Option<std::io::Error>,
    output: Output,
) -> Result<String, RunnerError> {
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

fn read_podman_output_after_exit(
    mut child: Child,
    status: std::process::ExitStatus,
) -> Result<Output, RunnerError> {
    let mut stdout = Vec::new();
    if let Some(mut reader) = child.stdout.take() {
        reader.read_to_end(&mut stdout)?;
    }

    let mut stderr = Vec::new();
    if let Some(mut reader) = child.stderr.take() {
        reader.read_to_end(&mut stderr)?;
    }

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Kills a child process and waits for it to be reaped.
///
/// Swallows `InvalidInput` from `kill()` as a defensive guard. Since Rust
/// 1.72, `kill()` returns `Ok(())` for already-exited processes, so the
/// leading `try_wait` check and the stdlib itself handle the benign race;
/// the `InvalidInput` arm is retained for robustness.
pub(crate) fn terminate_child(child: &mut Child) -> Result<(), RunnerError> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }

    match child.kill() {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
        Err(error) => return Err(RunnerError::Io(error)),
    }
    child.wait()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        FakePodmanFixture, FakePodmanScenario, assert_process_is_reaped, exit_status,
        fake_podman_lock,
    };

    #[test]
    fn podman_command_setup_is_defined_once_for_input_handling_paths() {
        let source = include_str!("podman.rs");
        let implementation_source = source
            .split("#[cfg(test)]")
            .next()
            .expect("podman.rs should contain implementation before tests");

        assert_eq!(
            implementation_source
                .matches("Command::new(\"podman\")")
                .count(),
            1
        );
        assert_eq!(
            implementation_source
                .matches(".stdin(Stdio::piped())")
                .count(),
            1
        );
        assert_eq!(
            implementation_source
                .matches("stdin.write_all(stdin_data)")
                .count(),
            1
        );
    }

    #[test]
    fn podman_commands_with_input_reap_failed_children_before_returning() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        let scenario = FakePodmanScenario::new().with_secret_create(
            crate::test_support::CommandBehavior::from_outcome(
                crate::test_support::CommandOutcome::new()
                    .record_pid_to("podman.pid")
                    .exit_code(17),
            ),
        );
        fixture.install(&scenario);

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

        let pid = fixture.start_pid_from("podman.pid");
        assert_process_is_reaped(pid);
        assert_eq!(exit_status(17).code(), Some(17));
    }

    #[test]
    fn podman_commands_with_input_until_reap_failed_children_before_returning() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        let scenario = FakePodmanScenario::new().with_secret_create(
            crate::test_support::CommandBehavior::from_outcome(
                crate::test_support::CommandOutcome::new()
                    .record_pid_to("podman.pid")
                    .exit_code(17),
            ),
        );
        fixture.install(&scenario);

        let stdin_data = vec![b'x'; 8 * 1024 * 1024];
        let error = fixture
            .run_with_fake_podman_env(|| {
                run_podman_command_with_input_until(
                    vec![
                        "secret".to_string(),
                        "create".to_string(),
                        "secret-name".to_string(),
                        "-".to_string(),
                    ],
                    Some(&stdin_data),
                    Instant::now() + Duration::from_secs(1),
                )
            })
            .expect_err("failed podman commands should surface an error");

        match error {
            RunnerError::PodmanCommandFailed { status, .. } => {
                assert_eq!(status.code(), Some(17));
            }
            other => panic!("expected PodmanCommandFailed, got {other:?}"),
        }

        let pid = fixture.start_pid_from("podman.pid");
        assert_process_is_reaped(pid);
        assert_eq!(exit_status(17).code(), Some(17));
    }
}
