//! 无人值守界面取证与自动化（开发/QA 用途，不属于产品功能）。
//!
//! # 为什么不用系统截图
//!
//! 本项目的验证环境是 GNOME Wayland 会话：`gnome-screenshot` 与
//! `org.gnome.Shell.Screenshot` 都需要 xdg-desktop-portal 的交互授权，无人值守时会被
//! 拒绝（`AccessDenied`）；XWayland 的根窗口不做合成，`x11grab` 只能抓到全黑画面。
//! 截图失败而继续「口头宣称界面正常」是不可接受的，因此这里改用 GTK 自己的渲染器把
//! 真实窗口渲染成 PNG：画面来自应用实际绘制的内容，不经过任何桌面截图接口。
//!
//! # 参数
//!
//! ```text
//! sqarvo-desktop --screenshot /tmp/ui.png [--screenshot-delay-ms 4000]
//!                 [--qa-profile /tmp/profile.json] [--qa-password-env SQARVO_QA_PASSWORD]
//! ```
//!
//! - `--screenshot <path>`：延时后把**当前最上层**的界面（连接对话框打开时是对话框，
//!   否则是主窗口）写成 PNG，然后关闭窗口（走与用户点关闭按钮相同的清理路径）；
//! - `--qa-profile <json>`：后端握手完成后自动打开连接对话框，把档案里的值填进
//!   **与用户输入相同的控件**，再触发与「保存并连接」按钮相同的回调；
//! - `--qa-password-env <ENV>`：从该环境变量读密码，填进密码行（命令行里不出现明文）。
//!
//! 三者都只影响启动参数，不改变任何正常启动路径；也不存在「绕过对话框直接调 gRPC」
//! 的捷径——自动化走的就是用户会走的那条路径。

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use gtk::prelude::*;
use serde::Deserialize;

/// 默认延时：握手（含拉起后端）在本机通常 1 秒内完成，留足时间让状态页/对话框渲染完。
const DEFAULT_DELAY: Duration = Duration::from_millis(4000);

/// `--screenshot` 解析结果。
#[derive(Debug, Clone)]
pub struct Capture {
    /// PNG 输出路径。
    pub path: PathBuf,
    /// 启动后等待多久再截图。
    pub delay: Duration,
}

/// `--qa-profile` 文件内容：一条连接的非敏感字段。
#[derive(Debug, Clone, Deserialize)]
pub struct QaProfile {
    /// 驱动 ID（默认 `mariadb`）。
    #[serde(default = "default_driver")]
    pub driver: String,
    /// 主机。
    pub host: String,
    /// 端口。
    #[serde(default = "default_port")]
    pub port: u16,
    /// 数据库名。
    pub database: String,
    /// 用户名。
    pub username: String,
    /// TLS 模式（`disabled` / `preferred` / `required` / `verify-ca` / `verify-full`）。
    #[serde(default)]
    pub tls_mode: Option<String>,
    /// 会话显示名（缺省时由对话框按 `user@host/db` 派生）。
    #[serde(default)]
    pub name: Option<String>,
}

fn default_driver() -> String {
    "mariadb".to_owned()
}

fn default_port() -> u16 {
    3306
}

/// QA 自动化要触发的动作（与两个按钮一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QaAction {
    /// 只点「测试连接」。
    Test,
    /// 点「保存并连接」（默认）。
    #[default]
    Save,
}

#[derive(Debug, Default)]
struct Args {
    capture: Option<Capture>,
    profile: Option<QaProfile>,
    password_env: Option<String>,
    action: QaAction,
}

static ARGS: OnceLock<Args> = OnceLock::new();

/// 解析命令行参数（`main` 调用一次）。失败时返回可读中文原因。
pub fn init(args: &[String]) -> Result<(), String> {
    let parsed = parse(args)?;
    let _ = ARGS.set(parsed);
    Ok(())
}

/// 已登记的截图请求（未启用时为 `None`）。
pub fn requested() -> Option<Capture> {
    ARGS.get().and_then(|args| args.capture.clone())
}

/// 已登记的 QA 连接档案（未启用时为 `None`）。
///
/// 返回克隆：档案里只有非敏感字段，密码不在这里。
pub fn profile() -> Option<QaProfile> {
    ARGS.get().and_then(|args| args.profile.clone())
}

/// 档案要触发的动作（`--qa-action test|save`，默认 `save`）。
#[must_use]
pub fn action() -> QaAction {
    ARGS.get().map_or(QaAction::Save, |args| args.action)
}

/// 从 `--qa-password-env` 指定的环境变量读密码。
///
/// 没有登记变量名时返回 `None`；登记了但变量未设置/为空时打印一行提示并返回 `None`
/// （对话框随后按「请输入密码」拒绝提交，绝不会静默用空密码连接）。
pub fn password() -> Option<String> {
    let name = ARGS.get()?.password_env.as_deref()?;
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => {
            eprintln!("[qa] 环境变量 {name} 未设置或为空：密码行保持为空");
            None
        }
    }
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut delay = DEFAULT_DELAY;
    let mut path: Option<PathBuf> = None;
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
            "--qa-profile" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-profile 需要一个 JSON 文件路径".to_owned())?;
                parsed.profile = Some(load_profile(value)?);
                index += 2;
            }
            "--qa-action" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-action 需要 test 或 save".to_owned())?;
                parsed.action = match value.as_str() {
                    "test" => QaAction::Test,
                    "save" => QaAction::Save,
                    other => return Err(format!("--qa-action 只接受 test 或 save，实际：{other}")),
                };
                index += 2;
            }
            "--qa-password-env" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-password-env 需要一个环境变量名".to_owned())?;
                if value.is_empty() {
                    return Err("--qa-password-env 的环境变量名不能为空".to_owned());
                }
                parsed.password_env = Some(value.clone());
                index += 2;
            }
            _ => index += 1,
        }
    }
    parsed.capture = path.map(|path| Capture { path, delay });
    Ok(parsed)
}

