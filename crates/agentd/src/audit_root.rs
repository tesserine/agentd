#[cfg(test)]
use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use getrandom::fill as fill_random_bytes;

use crate::config::{ConfigError, DaemonConfig};

const ACTIVE_AUDIT_PROBE_DIRECTORY_MODE: u32 = 0o755;
const ACTIVE_AUDIT_PROBE_FILE_MODE: u32 = 0o644;
const SEALED_AUDIT_PROBE_DIRECTORY_MODE: u32 = 0o555;
const SEALED_AUDIT_PROBE_FILE_MODE: u32 = 0o444;

#[cfg(test)]
std::thread_local! {
    static FAIL_PROBE_CHMOD_CALL_FOR_TESTS: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn prepare_audit_root(config: &DaemonConfig) -> Result<PathBuf, ConfigError> {
    let audit_root = config.resolve_audit_root()?;
    fs::create_dir_all(&audit_root).map_err(|error| ConfigError::AuditRootNotWritable {
        path: audit_root.clone(),
        error,
    })?;

    let probe_name =
        audit_root_probe_name().map_err(|error| ConfigError::AuditRootNotWritable {
            path: audit_root.clone(),
            error,
        })?;
    let probe_path = audit_root.join(probe_name);
    let probe_file_path = probe_path.join("probe-file");
    fs::create_dir(&probe_path).map_err(|error| ConfigError::AuditRootNotWritable {
        path: audit_root.clone(),
        error,
    })?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe_file_path)
        .map_err(|error| ConfigError::AuditRootNotWritable {
            path: audit_root.clone(),
            error,
        })?;
    set_probe_permissions(
        &probe_file_path,
        fs::Permissions::from_mode(SEALED_AUDIT_PROBE_FILE_MODE),
    )
    .map_err(|error| ConfigError::AuditRootNotWritable {
        path: audit_root.clone(),
        error,
    })?;
    set_probe_permissions(
        &probe_path,
        fs::Permissions::from_mode(SEALED_AUDIT_PROBE_DIRECTORY_MODE),
    )
    .map_err(|error| ConfigError::AuditRootNotWritable {
        path: audit_root.clone(),
        error,
    })?;
    set_probe_permissions(
        &probe_path,
        fs::Permissions::from_mode(ACTIVE_AUDIT_PROBE_DIRECTORY_MODE),
    )
    .map_err(|error| ConfigError::AuditRootNotWritable {
        path: audit_root.clone(),
        error,
    })?;
    set_probe_permissions(
        &probe_file_path,
        fs::Permissions::from_mode(ACTIVE_AUDIT_PROBE_FILE_MODE),
    )
    .map_err(|error| ConfigError::AuditRootNotWritable {
        path: audit_root.clone(),
        error,
    })?;
    fs::remove_file(&probe_file_path).map_err(|error| ConfigError::AuditRootNotWritable {
        path: audit_root.clone(),
        error,
    })?;
    fs::remove_dir(&probe_path).map_err(|error| ConfigError::AuditRootNotWritable {
        path: audit_root.clone(),
        error,
    })?;

    Ok(audit_root)
}

fn audit_root_probe_name() -> io::Result<String> {
    Ok(format!(
        ".agentd-audit-root-probe-{}",
        lower_hex_random_suffix_with(|bytes| fill_random_bytes(bytes).map_err(io::Error::other))?
    ))
}

