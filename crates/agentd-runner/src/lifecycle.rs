use std::fmt::Display;
use std::io::Write;

#[derive(Clone, Copy)]
pub(crate) enum LifecycleFailureKind {
    Cleanup,
    AttachedStartFinalization,
    AttachedStartKill,
}

impl LifecycleFailureKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Cleanup => "cleanup after",
            Self::AttachedStartFinalization => "attached start finalization after",
            Self::AttachedStartKill => "attached start kill after",
        }
    }
}

pub(crate) fn log_lifecycle_failure<E>(kind: LifecycleFailureKind, stage: &str, error: &E)
where
    E: Display,
{
    let mut stderr = std::io::stderr().lock();
    let _ = log_lifecycle_failure_to(&mut stderr, kind, stage, error);
}

pub(crate) fn log_lifecycle_failure_to<W, E>(
    writer: &mut W,
    kind: LifecycleFailureKind,
    stage: &str,
    error: &E,
) -> std::io::Result<()>
where
    W: Write,
    E: Display,
{
    writeln!(writer, "{} {stage} failed: {error}", kind.prefix())
}
