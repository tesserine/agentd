use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::{self, JoinHandle};

use agentd_scheduler::{
    Clock, DispatchError, Dispatcher, ScheduledProfile, ScheduledRunRequest, Scheduler,
    SystemClock, run_until_shutdown,
};

use crate::RunRequest;
use crate::config::{Config, DaemonConfig};
use crate::daemon::request_run_without_waiting;

pub(crate) fn spawn_scheduler_thread(
    config: &Config,
    shutdown: Arc<AtomicBool>,
) -> std::io::Result<Option<JoinHandle<()>>> {
    let scheduled_profiles = scheduled_profiles_from_config(config);
    if scheduled_profiles.is_empty() {
        return Ok(None);
    }

    let daemon_config = config.daemon().clone();
    thread::Builder::new()
        .name("agentd-scheduler".to_string())
        .spawn(move || {
            let clock = SystemClock;
            let mut scheduler = Scheduler::new(scheduled_profiles, clock.now())
                .expect("config validation should guarantee valid schedules");
            let dispatcher = SocketDispatcher { daemon_config };
            run_until_shutdown(&mut scheduler, &dispatcher, &clock, shutdown.as_ref());
        })
        .map(Some)
}

pub(crate) fn join_scheduler_thread(handle: Option<JoinHandle<()>>) {
    let Some(handle) = handle else {
        return;
    };

    if handle.join().is_err() {
        tracing::error!(
            event = "agentd.scheduler_panicked",
            "scheduler thread panicked"
        );
    }
}

fn scheduled_profiles_from_config(config: &Config) -> Vec<ScheduledProfile> {
    config
        .profiles()
        .iter()
        .filter_map(|profile| {
            let schedule = profile.schedule()?;
            let repo = profile
                .repo()
                .expect("config validation should guarantee scheduled profiles declare repo");
            Some(
                ScheduledProfile::new(profile.name().to_string(), repo.to_string(), schedule)
                    .expect("config validation should guarantee valid schedules"),
            )
        })
        .collect()
}

#[derive(Debug, Clone)]
struct SocketDispatcher {
    daemon_config: DaemonConfig,
}

