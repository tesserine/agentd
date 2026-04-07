//! Session resource allocation and cleanup.
//!
//! Resources (methodology staging directory, podman secrets) are allocated as
//! a unit by [`prepare_session_resources`] and cleaned up as a unit after the
//! session completes. Partial-failure cleanup during allocation ensures no
//! leaked secrets or stale directories when resource creation fails midway.

use crate::lifecycle::{LifecycleFailureKind, log_lifecycle_failure_to};
use crate::podman::run_podman_command_with_input;
use crate::types::{RunnerError, SessionInvocation, SessionSpec};
use crate::validation::REPO_TOKEN_ENV;
use getrandom::fill as fill_random_bytes;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const METHODOLOGY_STAGE_LINK_NAME: &str = "methodology";
const SESSION_SECRET_PREFIX: &str = "agentd-secret-";
const SESSION_STAGE_PREFIX: &str = "agentd-session-stage-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretBinding {
    pub(crate) secret_name: String,
    pub(crate) target_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionResources {
    pub(crate) container_name: String,
    pub(crate) methodology_staging_dir: PathBuf,
    pub(crate) methodology_mount_source: PathBuf,
    pub(crate) environment_secret_bindings: Vec<SecretBinding>,
    pub(crate) repo_token_secret_binding: Option<SecretBinding>,
}

impl SessionResources {
    pub(crate) fn all_secret_bindings(&self) -> Vec<SecretBinding> {
        let mut bindings = self.environment_secret_bindings.clone();
        if let Some(binding) = &self.repo_token_secret_binding {
            bindings.push(binding.clone());
        }
        bindings
    }
}

pub(crate) fn unique_suffix() -> Result<String, RunnerError> {
    unique_suffix_with(|bytes| fill_random_bytes(bytes).map_err(std::io::Error::other))
}

pub(crate) fn prepare_session_resources(
    container_name: &str,
    spec: &SessionSpec,
    invocation: &SessionInvocation,
    session_id: &str,
) -> Result<SessionResources, RunnerError> {
    let mut stderr = std::io::stderr().lock();
    prepare_session_resources_with_logger(container_name, spec, invocation, session_id, &mut stderr)
}

fn prepare_session_resources_with_logger<W>(
    container_name: &str,
    spec: &SessionSpec,
    invocation: &SessionInvocation,
    session_id: &str,
    writer: &mut W,
) -> Result<SessionResources, RunnerError>
where
    W: Write,
{
    let manifest_path = spec.methodology_dir.join("manifest.toml");
    if !manifest_path.is_file() {
        return Err(RunnerError::MissingMethodologyManifest {
            path: manifest_path,
        });
    }

    let methodology_staging_dir =
        create_methodology_staging_dir(&spec.methodology_dir, session_id)?;
    let methodology_mount_source = methodology_staging_dir.join(METHODOLOGY_STAGE_LINK_NAME);
    let mut resources = SessionResources {
        container_name: container_name.to_string(),
        methodology_staging_dir,
        methodology_mount_source,
        environment_secret_bindings: Vec::new(),
        repo_token_secret_binding: None,
    };

    for (index, variable) in spec.environment.iter().enumerate() {
        if variable.value.is_empty() {
            continue;
        }

        let secret_name = format!("{SESSION_SECRET_PREFIX}{session_id}-{index}");
        if let Err(error) = create_podman_secret(&secret_name, &variable.value) {
            return Err(rollback_failed_resource_allocation(
                writer, &resources, error,
            ));
        }
        resources.environment_secret_bindings.push(SecretBinding {
            secret_name,
            target_name: variable.name.clone(),
        });
    }

    if let Some(repo_token) = invocation
        .repo_token
        .as_deref()
        .filter(|token| !token.is_empty())
    {
        let secret_name = format!("{SESSION_SECRET_PREFIX}{session_id}-repo-token");
        if let Err(error) = create_podman_secret(&secret_name, repo_token) {
            return Err(rollback_failed_resource_allocation(
                writer, &resources, error,
            ));
        }
        resources.repo_token_secret_binding = Some(SecretBinding {
            secret_name,
            target_name: REPO_TOKEN_ENV.to_string(),
        });
    }

    Ok(resources)
}

