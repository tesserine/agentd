use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn write_temp_config(name: &str, contents: &str) -> PathBuf {
    let unique = format!(
        "agentd-cli-test-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(unique);

    std::fs::create_dir_all(&dir).expect("temp test directory should be created");

    let path = dir.join("agentd.toml");
    std::fs::write(&path, contents).expect("temp config should be written");
    path
}

fn daemon_test_config(socket_path: &Path, pid_file: &Path) -> String {
    format!(
        r#"
[daemon]
socket_path = "{socket_path}"
pid_file = "{pid_file}"

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
        socket_path = socket_path.display(),
        pid_file = pid_file.display()
    )
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }

    panic!("timed out waiting for {}", path.display());
}

fn terminate(child: &mut Child) -> io::Result<()> {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()?;

    assert!(status.success(), "kill should succeed");
    Ok(())
}

#[test]
fn binary_without_subcommand_starts_daemon_mode() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "daemon-default-command",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
        ])
        .env("AGENTD_LOG_FORMAT", "text")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("agentd binary should spawn");

    wait_for_path(&socket_path);
    wait_for_path(&pid_file);
    terminate(&mut child).expect("daemon should accept SIGTERM");
    let output = child
        .wait_with_output()
        .expect("daemon output should be available");

    assert!(
        output.status.success(),
        "daemon should exit cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("\"event\":\"agentd.logging_format_invalid\""),
        "expected tracing bootstrap warning, got: {stderr}"
    );
}

#[test]
fn binary_run_command_reports_clear_error_when_daemon_is_not_running() {
    let runtime_dir = std::env::temp_dir().join(format!(
        "agentd-cli-runtime-not-running-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir should be created");
    let socket_path = runtime_dir.join("agentd.sock");
    let pid_file = runtime_dir.join("agentd.pid");
    let config_path = write_temp_config(
        "client-command",
        &daemon_test_config(&socket_path, &pid_file),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_agentd"))
        .args([
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "run",
            "codex",
            "https://example.com/repo.git",
        ])
        .output()
        .expect("agentd binary should run");

    assert!(
        !output.status.success(),
        "run command should fail without daemon"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be valid UTF-8");
    assert!(
        stderr.contains("agentd is not running"),
        "expected daemon-not-running error, got: {stderr}"
    );
}
