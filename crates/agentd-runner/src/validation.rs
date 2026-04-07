use crate::resources::sanitize_name;
use crate::types::{EnvironmentNameValidationError, RunnerError, SessionInvocation, SessionSpec};

const AGENT_NAME_ENV: &str = "AGENT_NAME";
const SUPPORTED_REPO_URL_FORMS: &str = "https://, http://, or git://";
const SUPPORTED_REPO_URL_PREFIXES: [&str; 3] = ["https://", "http://", "git://"];

pub(crate) fn validate_spec(spec: &SessionSpec) -> Result<(), RunnerError> {
    if !is_valid_unix_username(&sanitize_name(&spec.agent_name)) {
        return Err(RunnerError::InvalidAgentName);
    }
    if spec.base_image.trim().is_empty() || spec.base_image != spec.base_image.trim() {
        return Err(RunnerError::InvalidBaseImage);
    }
    if spec.agent_command.is_empty() || spec.agent_command.iter().any(|arg| arg.is_empty()) {
        return Err(RunnerError::InvalidAgentCommand);
    }

    for variable in &spec.environment {
        match validate_environment_name(&variable.name) {
            Ok(()) => {}
            Err(EnvironmentNameValidationError::Invalid) => {
                return Err(RunnerError::InvalidEnvironmentName {
                    name: variable.name.clone(),
                });
            }
            Err(EnvironmentNameValidationError::Reserved) => {
                return Err(RunnerError::ReservedEnvironmentName {
                    name: variable.name.clone(),
                });
            }
        }
    }

    Ok(())
}

pub(crate) fn validate_invocation(invocation: &SessionInvocation) -> Result<(), RunnerError> {
    let repo_url = invocation.repo_url.as_str();
    if repo_url.trim().is_empty() || repo_url != repo_url.trim() {
        return Err(unsupported_repo_url_error());
    }

    if has_repo_url_userinfo(repo_url) {
        return Err(credential_bearing_repo_url_error());
    }

    if !is_supported_repo_url(repo_url) {
        return Err(unsupported_repo_url_error());
    }

    Ok(())
}

pub fn validate_environment_name(name: &str) -> Result<(), EnvironmentNameValidationError> {
    if name.is_empty() || name.contains('=') || name.contains(',') {
        return Err(EnvironmentNameValidationError::Invalid);
    }
    if is_reserved_environment_name(name) {
        return Err(EnvironmentNameValidationError::Reserved);
    }

    Ok(())
}

pub(crate) fn runner_managed_environment(spec: &SessionSpec) -> [(&str, &str); 1] {
    [(AGENT_NAME_ENV, &spec.agent_name)]
}

fn is_supported_repo_url(repo_url: &str) -> bool {
    if repo_url.contains(['?', '#']) {
        return false;
    }

    repo_url_authority(repo_url)
        .zip(repo_url_path(repo_url))
        .map(|(authority, path)| {
            !authority.is_empty()
                && !authority.starts_with('/')
                && path.starts_with('/')
                && path.len() > 1
                && SUPPORTED_REPO_URL_PREFIXES
                    .iter()
                    .any(|prefix| repo_url.starts_with(prefix))
        })
        .unwrap_or(false)
}

fn has_repo_url_userinfo(repo_url: &str) -> bool {
    repo_url_authority(repo_url)
        .map(|authority| authority.contains('@'))
        .unwrap_or(false)
}

fn repo_url_authority(repo_url: &str) -> Option<&str> {
    let scheme_end = repo_url.find("://")?;
    let remainder = repo_url.get(scheme_end + 3..)?;
    let path_start = remainder.find('/').unwrap_or(remainder.len());
    remainder.get(..path_start)
}

fn repo_url_path(repo_url: &str) -> Option<&str> {
    let scheme_end = repo_url.find("://")?;
    let remainder = repo_url.get(scheme_end + 3..)?;
    let path_start = remainder.find('/')?;
    remainder.get(path_start..)
}

fn unsupported_repo_url_error() -> RunnerError {
    RunnerError::InvalidRepoUrl {
        message: format!("must be a remote URL using {SUPPORTED_REPO_URL_FORMS}"),
    }
}

fn credential_bearing_repo_url_error() -> RunnerError {
    RunnerError::InvalidRepoUrl {
        message: "must not embed credentials in the URL; credential-bearing URLs are not accepted until #32 lands".to_string(),
    }
}