fn rollback_failed_resource_allocation<W>(
    writer: &mut W,
    resources: &SessionResources,
    error: RunnerError,
) -> RunnerError
where
    W: Write,
{
    if let Err(cleanup_error) = cleanup_podman_secrets(&resources.all_secret_bindings()) {
        let _ = log_lifecycle_failure_to(
            writer,
            LifecycleFailureKind::Cleanup,
            "session resource allocation",
            &cleanup_error,
        );
    }
    if let Err(cleanup_error) = cleanup_methodology_staging_dir(&resources.methodology_staging_dir)
    {
        let _ = log_lifecycle_failure_to(
            writer,
            LifecycleFailureKind::Cleanup,
            "session resource allocation",
            &cleanup_error,
        );
    }

    error
}

pub(crate) fn cleanup_podman_secrets(secret_bindings: &[SecretBinding]) -> Result<(), RunnerError> {
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
    crate::podman::run_podman_command(args).map(|_| ())
}

pub(crate) fn cleanup_methodology_staging_dir(path: &Path) -> Result<(), RunnerError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RunnerError::Io(error)),
    }
}

// Creates a staging directory containing a symlink to the canonical methodology
// path. The symlink indirection ensures the podman bind-mount source is always
// an absolute, canonical path free of characters that break mount syntax, even
// when the original methodology_dir is relative or contains problematic chars.
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

// Returns a base directory for session staging that is safe for podman
// bind-mount syntax. On systems where `std::env::temp_dir()` returns a path
// containing commas (e.g., certain Nix or sandbox environments), the comma
// breaks podman's comma-delimited mount option parsing. Falls back to `/tmp`
// on unix in that case.
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

#[cfg(unix)]
fn create_directory_symlink(source: &Path, destination: &Path) -> Result<(), RunnerError> {
    std::os::unix::fs::symlink(source, destination).map_err(RunnerError::Io)
}

#[cfg(windows)]
fn create_directory_symlink(source: &Path, destination: &Path) -> Result<(), RunnerError> {
    std::os::windows::fs::symlink_dir(source, destination).map_err(RunnerError::Io)
}

// Generates a 32-character hex string from 16 bytes of cryptographic randomness.
// Used to create unique container names and secret names that are unpredictable
// across sessions, preventing name collisions and name-guessing attacks.
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

#[cfg(test)]
mod tests {
    use super::prepare_session_resources;
    use crate::test_support::{
        CommandBehavior, CommandOutcome, FakePodmanFixture, FakePodmanScenario, fake_podman_lock,
        test_session_spec,
    };
    use crate::{ResolvedEnvironmentVariable, RunnerError, SessionInvocation};
    use std::env;
    use std::process::Command;

    const CHILD_MODE_ENV: &str = "AGENTD_RUNNER_RESOURCE_TEST_CHILD";

    #[test]
    fn prepare_session_resources_logs_cleanup_failure_after_environment_secret_create_failure() {
        if env::var_os(CHILD_MODE_ENV).as_deref() == Some("environment-secret".as_ref()) {
            run_environment_secret_create_failure_scenario();
            return;
        }

        let output = run_current_test_as_child(
            "resources::tests::prepare_session_resources_logs_cleanup_failure_after_environment_secret_create_failure",
            "environment-secret",
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("cleanup after session resource allocation failed:"),
            "expected cleanup failure log in child stderr, got: {stderr}"
        );
        assert!(
            stderr.contains("secret cleanup failed after create failure"),
            "expected secret cleanup stderr in child stderr, got: {stderr}"
        );
    }

    #[test]
    fn prepare_session_resources_logs_cleanup_failure_after_repo_token_secret_create_failure() {
        if env::var_os(CHILD_MODE_ENV).as_deref() == Some("repo-token".as_ref()) {
            run_repo_token_secret_create_failure_scenario();
            return;
        }

        let output = run_current_test_as_child(
            "resources::tests::prepare_session_resources_logs_cleanup_failure_after_repo_token_secret_create_failure",
            "repo-token",
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("cleanup after session resource allocation failed:"),
            "expected cleanup failure log in child stderr, got: {stderr}"
        );
        assert!(
            stderr.contains("secret cleanup failed after repo token create failure"),
            "expected secret cleanup stderr in child stderr, got: {stderr}"
        );
    }

