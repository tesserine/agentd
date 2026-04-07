use crate::podman::run_podman_command_with_input;
use crate::types::{ResolvedEnvironmentVariable, RunnerError, SessionSpec};
use getrandom::fill as fill_random_bytes;
use std::fs;
use std::path::{Path, PathBuf};

const METHODOLOGY_STAGE_LINK_NAME: &str = "methodology";
const SESSION_SECRET_PREFIX: &str = "agentd-secret-";
const SESSION_STAGE_PREFIX: &str = "agentd-session-stage-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretBinding {
    pub(crate) secret_name: String,
    pub(crate) target_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionResources {
    pub(crate) container_name: String,
    pub(crate) methodology_staging_dir: PathBuf,
    pub(crate) methodology_mount_source: PathBuf,
    pub(crate) secret_bindings: Vec<SecretBinding>,
}

pub(crate) fn unique_suffix() -> Result<String, RunnerError> {
    unique_suffix_with(|bytes| fill_random_bytes(bytes).map_err(std::io::Error::other))
}

pub(crate) fn prepare_session_resources(
    container_name: &str,
    spec: &SessionSpec,
    session_id: &str,
) -> Result<SessionResources, RunnerError> {
    let manifest_path = spec.methodology_dir.join("manifest.toml");
    if !manifest_path.is_file() {
        return Err(RunnerError::MissingMethodologyManifest {
            path: manifest_path,
        });
    }

    let methodology_staging_dir =
        create_methodology_staging_dir(&spec.methodology_dir, session_id)?;
    let methodology_mount_source = methodology_staging_dir.join(METHODOLOGY_STAGE_LINK_NAME);
    let mut resources = SessionResources {
        container_name: container_name.to_string(),
        methodology_staging_dir,
        methodology_mount_source,
        secret_bindings: Vec::new(),
    };

    for (index, variable) in spec.environment.iter().enumerate() {
        if variable.value.is_empty() {
            continue;
        }

        let secret_name = format!("{SESSION_SECRET_PREFIX}{session_id}-{index}");
        if let Err(error) = create_podman_secret(&secret_name, variable) {
            let _ = cleanup_podman_secrets(&resources.secret_bindings);
            let _ = cleanup_methodology_staging_dir(&resources.methodology_staging_dir);
            return Err(error);
        }
        resources.secret_bindings.push(SecretBinding {
            secret_name,
            target_name: variable.name.clone(),
        });
    }

    Ok(resources)
}

pub(crate) fn cleanup_podman_secrets(secret_bindings: &[SecretBinding]) -> Result<(), RunnerError> {
    if secret_bindings.is_empty() {
        return Ok(());
    }

    let mut args = vec![
        "secret".to_string(),
        "rm".to_string(),
        "--ignore".to_string(),
    ];
    args.extend(
        secret_bindings
            .iter()
            .map(|binding| binding.secret_name.clone()),
    );
    crate::podman::run_podman_command(args).map(|_| ())
}

pub(crate) fn cleanup_methodology_staging_dir(path: &Path) -> Result<(), RunnerError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RunnerError::Io(error)),
    }
}

fn create_methodology_staging_dir(
    methodology_dir: &Path,
    session_id: &str,
) -> Result<PathBuf, RunnerError> {
    let canonical_methodology_dir = methodology_dir.canonicalize()?;
    let staging_dir = safe_staging_root().join(format!("{SESSION_STAGE_PREFIX}{session_id}"));
    fs::create_dir_all(&staging_dir)?;
    let staged_link = staging_dir.join(METHODOLOGY_STAGE_LINK_NAME);

    if let Err(error) = create_directory_symlink(&canonical_methodology_dir, &staged_link) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(error);
    }

    Ok(staging_dir)
}

fn safe_staging_root() -> PathBuf {
    let temp_dir = std::env::temp_dir();
    if !path_requires_mount_staging_alias(&temp_dir) {
        return temp_dir;
    }

    #[cfg(unix)]
    {
        PathBuf::from("/tmp")
    }

    #[cfg(not(unix))]
    {
        temp_dir
    }
}

fn path_requires_mount_staging_alias(path: &Path) -> bool {
    path.to_string_lossy().contains(',')
}

fn create_podman_secret(
    secret_name: &str,
    variable: &ResolvedEnvironmentVariable,
) -> Result<(), RunnerError> {
    run_podman_command_with_input(
        vec![
            "secret".to_string(),
            "create".to_string(),
            secret_name.to_string(),
            "-".to_string(),
        ],
        Some(variable.value.as_bytes()),
    )
    .map(|_| ())
}

#[cfg(unix)]
fn create_directory_symlink(source: &Path, destination: &Path) -> Result<(), RunnerError> {
    std::os::unix::fs::symlink(source, destination).map_err(RunnerError::Io)
}

#[cfg(windows)]
fn create_directory_symlink(source: &Path, destination: &Path) -> Result<(), RunnerError> {
    std::os::windows::fs::symlink_dir(source, destination).map_err(RunnerError::Io)
}

fn unique_suffix_with<F>(fill_random: F) -> Result<String, RunnerError>
where
    F: FnOnce(&mut [u8]) -> std::io::Result<()>,
{
    let mut bytes = [0_u8; 16];
    fill_random(&mut bytes)?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        encoded.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }

    encoded
}
