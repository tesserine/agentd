//! Session lifecycle management for agentd.
//!
//! Owns the four phases of a session: input validation, resource allocation
//! (methodology staging, podman secrets), container execution (create, start
//! in attached mode, supervise), and teardown (force-remove container, release
//! secrets, remove staging directory). The public entry point is
//! [`run_session`], which accepts a [`SessionSpec`] and
//! [`SessionInvocation`] and returns a [`SessionOutcome`] or
//! [`RunnerError`].
//!
//! See `ARCHITECTURE.md` section "Session Lifecycle" for the design-level
//! treatment of these phases.

mod container;
mod lifecycle;
mod podman;
mod reconcile;
mod resources;
mod types;
mod validation;

#[cfg(test)]
pub(crate) mod test_support;

pub use reconcile::reconcile_startup_resources;
pub use types::{
    AgentNameValidationError, EnvironmentNameValidationError, ResolvedEnvironmentVariable,
    RunnerError, SessionInvocation, SessionOutcome, SessionSpec, StartupReconciliationReport,
};
pub use validation::{validate_agent_name, validate_environment_name};

use container::{create_container, run_container_to_completion, run_container_with_timeout};
use lifecycle::{
    LifecycleFailureKind, log_lifecycle_failure, log_session_error, log_session_outcome,
    log_session_started, log_session_teardown,
};
use resources::{
    SessionResources, cleanup_methodology_staging_dir, cleanup_podman_secrets,
    prepare_session_resources, unique_suffix,
};
use validation::{validate_invocation, validate_spec};

/// Executes a single agent session from validation through teardown.
///
/// Validates `spec` and `invocation`, allocates session resources (methodology
/// staging directory, podman secrets for non-empty environment values), creates
/// and runs an ephemeral podman container, then cleans up all resources
/// regardless of outcome.
///
/// Returns [`SessionOutcome::Succeeded`] when the container exits 0,
/// [`SessionOutcome::Failed`] for non-zero exits, or
/// [`SessionOutcome::TimedOut`] when the optional timeout fires. Returns
/// [`RunnerError`] for validation failures, I/O errors, or podman command
/// failures.
pub fn run_session(
    spec: SessionSpec,
    invocation: SessionInvocation,
) -> Result<SessionOutcome, RunnerError> {
    validate_spec(&spec)?;
    validate_invocation(&invocation)?;
    let session_id = unique_suffix()?;

    let container_name = format!("agentd-{}-{}", spec.agent_name, session_id);
    log_session_started(
        &session_id,
        &container_name,
        &spec.agent_name,
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
        capture_tracing_events, fake_podman_lock, test_session_spec, unique_temp_dir,
    };
    use std::fs;

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
                SessionOutcome::Succeeded
            );
        });

        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["fields"]["event"], "runner.session_started");
        assert_eq!(events[1]["fields"]["event"], "runner.session_outcome");
        assert_eq!(events[1]["fields"]["outcome"], "succeeded");
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
    fn startup_reconciliation_removes_only_dead_agentd_containers_and_orphaned_secrets() {
        let _guard = fake_podman_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fixture = FakePodmanFixture::new();
        fixture.install(
            &FakePodmanScenario::new()
                .with_ps(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    "agentd-codex-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa exited\n\
                         agentd-review-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb running\n\
                         postgres-db exited",
                )))
                .with_secret_ls(CommandBehavior::from_outcome(CommandOutcome::new().stdout(
                    "agentd-secret-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-0\n\
                         agentd-secret-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-0\n\
                         agentd-secret-cccccccccccccccccccccccccccccccc-repo-token\n\
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
            .run_with_fake_podman_env(reconcile_startup_resources)
            .expect("startup reconciliation should succeed");

        assert_eq!(
            report.removed_container_names,
            vec!["agentd-codex-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()]
        );
        assert_eq!(
            report.removed_secret_names,
            vec![
                "agentd-secret-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-0".to_string(),
                "agentd-secret-cccccccccccccccccccccccccccccccc-repo-token".to_string(),
            ]
        );
        assert_eq!(
            fixture.read_log("rm-commands.log"),
            "rm --force agentd-codex-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
        );
        assert_eq!(
            fixture.secret_commands(),
            "rm --ignore agentd-secret-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-0 agentd-secret-cccccccccccccccccccccccccccccccc-repo-token\n"
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
                .with_ps(CommandBehavior::from_outcome(
                    CommandOutcome::new()
                        .stdout("agentd-codex-dddddddddddddddddddddddddddddddd running"),
                ))
                .with_secret_ls(CommandBehavior::from_outcome(
                    CommandOutcome::new()
                        .stdout("agentd-secret-dddddddddddddddddddddddddddddddd-0"),
                )),
        );

        let report = fixture
            .run_with_fake_podman_env(reconcile_startup_resources)
            .expect("startup reconciliation should succeed");

        assert!(report.removed_container_names.is_empty());
        assert!(report.removed_secret_names.is_empty());
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
            .run_with_fake_podman_env(reconcile_startup_resources)
            .expect_err("startup reconciliation should fail when container listing fails");

        match error {
            RunnerError::PodmanCommandFailed {
                args,
                status,
                stderr,
            } => {
                assert_eq!(args, vec!["ps", "-a", "--format", "{{.Names}} {{.State}}"]);
                assert_eq!(status.code(), Some(29));
                assert_eq!(stderr.trim(), "container list failed");
            }
            other => panic!("expected podman command failure, got {other:?}"),
        }
    }
}
