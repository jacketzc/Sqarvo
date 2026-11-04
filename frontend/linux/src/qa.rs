//! 无人值守界面取证（开发/QA 用途，不属于产品功能）。
//!
//! # 为什么不用系统截图
//!
//! 本项目的验证环境是 GNOME Wayland 会话：`gnome-screenshot` 与
//! `org.gnome.Shell.Screenshot` 都需要 xdg-desktop-portal 的交互授权，无人值守时会被
//! 拒绝（`AccessDenied`）；XWayland 的根窗口不做合成，`x11grab` 只能抓到全黑画面。
//! 截图失败而继续「口头宣称界面正常」是不可接受的，因此这里改用 GTK 自己的渲染器把
//! 真实窗口渲染成 PNG：画面来自应用实际绘制的内容，不经过任何桌面截图接口。
//!
//! 用法：
//!
//! ```text
//! sqarvo-desktop --screenshot /tmp/ui.png [--screenshot-delay-ms 4000]
//! ```
//!
//! 到达延时后写 PNG 并关闭窗口（走与用户点关闭按钮相同的清理路径）。截图失败时进程以
//! 退出码 2 结束，避免把「没截到图」当成通过。
//!
//! 这个开关只影响进程启动参数，不改变任何正常启动路径的行为。

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use gtk::prelude::*;

/// 默认延时：握手（含拉起后端）在本机通常 1 秒内完成，留足时间让状态页渲染完。
const DEFAULT_DELAY: Duration = Duration::from_millis(4000);

/// `--screenshot` 解析结果。
#[derive(Debug, Clone)]
pub struct Capture {
    /// PNG 输出路径。
    pub path: PathBuf,
    /// 窗口呈现后等待多久再截图。
    pub delay: Duration,
}

static CAPTURE: OnceLock<Option<Capture>> = OnceLock::new();

/// 解析命令行参数并登记截图请求（`main` 调用一次）。
///
/// 失败时返回可读的中文原因：参数写错要比「静默不截图」更容易发现。
pub fn init(args: &[String]) -> Result<(), String> {
    let capture = parse(args)?;
    let _ = CAPTURE.set(capture);
    Ok(())
}

/// 已登记的截图请求（未启用时为 `None`）。
pub fn requested() -> Option<Capture> {
    CAPTURE.get().cloned().flatten()
}

fn parse(args: &[String]) -> Result<Option<Capture>, String> {
    let mut path: Option<PathBuf> = None;
    let mut delay = DEFAULT_DELAY;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--screenshot" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--screenshot 需要一个输出路径".to_owned())?;
                path = Some(PathBuf::from(value));
                index += 2;
            }
            "--screenshot-delay-ms" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--screenshot-delay-ms 需要一个毫秒数".to_owned())?;
                let millis: u64 = value
                    .parse()
                    .map_err(|_| format!("--screenshot-delay-ms 不是合法毫秒数：{value}"))?;
                delay = Duration::from_millis(millis);
                index += 2;
            }
            _ => index += 1,
        }
    }
    Ok(path.map(|path| Capture { path, delay }))
}

/// 把窗口当前绘制内容渲染成 PNG。
///
/// 用 `GtkWidgetPaintable` 取窗口快照，再交给窗口自己的 `GskRenderer` 渲染到
/// `GdkTexture`：这是 GTK 在没有任何桌面截图权限时唯一能拿到的「真实画面」。
pub fn save_window<W>(window: &W, path: &std::path::Path) -> Result<(), String>
where
    W: IsA<gtk::Widget> + IsA<gtk::Native>,
{
    let width = window.width();
    let height = window.height();
    if width <= 0 || height <= 0 {
        return Err(format!("窗口尺寸无效：{width}x{height}"));
    }

    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot
        .to_node()
        .ok_or_else(|| "窗口快照为空（界面尚未渲染）".to_owned())?;

    let native = window
        .native()
        .ok_or_else(|| "窗口还没有 GdkSurface".to_owned())?;
    let renderer = native
        .renderer()
        .ok_or_else(|| "窗口还没有 GskRenderer".to_owned())?;
    let viewport = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));
    texture
        .save_to_png(path)
        .map_err(|error| format!("写入 {} 失败：{error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn no_flag_means_no_capture() {
        assert!(parse(&args(&["sqarvo-desktop"])).unwrap().is_none());
    }

    #[test]
    fn screenshot_flag_takes_a_path_with_default_delay() {
        let capture = parse(&args(&["app", "--screenshot", "/tmp/a.png"]))
            .unwrap()
            .expect("应解析出截图请求");
        assert_eq!(capture.path, PathBuf::from("/tmp/a.png"));
        assert_eq!(capture.delay, DEFAULT_DELAY);
    }

    #[test]
    fn delay_can_be_overridden() {
        let capture = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--screenshot-delay-ms",
            "250",
        ]))
        .unwrap()
        .expect("应解析出截图请求");
        assert_eq!(capture.delay, Duration::from_millis(250));
    }

    #[test]
    fn malformed_values_are_reported() {
        assert!(parse(&args(&["app", "--screenshot"])).is_err());
        assert!(parse(&args(&["app", "--screenshot-delay-ms", "abc"])).is_err());
    }
}
