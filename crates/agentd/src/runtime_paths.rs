use std::fmt;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const SOCKET_BASENAME: &str = "agentd.sock";
const PID_BASENAME: &str = "agentd.pid";
const REQUIRED_TMP_MODE: u32 = 0o700;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonRuntimePaths {
    socket_path: PathBuf,
    pid_file: PathBuf,
}

impl DaemonRuntimePaths {
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn pid_file(&self) -> &Path {
        &self.pid_file
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientSocketPathError {
    InsecureTemporaryRuntimeDir { path: PathBuf, message: String },
}

impl fmt::Display for ClientSocketPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsecureTemporaryRuntimeDir { path, message } => {
                write!(
                    f,
                    "{message}: {}. Use --socket-path to override.",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ClientSocketPathError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePathLayout {
    uid: u32,
    xdg_runtime_dir: Option<PathBuf>,
    tmp_runtime_dir: PathBuf,
    system_runtime_dir: PathBuf,
}

impl RuntimePathLayout {
    fn detect() -> Self {
        let uid = unsafe { libc::geteuid() };
        Self {
            uid,
            xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
            tmp_runtime_dir: PathBuf::from(format!("/tmp/agentd-{uid}")),
            system_runtime_dir: PathBuf::from("/run/agentd"),
        }
    }

    fn client_socket_path(&self) -> Result<PathBuf, ClientSocketPathError> {
        if let Some(runtime_dir) = &self.xdg_runtime_dir {
            return Ok(runtime_dir.join("agentd").join(SOCKET_BASENAME));
        }

        if self.tmp_runtime_dir.exists() {
            validate_tmp_runtime_dir(&self.tmp_runtime_dir, self.uid)?;
            return Ok(self.tmp_runtime_dir.join(SOCKET_BASENAME));
        }

        Ok(self.system_runtime_dir.join(SOCKET_BASENAME))
    }

    fn daemon_runtime_paths(&self) -> DaemonRuntimePaths {
        let runtime_dir = if let Some(runtime_dir) = &self.xdg_runtime_dir {
            runtime_dir.join("agentd")
        } else if self.uid == 0 {
            self.system_runtime_dir.clone()
        } else {
            self.tmp_runtime_dir.clone()
        };

        DaemonRuntimePaths {
            socket_path: runtime_dir.join(SOCKET_BASENAME),
            pid_file: runtime_dir.join(PID_BASENAME),
        }
    }
}

pub fn default_daemon_runtime_paths() -> DaemonRuntimePaths {
    RuntimePathLayout::detect().daemon_runtime_paths()
}

pub fn resolve_client_socket_path(
    explicit_path: Option<&Path>,
) -> Result<PathBuf, ClientSocketPathError> {
    if let Some(path) = explicit_path {
        return Ok(path.to_path_buf());
    }

    RuntimePathLayout::detect().client_socket_path()
}

fn validate_tmp_runtime_dir(path: &Path, expected_uid: u32) -> Result<(), ClientSocketPathError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ClientSocketPathError::InsecureTemporaryRuntimeDir {
            path: path.to_path_buf(),
            message: format!("failed to inspect temporary runtime directory ({error})"),
        }
    })?;

    if !metadata.is_dir() {
        return Err(ClientSocketPathError::InsecureTemporaryRuntimeDir {
            path: path.to_path_buf(),
            message: "temporary runtime path exists but is not a directory".to_string(),
        });
    }

    let mode = metadata.mode() & 0o777;
    if mode != REQUIRED_TMP_MODE {
        return Err(ClientSocketPathError::InsecureTemporaryRuntimeDir {
            path: path.to_path_buf(),
            message: format!(
                "temporary runtime directory must have mode 0700, found {:04o}",
                mode
            ),
        });
    }

    let owner = metadata.uid();
    if owner != expected_uid {
        return Err(ClientSocketPathError::InsecureTemporaryRuntimeDir {
            path: path.to_path_buf(),
            message: format!(
                "temporary runtime directory must be owned by uid {expected_uid}, found uid {owner}"
            ),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn test_layout(
        uid: u32,
        xdg_runtime_dir: Option<PathBuf>,
        tmp_runtime_dir: PathBuf,
        system_runtime_dir: PathBuf,
    ) -> RuntimePathLayout {
        RuntimePathLayout {
            uid,
            xdg_runtime_dir,
            tmp_runtime_dir,
            system_runtime_dir,
        }
    }

    fn unique_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "agentd-runtime-paths-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("test directory should be created");
        path
    }

    #[test]
    fn client_socket_path_prefers_xdg_runtime_dir() {
        let layout = test_layout(
            1000,
            Some(PathBuf::from("/xdg/runtime")),
            PathBuf::from("/tmp/agentd-1000"),
            PathBuf::from("/run/agentd"),
        );

        assert_eq!(
            layout
                .client_socket_path()
                .expect("xdg path should resolve"),
            Path::new("/xdg/runtime/agentd/agentd.sock")
        );
    }

    #[test]
    fn client_socket_path_uses_tmp_runtime_dir_when_it_is_secure() {
        let root = unique_dir("client-tmp");
        let tmp_runtime_dir = root.join("tmp-runtime");
        std::fs::create_dir(&tmp_runtime_dir).expect("tmp runtime dir should be created");
        std::fs::set_permissions(&tmp_runtime_dir, std::fs::Permissions::from_mode(0o700))
            .expect("permissions should be set");

        let uid = unsafe { libc::geteuid() };
        let layout = test_layout(uid, None, tmp_runtime_dir.clone(), root.join("run-agentd"));

        assert_eq!(
            layout
                .client_socket_path()
                .expect("tmp path should resolve"),
            tmp_runtime_dir.join("agentd.sock")
        );
    }

    #[test]
    fn client_socket_path_rejects_tmp_runtime_dir_with_wrong_mode() {
        let root = unique_dir("client-tmp-bad-mode");
        let tmp_runtime_dir = root.join("tmp-runtime");
        std::fs::create_dir(&tmp_runtime_dir).expect("tmp runtime dir should be created");
        std::fs::set_permissions(&tmp_runtime_dir, std::fs::Permissions::from_mode(0o755))
            .expect("permissions should be set");

        let uid = unsafe { libc::geteuid() };
        let layout = test_layout(uid, None, tmp_runtime_dir.clone(), root.join("run-agentd"));

        let error = layout
            .client_socket_path()
            .expect_err("insecure tmp runtime dir should be rejected");
        assert!(error.to_string().contains("mode 0700"));
        assert!(
            error
                .to_string()
                .contains(tmp_runtime_dir.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn client_socket_path_falls_back_to_system_dir_when_tmp_runtime_dir_is_missing() {
        let root = unique_dir("client-run-fallback");
        let layout = test_layout(
            1000,
            None,
            root.join("missing-tmp-runtime"),
            root.join("run-agentd"),
        );

        assert_eq!(
            layout
                .client_socket_path()
                .expect("system fallback should resolve"),
            root.join("run-agentd/agentd.sock")
        );
    }

    #[test]
    fn daemon_runtime_paths_follow_xdg_runtime_dir_when_present() {
        let layout = test_layout(
            1000,
            Some(PathBuf::from("/xdg/runtime")),
            PathBuf::from("/tmp/agentd-1000"),
            PathBuf::from("/run/agentd"),
        );

        let runtime_paths = layout.daemon_runtime_paths();
        assert_eq!(
            runtime_paths.socket_path(),
            Path::new("/xdg/runtime/agentd/agentd.sock")
        );
        assert_eq!(
            runtime_paths.pid_file(),
            Path::new("/xdg/runtime/agentd/agentd.pid")
        );
    }

    #[test]
    fn daemon_runtime_paths_use_user_tmp_dir_when_xdg_is_unset_for_non_root() {
        let layout = test_layout(
            1000,
            None,
            PathBuf::from("/tmp/agentd-1000"),
            PathBuf::from("/run/agentd"),
        );

        let runtime_paths = layout.daemon_runtime_paths();
        assert_eq!(
            runtime_paths.socket_path(),
            Path::new("/tmp/agentd-1000/agentd.sock")
        );
        assert_eq!(
            runtime_paths.pid_file(),
            Path::new("/tmp/agentd-1000/agentd.pid")
        );
    }

    #[test]
    fn daemon_runtime_paths_use_system_dir_when_running_as_root_without_xdg() {
        let layout = test_layout(
            0,
            None,
            PathBuf::from("/tmp/agentd-0"),
            PathBuf::from("/run/agentd"),
        );

        let runtime_paths = layout.daemon_runtime_paths();
        assert_eq!(
            runtime_paths.socket_path(),
            Path::new("/run/agentd/agentd.sock")
        );
        assert_eq!(
            runtime_paths.pid_file(),
            Path::new("/run/agentd/agentd.pid")
        );
    }
}
