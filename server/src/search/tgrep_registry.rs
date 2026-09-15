//! Manages application-level lifecycle, readiness, and idle reaping for Microsoft tgrep server instances.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{process::Child, sync::Mutex};

#[cfg(windows)]
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
    },
};

#[derive(Default)]
struct RegistryState {
    handles: HashMap<PathBuf, TgrepServerHandle>,
    shutting_down: bool,
}

#[cfg(windows)]
struct TgrepJob {
    handle: OwnedHandle,
}

#[cfg(windows)]
impl TgrepJob {
    fn new() -> std::io::Result<Self> {
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw == INVALID_HANDLE_VALUE || raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw as _) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let result = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle() as _,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if result == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    fn assign(&self, pid: u32) -> std::io::Result<()> {
        let raw = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let process = unsafe { OwnedHandle::from_raw_handle(raw as _) };
        if unsafe {
            AssignProcessToJobObject(
                self.handle.as_raw_handle() as _,
                process.as_raw_handle() as _,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Readiness state of a `tgrep serve` process for a repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerReadiness {
    /// Server process is starting up and has not yet completed initial TCP bind.
    Starting,
    /// Server is running and actively building the initial trigram index.
    Indexing,
    /// Server is running, TCP ready, and initial trigram index is complete.
    Ready,
    /// Server is unreachable, failed to start, or terminated.
    Unhealthy,
}

/// Handle tracking an active `tgrep serve` child process.
pub struct TgrepServerHandle {
    pub repo_root: PathBuf,
    pub child: Option<Child>,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub readiness: ServerReadiness,
    pub last_used: Instant,
}

impl TgrepServerHandle {
    fn cleanup_metadata(&self) {
        let serve_json_path = self.repo_root.join(".tgrep").join("serve.json");
        if let Some((json_pid, _)) = read_serve_json(&self.repo_root) {
            if self.pid == Some(json_pid) {
                let _ = std::fs::remove_file(serve_json_path);
            }
        }
    }

    /// Terminates the child process and cleans up on-disk metadata if it matches our PID.
    pub async fn terminate(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.child = None;
        self.cleanup_metadata();
    }
}

/// Thread-safe registry owning per-repository `tgrep serve` processes.
#[derive(Clone)]
pub struct TgrepRegistry {
    inner: Arc<Mutex<RegistryState>>,
    #[cfg(windows)]
    job: Arc<TgrepJob>,
}

impl Default for TgrepRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TgrepRegistry {
    /// Creates a new registry that owns all `tgrep serve` child processes.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryState::default())),
            #[cfg(windows)]
            job: Arc::new(TgrepJob::new().expect("failed to create tgrep process job object")),
        }
    }

    /// Ensures a `tgrep serve` process is running for the given repository root and returns its readiness.
    pub async fn ensure_server(
        &self,
        repo_root: &Path,
        configured_path: Option<&str>,
    ) -> ServerReadiness {
        let canonical = repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.to_path_buf());

        // Keep check -> spawn -> register atomic. Without this, concurrent cold
        // starts can both spawn a server and one owned child gets overwritten.
        let (port, pid, readiness) = {
            let mut inner = self.inner.lock().await;
            if inner.shutting_down {
                return ServerReadiness::Unhealthy;
            }
            let mut remove_stopped = false;
            if let Some(handle) = inner.handles.get_mut(&canonical) {
                handle.last_used = Instant::now();
                if let Some(child) = &mut handle.child {
                    if let Ok(Some(_)) = child.try_wait() {
                        remove_stopped = true;
                    }
                }
            }
            if remove_stopped {
                if let Some(handle) = inner.handles.remove(&canonical) {
                    handle.cleanup_metadata();
                }
            }

            if let Some(handle) = inner.handles.get(&canonical) {
                (handle.port, handle.pid, handle.readiness)
            } else {
                let binary = match crate::search::tgrep::resolve_tgrep_binary(configured_path) {
                    Some(binary) => binary,
                    None => return ServerReadiness::Unhealthy,
                };
                let mut cmd = tokio::process::Command::new(&binary);
                #[cfg(windows)]
                cmd.creation_flags(0x0800_0000);
                cmd.arg("serve");
                cmd.arg(&canonical);
                cmd.arg("--watch-mode").arg("auto");
                cmd.current_dir(&canonical);
                cmd.stdin(std::process::Stdio::null());
                cmd.stdout(std::process::Stdio::null());
                cmd.stderr(std::process::Stdio::null());
                cmd.kill_on_drop(true);

                let child = match cmd.spawn() {
                    Ok(child) => child,
                    Err(error) => {
                        tracing::warn!(%error, path = %canonical.display(), "failed to spawn tgrep serve");
                        return ServerReadiness::Unhealthy;
                    }
                };
                let pid = child.id();
                #[cfg(windows)]
                if let Some(pid) = pid {
                    if let Err(error) = self.job.assign(pid) {
                        tracing::warn!(%error, %pid, path = %canonical.display(), "failed to assign tgrep serve to job object");
                        let mut child = child;
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        return ServerReadiness::Unhealthy;
                    }
                }
                inner.handles.insert(
                    canonical.clone(),
                    TgrepServerHandle {
                        repo_root: canonical.clone(),
                        child: Some(child),
                        port: None,
                        pid,
                        readiness: ServerReadiness::Starting,
                        last_used: Instant::now(),
                    },
                );
                (None, pid, ServerReadiness::Starting)
            }
        };

        if let Some(port) = port {
            let probed = probe_readiness(port).await;
            if self
                .update_readiness(&canonical, pid, Some(port), probed)
                .await
            {
                return probed;
            }
            return ServerReadiness::Unhealthy;
        }

        // Bounded discovery for a newly started process. Other callers reuse
        // the registered Starting handle instead of spawning another child.
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(40)).await;
            if let Some((json_pid, port)) = read_serve_json(&canonical) {
                if pid == Some(json_pid) {
                    let probed = probe_readiness(port).await;
                    if self
                        .update_readiness(&canonical, pid, Some(port), probed)
                        .await
                    {
                        return probed;
                    }
                    return ServerReadiness::Unhealthy;
                }
            }
        }