impl Dispatcher for SocketDispatcher {
    fn dispatch(&self, request: ScheduledRunRequest) -> Result<(), DispatchError> {
        let daemon_config = self.daemon_config.clone();
        thread::Builder::new()
            .name(format!("agentd-scheduled-dispatch-{}", request.profile))
            .spawn(move || {
                if let Err(error) = request_run_without_waiting(
                    &daemon_config,
                    &RunRequest {
                        profile: request.profile.clone(),
                        repo_url: request.repo_url.clone(),
                        work_unit: None,
                        input: None,
                    },
                ) {
                    tracing::warn!(
                        event = "agentd.scheduled_run_dispatch_failed",
                        profile = request.profile,
                        error = %error,
                        "scheduled run dispatch failed"
                    );
                }
            })
            .map(|_| ())
            .map_err(|error| DispatchError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, ErrorKind};
    use std::os::unix::net::UnixListener;
    use std::str::FromStr;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::{SessionExecutor, run_daemon_until_shutdown};
    use agentd_runner::{RunnerError, SessionInvocation, SessionOutcome, SessionSpec};

    #[derive(Clone)]
    struct RecordingExecutor {
        invocations: Arc<Mutex<Vec<SessionInvocation>>>,
    }

    impl RecordingExecutor {
        fn new() -> (Self, Arc<Mutex<Vec<SessionInvocation>>>) {
            let invocations = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    invocations: Arc::clone(&invocations),
                },
                invocations,
            )
        }
    }

    impl SessionExecutor for RecordingExecutor {
        fn run_session(
            &self,
            _spec: SessionSpec,
            invocation: SessionInvocation,
        ) -> Result<SessionOutcome, RunnerError> {
            self.invocations
                .lock()
                .expect("invocations should lock")
                .push(invocation);
            Ok(SessionOutcome::Success { exit_code: 0 })
        }
    }

    fn wait_for_path(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if path.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }

        panic!("timed out waiting for {}", path.display());
    }

    fn unique_runtime_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "agentd-scheduler-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("runtime dir should be created");
        path
    }

    #[test]
    fn scheduled_profiles_ignore_profiles_without_schedule() {
        let config = Config::from_str(
            r#"
[daemon]
socket_path = "/tmp/agentd.sock"
pid_file = "/tmp/agentd.pid"

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"
repo = "https://example.com/site.git"
schedule = "*/15 * * * *"

command = ["site-builder", "exec"]

[[profiles]]
name = "code-reviewer"
base_image = "ghcr.io/example/code-reviewer:latest"
methodology_dir = "../groundwork"
repo = "https://example.com/review.git"

command = ["code-reviewer", "exec"]
"#,
        )
        .expect("config should parse");

        let scheduled_profiles = scheduled_profiles_from_config(&config);

        assert_eq!(scheduled_profiles.len(), 1);
    }

    #[test]
    fn socket_dispatcher_sends_runs_through_the_daemon_socket() {
        let runtime_dir = unique_runtime_dir("dispatch");
        let config = Config::from_str(&format!(
            r#"
[daemon]
socket_path = "{socket_path}"
pid_file = "{pid_file}"

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"
repo = "https://example.com/site.git"

command = ["site-builder", "exec"]
"#,
            socket_path = runtime_dir.join("agentd.sock").display(),
            pid_file = runtime_dir.join("agentd.pid").display(),
        ))
        .expect("config should parse");
        let shutdown = Arc::new(AtomicBool::new(false));
        let daemon_config = config.clone();
        let daemon_shutdown = Arc::clone(&shutdown);
        let (executor, invocations) = RecordingExecutor::new();
        let handle = thread::spawn(move || {
            run_daemon_until_shutdown(daemon_config, executor, daemon_shutdown)
        });
        wait_for_path(config.daemon().socket_path());

        let dispatcher = SocketDispatcher {
            daemon_config: config.daemon().clone(),
        };
        dispatcher
            .dispatch(ScheduledRunRequest {
                profile: "site-builder".to_string(),
                repo_url: "https://example.com/site.git".to_string(),
            })
            .expect("dispatch should spawn a socket client");

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !invocations
                .lock()
                .expect("invocations should lock")
                .is_empty()
            {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }

        let invocations = invocations.lock().expect("invocations should lock");
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].repo_url, "https://example.com/site.git");

        shutdown.store(true, Ordering::Release);
        handle
            .join()
            .expect("daemon thread should join")
            .expect("daemon should exit cleanly");
    }

    #[test]
    fn socket_dispatcher_closes_the_socket_after_writing_the_run_request() {
        let runtime_dir = unique_runtime_dir("fire-and-forget");
        let socket_path = runtime_dir.join("agentd.sock");
        let listener = UnixListener::bind(&socket_path).expect("listener should bind");
        let config = Config::from_str(&format!(
            r#"
[daemon]
socket_path = "{socket_path}"
pid_file = "{pid_file}"

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"
repo = "https://example.com/site.git"

command = ["site-builder", "exec"]
"#,
            socket_path = socket_path.display(),
            pid_file = runtime_dir.join("agentd.pid").display(),
        ))
        .expect("config should parse");
        let dispatcher = SocketDispatcher {
            daemon_config: config.daemon().clone(),
        };

        dispatcher
            .dispatch(ScheduledRunRequest {
                profile: "site-builder".to_string(),
                repo_url: "https://example.com/site.git".to_string(),
            })
            .expect("dispatch should spawn a socket client");

        let (stream, _) = listener.accept().expect("dispatcher should connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("stream timeout should be configured");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let bytes_read = reader
            .read_line(&mut line)
            .expect("dispatcher should write one json line");
        assert!(bytes_read > 0, "expected a run request payload");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).expect("request should be valid json"),
            serde_json::json!({
                "type": "run",
                "profile": "site-builder",
                "repo_url": "https://example.com/site.git",
                "work_unit": null,
                "input": null,
            })
        );

        line.clear();
        let eof = reader.read_line(&mut line);
        match eof {
            Ok(0) => {}
            Ok(bytes_read) => panic!("expected EOF after the request line, got {bytes_read} bytes"),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                panic!("dispatcher kept the socket open instead of closing after the write")
            }
            Err(error) => panic!("expected EOF after the request line, got {error}"),
        }
    }
}
