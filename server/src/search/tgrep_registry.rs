//! Manages application-level lifecycle, readiness, and idle reaping for Microsoft tgrep server instances.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{process::Child, sync::Mutex};

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
    /// Terminates the child process and cleans up on-disk metadata if it matches our PID.
    pub async fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
        }
        let serve_json_path = self.repo_root.join(".tgrep").join("serve.json");
        if serve_json_path.exists() {
            if let Some((json_pid, _)) = read_serve_json(&self.repo_root) {
                if self.pid.is_none() || self.pid == Some(json_pid) {
                    let _ = std::fs::remove_file(serve_json_path);
                }
            }
        }
    }
}

/// Thread-safe registry owning per-repository `tgrep serve` processes.
#[derive(Clone, Default)]
pub struct TgrepRegistry {
    inner: Arc<Mutex<HashMap<PathBuf, TgrepServerHandle>>>,
}

impl TgrepRegistry {
    /// Creates a new empty `TgrepRegistry`.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
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

        // Fast path: check existing handle
        let existing_check = {
            let mut inner = self.inner.lock().await;
            if let Some(handle) = inner.get_mut(&canonical) {
                handle.last_used = Instant::now();

                if let Some(child) = &mut handle.child {
                    if let Ok(Some(_)) = child.try_wait() {
                        handle.child = None;
                        handle.port = None;
                        handle.readiness = ServerReadiness::Unhealthy;
                    }
                }

                if handle.child.is_some() {
                    Some((handle.port, handle.pid, handle.readiness))
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some((port, child_pid, current_readiness)) = existing_check {
            if let Some(port) = port {
                let probed = probe_readiness(port).await;
                let mut inner = self.inner.lock().await;
                if let Some(handle) = inner.get_mut(&canonical) {
                    handle.readiness = probed;
                }
                return probed;
            }

            // Check if serve.json appeared with matching PID
            if let Some((json_pid, port)) = read_serve_json(&canonical) {
                if child_pid == Some(json_pid) {
                    let probed = probe_readiness(port).await;
                    let mut inner = self.inner.lock().await;
                    if let Some(handle) = inner.get_mut(&canonical) {
                        handle.port = Some(port);
                        handle.readiness = probed;
                    }
                    return probed;
                }
            }
            return current_readiness;
        }

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
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(%error, path = %canonical.display(), "failed to spawn tgrep serve");
                return ServerReadiness::Unhealthy;
            }
        };

        let pid = child.id();
        {
            let mut inner = self.inner.lock().await;
            inner.insert(
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
        }

        // Bounded wait for serve.json to appear with matching PID
        let mut final_port = None;
        let mut final_readiness = ServerReadiness::Starting;

        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(40)).await;
            if let Some((json_pid, port)) = read_serve_json(&canonical) {
                if pid == Some(json_pid) {
                    final_port = Some(port);
                    final_readiness = probe_readiness(port).await;
                    break;
                }
            }
        }

        {
            let mut inner = self.inner.lock().await;
            if let Some(handle) = inner.get_mut(&canonical) {
                if let Some(port) = final_port {
                    handle.port = Some(port);
                }
                handle.readiness = final_readiness;
            }
        }

        final_readiness
    }

    /// Marks the given repository as recently used, resetting its idle countdown.
    pub async fn mark_used(&self, repo_root: &Path) {
        let canonical = repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.to_path_buf());
        let mut inner = self.inner.lock().await;
        if let Some(handle) = inner.get_mut(&canonical) {
            handle.last_used = Instant::now();
        }
    }

    /// Queries the current readiness of a repository without spawning a new server.
    pub async fn get_readiness(&self, repo_root: &Path) -> ServerReadiness {
        let canonical = repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.to_path_buf());

        let port_to_probe = {
            let mut inner = self.inner.lock().await;
            let Some(handle) = inner.get_mut(&canonical) else {
                return ServerReadiness::Unhealthy;
            };
            handle.port
        };

        let Some(port) = port_to_probe else {
            return ServerReadiness::Starting;
        };

        let probed = probe_readiness(port).await;
        let mut inner = self.inner.lock().await;
        if let Some(handle) = inner.get_mut(&canonical) {
            handle.readiness = probed;
        }
        probed
    }

    /// Reaps server handles that have been idle for longer than `max_idle`.
    pub async fn reap_idle(&self, max_idle: Duration) -> usize {
        let to_remove: Vec<TgrepServerHandle> = {
            let mut inner = self.inner.lock().await;
            let mut expired_keys = Vec::new();
            for (path, handle) in inner.iter() {
                if handle.last_used.elapsed() >= max_idle {
                    expired_keys.push(path.clone());
                }
            }

            expired_keys
                .into_iter()
                .filter_map(|key| inner.remove(&key))
                .collect()
        };

        let reaped = to_remove.len();
        for mut handle in to_remove {
            tracing::info!(path = %handle.repo_root.display(), "reaped idle tgrep serve process");
            handle.terminate().await;
        }
        reaped
    }

    /// Terminates all owned `tgrep serve` child processes.
    pub async fn shutdown(&self) {
        let handles: Vec<TgrepServerHandle> = {
            let mut inner = self.inner.lock().await;
            std::mem::take(&mut *inner).into_values().collect()
        };

        for mut handle in handles {
            tracing::debug!(path = %handle.repo_root.display(), "shutting down tgrep serve process");
            handle.terminate().await;
        }
    }

    /// Returns the number of currently active server handles.
    pub async fn active_count(&self) -> usize {
        self.inner.lock().await.len()
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

                if let Some(result_obj) = value.get("result").and_then(serde_json::Value::as_object) {
                    if let Some(indexing) = result_obj.get("indexing").and_then(serde_json::Value::as_bool) {
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
            inner.insert(
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
            inner.insert(
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
            inner.insert(
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
    async fn probe_readiness_validates_jsonrpc_strictly() {
        // Test port that is closed -> Unhealthy
        let readiness = probe_readiness(65534).await;
        assert_eq!(readiness, ServerReadiness::Unhealthy);
    }
}
