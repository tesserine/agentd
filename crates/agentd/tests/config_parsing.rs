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

fn assert_invalid_profile_name_parse_error(name: &str) {
    let error = Config::from_str(&format!(
        r#"
[[profiles]]
name = "{name}"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
"#
    ))
    .expect_err("invalid profile names should be rejected at parse time");

    match error {
        ConfigError::InvalidProfileName { name: invalid_name } => assert_eq!(invalid_name, name),
        other => panic!("expected invalid profile name error, got {other}"),
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
fn parses_example_config_into_static_profile_settings() {
    let config = Config::from_str(&example_config()).expect("example config should parse");
    let site_builder = config
        .profile("site-builder")
        .expect("site-builder profile should exist");
    let code_reviewer = config
        .profile("code-reviewer")
        .expect("code-reviewer profile should exist");

    assert_eq!(config.profiles().len(), 2);
    assert_eq!(
        config.daemon().socket_path(),
        Path::new("/run/agentd/agentd.sock")
    );
    assert_eq!(
        config.daemon().pid_file(),
        Path::new("/run/agentd/agentd.pid")
    );
    assert_eq!(site_builder.name(), "site-builder");
    assert_eq!(
        site_builder.base_image(),
        "ghcr.io/example/site-builder:latest"
    );
    assert_eq!(site_builder.methodology_dir(), Path::new("../groundwork"));
    assert_eq!(
        site_builder.repo_token_source(),
        Some("SITE_BUILDER_REPO_TOKEN")
    );
    assert_eq!(site_builder.command()[0], "/bin/sh");
    assert_eq!(site_builder.command()[1], "-lc");
    assert!(site_builder.command()[2].contains("runa init --methodology"));
    assert!(site_builder.command()[2].contains("/agentd/methodology/manifest.toml"));
    assert!(site_builder.command()[2].contains("command = [\"site-builder\", \"exec\"]"));
    assert!(site_builder.command()[2].contains("AGENTD_WORK_UNIT"));
    assert_eq!(site_builder.credentials().len(), 1);
    assert_eq!(site_builder.credentials()[0].name(), "GITHUB_TOKEN");
    assert_eq!(
        site_builder.credentials()[0].source(),
        "AGENTD_GITHUB_TOKEN"
    );

    assert_eq!(code_reviewer.name(), "code-reviewer");
    assert_eq!(
        code_reviewer.base_image(),
        "ghcr.io/example/code-reviewer:latest"
    );
    assert_eq!(code_reviewer.methodology_dir(), Path::new("../groundwork"));
    assert_eq!(
        code_reviewer.repo_token_source(),
        Some("CODE_REVIEWER_REPO_TOKEN")
    );
    assert_eq!(code_reviewer.command()[0], "/bin/sh");
    assert_eq!(code_reviewer.command()[1], "-lc");
    assert!(code_reviewer.command()[2].contains("runa init --methodology"));
    assert!(code_reviewer.command()[2].contains("/agentd/methodology/manifest.toml"));
    assert!(code_reviewer.command()[2].contains("command = [\"code-reviewer\", \"exec\"]"));
    assert!(code_reviewer.command()[2].contains("AGENTD_WORK_UNIT"));
    assert_eq!(code_reviewer.credentials().len(), 1);
    assert_eq!(code_reviewer.credentials()[0].name(), "GITHUB_TOKEN");
    assert_eq!(
        code_reviewer.credentials()[0].source(),
        "AGENTD_GITHUB_TOKEN"
    );
}

#[test]
fn loading_config_resolves_relative_methodology_path_from_file_location() {
    let path = write_temp_config(
        "relative-path",
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
"#,
    );

    let config = Config::load(&path).expect("config file should parse");
    let profile = config
        .profile("site-builder")
        .expect("profile should exist");

    assert_eq!(
        profile.methodology_dir(),
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
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
"#,
    );
    let relative_path = path
        .strip_prefix(&current_dir)
        .expect("fixture path should be under the current directory");

    let config = Config::load(relative_path).expect("config file should parse");
    let profile = config
        .profile("site-builder")
        .expect("profile should exist");

    assert_eq!(
        profile.methodology_dir(),
        path.parent()
            .expect("config file should have a parent directory")
            .join("../groundwork")
    );
    assert!(
        profile.methodology_dir().is_absolute(),
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

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
"#,
    )
    .expect("first config should parse");
    let second = Config::from_str(
        r#"
[daemon]
socket_path = "/run/agentd/b.sock"
pid_file = "/run/agentd/a.pid"

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"
repo_token_source = "SITE_BUILDER_REPO_TOKEN"

command = ["site-builder", "exec"]
"#,
    )
    .expect("config should parse repo token source");

    let profile = config
        .profile("site-builder")
        .expect("profile should exist");

    assert_eq!(profile.repo_token_source(), Some("SITE_BUILDER_REPO_TOKEN"));
}

#[test]
fn normalizes_empty_repo_token_source_to_none() {
    let config = Config::from_str(
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"
repo_token_source = ""

command = ["site-builder", "exec"]
"#,
    )
    .expect("empty repo token source should disable clone auth");

    let profile = config
        .profile("site-builder")
        .expect("profile should exist");

    assert_eq!(profile.repo_token_source(), None);
}

#[test]
fn rejects_repo_token_source_with_outer_whitespace() {
    for repo_token_source in [" SITE_BUILDER_REPO_TOKEN", "SITE_BUILDER_REPO_TOKEN "] {
        let error = Config::from_str(&format!(
            r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"
repo_token_source = "{repo_token_source}"

command = ["site-builder", "exec"]
"#
        ))
        .expect_err("whitespace-padded repo token sources should be rejected");

        match error {
            ConfigError::FieldHasOuterWhitespace {
                field,
                profile,
                credential,
            } => {
                assert_eq!(field, "repo_token_source");
                assert_eq!(profile.as_deref(), Some("site-builder"));
                assert_eq!(credential, None);
            }
            other => panic!("expected whitespace validation error, got {other}"),
        }
    }
}

#[test]
fn rejects_unknown_fields_in_profile_config() {
    let error = Config::from_str(
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"
extra = "nope"

command = ["site-builder", "exec"]
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

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
"#,
    )
    .expect_err("unknown daemon fields should be rejected");

    assert!(error.to_string().contains("unknown field"));
    assert!(error.to_string().contains("extra"));
}

#[test]
fn rejects_duplicate_profile_names() {
    let error = Config::from_str(
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]

[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:stable"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
"#,
    )
    .expect_err("duplicate profile names should be rejected");

    match error {
        ConfigError::DuplicateProfileName { name } => assert_eq!(name, "site-builder"),
        other => panic!("expected duplicate profile name error, got {other}"),
    }
}

#[test]
fn rejects_configs_without_profiles() {
    let error = Config::from_str("").expect_err("configs must define at least one profile");

    assert!(error.to_string().contains("at least one profile"));
}

#[test]
fn rejects_duplicate_credential_names_within_a_profile() {
    let error = Config::from_str(
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]

[[profiles.credentials]]
name = "GITHUB_TOKEN"
source = "AGENTD_GITHUB_TOKEN"

[[profiles.credentials]]
name = "GITHUB_TOKEN"
source = "AGENTD_GITHUB_OTHER_TOKEN"
"#,
    )
    .expect_err("duplicate credential names should be rejected");

    match error {
        ConfigError::DuplicateCredentialName { profile, name } => {
            assert_eq!(profile, "site-builder");
            assert_eq!(name, "GITHUB_TOKEN");
        }
        other => panic!("expected duplicate credential name error, got {other}"),
    }
}

#[test]
fn rejects_empty_required_string_fields() {
    let error = Config::from_str(
        r#"
[[profiles]]
name = ""
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
"#,
    )
    .expect_err("empty names should be rejected");

    assert!(error.to_string().contains("name"));
}

#[test]
fn rejects_profile_names_with_leading_or_trailing_whitespace() {
    for name in [" site-builder", "site-builder "] {
        let error = Config::from_str(&format!(
            r#"
[[profiles]]
name = "{name}"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
"#
        ))
        .expect_err("whitespace-padded profile names should be rejected");

        match error {
            ConfigError::FieldHasOuterWhitespace {
                field,
                profile,
                credential,
            } => {
                assert_eq!(field, "name");
                assert_eq!(profile, None);
                assert_eq!(credential, None);
            }
            other => panic!("expected whitespace validation error, got {other}"),
        }
    }
}

#[test]
fn rejects_uppercase_profile_names_at_parse_time() {
    assert_invalid_profile_name_parse_error("Site-Builder");
}

#[test]
fn rejects_digit_prefixed_profile_names_at_parse_time() {
    assert_invalid_profile_name_parse_error("123site-builder");
}

#[test]
fn rejects_reserved_profile_names_at_parse_time() {
    assert_invalid_profile_name_parse_error("root");
}

#[test]
fn rejects_profile_names_longer_than_thirty_two_characters_at_parse_time() {
    assert_invalid_profile_name_parse_error(&format!("a{}", "b".repeat(32)));
}

#[test]
fn rejects_credential_names_with_leading_or_trailing_whitespace() {
    for name in [" GITHUB_TOKEN", "GITHUB_TOKEN "] {
        let error = Config::from_str(&format!(
            r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]

[[profiles.credentials]]
name = "{name}"
source = "AGENTD_GITHUB_TOKEN"
"#
        ))
        .expect_err("whitespace-padded credential names should be rejected");

        match error {
            ConfigError::FieldHasOuterWhitespace {
                field,
                profile,
                credential,
            } => {
                assert_eq!(field, "credentials.name");
                assert_eq!(profile.as_deref(), Some("site-builder"));
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
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]

[[profiles.credentials]]
name = "TOKEN,EXTRA"
source = "AGENTD_GITHUB_TOKEN"
"#,
    )
    .expect_err("comma-delimited credential names should be rejected");

    match error {
        ConfigError::InvalidCredentialName { profile, name } => {
            assert_eq!(profile, "site-builder");
            assert_eq!(name, "TOKEN,EXTRA");
        }
        other => panic!("expected invalid credential name error, got {other}"),
    }
}

#[test]
fn rejects_credential_names_containing_equals_signs() {
    let error = Config::from_str(
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]

[[profiles.credentials]]
name = "FOO=BAR"
source = "AGENTD_GITHUB_TOKEN"
"#,
    )
    .expect_err("credential names containing '=' should be rejected");

    match error {
        ConfigError::InvalidCredentialName { profile, name } => {
            assert_eq!(profile, "site-builder");
            assert_eq!(name, "FOO=BAR");
        }
        other => panic!("expected invalid credential name error, got {other}"),
    }
}

