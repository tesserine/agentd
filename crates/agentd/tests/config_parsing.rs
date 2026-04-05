use std::path::{Path, PathBuf};
use std::str::FromStr;

use agentd::config::{Config, ConfigError};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn example_config() -> String {
    std::fs::read_to_string(workspace_root().join("examples/agentd.toml"))
        .expect("example config should be readable")
}

fn write_temp_config(name: &str, contents: &str) -> PathBuf {
    let unique = format!(
        "agentd-config-test-{name}-{}-{}",
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

#[test]
fn parses_example_config_into_static_agent_settings() {
    let config = Config::from_str(&example_config()).expect("example config should parse");
    let agent = config.agent("codex").expect("example agent should exist");

    assert_eq!(config.agents().len(), 1);
    assert_eq!(agent.name(), "codex");
    assert_eq!(agent.base_image(), "ghcr.io/example/codex:latest");
    assert_eq!(agent.methodology_dir(), Path::new("../groundwork"));
    assert_eq!(
        agent.runa().command(),
        &["codex".to_string(), "exec".to_string()]
    );
    assert_eq!(agent.credentials().len(), 1);
    assert_eq!(agent.credentials()[0].name(), "GITHUB_TOKEN");
    assert_eq!(agent.credentials()[0].source(), "op://agentd/github/token");
}

#[test]
fn loading_config_resolves_relative_methodology_path_from_file_location() {
    let path = write_temp_config(
        "relative-path",
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    );

    let config = Config::load(&path).expect("config file should parse");
    let agent = config.agent("codex").expect("agent should exist");

    assert_eq!(
        agent.methodology_dir(),
        path.parent()
            .expect("config file should have a parent directory")
            .join("../groundwork")
    );
}

#[test]
fn rejects_unknown_fields_in_agent_config() {
    let error = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"
extra = "nope"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect_err("unknown fields should be rejected");

    assert!(error.to_string().contains("unknown field"));
    assert!(error.to_string().contains("extra"));
}

#[test]
fn rejects_duplicate_agent_names() {
    let error = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:stable"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect_err("duplicate agent names should be rejected");

    match error {
        ConfigError::DuplicateAgentName { name } => assert_eq!(name, "codex"),
        other => panic!("expected duplicate agent name error, got {other}"),
    }
}

#[test]
fn rejects_configs_without_agents() {
    let error = Config::from_str("").expect_err("configs must define at least one agent");

    assert!(error.to_string().contains("at least one agent"));
}

#[test]
fn rejects_duplicate_credential_names_within_an_agent() {
    let error = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]

[[agents.credentials]]
name = "GITHUB_TOKEN"
source = "op://agentd/github/token"

[[agents.credentials]]
name = "GITHUB_TOKEN"
source = "op://agentd/github/other-token"
"#,
    )
    .expect_err("duplicate credential names should be rejected");

    match error {
        ConfigError::DuplicateCredentialName { agent, name } => {
            assert_eq!(agent, "codex");
            assert_eq!(name, "GITHUB_TOKEN");
        }
        other => panic!("expected duplicate credential name error, got {other}"),
    }
}

#[test]
fn rejects_empty_required_string_fields() {
    let error = Config::from_str(
        r#"
[[agents]]
name = ""
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect_err("empty names should be rejected");

    assert!(error.to_string().contains("name"));
}

#[test]
fn rejects_empty_command_arrays_and_empty_command_elements() {
    let empty_command_error = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = []
"#,
    )
    .expect_err("empty command arrays should be rejected");

    let empty_element_error = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", ""]
"#,
    )
    .expect_err("empty command elements should be rejected");

    assert!(empty_command_error.to_string().contains("command"));
    assert!(empty_element_error.to_string().contains("command"));
}

#[test]
fn rejects_missing_runa_table() {
    let error = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"
"#,
    )
    .expect_err("missing runa table should be rejected");

    assert!(error.to_string().contains("runa"));
}

#[test]
fn reports_io_errors_when_loading_missing_files() {
    let missing_path = std::env::temp_dir().join(format!(
        "agentd-missing-config-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    ));

    let error = Config::load(&missing_path).expect_err("missing config file should fail");

    match error {
        ConfigError::Io(io_error) => assert_eq!(io_error.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected io error, got {other}"),
    }
}
