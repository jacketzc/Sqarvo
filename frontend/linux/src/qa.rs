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
//!                 [--qa-sql-file /tmp/q.sql] [--qa-execute]
//!                 [--qa-scroll-to-row N]... [--qa-cancel-after-ms N]
//!                 [--qa-exit-after-ms N]
//! ```
//!
//! - `--screenshot <path>`：延时后把**当前最上层**的界面（连接对话框打开时是对话框，
//!   否则是主窗口）写成 PNG，然后关闭窗口（走与用户点关闭按钮相同的清理路径）；
//! - `--qa-profile <json>`：后端握手完成后自动打开连接对话框，把档案里的值填进
//!   **与用户输入相同的控件**，再触发与「保存并连接」按钮相同的回调；
//! - `--qa-password-env <ENV>`：从该环境变量读密码，填进密码行（命令行里不出现明文）；
//! - `--qa-sql-file <path>`：把文件内容填进 SQL 工作区的 `GtkTextView` 缓冲区；
//! - `--qa-execute`：连接成功后触发与「执行」按钮相同的方法；
//! - `--qa-scroll-to-row <n>`：滚动到第 n 行（可重复，按顺序每隔 1 秒一次）；未加载到
//!   该行时按批预取，与用户快速滚动走同一路径；
//! - `--qa-cancel-after-ms <n>`：执行后 n 毫秒点两次「取消」（间隔 150 ms），
//!   用于验证幂等与「点击 → Cancelled」耗时；
//! - `--qa-exit-after-ms <n>`：启动后 n 毫秒自动关窗（走正常清理路径），供基准脚本使用；
//! - `--qa-second-sql-file <path>` + `--qa-execute-again-after-ms <n>`：首次执行后 n 毫秒
//!   用第二段 SQL 再执行一次，用于验证「达到上限/取消之后仍能继续查询」。
//!
//! 这些参数只影响启动参数，不改变任何正常启动路径；也不存在「绕过 UI 直接调 gRPC」
//! 的捷径——自动化走的就是用户会走的那条路径。

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

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
    /// `--qa-sql-file` 读到的 SQL（在 init 阶段读取，文件不可读时直接报错）。
    sql: Option<String>,
    /// `--qa-execute`：连接成功后自动点「执行」。
    execute: bool,
    /// `--qa-scroll-to-row`：按出现顺序依次滚动；重复给出即多步滚动。
    scroll_rows: Vec<u32>,
    /// `--qa-cancel-after-ms`：执行后多少毫秒点两次取消（验证幂等与耗时）。
    cancel_after_ms: Option<u64>,
    /// `--qa-exit-after-ms`：启动后多少毫秒自动关闭窗口（基准脚本用，避免依赖人工关闭）。
    exit_after_ms: Option<u64>,
    /// `--qa-second-sql-file`：第二次执行用的 SQL（验证触顶/取消后仍能继续查询）。
    second_sql: Option<String>,
    /// `--qa-execute-again-after-ms`：首次执行后多少毫秒用 `second_sql`（或同一 SQL）再执行一次。
    execute_again_after_ms: Option<u64>,
    /// `--qa-data-table`：连接后打开的数据页表名。
    data_table: Option<String>,
    /// `--qa-data-begin-txn`：打开表后开始事务。
    data_begin: bool,
    /// `--qa-data-read-only`：开始只读事务（验证只读态）。
    data_read_only: bool,
    /// `--qa-data-edit <row>:<column>:<value>`：按顺序应用的单元格编辑。
    data_edits: Vec<(u32, String, String)>,
    /// `--qa-data-delete-row <row>`：标记删除的行（可重复）。
    data_delete_rows: Vec<u32>,
    /// `--qa-data-commit`：提交更改。
    data_commit: bool,
    /// `--qa-data-rollback`：回滚事务。
    data_rollback: bool,
    /// `--qa-data-scroll-to-row <n>`：滚动到第 n 行（触发 cursor 翻页）。
    data_scroll_rows: Vec<u32>,
    /// `--qa-data-reload`：提交/回滚后重新浏览当前表。
    data_reload: bool,
    /// `--qa-auto-confirm-risk`：无人值守时自动点风险确认对话框的「确认提交」。
    auto_confirm_risk: bool,
}

