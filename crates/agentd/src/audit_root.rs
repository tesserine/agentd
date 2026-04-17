use std::fs::{self, OpenOptions};
use std::path::PathBuf;

use crate::config::{ConfigError, DaemonConfig};

pub(crate) fn prepare_audit_root(config: &DaemonConfig) -> Result<PathBuf, ConfigError> {
    let audit_root = config.resolve_audit_root()?;
    fs::create_dir_all(&audit_root).map_err(|error| ConfigError::AuditRootNotWritable {
        path: audit_root.clone(),
        error,
    })?;

    let probe_name = format!(
        ".agentd-audit-root-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after the unix epoch")
            .as_nanos()
    );
    let probe_path = audit_root.join(probe_name);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe_path)
        .map_err(|error| ConfigError::AuditRootNotWritable {
            path: audit_root.clone(),
            error,
        })?;
    fs::remove_file(&probe_path).map_err(|error| ConfigError::AuditRootNotWritable {
        path: audit_root.clone(),
        error,
    })?;

    Ok(audit_root)
}
