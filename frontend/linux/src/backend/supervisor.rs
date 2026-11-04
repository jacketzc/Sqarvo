//! 后端进程生命周期：查找可执行文件、按需启动、等待端点描述、退出清理。
//!
//! 设计取舍：
//!
//! - **只清理自己启动的后端**。若端点描述来自别处（例如用户手动启动的后端），
//!   前端退出时既不发送信号，也不删除端点文件或 socket。
//! - 协议主版本不兼容时**不启动**新后端：多一个进程无法修复版本错配，只会
//!   掩盖问题、并可能与正在运行的后端争抢 socket。此时直接进入错误状态并提示
//!   用户升级。
//! - 端点文件里的 pid 已死（崩溃残留）视为陈旧端点，启动新后端覆盖它。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::backend::endpoint::{self, EndpointInfo};
use crate::backend::error::BackendError;

/// 覆盖后端可执行文件路径的环境变量。
pub const BACKEND_BIN_ENV: &str = "SQARVO_BACKEND_BIN";

/// 后端可执行文件名（package 名是 `sqarvo-backend-service`，二进制名是 `sqarvo-backend`）。
const BACKEND_BINARY_NAME: &str = "sqarvo-backend";

/// 等待端点描述文件出现的上限。
const ENDPOINT_TIMEOUT: Duration = Duration::from_secs(10);

/// 轮询端点描述文件的间隔。
const ENDPOINT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 等待后端响应 SIGTERM 的上限，超时后 SIGKILL。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// 握手的阶段，用于在「正在连接」界面展示进度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 读取端点描述文件。
    ReadingEndpoint,
    /// 校验协议版本与后端进程。
    CheckingEndpoint,
    /// 启动后端进程。
    StartingBackend,
    /// 等待后端写出端点描述。
    WaitingEndpoint,
    /// 建立 gRPC 通道。
    Connecting,
    /// 调用 GetVersion 做版本核对。
    VerifyingVersion,
}

impl Stage {
    /// 展示给用户的文案。
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::ReadingEndpoint => "读取端点描述文件…",
            Self::CheckingEndpoint => "校验协议版本与后端进程…",
            Self::StartingBackend => "启动后端进程 sqarvo-backend…",
            Self::WaitingEndpoint => "等待后端就绪（最多 10 秒）…",
            Self::Connecting => "建立 gRPC 通道并附带本地会话 token…",
            Self::VerifyingVersion => "调用 SystemService.GetVersion 核对版本…",
        }
    }
}

/// 后端进程管理器（每个前端实例一个）。
pub struct BackendSupervisor {
    endpoint_path: PathBuf,
    child: Option<Child>,
    cancel: Arc<AtomicBool>,
}

impl BackendSupervisor {
    /// 创建管理器；`cancel` 置位时握手流程会尽快放弃。
    #[must_use]
    pub fn new(endpoint_path: PathBuf, cancel: Arc<AtomicBool>) -> Self {
        Self {
            endpoint_path,
            child: None,
            cancel,
        }
    }

