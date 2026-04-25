use std::fmt;
use std::path::{Path, PathBuf};

const SOCKET_BASENAME: &str = "agentd.sock";
const PID_BASENAME: &str = "agentd.pid";

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
pub enum RuntimePathError {
    XdgRuntimeDirUnavailable,
    XdgRuntimeDirMustBeAbsolute { path: PathBuf },
}

impl fmt::Display for RuntimePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::XdgRuntimeDirUnavailable => write!(
                f,
                "XDG_RUNTIME_DIR is not set; set XDG_RUNTIME_DIR or configure explicit daemon runtime paths"
            ),
            Self::XdgRuntimeDirMustBeAbsolute { path } => write!(
                f,
                "XDG_RUNTIME_DIR must be an absolute path: {}; set XDG_RUNTIME_DIR to an absolute runtime directory or configure explicit daemon runtime paths",
                path.display()
            ),
        }
    }
}

impl std::error::Error for RuntimePathError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSocketPathError {
    source: RuntimePathError,
}

impl ClientSocketPathError {
    fn new(source: RuntimePathError) -> Self {
        Self { source }
    }
}

impl fmt::Display for ClientSocketPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            RuntimePathError::XdgRuntimeDirUnavailable => write!(
                f,
                "XDG_RUNTIME_DIR is not set; set XDG_RUNTIME_DIR or use --socket-path to override"
            ),
            RuntimePathError::XdgRuntimeDirMustBeAbsolute { path } => write!(
                f,
                "XDG_RUNTIME_DIR must be an absolute path: {}; set XDG_RUNTIME_DIR to an absolute runtime directory or use --socket-path to override",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ClientSocketPathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePathLayout {
    xdg_runtime_dir: PathBuf,
}

impl RuntimePathLayout {
    fn detect() -> Result<Self, RuntimePathError> {
        let Some(path) = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
        else {
            return Err(RuntimePathError::XdgRuntimeDirUnavailable);
        };

        if !path.is_absolute() {
            return Err(RuntimePathError::XdgRuntimeDirMustBeAbsolute { path });
        }

        Ok(Self {
            xdg_runtime_dir: path,
        })
    }

    fn client_socket_path(&self) -> PathBuf {
        self.runtime_dir().join(SOCKET_BASENAME)
    }

    fn daemon_runtime_paths(&self) -> DaemonRuntimePaths {
        let runtime_dir = self.runtime_dir();
        DaemonRuntimePaths {
            socket_path: runtime_dir.join(SOCKET_BASENAME),
            pid_file: runtime_dir.join(PID_BASENAME),
        }
    }

    fn runtime_dir(&self) -> PathBuf {
        self.xdg_runtime_dir.join("agentd")
    }
}

pub fn default_daemon_runtime_paths() -> Result<DaemonRuntimePaths, RuntimePathError> {
    RuntimePathLayout::detect().map(|layout| layout.daemon_runtime_paths())
}

pub fn resolve_client_socket_path(
    explicit_path: Option<&Path>,
) -> Result<PathBuf, ClientSocketPathError> {
    if let Some(path) = explicit_path {
        return Ok(path.to_path_buf());
    }

    RuntimePathLayout::detect()
        .map(|layout| layout.client_socket_path())
        .map_err(ClientSocketPathError::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn test_layout(xdg_runtime_dir: PathBuf) -> RuntimePathLayout {
        RuntimePathLayout { xdg_runtime_dir }
    }

    fn unique_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agentd-runtime-paths-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn client_socket_path_resolves_xdg_path_without_probing_for_a_daemon() {
        let xdg_runtime_dir = unique_dir("client-xdg-no-probe");
        let layout = test_layout(xdg_runtime_dir.clone());

        assert_eq!(
            layout.client_socket_path(),
            xdg_runtime_dir.join("agentd/agentd.sock")
        );
    }

    #[test]
    fn daemon_runtime_paths_resolve_from_the_same_xdg_runtime_dir() {
        let xdg_runtime_dir = unique_dir("daemon-xdg");
        let layout = test_layout(xdg_runtime_dir.clone());

        let runtime_paths = layout.daemon_runtime_paths();
        assert_eq!(
            runtime_paths.socket_path(),
            xdg_runtime_dir.join("agentd/agentd.sock")
        );
        assert_eq!(
            runtime_paths.pid_file(),
            xdg_runtime_dir.join("agentd/agentd.pid")
        );
    }

    #[test]
    fn client_socket_path_requires_xdg_runtime_dir_when_no_override_is_given() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }

        let error = resolve_client_socket_path(None)
            .expect_err("missing xdg runtime dir should be actionable");

        assert!(error.to_string().contains("XDG_RUNTIME_DIR"));
        assert!(error.to_string().contains("--socket-path"));
    }

    #[test]
    fn relative_xdg_runtime_dir_is_rejected() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "runtime");
        }

        let error = default_daemon_runtime_paths()
            .expect_err("relative xdg runtime dir should be rejected");

        assert!(error.to_string().contains("XDG_RUNTIME_DIR"));
        assert!(error.to_string().contains("absolute"));
        assert!(error.to_string().contains("runtime"));
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
    }

    #[test]
    fn explicit_client_socket_path_bypasses_xdg_resolution() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
        let explicit_path = Path::new("relative/debug.sock");

        assert_eq!(
            resolve_client_socket_path(Some(explicit_path))
                .expect("explicit socket path should be accepted unchanged"),
            explicit_path
        );
    }
}
