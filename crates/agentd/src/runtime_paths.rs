use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::protocol::{RequestMessage, ResponseMessage};

const SOCKET_BASENAME: &str = "agentd.sock";
const PID_BASENAME: &str = "agentd.pid";
const REQUIRED_TMP_MODE: u32 = 0o700;
const SOCKET_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

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
pub(crate) struct TemporaryRuntimeDirError {
    path: PathBuf,
    message: String,
}

impl TemporaryRuntimeDirError {
    fn new(path: &Path, message: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }
}

impl fmt::Display for TemporaryRuntimeDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.message, self.path.display())
    }
}

impl std::error::Error for TemporaryRuntimeDirError {}

impl From<TemporaryRuntimeDirError> for ClientSocketPathError {
    fn from(error: TemporaryRuntimeDirError) -> Self {
        Self::InsecureTemporaryRuntimeDir {
            path: error.path,
            message: error.message,
        }
    }
}

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
            tmp_runtime_dir: tmp_runtime_dir_for_uid(uid),
            system_runtime_dir: PathBuf::from("/run/agentd"),
        }
    }

    fn client_socket_path(&self) -> Result<PathBuf, ClientSocketPathError> {
        if let Some(runtime_dir) = &self.xdg_runtime_dir {
            let candidate = runtime_dir.join("agentd").join(SOCKET_BASENAME);
            if socket_candidate_is_ready(&candidate) {
                return Ok(candidate);
            }
        }

        if self.uid != 0 {
            let tmp_socket_path = self.tmp_runtime_dir.join(SOCKET_BASENAME);
            match fs::symlink_metadata(&self.tmp_runtime_dir) {
                Ok(_) => {
                    validate_tmp_runtime_dir(&self.tmp_runtime_dir, self.uid)?;
                    if socket_candidate_is_ready(&tmp_socket_path) {
                        return Ok(tmp_socket_path);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(TemporaryRuntimeDirError::new(
                        &self.tmp_runtime_dir,
                        format!("failed to inspect temporary runtime directory ({error})"),
                    )
                    .into());
                }
            }
        }

        let system_socket_path = self.system_runtime_dir.join(SOCKET_BASENAME);
        if socket_candidate_is_ready(&system_socket_path) {
            return Ok(system_socket_path);
        }

        Ok(system_socket_path)
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

fn socket_candidate_is_ready(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }

    match UnixStream::connect(path) {
        Ok(stream) => socket_candidate_answers_agentd_ping(stream),
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOENT) | Some(libc::ECONNREFUSED)
            ) =>
        {
            false
        }
        Err(_) => false,
    }
}

fn socket_candidate_answers_agentd_ping(mut stream: UnixStream) -> bool {
    if stream.set_read_timeout(Some(SOCKET_PROBE_TIMEOUT)).is_err() {
        return false;
    }

    if stream
        .set_write_timeout(Some(SOCKET_PROBE_TIMEOUT))
        .is_err()
    {
        return false;
    }

    let payload = match serde_json::to_vec(&RequestMessage::Ping) {
        Ok(payload) => payload,
        Err(_) => return false,
    };

    if stream.write_all(&payload).is_err()
        || stream.write_all(b"\n").is_err()
        || stream.flush().is_err()
    {
        return false;
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => false,
        Ok(_) => matches!(serde_json::from_str(&line), Ok(ResponseMessage::Pong)),
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

pub(crate) fn current_user_tmp_runtime_dir() -> PathBuf {
    tmp_runtime_dir_for_uid(unsafe { libc::geteuid() })
}

pub(crate) fn ensure_tmp_runtime_dir(
    path: &Path,
    expected_uid: u32,
) -> Result<(), TemporaryRuntimeDirError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_tmp_runtime_dir(path, expected_uid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|create_error| {
                TemporaryRuntimeDirError::new(
                    path,
                    format!("failed to create temporary runtime directory ({create_error})"),
                )
            })?;
            let mut permissions = fs::metadata(path)
                .map_err(|metadata_error| {
                    TemporaryRuntimeDirError::new(
                        path,
                        format!(
                            "failed to inspect created temporary runtime directory ({metadata_error})"
                        ),
                    )
                })?
                .permissions();
            permissions.set_mode(REQUIRED_TMP_MODE);
            fs::set_permissions(path, permissions).map_err(|permission_error| {
                TemporaryRuntimeDirError::new(
                    path,
                    format!(
                        "failed to set mode 0700 on temporary runtime directory ({permission_error})"
                    ),
                )
            })?;
            validate_tmp_runtime_dir(path, expected_uid)
        }
        Err(error) => Err(TemporaryRuntimeDirError::new(
            path,
            format!("failed to inspect temporary runtime directory ({error})"),
        )),
    }
}

