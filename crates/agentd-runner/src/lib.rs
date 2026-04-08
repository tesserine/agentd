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
mod resources;
mod types;
mod validation;

#[cfg(test)]
pub(crate) mod test_support;

pub use types::{
    AgentNameValidationError, EnvironmentNameValidationError, ResolvedEnvironmentVariable,
    RunnerError, SessionInvocation, SessionOutcome, SessionSpec,
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
        FakePodmanFixture, FakePodmanScenario, capture_tracing_events, fake_podman_lock,
        test_session_spec,
    };

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
}