    /// 端点描述文件路径（供界面展示）。
    #[must_use]
    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint_path
    }

    /// 后端进程是否由本前端启动。
    #[must_use]
    pub fn owns_backend(&self) -> bool {
        self.child.is_some()
    }

    /// 获取可用的端点描述：复用已运行的后端，或在必要时启动自己的后端。
    ///
    /// 顺序与 `frontend/README.md` 一致：读端点文件 → 校验协议主版本 →
    /// 校验 pid → （必要时）启动后端 → 轮询等待端点文件。
    pub async fn acquire(
        &mut self,
        progress: &(dyn Fn(Stage) + Send + Sync),
    ) -> Result<EndpointInfo, BackendError> {
        progress(Stage::ReadingEndpoint);
        self.ensure_not_cancelled()?;
        let existing = endpoint::read(&self.endpoint_path)?;

        if let Some(info) = existing {
            progress(Stage::CheckingEndpoint);
            // 协议不兼容是致命错误：不启动第二个后端，直接让用户升级。
            info.ensure_compatible()?;
            if endpoint::pid_alive(info.pid) {
                eprintln!(
                    "[sqarvo-desktop] 复用已运行的后端：{}",
                    info.redacted_summary()
                );
                return Ok(info);
            }
            eprintln!(
                "[sqarvo-desktop] 端点文件来自已退出的后端（pid {}），启动新后端",
                info.pid
            );
        }

        self.ensure_not_cancelled()?;
        // SQARVO_ENDPOINT_FILE 覆盖端点路径时，子进程必须写到同一目录，所以要同步
        // 它的 XDG_RUNTIME_DIR；不符合标准布局时直接报错，而不是白等 10 秒超时。
        let endpoint_override = std::env::var_os(endpoint::ENDPOINT_FILE_ENV).is_some();
        let child_runtime_root = endpoint::child_runtime_root(&self.endpoint_path);
        if endpoint_override && child_runtime_root.is_none() {
            return Err(BackendError::EndpointInvalid {
                path: self.endpoint_path.clone(),
                detail: format!(
                    "{} 需要指向 <runtime_root>/sqarvo/{}，前端才能让新启动的后端把端点写到这里",
                    endpoint::ENDPOINT_FILE_ENV,
                    endpoint::ENDPOINT_FILE_NAME,
                ),
            });
        }

        progress(Stage::StartingBackend);
        let binary = locate_binary()?;
        let mut command = Command::new(&binary);
        command.stdin(Stdio::null());
        if endpoint_override && let Some(root) = child_runtime_root {
            eprintln!(
                "[sqarvo-desktop] 端点路径被覆盖，子进程使用 XDG_RUNTIME_DIR={}",
                root.display()
            );
            command.env("XDG_RUNTIME_DIR", root);
        }
        // 保留后端日志（stderr）以便诊断；后端自身保证不输出 token。
        let child = command
            .spawn()
            .map_err(|error| BackendError::BackendSpawnFailed {
                binary: binary.clone(),
                detail: error.to_string(),
            })?;
        let child_pid = child.id();
        eprintln!(
            "[sqarvo-desktop] 已启动后端进程 pid={child_pid}：{}",
            binary.display()
        );
        self.child = Some(child);

        progress(Stage::WaitingEndpoint);
        match self.wait_for_endpoint(child_pid).await {
            Ok(info) => Ok(info),
            Err(error) => {
                // 失败时不要留下无人管理的后端进程。
                if let Err(cleanup) = self.shutdown().await {
                    eprintln!("[sqarvo-desktop] 启动失败后的清理未完成：{cleanup}");
                }
                Err(error)
            }
        }
    }

    /// 轮询等待自己启动的后端写出端点描述。
    async fn wait_for_endpoint(&mut self, child_pid: u32) -> Result<EndpointInfo, BackendError> {
        let deadline = Instant::now() + ENDPOINT_TIMEOUT;
        let mut last_error: Option<String> = None;

        loop {
            self.ensure_not_cancelled()?;

            // 后端先退出就不必等满超时。
            if let Some(child) = self.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        self.child = None;
                        return Err(BackendError::BackendExited {
                            status: status.to_string(),
                        });
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return Err(BackendError::Runtime {
                            detail: format!("无法查询后端进程 {child_pid} 状态：{error}"),
                        });
                    }
                }
            }

            match endpoint::read(&self.endpoint_path) {
                // 必须匹配自己启动的 pid：避免读到上一轮遗留的文件。
                Ok(Some(info)) if info.pid == child_pid => {
                    info.ensure_compatible()?;
                    eprintln!("[sqarvo-desktop] 后端已就绪：{}", info.redacted_summary());
                    return Ok(info);
                }
                Ok(_) => {}
                Err(error) => last_error = Some(error.short()),
            }

            if Instant::now() >= deadline {
                return Err(BackendError::EndpointTimeout {
                    path: self.endpoint_path.clone(),
                    waited: ENDPOINT_TIMEOUT,
                    last_error,
                });
            }
            tokio::time::sleep(ENDPOINT_POLL_INTERVAL).await;
        }
    }

    /// 关闭自己启动的后端：SIGTERM → 等待 → 超时 SIGKILL。
    ///
    /// 后端收到 SIGTERM 后自行删除 socket 与端点文件。若等待超时，返回
    /// [`BackendError::CleanupFailed`]，绝不假装清理成功。
    pub async fn shutdown(&mut self) -> Result<(), BackendError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let pid = child.id();

        // SAFETY: 只向本进程创建且尚未回收的子进程发送 SIGTERM；子进程未回收前
        // pid 不会被系统复用，因此不会误伤其他进程。
        let signal_result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if signal_result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(BackendError::CleanupFailed {
                    detail: format!("向进程 {pid} 发送 SIGTERM 失败：{error}"),
                });
            }
        }

        let deadline = Instant::now() + SHUTDOWN_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    eprintln!("[sqarvo-desktop] 后端进程 {pid} 已退出（{status}）");
                    return Ok(());
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(BackendError::CleanupFailed {
                        detail: format!("无法查询进程 {pid} 状态：{error}"),
                    });
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let _ = child.kill();
        let status = child.wait();
        Err(BackendError::CleanupFailed {
            detail: format!(
                "进程 {pid} 在 {} 秒内未响应 SIGTERM，已发送 SIGKILL（{status:?}）；\
                 socket 与端点文件可能残留",
                SHUTDOWN_GRACE.as_secs()
            ),
        })
    }

    fn ensure_not_cancelled(&self) -> Result<(), BackendError> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(BackendError::Cancelled);
        }
        Ok(())
    }
}