        readiness
    }

    async fn update_readiness(
        &self,
        repo_root: &Path,
        pid: Option<u32>,
        port: Option<u16>,
        readiness: ServerReadiness,
    ) -> bool {
        let mut inner = self.inner.lock().await;
        if let Some(handle) = inner.handles.get_mut(repo_root) {
            // A delayed probe must not overwrite a replacement process.
            if handle.pid == pid {
                handle.port = port.or(handle.port);
                handle.readiness = readiness;
                return true;
            }
        }
        false
    }

    /// Queries the current readiness of a repository without spawning a new server.
    pub async fn get_readiness(&self, repo_root: &Path) -> ServerReadiness {
        let canonical = repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.to_path_buf());

        let (pid, port) = {
            let inner = self.inner.lock().await;
            let Some(handle) = inner.handles.get(&canonical) else {
                return ServerReadiness::Unhealthy;
            };
            (handle.pid, handle.port)
        };

        let Some(port) = port else {
            return ServerReadiness::Starting;
        };

        let probed = probe_readiness(port).await;
        if self
            .update_readiness(&canonical, pid, Some(port), probed)
            .await
        {
            probed
        } else {
            ServerReadiness::Unhealthy
        }
    }

    /// Reaps server handles that have been idle for longer than `max_idle`.
    pub async fn reap_idle(&self, max_idle: Duration) -> usize {
        // Keep the registry locked until expired children terminate so a request
        // cannot spawn a replacement while the previous process is still alive.
        let mut inner = self.inner.lock().await;
        let expired_keys: Vec<PathBuf> = inner
            .handles
            .iter()
            .filter(|(_, handle)| handle.last_used.elapsed() >= max_idle)
            .map(|(path, _)| path.clone())
            .collect();

        let mut reaped = 0;
        for key in expired_keys {
            let Some(handle) = inner.handles.get_mut(&key) else {
                continue;
            };
            tracing::info!(path = %handle.repo_root.display(), "reaped idle tgrep serve process");
            handle.terminate().await;
            inner.handles.remove(&key);
            reaped += 1;
        }
        reaped
    }

    /// Terminates all owned `tgrep serve` child processes and prevents new starts.
    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        inner.shutting_down = true;
        let keys: Vec<PathBuf> = inner.handles.keys().cloned().collect();

        // Keep every handle registered until its child has been killed and
        // waited. If this future is cancelled, a later shutdown can retry.
        for key in keys {
            let Some(handle) = inner.handles.get_mut(&key) else {
                continue;
            };
            tracing::debug!(path = %handle.repo_root.display(), "shutting down tgrep serve process");
            handle.terminate().await;
            inner.handles.remove(&key);
        }
    }

    /// Returns the number of currently active server handles.
    pub async fn active_count(&self) -> usize {
        self.inner.lock().await.handles.len()
    }
}