static ARGS: OnceLock<Args> = OnceLock::new();

/// 进程启动时刻，用于给 `[query]` / `[qa]` 日志标注毫秒时间戳。
static START: OnceLock<Instant> = OnceLock::new();

/// 解析命令行参数（`main` 调用一次）。失败时返回可读中文原因。
pub fn init(args: &[String]) -> Result<(), String> {
    let _ = START.set(Instant::now());
    let parsed = parse(args)?;
    let _ = ARGS.set(parsed);
    Ok(())
}

/// 自进程启动以来的毫秒数（日志用，便于和 RSS 采样对照）。
#[must_use]
pub fn elapsed_ms() -> u128 {
    START.get().map_or(0, |start| start.elapsed().as_millis())
}

/// QA 是否启用了任何自动化参数。
#[must_use]
pub fn enabled() -> bool {
    ARGS.get().is_some_and(|args| {
        args.sql.is_some()
            || args.execute
            || !args.scroll_rows.is_empty()
            || args.cancel_after_ms.is_some()
            || args.data_table.is_some()
            || !args.data_edits.is_empty()
            || args.data_commit
            || args.data_rollback
    })
}

/// `--qa-sql-file` 的 SQL 内容（未启用时为 `None`）。
pub fn sql() -> Option<String> {
    ARGS.get().and_then(|args| args.sql.clone())
}

/// 是否请求自动执行。
#[must_use]
pub fn execute_requested() -> bool {
    ARGS.get().is_some_and(|args| args.execute)
}

/// 依次要滚动到的行号（0 起）。
#[must_use]
pub fn scroll_targets() -> Vec<u32> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.scroll_rows.clone())
}

/// 执行后多久点取消（毫秒）。
#[must_use]
pub fn cancel_after_ms() -> Option<u64> {
    ARGS.get().and_then(|args| args.cancel_after_ms)
}

/// 启动后多久自动关闭窗口（毫秒）。仅用于基准脚本：不依赖人工关窗，退出仍走正常清理路径。
#[must_use]
pub fn exit_after_ms() -> Option<u64> {
    ARGS.get().and_then(|args| args.exit_after_ms)
}

/// 第二次执行用的 SQL（未设置时为 `None`，此时复用编辑器里的内容）。
#[must_use]
pub fn second_sql() -> Option<String> {
    ARGS.get().and_then(|args| args.second_sql.clone())
}

/// 首次执行后多久再执行一次（毫秒）。
#[must_use]
pub fn execute_again_after_ms() -> Option<u64> {
    ARGS.get().and_then(|args| args.execute_again_after_ms)
}

/// `--qa-data-table`：连接后要打开的表名。
#[must_use]
pub fn data_table() -> Option<String> {
    ARGS.get().and_then(|args| args.data_table.clone())
}

/// 是否在打开表后开始事务。
#[must_use]
pub fn data_begin_transaction() -> bool {
    ARGS.get().is_some_and(|args| args.data_begin)
}

/// 开始事务时是否只读。
#[must_use]
pub fn data_read_only() -> bool {
    ARGS.get().is_some_and(|args| args.data_read_only)
}

/// 按顺序应用的单元格编辑 `(行, 列名, 文本)`。
#[must_use]
pub fn data_edits() -> Vec<(u32, String, String)> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.data_edits.clone())
}

/// 要标记删除的行（0 起）。
#[must_use]
pub fn data_delete_rows() -> Vec<u32> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.data_delete_rows.clone())
}

/// 是否提交更改。
#[must_use]
pub fn data_commit() -> bool {
    ARGS.get().is_some_and(|args| args.data_commit)
}

/// 是否回滚事务。
#[must_use]
pub fn data_rollback() -> bool {
    ARGS.get().is_some_and(|args| args.data_rollback)
}

/// 要滚动到的行（0 起，按顺序）。
#[must_use]
pub fn data_scroll_rows() -> Vec<u32> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.data_scroll_rows.clone())
}

/// 是否在提交/回滚后重新浏览。
#[must_use]
pub fn data_reload() -> bool {
    ARGS.get().is_some_and(|args| args.data_reload)
}

