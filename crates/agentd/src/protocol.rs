use agentd_runner::{InvocationInput, SessionOutcome};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RequestMessage {
    Ping,
    Run {
        profile: String,
        repo_url: Option<String>,
        work_unit: Option<String>,
        input: Option<InvocationInput>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseMessage {
    Pong,
    SessionOutcome { outcome: OutcomeMessage },
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum OutcomeMessage {
    Success { exit_code: i32 },
    GenericFailure { exit_code: i32 },
    UsageError { exit_code: i32 },
    Blocked { exit_code: i32 },
    NothingReady { exit_code: i32 },
    WorkFailed { exit_code: i32 },
    InfrastructureFailure { exit_code: i32 },
    CommandNotExecutable { exit_code: i32 },
    CommandNotFound { exit_code: i32 },
    TerminatedBySignal { exit_code: i32, signal: i32 },
    TimedOut,
}

impl From<SessionOutcome> for OutcomeMessage {
    fn from(outcome: SessionOutcome) -> Self {
        match outcome {
            SessionOutcome::Success { exit_code } => Self::Success { exit_code },
            SessionOutcome::GenericFailure { exit_code } => Self::GenericFailure { exit_code },
            SessionOutcome::UsageError { exit_code } => Self::UsageError { exit_code },
            SessionOutcome::Blocked { exit_code } => Self::Blocked { exit_code },
            SessionOutcome::NothingReady { exit_code } => Self::NothingReady { exit_code },
            SessionOutcome::WorkFailed { exit_code } => Self::WorkFailed { exit_code },
            SessionOutcome::InfrastructureFailure { exit_code } => {
                Self::InfrastructureFailure { exit_code }
            }
            SessionOutcome::CommandNotExecutable { exit_code } => {
                Self::CommandNotExecutable { exit_code }
            }
            SessionOutcome::CommandNotFound { exit_code } => Self::CommandNotFound { exit_code },
            SessionOutcome::TerminatedBySignal { exit_code, signal } => {
                Self::TerminatedBySignal { exit_code, signal }
            }
            SessionOutcome::TimedOut => Self::TimedOut,
        }
    }
}

impl From<OutcomeMessage> for SessionOutcome {
    fn from(outcome: OutcomeMessage) -> Self {
        match outcome {
            OutcomeMessage::Success { exit_code } => Self::Success { exit_code },
            OutcomeMessage::GenericFailure { exit_code } => Self::GenericFailure { exit_code },
            OutcomeMessage::UsageError { exit_code } => Self::UsageError { exit_code },
            OutcomeMessage::Blocked { exit_code } => Self::Blocked { exit_code },
            OutcomeMessage::NothingReady { exit_code } => Self::NothingReady { exit_code },
            OutcomeMessage::WorkFailed { exit_code } => Self::WorkFailed { exit_code },
            OutcomeMessage::InfrastructureFailure { exit_code } => {
                Self::InfrastructureFailure { exit_code }
            }
            OutcomeMessage::CommandNotExecutable { exit_code } => {
                Self::CommandNotExecutable { exit_code }
            }
            OutcomeMessage::CommandNotFound { exit_code } => Self::CommandNotFound { exit_code },
            OutcomeMessage::TerminatedBySignal { exit_code, signal } => {
                Self::TerminatedBySignal { exit_code, signal }
            }
            OutcomeMessage::TimedOut => Self::TimedOut,
        }
    }
}
