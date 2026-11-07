//! 退出信号：`SIGTERM` / `SIGINT` / `SIGHUP` 与「关窗口」走同一条清理路径。
//!
//! # 为什么需要这一层
//!
//! 默认处置下这三个信号会立刻终止进程（前端来不及通知后端），被前端启动的
//! `sqarvo-backend` 就变成孤儿：端点 socket 与进程都留在系统里，下次启动还会
//! 出现两个后端（#146）。因此这里把信号变成**普通的界面动作**：
//!
//! - 有窗口 → `window.close()`：与用户点关闭按钮完全相同（`close_request` →
//!   worker 停后端、等 `ShutdownComplete`、删端点文件、再退出应用）；
//! - 没有窗口 → `app.quit()`：仍然经过 `connect_shutdown` 的清理兜底。
//!
//! # 为什么用独立线程 + 通道
//!
//! GTK/GLib 对象只能在主线程碰，信号却可能落在任意线程。`tokio::signal` 装的是
//! 进程级处理器（不依赖信号屏蔽字，也不会被别的线程的默认处置截走），我们在一个
//! 只跑信号的线程里等它，再通过 `async_channel` 送回 GTK 主线程——与后端 worker
//! 上报事件的方式一致。
//!
//! 日志只写信号名与动作，不含端点路径、token 或任何凭据。

use adw::prelude::*;
use gtk::glib;
use tokio::signal::unix::{Signal, SignalKind, signal};

/// 监视的三个退出信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watched {
    Terminate,
    Interrupt,
    Hangup,
}

impl Watched {
    /// 日志里的信号名（与 `kill` 命令一致，便于对账）。
    #[must_use]
    fn name(self) -> &'static str {
        match self {
            Self::Terminate => "SIGTERM",
            Self::Interrupt => "SIGINT",
            Self::Hangup => "SIGHUP",
        }
    }
}

/// 收到退出信号后的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitAction {
    /// 有关闭窗口可用：走与点关闭按钮相同的清理路径。
    CloseWindow,
    /// 没有窗口：直接退出应用，仍经过 `connect_shutdown` 的清理。
    Quit,
}

/// 有窗口就关窗口，没有才直接退出。
#[must_use]
pub fn action_for(has_window: bool) -> ExitAction {
    if has_window {
        ExitAction::CloseWindow
    } else {
        ExitAction::Quit
    }
}

/// 安装退出信号处理。
///
/// 必须在 `app.run()` 之前调用（主线程）；安装失败只告警：后端仍会靠
/// `--parent-pid` 的父进程监视自行退出，不会留下孤儿。
pub fn install(app: &adw::Application) {
    let (sender, receiver) = async_channel::unbounded::<Watched>();
    if let Err(error) = spawn_watcher(sender) {
        eprintln!("[sqarvo-desktop] 无法监听退出信号：{error}（后端仍会监视父进程退出）");
        return;
    }

    let app = app.clone();
    glib::spawn_future_local(async move {
        while let Ok(watched) = receiver.recv().await {
            let windows = app.windows();
            let window = windows.first();
            match action_for(window.is_some()) {
                ExitAction::CloseWindow => {
                    eprintln!(
                        "[sqarvo-desktop] 收到 {} → 关闭窗口 → 回收后端",
                        watched.name()
                    );
                    if let Some(window) = window {
                        window.close();
                    }
                }
                ExitAction::Quit => {
                    eprintln!(
                        "[sqarvo-desktop] 收到 {} → 没有窗口，直接退出（app.quit）",
                        watched.name()
                    );
                    app.quit();
                }
            }
        }
    });
}

/// 在独立线程里等信号，再转给 GTK 主线程。
fn spawn_watcher(sender: async_channel::Sender<Watched>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("sqarvo-signals".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("[sqarvo-desktop] 信号线程无法启动运行时：{error}");
                    return;
                }
            };
            runtime.block_on(async move {
                let mut terminate = watch(SignalKind::terminate());
                let mut interrupt = watch(SignalKind::interrupt());
                let mut hangup = watch(SignalKind::hangup());
                loop {
                    let watched = tokio::select! {
                        () = receive(&mut terminate) => Watched::Terminate,
                        () = receive(&mut interrupt) => Watched::Interrupt,
                        () = receive(&mut hangup) => Watched::Hangup,
                    };
                    // 主线程已经退出（应用结束）：没人再需要这个事件。
                    if sender.try_send(watched).is_err() {
                        return;
                    }
                }
            });
        })
        .map(|_| ())
}

/// 注册一个信号流；失败时返回 `None`（该信号保持默认处置，其余信号仍可用）。
fn watch(kind: SignalKind) -> Option<Signal> {
    match signal(kind) {
        Ok(stream) => Some(stream),
        Err(error) => {
            eprintln!("[sqarvo-desktop] 无法监听 {kind:?}：{error}");
            None
        }
    }
}

/// 等一个信号流；未注册成功时永不返回。
async fn receive(stream: &mut Option<Signal>) {
    match stream {
        Some(stream) => {
            stream.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_names_match_kill_command() {
        assert_eq!(Watched::Terminate.name(), "SIGTERM");
        assert_eq!(Watched::Interrupt.name(), "SIGINT");
        assert_eq!(Watched::Hangup.name(), "SIGHUP");
    }

    #[test]
    fn window_present_uses_close_path() {
        // 有关窗口时必须走 window.close()（与用户点关闭按钮同一条路径），
        // 不能直接 app.quit() 绕过 close_request 的清理。
        assert_eq!(action_for(true), ExitAction::CloseWindow);
        assert_eq!(action_for(false), ExitAction::Quit);
    }
}
