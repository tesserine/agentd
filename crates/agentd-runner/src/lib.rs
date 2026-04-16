//! Session lifecycle management for agentd.
//!
//! Owns the four phases of a session: input validation, resource allocation
//! (mount staging, podman secrets), container execution (create, start in
//! attached mode, supervise), and teardown (force-remove container, release
//! secrets, remove staging directory). The public entry point is
//! [`run_session`], which accepts a [`SessionSpec`] and
//! [`SessionInvocation`] and returns a [`SessionOutcome`] or
//! [`RunnerError`].
//!
//! See `ARCHITECTURE.md` section "Session Lifecycle" for the design-level
//! treatment of these phases.

mod container;
mod lifecycle;
mod naming;
mod podman;
mod reconcile;
mod resources;
mod session_paths;
mod types;
mod validation;

#[cfg(test)]
pub(crate) mod test_support;

pub use reconcile::reconcile_startup_resources;
pub use types::{
    BindMount, EnvironmentNameValidationError, MountTargetValidationError,
    ProfileNameValidationError, ResolvedEnvironmentVariable, RunnerError, SessionInvocation,
    SessionOutcome, SessionSpec, StartupReconciliationReport,
};
pub use validation::{
    validate_environment_name, validate_mount_target, validate_profile_name, validate_repo_url,
};

use container::{create_container, run_container_to_completion, run_container_with_timeout};
use lifecycle::{
    LifecycleFailureKind, log_lifecycle_failure, log_session_error, log_session_outcome,
    log_session_started, log_session_teardown,
};
use naming::format_container_name;
use resources::{
    SessionResources, cleanup_methodology_staging_dir, cleanup_podman_secrets,
    prepare_session_resources, unique_suffix,
};
use validation::{validate_invocation, validate_spec};

/// Executes a single session from validation through teardown.
///
/// Validates `spec` and `invocation`, allocates session resources (mount
/// staging directory, podman secrets for non-empty environment values), creates
/// and runs an ephemeral podman container, then cleans up all resources
/// regardless of outcome.
///
/// Returns a semantic [`SessionOutcome`] interpreted from the container exit
/// code according to the shared commons contract, or
/// [`SessionOutcome::TimedOut`] when the optional timeout fires. Returns
/// [`RunnerError`] for validation failures, I/O errors, or podman command
/// failures before a terminal session outcome can be established.
pub fn run_session(
    spec: SessionSpec,
    invocation: SessionInvocation,
) -> Result<SessionOutcome, RunnerError> {
    validate_spec(&spec)?;
    validate_invocation(&invocation)?;
    let session_id = unique_suffix()?;

    let container_name =
        format_container_name(&spec.daemon_instance_id, &spec.profile_name, &session_id);
    log_session_started(
        &session_id,
        &container_name,
        &spec.profile_name,
        invocation.work_unit.is_some(),
        invocation.timeout,
    );

    let resources =
        match prepare_session_resources(&container_name, &spec, &invocation, &session_id) {
            Ok(resources) => resources,
            Err(error) => {
                log_session_error(
                    &session_id,
                    &container_name,
                    "session_resource_allocation",
                    &error,
                );
                tracing::info!(
                    event = "runner.session_teardown",
                    session_id = session_id,
                    container_name = container_name,
                    result = "skipped",
                    "runner session teardown skipped"
                );
                return Err(error);
            }
        };

    if let Err(error) = create_container(&resources, &spec, &invocation) {
        let cleanup_result = cleanup_session_resources(&resources);
        if let Err(cleanup_error) = &cleanup_result {
            log_lifecycle_failure(
                LifecycleFailureKind::Cleanup,
                "container creation",
                &resources.container_name,
                &session_id,
                cleanup_error,
            );
        }
        log_session_error(
            &session_id,
            &resources.container_name,
            "container_creation",
            &error,
        );
        log_session_teardown(
            &session_id,
            &resources.container_name,
            cleanup_result.as_ref().map(|_| ()),
        );
        return Err(error);
    }

    let secret_bindings = resources.all_secret_bindings();
    let start_result = match invocation.timeout {
        Some(timeout) => run_container_with_timeout(
            &resources.container_name,
            &session_id,
            &secret_bindings,
            timeout,
        ),
        None => {
            run_container_to_completion(&resources.container_name, &session_id, &secret_bindings)
        }
    };

    match &start_result {
        Ok(outcome) => log_session_outcome(&session_id, &resources.container_name, outcome),
        Err(error) => log_session_error(
            &session_id,
            &resources.container_name,
            "session_execution",
            error,
        ),
    }

    let cleanup_result = cleanup_session_resources(&resources);
    log_session_teardown(
        &session_id,
        &resources.container_name,
        cleanup_result.as_ref().map(|_| ()),
    );

    match (start_result, cleanup_result) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            log_lifecycle_failure(
                LifecycleFailureKind::Cleanup,
                "session execution",
                &resources.container_name,
                &session_id,
                &cleanup_error,
            );
            Err(error)
        }
    }
}

