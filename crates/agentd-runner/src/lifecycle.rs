use std::fmt::Display;
use std::io::{self, Write};
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

    write_fallback_to_stderr(|writer| {
        write_lifecycle_failure_message(writer, kind, stage, container_name, session_id, error)
    });
}

pub(crate) fn log_session_started(
    session_id: &str,
    container_name: &str,
    profile_name: &str,
    work_unit_present: bool,
    timeout: Option<Duration>,
) {
    tracing::info!(
        event = "runner.session_started",
        session_id = session_id,
        container_name = container_name,
        profile_name = profile_name,
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
        event = "runner.session_error",
        session_id = session_id,
        container_name = container_name,
        outcome = "error",
        stage = stage,
        error = %error,
        "runner session failed before completion"
    );

    write_fallback_to_stderr(|writer| {
        write_session_error_message(writer, session_id, container_name, stage, error)
    });
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

    if let Err(error) = result {
        write_fallback_to_stderr(|writer| {
            write_session_teardown_error_message(writer, session_id, container_name, error)
        });
    }
}

fn current_dispatcher_is_no_subscriber() -> bool {
    tracing::dispatcher::get_default(|dispatch| dispatch.is::<tracing::subscriber::NoSubscriber>())
}

fn write_fallback_to_stderr(emit: impl FnOnce(&mut dyn Write) -> io::Result<()>) {
    if !current_dispatcher_is_no_subscriber() {
        return;
    }

    let mut stderr = io::stderr().lock();
    let _ = emit(&mut stderr);
}

fn write_lifecycle_failure_message<E>(
    writer: &mut (impl Write + ?Sized),
    kind: LifecycleFailureKind,
    stage: &str,
    container_name: &str,
    session_id: &str,
    error: &E,
) -> io::Result<()>
where
    E: Display + ?Sized,
{
    writeln!(
        writer,
        "session {session_id} container {container_name} {} {stage} failed: {error}",
        kind.prefix()
    )
}

fn write_session_error_message(
    writer: &mut (impl Write + ?Sized),
    session_id: &str,
    container_name: &str,
    stage: &str,
    error: &RunnerError,
) -> io::Result<()> {
    writeln!(
        writer,
        "session {session_id} container {container_name} failed during {stage}: {error}"
    )
}

fn write_session_teardown_error_message(
    writer: &mut (impl Write + ?Sized),
    session_id: &str,
    container_name: &str,
    error: &RunnerError,
) -> io::Result<()> {
    writeln!(
        writer,
        "session {session_id} container {container_name} teardown failed: {error}"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        LifecycleFailureKind, current_dispatcher_is_no_subscriber, log_session_error,
        write_lifecycle_failure_message, write_session_error_message,
        write_session_teardown_error_message,
    };
    use crate::RunnerError;
    use crate::test_support::capture_tracing_events;

    #[test]
    fn log_session_error_uses_distinct_session_error_event_name() {
        let events = capture_tracing_events(|| {
            log_session_error(
                "session-123",
                "agentd-agent-session",
                "container_creation",
                &RunnerError::InvalidBaseImage,
            );
        });

        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["fields"]["event"], "runner.session_error");
        assert_eq!(events[0]["fields"]["outcome"], "error");
        assert_eq!(events[0]["fields"]["stage"], "container_creation");
    }

    #[test]
    fn current_dispatcher_detection_matches_no_subscriber_state() {
        assert!(current_dispatcher_is_no_subscriber());

        let subscriber = tracing_subscriber::fmt().finish();
        tracing::subscriber::with_default(subscriber, || {
            assert!(!current_dispatcher_is_no_subscriber());
        });
    }

    #[test]
    fn lifecycle_failure_fallback_includes_session_and_stage() {
        let mut output = Vec::new();
        write_lifecycle_failure_message(
            &mut output,
            LifecycleFailureKind::Cleanup,
            "session execution",
            "agentd-agent-session",
            "session-123",
            &RunnerError::InvalidBaseImage,
        )
        .expect("fallback write should succeed");

        let rendered = String::from_utf8(output).expect("fallback output should be utf-8");
        assert_eq!(
            rendered,
            "session session-123 container agentd-agent-session cleanup after session execution failed: base_image must not be empty\n"
        );
    }

    #[test]
    fn session_error_fallback_includes_stage_and_error() {
        let mut output = Vec::new();
        write_session_error_message(
            &mut output,
            "session-123",
            "agentd-agent-session",
            "session_execution",
            &RunnerError::InvalidCommand,
        )
        .expect("fallback write should succeed");

        let rendered = String::from_utf8(output).expect("fallback output should be utf-8");
        assert_eq!(
            rendered,
            "session session-123 container agentd-agent-session failed during session_execution: command must contain at least one argument\n"
        );
    }

    #[test]
    fn session_teardown_error_fallback_includes_error() {
        let mut output = Vec::new();
        write_session_teardown_error_message(
            &mut output,
            "session-123",
            "agentd-agent-session",
            &RunnerError::InvalidBaseImage,
        )
        .expect("fallback write should succeed");

        let rendered = String::from_utf8(output).expect("fallback output should be utf-8");
        assert_eq!(
            rendered,
            "session session-123 container agentd-agent-session teardown failed: base_image must not be empty\n"
        );
    }
}
