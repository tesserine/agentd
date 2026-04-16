use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use agentd_runner::{
    BindMount, ResolvedEnvironmentVariable, SessionInvocation, SessionOutcome, SessionSpec,
    run_session,
};

const TEST_DAEMON_INSTANCE_ID: &str = "1a2b3c4d";

#[test]
fn succeeds_without_timeout_and_cleans_up_container() {
    if skip_if_podman_unavailable("succeeds_without_timeout_and_cleans_up_container") {
        return;
    }
    let _guard = podman_test_lock()
        .lock()
        .expect("podman test lock should be acquired");

    let fixture = SessionFixture::new("success-run");
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            daemon_instance_id: TEST_DAEMON_INSTANCE_ID.to_string(),
            profile_name: "success-run".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            mounts: Vec::new(),
            command: vec![
                "site-builder".to_string(),
                "exec".to_string(),
                "--sandbox".to_string(),
                "workspace-write".to_string(),
            ],
            environment: vec![
                ResolvedEnvironmentVariable {
                    name: "GITHUB_TOKEN".to_string(),
                    value: "test-token".to_string(),
                },
                ResolvedEnvironmentVariable {
                    name: "SESSION_TEST_BEHAVIOR".to_string(),
                    value: "success".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: fixture.repo_url(),
            repo_token: None,
            work_unit: Some("task-42".to_string()),
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Success { exit_code: 0 });
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn succeeds_with_empty_and_non_empty_environment_values() {
    if skip_if_podman_unavailable("succeeds_with_empty_and_non_empty_environment_values") {
        return;
    }
    let _guard = podman_test_lock()
        .lock()
        .expect("podman test lock should be acquired");

    let fixture = SessionFixture::new("mixed-env-run");
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            daemon_instance_id: TEST_DAEMON_INSTANCE_ID.to_string(),
            profile_name: "mixed-env-run".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            mounts: Vec::new(),
            command: vec!["site-builder".to_string(), "exec".to_string()],
            environment: vec![
                ResolvedEnvironmentVariable {
                    name: "GITHUB_TOKEN".to_string(),
                    value: "test-token".to_string(),
                },
                ResolvedEnvironmentVariable {
                    name: "EMPTY_SESSION_ENV".to_string(),
                    value: String::new(),
                },
                ResolvedEnvironmentVariable {
                    name: "SESSION_TEST_BEHAVIOR".to_string(),
                    value: "success-empty-env".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: fixture.repo_url(),
            repo_token: None,
            work_unit: Some("task-42".to_string()),
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Success { exit_code: 0 });
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn clears_inherited_work_unit_when_invocation_omits_it() {
    if skip_if_podman_unavailable("clears_inherited_work_unit_when_invocation_omits_it") {
        return;
    }
    let _guard = podman_test_lock()
        .lock()
        .expect("podman test lock should be acquired");

    let fixture = SessionFixture::new("unset-work-unit-run");
    let image = fixture.build_image_with_agentd_work_unit("stale-from-image");

    let outcome = run_session(
        SessionSpec {
            daemon_instance_id: TEST_DAEMON_INSTANCE_ID.to_string(),
            profile_name: "unset-work-unit-run".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            mounts: Vec::new(),
            command: vec!["site-builder".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "SESSION_TEST_BEHAVIOR".to_string(),
                value: "success-without-work-unit".to_string(),
            }],
        },
        SessionInvocation {
            repo_url: fixture.repo_url(),
            repo_token: None,
            work_unit: None,
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Success { exit_code: 0 });
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn returns_failed_exit_code_without_timeout_and_cleans_up_container() {
    if skip_if_podman_unavailable(
        "returns_failed_exit_code_without_timeout_and_cleans_up_container",
    ) {
        return;
    }
    let _guard = podman_test_lock()
        .lock()
        .expect("podman test lock should be acquired");

    let fixture = SessionFixture::new("failure-run");
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            daemon_instance_id: TEST_DAEMON_INSTANCE_ID.to_string(),
            profile_name: "failure-run".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            mounts: Vec::new(),
            command: vec!["site-builder".to_string(), "exec".to_string()],
            environment: vec![
                ResolvedEnvironmentVariable {
                    name: "GITHUB_TOKEN".to_string(),
                    value: "test-token".to_string(),
                },
                ResolvedEnvironmentVariable {
                    name: "SESSION_TEST_BEHAVIOR".to_string(),
                    value: "fail".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: fixture.repo_url(),
            repo_token: None,
            work_unit: None,
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::GenericFailure { exit_code: 23 });
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn returns_failed_exit_code_125_without_timeout_and_cleans_up_runner_resources() {
    if skip_if_podman_unavailable(
        "returns_failed_exit_code_125_without_timeout_and_cleans_up_runner_resources",
    ) {
        return;
    }
    let _guard = podman_test_lock()
        .lock()
        .expect("podman test lock should be acquired");

    let fixture = SessionFixture::new("failure-run-125");
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            daemon_instance_id: TEST_DAEMON_INSTANCE_ID.to_string(),
            profile_name: "failure-run-125".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            mounts: Vec::new(),
            command: vec!["site-builder".to_string(), "exec".to_string()],
            environment: vec![
                ResolvedEnvironmentVariable {
                    name: "GITHUB_TOKEN".to_string(),
                    value: "test-token".to_string(),
                },
                ResolvedEnvironmentVariable {
                    name: "SESSION_TEST_BEHAVIOR".to_string(),
                    value: "fail-125".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: fixture.repo_url(),
            repo_token: None,
            work_unit: None,
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::GenericFailure { exit_code: 125 });
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn succeeds_when_methodology_dir_path_contains_commas() {
    if skip_if_podman_unavailable("succeeds_when_methodology_dir_path_contains_commas") {
        return;
    }
    let _guard = podman_test_lock()
        .lock()
        .expect("podman test lock should be acquired");

    let fixture = SessionFixture::new_with_root_prefix(
        "comma-methodology-run",
        "agentd-runner,comma,methodology",
    );
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            daemon_instance_id: TEST_DAEMON_INSTANCE_ID.to_string(),
            profile_name: "comma-methodology-run".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            mounts: Vec::new(),
            command: vec!["site-builder".to_string(), "exec".to_string()],
            environment: vec![
                ResolvedEnvironmentVariable {
                    name: "GITHUB_TOKEN".to_string(),
                    value: "test-token".to_string(),
                },
                ResolvedEnvironmentVariable {
                    name: "SESSION_TEST_BEHAVIOR".to_string(),
                    value: "success".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: fixture.repo_url(),
            repo_token: None,
            work_unit: Some("task-42".to_string()),
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Success { exit_code: 0 });
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn validates_read_only_additional_mounts_from_paths_containing_commas() {
    if skip_if_podman_unavailable(
        "validates_read_only_additional_mounts_from_paths_containing_commas",
    ) {
        return;
    }
    let _guard = podman_test_lock()
        .lock()
        .expect("podman test lock should be acquired");

    let fixture = SessionFixture::new("readonly-mount-run");
    let image = fixture.build_image();
    let host_mount = fixture.root.join("host,readonly");
    fs::create_dir_all(&host_mount).expect("read-only host mount should be created");
    fs::write(host_mount.join("auth.json"), "{\"token\":\"test\"}\n")
        .expect("read-only host fixture file should be written");
    fs::write(
        host_mount.join("sentinel.txt"),
        "host data should remain untouched\n",
    )
    .expect("read-only host sentinel file should be written");

    let outcome = run_session(
        SessionSpec {
            daemon_instance_id: TEST_DAEMON_INSTANCE_ID.to_string(),
            profile_name: "readonly-mount-run".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            mounts: vec![BindMount {
                source: host_mount.clone(),
                target: PathBuf::from("/home/readonly-mount-run/.claude"),
                read_only: true,
            }],
            command: vec!["site-builder".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "SESSION_TEST_BEHAVIOR".to_string(),
                value: "verify-read-only-mount".to_string(),
            }],
        },
        SessionInvocation {
            repo_url: fixture.repo_url(),
            repo_token: None,
            work_unit: None,
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Success { exit_code: 0 });
    assert!(
        !host_mount.join("write-should-fail").exists(),
        "read-only mount should not permit in-container writes"
    );
    assert_eq!(
        fs::read_to_string(host_mount.join("auth.json"))
            .expect("read-only host auth fixture should remain readable"),
        "{\"token\":\"test\"}\n"
    );
    assert_eq!(
        fs::read_to_string(host_mount.join("sentinel.txt"))
            .expect("read-only host sentinel should remain readable"),
        "host data should remain untouched\n"
    );
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn preserves_host_writes_through_read_write_additional_mounts() {
    if skip_if_podman_unavailable("preserves_host_writes_through_read_write_additional_mounts") {
        return;
    }
    let _guard = podman_test_lock()
        .lock()
        .expect("podman test lock should be acquired");

    let fixture = SessionFixture::new("readwrite-mount-run");
    let image = fixture.build_image();
    let host_mount = fixture.root.join("host-readwrite");
    fs::create_dir_all(&host_mount).expect("read-write host mount should be created");
    fs::write(host_mount.join("sentinel.txt"), "host sentinel\n")
        .expect("read-write host sentinel should be written");
    #[cfg(unix)]
    let sentinel_metadata_before =
        fs::metadata(host_mount.join("sentinel.txt")).expect("sentinel metadata should exist");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&host_mount, fs::Permissions::from_mode(0o777))
            .expect("read-write host mount should permit container writes");
    }

    let outcome = run_session(
        SessionSpec {
            daemon_instance_id: TEST_DAEMON_INSTANCE_ID.to_string(),
            profile_name: "readwrite-mount-run".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            mounts: vec![BindMount {
                source: host_mount.clone(),
                target: PathBuf::from("/home/readwrite-mount-run/.runa"),
                read_only: false,
            }],
            command: vec!["site-builder".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "SESSION_TEST_BEHAVIOR".to_string(),
                value: "write-read-write-mount".to_string(),
            }],
        },
        SessionInvocation {
            repo_url: fixture.repo_url(),
            repo_token: None,
            work_unit: None,
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Success { exit_code: 0 });
    assert_eq!(
        fs::read_to_string(host_mount.join("session-artifact.txt"))
            .expect("read-write mount should persist host-visible writes"),
        "persisted from container\n"
    );
    assert_eq!(
        fs::read_to_string(host_mount.join("sentinel.txt"))
            .expect("read-write host sentinel should remain readable"),
        "host sentinel\n"
    );
    #[cfg(unix)]
    {
        let sentinel_metadata_after =
            fs::metadata(host_mount.join("sentinel.txt")).expect("sentinel metadata should exist");
        assert_eq!(
            sentinel_metadata_after.uid(),
            sentinel_metadata_before.uid(),
            "runner setup must not re-own host-backed files under home mounts"
        );
        assert_eq!(
            sentinel_metadata_after.gid(),
            sentinel_metadata_before.gid(),
            "runner setup must not re-own host-backed files under home mounts"
        );
    }
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn times_out_when_a_timeout_is_provided_and_cleans_up_container() {
    if skip_if_podman_unavailable("times_out_when_a_timeout_is_provided_and_cleans_up_container") {
        return;
    }
    let _guard = podman_test_lock()
        .lock()
        .expect("podman test lock should be acquired");

    let fixture = SessionFixture::new("timeout-run");
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            daemon_instance_id: TEST_DAEMON_INSTANCE_ID.to_string(),
            profile_name: "timeout-run".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            mounts: Vec::new(),
            command: vec!["site-builder".to_string(), "exec".to_string()],
            environment: vec![
                ResolvedEnvironmentVariable {
                    name: "GITHUB_TOKEN".to_string(),
                    value: "test-token".to_string(),
                },
                ResolvedEnvironmentVariable {
                    name: "SESSION_TEST_BEHAVIOR".to_string(),
                    value: "sleep".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: fixture.repo_url(),
            repo_token: None,
            work_unit: None,
            timeout: Some(Duration::from_secs(1)),
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::TimedOut);
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn releases_session_secret_after_container_reaches_running_state() {
    if skip_if_podman_unavailable("releases_session_secret_after_container_reaches_running_state") {
        return;
    }
    let _guard = podman_test_lock()
        .lock()
        .expect("podman test lock should be acquired");

    let fixture = SessionFixture::new("running-secret-run");
    let image = fixture.build_image();
    let methodology_dir = fixture.methodology_dir();
    let repo_url = fixture.repo_url();

    let session = thread::spawn(move || {
        run_session(
            SessionSpec {
                daemon_instance_id: TEST_DAEMON_INSTANCE_ID.to_string(),
                profile_name: "running-secret-run".to_string(),
                base_image: image,
                methodology_dir,
                mounts: Vec::new(),
                command: vec!["site-builder".to_string(), "exec".to_string()],
                environment: vec![
                    ResolvedEnvironmentVariable {
                        name: "GITHUB_TOKEN".to_string(),
                        value: "test-token".to_string(),
                    },
                    ResolvedEnvironmentVariable {
                        name: "SESSION_TEST_BEHAVIOR".to_string(),
                        value: "sleep-short".to_string(),
                    },
                ],
            },
            SessionInvocation {
                repo_url,
                repo_token: None,
                work_unit: None,
                timeout: None,
            },
        )
    });

    let session_id = fixture.wait_for_runner_container_to_be_running(Duration::from_secs(5));
    fixture.wait_for_runner_secrets_to_be_released(&session_id, Duration::from_secs(5));

    let outcome = session
        .join()
        .expect("session thread should complete")
        .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Success { exit_code: 0 });
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

struct SessionFixture {
    root: PathBuf,
    profile_name: String,
    baseline_runner_secret_names: BTreeSet<String>,
    repo_server: RepoHttpServer,
}

impl SessionFixture {
    fn new(profile_name: &str) -> Self {
        Self::new_with_repo_server(profile_name, &format!("agentd-runner-{profile_name}"))
    }

    fn new_with_root_prefix(profile_name: &str, root_prefix: &str) -> Self {
        Self::new_with_repo_server(profile_name, root_prefix)
    }

    fn new_with_repo_server(profile_name: &str, root_prefix: &str) -> Self {
        let root = unique_temp_dir(root_prefix);
        fs::create_dir_all(&root).expect("fixture root should be created");

        let methodology_dir = root.join("methodology");
        fs::create_dir_all(&methodology_dir).expect("methodology directory should be created");
        fs::write(
            methodology_dir.join("manifest.toml"),
            "name = \"test-methodology\"\n",
        )
        .expect("methodology manifest should be written");
        let repo_root = root.join("repo-server");
        let bare_repo_dir = repo_root.join("repo.git");
        fs::create_dir_all(&repo_root).expect("repo root should be created");
        write_test_repo(&bare_repo_dir);

        Self {
            root,
            profile_name: profile_name.to_string(),
            baseline_runner_secret_names: list_runner_secret_names(),
            repo_server: RepoHttpServer::start(repo_root),
        }
    }

    fn methodology_dir(&self) -> PathBuf {
        self.root.join("methodology")
    }

    fn repo_url(&self) -> String {
        format!(
            "http://host.containers.internal:{}/repo.git",
            self.repo_server.port()
        )
    }

    fn build_image(&self) -> String {
        self.build_image_with_agentd_work_unit_line(None)
    }

    fn build_image_with_agentd_work_unit(&self, work_unit: &str) -> String {
        self.build_image_with_agentd_work_unit_line(Some(work_unit))
    }

    fn build_image_with_agentd_work_unit_line(&self, work_unit: Option<&str>) -> String {
        let context_dir = self.root.join("image-context");
        fs::create_dir_all(&context_dir).expect("image context should be created");

        fs::write(context_dir.join("site-builder"), SITE_BUILDER_STUB)
            .expect("site-builder stub should be written");
        fs::write(context_dir.join("entrypoint.sh"), ENTRYPOINT_SH)
            .expect("entrypoint script should be written");
        let containerfile = work_unit
            .map(|work_unit| CONTAINERFILE.replace(
                "FROM docker.io/library/debian:bookworm-slim\n",
                &format!("FROM docker.io/library/debian:bookworm-slim\nENV AGENTD_WORK_UNIT={work_unit}\n"),
            ))
            .unwrap_or_else(|| CONTAINERFILE.to_string());
        fs::write(context_dir.join("Containerfile"), containerfile)
            .expect("containerfile should be written");

        let tag = format!("agentd-runner-test:{}", self.profile_name);
        let status = Command::new("podman")
            .args(["build", "--tag", &tag, context_dir.to_str().unwrap()])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .expect("podman build should start");

        assert!(status.success(), "podman build failed");
        tag
    }

    fn assert_no_runner_container_left_behind(&self) {
        let output = Command::new("podman")
            .args(["ps", "-a", "--format", "{{.Names}}"])
            .output()
            .expect("podman ps should run");
        assert!(
            output.status.success(),
            "podman ps failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let names = String::from_utf8(output.stdout).expect("podman ps output should be utf-8");
        let expected_prefix = format!("agentd-{TEST_DAEMON_INSTANCE_ID}-{}-", self.profile_name);
        assert!(
            !names.lines().any(|name| name.starts_with(&expected_prefix)),
            "runner left container behind with prefix {expected_prefix}: {names}"
        );
    }

    fn assert_no_runner_secret_left_behind(&self) {
        let current_runner_secret_names = list_runner_secret_names();
        let leaked_secret_names = current_runner_secret_names
            .difference(&self.baseline_runner_secret_names)
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            leaked_secret_names.is_empty(),
            "runner left secrets behind: {}",
            leaked_secret_names.join("\n")
        );
    }

    fn wait_for_runner_container_to_be_running(&self, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let expected_prefix = format!("agentd-{TEST_DAEMON_INSTANCE_ID}-{}-", self.profile_name);

        loop {
            let running_container_names = list_running_container_names();
            if let Some(session_id) = running_container_names
                .iter()
                .find_map(|name| name.strip_prefix(&expected_prefix))
            {
                return session_id.to_string();
            }

            assert!(
                Instant::now() < deadline,
                "runner container with prefix {expected_prefix} did not reach running state"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_for_runner_secrets_to_be_released(&self, session_id: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let expected_secret_prefix = format!("agentd-{TEST_DAEMON_INSTANCE_ID}-{session_id}-");
        let expected_container_prefix = format!(
            "agentd-{TEST_DAEMON_INSTANCE_ID}-{}-{session_id}",
            self.profile_name
        );

        loop {
            let matching_secret_names = list_runner_secret_names()
                .into_iter()
                .filter(|name| name.starts_with(&expected_secret_prefix))
                .collect::<Vec<_>>();
            let running_container_names = list_running_container_names();
            let container_is_running = running_container_names
                .iter()
                .any(|name| name == &expected_container_prefix);

            if matching_secret_names.is_empty() {
                assert!(
                    container_is_running,
                    "runner secrets for {expected_secret_prefix} were only released after the container stopped"
                );
                return;
            }

            assert!(
                container_is_running,
                "runner left secrets behind until the container stopped: {}",
                matching_secret_names.join("\n")
            );
            assert!(
                Instant::now() < deadline,
                "runner left secrets behind for {expected_secret_prefix}: {}",
                matching_secret_names.join("\n")
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for SessionFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn skip_if_podman_unavailable(test_name: &str) -> bool {
    if podman_available() {
        return false;
    }

    eprintln!("skipping {test_name}: podman is unavailable");
    true
}

fn podman_available() -> bool {
    let status = Command::new("podman")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

fn podman_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let unique = format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after the unix epoch")
            .as_nanos()
    );

    std::env::temp_dir().join(unique)
}

fn list_running_container_names() -> Vec<String> {
    let output = Command::new("podman")
        .args(["ps", "--format", "{{.Names}}"])
        .output()
        .expect("podman ps should run");
    assert!(
        output.status.success(),
        "podman ps failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .expect("podman ps output should be utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

fn list_runner_secret_names() -> BTreeSet<String> {
    let output = Command::new("podman")
        .args(["secret", "ls", "--format", "{{.Name}}"])
        .output()
        .expect("podman secret ls should run");
    assert!(
        output.status.success(),
        "podman secret ls failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .expect("podman secret ls output should be utf-8")
        .lines()
        .filter(|name| name.starts_with("agentd-"))
        .map(str::to_string)
        .collect()
}

fn write_test_repo(destination: &Path) {
    let source_dir = destination
        .parent()
        .expect("repo destination should have a parent")
        .join("repo-source");
    fs::create_dir_all(&source_dir).expect("repo source directory should be created");
    fs::write(source_dir.join("README.md"), "# test repo\n")
        .expect("fixture repo readme should be written");

    run_git(&source_dir, ["init"]);
    run_git(&source_dir, ["config", "user.name", "agentd-runner-tests"]);
    run_git(
        &source_dir,
        ["config", "user.email", "agentd-runner-tests@example.com"],
    );
    run_git(&source_dir, ["add", "README.md"]);
    run_git(&source_dir, ["commit", "-m", "initial commit"]);
    run_git_in(
        destination
            .parent()
            .expect("repo destination should have a parent"),
        [
            "clone",
            "--bare",
            source_dir.to_str().unwrap(),
            destination.to_str().unwrap(),
        ],
    );
    run_git_in(destination, ["update-server-info"]);
}

fn run_git<const N: usize>(directory: &Path, args: [&str; N]) {
    run_git_in(directory, args);
}

fn run_git_in<const N: usize>(directory: &Path, args: [&str; N]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(directory)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .expect("git command should run");

    assert!(
        status.success(),
        "git command failed in {}",
        directory.display()
    );
}

const CONTAINERFILE: &str = r#"
FROM docker.io/library/debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends git gosu passwd \
    && rm -rf /var/lib/apt/lists/*
COPY site-builder /usr/local/bin/site-builder
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /usr/local/bin/site-builder /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
"#;

const ENTRYPOINT_SH: &str = r#"#!/bin/sh
set -eu

echo "image entrypoint should not run" >&2
exit 97
"#;

struct RepoHttpServer {
    port: u16,
    shutdown: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl RepoHttpServer {
    fn start(root: PathBuf) -> Self {
        let listener = TcpListener::bind(("0.0.0.0", 0))
            .expect("fixture repo HTTP server should bind an ephemeral port");
        listener
            .set_nonblocking(true)
            .expect("fixture repo HTTP server should become nonblocking");
        let port = listener
            .local_addr()
            .expect("fixture repo HTTP server should expose a local address")
            .port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_signal = Arc::clone(&shutdown);
        let thread = thread::spawn(move || serve_repo_http(listener, root, shutdown_signal));

        Self {
            port,
            shutdown,
            thread: Some(thread),
        }
    }

    fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for RepoHttpServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .expect("fixture repo HTTP server thread should stop cleanly");
        }
    }
}

fn serve_repo_http(listener: TcpListener, root: PathBuf, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let root = root.clone();
                thread::spawn(move || handle_repo_http_request(stream, &root));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("fixture repo HTTP server accept failed: {error}"),
        }
    }
}

fn handle_repo_http_request(stream: TcpStream, root: &Path) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
        return;
    }

    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header == "\r\n" {
            break;
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let request_target = parts.next().unwrap_or_default();
    let path = request_target.split('?').next().unwrap_or_default();
    let mut stream = reader.into_inner();

    if method != "GET" && method != "HEAD" {
        write_http_response(
            &mut stream,
            "405 Method Not Allowed",
            b"method not allowed",
            false,
        );
        return;
    }

    let relative_path = path.trim_start_matches('/');
    if relative_path.is_empty() || relative_path.split('/').any(|segment| segment == "..") {
        write_http_response(&mut stream, "404 Not Found", b"not found", method == "HEAD");
        return;
    }

    let file_path = root.join(relative_path);
    let Ok(body) = fs::read(&file_path) else {
        write_http_response(&mut stream, "404 Not Found", b"not found", method == "HEAD");
        return;
    };

    write_http_response(&mut stream, "200 OK", &body, method == "HEAD");
}

fn write_http_response(stream: &mut TcpStream, status: &str, body: &[u8], head_only: bool) {
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .expect("fixture repo HTTP server should write response headers");
    if !head_only {
        stream
            .write_all(body)
            .expect("fixture repo HTTP server should write response body");
    }
}

const SITE_BUILDER_STUB: &str = r#"#!/bin/sh
set -eu

command_name="$1"
shift

case "$command_name" in
    exec)
        [ -f /agentd/methodology/manifest.toml ]
        [ "${PROFILE_NAME:-}" != "" ]
        if [ "${GITHUB_TOKEN+set}" = "set" ]; then
            [ "${GITHUB_TOKEN}" = "test-token" ]
        fi
        [ "$(id -u)" != "0" ]
        [ "$(id -un)" = "${PROFILE_NAME}" ]
        [ "${HOME:-}" = "/home/${PROFILE_NAME}" ]
        [ "$(pwd)" = "/home/${PROFILE_NAME}/repo" ]
        [ -w "${HOME}" ]
        [ -w "${HOME}/repo" ]
        [ -f "${HOME}/repo/README.md" ]

        if [ "${SESSION_TEST_BEHAVIOR:-success}" = "success" ]; then
            [ "${AGENTD_WORK_UNIT:-}" = "task-42" ]
            exit 0
        fi

        if [ "${SESSION_TEST_BEHAVIOR:-}" = "success-empty-env" ]; then
            [ "${EMPTY_SESSION_ENV-__missing__}" = "" ]
            [ "${AGENTD_WORK_UNIT:-}" = "task-42" ]
            exit 0
        fi

        if [ "${SESSION_TEST_BEHAVIOR:-}" = "success-without-work-unit" ]; then
            [ "${AGENTD_WORK_UNIT+set}" != "set" ]
            exit 0
        fi

        if [ "${SESSION_TEST_BEHAVIOR:-}" = "verify-read-only-mount" ]; then
            [ -f "${HOME}/.claude/auth.json" ]
            if touch "${HOME}/.claude/write-should-fail" 2>/dev/null; then
                echo "read-only mount unexpectedly allowed writes" >&2
                exit 91
            fi
            exit 0
        fi

        if [ "${SESSION_TEST_BEHAVIOR:-}" = "write-read-write-mount" ]; then
            printf 'persisted from container\n' > "${HOME}/.runa/session-artifact.txt"
            exit 0
        fi

        if [ "${SESSION_TEST_BEHAVIOR:-}" = "fail" ]; then
            [ "$#" = "0" ]
            exit 23
        fi

        if [ "${SESSION_TEST_BEHAVIOR:-}" = "fail-125" ]; then
            [ "$#" = "0" ]
            exit 125
        fi

        if [ "${SESSION_TEST_BEHAVIOR:-}" = "sleep" ]; then
            sleep 30
            exit 0
        fi

        if [ "${SESSION_TEST_BEHAVIOR:-}" = "sleep-short" ]; then
            sleep 5
            exit 0
        fi

        echo "unknown SESSION_TEST_BEHAVIOR=${SESSION_TEST_BEHAVIOR:-}" >&2
        exit 99
        ;;
    *)
        echo "unexpected site-builder subcommand: $command_name" >&2
        exit 98
        ;;
esac
"#;