fn cleanup_session_resources(resources: &SessionResources) -> Result<(), RunnerError> {
    let container_result = container::cleanup_container(&resources.container_name);
    let secret_bindings = resources.all_secret_bindings();
    let secret_result = cleanup_podman_secrets(&secret_bindings);
    let staging_result = cleanup_methodology_staging_dir(&resources.methodology_staging_dir);

    container_result?;
    secret_result?;
    staging_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        CommandBehavior, CommandOutcome, FakePodmanFixture, FakePodmanScenario,
        capture_tracing_events, fake_podman_lock, fake_podman_ps_json, test_session_spec,
        unique_temp_dir,
    };
    use std::fs;

    const TEST_DAEMON_INSTANCE_ID: &str = "1a2b3c4d";

    fn reconcile_startup_resources_for_tests() -> Result<StartupReconciliationReport, RunnerError> {
        reconcile_startup_resources(TEST_DAEMON_INSTANCE_ID)
    }

    #[test]
    fn run_session_emits_start_outcome_and_teardown_events() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(&FakePodmanScenario::new());
        let methodology_dir = fixture.create_methodology_dir("runner-methodology");

        let events = capture_tracing_events(|| {
            let outcome = fixture.run_with_fake_podman(SessionSpec {
                methodology_dir,
                ..test_session_spec()
            });
            assert_eq!(
                outcome.expect("session should succeed"),
                SessionOutcome::Success { exit_code: 0 }
            );
        });

        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["fields"]["event"], "runner.session_started");
        assert_eq!(events[1]["fields"]["event"], "runner.session_outcome");
        assert_eq!(events[1]["fields"]["outcome"], "success");
        assert_eq!(events[1]["fields"]["exit_code"], 0);
        assert_eq!(events[2]["fields"]["event"], "runner.session_teardown");
        assert_eq!(events[2]["fields"]["result"], "ok");
    }

    #[test]
    fn run_session_emits_teardown_after_resource_allocation_failure() {
        let methodology_dir = unique_temp_dir("runner-missing-manifest");
        fs::create_dir_all(&methodology_dir).expect("methodology directory should be created");

        let events = capture_tracing_events(|| {
            let error = run_session(
                SessionSpec {
                    methodology_dir: methodology_dir.clone(),
                    ..test_session_spec()
                },
                SessionInvocation {
                    repo_url: "https://example.com/agentd.git".to_string(),
                    repo_token: None,
                    work_unit: None,
                    timeout: None,
                },
            )
            .expect_err("session should fail during resource allocation");

            assert!(
                matches!(error, RunnerError::MissingMethodologyManifest { .. }),
                "expected missing manifest error, got {error:?}"
            );
        });

        fs::remove_dir_all(&methodology_dir)
            .expect("temporary methodology directory should be removed");

        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["fields"]["event"], "runner.session_started");
        assert_eq!(events[1]["fields"]["event"], "runner.session_error");
        assert_eq!(events[1]["fields"]["stage"], "session_resource_allocation");
        assert_eq!(events[2]["fields"]["event"], "runner.session_teardown");
        assert_eq!(events[2]["fields"]["result"], "skipped");
    }

    #[test]
    fn run_session_marks_teardown_skipped_when_allocation_rollback_logs_failure() {
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

        let events = capture_tracing_events(|| {
            let error = fixture
                .run_with_fake_podman_env(|| {
                    run_session(
                        SessionSpec {
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
                        SessionInvocation {
                            repo_url: "https://example.com/agentd.git".to_string(),
                            repo_token: None,
                            work_unit: None,
                            timeout: None,
                        },
                    )
                })
                .expect_err("session should fail during resource allocation");

            assert!(
                matches!(error, RunnerError::PodmanCommandFailed { .. }),
                "expected podman command failure, got {error:?}"
            );
        });

        assert_eq!(events.len(), 4);
        assert_eq!(events[0]["fields"]["event"], "runner.session_started");
        assert_eq!(events[1]["fields"]["event"], "runner.lifecycle_failure");
        assert_eq!(events[1]["fields"]["stage"], "session resource allocation");
        assert_eq!(events[2]["fields"]["event"], "runner.session_error");
        assert_eq!(events[2]["fields"]["stage"], "session_resource_allocation");
        assert_eq!(events[3]["fields"]["event"], "runner.session_teardown");
        assert_eq!(events[3]["fields"]["result"], "skipped");
    }

    #[test]
    fn startup_reconciliation_removes_only_terminal_agentd_containers_and_orphaned_secrets() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_ps(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    &fake_podman_ps_json(&[
                        (
                            &["agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa"],
                            "exited",
                            "Exited (0) 10s ago",
                        ),
                        (&["agentd-1a2b3c4d-review-bbbbbbbbbbbbbbbb"], "dead", "Dead"),
                        (
                            &["agentd-1a2b3c4d-build-cccccccccccccccc"],
                            "stopped",
                            "Exited (137) 5m ago",
                        ),
                        (
                            &["agentd-1a2b3c4d-prepare-dddddddddddddddd"],
                            "created",
                            "Created",
                        ),
                        (
                            &["agentd-1a2b3c4d-init-1212121212121212"],
                            "initialized",
                            "Initialized",
                        ),
                        (
                            &["agentd-1a2b3c4d-pause-eeeeeeeeeeeeeeee"],
                            "paused",
                            "Up 2 minutes",
                        ),
                        (
                            &["agentd-1a2b3c4d-stop-ffffffffffffffff"],
                            "stopping",
                            "Stopping",
                        ),
                        (
                            &["agentd-1a2b3c4d-live-9999999999999999"],
                            "running",
                            "Up 4 hours",
                        ),
                        (
                            &["agentd-deadbeef-foreign-3434343434343434"],
                            "exited",
                            "Exited (0) 1m ago",
                        ),
                        (&["postgres-db"], "exited", "Exited (0) 1h ago"),
                    ]),
                )))
                .with_secret_ls(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    "agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0\n\
                         agentd-1a2b3c4d-bbbbbbbbbbbbbbbb-0\n\
                         agentd-1a2b3c4d-cccccccccccccccc-repo-token\n\
                         agentd-1a2b3c4d-dddddddddddddddd-0\n\
                         agentd-1a2b3c4d-1212121212121212-0\n\
                         agentd-1a2b3c4d-eeeeeeeeeeeeeeee-0\n\
                         agentd-1a2b3c4d-ffffffffffffffff-0\n\
                         agentd-1a2b3c4d-9999999999999999-0\n\
                         agentd-deadbeef-3434343434343434-repo-token\n\
                         foreign-secret",
                )))
                .with_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new().append_args_with_prefix("rm-commands.log", "rm"),
                ))
                .with_secret_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new().append_args_with_prefix("secret-commands.log", "rm"),
                )),
        );

        let report = fixture
            .run_with_fake_podman_env(reconcile_startup_resources_for_tests)
            .expect("startup reconciliation should succeed");

        assert_eq!(
            report.removed_container_names,
            vec![
                "agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa".to_string(),
                "agentd-1a2b3c4d-review-bbbbbbbbbbbbbbbb".to_string(),
                "agentd-1a2b3c4d-build-cccccccccccccccc".to_string(),
                "agentd-1a2b3c4d-prepare-dddddddddddddddd".to_string(),
                "agentd-1a2b3c4d-init-1212121212121212".to_string(),
            ]
        );
        assert_eq!(
            report.removed_secret_names,
            vec![
                "agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0".to_string(),
                "agentd-1a2b3c4d-bbbbbbbbbbbbbbbb-0".to_string(),
                "agentd-1a2b3c4d-cccccccccccccccc-repo-token".to_string(),
                "agentd-1a2b3c4d-dddddddddddddddd-0".to_string(),
                "agentd-1a2b3c4d-1212121212121212-0".to_string(),
            ]
        );
        assert_eq!(
            fixture.read_log("rm-commands.log"),
            "rm --force --ignore agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa agentd-1a2b3c4d-review-bbbbbbbbbbbbbbbb agentd-1a2b3c4d-build-cccccccccccccccc agentd-1a2b3c4d-prepare-dddddddddddddddd agentd-1a2b3c4d-init-1212121212121212\n"
        );
        assert_eq!(
            fixture.secret_commands(),
            "rm --ignore agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0 agentd-1a2b3c4d-bbbbbbbbbbbbbbbb-0 agentd-1a2b3c4d-cccccccccccccccc-repo-token agentd-1a2b3c4d-dddddddddddddddd-0 agentd-1a2b3c4d-1212121212121212-0\n"
        );
    }

    #[test]
    fn startup_reconciliation_keeps_running_agentd_containers_and_their_secrets() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_ps(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    &fake_podman_ps_json(&[(
                        &["agentd-1a2b3c4d-site-builder-dddddddddddddddd"],
                        "running",
                        "Up 2 minutes",
                    )]),
                )))
                .with_secret_ls(CommandBehavior::from_outcome(
                    CommandOutcome::new().stdout("agentd-1a2b3c4d-dddddddddddddddd-0"),
                )),
        );

        let report = fixture
            .run_with_fake_podman_env(reconcile_startup_resources_for_tests)
            .expect("startup reconciliation should succeed");

        assert!(report.removed_container_names.is_empty());
        assert!(report.removed_secret_names.is_empty());
        assert_eq!(fixture.read_log("rm-commands.log"), "");
        assert_eq!(fixture.secret_commands(), "");
    }

    #[test]
    fn startup_reconciliation_keeps_unknown_state_agentd_containers_and_their_secrets() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_ps(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    &fake_podman_ps_json(&[(
                        &["agentd-1a2b3c4d-site-builder-dddddddddddddddd"],
                        "mystery-state",
                        "Something odd just happened",
                    )]),
                )))
                .with_secret_ls(CommandBehavior::from_outcome(
                    CommandOutcome::new().stdout("agentd-1a2b3c4d-dddddddddddddddd-0"),
                )),
        );

        let report = fixture
            .run_with_fake_podman_env(reconcile_startup_resources_for_tests)
            .expect("startup reconciliation should succeed");

        assert!(report.removed_container_names.is_empty());
        assert!(report.removed_secret_names.is_empty());
        assert_eq!(fixture.read_log("rm-commands.log"), "");
        assert_eq!(fixture.secret_commands(), "");
    }

    #[test]
    fn startup_reconciliation_ignores_terminal_agentd_prefixed_non_session_containers() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_ps(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    &fake_podman_ps_json(&[
                        (&["agentd-proxy"], "exited", "Exited (0) 10s ago"),
                        (
                            &["agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa"],
                            "exited",
                            "Exited (0) 10s ago",
                        ),
                    ]),
                )))
                .with_secret_ls(CommandBehavior::from_outcome(
                    CommandOutcome::new().stdout("agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0"),
                ))
                .with_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new().append_args_with_prefix("rm-commands.log", "rm"),
                ))
                .with_secret_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new().append_args_with_prefix("secret-commands.log", "rm"),
                )),
        );

        let report = fixture
            .run_with_fake_podman_env(reconcile_startup_resources_for_tests)
            .expect("startup reconciliation should succeed");

        assert_eq!(
            report.removed_container_names,
            vec!["agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa".to_string(),]
        );
        assert_eq!(
            report.removed_secret_names,
            vec!["agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0".to_string()]
        );
        assert_eq!(
            fixture.read_log("rm-commands.log"),
            "rm --force --ignore agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa\n"
        );
        assert_eq!(
            fixture.secret_commands(),
            "rm --ignore agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0\n"
        );
    }

    #[test]
    fn startup_reconciliation_ignores_terminal_agentd_containers_with_uppercase_session_ids() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_ps(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    &fake_podman_ps_json(&[
                        (
                            &["agentd-1a2b3c4d-site-builder-AAAAAAAAAAAAAAAA"],
                            "exited",
                            "Exited (0) 10s ago",
                        ),
                        (
                            &["agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa"],
                            "exited",
                            "Exited (0) 10s ago",
                        ),
                    ]),
                )))
                .with_secret_ls(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    "agentd-1a2b3c4d-AAAAAAAAAAAAAAAA-0\n\
                     agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0",
                )))
                .with_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new().append_args_with_prefix("rm-commands.log", "rm"),
                ))
                .with_secret_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new().append_args_with_prefix("secret-commands.log", "rm"),
                )),
        );

        let report = fixture
            .run_with_fake_podman_env(reconcile_startup_resources_for_tests)
            .expect("startup reconciliation should succeed");

        assert_eq!(
            report.removed_container_names,
            vec!["agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa".to_string()]
        );
        assert_eq!(
            report.removed_secret_names,
            vec!["agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0".to_string()]
        );
        assert_eq!(
            fixture.read_log("rm-commands.log"),
            "rm --force --ignore agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa\n"
        );
        assert_eq!(
            fixture.secret_commands(),
            "rm --ignore agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0\n"
        );
    }

    #[test]
    fn startup_reconciliation_ignores_secrets_with_non_runner_suffixes() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_ps(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    &fake_podman_ps_json(&[(
                        &["agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa"],
                        "exited",
                        "Exited (0) 10s ago",
                    )]),
                )))
                .with_secret_ls(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    "agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0\n\
                     agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-backup",
                )))
                .with_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new().append_args_with_prefix("rm-commands.log", "rm"),
                ))
                .with_secret_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new().append_args_with_prefix("secret-commands.log", "rm"),
                )),
        );

        let report = fixture
            .run_with_fake_podman_env(reconcile_startup_resources_for_tests)
            .expect("startup reconciliation should succeed");

        assert_eq!(
            report.removed_container_names,
            vec!["agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa".to_string()]
        );
        assert_eq!(
            report.removed_secret_names,
            vec!["agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0".to_string()]
        );
        assert_eq!(
            fixture.read_log("rm-commands.log"),
            "rm --force --ignore agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa\n"
        );
        assert_eq!(
            fixture.secret_commands(),
            "rm --ignore agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0\n"
        );
    }

    #[test]
    fn startup_reconciliation_returns_an_error_when_container_listing_json_is_malformed() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new().with_ps(CommandBehavior::from_outcome(
                CommandOutcome::new().stdout("{not-json"),
            )),
        );

        let error = fixture
            .run_with_fake_podman_env(reconcile_startup_resources_for_tests)
            .expect_err(
                "startup reconciliation should fail when container listing JSON is invalid",
            );

        match error {
            RunnerError::Io(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
                assert!(
                    error.to_string().contains("podman ps --format json"),
                    "expected invalid json error to mention podman ps output, got {error}"
                );
            }
            other => panic!("expected invalid-data io error, got {other:?}"),
        }
        assert_eq!(fixture.read_log("rm-commands.log"), "");
        assert_eq!(fixture.secret_commands(), "");
    }

    #[test]
    fn startup_reconciliation_returns_an_error_when_container_listing_fails() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new().with_ps(CommandBehavior::from_outcome(
                CommandOutcome::new()
                    .stderr("container list failed")
                    .exit_code(29),
            )),
        );

        let error = fixture
            .run_with_fake_podman_env(reconcile_startup_resources_for_tests)
            .expect_err("startup reconciliation should fail when container listing fails");

        match error {
            RunnerError::PodmanCommandFailed {
                args,
                status,
                stderr,
            } => {
                assert_eq!(args, vec!["ps", "-a", "--format", "json"]);
                assert_eq!(status.code(), Some(29));
                assert_eq!(stderr.trim(), "container list failed");
            }
            other => panic!("expected podman command failure, got {other:?}"),
        }
    }

    #[test]
    fn startup_reconciliation_returns_an_error_when_container_removal_fails() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_ps(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    &fake_podman_ps_json(&[(
                        &["agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa"],
                        "exited",
                        "Exited (0) 10s ago",
                    )]),
                )))
                .with_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new()
                        .write_args_to("rm-commands.log")
                        .stderr("rm failed")
                        .exit_code(51),
                )),
        );

        let error = fixture
            .run_with_fake_podman_env(reconcile_startup_resources_for_tests)
            .expect_err("startup reconciliation should fail when container removal fails");

        match error {
            RunnerError::PodmanCommandFailed {
                args,
                status,
                stderr,
            } => {
                assert_eq!(
                    args,
                    vec![
                        "rm",
                        "--force",
                        "--ignore",
                        "agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa",
                    ]
                );
                assert_eq!(status.code(), Some(51));
                assert_eq!(stderr.trim(), "rm failed");
            }
            other => panic!("expected podman command failure, got {other:?}"),
        }
        assert_eq!(
            fixture.read_log("rm-commands.log"),
            "--force --ignore agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa\n"
        );
    }

    #[test]
    fn startup_reconciliation_keeps_resources_owned_by_other_daemon_instances() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_ps(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    &fake_podman_ps_json(&[
                        (
                            &["agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa"],
                            "exited",
                            "Exited (0) 10s ago",
                        ),
                        (
                            &["agentd-deadbeef-site-builder-bbbbbbbbbbbbbbbb"],
                            "exited",
                            "Exited (0) 10s ago",
                        ),
                    ]),
                )))
                .with_secret_ls(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    "agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0\n\
                     agentd-deadbeef-bbbbbbbbbbbbbbbb-0",
                )))
                .with_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new().append_args_with_prefix("rm-commands.log", "rm"),
                ))
                .with_secret_rm(CommandBehavior::from_outcome(
                    CommandOutcome::new().append_args_with_prefix("secret-commands.log", "rm"),
                )),
        );

        let report = fixture
            .run_with_fake_podman_env(reconcile_startup_resources_for_tests)
            .expect("startup reconciliation should succeed");

        assert_eq!(
            report.removed_container_names,
            vec!["agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa".to_string()]
        );
        assert_eq!(
            report.removed_secret_names,
            vec!["agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0".to_string()]
        );
        assert_eq!(
            fixture.read_log("rm-commands.log"),
            "rm --force --ignore agentd-1a2b3c4d-site-builder-aaaaaaaaaaaaaaaa\n"
        );
        assert_eq!(
            fixture.secret_commands(),
            "rm --ignore agentd-1a2b3c4d-aaaaaaaaaaaaaaaa-0\n"
        );
    }
}
