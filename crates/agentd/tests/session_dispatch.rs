use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};

use agentd::config::{Config, ConfigError};
use agentd::{DispatchError, RunRequest, SessionExecutor, dispatch_run};
use agentd_runner::{
    ResolvedEnvironmentVariable, RunnerError, SessionInvocation, SessionOutcome, SessionSpec,
};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct RecordingExecutor {
    state: Arc<Mutex<RecordingState>>,
}

struct RecordingState {
    outcome: SessionOutcome,
    last_spec: Option<SessionSpec>,
    last_invocation: Option<SessionInvocation>,
}

impl RecordingExecutor {
    fn succeeding(outcome: SessionOutcome) -> (Self, Arc<Mutex<RecordingState>>) {
        let state = Arc::new(Mutex::new(RecordingState {
            outcome,
            last_spec: None,
            last_invocation: None,
        }));

        (
            Self {
                state: state.clone(),
            },
            state,
        )
    }
}

impl SessionExecutor for RecordingExecutor {
    fn run_session(
        &self,
        spec: SessionSpec,
        invocation: SessionInvocation,
    ) -> Result<SessionOutcome, RunnerError> {
        let mut state = self.state.lock().expect("recording state should lock");
        state.last_spec = Some(spec);
        state.last_invocation = Some(invocation);
        Ok(state.outcome.clone())
    }
}

fn config_with_repo_token_source(repo_token_source: &str) -> Config {
    Config::from_str(&format!(
        r#"
[[profiles]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"
repo_token_source = "{repo_token_source}"

[profiles.runa]
command = ["codex", "exec"]

[[profiles.credentials]]
name = "GITHUB_TOKEN"
source = "AGENTD_GITHUB_TOKEN"
"#
    ))
    .expect("config should parse")
}

#[test]
fn dispatch_run_resolves_repo_token_without_injecting_it_into_runtime_environment() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe {
        std::env::set_var("AGENTD_GITHUB_TOKEN", "runtime-secret");
        std::env::set_var("CODEX_REPO_TOKEN", "clone-only-secret");
    }
    let config = config_with_repo_token_source("CODEX_REPO_TOKEN");
    let request = RunRequest {
        profile: "codex".to_string(),
        repo_url: "https://example.com/repo.git".to_string(),
        work_unit: Some("task-42".to_string()),
    };
    let (executor, state) = RecordingExecutor::succeeding(SessionOutcome::Succeeded);

    let outcome = dispatch_run(&config, &request, &executor).expect("dispatch should succeed");

    assert_eq!(outcome, SessionOutcome::Succeeded);

    let state = state.lock().expect("recording state should lock");
    let spec = state
        .last_spec
        .as_ref()
        .expect("executor should receive spec");
    let invocation = state
        .last_invocation
        .as_ref()
        .expect("executor should receive invocation");

    assert_eq!(spec.profile_name, "codex");
    assert_eq!(spec.base_image, "ghcr.io/example/codex:latest");
    assert_eq!(spec.methodology_dir, Path::new("../groundwork"));
    assert_eq!(
        spec.daemon_instance_id,
        config
            .daemon()
            .daemon_instance_id()
            .expect("daemon instance id should resolve")
    );
    assert_eq!(spec.command, vec!["codex".to_string(), "exec".to_string()]);
    assert_eq!(
        spec.environment,
        vec![ResolvedEnvironmentVariable {
            name: "GITHUB_TOKEN".to_string(),
            value: "runtime-secret".to_string(),
        }]
    );
    assert_eq!(invocation.repo_url, "https://example.com/repo.git");
    assert_eq!(invocation.repo_token.as_deref(), Some("clone-only-secret"));
    assert_eq!(invocation.work_unit.as_deref(), Some("task-42"));
    assert_eq!(invocation.timeout, None);

    unsafe {
        std::env::remove_var("AGENTD_GITHUB_TOKEN");
        std::env::remove_var("CODEX_REPO_TOKEN");
    }
}