impl Drop for BackendSupervisor {
    fn drop(&mut self) {
        // 兜底：worker 线程异常结束时不留下孤儿后端。
        if let Some(mut child) = self.child.take() {
            eprintln!(
                "[sqarvo-desktop] 兜底终止后端进程 {}（SIGKILL）",
                child.id()
            );
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 按固定优先级查找后端可执行文件：
///
/// 1. `SQARVO_BACKEND_BIN`；
/// 2. 与前端可执行文件同目录；
/// 3. 仓库内相对路径 `backend/target/debug/sqarvo-backend`（相对可执行文件目录
///    与当前工作目录各尝试若干层上级）。
///
/// 找不到时返回带完整搜索列表的错误，而不是 panic。
pub fn locate_binary() -> Result<PathBuf, BackendError> {
    let mut searched = Vec::new();

    if let Some(raw) = std::env::var_os(BACKEND_BIN_ENV) {
        let candidate = PathBuf::from(&raw);
        searched.push(candidate.clone());
        if candidate.is_file() {
            return Ok(canonical(candidate));
        }
        // 显式指定却不可用：直接报错，避免悄悄回退到别的二进制。
        return Err(BackendError::BackendBinaryNotFound { searched });
    }

    let exe = std::env::current_exe().ok();
    if let Some(dir) = exe.as_deref().and_then(Path::parent) {
        let candidate = dir.join(BACKEND_BINARY_NAME);
        searched.push(candidate.clone());
        if candidate.is_file() {
            return Ok(canonical(candidate));
        }
    }

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe.as_deref().and_then(Path::parent) {
        roots.push(dir.to_path_buf());
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    for root in roots {
        for depth in ["..", "../..", "../../..", "../../../.."] {
            let candidate = root
                .join(depth)
                .join("backend/target/debug")
                .join(BACKEND_BINARY_NAME);
            if searched.contains(&candidate) {
                continue;
            }
            searched.push(candidate.clone());
            if candidate.is_file() {
                return Ok(canonical(candidate));
            }
        }
    }

    Err(BackendError::BackendBinaryNotFound { searched })
}

/// 统一成绝对路径，便于日志与错误提示阅读。
fn canonical(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_messages_are_chinese_and_stable() {
        assert!(Stage::ReadingEndpoint.message().contains("端点"));
        assert!(Stage::WaitingEndpoint.message().contains("10 秒"));
    }

    #[test]
    fn locate_binary_reports_searched_paths() {
        // 用不存在的 SQARVO_BACKEND_BIN 触发错误路径；环境变量只在测试内可见。
        let key = BACKEND_BIN_ENV;
        let previous = std::env::var_os(key);
        // SAFETY: 本用例串行修改进程环境变量。
        unsafe { std::env::set_var(key, "/nonexistent/sqarvo-backend") };
        let error = locate_binary().unwrap_err();
        let message = error.to_string();
        assert!(message.contains("SQARVO_BACKEND_BIN"));
        assert!(message.contains("/nonexistent/sqarvo-backend"));
        match previous {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}
