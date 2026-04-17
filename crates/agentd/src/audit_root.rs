use std::fs::{self, OpenOptions};
use std::io;
use std::path::PathBuf;

use getrandom::fill as fill_random_bytes;

use crate::config::{ConfigError, DaemonConfig};

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

#[cfg(test)]
mod tests {
    use super::lower_hex_random_suffix_with;

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
}
