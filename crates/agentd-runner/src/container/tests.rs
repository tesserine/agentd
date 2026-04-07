use super::*;
use crate::resources::SessionResources;
use crate::test_support::{
    CommandBehavior, CommandOutcome, FakePodmanFixture, FakePodmanScenario, InspectBehavior,
    exit_status, fake_podman_lock, test_session_spec,
};
use crate::{ResolvedEnvironmentVariable, SessionInvocation};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const VALID_REMOTE_REPO_URL: &str = "https://example.com/agentd.git";

#[test]
fn create_container_args_include_shared_relabel_for_methodology_mount() {
    let args = build_create_container_args(
        &SessionResources {
            container_name: "agentd-agent-session".to_string(),
            methodology_staging_dir: PathBuf::from("/tmp/staging"),
            methodology_mount_source: PathBuf::from("/tmp/staging/methodology"),
            secret_bindings: Vec::new(),
        },
        &test_session_spec(),
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
fn create_container_args_force_root_user_and_entrypoint_before_image_argument() {
    let args = build_create_container_args(
        &SessionResources {
            container_name: "agentd-agent-session".to_string(),
            methodology_staging_dir: PathBuf::from("/tmp/staging"),
            methodology_mount_source: PathBuf::from("/tmp/staging/methodology"),
            secret_bindings: Vec::new(),
        },
        &test_session_spec(),
        &SessionInvocation {
            repo_url: VALID_REMOTE_REPO_URL.to_string(),
            work_unit: None,
            timeout: None,
        },
    );

    let user_index = args
        .iter()
        .position(|arg| arg == "--user")
        .expect("podman create should receive --user");
    assert_eq!(args.get(user_index + 1).map(String::as_str), Some("0:0"));
    let entrypoint_index = args
        .iter()
        .position(|arg| arg == "--entrypoint")
        .expect("podman create should receive --entrypoint");
    assert_eq!(
        args.get(entrypoint_index + 1).map(String::as_str),
        Some("/bin/sh")
    );

    let image_index = args
        .iter()
        .position(|arg| arg == "image")
        .expect("podman create should include the base image");
    assert!(user_index < image_index && entrypoint_index < image_index);
}

#[test]
fn create_container_args_pass_shell_flags_after_image_argument() {
    let spec = test_session_spec();
    let invocation = SessionInvocation {
        repo_url: VALID_REMOTE_REPO_URL.to_string(),
        work_unit: None,
        timeout: None,
    };
    let args = build_create_container_args(
        &SessionResources {
            container_name: "agentd-agent-session".to_string(),
            methodology_staging_dir: PathBuf::from("/tmp/staging"),
            methodology_mount_source: PathBuf::from("/tmp/staging/methodology"),
            secret_bindings: Vec::new(),
        },
        &spec,
        &invocation,
    );
    let expected_script = build_container_script(&spec, &invocation);

    let image_index = args
        .iter()
        .position(|arg| arg == "image")
        .expect("podman create should include the base image");
    assert_eq!(args.get(image_index + 1).map(String::as_str), Some("-lc"));
    assert_eq!(
        args.get(image_index + 2).map(String::as_str),
        Some(expected_script.as_str())
    );
}

#[test]
fn build_container_script_terminates_git_clone_options_before_repo_url() {
    let script = build_container_script(
        &test_session_spec(),
        &SessionInvocation {
            repo_url: "-repo.git".to_string(),
            work_unit: None,
            timeout: None,
        },
    );

    assert!(script.contains("git clone --no-hardlinks -- '-repo.git' '/home/agent/repo'"));
}

#[test]
fn build_container_script_disables_git_terminal_prompts() {
    let script = build_container_script(
        &test_session_spec(),
        &SessionInvocation {
            repo_url: VALID_REMOTE_REPO_URL.to_string(),
            work_unit: None,
            timeout: None,
        },
    );

    assert!(script.contains("GIT_TERMINAL_PROMPT=0 git clone --no-hardlinks -- "));
}

#[test]
fn build_container_script_creates_home_workspace_and_execs_runa_from_repo_as_unprivileged_user() {
    let script = build_container_script(
        &crate::SessionSpec {
            agent_name: "agent_name".to_string(),
            ..test_session_spec()
        },
        &SessionInvocation {
            repo_url: VALID_REMOTE_REPO_URL.to_string(),
            work_unit: Some("task-42".to_string()),
            timeout: None,
        },
    );

    assert!(script.contains("useradd --create-home --home-dir '/home/agent_name' --shell /bin/sh --user-group 'agent_name'"));
    assert!(script.contains(
        "git clone --no-hardlinks -- 'https://example.com/agentd.git' '/home/agent_name/repo'"
    ));
    assert!(script.contains("\ncd '/home/agent_name/repo'\n"));
    assert!(script.contains("runa init --methodology '/agentd/methodology/manifest.toml'"));
    assert!(script.contains("cat >> .runa/config.toml <<'EOF'"));
    assert!(script.contains("\nchown -R 'agent_name:agent_name' '/home/agent_name'\n"));
    assert!(script.contains("\nexport HOME='/home/agent_name'\n"));
    assert!(script.contains("exec gosu 'agent_name:agent_name' runa run --work-unit 'task-42'"));
}

#[test]
fn toml_string_escapes_control_characters_into_valid_toml() {
    let original = "before\x08middle\x0cafter\0tail";
    let encoded = toml_string(original);
    let document = format!("value = {encoded}\n");
    let parsed: toml::Value =
        toml::from_str(&document).expect("escaped string should parse as TOML");

    assert_eq!(
        parsed.get("value").and_then(toml::Value::as_str),
        Some(original)
    );
}

#[test]
fn wait_for_container_exit_checks_child_status_again_after_timeout_boundary() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(
        &FakePodmanScenario::new()
            .with_secret_rm(CommandBehavior::from_outcome(
                CommandOutcome::new().append_args_with_prefix("secret-commands.log", "rm"),
            ))
            .with_inspect(
                InspectBehavior::new()
                    .sleep_before(Duration::from_millis(200))
                    .status_fixed("running"),
            ),
    );

    let mut child = Command::new("sh")
        .args(["-c", "sleep 0.02"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("child process should start");

    let started_at = Instant::now();
    let result = fixture.run_with_fake_podman_env(|| {
        wait_for_container_exit(
            &mut child,
            "container",
            &[SecretBinding {
                secret_name: "secret".to_string(),
                target_name: "GITHUB_TOKEN".to_string(),
            }],
            Some(Duration::from_millis(50)),
        )
    });
    let elapsed = started_at.elapsed();

    let status = result
        .expect("wait should succeed")
        .expect("completed child should win over timeout");
    assert_eq!(status.code(), Some(0));
    assert!(elapsed < Duration::from_millis(150));
    assert_eq!(fixture.secret_commands(), "");
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
                    "container".to_string()
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
fn logs_cleanup_failures_with_cleanup_prefix() {
    let mut output = Vec::new();

    log_container_lifecycle_failure_to(
        &mut output,
        ContainerLifecycleFailureKind::Cleanup,
        "session execution",
        &RunnerError::InvalidBaseImage,
    )
    .expect("logging to an in-memory buffer should succeed");

    assert_eq!(
        String::from_utf8(output).expect("log output should be valid UTF-8"),
        "cleanup after session execution failed: base_image must not be empty\n"
    );
}

#[test]
fn logs_attached_start_finalization_failures_with_finalization_prefix() {
    let mut output = Vec::new();

    log_container_lifecycle_failure_to(
        &mut output,
        ContainerLifecycleFailureKind::AttachedStartFinalization,
        "session execution",
        &RunnerError::InvalidAgentCommand,
    )
    .expect("logging to an in-memory buffer should succeed");

    assert_eq!(
        String::from_utf8(output).expect("log output should be valid UTF-8"),
        "attached start finalization after session execution failed: agent_command must contain at least one argument\n"
    );
}

#[test]
fn logs_attached_start_kill_failures_with_kill_prefix() {
    let mut output = Vec::new();
    let error = std::io::Error::other("kill failed");

    log_container_lifecycle_failure_to(
        &mut output,
        ContainerLifecycleFailureKind::AttachedStartKill,
        "session execution",
        &error,
    )
    .expect("logging to an in-memory buffer should succeed");

    assert_eq!(
        String::from_utf8(output).expect("log output should be valid UTF-8"),
        "attached start kill after session execution failed: kill failed\n"
    );
}

#[test]
fn fake_podman_scenario_records_create_arguments_for_a_successful_session() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    let scenario = FakePodmanScenario::new();
    fixture.install(&scenario);

    let methodology_dir = fixture.create_methodology_dir("runner-methodology");
    let outcome = fixture.run_with_fake_podman(crate::SessionSpec {
        methodology_dir,
        environment: vec![ResolvedEnvironmentVariable {
            name: "GITHUB_TOKEN".to_string(),
            value: "test-token".to_string(),
        }],
        ..test_session_spec()
    });

    assert_eq!(
        outcome.expect("session should succeed with fake podman"),
        SessionOutcome::Succeeded
    );
    assert!(fixture.create_args().contains("--name"));
}

#[test]
fn run_session_does_not_pass_resolved_environment_values_via_podman_create_arguments() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(&FakePodmanScenario::new());

    let methodology_dir = fixture.create_methodology_dir("runner-methodology");
    let outcome = fixture.run_with_fake_podman(crate::SessionSpec {
        methodology_dir,
        environment: vec![ResolvedEnvironmentVariable {
            name: "GITHUB_TOKEN".to_string(),
            value: "test-token".to_string(),
        }],
        ..test_session_spec()
    });

    assert_eq!(
        outcome.expect("session should succeed with fake podman"),
        SessionOutcome::Succeeded
    );

    let create_args = fixture.create_args();
    assert!(!create_args.contains("GITHUB_TOKEN=test-token"));
    assert!(create_args.contains("--secret"));

    let secret_args = fixture.secret_commands();
    assert!(secret_args.contains("create"));
    assert_eq!(fixture.read_log("secret-value.log"), "test-token");
}

#[test]
fn run_session_injects_empty_environment_values_via_direct_env_args() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(
        &FakePodmanScenario::new().with_secret_create(CommandBehavior::from_outcome(
            CommandOutcome::new()
                .append_args_with_prefix("secret-commands.log", "create")
                .capture_stdin_to("secret-value.log")
                .reject_empty_stdin("secret data must be larger than 0", 96),
        )),
    );

    let methodology_dir = fixture.create_methodology_dir("runner-methodology");
    let outcome = fixture.run_with_fake_podman(crate::SessionSpec {
        methodology_dir,
        environment: vec![
            ResolvedEnvironmentVariable {
                name: "EMPTY_VALUE".to_string(),
                value: String::new(),
            },
            ResolvedEnvironmentVariable {
                name: "GITHUB_TOKEN".to_string(),
                value: "test-token".to_string(),
            },
        ],
        ..test_session_spec()
    });

    assert_eq!(
        outcome.expect("session should succeed with mixed empty and non-empty environment"),
        SessionOutcome::Succeeded
    );

    let create_args = fixture.create_args();
    assert!(create_args.contains("--env EMPTY_VALUE="));
    assert!(!create_args.contains("target=EMPTY_VALUE"));
    assert!(!create_args.contains("GITHUB_TOKEN=test-token"));
    assert!(create_args.contains("--secret"));
    assert_eq!(
        fixture
            .secret_commands()
            .lines()
            .filter(|line| line.starts_with("create "))
            .count(),
        1
    );
}

#[test]
fn run_session_reuses_one_session_identifier_for_container_stage_and_secret_names() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(&FakePodmanScenario::new());
    let agent_name = "agent_name";

    let methodology_dir = fixture.create_methodology_dir("runner-methodology");
    let outcome = fixture.run_with_fake_podman(crate::SessionSpec {
        agent_name: agent_name.to_string(),
        methodology_dir,
        environment: vec![ResolvedEnvironmentVariable {
            name: "GITHUB_TOKEN".to_string(),
            value: "test-token".to_string(),
        }],
        ..test_session_spec()
    });

    assert_eq!(
        outcome.expect("session should succeed with fake podman"),
        SessionOutcome::Succeeded
    );

    let create_args = fixture.create_args();
    let container_name =
        argument_value(&create_args, "--name").expect("podman create should receive a name");
    let mount_value =
        argument_value(&create_args, "--mount").expect("podman create should receive a mount");
    let mount_source = mount_src_value(&mount_value).expect("mount should include src");
    let stage_dir_name = Path::new(&mount_source)
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .expect("mount source should live under the runner staging directory");
    let secret_commands = fixture.secret_commands();
    let secret_name = secret_commands
        .split_whitespace()
        .nth(1)
        .expect("secret create should include a secret name");

    let container_prefix = format!("agentd-{agent_name}-");
    let container_suffix = container_name
        .strip_prefix(&container_prefix)
        .expect("container name should include agent prefix");
    let stage_suffix = stage_dir_name
        .strip_prefix("agentd-session-stage-")
        .expect("staging dir should include session stage prefix");

    assert_eq!(stage_suffix, container_suffix);
    assert_eq!(secret_name, format!("agentd-secret-{container_suffix}-0"));
    assert_eq!(container_suffix.len(), 32);
}

