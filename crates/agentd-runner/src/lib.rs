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

use container::{
    ContainerLifecycleFailureKind, create_container, log_container_lifecycle_failure,
    run_container_to_completion, run_container_with_timeout,
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
    let resources = prepare_session_resources(&container_name, &spec, &invocation, &session_id)?;

    if let Err(error) = create_container(&resources, &spec, &invocation) {
        if let Err(cleanup_error) = cleanup_session_resources(&resources) {
            log_container_lifecycle_failure(
                ContainerLifecycleFailureKind::Cleanup,
                "container creation",
                &cleanup_error,
            );
        }
        return Err(error);
    }

    let secret_bindings = resources.all_secret_bindings();
    let start_result = match invocation.timeout {
        Some(timeout) => {
            run_container_with_timeout(&resources.container_name, &secret_bindings, timeout)
        }
        None => run_container_to_completion(&resources.container_name, &secret_bindings),
    };

    let cleanup_result = cleanup_session_resources(&resources);

    match (start_result, cleanup_result) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            log_container_lifecycle_failure(
                ContainerLifecycleFailureKind::Cleanup,
                "session execution",
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
