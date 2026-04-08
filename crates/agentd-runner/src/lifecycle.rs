use std::fmt::Display;
use std::time::Duration;

use crate::types::{RunnerError, SessionOutcome};

#[derive(Clone, Copy)]
pub(crate) enum LifecycleFailureKind {
    Cleanup,
    AttachedStartFinalization,
    AttachedStartKill,
}

impl LifecycleFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cleanup => "cleanup",
            Self::AttachedStartFinalization => "attached_start_finalization",
            Self::AttachedStartKill => "attached_start_kill",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Cleanup => "cleanup after",
            Self::AttachedStartFinalization => "attached start finalization after",
            Self::AttachedStartKill => "attached start kill after",
        }
    }
}

pub(crate) fn log_lifecycle_failure<E>(
    kind: LifecycleFailureKind,
    stage: &str,
    container_name: &str,
    session_id: &str,
    error: &E,
) where
    E: Display + ?Sized,
{
    tracing::warn!(
        event = "runner.lifecycle_failure",
        lifecycle_kind = kind.as_str(),
        stage = stage,
        container_name = container_name,
        session_id = session_id,
        error = %error,
        "{} {} failed",
        kind.prefix(),
        stage
    );
}

pub(crate) fn log_session_started(
    session_id: &str,
    container_name: &str,
    agent_name: &str,
    work_unit_present: bool,
    timeout: Option<Duration>,
) {
    tracing::info!(
        event = "runner.session_started",
        session_id = session_id,
        container_name = container_name,
        agent_name = agent_name,
        work_unit_present = work_unit_present,
        timeout_ms = timeout.map(|value| value.as_millis() as u64),
        "runner session started"
    );
}

pub(crate) fn log_session_outcome(
    session_id: &str,
    container_name: &str,
    outcome: &SessionOutcome,
) {
    match outcome {
        SessionOutcome::Succeeded => tracing::info!(
            event = "runner.session_outcome",
            session_id = session_id,
            container_name = container_name,
            outcome = "succeeded",
            "runner session completed"
        ),
        SessionOutcome::Failed { exit_code } => tracing::info!(
            event = "runner.session_outcome",
            session_id = session_id,
            container_name = container_name,
            outcome = "failed",
            exit_code = *exit_code,
            "runner session completed"
        ),
        SessionOutcome::TimedOut => tracing::warn!(
            event = "runner.session_outcome",
            session_id = session_id,
            container_name = container_name,
            outcome = "timed_out",
            "runner session timed out"
        ),
    }
}

pub(crate) fn log_session_error(
    session_id: &str,
    container_name: &str,
    stage: &str,
    error: &RunnerError,
) {
    tracing::error!(
        event = "runner.session_outcome",
        session_id = session_id,
        container_name = container_name,
        outcome = "error",
        stage = stage,
        error = %error,
        "runner session failed before completion"
    );
}

pub(crate) fn log_session_teardown(
    session_id: &str,
    container_name: &str,
    result: Result<(), &RunnerError>,
) {
    match result {
        Ok(()) => tracing::info!(
            event = "runner.session_teardown",
            session_id = session_id,
            container_name = container_name,
            result = "ok",
            "runner session teardown completed"
        ),
        Err(error) => tracing::warn!(
            event = "runner.session_teardown",
            session_id = session_id,
            container_name = container_name,
            result = "error",
            error = %error,
            "runner session teardown failed"
        ),
    }
}
