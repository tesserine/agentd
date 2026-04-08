use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use agentd_runner::SessionOutcome;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::{DispatchError, ManualRunRequest, SessionExecutor, dispatch_manual_run};

const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Startup or runtime failures for the foreground daemon loop.
#[derive(Debug)]
pub enum DaemonError {
    AlreadyRunning { pid: Option<u32> },
    Io(io::Error),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning { pid: Some(pid) } => {
                write!(f, "agentd is already running (pid {pid})")
            }
            Self::AlreadyRunning { pid: None } => write!(f, "agentd is already running"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for DaemonError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Errors returned to operator-side client commands.
#[derive(Debug)]
pub enum ClientError {
    DaemonNotRunning { path: PathBuf },
    Io(io::Error),
    Protocol(serde_json::Error),
    Server { message: String },
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DaemonNotRunning { path } => {
                write!(f, "agentd is not running (socket {})", path.display())
            }
            Self::Io(error) => write!(f, "{error}"),
            Self::Protocol(error) => write!(f, "{error}"),
            Self::Server { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::DaemonNotRunning { .. } | Self::Server { .. } => None,
        }
    }
}

impl From<io::Error> for ClientError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(error: serde_json::Error) -> Self {
        Self::Protocol(error)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RequestMessage {
    Ping,
    Run {
        agent: String,
        repo_url: String,
        work_unit: Option<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseMessage {
    Pong,
    SessionOutcome { outcome: OutcomeMessage },
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum OutcomeMessage {
    Succeeded,
    Failed { exit_code: i32 },
    TimedOut,
}

impl From<SessionOutcome> for OutcomeMessage {
    fn from(outcome: SessionOutcome) -> Self {
        match outcome {
            SessionOutcome::Succeeded => Self::Succeeded,
            SessionOutcome::Failed { exit_code } => Self::Failed { exit_code },
            SessionOutcome::TimedOut => Self::TimedOut,
        }
    }
}

impl From<OutcomeMessage> for SessionOutcome {
    fn from(outcome: OutcomeMessage) -> Self {
        match outcome {
            OutcomeMessage::Succeeded => Self::Succeeded,
            OutcomeMessage::Failed { exit_code } => Self::Failed { exit_code },
            OutcomeMessage::TimedOut => Self::TimedOut,
        }
    }
}

/// Run the foreground daemon until `shutdown` becomes true.
pub fn run_daemon_until_shutdown(
    config: Config,
    executor: &impl SessionExecutor,
    shutdown: &AtomicBool,
) -> Result<(), DaemonError> {
    let runtime = DaemonRuntime::bind(config.daemon().socket_path(), config.daemon().pid_file())?;
    tracing::info!(
        event = "agentd.daemon_started",
        socket_path = %config.daemon().socket_path().display(),
        pid_file = %config.daemon().pid_file().display(),
        "agentd daemon started"
    );

    while !shutdown.load(Ordering::Acquire) {
        match runtime.listener.accept() {
            Ok((stream, _)) => handle_connection(stream, &config, executor),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => return Err(DaemonError::Io(error)),
        }
    }

    tracing::info!(event = "agentd.daemon_stopped", "agentd daemon stopped");
    Ok(())
}

/// Trigger a manual run against the local daemon and wait for its terminal outcome.
pub fn request_manual_run(
    config: &Config,
    request: &ManualRunRequest,
) -> Result<SessionOutcome, ClientError> {
    match send_request(
        config.daemon().socket_path(),
        &RequestMessage::Run {
            agent: request.agent.clone(),
            repo_url: request.repo_url.clone(),
            work_unit: request.work_unit.clone(),
        },
    )? {
        ResponseMessage::SessionOutcome { outcome } => Ok(outcome.into()),
        ResponseMessage::Error { message } => Err(ClientError::Server { message }),
        ResponseMessage::Pong => Err(ClientError::Server {
            message: "unexpected pong from daemon".to_string(),
        }),
    }
}

fn send_request(
    socket_path: &Path,
    request: &RequestMessage,
) -> Result<ResponseMessage, ClientError> {
    let mut stream = UnixStream::connect(socket_path).map_err(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        ) {
            ClientError::DaemonNotRunning {
                path: socket_path.to_path_buf(),
            }
        } else {
            ClientError::Io(error)
        }
    })?;
    let payload = serde_json::to_vec(request)?;
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line)?;
    if bytes_read == 0 {
        return Err(ClientError::Server {
            message: "daemon closed the connection without a response".to_string(),
        });
    }

    Ok(serde_json::from_str(&line)?)
}

fn handle_connection(stream: UnixStream, config: &Config, executor: &impl SessionExecutor) {
    if let Err(error) = handle_connection_inner(stream, config, executor) {
        tracing::warn!(
            event = "agentd.operator_connection_failed",
            error = %error,
            "operator connection handling failed"
        );
    }
}

fn handle_connection_inner(
    mut stream: UnixStream,
    config: &Config,
    executor: &impl SessionExecutor,
) -> Result<(), io::Error> {
    let request = {
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            return Ok(());
        }

        match serde_json::from_str::<RequestMessage>(&line) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut stream,
                    &ResponseMessage::Error {
                        message: format!("invalid request: {error}"),
                    },
                )?;
                return Ok(());
            }
        }
    };

    let response = match request {
        RequestMessage::Ping => ResponseMessage::Pong,
        RequestMessage::Run {
            agent,
            repo_url,
            work_unit,
        } => match dispatch_manual_run(
            config,
            &ManualRunRequest {
                agent,
                repo_url,
                work_unit,
            },
            executor,
        ) {
            Ok(outcome) => ResponseMessage::SessionOutcome {
                outcome: outcome.into(),
            },
            Err(error) => {
                tracing::warn!(
                    event = "agentd.manual_run_rejected",
                    error = %error,
                    "manual run request rejected"
                );
                ResponseMessage::Error {
                    message: dispatch_error_message(&error),
                }
            }
        },
    };

    write_response(&mut stream, &response)
}

fn write_response(stream: &mut UnixStream, response: &ResponseMessage) -> Result<(), io::Error> {
    let payload = serde_json::to_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn dispatch_error_message(error: &DispatchError) -> String {
    error.to_string()
}

struct DaemonRuntime {
    listener: UnixListener,
    _pid_lock: File,
    pid_file: PathBuf,
    socket_path: PathBuf,
}

impl DaemonRuntime {
    fn bind(socket_path: &Path, pid_file: &Path) -> Result<Self, DaemonError> {
        if let Some(parent) = pid_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut pid_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(pid_file)?;

        if !try_lock_exclusive(&pid_lock)? {
            return Err(DaemonError::AlreadyRunning {
                pid: read_pid(pid_file),
            });
        }

        pid_lock.set_len(0)?;
        write!(&mut pid_lock, "{}", std::process::id())?;
        pid_lock.sync_data()?;

        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        let listener = UnixListener::bind(socket_path)?;
        listener.set_nonblocking(true)?;

        Ok(Self {
            listener,
            _pid_lock: pid_lock,
            pid_file: pid_file.to_path_buf(),
            socket_path: socket_path.to_path_buf(),
        })
    }
}

impl Drop for DaemonRuntime {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.pid_file);
    }
}

fn try_lock_exclusive(file: &File) -> Result<bool, io::Error> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(libc::EWOULDBLOCK)) {
        Ok(false)
    } else {
        Err(error)
    }
}

fn read_pid(pid_file: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_file)
        .ok()
        .and_then(|contents| contents.trim().parse::<u32>().ok())
}