#[test]
fn run_session_releases_session_secrets_after_container_reaches_running_state() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(
        &FakePodmanScenario::new()
            .with_start(CommandBehavior::from_outcome(
                CommandOutcome::new()
                    .set_container_state("running")
                    .wait_for_file(
                        "secret-removed",
                        Duration::from_secs(3),
                        "secret was not removed while container was running",
                        42,
                    ),
            ))
            .with_secret_rm(CommandBehavior::from_outcome(
                CommandOutcome::new()
                    .append_args_with_prefix("secret-commands.log", "rm")
                    .touch_file("secret-removed"),
            )),
    );

    let methodology_dir = fixture.create_methodology_dir("runner-methodology");
    let outcome = fixture.run_with_fake_podman(crate::SessionSpec {
        methodology_dir,
        environment: vec![ResolvedEnvironmentVariable {
            name: "GITHUB_TOKEN".to_string(),
            value: "test-token".to_string(),
        }],
        ..test_session_spec()
    });

    assert_eq!(
        outcome.expect("session should succeed with fake podman"),
        SessionOutcome::Succeeded
    );
    assert!(fixture.secret_commands().contains("create"));
    assert!(fixture.secret_commands().contains("rm"));
}

#[test]
fn run_session_continues_when_secret_release_fails_after_container_reaches_running_state() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(
        &FakePodmanScenario::new()
            .with_start(CommandBehavior::from_outcome(
                CommandOutcome::new()
                    .set_container_state("running")
                    .wait_for_file(
                        "secret-rm-attempted",
                        Duration::from_secs(3),
                        "secret release was not attempted while container was running",
                        42,
                    ),
            ))
            .with_secret_rm(CommandBehavior::sequence(vec![
                CommandOutcome::new()
                    .append_args_with_prefix("secret-commands.log", "rm")
                    .touch_file("secret-rm-attempted")
                    .stderr("secret cleanup failed after container reached running")
                    .exit_code(29),
                CommandOutcome::new()
                    .append_args_with_prefix("secret-commands.log", "rm")
                    .touch_file("secret-removed"),
            ])),
    );

    let methodology_dir = fixture.create_methodology_dir("runner-methodology");
    let outcome = fixture.run_with_fake_podman(crate::SessionSpec {
        methodology_dir,
        environment: vec![ResolvedEnvironmentVariable {
            name: "GITHUB_TOKEN".to_string(),
            value: "test-token".to_string(),
        }],
        ..test_session_spec()
    });

    assert_eq!(
        outcome.expect("session should still succeed when secret release fails"),
        SessionOutcome::Succeeded
    );
    assert_eq!(
        fixture
            .secret_commands()
            .lines()
            .filter(|line| line.starts_with("rm "))
            .count(),
        2
    );
}

