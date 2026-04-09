use std::collections::BTreeSet;

use crate::podman::run_podman_command;
use crate::{RunnerError, StartupReconciliationReport};

const CONTAINER_PREFIX: &str = "agentd-";
const SECRET_PREFIX: &str = "agentd-secret-";
const SESSION_ID_LEN: usize = 32;

/// Removes stale runner-managed Podman resources before the daemon begins
/// accepting new work.
pub fn reconcile_startup_resources() -> Result<StartupReconciliationReport, RunnerError> {
    let containers = list_agentd_containers()?;

    let removed_container_names = containers
        .iter()
        .filter(|container| container.startup_reconciliation == StartupReconciliation::Remove)
        .map(|container| container.name.clone())
        .collect::<Vec<_>>();
    remove_containers(&removed_container_names)?;

    let live_session_ids = containers
        .iter()
        .filter(|container| container.startup_reconciliation == StartupReconciliation::Preserve)
        .filter_map(|container| parse_container_session_id(&container.name))
        .collect::<BTreeSet<_>>();

    let removed_secret_names = list_agentd_secret_names()?
        .into_iter()
        .filter(|name| {
            parse_secret_session_id(name)
                .map(|session_id| !live_session_ids.contains(session_id))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    remove_secrets(&removed_secret_names)?;

    Ok(StartupReconciliationReport {
        removed_container_names,
        removed_secret_names,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContainerRecord {
    name: String,
    startup_reconciliation: StartupReconciliation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupReconciliation {
    Remove,
    Preserve,
}

fn list_agentd_containers() -> Result<Vec<ContainerRecord>, RunnerError> {
    Ok(run_podman_command(vec![
        "ps".to_string(),
        "-a".to_string(),
        "--format".to_string(),
        "{{.Names}} {{.State}}".to_string(),
    ])?
    .lines()
    .filter_map(parse_container_record_line)
    .filter(|container| container.name.starts_with(CONTAINER_PREFIX))
    .collect())
}

fn parse_container_record_line(line: &str) -> Option<ContainerRecord> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (name, state) = trimmed.rsplit_once(char::is_whitespace)?;
    Some(ContainerRecord {
        name: name.trim().to_string(),
        startup_reconciliation: classify_startup_reconciliation(state.trim()),
    })
}

fn classify_startup_reconciliation(state: &str) -> StartupReconciliation {
    match state {
        "exited" | "dead" | "stopped" | "created" => StartupReconciliation::Remove,
        _ => StartupReconciliation::Preserve,
    }
}

fn list_agentd_secret_names() -> Result<Vec<String>, RunnerError> {
    Ok(run_podman_command(vec![
        "secret".to_string(),
        "ls".to_string(),
        "--format".to_string(),
        "{{.Name}}".to_string(),
    ])?
    .lines()
    .map(str::trim)
    .filter(|line| line.starts_with(SECRET_PREFIX))
    .map(ToString::to_string)
    .collect())
}

fn remove_containers(container_names: &[String]) -> Result<(), RunnerError> {
    if container_names.is_empty() {
        return Ok(());
    }

    let mut args = vec!["rm".to_string(), "--force".to_string()];
    args.extend(container_names.iter().cloned());
    run_podman_command(args).map(|_| ())
}

fn remove_secrets(secret_names: &[String]) -> Result<(), RunnerError> {
    if secret_names.is_empty() {
        return Ok(());
    }

    let mut args = vec![
        "secret".to_string(),
        "rm".to_string(),
        "--ignore".to_string(),
    ];
    args.extend(secret_names.iter().cloned());
    run_podman_command(args).map(|_| ())
}

fn parse_container_session_id(name: &str) -> Option<&str> {
    let suffix = name.strip_prefix(CONTAINER_PREFIX)?;
    let (_, session_id) = suffix.rsplit_once('-')?;
    is_session_id(session_id).then_some(session_id)
}

fn parse_secret_session_id(name: &str) -> Option<&str> {
    let suffix = name.strip_prefix(SECRET_PREFIX)?;
    let (session_id, _) = suffix.split_once('-')?;
    is_session_id(session_id).then_some(session_id)
}

fn is_session_id(value: &str) -> bool {
    value.len() == SESSION_ID_LEN && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
