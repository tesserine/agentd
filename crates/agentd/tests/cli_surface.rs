use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use agentd::config::Config;
use agentd::{SessionExecutor, run_daemon_until_shutdown};
use agentd_runner::{InvocationInput, RunnerError, SessionInvocation, SessionOutcome, SessionSpec};
use serde_json::json;

type DaemonHandle = thread::JoinHandle<Result<(), agentd::DaemonError>>;
type RecordedInvocations = Arc<Mutex<Vec<SessionInvocation>>>;

#[derive(Clone)]
struct FixedOutcomeExecutor {
    outcome: SessionOutcome,
}

impl SessionExecutor for FixedOutcomeExecutor {
    fn run_session(
        &self,
        _spec: SessionSpec,
        _invocation: SessionInvocation,
    ) -> Result<SessionOutcome, RunnerError> {
        Ok(self.outcome.clone())
    }
}

#[derive(Clone)]
struct RecordingInvocationExecutor {
    outcome: SessionOutcome,
    invocations: Arc<Mutex<Vec<SessionInvocation>>>,
}

impl RecordingInvocationExecutor {
    fn new(outcome: SessionOutcome) -> (Self, Arc<Mutex<Vec<SessionInvocation>>>) {
        let invocations = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                outcome,
                invocations: Arc::clone(&invocations),
            },
            invocations,
        )
    }
}

impl SessionExecutor for RecordingInvocationExecutor {
    fn run_session(
        &self,
        _spec: SessionSpec,
        invocation: SessionInvocation,
    ) -> Result<SessionOutcome, RunnerError> {
        self.invocations
            .lock()
            .expect("recorded invocations should lock")
            .push(invocation);
        Ok(self.outcome.clone())
    }
}

fn write_temp_config(name: &str, contents: &str) -> PathBuf {
    let unique = format!(
        "agentd-cli-test-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(unique);

    std::fs::create_dir_all(&dir).expect("temp test directory should be created");

    let path = dir.join("agentd.toml");
    std::fs::write(&path, contents).expect("temp config should be written");
    path
}

fn daemon_test_config(socket_path: &Path, pid_file: &Path) -> String {
    format!(
        r#"
[daemon]
socket_path = "{socket_path}"
pid_file = "{pid_file}"

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
"#,
        socket_path = socket_path.display(),
        pid_file = pid_file.display()
    )
}

fn daemon_test_config_with_default_repo(socket_path: &Path, pid_file: &Path, repo: &str) -> String {
    format!(
        r#"
[daemon]
socket_path = "{socket_path}"
pid_file = "{pid_file}"

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"
repo = "{repo}"

command = ["site-builder", "exec"]
"#,
        socket_path = socket_path.display(),
        pid_file = pid_file.display(),
        repo = repo
    )
}

fn daemon_test_config_with_credential(socket_path: &Path, pid_file: &Path) -> String {
    format!(
        r#"
[daemon]
socket_path = "{socket_path}"
pid_file = "{pid_file}"

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]

[[profiles.credentials]]
name = "GITHUB_TOKEN"
source = "AGENTD_GITHUB_TOKEN"
"#,
        socket_path = socket_path.display(),
        pid_file = pid_file.display()
    )
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }

    panic!("timed out waiting for {}", path.display());
}

fn terminate(child: &mut Child) -> io::Result<()> {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()?;

    assert!(status.success(), "kill should succeed");
    Ok(())
}

fn start_test_daemon(
    config_path: &Path,
    outcome: SessionOutcome,
) -> (Arc<AtomicBool>, DaemonHandle, Config) {
    let config = Config::load(config_path).expect("test config should load");
    let shutdown = Arc::new(AtomicBool::new(false));
    let daemon_config = config.clone();
    let daemon_shutdown = shutdown.clone();
    let executor = FixedOutcomeExecutor { outcome };
    let handle =
        thread::spawn(move || run_daemon_until_shutdown(daemon_config, executor, daemon_shutdown));
    wait_for_path(config.daemon().socket_path());
    (shutdown, handle, config)
}

fn start_recording_test_daemon(
    config_path: &Path,
    outcome: SessionOutcome,
) -> (Arc<AtomicBool>, DaemonHandle, Config, RecordedInvocations) {
    let config = Config::load(config_path).expect("test config should load");
    let shutdown = Arc::new(AtomicBool::new(false));
    let daemon_config = config.clone();
    let daemon_shutdown = shutdown.clone();
    let (executor, invocations) = RecordingInvocationExecutor::new(outcome);
    let handle =
        thread::spawn(move || run_daemon_until_shutdown(daemon_config, executor, daemon_shutdown));
    wait_for_path(config.daemon().socket_path());
    (shutdown, handle, config, invocations)
}

