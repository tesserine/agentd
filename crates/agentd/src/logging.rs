//! Tracing bootstrap for the `agentd` process.
//!
//! Logging is configured entirely through environment variables for now:
//! `RUST_LOG` overrides `AGENTD_LOG` for filter selection, and
//! `AGENTD_LOG_FORMAT` selects `json` or `pretty` output on stderr. The
//! configuration shape stays environment-driven so the bootstrap can move into
//! a shared ecosystem crate later without changing call sites.

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::time::SystemTime;
use tracing_subscriber::fmt::{self as tracing_fmt, MakeWriter};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;

const DEFAULT_FILTER: &str = "info";
const DEFAULT_FORMAT: LogFormat = LogFormat::Json;
const AGENTD_LOG_FORMAT_ENV: &str = "AGENTD_LOG_FORMAT";
const AGENTD_LOG_ENV: &str = "AGENTD_LOG";

type FilterHandle = reload::Handle<EnvFilter, Registry>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Structured JSON lines written to stderr.
    Json,
    /// Human-readable compact text written to stderr.
    Pretty,
}

/// The active logging configuration after environment resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLoggingConfig {
    /// Output format selected from `AGENTD_LOG_FORMAT`.
    pub format: LogFormat,
    /// Active tracing filter selected from `RUST_LOG` or `AGENTD_LOG`.
    pub filter: String,
}

/// Errors returned while installing or reloading the global tracing subscriber.
#[derive(Debug)]
pub enum LoggingError {
    SetGlobalDefault(String),
    Reload(String),
}

impl fmt::Display for LoggingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SetGlobalDefault(error) | Self::Reload(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for LoggingError {}

/// Resolve the current logging configuration from process environment.
pub fn resolve_logging_config() -> ResolvedLoggingConfig {
    let rust_log = std::env::var("RUST_LOG").ok();
    let agentd_log = std::env::var(AGENTD_LOG_ENV).ok();

    ResolvedLoggingConfig {
        format: resolve_log_format(std::env::var(AGENTD_LOG_FORMAT_ENV).ok().as_deref()).0,
        filter: resolve_logging_config_with_env(rust_log.as_deref(), agentd_log.as_deref()).filter,
    }
}

/// Resolve the active log filter precedence without reading the process environment.
pub fn resolve_logging_config_with_env(
    rust_log: Option<&str>,
    agentd_log: Option<&str>,
) -> ResolvedLoggingConfig {
    let filter = rust_log
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            agentd_log
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| DEFAULT_FILTER.to_string());

    ResolvedLoggingConfig {
        format: DEFAULT_FORMAT,
        filter,
    }
}

/// Install or reload the global tracing subscriber for the `agentd` process.
pub fn configure_tracing() -> Result<(), LoggingError> {
    let rust_log = std::env::var("RUST_LOG").ok();
    let agentd_log = std::env::var(AGENTD_LOG_ENV).ok();
    let format = std::env::var(AGENTD_LOG_FORMAT_ENV).ok();
    let resolved = ResolvedLoggingConfig {
        format: resolve_log_format(format.as_deref()).0,
        filter: resolve_logging_config_with_env(rust_log.as_deref(), agentd_log.as_deref()).filter,
    };
    let invalid_format = resolve_log_format(format.as_deref()).1;

    if let Some(handle) = TRACING_HANDLE.get() {
        return handle.reload(&resolved, invalid_format.as_deref());
    }

    let handle = TracingHandle::new(&resolved, invalid_format.as_deref())?;
    match TRACING_HANDLE.set(handle) {
        Ok(()) => Ok(()),
        Err(existing) => existing.reload(&resolved, invalid_format.as_deref()),
    }
}

struct TracingHandle {
    format: Arc<AtomicU8>,
    filter: FilterHandle,
}

impl TracingHandle {
    fn new(
        resolved: &ResolvedLoggingConfig,
        invalid_format: Option<&str>,
    ) -> Result<Self, LoggingError> {
        let format = Arc::new(AtomicU8::new(format_code(resolved.format)));
        let pretty_writer = ActiveFormatWriter::new(format.clone(), LogFormat::Pretty);
        let json_writer = ActiveFormatWriter::new(format.clone(), LogFormat::Json);

        let (initial_filter, fallback_warning) = match EnvFilter::try_new(&resolved.filter) {
            Ok(filter) => (filter, None),
            Err(error) => (
                EnvFilter::new(DEFAULT_FILTER),
                Some((resolved.filter.clone(), error.to_string())),
            ),
        };
        let (filter_layer, filter) = reload::Layer::new(initial_filter);

        let pretty_layer = tracing_fmt::layer()
            .with_writer(pretty_writer)
            .with_ansi(false)
            .without_time()
            .compact();
        let json_layer = build_json_layer(json_writer);

        let subscriber = Registry::default()
            .with(filter_layer)
            .with(pretty_layer)
            .with(json_layer);
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|error| LoggingError::SetGlobalDefault(error.to_string()))?;

        if let Some((spec, error)) = fallback_warning {
            tracing::warn!(
                event = "agentd.logging_filter_invalid",
                spec = %spec,
                error = %error,
                fallback = DEFAULT_FILTER,
                "configured log filter is invalid, falling back to default"
            );
        }
        if let Some(value) = invalid_format {
            log_invalid_format_warning(value);
        }

