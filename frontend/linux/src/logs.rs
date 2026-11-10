//! 日志落盘（v0.2.3.2）：把进程 stderr 同时写进 `$XDG_STATE_HOME/sqarvo/desktop.log`。
//!
//! 为什么需要它：产品里不再出现「断开后端」这类进入工程概念的按钮（用户只认识
//! 「这一个软件」），后端失效时要给用户两件事——**重启软件**与**看日志**。日志
//! 必须真的存在一个文件可看，不能只指向终端 stderr（从桌面图标启动时根本没有终端）。
//!
//! 做法：`pipe()` + `dup2(2)`，后台线程把管道的字节**同时**写到原来的 stderr（终端
//! 里仍然看得到同样的输出）与日志文件。不改任何 `eprintln!` 调用点。
//!
//! 失败时**不阻断启动**：拿不到状态目录、pipe/dup2 失败都只记一行 stderr（那行本身
//! 就说明了「日志文件不可用」），软件照常运行。

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::path::PathBuf;

use adw::prelude::*;

use crate::layout::{Gap, gap};

/// 日志文件路径（`$XDG_STATE_HOME/sqarvo/desktop.log`，缺省 `~/.local/state/…`）。
#[must_use]
pub fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(base.join("sqarvo").join("desktop.log"))
}

/// 安装日志镜像；返回日志文件路径（安装失败返回 `None`，不影响启动）。
pub fn install() -> Option<PathBuf> {
    let path = log_path()?;
    if let Some(dir) = path.parent()
        && let Err(error) = std::fs::create_dir_all(dir)
    {
        eprintln!("[logs] 无法创建日志目录 {}：{error}", dir.display());
        return None;
    }

    // SAFETY: `pipe`/`dup`/`dup2`/`close` 都只用本函数自己的两个整数 fd；失败时
    // 返回值被检查，成功时 fd 的所有权明确（读端交给后台线程的 File，写端 dup2
    // 之后立刻关闭，避免管道永远不 EOF）。
    let (read_fd, saved_stderr) = unsafe {
        let mut fds: [libc::c_int; 2] = [0; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            eprintln!("[logs] pipe 失败：日志只写 stderr");
            return None;
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        let saved = libc::dup(libc::STDERR_FILENO);
        if saved < 0 || libc::dup2(write_fd, libc::STDERR_FILENO) < 0 {
            eprintln!("[logs] 重定向 stderr 失败：日志只写 stderr");
            libc::close(read_fd);
            libc::close(write_fd);
            return None;
        }
        libc::close(write_fd);
        (read_fd, saved)
    };

    let path_for_thread = path.clone();
    std::thread::Builder::new()
        .name("sqarvo-log-mirror".to_owned())
        .spawn(move || mirror(read_fd, saved_stderr, path_for_thread))
        .ok()?;
    Some(path)
}

/// 后台线程：管道 → 日志文件 + 原 stderr。
fn mirror(read_fd: RawFd, saved_stderr: RawFd, path: PathBuf) {
    // SAFETY: `read_fd` 由 `install` 创建并在此处独占；`File` 接管后不再有别的
    // 地方关闭它。
    let mut pipe = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| eprintln!("[logs] 打开 {} 失败：{error}", path.display()))
        .ok();
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if let Some(file) = file.as_mut() {
                    let _ = file.write_all(&buffer[..count]);
                    let _ = file.flush();
                }
                // SAFETY: `saved_stderr` 是 `dup` 出来的有效 fd，`buffer[..count]` 是
                // 本线程的局部内存，`write` 只读它。
                unsafe {
                    libc::write(saved_stderr, buffer.as_ptr().cast::<libc::c_void>(), count);
                }
            }
            Err(_) => break,
        }
    }
}

/// 日志文件末尾 `limit` 字节（日志查看窗口用；超大文件不整个读进内存）。
#[must_use]
pub fn tail(limit: u64) -> String {
    let Some(path) = log_path() else {
        return "（找不到日志路径：XDG_STATE_HOME / HOME 都不可用）".to_owned();
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return format!("（还没有日志文件：{}）", path.display());
    };
    let length = metadata.len();
    let offset = length.saturating_sub(limit);
    let Ok(mut file) = std::fs::File::open(&path) else {
        return format!("（打不开日志文件：{}）", path.display());
    };
    if offset > 0 {
        use std::io::{Seek, SeekFrom};
        let _ = file.seek(SeekFrom::Start(offset));
    }
    let mut text = String::new();
    let _ = file.read_to_string(&mut text);
    if offset > 0 {
        // 从中间切的这一行可能是半行：丢掉第一行，避免显示乱码般的前缀。
        if let Some(newline) = text.find('\n') {
            text = text[newline + 1..].to_owned();
        }
    }
    text
}

/// 日志查看窗口（末尾 200 KB；只读、可选中复制，提供「复制路径」）。
///
/// 用 `GtkTextView` 而不是外部编辑器：从桌面图标启动时没有终端，也不该要求用户
/// 知道日志文件在哪；路径仍然显示在副标题里，方便反馈问题时贴出来。
pub fn present_window(parent: Option<&gtk::Window>) {
    let window = adw::Window::new();
    window.set_title(Some("Sqarvo 日志"));
    window.set_default_size(900, 600);
    if let Some(parent) = parent {
        window.set_transient_for(Some(parent));
    }

    let header = adw::HeaderBar::new();
    let path_text = log_path().map_or_else(
        || "（日志路径不可用）".to_owned(),
        |path| path.display().to_string(),
    );
    let subtitle = adw::WindowTitle::new("Sqarvo 日志", &path_text);
    header.set_title_widget(Some(&subtitle));

    let copy = gtk::Button::with_label("复制路径");
    copy.set_tooltip_text(Some("把日志文件路径复制到剪贴板"));
    {
        let path_text = path_text.clone();
        copy.connect_clicked(move |button| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&path_text);
                eprintln!("[logs] 已复制日志路径：{path_text}");
            }
            button.set_tooltip_text(Some("已复制"));
        });
    }
    header.pack_start(&copy);

    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(&tail(200 * 1024));
    let view = gtk::TextView::with_buffer(&buffer);
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_left_margin(gap(Gap::Lg));
    view.set_right_margin(gap(Gap::Lg));
    view.set_top_margin(gap(Gap::Md));
    view.set_wrap_mode(gtk::WrapMode::None);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&view)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
    root.append(&header);
    root.append(&scroll);
    window.set_content(Some(&root));
    window.present();
    // 滚到末尾：最新的一行最可能是用户要看的。
    let end = buffer.end_iter();
    let mark = buffer.create_mark(None, &end, false);
    view.scroll_mark_onscreen(&mark);
    mark.set_visible(true);
}