#[test]
fn binary_without_subcommand_starts_daemon_mode() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "daemon-default-command",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
        ])
        .env("AGENTD_LOG_FORMAT", "text")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("agentd binary should spawn");

    wait_for_path(&socket_path);
    wait_for_path(&pid_file);
    terminate(&mut child).expect("daemon should accept SIGTERM");
    let output = child
        .wait_with_output()
        .expect("daemon output should be available");

    assert!(
        output.status.success(),
        "daemon should exit cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("\"event\":\"agentd.logging_format_invalid\""),
        "expected tracing bootstrap warning, got: {stderr}"
    );
}

#[test]
fn binary_run_command_reports_clear_error_when_daemon_is_not_running() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-not-running-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
        ])
        .output()
        .expect("agentd binary should run");

    assert!(
        !output.status.success(),
        "run command should fail without daemon"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("agentd is not running"),
        "expected daemon-not-running error, got: {stderr}"
    );
}

#[test]
fn binary_run_command_uses_profile_default_repo_when_repo_argument_is_omitted() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-default-repo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let default_repo = "https://example.com/default.git";
    let config_path = write_temp_config(
        "client-command-default-repo",
        &daemon_test_config_with_default_repo(&socket_path, &pid_file, default_repo),
    );

    let (shutdown, handle, _config, invocations) =
        start_recording_test_daemon(&config_path, SessionOutcome::Success { exit_code: 0 });

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
        ])
        .output()
        .expect("agentd binary should run");

    shutdown.store(true, Ordering::Release);
    handle
        .join()
        .expect("daemon thread should join")
        .expect("daemon should exit cleanly");

    assert!(
        output.status.success(),
        "run command should succeed with a profile default repo: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        invocations.lock().expect("invocations should lock")[0].repo_url,
        default_repo
    );
}

#[test]
fn binary_run_command_prefers_explicit_repo_over_profile_default_repo() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-explicit-repo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-explicit-repo",
        &daemon_test_config_with_default_repo(
            &socket_path,
            &pid_file,
            "https://example.com/default.git",
        ),
    );
    let explicit_repo = "https://example.com/override.git";

    let (shutdown, handle, _config, invocations) =
        start_recording_test_daemon(&config_path, SessionOutcome::Success { exit_code: 0 });

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            explicit_repo,
        ])
        .output()
        .expect("agentd binary should run");

    shutdown.store(true, Ordering::Release);
    handle
        .join()
        .expect("daemon thread should join")
        .expect("daemon should exit cleanly");

    assert!(
        output.status.success(),
        "run command should succeed with an explicit repo override: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        invocations.lock().expect("invocations should lock")[0].repo_url,
        explicit_repo
    );
}

#[test]
fn binary_run_command_rejects_request_when_work_unit_is_also_supplied() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-request-work-unit-conflict-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-request-work-unit-conflict",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
            "--work-unit",
            "issue-81",
            "--request",
            "Add a status page",
        ])
        .output()
        .expect("agentd binary should run");

    assert!(
        !output.status.success(),
        "run command should reject conflicting request/work-unit flags"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("--request") && stderr.contains("--work-unit"),
        "expected clap conflict mentioning both flags, got: {stderr}"
    );
}

#[test]
fn binary_run_command_requires_artifact_type_when_artifact_file_is_supplied() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-artifact-type-required-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-artifact-type-required",
        &daemon_test_config(&socket_path, &pid_file),
    );
    let artifact_path = runtime_dir.join("request.json");
    std::fs::write(
        &artifact_path,
        r#"{"description":"Add a status page","source":"operator"}"#,
    )
    .expect("artifact file should be written");

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
            "--artifact-file",
            artifact_path
                .to_str()
                .expect("artifact path should be utf-8"),
        ])
        .output()
        .expect("agentd binary should run");

    assert!(
        !output.status.success(),
        "run command should reject artifact files without an artifact type"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("--artifact-type"),
        "expected missing-artifact-type error, got: {stderr}"
    );
}