        Ok(Self { format, filter })
    }

    fn reload(
        &self,
        resolved: &ResolvedLoggingConfig,
        invalid_format: Option<&str>,
    ) -> Result<(), LoggingError> {
        self.filter
            .reload(build_filter(&resolved.filter))
            .map_err(|error| LoggingError::Reload(error.to_string()))?;
        self.format
            .store(format_code(resolved.format), Ordering::Release);
        if let Some(value) = invalid_format {
            log_invalid_format_warning(value);
        }
        Ok(())
    }
}

#[derive(Clone)]
struct ActiveFormatWriter {
    format: Arc<AtomicU8>,
    active_format: LogFormat,
}

impl ActiveFormatWriter {
    fn new(format: Arc<AtomicU8>, active_format: LogFormat) -> Self {
        Self {
            format,
            active_format,
        }
    }
}

impl<'a> MakeWriter<'a> for ActiveFormatWriter {
    type Writer = Box<dyn Write + Send + 'a>;

    fn make_writer(&'a self) -> Self::Writer {
        if self.format.load(Ordering::Acquire) == format_code(self.active_format) {
            Box::new(io::stderr())
        } else {
            Box::new(NullWriter)
        }
    }
}

struct NullWriter;

impl Write for NullWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn build_filter(spec: &str) -> EnvFilter {
    EnvFilter::try_new(spec).unwrap_or_else(|error| {
        tracing::warn!(
            event = "agentd.logging_filter_invalid",
            spec = spec,
            error = %error,
            fallback = DEFAULT_FILTER,
            "configured log filter is invalid, falling back to default"
        );
        EnvFilter::new(DEFAULT_FILTER)
    })
}

fn resolve_log_format(value: Option<&str>) -> (LogFormat, Option<String>) {
    match value.filter(|value| !value.is_empty()) {
        Some("json") => (LogFormat::Json, None),
        Some("pretty") => (LogFormat::Pretty, None),
        Some(other) => (DEFAULT_FORMAT, Some(other.to_string())),
        None => (DEFAULT_FORMAT, None),
    }
}

fn log_invalid_format_warning(value: &str) {
    tracing::warn!(
        event = "agentd.logging_format_invalid",
        value = value,
        fallback = "json",
        "configured log format is invalid, falling back to default"
    );
}

fn format_code(format: LogFormat) -> u8 {
    match format {
        LogFormat::Pretty => 0,
        LogFormat::Json => 1,
    }
}

static TRACING_HANDLE: OnceLock<TracingHandle> = OnceLock::new();

fn build_json_layer<S, W>(
    writer: W,
) -> tracing_fmt::Layer<
    S,
    tracing_subscriber::fmt::format::JsonFields,
    tracing_subscriber::fmt::format::Format<tracing_subscriber::fmt::format::Json, SystemTime>,
    W,
>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    tracing_fmt::layer()
        .json()
        .with_writer(writer)
        .with_ansi(false)
        .with_timer(SystemTime)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use super::{LogFormat, build_json_layer, resolve_log_format, resolve_logging_config_with_env};
    use tracing_subscriber::Registry;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone)]
    struct SharedBuffer {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedBuffer {
        fn new(inner: Arc<Mutex<Vec<u8>>>) -> Self {
            Self { inner }
        }
    }

    impl<'a> MakeWriter<'a> for SharedBuffer {
        type Writer = SharedBufferWriter;

        fn make_writer(&'a self) -> Self::Writer {
            SharedBufferWriter {
                inner: self.inner.clone(),
            }
        }
    }

    struct SharedBufferWriter {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for SharedBufferWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner
                .lock()
                .expect("trace buffer should be lockable")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn resolve_logging_config_defaults_to_json_info() {
        let resolved = resolve_logging_config_with_env(None, None);

        assert_eq!(resolved.format, LogFormat::Json);
        assert_eq!(resolved.filter, "info");
    }

    #[test]
    fn resolve_logging_config_prefers_rust_log_over_agentd_log() {
        let resolved = resolve_logging_config_with_env(Some("error"), Some("info"));

        assert_eq!(resolved.filter, "error");
    }

    #[test]
    fn resolve_logging_config_uses_agentd_log_when_rust_log_missing() {
        let resolved = resolve_logging_config_with_env(None, Some("debug"));

        assert_eq!(resolved.filter, "debug");
    }

    #[test]
    fn resolve_logging_config_ignores_empty_filter_values() {
        let resolved = resolve_logging_config_with_env(Some(""), Some(""));

        assert_eq!(resolved.filter, "info");
    }

    #[test]
    fn resolve_log_format_defaults_to_json() {
        let (format, invalid) = resolve_log_format(None);

        assert_eq!(format, LogFormat::Json);
        assert_eq!(invalid, None);
    }

    #[test]
    fn resolve_log_format_accepts_pretty() {
        let (format, invalid) = resolve_log_format(Some("pretty"));

        assert_eq!(format, LogFormat::Pretty);
        assert_eq!(invalid, None);
    }

    #[test]
    fn resolve_log_format_falls_back_to_json_for_invalid_values() {
        let (format, invalid) = resolve_log_format(Some("text"));

        assert_eq!(format, LogFormat::Json);
        assert_eq!(invalid.as_deref(), Some("text"));
    }

    #[test]
    fn json_logs_include_timestamps() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let subscriber =
            Registry::default().with(build_json_layer(SharedBuffer::new(buffer.clone())));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(event = "agentd.logging_test", "json log event");
        });

        let output = String::from_utf8(
            buffer
                .lock()
                .expect("trace buffer should be lockable")
                .clone(),
        )
        .expect("trace output should be valid UTF-8");

        assert!(
            output.contains("\"timestamp\""),
            "expected timestamp field in json output: {output}"
        );
    }
}