#[test]
fn dispatch_run_omits_repo_token_when_source_env_var_is_missing() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe {
        std::env::set_var("AGENTD_GITHUB_TOKEN", "runtime-secret");
        std::env::remove_var("CODEX_REPO_TOKEN");
    }
    let config = config_with_repo_token_source("CODEX_REPO_TOKEN");
    let request = RunRequest {
        profile: "codex".to_string(),
        repo_url: "https://example.com/repo.git".to_string(),
        work_unit: None,
    };
    let (executor, state) = RecordingExecutor::succeeding(SessionOutcome::Succeeded);

    dispatch_run(&config, &request, &executor).expect("dispatch should succeed");

    let state = state.lock().expect("recording state should lock");
    let invocation = state
        .last_invocation
        .as_ref()
        .expect("executor should receive invocation");

    assert_eq!(invocation.repo_token, None);

    unsafe {
        std::env::remove_var("AGENTD_GITHUB_TOKEN");
    }
}

#[test]
fn dispatch_run_omits_repo_token_when_source_env_var_is_empty() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe {
        std::env::set_var("AGENTD_GITHUB_TOKEN", "runtime-secret");
        std::env::set_var("CODEX_REPO_TOKEN", "");
    }
    let config = config_with_repo_token_source("CODEX_REPO_TOKEN");
    let request = RunRequest {
        profile: "codex".to_string(),
        repo_url: "https://example.com/repo.git".to_string(),
        work_unit: None,
    };
    let (executor, state) = RecordingExecutor::succeeding(SessionOutcome::Succeeded);

    dispatch_run(&config, &request, &executor).expect("dispatch should succeed");

    let state = state.lock().expect("recording state should lock");
    let invocation = state
        .last_invocation
        .as_ref()
        .expect("executor should receive invocation");

    assert_eq!(invocation.repo_token, None);

    unsafe {
        std::env::remove_var("AGENTD_GITHUB_TOKEN");
        std::env::remove_var("CODEX_REPO_TOKEN");
    }
}

#[test]
fn dispatch_run_errors_when_runtime_credential_source_is_missing() {
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe {
        std::env::remove_var("AGENTD_GITHUB_TOKEN");
        std::env::set_var("CODEX_REPO_TOKEN", "clone-only-secret");
    }
    let config = config_with_repo_token_source("CODEX_REPO_TOKEN");
    let request = RunRequest {
        profile: "codex".to_string(),
        repo_url: "https://example.com/repo.git".to_string(),
        work_unit: None,
    };
    let (executor, _state) = RecordingExecutor::succeeding(SessionOutcome::Succeeded);

    let error = dispatch_run(&config, &request, &executor)
        .expect_err("missing runtime credential sources should fail dispatch");

    match error {
        DispatchError::MissingCredentialSource {
            profile,
            credential,
            source,
        } => {
            assert_eq!(profile, "codex");
            assert_eq!(credential, "GITHUB_TOKEN");
            assert_eq!(source, "AGENTD_GITHUB_TOKEN");
        }
        other => panic!("expected missing credential source error, got {other:?}"),
    }

    unsafe {
        std::env::remove_var("CODEX_REPO_TOKEN");
    }
}

#[test]
fn dispatch_run_rejects_relative_daemon_runtime_paths_as_config_errors() {
    let config = Config::from_str(
        r#"
[daemon]
socket_path = "runtime/agentd.sock"
pid_file = "runtime/agentd.pid"

[[profiles]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[profiles.runa]
command = ["codex", "exec"]
"#,
    )
    .expect("config should parse");
    let request = RunRequest {
        profile: "codex".to_string(),
        repo_url: "https://example.com/repo.git".to_string(),
        work_unit: None,
    };
    let (executor, _state) = RecordingExecutor::succeeding(SessionOutcome::Succeeded);

    let error =
        dispatch_run(&config, &request, &executor).expect_err("relative daemon paths should fail");

    match error {
        DispatchError::Config(ConfigError::RelativeDaemonRuntimePath { field, path }) => {
            assert_eq!(field, "daemon.socket_path");
            assert_eq!(path, Path::new("runtime/agentd.sock"));
        }
        other => panic!("expected config error, got {other:?}"),
    }
}