#[test]
fn binary_run_command_forwards_request_text_as_typed_invocation_input() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-request-input-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-request-input",
        &daemon_test_config(&socket_path, &pid_file),
    );
    let (shutdown, handle, _config, invocations) =
        start_recording_test_daemon(&config_path, SessionOutcome::Success { exit_code: 0 });

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
            "--request",
            "Add a status page",
        ])
        .output()
        .expect("agentd binary should run");

    shutdown.store(true, Ordering::Release);
    handle
        .join()
        .expect("daemon thread should join")
        .expect("daemon should exit cleanly");

    assert!(
        output.status.success(),
        "run command should succeed with request input: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let invocation = invocations.lock().expect("invocations should lock")[0].clone();
    assert_eq!(
        invocation.input,
        Some(InvocationInput::RequestText {
            description: "Add a status page".to_string(),
        })
    );
}

#[test]
fn binary_run_command_reads_artifact_file_and_forwards_structured_input() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-artifact-input-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-artifact-input",
        &daemon_test_config(&socket_path, &pid_file),
    );
    let artifact_path = runtime_dir.join("request.json");
    std::fs::write(
        &artifact_path,
        r#"{"description":"Add a status page","source":"operator"}"#,
    )
    .expect("artifact file should be written");
    let (shutdown, handle, _config, invocations) =
        start_recording_test_daemon(&config_path, SessionOutcome::Success { exit_code: 0 });

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
            "--artifact-type",
            "request",
            "--artifact-file",
            artifact_path
                .to_str()
                .expect("artifact path should be utf-8"),
        ])
        .output()
        .expect("agentd binary should run");

    shutdown.store(true, Ordering::Release);
    handle
        .join()
        .expect("daemon thread should join")
        .expect("daemon should exit cleanly");

    assert!(
        output.status.success(),
        "run command should succeed with artifact input: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let invocation = invocations.lock().expect("invocations should lock")[0].clone();
    assert_eq!(
        invocation.input,
        Some(InvocationInput::Artifact {
            artifact_type: "request".to_string(),
            artifact_id: "request".to_string(),
            document: json!({
                "description": "Add a status page",
                "source": "operator",
            }),
        })
    );
}

#[test]
fn binary_run_command_reports_clear_error_when_repo_is_missing_from_cli_and_profile() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-missing-repo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-missing-repo",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
        ])
        .output()
        .expect("agentd binary should run");

    assert!(
        !output.status.success(),
        "run command should fail when no repo is available"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("requires a repo argument or configured profile repo"),
        "expected missing-repo error, got: {stderr}"
    );
}

#[test]
fn binary_run_command_reports_unknown_profile_when_repo_argument_is_omitted() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-unknown-profile-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-unknown-profile",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "unknown-profile",
        ])
        .output()
        .expect("agentd binary should run");

    assert!(
        !output.status.success(),
        "run command should fail for an unknown profile"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("unknown profile 'unknown-profile'"),
        "expected unknown-profile error, got: {stderr}"
    );
    assert!(
        !stderr.contains("requires a repo argument or configured profile repo"),
        "unknown-profile failure should not be reported as missing repo: {stderr}"
    );
}

#[test]
fn binary_run_command_exits_non_zero_and_reports_failed_sessions_on_stderr() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-failed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-failed",
        &daemon_test_config_with_credential(&socket_path, &pid_file),
    );

    unsafe {
        std::env::set_var("AGENTD_GITHUB_TOKEN", "runtime-secret");
    }
    let (shutdown, handle, _config) = start_test_daemon(
        &config_path,
        SessionOutcome::GenericFailure { exit_code: 23 },
    );

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
        ])
        .output()
        .expect("agentd binary should run");

    shutdown.store(true, Ordering::Release);
    handle
        .join()
        .expect("daemon thread should join")
        .expect("daemon should exit cleanly");
    unsafe {
        std::env::remove_var("AGENTD_GITHUB_TOKEN");
    }

    assert!(
        !output.status.success(),
        "run command should fail when the daemon reports a failed session"
    );
    assert!(
        String::from_utf8(output.stdout)
            .expect("stdout should be valid UTF-8")
            .is_empty(),
        "failed run should not print a success-style stdout message"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("session generic_failure (exit code 23)"),
        "expected failed-session error on stderr, got: {stderr}"
    );
}

