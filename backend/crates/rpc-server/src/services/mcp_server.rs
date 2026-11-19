//! Owns the child handles for managed local MCP HTTP servers.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use sqarvo_common::{Error, ErrorCode};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot, watch};
use tonic::Status;

use crate::error::to_status;
use crate::proto::services::v1::{McpServerConfig, McpServerStatus, mcp_server_status::State};

const HOST: &str = "127.0.0.1";
const PATH: &str = "/mcp";
const DEFAULT_PORT: u16 = 8765;
const PORT_IN_USE: &str = "mcp.http.port_in_use";
const FAILED: &str = "mcp.http.failed";
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(4);

#[derive(Default)]
struct Managed {
    stop: Option<oneshot::Sender<()>>,
    shutdown: bool,
}

/// Configuration and supervision for the MCP process owned by this backend.
pub struct McpServerManager {
    config_file: PathBuf,
    executable: Option<PathBuf>,
    operations: Mutex<Managed>,
    status: watch::Sender<McpServerStatus>,
}

impl McpServerManager {
    /// Create a manager for the current backend deployment.
    pub fn new(config_dir: &Path) -> Arc<Self> {
        let executable = if cfg!(target_os = "linux") {
            std::env::current_exe()
                .ok()
                .map(|path| path.with_file_name("sqarvo-mcp"))
                .and_then(|path| path.canonicalize().ok())
                .filter(|path| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        path.metadata().is_ok_and(|metadata| {
                            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                        })
                    }
                    #[cfg(not(unix))]
                    {
                        path.is_file()
                    }
                })
        } else {
            None
        };
        Arc::new(Self {
            executable,
            config_file: config_dir.join("mcp-http-port"),
            operations: Mutex::new(Managed::default()),
            status: watch::Sender::new(state(State::Stopped)),
        })
    }

    /// Latest confirmed managed process state.
    pub fn status(&self) -> McpServerStatus {
        self.status.borrow().clone()
    }

    /// Read next-launch configuration.
    pub async fn config(&self) -> Result<McpServerConfig, Status> {
        let path = self.config_file.clone();
        let port = tokio::task::spawn_blocking(move || read_port(&path))
            .await
            .map_err(|_| Status::internal("MCP HTTP 配置读取任务失败"))??;
        Ok(McpServerConfig {
            port: u32::from(port),
            host: HOST.into(),
            path: PATH.into(),
            authentication_required: true,
            stdio_command: self.executable.as_ref().map_or_else(String::new, |path| {
                path.to_str().unwrap_or_default().to_owned()
            }),
        })
    }

    /// Persist a valid port atomically while the managed server is stopped.
    pub async fn update(&self, port: u32) -> Result<McpServerConfig, Status> {
        let port = u16::try_from(port)
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| Status::invalid_argument("MCP HTTP 端口须在 1–65535 范围内"))?;
        let operation = self.operations.lock().await;
        if operation.shutdown || in_progress(self.status().state()) {
            return Err(Status::failed_precondition("请先停止 MCP HTTP 服务"));
        }
        let file = self.config_file.clone();
        tokio::task::spawn_blocking(move || write_port(&file, port))
            .await
            .map_err(|_| Status::internal("MCP HTTP 配置写入任务失败"))?
            .map_err(|_| Status::internal("无法保存 MCP HTTP 端口"))?;
        self.config().await
    }

    /// Request a supervised start. The status changes to Running only on child readiness.
    pub async fn start(&self) -> Result<McpServerStatus, Status> {
        let mut operation = self.operations.lock().await;
        if operation.shutdown {
            return Err(Status::failed_precondition("后端正在关闭"));
        }
        let current = self.status();
        if in_progress(current.state()) {
            return Ok(current);
        }
        let config = self.config().await?;
        let executable = self.executable()?;
        let child = Command::new(executable)
            .args([
                "serve",
                "--transport",
                "http",
                "--port",
                &config.port.to_string(),
                "--managed",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                self.status.send_replace(failed(FAILED));
                mcp_error(FAILED, "无法启动 MCP 伴生程序")
            })?;
        let (stop, stop_request) = oneshot::channel();
        operation.stop = Some(stop);
        let starting = state(State::Starting);
        self.status.send_replace(starting.clone());
        tokio::spawn(supervise(
            child,
            stop_request,
            self.status.clone(),
            config.port,
        ));
        Ok(starting)
    }

    /// Request a stop for the owned child only.
    pub async fn stop(&self) -> Result<McpServerStatus, Status> {
        let mut operation = self.operations.lock().await;
        let current = self.status();
        match current.state() {
            State::Starting | State::Running => {
                // Serialize the replacement with the supervisor's readiness transition.
                self.status.send_if_modified(|status| {
                    if matches!(status.state(), State::Starting | State::Running) {
                        *status = state(State::Stopping);
                        true
                    } else {
                        false
                    }
                });
                if let Some(stop) = operation.stop.take() {
                    let _ = stop.send(());
                }
                Ok(self.status())
            }
            State::Failed if current.error_code == PORT_IN_USE => Err(mcp_error(
                "mcp.http.external_listener",
                "当前端口由外部进程使用，不能由此后端停止",
            )),
            _ => Ok(current),
        }
    }

    /// Revoke or replace the keyring credential through the installed companion.
    pub async fn auth(&self, rotate: bool) -> Result<(), Status> {
        let _operation = self.operations.lock().await;
        let child = Command::new(self.executable()?)
            .args(["http-auth", if rotate { "rotate" } else { "revoke" }])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| mcp_error(FAILED, "无法启动 MCP 伴生程序"))?;
        let result = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await;
        match result {
            Ok(Ok(output)) if output.status.success() => Ok(()),
            _ => Err(mcp_error(
                "credential.unavailable",
                "无法更新 HTTP 凭据，请检查系统密钥环",
            )),
        }
    }

    /// Prevent subsequent starts and wait for the owned child to be reaped.
    pub async fn shutdown(&self) {
        self.operations.lock().await.shutdown = true;
        let mut status = self.status.subscribe();
        let _ = self.stop().await;
        let _ = tokio::time::timeout(
            SHUTDOWN_LIMIT + Duration::from_secs(1),
            status.wait_for(|status| !in_progress(status.state())),
        )
        .await;
    }

    fn executable(&self) -> Result<&Path, Status> {
        self.executable
            .as_deref()
            .ok_or_else(|| mcp_error("mcp.http.binary_unavailable", "找不到可执行的 sqarvo-mcp"))
    }
}