fn is_reserved_environment_name(name: &str) -> bool {
    matches!(name, AGENT_NAME_ENV)
}

fn is_valid_unix_username(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first_character) = characters.next() else {
        return false;
    };

    if !first_character.is_ascii_lowercase() || name.len() > 32 {
        return false;
    }

    characters.all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResolvedEnvironmentVariable;
    use std::path::PathBuf;

    #[test]
    fn validate_spec_rejects_reserved_environment_names() {
        let error = validate_spec(&SessionSpec {
            agent_name: "agent".to_string(),
            base_image: "image".to_string(),
            methodology_dir: PathBuf::from("/tmp/methodology"),
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "AGENT_NAME".to_string(),
                value: "spoofed".to_string(),
            }],
        })
        .expect_err("reserved runner environment names should be rejected");

        match error {
            RunnerError::ReservedEnvironmentName { name } => {
                assert_eq!(name, "AGENT_NAME");
            }
            other => panic!("expected ReservedEnvironmentName, got {other:?}"),
        }
    }

    #[test]
    fn validate_spec_rejects_environment_names_containing_commas() {
        let error = validate_spec(&SessionSpec {
            agent_name: "agent".to_string(),
            base_image: "image".to_string(),
            methodology_dir: PathBuf::from("/tmp/methodology"),
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "TOKEN,EXTRA".to_string(),
                value: "secret".to_string(),
            }],
        })
        .expect_err("comma-delimited environment names should be rejected");

        match error {
            RunnerError::InvalidEnvironmentName { name } => {
                assert_eq!(name, "TOKEN,EXTRA");
            }
            other => panic!("expected InvalidEnvironmentName, got {other:?}"),
        }
    }

    #[test]
    fn validate_spec_rejects_environment_names_containing_equals_signs() {
        let error = validate_spec(&SessionSpec {
            agent_name: "agent".to_string(),
            base_image: "image".to_string(),
            methodology_dir: PathBuf::from("/tmp/methodology"),
            agent_command: vec!["codex".to_string(), "exec".to_string()],
            environment: vec![ResolvedEnvironmentVariable {
                name: "TOKEN=EXTRA".to_string(),
                value: "secret".to_string(),
            }],
        })
        .expect_err("environment names containing '=' should be rejected");

        match error {
            RunnerError::InvalidEnvironmentName { name } => {
                assert_eq!(name, "TOKEN=EXTRA");
            }
            other => panic!("expected InvalidEnvironmentName, got {other:?}"),
        }
    }

    #[test]
    fn validate_spec_rejects_base_image_with_surrounding_whitespace() {
        for base_image in [" image", "image ", " image "] {
            let error = validate_spec(&SessionSpec {
                agent_name: "agent".to_string(),
                base_image: base_image.to_string(),
                methodology_dir: PathBuf::from("/tmp/methodology"),
                agent_command: vec!["codex".to_string(), "exec".to_string()],
                environment: Vec::new(),
            })
            .expect_err("base_image values with surrounding whitespace should be rejected");

            assert!(
                matches!(error, RunnerError::InvalidBaseImage),
                "expected InvalidBaseImage for {base_image:?}, got {error:?}"
            );
        }
    }

    #[test]
    fn validate_spec_accepts_agent_names_that_sanitize_to_valid_unix_usernames() {
        for agent_name in ["agent", "agent-01", "Agent 01", "agent_name"] {
            validate_spec(&SessionSpec {
                agent_name: agent_name.to_string(),
                base_image: "image".to_string(),
                methodology_dir: PathBuf::from("/tmp/methodology"),
                agent_command: vec!["codex".to_string(), "exec".to_string()],
                environment: Vec::new(),
            })
            .unwrap_or_else(|error| panic!("expected {agent_name:?} to be accepted, got {error}"));
        }
    }

    #[test]
    fn validate_spec_rejects_agent_names_that_sanitize_to_invalid_unix_usernames() {
        for agent_name in [
            "",
            "   ",
            "123agent",
            "---",
            &format!("a{}", "b".repeat(32)),
        ] {
            let error = validate_spec(&SessionSpec {
                agent_name: agent_name.to_string(),
                base_image: "image".to_string(),
                methodology_dir: PathBuf::from("/tmp/methodology"),
                agent_command: vec!["codex".to_string(), "exec".to_string()],
                environment: Vec::new(),
            })
            .expect_err("invalid sanitized agent names should be rejected");

            assert!(
                matches!(error, RunnerError::InvalidAgentName),
                "expected InvalidAgentName for {agent_name:?}, got {error:?}"
            );
        }
    }

    #[test]
    fn validate_invocation_accepts_supported_remote_repo_urls() {
        for repo_url in [
            "https://example.com/agentd.git",
            "https://example.com/agentd.git/",
            "http://example.com/agentd.git",
            "http://example.com/agentd.git/",
            "git://example.com/agentd.git",
            "git://example.com/agentd.git/",
        ] {
            validate_invocation(&SessionInvocation {
                repo_url: repo_url.to_string(),
                work_unit: None,
                timeout: None,
            })
            .unwrap_or_else(|error| panic!("expected {repo_url} to be accepted, got {error}"));
        }
    }

    #[test]
    fn validate_invocation_rejects_non_remote_repo_urls() {
        for repo_url in [
            "",
            " ",
            " repo ",
            "repo",
            "./repo",
            "../repo.git",
            "/srv/test-repo.git",
            "file:///srv/test-repo.git",
            "ftp://example.com/agentd.git",
            "gopher://example.com/agentd.git",
            "ssh://git@example.com/agentd.git",
            "git@example.com:agentd.git",
            "https://user:token@example.com/repo.git",
            "https://",
            "http://",
            "git://",
            "https://github.com",
            "http:///repo.git",
            "https://?ref=main",
            "https://#readme",
            "https://example.com/repo.git?token=secret",
            "https://example.com/repo.git#readme",
            "example.com:agentd.git",
            "git@example.com",
            "@example.com:agentd.git",
            "git@:agentd.git",
        ] {
            let error = validate_invocation(&SessionInvocation {
                repo_url: repo_url.to_string(),
                work_unit: None,
                timeout: None,
            })
            .expect_err("non-remote repo URL should be rejected");

            assert!(
                matches!(error, RunnerError::InvalidRepoUrl { .. }),
                "expected InvalidRepoUrl for {repo_url}, got {error:?}"
            );
        }
    }

    #[test]
    fn validate_invocation_rejects_credential_bearing_repo_urls() {
        let error = validate_invocation(&SessionInvocation {
            repo_url: "https://user:token@example.com/repo.git".to_string(),
            work_unit: None,
            timeout: None,
        })
        .expect_err("credential-bearing repo URLs should be rejected");

        let message = error.to_string();
        assert!(
            message.contains("credential-bearing URLs are not accepted"),
            "expected credential-bearing URL rejection message, got {message}"
        );
        assert!(
            message.contains("#32"),
            "expected credential-bearing URL rejection to reference #32, got {message}"
        );
    }

    #[test]
    fn run_session_rejects_invalid_repo_url_before_methodology_validation() {
        let error = crate::run_session(
            SessionSpec {
                agent_name: "agent".to_string(),
                base_image: "image".to_string(),
                methodology_dir: PathBuf::from("/tmp/does-not-exist"),
                agent_command: vec!["codex".to_string(), "exec".to_string()],
                environment: Vec::new(),
            },
            SessionInvocation {
                repo_url: "/srv/test-repo.git".to_string(),
                work_unit: None,
                timeout: None,
            },
        )
        .expect_err("invalid repo URL should be rejected before setup");

        assert!(
            matches!(error, RunnerError::InvalidRepoUrl { .. }),
            "expected InvalidRepoUrl, got {error:?}"
        );
    }

    #[test]
    fn run_session_rejects_credential_bearing_repo_url_before_methodology_validation() {
        let error = crate::run_session(
            SessionSpec {
                agent_name: "agent".to_string(),
                base_image: "image".to_string(),
                methodology_dir: PathBuf::from("/tmp/does-not-exist"),
                agent_command: vec!["codex".to_string(), "exec".to_string()],
                environment: Vec::new(),
            },
            SessionInvocation {
                repo_url: "https://user:token@example.com/repo.git".to_string(),
                work_unit: None,
                timeout: None,
            },
        )
        .expect_err("credential-bearing repo URL should be rejected before setup");

        assert!(
            matches!(error, RunnerError::InvalidRepoUrl { .. }),
            "expected InvalidRepoUrl, got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("credential-bearing URLs are not accepted until #32 lands"),
            "expected credential-bearing URL message, got {error}"
        );
    }
}