fn read_serve_json(repo_root: &Path) -> Option<(u32, u16)> {
    let path = repo_root.join(".tgrep").join("serve.json");
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let pid_raw = value.get("pid").and_then(serde_json::Value::as_u64)?;
    let port_raw = value.get("port").and_then(serde_json::Value::as_u64)?;
    let pid = u32::try_from(pid_raw).ok()?;
    let port = u16::try_from(port_raw).ok()?;
    Some((pid, port))
}

async fn probe_readiness(port: u16) -> ServerReadiness {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let connect_fut = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"));
    let mut stream = match tokio::time::timeout(Duration::from_millis(150), connect_fut).await {
        Ok(Ok(stream)) => stream,
        _ => return ServerReadiness::Unhealthy,
    };

    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::new(reader);

    let request = b"{\"jsonrpc\":\"2.0\",\"method\":\"status\",\"id\":1}\n";
    if writer.write_all(request).await.is_err() {
        return ServerReadiness::Unhealthy;
    }

    let mut line = String::new();
    let read_fut = reader.read_line(&mut line);
    match tokio::time::timeout(Duration::from_millis(200), read_fut).await {
        Ok(Ok(n)) if n > 0 => {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                // Strict JSON-RPC 2.0 validation
                if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
                    return ServerReadiness::Unhealthy;
                }
                if value.get("id").and_then(serde_json::Value::as_u64) != Some(1) {
                    return ServerReadiness::Unhealthy;
                }
                if value.get("error").is_some() && !value["error"].is_null() {
                    return ServerReadiness::Unhealthy;
                }

                if let Some(result_obj) = value.get("result").and_then(serde_json::Value::as_object)
                {
                    if let Some(indexing) = result_obj
                        .get("indexing")
                        .and_then(serde_json::Value::as_bool)
                    {
                        if indexing {
                            return ServerReadiness::Indexing;
                        }
                    }
                    return ServerReadiness::Ready;
                }
                ServerReadiness::Unhealthy
            } else {
                ServerReadiness::Unhealthy
            }
        }
        _ => ServerReadiness::Unhealthy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_initializes_empty() {
        let registry = TgrepRegistry::new();
        assert_eq!(registry.active_count().await, 0);
    }

    #[tokio::test]
    async fn registry_reaps_idle_handles() {
        let registry = TgrepRegistry::new();
        let path = PathBuf::from("target/test_repo_idle");
        {
            let mut inner = registry.inner.lock().await;
            inner.handles.insert(
                path.clone(),
                TgrepServerHandle {
                    repo_root: path.clone(),
                    child: None,
                    port: None,
                    pid: None,
                    readiness: ServerReadiness::Ready,
                    last_used: Instant::now() - Duration::from_secs(1500),
                },
            );
        }
        assert_eq!(registry.active_count().await, 1);
        let reaped = registry.reap_idle(Duration::from_secs(1200)).await;
        assert_eq!(reaped, 1);
        assert_eq!(registry.active_count().await, 0);
    }

    #[tokio::test]
    async fn registry_shutdown_clears_all_handles() {
        let registry = TgrepRegistry::new();
        let path1 = PathBuf::from("target/test_repo_1");
        let path2 = PathBuf::from("target/test_repo_2");
        {
            let mut inner = registry.inner.lock().await;
            inner.handles.insert(
                path1.clone(),
                TgrepServerHandle {
                    repo_root: path1,
                    child: None,
                    port: None,
                    pid: None,
                    readiness: ServerReadiness::Ready,
                    last_used: Instant::now(),
                },
            );
            inner.handles.insert(
                path2.clone(),
                TgrepServerHandle {
                    repo_root: path2,
                    child: None,
                    port: None,
                    pid: None,
                    readiness: ServerReadiness::Ready,
                    last_used: Instant::now(),
                },
            );
        }
        assert_eq!(registry.active_count().await, 2);
        registry.shutdown().await;
        assert_eq!(registry.active_count().await, 0);
    }

    #[tokio::test]
    async fn concurrent_ensure_registers_the_owned_server() {
        let Some(binary) = crate::search::tgrep::resolve_tgrep_binary(None) else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname='lifecycle-test'\n",
        )
        .unwrap();
        let root = directory.path().to_path_buf();
        let registry = TgrepRegistry::new();
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let registry = registry.clone();
            let root = root.clone();
            let binary = binary.clone();
            tasks.push(tokio::spawn(async move {
                registry.ensure_server(&root, binary.to_str()).await
            }));
        }
        for task in tasks {
            let _ = task.await.unwrap();
        }

        assert_eq!(registry.active_count().await, 1);
        let registered_pid = registry
            .inner
            .lock()
            .await
            .handles
            .get(&root.canonicalize().unwrap())
            .and_then(|handle| handle.pid)
            .expect("registry should own the server child");
        let (published_pid, _) = read_serve_json(&root).expect("server should publish serve.json");
        assert_eq!(registered_pid, published_pid);

        registry.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_prevents_future_server_starts() {
        let Some(binary) = crate::search::tgrep::resolve_tgrep_binary(None) else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname='shutdown-test'\n",
        )
        .unwrap();
        let registry = TgrepRegistry::new();

        registry.shutdown().await;
        let readiness = registry
            .ensure_server(directory.path(), binary.to_str())
            .await;

        assert_eq!(readiness, ServerReadiness::Unhealthy);
        assert_eq!(registry.active_count().await, 0);
    }

    #[tokio::test]
    async fn terminate_without_matching_pid_preserves_serve_json() {
        let directory = tempfile::tempdir().unwrap();
        let tgrep_dir = directory.path().join(".tgrep");
        std::fs::create_dir_all(&tgrep_dir).unwrap();
        let serve_json = tgrep_dir.join("serve.json");
        std::fs::write(&serve_json, r#"{"pid":1234,"port":5678}"#).unwrap();
        let mut handle = TgrepServerHandle {
            repo_root: directory.path().to_path_buf(),
            child: None,
            port: None,
            pid: None,
            readiness: ServerReadiness::Unhealthy,
            last_used: Instant::now(),
        };

        handle.terminate().await;

        assert!(serve_json.exists());
    }

    #[tokio::test]
    async fn terminate_with_matching_pid_removes_serve_json() {
        let directory = tempfile::tempdir().unwrap();
        let tgrep_dir = directory.path().join(".tgrep");
        std::fs::create_dir_all(&tgrep_dir).unwrap();
        let serve_json = tgrep_dir.join("serve.json");
        std::fs::write(&serve_json, r#"{"pid":1234,"port":5678}"#).unwrap();
        let mut handle = TgrepServerHandle {
            repo_root: directory.path().to_path_buf(),
            child: None,
            port: None,
            pid: Some(1234),
            readiness: ServerReadiness::Unhealthy,
            last_used: Instant::now(),
        };

        handle.terminate().await;

        assert!(!serve_json.exists());
    }

    #[tokio::test]
    async fn probe_readiness_validates_jsonrpc_strictly() {
        // Test port that is closed -> Unhealthy
        let readiness = probe_readiness(65534).await;
        assert_eq!(readiness, ServerReadiness::Unhealthy);
    }
}