/// 是否自动确认风险对话框（无人值守验证用）。
#[must_use]
pub fn auto_confirm_risk() -> bool {
    ARGS.get().is_some_and(|args| args.auto_confirm_risk)
}

/// 日志里记一行 SQL 摘要：只在 QA 模式下记录，并截断到 100 字符。
pub fn log_sql_preview(sql: &str) {
    if !enabled() {
        return;
    }
    let first = sql
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let preview: String = first.chars().take(100).collect();
    eprintln!("[qa] SQL 首行：{preview}");
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
            "--qa-sql-file" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-sql-file 需要一个文件路径".to_owned())?;
                let text = std::fs::read_to_string(value)
                    .map_err(|error| format!("无法读取 --qa-sql-file 文件 {value}：{error}"))?;
                if text.trim().is_empty() {
                    return Err(format!("--qa-sql-file 文件 {value} 内容为空"));
                }
                parsed.sql = Some(text);
                index += 2;
            }
            "--qa-execute" => {
                parsed.execute = true;
                index += 1;
            }
            "--qa-scroll-to-row" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-scroll-to-row 需要一个行号".to_owned())?;
                let row: u32 = value
                    .parse()
                    .map_err(|_| format!("--qa-scroll-to-row 不是合法行号：{value}"))?;
                parsed.scroll_rows.push(row);
                index += 2;
            }
            "--qa-cancel-after-ms" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-cancel-after-ms 需要一个毫秒数".to_owned())?;
                let millis: u64 = value
                    .parse()
                    .map_err(|_| format!("--qa-cancel-after-ms 不是合法毫秒数：{value}"))?;
                parsed.cancel_after_ms = Some(millis);
                index += 2;
            }
            "--qa-exit-after-ms" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-exit-after-ms 需要一个毫秒数".to_owned())?;
                let millis: u64 = value
                    .parse()
                    .map_err(|_| format!("--qa-exit-after-ms 不是合法毫秒数：{value}"))?;
                parsed.exit_after_ms = Some(millis);
                index += 2;
            }
            "--qa-second-sql-file" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-second-sql-file 需要一个文件路径".to_owned())?;
                let text = std::fs::read_to_string(value).map_err(|error| {
                    format!("无法读取 --qa-second-sql-file 文件 {value}：{error}")
                })?;
                if text.trim().is_empty() {
                    return Err(format!("--qa-second-sql-file 文件 {value} 内容为空"));
                }
                parsed.second_sql = Some(text);
                index += 2;
            }
            "--qa-execute-again-after-ms" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-execute-again-after-ms 需要一个毫秒数".to_owned())?;
                let millis: u64 = value
                    .parse()
                    .map_err(|_| format!("--qa-execute-again-after-ms 不是合法毫秒数：{value}"))?;
                parsed.execute_again_after_ms = Some(millis);
                index += 2;
            }
            "--qa-data-table" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-data-table 需要一个表名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-data-table 的表名不能为空".to_owned());
                }
                parsed.data_table = Some(value.clone());
                index += 2;
            }
            "--qa-data-begin-txn" => {
                parsed.data_begin = true;
                index += 1;
            }
            "--qa-data-read-only" => {
                parsed.data_read_only = true;
                index += 1;
            }
            "--qa-data-edit" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-data-edit 需要 row:column:value".to_owned())?;
                let mut parts = value.splitn(3, ':');
                let row: u32 = parts
                    .next()
                    .ok_or_else(|| format!("--qa-data-edit 缺少行号：{value}"))?
                    .parse()
                    .map_err(|_| format!("--qa-data-edit 的行号不合法：{value}"))?;
                let column = parts
                    .next()
                    .filter(|column| !column.is_empty())
                    .ok_or_else(|| format!("--qa-data-edit 缺少列名：{value}"))?;
                let text = parts
                    .next()
                    .ok_or_else(|| format!("--qa-data-edit 缺少值：{value}"))?;
                parsed
                    .data_edits
                    .push((row, column.to_owned(), text.to_owned()));
                index += 2;
            }
            "--qa-data-delete-row" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-data-delete-row 需要一个行号".to_owned())?;
                let row: u32 = value
                    .parse()
                    .map_err(|_| format!("--qa-data-delete-row 不是合法行号：{value}"))?;
                parsed.data_delete_rows.push(row);
                index += 2;
            }
            "--qa-data-commit" => {
                parsed.data_commit = true;
                index += 1;
            }
            "--qa-data-rollback" => {
                parsed.data_rollback = true;
                index += 1;
            }
            "--qa-data-scroll-to-row" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-data-scroll-to-row 需要一个行号".to_owned())?;
                let row: u32 = value
                    .parse()
                    .map_err(|_| format!("--qa-data-scroll-to-row 不是合法行号：{value}"))?;
                parsed.data_scroll_rows.push(row);
                index += 2;
            }
            "--qa-data-reload" => {
                parsed.data_reload = true;
                index += 1;
            }
            "--qa-auto-confirm-risk" => {
                parsed.auto_confirm_risk = true;
                index += 1;
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
    fn sql_flags_are_parsed_in_order() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("q.sql");
        std::fs::write(&path, "SELECT 1;\n").expect("写 SQL 文件");
        let parsed = parse(&args(&[
            "app",
            "--qa-sql-file",
            path.to_str().expect("UTF-8 路径"),
            "--qa-execute",
            "--qa-scroll-to-row",
            "50000",
            "--qa-scroll-to-row",
            "99999",
            "--qa-cancel-after-ms",
            "1200",
        ]))
        .unwrap();
        assert_eq!(parsed.sql.as_deref(), Some("SELECT 1;\n"));
        assert!(parsed.execute);
        assert_eq!(parsed.scroll_rows, vec![50000, 99999]);
        assert_eq!(parsed.cancel_after_ms, Some(1200));
    }

    #[test]
    fn sql_flags_reject_bad_values() {
        assert!(parse(&args(&["app", "--qa-sql-file"])).is_err());
        assert!(parse(&args(&["app", "--qa-sql-file", "/nonexistent/q.sql"])).is_err());
        assert!(parse(&args(&["app", "--qa-scroll-to-row"])).is_err());
        assert!(parse(&args(&["app", "--qa-scroll-to-row", "-1"])).is_err());
        assert!(parse(&args(&["app", "--qa-cancel-after-ms", "soon"])).is_err());
        assert!(parse(&args(&["app", "--qa-cancel-after-ms"])).is_err());
        assert!(parse(&args(&["app", "--qa-exit-after-ms", "soon"])).is_err());
    }

    #[test]
    fn exit_after_ms_is_parsed() {
        let parsed = parse(&args(&["app", "--qa-exit-after-ms", "9000"])).unwrap();
        assert_eq!(parsed.exit_after_ms, Some(9000));
        // 不设置时没有定时退出：正常交互路径不受影响。
        assert_eq!(parse(&args(&["app"])).unwrap().exit_after_ms, None);
    }

    #[test]
    fn second_execution_flags_are_parsed() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("second.sql");
        std::fs::write(&path, "SELECT 42 AS answer;\n").expect("写 SQL 文件");
        let parsed = parse(&args(&[
            "app",
            "--qa-second-sql-file",
            path.to_str().expect("UTF-8 路径"),
            "--qa-execute-again-after-ms",
            "2500",
        ]))
        .unwrap();
        assert_eq!(parsed.second_sql.as_deref(), Some("SELECT 42 AS answer;\n"));
        assert_eq!(parsed.execute_again_after_ms, Some(2500));
    }

    #[test]
    fn second_execution_flags_reject_bad_values() {
        assert!(parse(&args(&["app", "--qa-second-sql-file"])).is_err());
        assert!(
            parse(&args(&[
                "app",
                "--qa-second-sql-file",
                "/nonexistent/2.sql"
            ]))
            .is_err()
        );
        assert!(parse(&args(&["app", "--qa-execute-again-after-ms", "later"])).is_err());
    }

    #[test]
    fn missing_profile_file_is_reported() {
        let error = load_profile("/nonexistent/qa-profile.json").unwrap_err();
        assert!(error.contains("无法读取"));
    }
}
