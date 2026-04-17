use crate::podman::run_podman_command;
use crate::{RunnerError, SessionInvocation, SessionOutcome, SessionSpec};
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const METADATA_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionAuditRecord {
    pub(crate) record_dir: PathBuf,
    pub(crate) runa_dir: PathBuf,
    pub(crate) metadata_path: PathBuf,
    pub(crate) session_id: String,
    pub(crate) profile: String,
    pub(crate) repo_url: String,
    pub(crate) work_unit: Option<String>,
    pub(crate) start_timestamp: String,
}

pub(crate) enum SessionAuditCompletion<'a> {
    Outcome(&'a SessionOutcome),
    Error,
}

#[derive(Debug, Serialize)]
struct SessionAuditMetadata<'a> {
    schema_version: u32,
    session_id: &'a str,
    profile: &'a str,
    repo_url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    work_unit: Option<&'a str>,
    start_timestamp: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_timestamp: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
}

pub(crate) fn prepare_session_audit_record(
    session_id: &str,
    spec: &SessionSpec,
    invocation: &SessionInvocation,
) -> Result<SessionAuditRecord, RunnerError> {
    prepare_session_audit_record_at(&spec.audit_root, session_id, spec, invocation)
}

fn prepare_session_audit_record_at(
    host_root: &Path,
    session_id: &str,
    spec: &SessionSpec,
    invocation: &SessionInvocation,
) -> Result<SessionAuditRecord, RunnerError> {
    let record_dir = host_root.join(&spec.profile_name).join(session_id);
    let runa_dir = record_dir.join("runa");
    let agentd_dir = record_dir.join("agentd");
    let metadata_path = agentd_dir.join("session.json");

    fs::create_dir_all(&runa_dir)?;
    fs::create_dir_all(&agentd_dir)?;
    set_active_runa_permissions(&runa_dir)?;

    let start_timestamp = current_timestamp()?;
    let record = SessionAuditRecord {
        record_dir,
        runa_dir,
        metadata_path,
        session_id: session_id.to_string(),
        profile: spec.profile_name.clone(),
        repo_url: invocation.repo_url.clone(),
        work_unit: invocation.work_unit.clone(),
        start_timestamp,
    };

    write_session_audit_metadata(&record, None, None, None)?;
    Ok(record)
}

pub(crate) fn finalize_session_audit_record(
    record: &SessionAuditRecord,
    completion: SessionAuditCompletion<'_>,
) -> Result<(), RunnerError> {
    let (outcome, exit_code) = match completion {
        SessionAuditCompletion::Outcome(outcome) => (Some(outcome.label()), outcome.exit_code()),
        SessionAuditCompletion::Error => (Some("error"), None),
    };
    let end_timestamp = current_timestamp()?;

    write_session_audit_metadata(record, Some(&end_timestamp), outcome, exit_code)?;
    seal_session_audit_record(record)
}

fn write_session_audit_metadata(
    record: &SessionAuditRecord,
    end_timestamp: Option<&str>,
    outcome: Option<&str>,
    exit_code: Option<i32>,
) -> Result<(), RunnerError> {
    let metadata = SessionAuditMetadata {
        schema_version: METADATA_SCHEMA_VERSION,
        session_id: &record.session_id,
        profile: &record.profile,
        repo_url: &record.repo_url,
        work_unit: record.work_unit.as_deref(),
        start_timestamp: &record.start_timestamp,
        end_timestamp,
        outcome,
        exit_code,
    };
    let mut payload = serde_json::to_vec_pretty(&metadata)
        .map_err(|error| RunnerError::Io(std::io::Error::other(error)))?;
    payload.push(b'\n');
    write_atomic(&record.metadata_path, &payload)?;
    Ok(())
}

fn current_timestamp() -> Result<String, RunnerError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| RunnerError::Io(std::io::Error::other(error)))
}

fn set_active_runa_permissions(path: &Path) -> Result<(), RunnerError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o777))?;
    Ok(())
}

fn seal_session_audit_record(record: &SessionAuditRecord) -> Result<(), RunnerError> {
    match seal_path_recursive(&record.record_dir) {
        Ok(()) => Ok(()),
        Err(RunnerError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            seal_with_podman_unshare(&record.record_dir)
        }
        Err(error) => Err(error),
    }
}