fn lower_hex_random_suffix_with<F>(fill_random: F) -> io::Result<String>
where
    F: FnOnce(&mut [u8]) -> io::Result<()>,
{
    let mut bytes = [0_u8; 8];
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

fn set_probe_permissions(path: &std::path::Path, permissions: fs::Permissions) -> io::Result<()> {
    #[cfg(test)]
    {
        let should_fail = FAIL_PROBE_CHMOD_CALL_FOR_TESTS.with(Cell::get);
        if should_fail {
            return Err(io::Error::other("injected audit-root chmod probe failure"));
        }
    }

    fs::set_permissions(path, permissions)
}

#[cfg(test)]
fn with_probe_chmod_failure_for_tests<T>(run: impl FnOnce() -> T) -> T {
    FAIL_PROBE_CHMOD_CALL_FOR_TESTS.with(|failure| {
        assert!(
            !failure.get(),
            "audit-root chmod failure injection should not be nested"
        );
        failure.set(true);
    });

    struct ResetGuard;

    impl Drop for ResetGuard {
        fn drop(&mut self) {
            FAIL_PROBE_CHMOD_CALL_FOR_TESTS.with(|failure| failure.set(false));
        }
    }

    let _guard = ResetGuard;
    run()
}

#[cfg(test)]
mod tests {
    use super::lower_hex_random_suffix_with;
    use crate::config::{Config, ConfigError};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::str::FromStr;

    fn unique_test_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after the unix epoch")
                .as_nanos()
        ))
    }

    fn config_with_audit_root(audit_root: &Path) -> Config {
        Config::from_str(&format!(
            r#"
[daemon]
audit_root = "{audit_root}"

[[agents]]
name = "site-builder"
base_image = "ghcr.io/example/site-builder:latest"
methodology_dir = "../groundwork"

[agents.command]
argv = ["site-builder", "exec"]
"#,
            audit_root = audit_root.display(),
        ))
        .expect("config should parse")
    }

    #[test]
    fn prepare_audit_root_does_not_derive_probe_names_from_process_id_or_system_time() {
        let source = include_str!("audit_root.rs");
        let implementation_source = source
            .split("#[cfg(test)]")
            .next()
            .expect("audit_root.rs should contain implementation before tests");

        assert!(
            !implementation_source.contains("std::process::id()"),
            "audit-root probe names must not derive uniqueness from process id"
        );
        assert!(
            !implementation_source.contains("SystemTime::now()"),
            "audit-root probe names must not derive uniqueness from system time"
        );
    }

    #[test]
    fn lower_hex_random_suffix_encodes_eight_random_bytes_as_sixteen_lower_hex_characters() {
        let suffix = lower_hex_random_suffix_with(|bytes| {
            bytes.copy_from_slice(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
            Ok(())
        })
        .expect("probe suffix generation should succeed");

        assert_eq!(suffix, "0123456789abcdef");
        assert_eq!(suffix.len(), 16);
        assert!(
            suffix
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
            "probe suffix should be lowercase hex: {suffix}"
        );
    }

    #[test]
    fn prepare_audit_root_probes_create_chmod_restore_and_remove() {
        let audit_root = unique_test_dir("agentd-audit-root-probe-pass");
        let config = config_with_audit_root(&audit_root);

        let prepared = super::prepare_audit_root(config.daemon())
            .expect("audit root probe should pass for daemon-owned files");

        assert_eq!(prepared, audit_root);
        assert!(
            fs::read_dir(&prepared)
                .expect("audit root should be readable")
                .next()
                .is_none(),
            "successful probe should remove the probe tree"
        );

        fs::remove_dir_all(&prepared).expect("temporary audit root should be removed");
    }

    #[test]
    fn prepare_audit_root_reports_daemon_local_probe_scope_and_uid_alignment_requirement() {
        let audit_root = unique_test_dir("agentd-audit-root-probe-chmod-fail");
        let config = config_with_audit_root(&audit_root);

        let error = super::with_probe_chmod_failure_for_tests(|| {
            super::prepare_audit_root(config.daemon())
                .expect_err("injected chmod failure should fail the audit-root probe")
        });

        match error {
            ConfigError::AuditRootNotWritable { ref path, .. } => {
                assert_eq!(path, &audit_root);
            }
            other => panic!("expected audit-root config error, got {other:?}"),
        }

        let message = error.to_string();
        assert!(
            message.contains("daemon-local create/chmod/remove probe"),
            "error should name probe scope, got {message}"
        );
        assert!(
            message.contains("UID-aligned"),
            "error should name UID-alignment requirement, got {message}"
        );
        assert!(
            message.contains(&audit_root.display().to_string()),
            "error should include audit root path, got {message}"
        );

        if audit_root.exists() {
            fs::remove_dir_all(&audit_root).expect("temporary audit root should be removed");
        }
    }
}