/// 读取并解析 QA 档案；任何问题都报错退出，避免「档案没生效却以为通过了」。
fn load_profile(path: &str) -> Result<QaProfile, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("无法读取 --qa-profile 文件 {path}：{error}"))?;
    let profile: QaProfile = serde_json::from_str(&text)
        .map_err(|error| format!("--qa-profile 文件 {path} 不是合法的连接档案：{error}"))?;
    if profile.host.trim().is_empty()
        || profile.database.trim().is_empty()
        || profile.username.trim().is_empty()
    {
        return Err("--qa-profile 缺少 host / database / username".to_owned());
    }
    // 驱动固定：对话框只支持 MariaDB，档案写别的值必须报错，不能静默忽略。
    if profile.driver != "mariadb" {
        return Err(format!(
            "--qa-profile 的 driver 必须是 mariadb（当前只支持该驱动），实际：{}",
            profile.driver
        ));
    }
    Ok(profile)
}

/// 把 `target` 当前的绘制内容渲染成 PNG。
///
/// `anchor` 提供 `GskRenderer`（通常是主窗口）；`target` 是要拍的内容（主窗口或已呈现的
/// `AdwDialog`）。用 `GtkWidgetPaintable` 取快照，是 GTK 在没有任何桌面截图权限时
/// 唯一能拿到的「真实画面」。
pub fn save_widget(
    anchor: &(impl IsA<gtk::Widget> + IsA<gtk::Native>),
    target: &impl IsA<gtk::Widget>,
    path: &std::path::Path,
) -> Result<(), String> {
    let width = target.width();
    let height = target.height();
    if width <= 0 || height <= 0 {
        return Err(format!("目标控件尺寸无效：{width}x{height}"));
    }

    let paintable = gtk::WidgetPaintable::new(Some(target));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot
        .to_node()
        .ok_or_else(|| "控件快照为空（界面尚未渲染）".to_owned())?;

    let native = anchor
        .native()
        .ok_or_else(|| "锚点窗口还没有 GdkSurface".to_owned())?;
    let renderer = native
        .renderer()
        .ok_or_else(|| "锚点窗口还没有 GskRenderer".to_owned())?;
    let viewport = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));
    texture
        .save_to_png(path)
        .map_err(|error| format!("写入 {} 失败：{error}", path.display()))
}

/// 把窗口自身渲染成 PNG（主窗口截图路径，保留原有调用方式）。
pub fn save_window<W>(window: &W, path: &std::path::Path) -> Result<(), String>
where
    W: IsA<gtk::Widget> + IsA<gtk::Native>,
{
    save_widget(window, window, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn no_flag_means_no_capture() {
        let parsed = parse(&args(&["sqarvo-desktop"])).unwrap();
        assert!(parsed.capture.is_none());
        assert!(parsed.profile.is_none());
        assert!(parsed.password_env.is_none());
    }

    #[test]
    fn screenshot_flag_takes_a_path_with_default_delay() {
        let capture = parse(&args(&["app", "--screenshot", "/tmp/a.png"]))
            .unwrap()
            .capture
            .expect("应解析出截图请求");
        assert_eq!(capture.path, PathBuf::from("/tmp/a.png"));
        assert_eq!(capture.delay, DEFAULT_DELAY);
    }

    #[test]
    fn delay_can_be_overridden() {
        let parsed = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--screenshot-delay-ms",
            "250",
        ]))
        .unwrap();
        assert_eq!(
            parsed.capture.expect("应解析出截图请求").delay,
            Duration::from_millis(250)
        );
    }

    #[test]
    fn malformed_values_are_reported() {
        assert!(parse(&args(&["app", "--screenshot"])).is_err());
        assert!(parse(&args(&["app", "--screenshot-delay-ms", "abc"])).is_err());
        assert!(parse(&args(&["app", "--qa-profile"])).is_err());
        assert!(parse(&args(&["app", "--qa-password-env"])).is_err());
        assert!(parse(&args(&["app", "--qa-password-env", ""])).is_err());
        assert!(parse(&args(&["app", "--qa-action", "bogus"])).is_err());
    }

    #[test]
    fn profile_file_is_parsed_with_defaults() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("profile.json");
        std::fs::write(
            &path,
            r#"{"host":"127.0.0.1","database":"sqarvo_test_27","username":"sqarvo_test"}"#,
        )
        .expect("写档案");
        let profile = load_profile(path.to_str().expect("UTF-8 路径")).expect("解析档案");
        assert_eq!(profile.driver, "mariadb");
        assert_eq!(profile.port, 3306);
        assert_eq!(profile.tls_mode, None);
    }

    #[test]
    fn profile_without_required_fields_is_rejected() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("profile.json");
        std::fs::write(&path, r#"{"host":"127.0.0.1"}"#).expect("写档案");
        let error = load_profile(path.to_str().expect("UTF-8 路径")).unwrap_err();
        // serde 会指出缺失的字段名（这里被截到「不是合法的连接档案：missing field ...」）。
        assert!(
            error.contains("缺少") || error.contains("missing field"),
            "{error}"
        );
    }

    #[test]
    fn missing_profile_file_is_reported() {
        let error = load_profile("/nonexistent/qa-profile.json").unwrap_err();
        assert!(error.contains("无法读取"));
    }
}