fn seal_path_recursive(path: &Path) -> Result<(), RunnerError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            seal_path_recursive(&entry.path())?;
        }
    }

    seal_path(path, metadata.is_dir())
}

fn seal_path(path: &Path, is_dir: bool) -> Result<(), RunnerError> {
    let sealed_mode = if is_dir { 0o555 } else { 0o444 };
    fs::set_permissions(path, fs::Permissions::from_mode(sealed_mode))?;
    Ok(())
}

fn seal_with_podman_unshare(path: &Path) -> Result<(), RunnerError> {
    run_podman_command(vec![
        "unshare".to_string(),
        "find".to_string(),
        "-P".to_string(),
        path.display().to_string(),
        "-type".to_string(),
        "d".to_string(),
        "-exec".to_string(),
        "chmod".to_string(),
        "0555".to_string(),
        "{}".to_string(),
        "+".to_string(),
    ])?;
    run_podman_command(vec![
        "unshare".to_string(),
        "find".to_string(),
        "-P".to_string(),
        path.display().to_string(),
        "!".to_string(),
        "-type".to_string(),
        "d".to_string(),
        "!".to_string(),
        "-type".to_string(),
        "l".to_string(),
        "-exec".to_string(),
        "chmod".to_string(),
        "0444".to_string(),
        "{}".to_string(),
        "+".to_string(),
    ])
    .map(|_| ())
}

fn write_atomic(path: &Path, payload: &[u8]) -> Result<(), RunnerError> {
    let temp_path = path.with_extension("json.tmp");
    let parent = path.parent().ok_or_else(|| {
        RunnerError::Io(std::io::Error::other(
            "audit metadata path must have a parent directory",
        ))
    })?;
    let write_result = (|| -> Result<(), RunnerError> {
        let mut temp_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)?;
        temp_file.write_all(payload)?;
        temp_file.sync_all()?;
        drop(temp_file);
        fs::rename(&temp_path, path)?;
        sync_parent_dir(parent)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }

    write_result
}

