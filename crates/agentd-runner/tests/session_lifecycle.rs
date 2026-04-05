use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use agentd_runner::{
    ResolvedEnvironmentVariable, SessionInvocation, SessionOutcome, SessionSpec, run_session,
};

#[test]
fn succeeds_without_timeout_and_cleans_up_container() {
    if skip_if_podman_unavailable("succeeds_without_timeout_and_cleans_up_container") {
        return;
    }

    let fixture = SessionFixture::new("success-agent");
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            agent_name: "success-agent".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            agent_command: vec![
                "codex".to_string(),
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
                    name: "RUNA_TEST_BEHAVIOR".to_string(),
                    value: "success".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: "/srv/test-repo.git".to_string(),
            work_unit: Some("task-42".to_string()),
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Succeeded);
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

    let fixture = SessionFixture::new("failure-agent");
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            agent_name: "failure-agent".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![
                ResolvedEnvironmentVariable {
                    name: "GITHUB_TOKEN".to_string(),
                    value: "test-token".to_string(),
                },
                ResolvedEnvironmentVariable {
                    name: "RUNA_TEST_BEHAVIOR".to_string(),
                    value: "fail".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: "/srv/test-repo.git".to_string(),
            work_unit: None,
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Failed { exit_code: 23 });
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

    let fixture = SessionFixture::new("failure-agent-125");
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            agent_name: "failure-agent-125".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![
                ResolvedEnvironmentVariable {
                    name: "GITHUB_TOKEN".to_string(),
                    value: "test-token".to_string(),
                },
                ResolvedEnvironmentVariable {
                    name: "RUNA_TEST_BEHAVIOR".to_string(),
                    value: "fail-125".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: "/srv/test-repo.git".to_string(),
            work_unit: None,
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Failed { exit_code: 125 });
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn succeeds_when_methodology_dir_path_contains_commas() {
    if skip_if_podman_unavailable("succeeds_when_methodology_dir_path_contains_commas") {
        return;
    }

    let fixture = SessionFixture::new_with_root_prefix(
        "comma-methodology-agent",
        "agentd-runner,comma,methodology",
    );
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            agent_name: "comma-methodology-agent".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![
                ResolvedEnvironmentVariable {
                    name: "GITHUB_TOKEN".to_string(),
                    value: "test-token".to_string(),
                },
                ResolvedEnvironmentVariable {
                    name: "RUNA_TEST_BEHAVIOR".to_string(),
                    value: "success".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: "/srv/test-repo.git".to_string(),
            work_unit: Some("task-42".to_string()),
            timeout: None,
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::Succeeded);
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

#[test]
fn times_out_when_a_timeout_is_provided_and_cleans_up_container() {
    if skip_if_podman_unavailable("times_out_when_a_timeout_is_provided_and_cleans_up_container") {
        return;
    }

    let fixture = SessionFixture::new("timeout-agent");
    let image = fixture.build_image();

    let outcome = run_session(
        SessionSpec {
            agent_name: "timeout-agent".to_string(),
            base_image: image,
            methodology_dir: fixture.methodology_dir(),
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![
                ResolvedEnvironmentVariable {
                    name: "GITHUB_TOKEN".to_string(),
                    value: "test-token".to_string(),
                },
                ResolvedEnvironmentVariable {
                    name: "RUNA_TEST_BEHAVIOR".to_string(),
                    value: "sleep".to_string(),
                },
            ],
        },
        SessionInvocation {
            repo_url: "/srv/test-repo.git".to_string(),
            work_unit: None,
            timeout: Some(Duration::from_secs(1)),
        },
    )
    .expect("session should run");

    assert_eq!(outcome, SessionOutcome::TimedOut);
    fixture.assert_no_runner_container_left_behind();
    fixture.assert_no_runner_secret_left_behind();
}

struct SessionFixture {
    root: PathBuf,
    agent_name: String,
}

impl SessionFixture {
    fn new(agent_name: &str) -> Self {
        Self::new_with_root_prefix(agent_name, &format!("agentd-runner-{agent_name}"))
    }

    fn new_with_root_prefix(agent_name: &str, root_prefix: &str) -> Self {
        let root = unique_temp_dir(root_prefix);
        fs::create_dir_all(&root).expect("fixture root should be created");

        let methodology_dir = root.join("methodology");
        fs::create_dir_all(&methodology_dir).expect("methodology directory should be created");
        fs::write(
            methodology_dir.join("manifest.toml"),
            "name = \"test-methodology\"\n",
        )
        .expect("methodology manifest should be written");

        Self {
            root,
            agent_name: agent_name.to_string(),
        }
    }

    fn methodology_dir(&self) -> PathBuf {
        self.root.join("methodology")
    }

    fn build_image(&self) -> String {
        let context_dir = self.root.join("image-context");
        let bare_repo_dir = context_dir.join("repo.git");
        fs::create_dir_all(&context_dir).expect("image context should be created");

        write_test_repo(&bare_repo_dir);
        fs::write(context_dir.join("runa"), RUNA_STUB).expect("runa stub should be written");
        fs::write(context_dir.join("Containerfile"), CONTAINERFILE)
            .expect("containerfile should be written");

        let tag = format!("agentd-runner-test:{}", self.agent_name);
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
        let expected_prefix = format!("agentd-{}-", self.agent_name);
        assert!(
            !names.lines().any(|name| name.starts_with(&expected_prefix)),
            "runner left container behind with prefix {expected_prefix}: {names}"
        );
    }

    fn assert_no_runner_secret_left_behind(&self) {
        let output = Command::new("podman")
            .args(["secret", "ls", "--format", "{{.Name}}"])
            .output()
            .expect("podman secret ls should run");
        assert!(
            output.status.success(),
            "podman secret ls failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let names =
            String::from_utf8(output.stdout).expect("podman secret ls output should be utf-8");
        let expected_fragment = format!("agentd-{}-", self.agent_name);
        assert!(
            !names.lines().any(|name| name.contains(&expected_fragment)),
            "runner left secrets behind for {expected_fragment}: {names}"
        );
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
FROM docker.io/library/alpine:3.20

RUN apk add --no-cache git
COPY runa /usr/local/bin/runa
RUN chmod +x /usr/local/bin/runa
COPY repo.git /srv/test-repo.git
"#;

const RUNA_STUB: &str = r#"#!/bin/sh
set -eu

command_name="$1"
shift

case "$command_name" in
    init)
        [ "$1" = "--methodology" ]
        [ -f "$2" ]
        mkdir -p .runa
        cat > .runa/config.toml <<'EOF'
[project]
name = "test-project"
EOF
        ;;
    run)
        [ -f /agentd/methodology/manifest.toml ]
        [ -f README.md ]
        [ "${AGENT_NAME:-}" != "" ]
        [ "${GITHUB_TOKEN:-}" = "${GITHUB_TOKEN:-test-token}" ]
        grep -F 'command = ["codex", "exec"' .runa/config.toml >/dev/null

        if [ "${RUNA_TEST_BEHAVIOR:-success}" = "success" ]; then
            [ "$1" = "--work-unit" ]
            [ "$2" = "task-42" ]
            exit 0
        fi

        if [ "${RUNA_TEST_BEHAVIOR:-}" = "fail" ]; then
            [ "$#" = "0" ]
            exit 23
        fi

        if [ "${RUNA_TEST_BEHAVIOR:-}" = "fail-125" ]; then
            [ "$#" = "0" ]
            exit 125
        fi

        if [ "${RUNA_TEST_BEHAVIOR:-}" = "sleep" ]; then
            sleep 30
            exit 0
        fi

        echo "unknown RUNA_TEST_BEHAVIOR=${RUNA_TEST_BEHAVIOR:-}" >&2
        exit 99
        ;;
    *)
        echo "unexpected runa subcommand: $command_name" >&2
        exit 98
        ;;
esac
"#;