#[test]
fn wait_for_container_exit_returns_timeout_when_secret_release_fails() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(
        &FakePodmanScenario::new()
            .with_secret_rm(CommandBehavior::from_outcome(
                CommandOutcome::new()
                    .append_args_with_prefix("secret-commands.log", "rm")
                    .stderr("secret cleanup failed while attached start was still running")
                    .exit_code(29),
            ))
            .with_inspect(InspectBehavior::new().status_fixed("running")),
    );

    let mut child = Command::new("sh")
        .args(["-c", "sleep 0.3"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("child process should start");

    let started_at = Instant::now();
    let outcome = fixture
        .run_with_fake_podman_env(|| {
            wait_for_container_exit(
                &mut child,
                "container",
                &[SecretBinding {
                    secret_name: "secret".to_string(),
                    target_name: "GITHUB_TOKEN".to_string(),
                }],
                Some(Duration::from_millis(50)),
            )
        })
        .expect("secret release failure should not surface as a runner error");
    let elapsed = started_at.elapsed();

    assert_eq!(outcome, None);
    assert!(elapsed < Duration::from_millis(250));
    assert_eq!(
        fixture
            .secret_commands()
            .lines()
            .filter(|line| line.starts_with("rm "))
            .count(),
        1
    );

    let _ = child.kill();
    child.wait().expect("child process should be reaped");
}

#[test]
fn wait_for_container_exit_returns_timeout_promptly_when_inspect_stalls_past_deadline() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(
        &FakePodmanScenario::new()
            .with_secret_rm(CommandBehavior::from_outcome(
                CommandOutcome::new().append_args_with_prefix("secret-commands.log", "rm"),
            ))
            .with_inspect(
                InspectBehavior::new()
                    .sleep_before(Duration::from_millis(200))
                    .status_fixed("running"),
            ),
    );

    let mut child = Command::new("sh")
        .args(["-c", "sleep 0.3"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("child process should start");

    let started_at = Instant::now();
    let outcome = fixture
        .run_with_fake_podman_env(|| {
            wait_for_container_exit(
                &mut child,
                "container",
                &[SecretBinding {
                    secret_name: "secret".to_string(),
                    target_name: "GITHUB_TOKEN".to_string(),
                }],
                Some(Duration::from_millis(50)),
            )
        })
        .expect("inspect timeout should not surface as a runner error");
    let elapsed = started_at.elapsed();

    assert_eq!(outcome, None);
    assert!(elapsed < Duration::from_millis(150));
    assert_eq!(fixture.secret_commands(), "");

    let _ = child.kill();
    child.wait().expect("child process should be reaped");
}

#[test]
fn run_container_to_completion_reaps_attached_child_when_wait_for_container_exit_errors() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(
        &FakePodmanScenario::new()
            .with_start(CommandBehavior::from_outcome(
                CommandOutcome::new()
                    .record_pid_to("start.pid")
                    .wait_for_file(
                        "rm-called",
                        Duration::from_secs(3),
                        "timed out waiting for forced cleanup",
                        42,
                    ),
            ))
            .with_rm(CommandBehavior::from_outcome(
                CommandOutcome::new()
                    .write_args_to("rm-commands.log")
                    .touch_file("rm-called"),
            ))
            .with_inspect(
                InspectBehavior::new()
                    .fail("inspect failed while attached start was still running", 41),
            ),
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

    assert!(elapsed < Duration::from_millis(750));
    assert_eq!(
        fixture.read_log("rm-commands.log"),
        "--force --ignore container\n"
    );
    let pid = fixture.start_pid();
    crate::test_support::assert_process_is_reaped(pid);
}

#[test]
fn run_container_with_timeout_returns_timed_out_promptly_when_cleanup_container_fails_after_timeout()
 {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(
        &FakePodmanScenario::new()
            .with_start(CommandBehavior::from_outcome(
                CommandOutcome::new()
                    .record_pid_to("start.pid")
                    .exec_sleep(Duration::from_millis(300)),
            ))
            .with_rm(CommandBehavior::from_outcome(
                CommandOutcome::new()
                    .write_args_to("rm-commands.log")
                    .stderr("rm failed after timeout")
                    .exit_code(47),
            ))
            .with_inspect(InspectBehavior::new().status_fixed("created")),
    );

    let started_at = Instant::now();
    let outcome = fixture
        .run_with_fake_podman_env(|| {
            run_container_with_timeout("container", &[], Duration::from_millis(50))
        })
        .expect("timeout cleanup failure should still return TimedOut promptly");
    let elapsed = started_at.elapsed();

    assert_eq!(outcome, SessionOutcome::TimedOut);
    assert!(elapsed < Duration::from_millis(250));
    assert_eq!(
        fixture.read_log("rm-commands.log"),
        "--force --ignore container\n"
    );
    crate::test_support::assert_process_is_reaped(fixture.start_pid());
}

#[test]
fn run_session_returns_run_error_when_cleanup_after_run_also_fails() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(
        &FakePodmanScenario::new()
            .with_start(CommandBehavior::from_outcome(
                CommandOutcome::new()
                    .set_container_state("running")
                    .stderr("attached start failed")
                    .exit_code(125),
            ))
            .with_secret_rm(CommandBehavior::sequence(vec![
                CommandOutcome::new().append_args_with_prefix("secret-commands.log", "rm"),
                CommandOutcome::new()
                    .append_args_with_prefix("secret-commands.log", "rm")
                    .stderr("secret cleanup failed after run failure")
                    .exit_code(29),
            ]))
            .with_inspect(
                InspectBehavior::new()
                    .status_fixed("running")
                    .status_exit_fixed("running 0"),
            ),
    );

    let methodology_dir = fixture.create_methodology_dir("runner-methodology");
    let error = fixture
        .run_with_fake_podman_env(|| {
            crate::run_session(
                crate::SessionSpec {
                    methodology_dir,
                    environment: vec![ResolvedEnvironmentVariable {
                        name: "GITHUB_TOKEN".to_string(),
                        value: "test-token".to_string(),
                    }],
                    ..test_session_spec()
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

#[test]
fn fake_podman_scenario_allows_create_stdout_without_breaking_success() {
    let _guard = fake_podman_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FakePodmanFixture::new();
    fixture.install(
        &FakePodmanScenario::new().with_create(CommandBehavior::from_outcome(
            CommandOutcome::new()
                .write_args_to("create-args.log")
                .set_container_state("created")
                .stdout("created"),
        )),
    );

    let methodology_dir = fixture.create_methodology_dir("runner-methodology");
    let outcome = fixture.run_with_fake_podman(crate::SessionSpec {
        methodology_dir,
        environment: vec![ResolvedEnvironmentVariable {
            name: "GITHUB_TOKEN".to_string(),
            value: "test-token".to_string(),
        }],
        ..test_session_spec()
    });

    assert_eq!(
        outcome.expect("session should still succeed"),
        SessionOutcome::Succeeded
    );
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