fn sync_parent_dir(path: &Path) -> Result<(), RunnerError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{SessionAuditCompletion, current_timestamp, prepare_session_audit_record_at};
    use crate::test_support::test_session_spec;
    use crate::{SessionInvocation, SessionOutcome};
    use serde_json::Value;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

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

    fn make_tree_writable(path: &Path) {
        let metadata = fs::symlink_metadata(path).expect("path metadata should exist");
        if metadata.file_type().is_symlink() {
            return;
        }

        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("directory should be readable") {
                let entry = entry.expect("directory entry should be readable");
                make_tree_writable(&entry.path());
            }
        }

        let writable_mode = metadata.permissions().mode() | 0o700;
        fs::set_permissions(path, fs::Permissions::from_mode(writable_mode))
            .expect("path should become writable for cleanup");
    }

    #[test]
    fn prepare_session_audit_record_writes_initial_metadata_without_end_or_outcome() {
        let root = unique_test_dir("agentd-audit-initial");
        let record = prepare_session_audit_record_at(
            &root,
            "0123456789abcdef",
            &test_session_spec(),
            &SessionInvocation {
                repo_url: "https://example.com/agentd.git".to_string(),
                repo_token: None,
                work_unit: Some("issue-76".to_string()),
                timeout: None,
            },
        )
        .expect("audit record should be created");

        let payload = fs::read_to_string(record.metadata_path)
            .expect("initial session metadata should be readable");
        let json: Value = serde_json::from_str(&payload).expect("metadata should be valid json");

        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["session_id"], "0123456789abcdef");
        assert_eq!(json["profile"], "site-builder");
        assert_eq!(json["repo_url"], "https://example.com/agentd.git");
        assert_eq!(json["work_unit"], "issue-76");
        assert!(json.get("end_timestamp").is_none());
        assert!(json.get("outcome").is_none());
        assert!(json.get("exit_code").is_none());

        fs::remove_dir_all(root).expect("temporary audit root should be removed");
    }

    #[test]
    fn finalize_session_audit_record_writes_outcome_and_seals_record() {
        let root = unique_test_dir("agentd-audit-final");
        let record = prepare_session_audit_record_at(
            &root,
            "fedcba9876543210",
            &test_session_spec(),
            &SessionInvocation {
                repo_url: "https://example.com/agentd.git".to_string(),
                repo_token: None,
                work_unit: None,
                timeout: None,
            },
        )
        .expect("audit record should be created");
        fs::write(record.runa_dir.join("artifact.txt"), "persisted\n")
            .expect("runa artifact should be writable before sealing");

        super::finalize_session_audit_record(
            &record,
            SessionAuditCompletion::Outcome(&SessionOutcome::WorkFailed { exit_code: 5 }),
        )
        .expect("audit record should finalize");

        let payload = fs::read_to_string(&record.metadata_path)
            .expect("final session metadata should be readable");
        let json: Value = serde_json::from_str(&payload).expect("metadata should be valid json");

        assert_eq!(json["outcome"], "work_failed");
        assert_eq!(json["exit_code"], 5);
        assert!(json["end_timestamp"].is_string());

        let runa_mode = fs::metadata(&record.runa_dir)
            .expect("runa dir metadata should exist")
            .permissions()
            .mode();
        let metadata_mode = fs::metadata(&record.metadata_path)
            .expect("metadata file should exist")
            .permissions()
            .mode();
        assert_eq!(runa_mode & 0o777, 0o555);
        assert_eq!(metadata_mode & 0o777, 0o444);

        make_tree_writable(&root);

        fs::remove_dir_all(root).expect("temporary audit root should be removed");
    }

    #[test]
    fn finalize_session_audit_record_skips_symlinks_when_sealing() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = unique_test_dir("agentd-audit-symlink");
        let outside_target = root.join("outside-target.txt");
        let record = prepare_session_audit_record_at(
            &root,
            "1111222233334444",
            &test_session_spec(),
            &SessionInvocation {
                repo_url: "https://example.com/agentd.git".to_string(),
                repo_token: None,
                work_unit: None,
                timeout: None,
            },
        )
        .expect("audit record should be created");

        fs::write(&outside_target, "outside\n").expect("outside target should be writable");
        fs::set_permissions(&outside_target, fs::Permissions::from_mode(0o666))
            .expect("outside target mode should be writable");
        symlink(&outside_target, record.runa_dir.join("escaped-link"))
            .expect("symlink should be created");

        super::finalize_session_audit_record(
            &record,
            SessionAuditCompletion::Outcome(&SessionOutcome::Success { exit_code: 0 }),
        )
        .expect("audit record should finalize");

        let outside_mode = fs::metadata(&outside_target)
            .expect("outside target metadata should exist")
            .permissions()
            .mode();
        assert_eq!(outside_mode & 0o777, 0o666);

        make_tree_writable(&root);
        fs::remove_file(&outside_target).expect("outside target should be removed");
        fs::remove_dir_all(root).expect("temporary audit root should be removed");
    }

    #[test]
    fn write_session_audit_metadata_replaces_file_without_leaving_temp_file() {
        let root = unique_test_dir("agentd-audit-atomic-write");
        let record = prepare_session_audit_record_at(
            &root,
            "abcdabcdabcdabcd",
            &test_session_spec(),
            &SessionInvocation {
                repo_url: "https://example.com/agentd.git".to_string(),
                repo_token: None,
                work_unit: None,
                timeout: None,
            },
        )
        .expect("audit record should be created");

        super::finalize_session_audit_record(
            &record,
            SessionAuditCompletion::Outcome(&SessionOutcome::Success { exit_code: 0 }),
        )
        .expect("audit record should finalize");

        let payload = fs::read_to_string(&record.metadata_path)
            .expect("final session metadata should be readable");
        let json: Value = serde_json::from_str(&payload).expect("metadata should be valid json");
        assert_eq!(json["outcome"], "success");
        assert!(
            !record.metadata_path.with_extension("json.tmp").exists(),
            "temporary metadata file should not remain after atomic replace"
        );

        make_tree_writable(&root);

        fs::remove_dir_all(root).expect("temporary audit root should be removed");
    }

    #[test]
    fn current_timestamp_emits_rfc3339_utc_values() {
        let timestamp = current_timestamp().expect("timestamp should format");
        assert!(timestamp.ends_with('Z'));
        assert!(timestamp.contains('T'));
    }
}
