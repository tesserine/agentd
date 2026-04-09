pub(crate) const PODMAN_RESOURCE_PREFIX: &str = "agentd-";
pub(crate) const DAEMON_INSTANCE_ID_LEN: usize = 8;
pub(crate) const SESSION_ID_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContainerNameParts<'a> {
    pub(crate) daemon_instance_id: &'a str,
    pub(crate) agent_name: &'a str,
    pub(crate) session_id: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SecretNameParts<'a> {
    pub(crate) daemon_instance_id: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) secret_suffix: &'a str,
}

pub(crate) fn format_container_name(
    daemon_instance_id: &str,
    agent_name: &str,
    session_id: &str,
) -> String {
    format!("{PODMAN_RESOURCE_PREFIX}{daemon_instance_id}-{agent_name}-{session_id}")
}

pub(crate) fn format_secret_name(
    daemon_instance_id: &str,
    session_id: &str,
    secret_suffix: &str,
) -> String {
    format!("{PODMAN_RESOURCE_PREFIX}{daemon_instance_id}-{session_id}-{secret_suffix}")
}

pub(crate) fn parse_container_name(name: &str) -> Option<ContainerNameParts<'_>> {
    let suffix = name.strip_prefix(PODMAN_RESOURCE_PREFIX)?;
    let (daemon_instance_id, remainder) = suffix.split_once('-')?;
    let (agent_name, session_id) = remainder.rsplit_once('-')?;

    (is_daemon_instance_id(daemon_instance_id)
        && !agent_name.is_empty()
        && is_session_id(session_id))
    .then_some(ContainerNameParts {
        daemon_instance_id,
        agent_name,
        session_id,
    })
}

pub(crate) fn parse_secret_name(name: &str) -> Option<SecretNameParts<'_>> {
    let suffix = name.strip_prefix(PODMAN_RESOURCE_PREFIX)?;
    let (daemon_instance_id, remainder) = suffix.split_once('-')?;
    let (session_id, secret_suffix) = remainder.split_once('-')?;

    (is_daemon_instance_id(daemon_instance_id)
        && is_session_id(session_id)
        && is_owned_secret_suffix(secret_suffix))
    .then_some(SecretNameParts {
        daemon_instance_id,
        session_id,
        secret_suffix,
    })
}

pub(crate) fn is_daemon_instance_id(value: &str) -> bool {
    is_lower_hex_of_len(value, DAEMON_INSTANCE_ID_LEN)
}

pub(crate) fn is_session_id(value: &str) -> bool {
    is_lower_hex_of_len(value, SESSION_ID_LEN)
}

pub(crate) fn is_owned_secret_suffix(value: &str) -> bool {
    value == "repo-token" || (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_lower_hex_of_len(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}