    fn run_current_test_as_child(test_name: &str, child_mode: &str) -> std::process::Output {
        let output = Command::new(env::current_exe().expect("current test binary should exist"))
            .env(CHILD_MODE_ENV, child_mode)
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .output()
            .expect("child test process should start");

        assert!(
            output.status.success(),
            "child test should succeed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        output
    }

    fn run_environment_secret_create_failure_scenario() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_secret_create(CommandBehavior::sequence(vec![
                    CommandOutcome::new()
                        .append_args_with_prefix("secret-commands.log", "create")
                        .capture_stdin_to("secret-value.log"),
                    CommandOutcome::new()
                        .append_args_with_prefix("secret-commands.log", "create")
                        .stderr("secret create failed")
                        .exit_code(37),
                ]))
                .with_secret_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new()
                        .append_args_with_prefix("secret-commands.log", "rm")
                        .stderr("secret cleanup failed after create failure")
                        .exit_code(41),
                )),
        );

        let methodology_dir = fixture.create_methodology_dir("runner-methodology");
        let session_id = format!("session-env-{}", std::process::id());
        let result = fixture.run_with_fake_podman_env(|| {
            prepare_session_resources(
                "agentd-agent-session",
                &crate::SessionSpec {
                    methodology_dir,
                    environment: vec![
                        ResolvedEnvironmentVariable {
                            name: "GITHUB_TOKEN".to_string(),
                            value: "test-token".to_string(),
                        },
                        ResolvedEnvironmentVariable {
                            name: "SECOND_SECRET".to_string(),
                            value: "second-token".to_string(),
                        },
                    ],
                    ..test_session_spec()
                },
                &SessionInvocation {
                    repo_url: "https://example.com/repo.git".to_string(),
                    repo_token: None,
                    work_unit: None,
                    timeout: None,
                },
                &session_id,
            )
        });

        match result.expect_err("second secret create should fail") {
            RunnerError::PodmanCommandFailed {
                args,
                status,
                stderr,
            } => {
                assert_eq!(
                    args,
                    vec![
                        "secret".to_string(),
                        "create".to_string(),
                        format!("agentd-secret-{session_id}-1"),
                        "-".to_string(),
                    ]
                );
                assert_eq!(status.code(), Some(37));
                assert_eq!(stderr.trim(), "secret create failed");
            }
            other => panic!("expected PodmanCommandFailed, got {other:?}"),
        }
    }

    fn run_repo_token_secret_create_failure_scenario() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_secret_create(CommandBehavior::sequence(vec![
                    CommandOutcome::new()
                        .append_args_with_prefix("secret-commands.log", "create")
                        .capture_stdin_to("secret-value.log"),
                    CommandOutcome::new()
                        .append_args_with_prefix("secret-commands.log", "create")
                        .stderr("repo token secret create failed")
                        .exit_code(43),
                ]))
                .with_secret_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new()
                        .append_args_with_prefix("secret-commands.log", "rm")
                        .stderr("secret cleanup failed after repo token create failure")
                        .exit_code(47),
                )),
        );

        let methodology_dir = fixture.create_methodology_dir("runner-methodology");
        let session_id = format!("session-repo-token-{}", std::process::id());
        let result = fixture.run_with_fake_podman_env(|| {
            prepare_session_resources(
                "agentd-agent-session",
                &crate::SessionSpec {
                    methodology_dir,
                    environment: vec![ResolvedEnvironmentVariable {
                        name: "GITHUB_TOKEN".to_string(),
                        value: "test-token".to_string(),
                    }],
                    ..test_session_spec()
                },
                &SessionInvocation {
                    repo_url: "https://example.com/repo.git".to_string(),
                    repo_token: Some("repo-token".to_string()),
                    work_unit: None,
                    timeout: None,
                },
                &session_id,
            )
        });

        match result.expect_err("repo token secret create should fail") {
            RunnerError::PodmanCommandFailed {
                args,
                status,
                stderr,
            } => {
                assert_eq!(
                    args,
                    vec![
                        "secret".to_string(),
                        "create".to_string(),
                        format!("agentd-secret-{session_id}-repo-token"),
                        "-".to_string(),
                    ]
                );
                assert_eq!(status.code(), Some(43));
                assert_eq!(stderr.trim(), "repo token secret create failed");
            }
            other => panic!("expected PodmanCommandFailed, got {other:?}"),
        }
    }
}