#[test]
fn rejects_reserved_credential_names() {
    let error = Config::from_str(
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]

[[profiles.credentials]]
name = "PROFILE_NAME"
source = "AGENTD_GITHUB_TOKEN"
"#,
    )
    .expect_err("runner-reserved credential names should be rejected");

    match error {
        ConfigError::InvalidCredentialName { profile, name } => {
            assert_eq!(profile, "site-builder");
            assert_eq!(name, "PROFILE_NAME");
        }
        other => panic!("expected invalid credential name error, got {other}"),
    }
}

#[test]
fn rejects_empty_command_arrays_and_empty_command_elements() {
    let empty_command_error = Config::from_str(
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = []
"#,
    )
    .expect_err("empty command arrays should be rejected");

    let empty_element_error = Config::from_str(
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", ""]
"#,
    )
    .expect_err("empty command elements should be rejected");

    assert!(empty_command_error.to_string().contains("command"));
    assert!(empty_element_error.to_string().contains("command"));
}

#[test]
fn rejects_missing_command_field() {
    let error = Config::from_str(
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"
"#,
    )
    .expect_err("missing command field should be rejected");

    assert!(error.to_string().contains("command"));
}

#[test]
fn rejects_legacy_runa_table() {
    let error = Config::from_str(
        r#"
[[profiles]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

[profiles.runa]
command = ["site-builder", "exec"]
"#,
    )
    .expect_err("legacy runa table should be rejected");

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

[[profiles]]
name = "Site-Builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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
[[profiles]]
name = "Site-Builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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

[[profiles]]
name = "Site-Builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

command = ["site-builder", "exec"]
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

[[profiles]]
unexpected = "still allowed to exist here"
"#,
    );

    let error =
        DaemonConfig::load(&path).expect_err("unknown top-level sections should be rejected");

    assert!(error.to_string().contains("unknown field"));
    assert!(error.to_string().contains("deamon"));
}

#[test]
fn loading_daemon_config_ignores_invalid_profile_registry_entries() {
    let path = write_temp_config(
        "daemon-only-ignores-profiles",
        r#"
[daemon]
socket_path = "runtime/agentd.sock"
pid_file = "runtime/agentd.pid"

[[profiles]]
unexpected = "daemon loader should ignore profile schema entirely"
"#,
    );

    let config = DaemonConfig::load(&path).expect("daemon config should parse");
    let base_dir = path
        .parent()
        .expect("config file should have a parent directory");

    assert_eq!(config.socket_path(), base_dir.join("runtime/agentd.sock"));
    assert_eq!(config.pid_file(), base_dir.join("runtime/agentd.pid"));
}
