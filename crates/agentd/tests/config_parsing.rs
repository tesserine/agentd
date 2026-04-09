use std::path::{Path, PathBuf};
use std::str::FromStr;

use agentd::config::{Config, ConfigError, DaemonConfig};

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

fn assert_invalid_agent_name_parse_error(name: &str) {
    let error = Config::from_str(&format!(
        r#"
[[agents]]
name = "{name}"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#
    ))
    .expect_err("invalid agent names should be rejected at parse time");

    match error {
        ConfigError::InvalidAgentName { name: invalid_name } => assert_eq!(invalid_name, name),
        other => panic!("expected invalid agent name error, got {other}"),
    }
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

fn write_temp_config_under(base_dir: &Path, name: &str, contents: &str) -> PathBuf {
    let unique = format!(
        "agentd-config-test-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos()
    );
    let dir = base_dir.join(unique);

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
    assert_eq!(
        config.daemon().socket_path(),
        Path::new("/run/agentd/agentd.sock")
    );
    assert_eq!(
        config.daemon().pid_file(),
        Path::new("/run/agentd/agentd.pid")
    );
    assert_eq!(agent.name(), "codex");
    assert_eq!(agent.base_image(), "ghcr.io/example/codex:latest");
    assert_eq!(agent.methodology_dir(), Path::new("../groundwork"));
    assert_eq!(agent.repo_token_source(), Some("CODEX_REPO_TOKEN"));
    assert_eq!(
        agent.runa().command(),
        &["codex".to_string(), "exec".to_string()]
    );
    assert_eq!(agent.credentials().len(), 1);
    assert_eq!(agent.credentials()[0].name(), "GITHUB_TOKEN");
    assert_eq!(agent.credentials()[0].source(), "AGENTD_GITHUB_TOKEN");
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
fn loading_config_from_a_relative_path_resolves_methodology_dir_from_an_absolute_base_dir() {
    let current_dir = std::env::current_dir().expect("current directory should be available");
    let path = write_temp_config_under(
        &current_dir.join("target"),
        "relative-load-path",
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    );
    let relative_path = path
        .strip_prefix(&current_dir)
        .expect("fixture path should be under the current directory");

    let config = Config::load(relative_path).expect("config file should parse");
    let agent = config.agent("codex").expect("agent should exist");

    assert_eq!(
        agent.methodology_dir(),
        path.parent()
            .expect("config file should have a parent directory")
            .join("../groundwork")
    );
    assert!(
        agent.methodology_dir().is_absolute(),
        "loaded methodology_dir should be absolute when loaded from a file"
    );
}

#[test]
fn loading_config_resolves_relative_daemon_paths_from_file_location() {
    let path = write_temp_config(
        "relative-daemon-paths",
        r#"
[daemon]
socket_path = "runtime/agentd.sock"
pid_file = "runtime/agentd.pid"

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    );

    let config = Config::load(&path).expect("config file should parse");
    let base_dir = path
        .parent()
        .expect("config file should have a parent directory");

    assert_eq!(
        config.daemon().socket_path(),
        base_dir.join("runtime/agentd.sock")
    );
    assert_eq!(
        config.daemon().pid_file(),
        base_dir.join("runtime/agentd.pid")
    );
}

#[test]
fn loading_config_from_a_relative_path_resolves_relative_daemon_paths_from_an_absolute_base_dir() {
    let current_dir = std::env::current_dir().expect("current directory should be available");
    let path = write_temp_config_under(
        &current_dir.join("target"),
        "relative-daemon-load-path",
        r#"
[daemon]
socket_path = "runtime/agentd.sock"
pid_file = "runtime/agentd.pid"

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    );
    let relative_path = path
        .strip_prefix(&current_dir)
        .expect("fixture path should be under the current directory");

    let config = Config::load(relative_path).expect("config file should parse");
    let base_dir = path
        .parent()
        .expect("config file should have a parent directory");

    assert_eq!(
        config.daemon().socket_path(),
        base_dir.join("runtime/agentd.sock")
    );
    assert_eq!(
        config.daemon().pid_file(),
        base_dir.join("runtime/agentd.pid")
    );
    assert!(
        config.daemon().socket_path().is_absolute(),
        "loaded socket_path should be absolute when loaded from a file"
    );
    assert!(
        config.daemon().pid_file().is_absolute(),
        "loaded pid_file should be absolute when loaded from a file"
    );
}

#[test]
fn daemon_instance_id_is_stable_for_the_same_runtime_paths() {
    let config = Config::from_str(
        r#"
[daemon]
socket_path = "/run/agentd/a.sock"
pid_file = "/run/agentd/a.pid"

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect("config should parse");

    let first = config
        .daemon()
        .daemon_instance_id()
        .expect("daemon instance id should resolve");
    let second = config
        .daemon()
        .daemon_instance_id()
        .expect("daemon instance id should resolve");

    assert_eq!(first, second);
    assert_eq!(first.len(), 8);
    assert!(
        first
            .chars()
            .all(|character| matches!(character, '0'..='9' | 'a'..='f')),
        "daemon instance id should be lowercase hex: {first}"
    );
}

#[test]
fn daemon_instance_id_changes_when_daemon_runtime_paths_change() {
    let first = Config::from_str(
        r#"
[daemon]
socket_path = "/run/agentd/a.sock"
pid_file = "/run/agentd/a.pid"

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect("first config should parse");
    let second = Config::from_str(
        r#"
[daemon]
socket_path = "/run/agentd/b.sock"
pid_file = "/run/agentd/a.pid"

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect("second config should parse");

    assert_ne!(
        first
            .daemon()
            .daemon_instance_id()
            .expect("first daemon instance id should resolve"),
        second
            .daemon()
            .daemon_instance_id()
            .expect("second daemon instance id should resolve")
    );
}

#[test]
fn daemon_instance_id_rejects_relative_daemon_runtime_paths_from_str_configs() {
    let config = Config::from_str(
        r#"
[daemon]
socket_path = "runtime/agentd.sock"
pid_file = "runtime/agentd.pid"

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect("config should parse");

    let error = config
        .daemon()
        .daemon_instance_id()
        .expect_err("relative daemon runtime paths should be rejected");

    match error {
        ConfigError::RelativeDaemonRuntimePath { field, path } => {
            assert_eq!(field, "daemon.socket_path");
            assert_eq!(path, Path::new("runtime/agentd.sock"));
        }
        other => panic!("expected relative daemon runtime path error, got {other}"),
    }
}

#[test]
fn parses_default_daemon_paths_when_daemon_section_is_omitted() {
    let config = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect("config should parse with daemon defaults");

    assert_eq!(
        config.daemon().socket_path(),
        Path::new("/run/agentd/agentd.sock")
    );
    assert_eq!(
        config.daemon().pid_file(),
        Path::new("/run/agentd/agentd.pid")
    );
}

#[test]
fn daemon_instance_id_rejects_relative_pid_file_after_absolute_socket_path() {
    let config = Config::from_str(
        r#"
[daemon]
socket_path = "/run/agentd/agentd.sock"
pid_file = "runtime/agentd.pid"

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect("config should parse");

    let error = config
        .daemon()
        .daemon_instance_id()
        .expect_err("relative pid file should be rejected");

    match error {
        ConfigError::RelativeDaemonRuntimePath { field, path } => {
            assert_eq!(field, "daemon.pid_file");
            assert_eq!(path, Path::new("runtime/agentd.pid"));
        }
        other => panic!("expected relative daemon runtime path error, got {other}"),
    }
}

#[test]
fn parses_explicit_daemon_paths() {
    let config = Config::from_str(
        r#"
[daemon]
socket_path = "/tmp/agentd-test.sock"
pid_file = "/tmp/agentd-test.pid"

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect("config should parse explicit daemon paths");

    assert_eq!(
        config.daemon().socket_path(),
        Path::new("/tmp/agentd-test.sock")
    );
    assert_eq!(
        config.daemon().pid_file(),
        Path::new("/tmp/agentd-test.pid")
    );
}

#[test]
fn parses_repo_token_source_as_optional_clone_auth_lookup_key() {
    let config = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"
repo_token_source = "CODEX_REPO_TOKEN"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect("config should parse repo token source");

    let agent = config.agent("codex").expect("agent should exist");

    assert_eq!(agent.repo_token_source(), Some("CODEX_REPO_TOKEN"));
}

#[test]
fn normalizes_empty_repo_token_source_to_none() {
    let config = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"
repo_token_source = ""

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect("empty repo token source should disable clone auth");

    let agent = config.agent("codex").expect("agent should exist");

    assert_eq!(agent.repo_token_source(), None);
}

#[test]
fn rejects_repo_token_source_with_outer_whitespace() {
    for repo_token_source in [" CODEX_REPO_TOKEN", "CODEX_REPO_TOKEN "] {
        let error = Config::from_str(&format!(
            r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"
repo_token_source = "{repo_token_source}"

[agents.runa]
command = ["codex", "exec"]
"#
        ))
        .expect_err("whitespace-padded repo token sources should be rejected");

        match error {
            ConfigError::FieldHasOuterWhitespace {
                field,
                agent,
                credential,
            } => {
                assert_eq!(field, "repo_token_source");
                assert_eq!(agent.as_deref(), Some("codex"));
                assert_eq!(credential, None);
            }
            other => panic!("expected whitespace validation error, got {other}"),
        }
    }
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
fn rejects_unknown_fields_in_daemon_config() {
    let error = Config::from_str(
        r#"
[daemon]
socket_path = "/tmp/agentd.sock"
extra = "nope"

[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    )
    .expect_err("unknown daemon fields should be rejected");

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
source = "AGENTD_GITHUB_TOKEN"

[[agents.credentials]]
name = "GITHUB_TOKEN"
source = "AGENTD_GITHUB_OTHER_TOKEN"
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
fn rejects_agent_names_with_leading_or_trailing_whitespace() {
    for name in [" codex", "codex "] {
        let error = Config::from_str(&format!(
            r#"
[[agents]]
name = "{name}"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#
        ))
        .expect_err("whitespace-padded agent names should be rejected");

        match error {
            ConfigError::FieldHasOuterWhitespace {
                field,
                agent,
                credential,
            } => {
                assert_eq!(field, "name");
                assert_eq!(agent, None);
                assert_eq!(credential, None);
            }
            other => panic!("expected whitespace validation error, got {other}"),
        }
    }
}

#[test]
fn rejects_uppercase_agent_names_at_parse_time() {
    assert_invalid_agent_name_parse_error("Codex");
}

#[test]
fn rejects_digit_prefixed_agent_names_at_parse_time() {
    assert_invalid_agent_name_parse_error("123agent");
}

#[test]
fn rejects_reserved_agent_names_at_parse_time() {
    assert_invalid_agent_name_parse_error("root");
}

#[test]
fn rejects_agent_names_longer_than_thirty_two_characters_at_parse_time() {
    assert_invalid_agent_name_parse_error(&format!("a{}", "b".repeat(32)));
}

#[test]
fn rejects_credential_names_with_leading_or_trailing_whitespace() {
    for name in [" GITHUB_TOKEN", "GITHUB_TOKEN "] {
        let error = Config::from_str(&format!(
            r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]

[[agents.credentials]]
name = "{name}"
source = "AGENTD_GITHUB_TOKEN"
"#
        ))
        .expect_err("whitespace-padded credential names should be rejected");

        match error {
            ConfigError::FieldHasOuterWhitespace {
                field,
                agent,
                credential,
            } => {
                assert_eq!(field, "credentials.name");
                assert_eq!(agent.as_deref(), Some("codex"));
                assert_eq!(credential, None);
            }
            other => panic!("expected whitespace validation error, got {other}"),
        }
    }
}

#[test]
fn rejects_credential_names_containing_commas() {
    let error = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]

[[agents.credentials]]
name = "TOKEN,EXTRA"
source = "AGENTD_GITHUB_TOKEN"
"#,
    )
    .expect_err("comma-delimited credential names should be rejected");

    match error {
        ConfigError::InvalidCredentialName { agent, name } => {
            assert_eq!(agent, "codex");
            assert_eq!(name, "TOKEN,EXTRA");
        }
        other => panic!("expected invalid credential name error, got {other}"),
    }
}

#[test]
fn rejects_credential_names_containing_equals_signs() {
    let error = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]

[[agents.credentials]]
name = "FOO=BAR"
source = "AGENTD_GITHUB_TOKEN"
"#,
    )
    .expect_err("credential names containing '=' should be rejected");

    match error {
        ConfigError::InvalidCredentialName { agent, name } => {
            assert_eq!(agent, "codex");
            assert_eq!(name, "FOO=BAR");
        }
        other => panic!("expected invalid credential name error, got {other}"),
    }
}

#[test]
fn rejects_reserved_credential_names() {
    let error = Config::from_str(
        r#"
[[agents]]
name = "codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]

[[agents.credentials]]
name = "AGENT_NAME"
source = "AGENTD_GITHUB_TOKEN"
"#,
    )
    .expect_err("runner-reserved credential names should be rejected");

    match error {
        ConfigError::InvalidCredentialName { agent, name } => {
            assert_eq!(agent, "codex");
            assert_eq!(name, "AGENT_NAME");
        }
        other => panic!("expected invalid credential name error, got {other}"),
    }
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

#[test]
fn loading_daemon_config_resolves_relative_paths_from_file_location() {
    let path = write_temp_config(
        "daemon-only-relative-paths",
        r#"
[daemon]
socket_path = "runtime/agentd.sock"
pid_file = "runtime/agentd.pid"

[[agents]]
name = "Codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    );

    let config = DaemonConfig::load(&path).expect("daemon config should parse");
    let base_dir = path
        .parent()
        .expect("config file should have a parent directory");

    assert_eq!(config.socket_path(), base_dir.join("runtime/agentd.sock"));
    assert_eq!(config.pid_file(), base_dir.join("runtime/agentd.pid"));
}

#[test]
fn loading_daemon_config_uses_defaults_when_daemon_section_is_omitted() {
    let path = write_temp_config(
        "daemon-only-defaults",
        r#"
[[agents]]
name = "Codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    );

    let config = DaemonConfig::load(&path).expect("daemon config should parse");

    assert_eq!(config.socket_path(), Path::new("/run/agentd/agentd.sock"));
    assert_eq!(config.pid_file(), Path::new("/run/agentd/agentd.pid"));
}

#[test]
fn loading_daemon_config_rejects_unknown_fields_in_daemon_section() {
    let path = write_temp_config(
        "daemon-only-unknown-field",
        r#"
[daemon]
socket_path = "/tmp/agentd.sock"
extra = "nope"

[[agents]]
name = "Codex"
base_image = "ghcr.io/example/codex:latest"
methodology_dir = "../groundwork"

[agents.runa]
command = ["codex", "exec"]
"#,
    );

    let error = DaemonConfig::load(&path).expect_err("unknown daemon fields should be rejected");

    assert!(error.to_string().contains("unknown field"));
    assert!(error.to_string().contains("extra"));
}

#[test]
fn loading_daemon_config_rejects_unknown_top_level_sections() {
    let path = write_temp_config(
        "daemon-only-unknown-top-level",
        r#"
[deamon]
socket_path = "/tmp/agentd.sock"

[[agents]]
unexpected = "still allowed to exist here"
"#,
    );

    let error =
        DaemonConfig::load(&path).expect_err("unknown top-level sections should be rejected");

    assert!(error.to_string().contains("unknown field"));
    assert!(error.to_string().contains("deamon"));
}

#[test]
fn loading_daemon_config_ignores_invalid_agent_registry_entries() {
    let path = write_temp_config(
        "daemon-only-ignores-agents",
        r#"
[daemon]
socket_path = "runtime/agentd.sock"
pid_file = "runtime/agentd.pid"

[[agents]]
unexpected = "daemon loader should ignore agent schema entirely"
"#,
    );

    let config = DaemonConfig::load(&path).expect("daemon config should parse");
    let base_dir = path
        .parent()
        .expect("config file should have a parent directory");

    assert_eq!(config.socket_path(), base_dir.join("runtime/agentd.sock"));
    assert_eq!(config.pid_file(), base_dir.join("runtime/agentd.pid"));
}