async fn supervise(
    mut child: Child,
    mut stop: oneshot::Receiver<()>,
    status: watch::Sender<McpServerStatus>,
    port: u32,
) {
    // Child::wait closes the stdin still attached to Child. Own the control pipe
    // separately so waiting for unexpected exit does not request a normal stop.
    let stdin = child.stdin.take();
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill().await;
        status.send_replace(failed(FAILED));
        return;
    };
    let mut startup = Vec::with_capacity(96);
    let mut stdout = BufReader::new(stdout).take(96);
    let ready = tokio::select! {
        biased;
        _ = &mut stop => {
            drop(stdin);
            stop_child(child).await;
            status.send_replace(state(State::Stopped));
            return;
        }
        result = tokio::time::timeout(Duration::from_secs(8), stdout.read_until(b'\n', &mut startup)) => {
            matches!(result, Ok(Ok(_))) && startup == b"ready\n"
        }
    };
    if !ready {
        let _ = child.kill().await;
        let error = match startup.as_slice() {
            b"error mcp.http.port_in_use\n" => PORT_IN_USE,
            b"error credential.unavailable\n" | b"error credential.not_found\n" => {
                "credential.unavailable"
            }
            _ => FAILED,
        };
        status.send_replace(failed(error));
        return;
    }
    status.send_if_modified(|status| {
        if status.state() == State::Starting {
            *status = McpServerStatus {
                state: State::Running as i32,
                url: format!("http://{HOST}:{port}{PATH}"),
                error_code: String::new(),
            };
            true
        } else {
            false
        }
    });
    tokio::select! {
        biased;
        _ = &mut stop => {
            drop(stdin);
            stop_child(child).await;
            status.send_replace(state(State::Stopped));
        }
        _ = child.wait() => {
            status.send_replace(failed(FAILED));
        }
    }
}

async fn stop_child(mut child: Child) {
    if !matches!(
        tokio::time::timeout(SHUTDOWN_LIMIT, child.wait()).await,
        Ok(Ok(_))
    ) {
        let _ = child.kill().await;
    }
}

fn mcp_error(code: &'static str, message: &'static str) -> Status {
    to_status(Error::failed_precondition(
        ErrorCode::from_static(code),
        message,
    ))
}

fn state(state: State) -> McpServerStatus {
    McpServerStatus {
        state: state as i32,
        ..Default::default()
    }
}

fn failed(code: &str) -> McpServerStatus {
    McpServerStatus {
        error_code: code.into(),
        ..state(State::Failed)
    }
}

fn in_progress(state: State) -> bool {
    matches!(state, State::Starting | State::Running | State::Stopping)
}

fn read_port(path: &Path) -> Result<u16, Status> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(DEFAULT_PORT),
        Err(_) => return Err(Status::internal("无法读取 MCP HTTP 端口")),
    };
    let mut text = String::new();
    file.take(16)
        .read_to_string(&mut text)
        .map_err(|_| Status::internal("无法读取 MCP HTTP 端口"))?;
    text.trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| Status::failed_precondition("MCP HTTP 端口配置无效"))
}

fn write_port(path: &Path, port: u16) -> std::io::Result<()> {
    let dir = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    writeln!(file, "{port}")?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[tokio::test]
    async fn waiting_for_an_owned_child_does_not_close_its_control_pipe() {
        let folder = tempfile::tempdir().unwrap();
        let script = folder.path().join("mcp-fixture");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'ready\\n'\nread -r ignored || true\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut manager = McpServerManager::new(folder.path());
        Arc::get_mut(&mut manager).unwrap().executable = Some(script);
        assert_eq!(
            manager.config().await.unwrap().port,
            u32::from(DEFAULT_PORT)
        );
        assert!(manager.update(0).await.is_err());
        assert_eq!(manager.update(18765).await.unwrap().port, 18765);
        let mut status = manager.status.subscribe();
        assert_eq!(manager.start().await.unwrap().state(), State::Starting);
        tokio::time::timeout(
            Duration::from_secs(3),
            status.wait_for(|s| s.state() == State::Running),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(manager.start().await.unwrap().state(), State::Running);
        assert!(manager.update(18766).await.is_err());
        assert!(
            tokio::time::timeout(
                Duration::from_millis(150),
                status.wait_for(|s| s.state() == State::Failed)
            )
            .await
            .is_err()
        );
        manager.shutdown().await;
        assert_eq!(manager.status().state(), State::Stopped);
        assert!(manager.start().await.is_err());
    }
}