#[test]
fn binary_run_command_exits_non_zero_and_reports_timed_out_sessions_on_stderr() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-timeout-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-timeout",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let (shutdown, handle, _config) = start_test_daemon(&config_path, SessionOutcome::TimedOut);

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
        ])
        .output()
        .expect("agentd binary should run");

    shutdown.store(true, Ordering::Release);
    handle
        .join()
        .expect("daemon thread should join")
        .expect("daemon should exit cleanly");

    assert!(
        !output.status.success(),
        "run command should fail when the daemon reports a timed-out session"
    );
    assert!(
        String::from_utf8(output.stdout)
            .expect("stdout should be valid UTF-8")
            .is_empty(),
        "timed-out run should not print a success-style stdout message"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("session timed out"),
        "expected timed-out session error on stderr, got: {stderr}"
    );
}

#[test]
fn binary_run_command_exits_non_zero_and_reports_signal_terminated_sessions_on_stderr() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-signaled-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-signaled",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let (shutdown, handle, _config) = start_test_daemon(
        &config_path,
        SessionOutcome::TerminatedBySignal {
            exit_code: 130,
            signal: 2,
        },
    );

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
        ])
        .output()
        .expect("agentd binary should run");

    shutdown.store(true, Ordering::Release);
    handle
        .join()
        .expect("daemon thread should join")
        .expect("daemon should exit cleanly");

    assert!(
        !output.status.success(),
        "run command should fail when the daemon reports a signal-terminated session"
    );
    assert!(
        String::from_utf8(output.stdout)
            .expect("stdout should be valid UTF-8")
            .is_empty(),
        "signal-terminated run should not print a success-style stdout message"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("session terminated_by_signal (exit code 130, signal 2)"),
        "expected signal-terminated session error on stderr, got: {stderr}"
    );
}

#[test]
fn binary_run_command_exits_zero_and_reports_blocked_sessions_on_stdout() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-blocked-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-blocked",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let (shutdown, handle, _config) =
        start_test_daemon(&config_path, SessionOutcome::Blocked { exit_code: 3 });

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
        ])
        .output()
        .expect("agentd binary should run");

    shutdown.store(true, Ordering::Release);
    handle
        .join()
        .expect("daemon thread should join")
        .expect("daemon should exit cleanly");

    assert!(
        output.status.success(),
        "run command should treat blocked as a normal terminal state: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout should be valid UTF-8"),
        "session blocked\n"
    );
    assert!(
        String::from_utf8(output.stderr)
            .expect("stderr should be valid UTF-8")
            .is_empty(),
        "blocked run should not print an error-style stderr message"
    );
}

#[test]
fn binary_run_command_exits_zero_and_reports_nothing_ready_sessions_on_stdout() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-nothing-ready-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-nothing-ready",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let (shutdown, handle, _config) =
        start_test_daemon(&config_path, SessionOutcome::NothingReady { exit_code: 4 });

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
        ])
        .output()
        .expect("agentd binary should run");

    shutdown.store(true, Ordering::Release);
    handle
        .join()
        .expect("daemon thread should join")
        .expect("daemon should exit cleanly");

    assert!(
        output.status.success(),
        "run command should treat nothing_ready as a normal terminal state: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout should be valid UTF-8"),
        "session nothing_ready\n"
    );
    assert!(
        String::from_utf8(output.stderr)
            .expect("stderr should be valid UTF-8")
            .is_empty(),
        "nothing_ready run should not print an error-style stderr message"
    );
}

#[test]
fn binary_run_command_succeeds_when_profile_registry_becomes_invalid_after_daemon_start() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-invalid-registry-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command-invalid-registry-after-start",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let (shutdown, handle, _config) =
        start_test_daemon(&config_path, SessionOutcome::Success { exit_code: 0 });
    std::fs::write(
        &config_path,
        format!(
            r#"
[daemon]
socket_path = "{socket_path}"
pid_file = "{pid_file}"

[[profiles]]
name = "Site-Builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
"#,
            socket_path = socket_path.display(),
            pid_file = pid_file.display()
        ),
    )
    .expect("config should be rewritten");

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "site-builder",
            "https://example.com/repo.git",
        ])
        .output()
        .expect("agentd binary should run");

    shutdown.store(true, Ordering::Release);
    handle
        .join()
        .expect("daemon thread should join")
        .expect("daemon should exit cleanly");

    assert!(
        output.status.success(),
        "run command should still succeed while daemon is healthy: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout should be valid UTF-8"),
        "session success\n"
    );
}
