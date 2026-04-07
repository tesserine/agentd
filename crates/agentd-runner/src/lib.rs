//! Session lifecycle management for agentd.

mod container;
mod podman;
mod resources;
mod types;
mod validation;

#[cfg(test)]
pub(crate) mod test_support;

pub use types::{
    EnvironmentNameValidationError, ResolvedEnvironmentVariable, RunnerError, SessionInvocation,
    SessionOutcome, SessionSpec,
};
pub use validation::validate_environment_name;

use container::{
    create_container, log_cleanup_failure, run_container_to_completion, run_container_with_timeout,
};
use resources::{
    SessionResources, cleanup_methodology_staging_dir, cleanup_podman_secrets,
    prepare_session_resources, unique_suffix,
};
use validation::{validate_invocation, validate_spec};

pub fn run_session(
    spec: SessionSpec,
    invocation: SessionInvocation,
) -> Result<SessionOutcome, RunnerError> {
    validate_spec(&spec)?;
    validate_invocation(&invocation)?;
    let session_id = unique_suffix()?;

    let container_name = format!("agentd-{}-{}", spec.agent_name, session_id);
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

fn cleanup_session_resources(resources: &SessionResources) -> Result<(), RunnerError> {
    let container_result = container::cleanup_container(&resources.container_name);
    let secret_result = cleanup_podman_secrets(&resources.secret_bindings);
    let staging_result = cleanup_methodology_staging_dir(&resources.methodology_staging_dir);

    container_result?;
    secret_result?;
    staging_result
}