fn tmp_runtime_dir_for_uid(uid: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/agentd-{uid}"))
}

fn validate_tmp_runtime_dir(
    path: &Path,
    expected_uid: u32,
) -> Result<(), TemporaryRuntimeDirError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        TemporaryRuntimeDirError::new(
            path,
            format!("failed to inspect temporary runtime directory ({error})"),
        )
    })?;

    if !metadata.is_dir() {
        return Err(TemporaryRuntimeDirError::new(
            path,
            "temporary runtime path exists but is not a directory",
        ));
    }

    let mode = metadata.mode() & 0o777;
    if mode != REQUIRED_TMP_MODE {
        return Err(TemporaryRuntimeDirError::new(
            path,
            format!(
                "temporary runtime directory must have mode 0700, found {:04o}",
                mode
            ),
        ));
    }

    let owner = metadata.uid();
    if owner != expected_uid {
        return Err(TemporaryRuntimeDirError::new(
            path,
            format!(
                "temporary runtime directory must be owned by uid {expected_uid}, found uid {owner}"
            ),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::thread;
    use std::time::Duration;

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

    fn spawn_agentd_ping_responder(path: &Path) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(path).expect("agentd responder socket should bind");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("probe connection should arrive");
            let mut line = String::new();
            BufReader::new(&mut stream)
                .read_line(&mut line)
                .expect("ping should be readable");
            assert_eq!(line, "{\"type\":\"ping\"}\n");
            stream
                .write_all(b"{\"type\":\"pong\"}\n")
                .expect("pong should be writable");
        })
    }

    fn spawn_wrong_protocol_responder(path: &Path) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(path).expect("wrong-protocol socket should bind");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("probe connection should arrive");
            let mut line = String::new();
            let _ = BufReader::new(&mut stream).read_line(&mut line);
            let _ = stream.write_all(b"{\"type\":\"error\",\"message\":\"not agentd\"}\n");
        })
    }

    fn spawn_silent_responder(path: &Path) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(path).expect("silent socket should bind");
        thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("probe connection should arrive");
            thread::sleep(Duration::from_millis(500));
        })
    }

    #[test]
    fn client_socket_path_prefers_xdg_runtime_dir_when_it_answers_agentd_ping() {
        let root = unique_dir("client-xdg");
        let xdg_runtime_dir = root.join("xdg-runtime");
        let xdg_agentd_dir = xdg_runtime_dir.join("agentd");
        std::fs::create_dir_all(&xdg_agentd_dir).expect("xdg runtime dir should be created");
        let responder = spawn_agentd_ping_responder(&xdg_agentd_dir.join("agentd.sock"));

        let layout = test_layout(
            1000,
            Some(xdg_runtime_dir),
            PathBuf::from("/tmp/agentd-1000"),
            PathBuf::from("/run/agentd"),
        );

        assert_eq!(
            layout
                .client_socket_path()
                .expect("xdg path should resolve"),
            xdg_agentd_dir.join("agentd.sock")
        );
        responder.join().expect("agentd responder should finish");
    }

    #[test]
    fn client_socket_path_falls_through_to_tmp_when_xdg_socket_is_missing() {
        let xdg_root = unique_dir("client-xdg-fallback-to-tmp-xdg");
        let xdg_runtime_dir = xdg_root.join("xdg-runtime");
        std::fs::create_dir_all(xdg_runtime_dir.join("agentd"))
            .expect("xdg runtime dir should be created");
        let root = unique_dir("client-xdg-fallback-to-tmp");
        let tmp_runtime_dir = root.join("tmp-runtime");
        std::fs::create_dir(&tmp_runtime_dir).expect("tmp runtime dir should be created");
        std::fs::set_permissions(&tmp_runtime_dir, std::fs::Permissions::from_mode(0o700))
            .expect("permissions should be set");
        let responder = spawn_agentd_ping_responder(&tmp_runtime_dir.join("agentd.sock"));

        let uid = unsafe { libc::geteuid() };
        let layout = test_layout(
            uid,
            Some(xdg_runtime_dir),
            tmp_runtime_dir.clone(),
            root.join("run-agentd"),
        );

        assert_eq!(
            layout
                .client_socket_path()
                .expect("tmp path should resolve"),
            tmp_runtime_dir.join("agentd.sock")
        );
        responder.join().expect("tmp responder should finish");
    }

    #[test]
    fn client_socket_path_ignores_insecure_tmp_dir_when_xdg_socket_is_available() {
        let root = unique_dir("xdg-ignore-bad-tmp");
        let xdg_runtime_dir = root.join("xdg-runtime");
        let xdg_agentd_dir = xdg_runtime_dir.join("agentd");
        std::fs::create_dir_all(&xdg_agentd_dir).expect("xdg runtime dir should be created");
        let responder = spawn_agentd_ping_responder(&xdg_agentd_dir.join("agentd.sock"));

        let tmp_runtime_dir = root.join("tmp-runtime");
        std::fs::create_dir(&tmp_runtime_dir).expect("tmp runtime dir should be created");
        std::fs::set_permissions(&tmp_runtime_dir, std::fs::Permissions::from_mode(0o755))
            .expect("permissions should be set");

        let uid = unsafe { libc::geteuid() };
        let layout = test_layout(
            uid,
            Some(xdg_runtime_dir),
            tmp_runtime_dir,
            root.join("run-agentd"),
        );

        assert_eq!(
            layout
                .client_socket_path()
                .expect("xdg path should resolve before tmp validation"),
            xdg_agentd_dir.join("agentd.sock")
        );
        responder.join().expect("agentd responder should finish");
    }

    #[test]
    fn client_socket_path_prefers_xdg_runtime_dir_for_root_when_present() {
        let root = unique_dir("client-root-xdg");
        let xdg_runtime_dir = root.join("xdg-runtime");
        let xdg_agentd_dir = xdg_runtime_dir.join("agentd");
        std::fs::create_dir_all(&xdg_agentd_dir).expect("xdg runtime dir should be created");
        let responder = spawn_agentd_ping_responder(&xdg_agentd_dir.join("agentd.sock"));

        let tmp_runtime_dir = root.join("tmp-runtime");
        std::fs::create_dir(&tmp_runtime_dir).expect("tmp runtime dir should be created");
        std::fs::set_permissions(&tmp_runtime_dir, std::fs::Permissions::from_mode(0o755))
            .expect("permissions should be set");

        let layout = test_layout(
            0,
            Some(xdg_runtime_dir),
            tmp_runtime_dir,
            root.join("run-agentd"),
        );

        assert_eq!(
            layout
                .client_socket_path()
                .expect("xdg path should resolve before root system fallback"),
            xdg_agentd_dir.join("agentd.sock")
        );
        responder.join().expect("agentd responder should finish");
    }

    #[test]
    fn client_socket_path_falls_through_to_system_when_earlier_candidates_are_unavailable() {
        let root = unique_dir("client-system-fallback");
        let xdg_runtime_dir = root.join("xdg-runtime");
        let xdg_agentd_dir = xdg_runtime_dir.join("agentd");
        std::fs::create_dir_all(&xdg_agentd_dir).expect("xdg runtime dir should be created");
        let xdg_listener =
            UnixListener::bind(xdg_agentd_dir.join("agentd.sock")).expect("xdg socket should bind");
        drop(xdg_listener);

        let tmp_runtime_dir = root.join("tmp-runtime");
        std::fs::create_dir(&tmp_runtime_dir).expect("tmp runtime dir should be created");
        std::fs::set_permissions(&tmp_runtime_dir, std::fs::Permissions::from_mode(0o700))
            .expect("permissions should be set");
        let tmp_listener = UnixListener::bind(tmp_runtime_dir.join("agentd.sock"))
            .expect("tmp socket should bind");
        drop(tmp_listener);

        let system_runtime_dir = root.join("run-agentd");
        std::fs::create_dir_all(&system_runtime_dir).expect("system runtime dir should be created");
        let responder = spawn_agentd_ping_responder(&system_runtime_dir.join("agentd.sock"));

        let uid = unsafe { libc::geteuid() };
        let layout = test_layout(
            uid,
            Some(xdg_runtime_dir),
            tmp_runtime_dir,
            system_runtime_dir.clone(),
        );

        assert_eq!(
            layout
                .client_socket_path()
                .expect("system path should resolve"),
            system_runtime_dir.join("agentd.sock")
        );
        responder.join().expect("system responder should finish");
    }

    #[test]
    fn client_socket_path_falls_through_when_xdg_socket_does_not_speak_agentd() {
        let root = unique_dir("xdg-wrong");
        let xdg_runtime_dir = root.join("xdg-runtime");
        let xdg_agentd_dir = xdg_runtime_dir.join("agentd");
        std::fs::create_dir_all(&xdg_agentd_dir).expect("xdg runtime dir should be created");
        let xdg_responder = spawn_wrong_protocol_responder(&xdg_agentd_dir.join("agentd.sock"));

        let system_runtime_dir = root.join("run-agentd");
        std::fs::create_dir_all(&system_runtime_dir).expect("system runtime dir should be created");
        let system_responder = spawn_agentd_ping_responder(&system_runtime_dir.join("agentd.sock"));

        let layout = test_layout(
            0,
            Some(xdg_runtime_dir),
            root.join("tmp-runtime"),
            system_runtime_dir.clone(),
        );

        assert_eq!(
            layout
                .client_socket_path()
                .expect("system path should resolve after wrong protocol"),
            system_runtime_dir.join("agentd.sock")
        );
        xdg_responder
            .join()
            .expect("wrong-protocol responder should finish");
        system_responder
            .join()
            .expect("system responder should finish");
    }

    #[test]
    fn client_socket_path_falls_through_when_xdg_socket_does_not_answer_ping() {
        let root = unique_dir("xdg-silent");
        let xdg_runtime_dir = root.join("xdg-runtime");
        let xdg_agentd_dir = xdg_runtime_dir.join("agentd");
        std::fs::create_dir_all(&xdg_agentd_dir).expect("xdg runtime dir should be created");
        let xdg_responder = spawn_silent_responder(&xdg_agentd_dir.join("agentd.sock"));

        let system_runtime_dir = root.join("run-agentd");
        std::fs::create_dir_all(&system_runtime_dir).expect("system runtime dir should be created");
        let system_responder = spawn_agentd_ping_responder(&system_runtime_dir.join("agentd.sock"));

        let layout = test_layout(
            0,
            Some(xdg_runtime_dir),
            root.join("tmp-runtime"),
            system_runtime_dir.clone(),
        );

        assert_eq!(
            layout
                .client_socket_path()
                .expect("system path should resolve after silent socket"),
            system_runtime_dir.join("agentd.sock")
        );
        xdg_responder
            .join()
            .expect("silent responder should finish");
        system_responder
            .join()
            .expect("system responder should finish");
    }

    #[test]
    fn client_socket_path_falls_through_when_candidate_connect_error_is_unclassified() {
        let root = unique_dir("xdg-unknown");
        let xdg_runtime_dir = root.join("x".repeat(96));
        let xdg_agentd_dir = xdg_runtime_dir.join("agentd");
        std::fs::create_dir_all(&xdg_agentd_dir).expect("xdg runtime dir should be created");
        let xdg_socket_path = xdg_agentd_dir.join("agentd.sock");
        std::fs::write(&xdg_socket_path, "").expect("candidate path should exist");

        let system_runtime_dir = root.join("run-agentd");
        std::fs::create_dir_all(&system_runtime_dir).expect("system runtime dir should be created");
        let system_responder = spawn_agentd_ping_responder(&system_runtime_dir.join("agentd.sock"));

        let layout = test_layout(
            0,
            Some(xdg_runtime_dir),
            root.join("tmp-runtime"),
            system_runtime_dir.clone(),
        );

        assert_eq!(
            layout
                .client_socket_path()
                .expect("system path should resolve after unclassified connect error"),
            system_runtime_dir.join("agentd.sock")
        );
        system_responder
            .join()
            .expect("system responder should finish");
    }

    #[test]
    fn client_socket_path_rejects_insecure_tmp_dir_before_falling_through_to_system() {
        let root = unique_dir("sys-reject-bad-tmp");
        let tmp_runtime_dir = root.join("tmp-runtime");
        std::fs::create_dir(&tmp_runtime_dir).expect("tmp runtime dir should be created");
        std::fs::set_permissions(&tmp_runtime_dir, std::fs::Permissions::from_mode(0o700))
            .expect("permissions should be set");
        let system_runtime_dir = root.join("run-agentd");
        std::fs::create_dir_all(&system_runtime_dir).expect("system runtime dir should be created");

        let layout = test_layout(
            unsafe { libc::geteuid() + 1 },
            None,
            tmp_runtime_dir.clone(),
            system_runtime_dir,
        );

        let error = layout
            .client_socket_path()
            .expect_err("insecure tmp runtime dir should halt discovery");
        assert!(error.to_string().contains("owned by uid"));
        assert!(
            error
                .to_string()
                .contains(tmp_runtime_dir.to_string_lossy().as_ref())
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
    fn client_socket_path_skips_tmp_validation_for_root_without_xdg() {
        let root = unique_dir("client-root-skip-insecure-tmp");
        let tmp_runtime_dir = root.join("tmp-runtime");
        std::fs::create_dir(&tmp_runtime_dir).expect("tmp runtime dir should be created");
        std::fs::set_permissions(&tmp_runtime_dir, std::fs::Permissions::from_mode(0o755))
            .expect("permissions should be set");

        let system_runtime_dir = root.join("run-agentd");
        std::fs::create_dir_all(&system_runtime_dir).expect("system runtime dir should be created");
        let responder = spawn_agentd_ping_responder(&system_runtime_dir.join("agentd.sock"));

        let layout = test_layout(0, None, tmp_runtime_dir, system_runtime_dir.clone());

        assert_eq!(
            layout
                .client_socket_path()
                .expect("root should resolve the system socket"),
            system_runtime_dir.join("agentd.sock")
        );
        responder.join().expect("system responder should finish");
    }

    #[test]
    fn client_socket_path_returns_system_path_when_no_default_socket_is_available() {
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
    fn client_socket_path_ignores_tmp_socket_for_root_without_xdg() {
        let root = unique_dir("client-root-ignore-tmp-socket");
        let tmp_runtime_dir = root.join("tmp-runtime");
        std::fs::create_dir(&tmp_runtime_dir).expect("tmp runtime dir should be created");
        std::fs::set_permissions(&tmp_runtime_dir, std::fs::Permissions::from_mode(0o700))
            .expect("permissions should be set");
        let _tmp_listener = UnixListener::bind(tmp_runtime_dir.join("agentd.sock"))
            .expect("tmp runtime socket should bind");

        let system_runtime_dir = root.join("run-agentd");
        std::fs::create_dir_all(&system_runtime_dir).expect("system runtime dir should be created");
        let responder = spawn_agentd_ping_responder(&system_runtime_dir.join("agentd.sock"));

        let layout = test_layout(0, None, tmp_runtime_dir, system_runtime_dir.clone());

        assert_eq!(
            layout
                .client_socket_path()
                .expect("root should prefer the system socket"),
            system_runtime_dir.join("agentd.sock")
        );
        responder.join().expect("system responder should finish");
    }

    #[test]
    fn ensure_tmp_runtime_dir_creates_a_private_directory_when_missing() {
        let root = unique_dir("ensure-tmp-runtime-created");
        let tmp_runtime_dir = root.join("tmp-runtime");
        let uid = unsafe { libc::geteuid() };

        ensure_tmp_runtime_dir(&tmp_runtime_dir, uid)
            .expect("missing tmp runtime directory should be created");

        let metadata =
            std::fs::metadata(&tmp_runtime_dir).expect("tmp runtime metadata should be readable");
        assert!(metadata.is_dir(), "tmp runtime path should be a directory");
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o700,
            "tmp runtime directory should be created with mode 0700"
        );
    }

    #[test]
    fn ensure_tmp_runtime_dir_rejects_preexisting_directory_with_wrong_mode() {
        let root = unique_dir("ensure-tmp-runtime-bad-mode");
        let tmp_runtime_dir = root.join("tmp-runtime");
        std::fs::create_dir(&tmp_runtime_dir).expect("tmp runtime dir should be created");
        std::fs::set_permissions(&tmp_runtime_dir, std::fs::Permissions::from_mode(0o755))
            .expect("permissions should be set");

        let uid = unsafe { libc::geteuid() };
        let error = ensure_tmp_runtime_dir(&tmp_runtime_dir, uid)
            .expect_err("preexisting directory with wrong mode should be rejected");

        assert!(error.to_string().contains("mode 0700"));
        assert!(
            error
                .to_string()
                .contains(tmp_runtime_dir.to_string_lossy().as_ref())
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
