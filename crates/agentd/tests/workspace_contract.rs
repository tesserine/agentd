use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn read_workspace_file(path: &str) -> String {
    std::fs::read_to_string(workspace_root().join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

#[test]
fn workspace_metadata_lists_only_grounded_crates() {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata should run");

    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("cargo metadata stdout should be utf-8");

    assert!(stdout.contains("\"name\":\"agentd\""));
    assert!(stdout.contains("\"name\":\"agentd-runner\""));
    assert!(stdout.contains("\"name\":\"agentd-scheduler\""));
    assert!(
        !stdout.contains("\"name\":\"mcp-transport\""),
        "workspace metadata still includes mcp-transport"
    );
    assert!(
        !stdout.contains("\"name\":\"forgejo-mcp\""),
        "workspace metadata still includes forgejo-mcp"
    );
}

#[test]
fn removed_crate_directories_are_absent() {
    let workspace_root = workspace_root();

    assert!(
        !workspace_root.join("crates/mcp-transport").exists(),
        "crates/mcp-transport still exists"
    );
    assert!(
        !workspace_root.join("crates/forgejo-mcp").exists(),
        "crates/forgejo-mcp still exists"
    );
}

#[test]
fn workspace_docs_describe_only_the_three_grounded_crates() {
    let readme = read_workspace_file("README.md");
    let architecture = read_workspace_file("ARCHITECTURE.md");
    let agents = read_workspace_file("AGENTS.md");

    for document in [&readme, &architecture, &agents] {
        assert!(
            !document.contains("mcp-transport"),
            "documentation still references mcp-transport"
        );
        assert!(
            !document.contains("forgejo-mcp"),
            "documentation still references forgejo-mcp"
        );
    }

    assert!(architecture.contains("`agentd`"));
    assert!(architecture.contains("`agentd-runner`"));
    assert!(architecture.contains("`agentd-scheduler`"));
}

#[test]
fn architecture_describes_uniform_socket_intake_for_session_triggers() {
    let architecture = read_workspace_file("ARCHITECTURE.md");

    assert!(
        architecture.contains("single intake for all session triggers"),
        "architecture should describe the Unix socket as the single session intake"
    );
    assert!(
        architecture.contains("scheduler is a socket client"),
        "architecture should describe the scheduler as a socket client"
    );
    assert!(
        !architecture.contains("The scheduler passes agent identity plus mission context to the runner."),
        "architecture should not describe the scheduler as handing work directly to the runner"
    );
}
