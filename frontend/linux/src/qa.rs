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
//!                 [--window-size 1200x800] [--qa-layout-audit]
//!                 [--qa-profile /tmp/profile.json] [--qa-password-env SQARVO_QA_PASSWORD]
//!                 [--qa-sql-file /tmp/q.sql] [--qa-execute]
//!                 [--qa-scroll-to-row N]... [--qa-cancel-after-ms N]
//!                 [--qa-exit-after-ms N]
//!                 [--qa-report-monitor]          # 只读：打印一行显示器 / 工作区 / 窗口结论
//! ```
//!
//! - `--window-size WIDTHxHEIGHT`：取证用的初始窗口尺寸（**优先于**按显示器工作区分档
//!   算出来的尺寸，日志写明「来自 QA」；小于当次最小窗口时仍会被抬到下限）；
//!   不给就按显示器逻辑工作区分档（scaling-and-window-sizing §5.2）。
//! - `--qa-report-monitor`：只读打印 `[monitor] <connector> 几何 … → 窗口 … 档位=… 来源=…`
//!   一行，**不改变任何尺寸**（#158 的回归入口）。
//! - `--qa-layout-audit`：截图之后在当前窗口尺寸上做「无裁切」量化自检——
//!   逐区域比较 GTK 最小/自然宽度与实际分配宽度，超限时以退出码 **5** 结束并打印
//!   超出的 px 数；必须与 `--screenshot` 一起使用（自检发生在截图时刻）。
//!
//! - `--screenshot <path>`：延时后把**当前最上层**的界面（连接对话框打开时是对话框，
//!   否则是主窗口）写成 PNG，然后关闭窗口（走与用户点关闭按钮相同的清理路径）；
//! - `--qa-profile <json>`：后端握手完成后自动打开连接对话框，把档案里的值填进
//!   **与用户输入相同的控件**，再触发与「保存并连接」按钮相同的回调；
//! - `--qa-profile` 的 `driver` 可以是 `mariadb` 或 `sqlite`：
//!   - `mariadb`：`host` / `port` / `database`（**可选**，省略 = 无默认库）/ `username` / `tls_mode`；
//!   - `sqlite`：`path`（必须已存在，或用 `:memory:`）/ `read_only`（布尔）；
//! - 档案里的驱动必须是后端 `ListDrivers` 返回的驱动，否则进程以退出码 **4** 结束
//!   （档案没生效就不算跑过）；
//! - `--qa-password-env <ENV>`：从该环境变量读密码，填进密码行（命令行里不出现明文）；
//! - `--qa-sql-file <path>`：把文件内容填进 SQL 工作区的 `GtkTextView` 缓冲区；
//! - `--qa-execute`：连接成功后触发与「执行」按钮相同的方法；
//! - `--qa-scroll-to-row <n>`：滚动到第 n 行（可重复，按顺序每隔 1 秒一次）；未加载到
//!   该行时按批预取，与用户快速滚动走同一路径；
//! - `--qa-cancel-after-ms <n>`：执行后 n 毫秒点两次「取消」（间隔 150 ms），
//!   用于验证幂等与「点击 → Cancelled」耗时；
//! - `--qa-exit-after-ms <n>`：启动后 n 毫秒自动关窗（走正常清理路径），供基准脚本使用；
//! - `--qa-second-sql-file <path>` + `--qa-execute-again-after-ms <n>`：首次执行后 n 毫秒
//!   用第二段 SQL 再执行一次，用于验证「达到上限/取消之后仍能继续查询」；
//! - `--qa-column-info <column>`：打开表后点**该列的列头**展开列信息浮层并高亮该列
//!   （走列头点击同一个 `publish_column_info` 路径）；
//! - `--qa-column-width <column>:<width>`：打开表后把该列宽度设成指定像素（可重复），
//!   走与用户拖拽列边界同一个 `fixed-width` 属性；截图时刻断言表头宽度与列宽一致
//!   （#133）；
//! - `--qa-expect-begin enabled|disabled`：截图时刻断言「开始事务」按钮的可点状态
//!   （有未提交的更改时必须禁用，tooltip 给出原因；读控件自身的 `sensitive` 与
//!   `tooltip_text()`）；
//! - `--qa-expect-column <column>:<field>=<text>`：截图时刻断言浮层里该列某字段的控件
//!   文本（可重复；field 取 `name|type|nullable|default|charset|collation`），
//!   不符时退出码 3；
//! - 对象树 / 搜索 / DDL（#86）：`--qa-tree-expand <name>`（可重复，逐层展开；对**库**
//!   节点会连它的四个分组行一起展开（`Functions` / `Queries` 只出说明行）——把
//!   「用户再点几下」自动化，
//!   产品行为是双击库只展开库本身、分组保持收起，v0.2.3.3）、
//!   `--qa-tree-open <name>`（打开表）、
//!   `--qa-tree-activate <name>`（走双击 / 回车的 `activate` 信号激活节点，v0.2.3.2 回归）、
//!   `--qa-tree-search <query>`（输入关键词）、`--qa-tree-click-result`（点第一条
//!   结果跳转）、`--qa-tree-load-more`（点「加载更多…」翻页）、
//!   `--qa-refresh`（触发与 `F5` 相同的 `win.refresh`：当前标签是
//!   表数据页时刷该表，否则重建对象树；日志写出走的是哪一条）、
//!   `--qa-tree-disconnect <name>`（右键「断开连接」同一条路径，含确认框）；
//!   `--qa-tree-menu-group <tables|views>` + `--qa-tree-menu-pick <条目>`（在种类分组行上
//!   打开右键菜单再点条目，与用户点同一条路径；未展开分组的「刷新这一组」提示用它取证）；
//! - `Objects` 固定页（v0.2.4 三列表格，与数据网格同套渲染）：
//!   `--qa-objects-select <name>`（走单选模型，与单击同一条路径，会同步信息面板）、
//!   `--qa-objects-activate <name>`（发 `GtkColumnView::activate`，与双击 / 回车同一条
//!   路径，会开表数据标签）；
//!   `--qa-expect-objects selected=<name>` 断言哪一行处于选中态；
//! - `--qa-search <query>`（走搜索框防抖同一条路径）+ `--qa-search-clear`（清空退出筛选）
//!   + `--qa-search-click-object`（点结果里第一个表 / 视图行，走 `row-activated` 信号：
//!     打开数据标签而结果页原地保留，v0.2.4）+ `--qa-expect-search <field>=<value>`：
//!     `hits|row|rows|highlight|status|state`，
//!     断言结果树的**行序**与**高亮行集合**（v0.2.4）；
//! - `--qa-expect-tree <field>=<value>`：`health|selected|search|results|first|page|
//!   loaded|status_contains`（`results` 现为结果页行数，含祖先行）；`--qa-expect-ddl <field>=<value>`：`contains|state|visible`
//!   （v0.2.3.3 起来源徽标 / 来源说明 / 「复制」按钮已从面板移除，对应字段一并删除）；
//! - `--qa-tabs-file <path>`（#116）：标签工作区脚本，每行一步，走与用户相同的路径
//!   （`open-table` / `query-tab` / `begin` / `edit` / `apply` / `rollback` / `reload` /
//!   `log-rows` / `sql` / `execute` / `close <n> <save|discard|cancel>` / `sessions <标签>` /
//!   `expect-tabs <n>` / `expect-limit <子串>` / `sleep <ms>`；`#` 开头为注释）。
//! - #117 增补的脚本步骤（同一个 `--qa-tabs-file`，不新增命令行开关）：
//!   `cancel <n>`（点该查询标签的「取消」，与用户路径一致）、`scroll <n> <row>`
//!   （表标签触发翻页 / 查询标签触发分批预取）、`trace <n>`（打印该标签实际收到的事件序列）、
//!   `expect-trace <n> <a,b,c>`（事件序列逐项相等）、`expect-rows <n> <count>`
//!   （已加载行数）、`expect-query <n> <rows|state|first|summary>=<值>`（结果归属与状态）。
//!   这三类 `expect-*` 失败都以退出码 3 结束，与既有断言同一口径。
//! - #120 增补：`--qa-export <path>`（数据就绪后导出，仍走确认对话框并自动点「导出」）、
//!   `--qa-export-hold`（停在确认框，截图用）、`--qa-export-cancel-after-rows <n>`
//!   （写满 n 行后走取消路径，验证目标不留半截文件、临时文件被清理）、
//!   `--qa-export-raw-values`（关掉公式注入防护，验证 #137 的开关）。
//! - #120 增补（整表导出）：`--qa-export-scope <page|table>`（默认 `page`，语义与
//!   原来一致；`table` 走「后台 offset 逐页读取 + 流式写同一个 CSV」）。整表路径的
//!   进度与取消断言**复用既有参数**，不新增开关：
//!   `--qa-export-cancel-after-rows <n>` 在写满 n 行后停（与用户点「取消」同一条
//!   清理路径：临时文件删除、目标不出现），日志里每页一行
//!   `[export] 整表进度：offset=… 累计 N 行`，终态是
//!   `[export] 整表导出已取消：已写入 N 行，临时文件已删除` 或
//!   `[export] 整表导出完成：N 行，M 字节`；文件本身用 `--qa-tabs-file` 的
//!   `expect-file <路径> <最少字节>` 断言。
//! - #121 增补（全部是 `--qa-tabs-file` 的**脚本步骤**，不新增命令行开关）：
//!   `expand <名字>`（展开树节点）、`select-node <名字>`（选中并切信息面板）、
//!   `inspector <ddl|info|show|hide>`、
//!   `sort-panel <标签> <列> <asc|desc>`（走筛选面板的排序区；v0.2.3.3 删掉表头点击排序后
//!   这是唯一的排序步骤）、
//!   `filter-panel <标签> <open|close>`（面板截图取证）、`edit-condition <标签> <序号>`
//!   （条件行内编辑态）、
//!   `filter <标签> <列> <算子> <值>`、`page <标签> <next|prev|first|last>`、
//!   `export <标签> <路径>`、`expect-file <路径> <最少字节>`、`screenshot <路径>`
//!   （GTK 自渲染，仓库外取证）、`focus-search`（等价于 §10 的 Ctrl+F 焦点目标）、
//!   `expect-focus search`、`mark-list-nodes` / `expect-list-nodes <n>`（展开一个
//!   catalog 的 `ListNodes` 次数 = 分组数）、`expect-sessions <n>`（`ListSessions`
//!   返回的会话总数）、`expect-browse <标签> <字段>=<值>`、
//!   `expect-inspector <字段>=<值>`、`quit`（走正常清理路径关窗）。
//!
//! - #141 增补（无默认库跨库验收）：`open-node <库> <表>`（按 `ObjectRef.catalog`
//!   精确定位，同名表分属两个库时不会开错）、`select-connection <名字>`（信息面板切到
//!   连接态）、`expect-tab-title <标签> <子串>`（读 AdwTabPage 的真实标题）、
//!   `expect-inspector facts=<子串>`（断言字段行，如 `默认库=未指定`）。
//!   验收要和用户正在运行的实例共存时，用环境变量 `SQARVO_QA_APP_ID` 给本次进程一个
//!   独立的 GApplication id（否则同 id 的新进程只会把激活转给用户窗口并以 0 退出）。
//!
//! - #139 增补（截图时刻断言，全部要求 `--screenshot`）：
//!   `--qa-startup-workspace`（未连接时主区域是 `Objects` 页，且不含「后端版本」
//!   「端点文件」等诊断字段）、`--qa-open-about`（握手后打开「关于 Squarvo」）、
//!   `--qa-expect-about <子串>`（「关于」里必须出现的文案，可重复）。
//!
//! - #144 增补（启动态点击已保存连接；全部要求 `--screenshot`，`--qa-startup-click`
//!   与 `--qa-startup-menu` 除外）：
//!   `--qa-startup-click <名字>`（激活启动态侧栏的一行，走对象树行自己的 `activate` 信号）、
//!   `--qa-expect-startup-connect <字段=值>`（可重复：`sessions|connects|log|
//!   title_contains|connected|tree_expanded`）、`--qa-expect-dialog <子串>`（连接对话框
//!   已打开且含该文案）、`--qa-startup-menu <名字>` + `--qa-startup-menu-pick <条目>`
//!   （打开右键菜单并点条目）、`--qa-delete-connection <名字>`（走「删除连接」回调并
//!   自动确认，仍经过二次确认对话框）。
//!
//! - 关窗守卫：`--qa-close-dirty answer=discard|cancel`（先用 `--qa-data-edit` /
//!   `--qa-data-delete-row` 建出脏行，再触发产品的 `app.close-window` 动作；
//!   `--qa-quit` 把触发动作换成 `app.quit`（`Ctrl+Q` 同一个动作），用来证明两条
//!   退出入口落到同一张确认框上；
//!   `discard` = 放弃更改并退出（退出码 0，日志「已放弃未提交更改并退出」），
//!   `cancel` = 取消关窗（进程不退出，由 `--qa-exit-after-ms` 收尾，日志
//!   「取消关窗：保持在工作区」）。答案走确认框自己的 `response` 信号，不绕过确认）。
//!
//! 这些都是「脚本 + 断言」的收拢：既有 `--qa-*` 的语义一个都没改。
//!
//! 这些参数只影响启动参数，不改变任何正常启动路径；也不存在「绕过 UI 直接调 gRPC」
//! 的捷径——自动化走的就是用户会走的那条路径。

use std::cell::RefCell;
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
///
/// 字段集按驱动不同：服务器型（MariaDB）用 `host`/`port`/`database`/`username`/`tls_mode`，
/// 文件型（SQLite）用 `path`/`read_only`。缺哪个驱动需要的字段由 [`load_profile`] 报错，
/// 不会静默用空值连接。
#[derive(Debug, Clone, Deserialize)]
pub struct QaProfile {
    /// 驱动 ID（默认 `mariadb`）。
    #[serde(default = "default_driver")]
    pub driver: String,
    /// 主机（服务器型驱动）。
    #[serde(default)]
    pub host: String,
    /// 端口（服务器型驱动）。
    #[serde(default = "default_port")]
    pub port: u16,
    /// 数据库名（服务器型驱动）。
    #[serde(default)]
    pub database: String,
    /// 用户名（服务器型驱动）。
    #[serde(default)]
    pub username: String,
    /// TLS 模式（`disabled` / `preferred` / `required` / `verify-ca` / `verify-full`）。
    #[serde(default)]
    pub tls_mode: Option<String>,
    /// 数据库文件路径（文件型驱动，SQLite）。
    #[serde(default)]
    pub path: Option<String>,
    /// 以只读方式打开（文件型驱动）。
    #[serde(default)]
    pub read_only: bool,
    /// 会话显示名（缺省时由对话框按驱动派生）。
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

/// `--qa-expect-data-state`：截图时必须成立的数据页状态。
///
/// 断言读的是界面上真实渲染的状态（同一份 `DataState` 枚举），不是「假设它会是」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataExpectation {
    /// 正在读取。
    Loading,
    /// 已加载（有行）。
    Ready,
    /// 表存在但没有行。
    Empty,
    /// 读取/提交失败。
    Error,
    /// 已断开。
    Disconnected,
}

impl DataExpectation {
    /// 参数取值（`--qa-expect-data-state`）。
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "loading" => Ok(Self::Loading),
            "ready" => Ok(Self::Ready),
            "empty" => Ok(Self::Empty),
            "error" => Ok(Self::Error),
            "disconnected" => Ok(Self::Disconnected),
            other => Err(format!(
                "--qa-expect-data-state 只接受 loading|ready|empty|error|disconnected，实际：{other}"
            )),
        }
    }

    /// 人类可读名称（日志用）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Loading => "Loading",
            Self::Ready => "Ready",
            Self::Empty => "Empty",
            Self::Error => "Error",
            Self::Disconnected => "Disconnected",
        }
    }
}

/// `--qa-expect-txn`：截图时必须成立的事务徽标状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnExpectation {
    /// 自动提交。
    AutoCommit,
    /// 事务中（读写）。
    Active,
    /// 事务中（只读）。
    ReadOnly,
    /// 失败态（只能回滚）。
    Failed,
    /// 已断开。
    Disconnected,
}

impl TxnExpectation {
    /// 参数取值（`--qa-expect-txn`）。
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::AutoCommit),
            "active" => Ok(Self::Active),
            "readonly" => Ok(Self::ReadOnly),
            "failed" => Ok(Self::Failed),
            "disconnected" => Ok(Self::Disconnected),
            other => Err(format!(
                "--qa-expect-txn 只接受 auto|active|readonly|failed|disconnected，实际：{other}"
            )),
        }
    }

    /// 徽标上应显示的文案（断言直接比对控件文本）。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AutoCommit => "自动提交",
            Self::Active => "事务中",
            Self::ReadOnly => "事务中（只读）",
            Self::Failed => "事务失败（只能回滚）",
            Self::Disconnected => "已断开",
        }
    }

    /// 徽标上应带的 CSS 类（视觉状态与文案一起断言）。
    #[must_use]
    pub fn css_class(self) -> &'static str {
        match self {
            Self::AutoCommit => "sqarvo-txn-commit",
            Self::Active => "sqarvo-txn-active",
            Self::ReadOnly => "sqarvo-txn-readonly",
            Self::Failed => "sqarvo-txn-failed",
            Self::Disconnected => "sqarvo-txn-disconnected",
        }
    }
}

/// `--qa-expect-column` 里的字段名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnField {
    /// 列名。
    Name,
    /// 原始类型。
    Type,
    /// 可空文案。
    Nullable,
    /// 默认值文案。
    DefaultValue,
    /// 字符集文案。
    Charset,
    /// 排序规则文案。
    Collation,
}

impl ColumnField {
    /// 参数里的字段名。
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "name" => Ok(Self::Name),
            "type" => Ok(Self::Type),
            "nullable" => Ok(Self::Nullable),
            "default" => Ok(Self::DefaultValue),
            "charset" => Ok(Self::Charset),
            "collation" => Ok(Self::Collation),
            other => Err(format!(
                "--qa-expect-column 的字段只接受 name|type|nullable|default|charset|collation，实际：{other}"
            )),
        }
    }

    /// 字段在参数里的名字（日志用，与 [`Self::parse`] 同一份词表）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Type => "type",
            Self::Nullable => "nullable",
            Self::DefaultValue => "default",
            Self::Charset => "charset",
            Self::Collation => "collation",
        }
    }

    /// 从面板的一行里取该字段的**控件文本**。
    #[must_use]
    pub fn value(self, row: &crate::data_page::ColumnInfoRowObservation) -> &str {
        match self {
            Self::Name => &row.name,
            Self::Type => &row.type_label,
            Self::Nullable => &row.nullable,
            Self::DefaultValue => &row.default_value,
            Self::Charset => &row.charset,
            Self::Collation => &row.collation,
        }
    }
}

/// `--qa-expect-submit` 的字段名（在途提交批次的可见表现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitField {
    /// 批次阶段：`idle|submitting|cancelling|done`。
    Phase,
    /// 结束方式：`applied|cancelled|failed`。
    Outcome,
    /// 「提交更改 / 取消提交」按钮的文案。
    Button,
    /// 状态行文本（含子串比对）。
    Status,
}

impl SubmitField {
    /// 参数里的字段名。
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "phase" => Ok(Self::Phase),
            "outcome" => Ok(Self::Outcome),
            "button" => Ok(Self::Button),
            "status" => Ok(Self::Status),
            other => Err(format!(
                "--qa-expect-submit 的字段只接受 phase|outcome|button|status，实际：{other}"
            )),
        }
    }

    /// 字段在参数里的名字（日志用）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Phase => "phase",
            Self::Outcome => "outcome",
            Self::Button => "button",
            Self::Status => "status",
        }
    }
}

/// 关窗守卫的无人值守答案（`--qa-close-dirty answer=discard|cancel`）。
///
/// 触发的是产品的 `app.close-window` 动作（`Ctrl+Shift+W` 同一条路径），答案通过
/// 确认框自己的 `response` 信号送出，不绕过确认直接关窗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseDirtyAnswer {
    /// 「放弃更改并退出」：进程正常退出（退出码 0）。
    Discard,
    /// 「取消」：不关窗，保持在工作区（由 `--qa-exit-after-ms` 收尾）。
    Cancel,
}

impl CloseDirtyAnswer {
    /// 参数里的写法（同时接受 `answer=` 前缀）。
    fn parse(value: &str) -> Result<Self, String> {
        let answer = value.strip_prefix("answer=").unwrap_or(value).trim();
        match answer {
            "discard" => Ok(Self::Discard),
            "cancel" => Ok(Self::Cancel),
            other => Err(format!(
                "--qa-close-dirty 只接受 answer=discard 或 answer=cancel，实际：{other}"
            )),
        }
    }

    /// 对话框的 response 名（与 `ui` 里的按钮一一对应）。
    #[must_use]
    pub fn response(self) -> &'static str {
        match self {
            Self::Discard => "discard",
            Self::Cancel => "cancel",
        }
    }
}

/// `--qa-expect-submit <field>=<value>` 的一条断言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitExpectation {
    /// 要比对的字段。
    pub field: SubmitField,
    /// 期望值（`button` / `status` 按「包含」比对，`phase` / `outcome` 逐字相等）。
    pub expected: String,
}

impl SubmitExpectation {
    /// 解析 `字段=值`；值里可以有 `=`。
    fn parse(value: &str) -> Result<Self, String> {
        let (field, expected) = value
            .split_once('=')
            .ok_or_else(|| format!("--qa-expect-submit 需要 字段=值，实际：{value}"))?;
        Ok(Self {
            field: SubmitField::parse(field)?,
            expected: expected.to_owned(),
        })
    }
}

/// `--qa-expect-column <column>:<field>=<value>` 的一条断言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnExpectation {
    /// 列名。
    pub column: String,
    /// 要比对的字段。
    pub field: ColumnField,
    /// 期望的展示文案（与界面上看到的逐字一致）。
    pub expected: String,
}

impl ColumnExpectation {
    /// 解析 `列名:字段=期望文案`；列名里不能有 `:`，文案里可以有 `=`。
    fn parse(value: &str) -> Result<Self, String> {
        let (column, rest) = value
            .split_once(':')
            .ok_or_else(|| format!("--qa-expect-column 需要 列名:字段=文案，实际：{value}"))?;
        let (field, expected) = rest
            .split_once('=')
            .ok_or_else(|| format!("--qa-expect-column 需要 列名:字段=文案，实际：{value}"))?;
        if column.trim().is_empty() {
            return Err(format!("--qa-expect-column 的列名为空：{value}"));
        }
        Ok(Self {
            column: column.to_owned(),
            field: ColumnField::parse(field)?,
            expected: expected.to_owned(),
        })
    }
}

/// `--qa-expect-tree` 要比对的字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeField {
    /// 树的健康状态（Ready / Empty / Unsupported / Failed…）。
    Health,
    /// 当前选中的节点名。
    Selected,
    /// 搜索页状态（results / empty / denied / failed）。
    Search,
    /// 搜索结果条数。
    Results,
    /// 第一条搜索结果的名字。
    First,
    /// 当前可见页（tree / search / state）。
    Page,
    /// 已加载的真实节点数。
    Loaded,
    /// 树的状态行是否包含某段文本。
    StatusContains,
}

impl TreeField {
    /// 参数里的字段名。
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "health" => Ok(Self::Health),
            "selected" => Ok(Self::Selected),
            "search" => Ok(Self::Search),
            "results" => Ok(Self::Results),
            "first" => Ok(Self::First),
            "page" => Ok(Self::Page),
            "loaded" => Ok(Self::Loaded),
            "status_contains" => Ok(Self::StatusContains),
            other => Err(format!(
                "--qa-expect-tree 的字段只接受 health|selected|search|results|first|page|loaded|status_contains，实际：{other}"
            )),
        }
    }

    /// 字段在参数里的名字（日志用，与 [`Self::parse`] 同一份词表）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Health => "health",
            Self::Selected => "selected",
            Self::Search => "search",
            Self::Results => "results",
            Self::First => "first",
            Self::Page => "page",
            Self::Loaded => "loaded",
            Self::StatusContains => "status_contains",
        }
    }
}

/// `--qa-expect-tree <field>=<value>` 的一条断言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeExpectation {
    /// 字段。
    pub field: TreeField,
    /// 期望值（`status_contains` 是子串，其余是逐字相等）。
    pub expected: String,
}

impl TreeExpectation {
    /// 解析 `字段=值`。
    fn parse(value: &str) -> Result<Self, String> {
        let (field, expected) = value
            .split_once('=')
            .ok_or_else(|| format!("--qa-expect-tree 需要 字段=值，实际：{value}"))?;
        Ok(Self {
            field: TreeField::parse(field)?,
            expected: expected.to_owned(),
        })
    }
}

/// `--qa-expect-search` 要比对的字段（#搜索树）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchField {
    /// 高亮行数（真正命中的行，不含连接 / 库 / 分组祖先）。
    Hits,
    /// 结果页必须包含某条路径（可重复 / 逗号分隔，`*` 匹配单段）。
    Row,
    /// 结果页的完整行序（逗号分隔，逐项相等）。
    Rows,
    /// 高亮行的完整集合（逗号分隔，逐项相等）。
    Highlight,
    /// 搜索状态行是否包含某段文本。
    Status,
    /// 搜索页状态（results / empty / denied / failed）。
    State,
    /// 已派发的搜索次数（回车重搜会让它 +1）。
    Runs,
}

impl SearchField {
    /// 参数里的字段名。
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "hits" => Ok(Self::Hits),
            "row" => Ok(Self::Row),
            "rows" => Ok(Self::Rows),
            "highlight" => Ok(Self::Highlight),
            "status" => Ok(Self::Status),
            "state" => Ok(Self::State),
            "runs" => Ok(Self::Runs),
            other => Err(format!(
                "--qa-expect-search 的字段只接受 hits|row|rows|highlight|status|state|runs，实际：{other}"
            )),
        }
    }

    /// 字段在参数里的名字（日志用，与 [`Self::parse`] 同一份词表）。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Hits => "hits",
            Self::Row => "row",
            Self::Rows => "rows",
            Self::Highlight => "highlight",
            Self::Status => "status",
            Self::State => "state",
            Self::Runs => "runs",
        }
    }
}

/// `--qa-expect-search <field>=<value>` 的一条断言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchExpectation {
    /// 字段。
    pub field: SearchField,
    /// 期望值（`hits` 是数字；`rows` / `highlight` / `row` 是 `/` 分隔的路径列表，
    /// 逗号分隔多条；`status` 是子串；其余逐字相等）。
    pub expected: String,
}

impl SearchExpectation {
    /// 解析 `字段=值`。
    fn parse(value: &str) -> Result<Self, String> {
        let (field, expected) = value
            .split_once('=')
            .ok_or_else(|| format!("--qa-expect-search 需要 字段=值，实际：{value}"))?;
        let field = SearchField::parse(field)?;
        if field == SearchField::Hits {
            expected
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("--qa-expect-search hits= 需要非负整数，实际：{expected}"))?;
        }
        if field == SearchField::Runs {
            expected
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("--qa-expect-search runs= 需要非负整数，实际：{expected}"))?;
        }
        if matches!(
            field,
            SearchField::Row | SearchField::Rows | SearchField::Highlight
        ) && expected.trim().is_empty()
        {
            return Err(format!(
                "--qa-expect-search {} 需要至少一条 `连接/库/.../对象` 路径",
                field.name()
            ));
        }
        Ok(Self {
            field,
            expected: expected.to_owned(),
        })
    }
}

/// 结果行路径匹配：`*` 匹配**一段**（连接名常有前缀 / 后缀，断言不必钉死它）。
///
/// 例如 `*/main/Tables/users` 命中 `ui-probe/main/Tables/users`；其它段逐字相等。
#[must_use]
pub fn search_path_matches(pattern: &str, actual: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let actual: Vec<&str> = actual.split('/').collect();
    pattern.len() == actual.len()
        && pattern
            .iter()
            .zip(&actual)
            .all(|(expected, got)| *expected == "*" || expected == got)
}

/// 逗号分隔的断言值 → 去空白的列表（`row=` / `rows=` / `highlight=` 共用）。
#[must_use]
pub fn search_paths(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `--qa-expect-ddl` 要比对的字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdlField {
    /// DDL 正文是否包含某段文本。
    Contains,
    /// 面板状态（idle / loading / ready / empty / error / unsupported）。
    State,
    /// 面板是否可见（on / off）。
    Visible,
}

impl DdlField {
    /// 参数里的字段名。
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "contains" => Ok(Self::Contains),
            "state" => Ok(Self::State),
            "visible" => Ok(Self::Visible),
            other => Err(format!(
                "--qa-expect-ddl 的字段只接受 contains|state|visible，实际：{other}"
            )),
        }
    }

    /// 字段在参数里的名字。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::State => "state",
            Self::Visible => "visible",
        }
    }
}

/// `--qa-expect-ddl <field>=<value>` 的一条断言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdlExpectation {
    /// 字段。
    pub field: DdlField,
    /// 期望值（`*_contains` 是子串，其余逐字相等）。
    pub expected: String,
}

impl DdlExpectation {
    /// 解析 `字段=值`；值里可以有 `=`（只按第一个切分）。
    fn parse(value: &str) -> Result<Self, String> {
        let (field, expected) = value
            .split_once('=')
            .ok_or_else(|| format!("--qa-expect-ddl 需要 字段=值，实际：{value}"))?;
        Ok(Self {
            field: DdlField::parse(field)?,
            expected: expected.to_owned(),
        })
    }
}

/// `--qa-expect-inspector <field>=<value>` 的一条断言（#119）。
///
/// 字段取值：`page|table_page|title|subtitle|txn_hint|ddl_state|facts_contains|state_contains|columns|visible`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorExpectation {
    /// 字段名（原样保留，比对时再分派）。
    pub field: String,
    /// 期望值。
    pub expected: String,
}

impl InspectorExpectation {
    fn parse(value: &str) -> Result<Self, String> {
        let (field, expected) = value
            .split_once('=')
            .ok_or_else(|| format!("--qa-expect-inspector 需要 字段=值，实际：{value}"))?;
        if !matches!(
            field,
            "page"
                | "table_page"
                | "title"
                | "subtitle"
                | "txn_hint"
                | "ddl_state"
                | "facts_contains"
                | "state_contains"
                | "columns"
                | "visible"
        ) {
            return Err(format!(
                "--qa-expect-inspector 只接受 page|table_page|title|subtitle|txn_hint|ddl_state|facts_contains|state_contains|columns|visible，实际：{field}"
            ));
        }
        Ok(Self {
            field: field.to_owned(),
            expected: expected.to_owned(),
        })
    }
}

/// `--qa-expect-objects` 的断言（#135：读 Objects 固定页的控件文本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectsExpectation {
    /// 字段名（原样保留，比对时再分派）。
    pub field: String,
    /// 期望值。
    pub expected: String,
}

impl ObjectsExpectation {
    fn parse(value: &str) -> Result<Self, String> {
        let (field, expected) = value
            .split_once('=')
            .ok_or_else(|| format!("--qa-expect-objects 需要 字段=值，实际：{value}"))?;
        if !matches!(field, "count" | "first" | "selected" | "status" | "empty") {
            return Err(format!(
                "--qa-expect-objects 只接受 count|first|selected|status|empty，实际：{field}"
            ));
        }
        Ok(Self {
            field: field.to_owned(),
            expected: expected.to_owned(),
        })
    }
}

/// `--qa-isolation-level` 的取值：只想让脚本说人话，映射到 proto 枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationChoice {
    /// 不指定（交给数据库默认值）。
    Default,
    /// 读未提交。
    ReadUncommitted,
    /// 读已提交。
    ReadCommitted,
    /// 可重复读。
    RepeatableRead,
    /// 可序列化。
    Serializable,
}

impl IsolationChoice {
    /// 解析参数取值。
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "default" | "unspecified" => Ok(Self::Default),
            "read-uncommitted" => Ok(Self::ReadUncommitted),
            "read-committed" => Ok(Self::ReadCommitted),
            "repeatable-read" => Ok(Self::RepeatableRead),
            "serializable" => Ok(Self::Serializable),
            other => Err(format!(
                "--qa-isolation-level 只接受 default|read-uncommitted|read-committed|repeatable-read|serializable，实际：{other}"
            )),
        }
    }

    /// 对应的 proto 枚举。
    #[must_use]
    pub fn level(self) -> crate::proto::services::v1::IsolationLevel {
        use crate::proto::services::v1::IsolationLevel;
        match self {
            Self::Default => IsolationLevel::Unspecified,
            Self::ReadUncommitted => IsolationLevel::ReadUncommitted,
            Self::ReadCommitted => IsolationLevel::ReadCommitted,
            Self::RepeatableRead => IsolationLevel::RepeatableRead,
            Self::Serializable => IsolationLevel::Serializable,
        }
    }
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
    /// `--qa-report-monitor`：只读打印一行窗口 / 显示器 / 工作区 / 档位结论，不改任何尺寸。
    report_monitor: bool,
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
    /// `--qa-data-fail-txn`：把「服务端报告 transaction.failed」这条失败路径重放到界面。
    data_fail_txn: bool,
    /// `--qa-data-replay-cancelled`：把「批次被取消」的响应重放到界面。
    data_replay_cancelled: bool,
    /// `--qa-data-edit-range <start>:<count>:<column>:<value>`：批量编辑连续行
    /// （每行都走与单元格编辑相同的一条路径）。
    data_edit_ranges: Vec<(u32, u32, String, String)>,
    /// `--qa-data-cancel-after-ms <n>`：点「提交更改」后 n 毫秒点「取消提交」。
    data_cancel_after_ms: Option<u64>,
    /// `--qa-close-dirty answer=discard|cancel`：脏行建好后触发 `app.close-window`，
    /// 由确认框的 response 送答案（验证关窗守卫的两条去向）。
    close_dirty: Option<CloseDirtyAnswer>,
    /// `--qa-quit`：把上面那一步改成触发产品的 `app.quit`（`Ctrl+Q` 同一个动作），
    /// 用来验证「`Ctrl+Q` 也走脏行守卫」。
    quit_action: bool,
    /// `--qa-expect-submit <field>=<value>`：在途提交批次的断言（可重复）。
    expect_submit: Vec<SubmitExpectation>,
    /// `--qa-expect-data-state`：截图时必须成立的数据页状态。
    expect_data_state: Option<DataExpectation>,
    /// `--qa-expect-txn`：截图时必须成立的事务徽标状态。
    expect_txn: Option<TxnExpectation>,
    /// `--qa-expect-status-dot <子串>`：截图时状态圆点的 tooltip 必须包含该子串。
    ///
    /// tooltip 是「事务徽标 + 脏行计数」两段文案的唯一可见出口，断言直接读
    /// `widget.tooltip_text()`（GTK 头less 读回），不靠截图里能不能截到浮层。
    expect_status_dot: Option<String>,
    /// `--qa-expect-begin <enabled|disabled>`：截图时「开始事务」按钮的可点状态必须如此。
    ///
    /// 有未提交的更改（脏行 / 标记删除 / 新行草稿）时必须禁用，且 tooltip 说明原因
    /// （§5.1）；读的是按钮自己的 `sensitive` 与 `tooltip_text()`，不是状态机推算。
    expect_begin: Option<bool>,
    /// `--qa-column-info <column>`：连接并打开表后点该列的列头，展开列信息浮层并高亮该列。
    column_info: Option<String>,
    /// `--qa-column-width <column:width>`：把该列宽度设成指定像素（可重复）。走用户拖拽
    /// 列边界同一条属性路径（`GtkColumnViewColumn::set_fixed_width`）；截图时刻断言
    /// 类型条带宽度与列宽一致（#133）。
    column_widths: Vec<(String, i32)>,
    /// `--qa-expect-column <column>:<field>=<value>`：列信息浮层上必须显示的文案（可重复）。
    expect_columns: Vec<ColumnExpectation>,
    /// `--qa-tree-expand <name>`：依次展开的节点名（可重复，逐层展开）。
    tree_expand: Vec<String>,
    /// `--qa-tree-open <name>`：在树里打开的表名。
    tree_open: Option<String>,
    /// `--qa-refresh`：执行一次 `win.refresh`（有表标签刷当前表，否则刷对象树）。
    refresh: bool,
    /// `--qa-tree-activate <name>`：走双击 / 回车那条 `GtkListView::activate` 信号激活
    /// 节点（v0.2.3.2 回归：表有子节点之后双击仍应打开表，只选中不算）。
    tree_activate: Option<String>,
    /// `--qa-tree-search <query>`：搜索关键词。
    tree_search: Option<String>,
    /// `--qa-search <query>`：走搜索框 `changed` → 防抖 → 请求的同一条路径（#搜索树）。
    search_query: Option<String>,
    /// `--qa-expect-search <field>=<value>`：结果行序 / 高亮行集合的断言（可重复）。
    expect_search: Vec<SearchExpectation>,
    /// `--qa-search-clear`：搜索后把输入框清空（走用户清空 / Esc 同一条路径）。
    search_clear: bool,
    /// `--qa-search-enter`：在搜索框上发一次真实的回车（`activate` 信号），
    /// 文本不变也必须重新派发一次搜索。
    search_enter: bool,
    /// `--qa-tree-click-result`：点第一条搜索结果（跳转）。
    tree_click_result: bool,
    /// `--qa-search-click-object`（v0.2.4）：点结果里第一个**可打开的对象**行
    /// （表 / 视图），走真实 `row-activated` 信号 → 打开数据标签，结果页原地保留。
    search_click_object: bool,
    /// `--qa-tree-load-more`：点第一个「加载更多…」占位行。
    tree_load_more: bool,
    /// `--qa-tree-disconnect <name>`：断开指定连接的浏览会话（#114）。
    tree_disconnect: Option<String>,
    /// `--qa-tree-select <name>`：只选中某个对象节点（信息面板据此换内容，#119）。
    tree_select: Option<String>,
    /// `--qa-tree-select-connection <name>`：选中某条连接节点（#119）。
    tree_select_connection: Option<String>,
    /// `--qa-tree-select-group <tables|views>`：选中库下的种类分组行（v0.2.3.3）。
    tree_select_group: Option<String>,
    /// `--qa-tree-menu-group <tables|views>`：在库下的种类分组行上打开右键菜单
    /// （未展开的分组刷新反馈用它取证；只有这两组带「刷新这一组」）。
    tree_menu_group: Option<String>,
    /// `--qa-tree-menu-pick <label>`：点当前右键菜单里的这一项（与用户点同一条点击路径）。
    tree_menu_pick: Option<String>,
    /// `--qa-objects-select <name>`：选中 `Objects` 固定页里的某一行（走单选模型，
    /// 与单击同一条路径；v0.2.4 的三列表格交互）。
    objects_select: Option<String>,
    /// `--qa-objects-activate <name>`：激活 `Objects` 固定页的某一行（发
    /// `GtkColumnView::activate`，与双击 / 回车同一条路径）。
    objects_activate: Option<String>,
    /// `--qa-kind <tables|views|functions|queries>`：点主工具条的种类入口（`emit_clicked`，
    /// 与用户点击同一条信号；v0.2.3.3）。
    kind_click: Option<crate::tabs::KindShortcut>,
    /// `--qa-inspector <ddl|info>`：信息面板切到哪一页（#119；v0.2.3.2 起面板常驻、不能折叠）。
    inspector: Option<String>,
    /// `--qa-inspector-width <px>`：把面板拖到指定宽度（窗口级记忆的证据，#119 / #155）。
    inspector_width: Option<i32>,
    /// `--qa-expect-tree <field>=<value>`：树 / 搜索的断言（可重复）。
    expect_tree: Vec<TreeExpectation>,
    /// `--qa-expect-ddl <field>=<value>`：DDL 面板的断言（可重复）。
    expect_ddl: Vec<DdlExpectation>,
    /// `--qa-expect-inspector <field>=<value>`：信息面板的断言（可重复，#119）。
    expect_inspector: Vec<InspectorExpectation>,
    /// `--qa-expect-objects <field>=<value>`：`Objects` 固定页的断言（可重复，#135）。
    expect_objects: Vec<ObjectsExpectation>,
    // ---- #88 插入行与隔离级别 ----
    /// `--qa-data-insert`：点「新增行」（走状态栏行操作组 `RowOp::Add` 那条路径）。
    data_insert: bool,
    /// `--qa-data-insert-set <column:value>`：给新行某列填值（按列名，可重复）。
    data_insert_set: Vec<(String, String)>,
    /// `--qa-data-insert-null <column>`：把新行某列显式设为 NULL（可重复）。
    data_insert_null: Vec<String>,
    /// `--qa-expect-row <column=value>`：重新浏览后必须存在该值的行（否则退出码 3）。
    expect_rows: Vec<(String, String)>,
    /// `--qa-isolation-dialog`：打开「开始事务」的隔离级别选择卡片（截图用）。
    isolation_dialog: bool,
    /// `--qa-isolation-level <level>`：对话框里预选的隔离级别。
    isolation_level: Option<IsolationChoice>,
    /// `--qa-isolation-confirm`：在卡片里点「开始事务」（真的开事务）。
    isolation_confirm: bool,
    /// `--qa-layout-audit`：截图后在当前窗口尺寸上做「无裁切」量化自检。
    layout_audit: bool,
    // ---- #139 启动落地工作区 ----
    /// `--qa-startup-workspace`：截图时刻断言当前是未连接的启动工作区（Objects 空态）。
    startup_workspace: bool,
    // ---- #144 启动态点击已保存连接 ----
    /// `--qa-startup-click <name>`：在启动态侧栏按名字激活一行（走对象树 `activate` 同一条路径）。
    startup_click: Option<String>,
    /// `--qa-expect-startup-connect <字段=值>`：截图时刻断言「按保存凭据连接」的结果（可重复）。
    expect_startup_connect: Vec<(String, String)>,
    /// `--qa-expect-dialog <子串>`：截图时刻断言连接对话框已打开且含该文案（可重复）。
    expect_dialog: Vec<String>,
    /// `--qa-startup-menu <name>`：在启动态侧栏打开该行的右键菜单。
    startup_menu: Option<String>,
    /// `--qa-startup-menu-pick <label>`：点右键菜单里该文案的条目（与用户点击同一条路径）。
    startup_menu_pick: Option<String>,
    /// `--qa-delete-connection <name>`：走「删除连接」同一条回调并自动确认。
    delete_connection: Option<String>,
    /// `--qa-open-about`：握手完成后打开「关于 Squarvo」（截图与断言用）。
    open_about: bool,
    /// `--qa-open-menu`：截图前打开 headerbar 主菜单（弹出层截图用）。
    open_menu: bool,
    /// `--qa-expect-about <substring>`：「关于」对话框里必须出现的文案（可重复）。
    expect_about: Vec<String>,
    /// `--window-size <WIDTHxHEIGHT>`：取证用的初始窗口尺寸（产品默认 1200×800）。
    window_size: Option<(i32, i32)>,
    /// `--tier <compact|comfortable|large>`：取证用的起始密度档（产品默认 compact / 记忆值）。
    ///
    /// 与 `--window-size` 同一次解析、同一套错误文案；只在「用户能走到」的意义上改用，
    /// 不改变产品路径（菜单 / 快捷键才是用户入口）。
    tier: Option<crate::layout::Tier>,
    // ---- #116 标签工作区 ----
    /// `--qa-tabs-file <path>`：标签工作区脚本（步骤见 `tabs::parse_script`）。
    tabs_script: Option<String>,
    // ---- #118 表格浏览：offset 分页 / 排序 / 筛选 / 多选 / 列显隐 ----
    /// `--qa-browse <step>`：按顺序执行的浏览步骤（可重复，见 [`BrowseStep`]）。
    browse_steps: Vec<BrowseStep>,
    // ---- #120 导出 CSV ----
    /// `--qa-export <path>`：数据就绪后导出到该路径（仍走确认对话框，QA 自动点确认）。
    export_path: Option<PathBuf>,
    /// `--qa-export-hold`：停在导出确认对话框（截图用，不自动确认）。
    export_hold: bool,
    /// `--qa-export-cancel-after-rows <n>`：写满 n 行后走取消路径（验证临时文件清理）。
    export_cancel_after_rows: Option<u64>,
    /// `--qa-export-raw-values`：导出时关闭公式注入防护（写原始值，验证 #137 的开关）。
    export_raw_values: bool,
    /// `--qa-export-scope <page|table>`：导出范围（默认当前页，与 #120 语义一致）。
    export_scope: ExportScopeArg,
}

/// `--qa-export-scope` 的取值：整表导出（#120 增补）与默认的当前页。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExportScopeArg {
    /// 当前页 / 已加载的行（默认；行为与 #120 完全一致）。
    #[default]
    Page,
    /// 整张表：worker 侧按当前排序与筛选 offset 逐页读取。
    Table,
}

impl ExportScopeArg {
    /// 解析 `page` / `table`（其它值报错，不静默退回默认）。
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "page" => Ok(Self::Page),
            "table" => Ok(Self::Table),
            other => Err(format!(
                "--qa-export-scope 只能是 page 或 table（收到 {other:?}）"
            )),
        }
    }
}

/// `--qa-browse` 的一步（顺序执行；全部走用户会走的同一条界面路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseStep {
    /// `open=<table>`：按名打开表（与点侧栏条目同一条路径）。
    Open(String),
    /// `filter=<column>:<op>:<value>`：填筛选面板并应用（op = eq/ne/lt/le/gt/ge/like/null/notnull）。
    Filter(String, String, String),
    /// `next` / `prev` / `first` / `last`：分页器动作。
    Page(String),
    /// `select=<n>`：选中前 n 行（多选）。
    Select(usize),
    /// `hide=<column>` / `show=<column>`：列显隐。
    Visible(String, bool),
    /// `log`：把当前浏览状态（页码 / 总数文案 / 排序 / 筛选 / 可见列 / 选中数）打进日志。
    Log,
}

impl BrowseStep {
    /// 解析 `--qa-browse` 的一步。
    pub fn parse(raw: &str) -> Result<Self, String> {
        let (key, value) = raw
            .split_once('=')
            .map_or((raw.trim(), None), |(key, value)| (key.trim(), Some(value)));
        match key {
            "open" => {
                let value = value.ok_or_else(|| "--qa-browse open= 需要表名".to_owned())?;
                if value.is_empty() {
                    return Err("--qa-browse open= 的表名不能为空".to_owned());
                }
                Ok(Self::Open(value.to_owned()))
            }
            "filter" => {
                let value = value
                    .ok_or_else(|| "--qa-browse filter= 需要 <column>:<op>:<value>".to_owned())?;
                let mut parts = value.splitn(3, ':');
                let column = parts.next().unwrap_or_default();
                let operator = parts
                    .next()
                    .ok_or_else(|| "--qa-browse filter= 缺少算子".to_owned())?;
                let operand = parts.next().unwrap_or_default();
                if column.is_empty() {
                    return Err("--qa-browse filter= 的列名不能为空".to_owned());
                }
                Ok(Self::Filter(
                    column.to_owned(),
                    operator.to_owned(),
                    operand.to_owned(),
                ))
            }
            "next" | "prev" | "first" | "last" => {
                if value.is_some() {
                    return Err(format!("--qa-browse {key} 不接受取值"));
                }
                Ok(Self::Page(key.to_owned()))
            }
            "select" => {
                let value = value.ok_or_else(|| "--qa-browse select= 需要行数".to_owned())?;
                let count = value
                    .parse::<usize>()
                    .map_err(|_| format!("--qa-browse select= 不是合法行数：{value}"))?;
                Ok(Self::Select(count))
            }
            "hide" | "show" => {
                let value = value.ok_or_else(|| format!("--qa-browse {key}= 需要列名"))?;
                if value.is_empty() {
                    return Err(format!("--qa-browse {key}= 的列名不能为空"));
                }
                Ok(Self::Visible(value.to_owned(), key == "show"))
            }
            "log" => {
                if value.is_some() {
                    return Err("--qa-browse log 不接受取值".to_owned());
                }
                Ok(Self::Log)
            }
            other => Err(format!(
                "未知的 --qa-browse 步骤：{other}（可用：open/filter/next/prev/first/last/select/hide/show/log）"
            )),
        }
    }
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
        // `--screenshot` 也是 QA 参数：不把它算进来，只传截图时 application-id 不会换成
        // `SQARVO_QA_APP_ID`，进程会转发给用户正在运行的实例后以 0 退出、不产图。
        args.capture.is_some()
            || args.report_monitor
            || args.sql.is_some()
            || args.execute
            || !args.scroll_rows.is_empty()
            || args.cancel_after_ms.is_some()
            || args.data_table.is_some()
            || !args.data_edits.is_empty()
            || args.data_commit
            || args.data_rollback
            || args.data_fail_txn
            || args.data_replay_cancelled
            || args.column_info.is_some()
            || !args.tree_expand.is_empty()
            || args.tree_open.is_some()
            || args.tree_activate.is_some()
            || args.objects_select.is_some()
            || args.objects_activate.is_some()
            || args.kind_click.is_some()
            || args.tree_select_group.is_some()
            || args.tree_menu_group.is_some()
            || args.tree_menu_pick.is_some()
            || args.tree_search.is_some()
            || args.search_query.is_some()
            || args.search_enter
            || args.search_clear
            || args.tree_click_result
            || args.search_click_object
            || args.tree_load_more
            || args.data_insert
            || !args.data_edit_ranges.is_empty()
            || args.data_cancel_after_ms.is_some()
            || args.close_dirty.is_some()
            || args.quit_action
            || args.isolation_dialog
            || args.isolation_confirm
            || args.layout_audit
            || args.tier.is_some()
            || args.tabs_script.is_some()
            || args.open_menu
            || !args.browse_steps.is_empty()
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

/// `--qa-report-monitor`：只读打印一行窗口 / 显示器 / 工作区 / 档位结论（#158）。
///
/// 这个开关**不改任何尺寸**：它是 [crate::window_size] 决策过程的日志出口，
/// 用来回归「1366×768@125% 这类场景下窗口不超工作区」。
#[must_use]
pub fn report_monitor() -> bool {
    ARGS.get().is_some_and(|args| args.report_monitor)
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

/// `--qa-tabs-file` 的脚本原文（未启用时为 `None`）。
#[must_use]
pub fn tabs_script() -> Option<String> {
    ARGS.get().and_then(|args| args.tabs_script.clone())
}

/// `--qa-browse` 的步骤（顺序执行；空表示不跑浏览脚本）。
#[must_use]
pub fn browse_steps() -> Vec<BrowseStep> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.browse_steps.clone())
}

/// `--qa-export` 的目标路径（数据就绪后由页面触发导出）。
#[must_use]
pub fn export_path() -> Option<PathBuf> {
    ARGS.get().and_then(|args| args.export_path.clone())
}

/// `--qa-export-hold`：停在导出确认对话框（截图用）。
#[must_use]
pub fn export_hold() -> bool {
    ARGS.get().is_some_and(|args| args.export_hold)
}

/// `--qa-export-cancel-after-rows`：写满多少行后取消（验证清理）。
#[must_use]
pub fn export_cancel_after_rows() -> Option<u64> {
    ARGS.get().and_then(|args| args.export_cancel_after_rows)
}

/// `--qa-export-raw-values`：导出时关闭公式注入防护（写原始值）。
#[must_use]
pub fn export_raw_values() -> bool {
    ARGS.get().is_some_and(|args| args.export_raw_values)
}

/// `--qa-export-scope`：导出范围（默认当前页；`table` 走整表分页导出）。
#[must_use]
pub fn export_scope() -> ExportScopeArg {
    ARGS.get()
        .map_or(ExportScopeArg::Page, |args| args.export_scope)
}

/// 是否登记了标签脚本（上限对话框在无人值守时会自动关闭）。
#[must_use]
pub fn tabs_requested() -> bool {
    ARGS.get().is_some_and(|args| args.tabs_script.is_some())
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

/// 是否重放「事务已失败」这条失败路径（`--qa-data-fail-txn`）。
#[must_use]
pub fn data_fail_transaction() -> bool {
    ARGS.get().is_some_and(|args| args.data_fail_txn)
}

/// 批量编辑的区间（`--qa-data-edit-range`，按出现顺序应用）。
#[must_use]
pub fn data_edit_ranges() -> Vec<(u32, u32, String, String)> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.data_edit_ranges.clone())
}

/// 点「提交更改」后多久点「取消提交」（毫秒）。
#[must_use]
pub fn data_cancel_after_ms() -> Option<u64> {
    ARGS.get().and_then(|args| args.data_cancel_after_ms)
}

/// `--qa-close-dirty` 的答案（未启用时为 `None`）：脏行建好后触发关窗守卫。
#[must_use]
pub fn close_dirty() -> Option<CloseDirtyAnswer> {
    ARGS.get().and_then(|args| args.close_dirty)
}

/// `--qa-quit`：用产品的 `app.quit`（`Ctrl+Q`）代替 `app.close-window` 触发同一条守卫。
#[must_use]
pub fn quit_action() -> bool {
    ARGS.get().is_some_and(|args| args.quit_action)
}

/// `--qa-expect-submit` 的断言（截图时刻校验）。
#[must_use]
pub fn expect_submit() -> Vec<SubmitExpectation> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_submit.clone())
}

/// 是否重放「批次被取消」的响应（`--qa-data-replay-cancelled`）。
#[must_use]
pub fn data_replay_cancelled() -> bool {
    ARGS.get().is_some_and(|args| args.data_replay_cancelled)
}

/// 截图时要求成立的数据页状态。
#[must_use]
pub fn expect_data_state() -> Option<DataExpectation> {
    ARGS.get().and_then(|args| args.expect_data_state)
}

/// 截图时要求成立的事务徽标状态。
#[must_use]
pub fn expect_txn() -> Option<TxnExpectation> {
    ARGS.get().and_then(|args| args.expect_txn)
}

/// 截图时状态圆点 tooltip 必须包含的文案（`--qa-expect-status-dot`）。
#[must_use]
pub fn expect_status_dot() -> Option<String> {
    ARGS.get().and_then(|args| args.expect_status_dot.clone())
}

/// 截图时「开始事务」按钮必须的可点状态（`--qa-expect-begin`）。
#[must_use]
pub fn expect_begin() -> Option<bool> {
    ARGS.get().and_then(|args| args.expect_begin)
}

/// 打开表后要展开列信息浮层的目标列（`--qa-column-info`）。
#[must_use]
pub fn column_info() -> Option<String> {
    ARGS.get().and_then(|args| args.column_info.clone())
}

/// 要在截图前设置的列宽（`--qa-column-width`，按登记顺序；#133）。
#[must_use]
pub fn column_widths() -> Vec<(String, i32)> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.column_widths.clone())
}

/// 列信息浮层上必须显示的文案（`--qa-expect-column`，按登记顺序）。
#[must_use]
pub fn expect_columns() -> Vec<ColumnExpectation> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_columns.clone())
}

/// 依次要展开的节点名（`--qa-tree-expand`，按出现顺序）。
#[must_use]
pub fn tree_expand() -> Vec<String> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.tree_expand.clone())
}

/// 要在树里打开的表名（`--qa-tree-open`）。
#[must_use]
pub fn tree_open() -> Option<String> {
    ARGS.get().and_then(|args| args.tree_open.clone())
}

/// 是否触发一次 `win.refresh`（`--qa-refresh`）。
#[must_use]
pub fn refresh_requested() -> bool {
    ARGS.get().is_some_and(|args| args.refresh)
}

/// 要激活（双击 / 回车同一条 `activate` 信号）的节点名（`--qa-tree-activate`）。
#[must_use]
pub fn tree_activate() -> Option<String> {
    ARGS.get().and_then(|args| args.tree_activate.clone())
}

/// 搜索关键词（`--qa-tree-search`）。
#[must_use]
pub fn tree_search() -> Option<String> {
    ARGS.get().and_then(|args| args.tree_search.clone())
}

/// 走搜索框同一条输入路径的关键词（`--qa-search`，含 250ms 防抖）。
#[must_use]
pub fn search_query() -> Option<String> {
    ARGS.get().and_then(|args| args.search_query.clone())
}

/// 搜索后是否清空输入框（`--qa-search-clear`，走清空 / `Esc` 同一条清理路径）。
#[must_use]
pub fn search_clear() -> bool {
    ARGS.get().is_some_and(|args| args.search_clear)
}

/// 是否在搜索框上按一次回车（`--qa-search-enter`）。
#[must_use]
pub fn search_enter() -> bool {
    ARGS.get().is_some_and(|args| args.search_enter)
}

/// 结果树断言（`--qa-expect-search`，按登记顺序）。
#[must_use]
pub fn expect_search() -> Vec<SearchExpectation> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_search.clone())
}

/// 是否点第一条搜索结果（`--qa-tree-click-result`）。
#[must_use]
pub fn tree_click_result() -> bool {
    ARGS.get().is_some_and(|args| args.tree_click_result)
}

/// 是否点结果里第一个可打开的对象行（`--qa-search-click-object`，v0.2.4）。
#[must_use]
pub fn search_click_object() -> bool {
    ARGS.get().is_some_and(|args| args.search_click_object)
}

/// 是否点「加载更多…」（`--qa-tree-load-more`）。
#[must_use]
pub fn tree_load_more() -> bool {
    ARGS.get().is_some_and(|args| args.tree_load_more)
}

/// 要断开浏览会话的连接名（`--qa-tree-disconnect`）。
#[must_use]
pub fn tree_disconnect() -> Option<String> {
    ARGS.get().and_then(|args| args.tree_disconnect.clone())
}

/// 只选中（不打开）的对象名（`--qa-tree-select`，#119）。
#[must_use]
pub fn tree_select() -> Option<String> {
    ARGS.get().and_then(|args| args.tree_select.clone())
}

/// 要选中的连接节点名（`--qa-tree-select-connection`，#119）。
#[must_use]
pub fn tree_select_connection() -> Option<String> {
    ARGS.get()
        .and_then(|args| args.tree_select_connection.clone())
}

/// `--qa-tree-select-group <tables|views>`：选中种类分组行。
#[must_use]
pub fn tree_select_group() -> Option<String> {
    ARGS.get().and_then(|args| args.tree_select_group.clone())
}

/// `--qa-tree-menu-group <tables|views|…>`：在种类分组行上打开右键菜单。
#[must_use]
pub fn tree_menu_group() -> Option<String> {
    ARGS.get().and_then(|args| args.tree_menu_group.clone())
}

/// `--qa-tree-menu-pick <label>`：点当前右键菜单里的这一项。
#[must_use]
pub fn tree_menu_pick() -> Option<String> {
    ARGS.get().and_then(|args| args.tree_menu_pick.clone())
}

/// `--qa-objects-select <name>`：选中 `Objects` 页的一行（与单击同一条路径）。
#[must_use]
pub fn objects_select() -> Option<String> {
    ARGS.get().and_then(|args| args.objects_select.clone())
}

/// `--qa-objects-activate <name>`：激活 `Objects` 页的一行（与双击 / 回车同一条路径）。
#[must_use]
pub fn objects_activate() -> Option<String> {
    ARGS.get().and_then(|args| args.objects_activate.clone())
}

/// `--qa-kind <tables|views|functions|queries>`：点主工具条的种类入口。
#[must_use]
pub fn kind_click() -> Option<crate::tabs::KindShortcut> {
    ARGS.get().and_then(|args| args.kind_click)
}

/// 信息面板的页选择（`--qa-inspector`，#119）：只有 `ddl|info`；面板常驻，不能折叠。
#[must_use]
pub fn inspector() -> Option<String> {
    ARGS.get().and_then(|args| args.inspector.clone())
}

/// 把面板拖到的宽度（`--qa-inspector-width`，#119）。
#[must_use]
pub fn inspector_width() -> Option<i32> {
    ARGS.get().and_then(|args| args.inspector_width)
}

/// 信息面板的断言（`--qa-expect-inspector`，按登记顺序）。
#[must_use]
pub fn expect_inspector() -> Vec<InspectorExpectation> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_inspector.clone())
}

/// `Objects` 固定页的断言（`--qa-expect-objects`，按登记顺序；#135）。
#[must_use]
pub fn expect_objects() -> Vec<ObjectsExpectation> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_objects.clone())
}

/// 树 / 搜索的断言（`--qa-expect-tree`，按登记顺序）。
#[must_use]
pub fn expect_tree() -> Vec<TreeExpectation> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_tree.clone())
}

/// DDL 面板的断言（`--qa-expect-ddl`，按登记顺序）。
#[must_use]
pub fn expect_ddl() -> Vec<DdlExpectation> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_ddl.clone())
}

/// 是否登记了任何对象树 / 搜索 / DDL 动作（`maybe_run_qa_tree` 的开关）。
#[must_use]
pub fn tree_requested() -> bool {
    ARGS.get().is_some_and(|args| {
        !args.tree_expand.is_empty()
            || args.tree_open.is_some()
            || args.refresh
            || args.tree_activate.is_some()
            || args.tree_search.is_some()
            || args.search_query.is_some()
            || args.search_enter
            || args.search_clear
            || args.tree_click_result
            || args.search_click_object
            || args.tree_load_more
            || args.tree_disconnect.is_some()
            || args.tree_select.is_some()
            || args.tree_select_connection.is_some()
            || args.tree_select_group.is_some()
            || args.tree_menu_group.is_some()
            || args.tree_menu_pick.is_some()
            || args.objects_select.is_some()
            || args.objects_activate.is_some()
            || args.kind_click.is_some()
            || args.inspector.is_some()
            || args.inspector_width.is_some()
    })
}

/// `--qa-data-insert`：点「新增行」。
#[must_use]
pub fn data_insert() -> bool {
    ARGS.get().is_some_and(|args| args.data_insert)
}

/// `--qa-data-insert-set <column:value>`：给新行填的值（按登记顺序）。
#[must_use]
pub fn data_insert_set() -> Vec<(String, String)> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.data_insert_set.clone())
}

/// `--qa-data-insert-null <column>`：新行里显式写 NULL 的列。
#[must_use]
pub fn data_insert_null() -> Vec<String> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.data_insert_null.clone())
}

/// `--qa-expect-row <column=value>`：重新浏览后必须存在的行（否则退出码 3）。
#[must_use]
pub fn expect_rows() -> Vec<(String, String)> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_rows.clone())
}

/// `--qa-isolation-dialog`：打开隔离级别选择卡片。
#[must_use]
pub fn isolation_dialog() -> bool {
    ARGS.get().is_some_and(|args| args.isolation_dialog)
}

/// `--qa-isolation-level`：预选的隔离级别。
#[must_use]
pub fn isolation_level() -> Option<IsolationChoice> {
    ARGS.get().and_then(|args| args.isolation_level)
}

/// `--qa-isolation-confirm`：在卡片里点「开始事务」。
#[must_use]
pub fn isolation_confirm() -> bool {
    ARGS.get().is_some_and(|args| args.isolation_confirm)
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

/// 取证用的初始窗口尺寸（`--window-size`）；未给出时为 `None`（用产品默认值）。
///
/// 这个开关只用于在 1200×800 与 960×640 两档取截图与目检，不改变产品默认窗口大小。
#[must_use]
pub fn window_size() -> Option<(i32, i32)> {
    ARGS.get().and_then(|args| args.window_size)
}

/// `--tier <compact|comfortable|large>`：起始密度档（未指定时为 `None`，按记忆 / 默认走）。
#[must_use]
pub fn tier() -> Option<crate::layout::Tier> {
    ARGS.get().and_then(|args| args.tier)
}

/// 是否在截图之后做「无裁切」量化布局自检（`--qa-layout-audit`）。
#[must_use]
pub fn layout_audit() -> bool {
    ARGS.get().is_some_and(|args| args.layout_audit)
}

/// 截图时刻是否断言「未连接的启动工作区」（`--qa-startup-workspace`，#139）。
#[must_use]
pub fn startup_workspace() -> bool {
    ARGS.get().is_some_and(|args| args.startup_workspace)
}

/// 握手完成后是否打开「关于 Squarvo」（`--qa-open-about`，#139）。
#[must_use]
pub fn open_about() -> bool {
    ARGS.get().is_some_and(|args| args.open_about)
}

/// 截图前是否打开 headerbar 主菜单（`--qa-open-menu`）。
#[must_use]
pub fn open_menu() -> bool {
    ARGS.get().is_some_and(|args| args.open_menu)
}

/// 启动态侧栏要按名字激活的连接（`--qa-startup-click`，#144）。
#[must_use]
pub fn startup_click() -> Option<String> {
    ARGS.get().and_then(|args| args.startup_click.clone())
}

/// 「按保存凭据连接」的截图时刻断言（`--qa-expect-startup-connect`，可重复；#144）。
#[must_use]
pub fn expect_startup_connect() -> Vec<(String, String)> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_startup_connect.clone())
}

/// 截图时刻要断言的对话框文案（`--qa-expect-dialog`，可重复；#144）。
#[must_use]
pub fn expect_dialog() -> Vec<String> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_dialog.clone())
}

/// 启动态侧栏要打开右键菜单的连接名（`--qa-startup-menu`，#144）。
#[must_use]
pub fn startup_menu() -> Option<String> {
    ARGS.get().and_then(|args| args.startup_menu.clone())
}

/// 右键菜单里要点的条目文案（`--qa-startup-menu-pick`，#144）。
#[must_use]
pub fn startup_menu_pick() -> Option<String> {
    ARGS.get().and_then(|args| args.startup_menu_pick.clone())
}

/// 要删除并自动确认的连接名（`--qa-delete-connection`，#144）。
#[must_use]
pub fn delete_connection() -> Option<String> {
    ARGS.get().and_then(|args| args.delete_connection.clone())
}

/// 「关于」对话框里必须出现的文案（`--qa-expect-about`，可重复；#139）。
#[must_use]
pub fn expect_about() -> Vec<String> {
    ARGS.get()
        .map_or_else(Vec::new, |args| args.expect_about.clone())
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

/// 把 `--opt=value` 展开成 `--opt value`：GLib 的选项解析接受前一种写法，
/// QA 的自解析必须一致，否则「参数没生效」会看起来像功能坏了。
fn expand_equals(args: &[String]) -> Vec<String> {
    let mut expanded = Vec::with_capacity(args.len());
    for arg in args {
        if arg.starts_with("--")
            && let Some((name, value)) = arg.split_once('=')
        {
            expanded.push(name.to_owned());
            expanded.push(value.to_owned());
        } else {
            expanded.push(arg.clone());
        }
    }
    expanded
}

/// 解析 `WIDTHxHEIGHT`：两段都必须是正整数。
///
/// 小于最小窗口（`layout::MIN_WINDOW_*`）的取值**照收**：960×640 已是窗口自己强制的
/// 下限（`ui::activate` 的 `set_size_request`），窗口会被抬到下限；这一档正好用来
/// 验证「强制生效」，实测尺寸看 `--qa-layout-audit` 的「窗口 WxH」那一行。
fn parse_window_size(value: &str) -> Result<(i32, i32), String> {
    let (width, height) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("--window-size 需要 WIDTHxHEIGHT，实际：{value}"))?;
    let width: i32 = width
        .parse()
        .map_err(|_| format!("--window-size 的宽度不是整数：{value}"))?;
    let height: i32 = height
        .parse()
        .map_err(|_| format!("--window-size 的高度不是整数：{value}"))?;
    // 0 / 负数不是尺寸：窗口没有意义，也拼不出合法的截图口径。
    if width <= 0 || height <= 0 {
        return Err(format!("--window-size 需要正整数，实际：{width}x{height}"));
    }
    Ok((width, height))
}

fn parse(args: &[String]) -> Result<Args, String> {
    let expanded = expand_equals(args);
    let args = &expanded[..];
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
            "--qa-report-monitor" => {
                parsed.report_monitor = true;
                index += 1;
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
            "--qa-data-edit-range" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    "--qa-data-edit-range 需要 start:count:column:value".to_owned()
                })?;
                let mut parts = value.splitn(4, ':');
                let start: u32 = parts
                    .next()
                    .ok_or_else(|| format!("--qa-data-edit-range 缺少起始行：{value}"))?
                    .parse()
                    .map_err(|_| format!("--qa-data-edit-range 的起始行不合法：{value}"))?;
                let count: u32 = parts
                    .next()
                    .ok_or_else(|| format!("--qa-data-edit-range 缺少行数：{value}"))?
                    .parse()
                    .map_err(|_| format!("--qa-data-edit-range 的行数不合法：{value}"))?;
                let column = parts
                    .next()
                    .filter(|column| !column.is_empty())
                    .ok_or_else(|| format!("--qa-data-edit-range 缺少列名：{value}"))?;
                let text = parts
                    .next()
                    .ok_or_else(|| format!("--qa-data-edit-range 缺少值：{value}"))?;
                parsed
                    .data_edit_ranges
                    .push((start, count, column.to_owned(), text.to_owned()));
                index += 2;
            }
            "--qa-data-cancel-after-ms" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-data-cancel-after-ms 需要一个毫秒数".to_owned())?;
                let millis: u64 = value
                    .parse()
                    .map_err(|_| format!("--qa-data-cancel-after-ms 不是合法毫秒数：{value}"))?;
                parsed.data_cancel_after_ms = Some(millis);
                index += 2;
            }
            "--qa-close-dirty" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-close-dirty 需要 answer=discard|cancel".to_owned())?;
                parsed.close_dirty = Some(CloseDirtyAnswer::parse(value)?);
                index += 2;
            }
            "--qa-quit" => {
                parsed.quit_action = true;
                index += 1;
            }
            "--qa-expect-submit" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-submit 需要 字段=值".to_owned())?;
                parsed.expect_submit.push(SubmitExpectation::parse(value)?);
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
            "--qa-data-fail-txn" => {
                parsed.data_fail_txn = true;
                index += 1;
            }
            "--qa-data-replay-cancelled" => {
                parsed.data_replay_cancelled = true;
                index += 1;
            }
            "--qa-expect-data-state" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-data-state 需要一个状态名".to_owned())?;
                parsed.expect_data_state = Some(DataExpectation::parse(value)?);
                index += 2;
            }
            "--qa-expect-txn" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-txn 需要一个徽标状态".to_owned())?;
                parsed.expect_txn = Some(TxnExpectation::parse(value)?);
                index += 2;
            }
            "--qa-expect-status-dot" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-status-dot 需要一段 tooltip 文案".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-expect-status-dot 的文案不能为空".to_owned());
                }
                parsed.expect_status_dot = Some(value.clone());
                index += 2;
            }
            "--qa-expect-begin" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-begin 需要 enabled|disabled".to_owned())?;
                parsed.expect_begin = Some(match value.as_str() {
                    "enabled" => true,
                    "disabled" => false,
                    other => {
                        return Err(format!(
                            "--qa-expect-begin 只接受 enabled|disabled，实际：{other}"
                        ));
                    }
                });
                index += 2;
            }
            "--qa-column-info" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-column-info 需要一个列名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-column-info 的列名不能为空".to_owned());
                }
                parsed.column_info = Some(value.clone());
                index += 2;
            }
            "--qa-column-width" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-column-width 需要 column:width".to_owned())?;
                let (column, width) = value
                    .split_once(':')
                    .ok_or_else(|| format!("--qa-column-width 需要 column:width，实际：{value}"))?;
                if column.trim().is_empty() {
                    return Err(format!("--qa-column-width 的列名为空：{value}"));
                }
                let width: i32 = width
                    .trim()
                    .parse()
                    .map_err(|_| format!("--qa-column-width 的宽度不是合法像素值：{value}"))?;
                if width <= 0 {
                    return Err(format!("--qa-column-width 的宽度必须为正数：{value}"));
                }
                parsed.column_widths.push((column.to_owned(), width));
                index += 2;
            }
            "--qa-expect-column" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-column 需要 列名:字段=文案".to_owned())?;
                parsed.expect_columns.push(ColumnExpectation::parse(value)?);
                index += 2;
            }
            // ---- #88：插入行与隔离级别 ----
            "--qa-data-insert" => {
                parsed.data_insert = true;
                index += 1;
            }
            "--qa-data-insert-set" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-data-insert-set 需要 column:value".to_owned())?;
                let (column, text) = value.split_once(':').ok_or_else(|| {
                    format!("--qa-data-insert-set 需要 column:value，实际：{value}")
                })?;
                if column.trim().is_empty() {
                    return Err(format!("--qa-data-insert-set 的列名为空：{value}"));
                }
                parsed
                    .data_insert_set
                    .push((column.to_owned(), text.to_owned()));
                index += 2;
            }
            "--qa-data-insert-null" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-data-insert-null 需要一个列名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-data-insert-null 的列名不能为空".to_owned());
                }
                parsed.data_insert_null.push(value.clone());
                index += 2;
            }
            "--qa-expect-row" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-row 需要 column=value".to_owned())?;
                let (column, expected) = value
                    .split_once('=')
                    .ok_or_else(|| format!("--qa-expect-row 需要 column=value，实际：{value}"))?;
                if column.trim().is_empty() {
                    return Err(format!("--qa-expect-row 的列名为空：{value}"));
                }
                parsed
                    .expect_rows
                    .push((column.to_owned(), expected.to_owned()));
                index += 2;
            }
            "--qa-isolation-dialog" => {
                parsed.isolation_dialog = true;
                index += 1;
            }
            "--qa-isolation-level" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-isolation-level 需要一个级别名".to_owned())?;
                parsed.isolation_level = Some(IsolationChoice::parse(value)?);
                index += 2;
            }
            "--qa-isolation-confirm" => {
                parsed.isolation_confirm = true;
                index += 1;
            }
            "--qa-layout-audit" => {
                parsed.layout_audit = true;
                index += 1;
            }
            "--qa-startup-workspace" => {
                parsed.startup_workspace = true;
                index += 1;
            }
            "--qa-startup-click" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-startup-click 需要一个连接名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-startup-click 的连接名不能为空".to_owned());
                }
                parsed.startup_click = Some(value.clone());
                index += 2;
            }
            "--qa-expect-startup-connect" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-startup-connect 需要 字段=值".to_owned())?;
                let (field, expected) = value.split_once('=').ok_or_else(|| {
                    format!("--qa-expect-startup-connect 需要 字段=值，实际：{value}")
                })?;
                if field.trim().is_empty() {
                    return Err(format!("--qa-expect-startup-connect 的字段为空：{value}"));
                }
                parsed
                    .expect_startup_connect
                    .push((field.trim().to_owned(), expected.trim().to_owned()));
                index += 2;
            }
            "--qa-startup-menu" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-startup-menu 需要一个连接名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-startup-menu 的连接名不能为空".to_owned());
                }
                parsed.startup_menu = Some(value.clone());
                index += 2;
            }
            "--qa-startup-menu-pick" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-startup-menu-pick 需要一个菜单项文案".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-startup-menu-pick 的菜单项文案不能为空".to_owned());
                }
                parsed.startup_menu_pick = Some(value.clone());
                index += 2;
            }
            "--qa-delete-connection" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-delete-connection 需要一个连接名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-delete-connection 的连接名不能为空".to_owned());
                }
                parsed.delete_connection = Some(value.clone());
                index += 2;
            }
            "--qa-expect-dialog" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-dialog 需要一个文案子串".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-expect-dialog 的文案子串不能为空".to_owned());
                }
                parsed.expect_dialog.push(value.clone());
                index += 2;
            }
            "--qa-open-about" => {
                parsed.open_about = true;
                index += 1;
            }
            "--qa-open-menu" => {
                parsed.open_menu = true;
                index += 1;
            }
            "--qa-expect-about" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-about 需要一个文案子串".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-expect-about 的文案子串不能为空".to_owned());
                }
                parsed.expect_about.push(value.clone());
                index += 2;
            }
            "--qa-browse" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-browse 需要一个步骤".to_owned())?;
                parsed.browse_steps.push(BrowseStep::parse(value)?);
                index += 2;
            }
            "--qa-tabs-file" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-tabs-file 需要一个脚本路径".to_owned())?;
                parsed.tabs_script = Some(
                    std::fs::read_to_string(value)
                        .map_err(|error| format!("--qa-tabs-file 读取 {value} 失败：{error}"))?,
                );
                index += 2;
            }
            "--qa-export" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-export 需要一个目标路径".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-export 的目标路径不能为空".to_owned());
                }
                parsed.export_path = Some(PathBuf::from(value));
                index += 2;
            }
            "--qa-export-hold" => {
                parsed.export_hold = true;
                index += 1;
            }
            "--qa-export-raw-values" => {
                parsed.export_raw_values = true;
                index += 1;
            }
            "--qa-export-scope" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-export-scope 需要 page 或 table".to_owned())?;
                parsed.export_scope = ExportScopeArg::parse(value)?;
                index += 2;
            }
            "--qa-export-cancel-after-rows" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-export-cancel-after-rows 需要行数".to_owned())?;
                let rows: u64 = value
                    .parse()
                    .map_err(|_| format!("--qa-export-cancel-after-rows 不是合法行数：{value}"))?;
                parsed.export_cancel_after_rows = Some(rows);
                index += 2;
            }
            "--qa-tree-expand" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-tree-expand 需要一个节点名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-tree-expand 的节点名不能为空".to_owned());
                }
                parsed.tree_expand.push(value.clone());
                index += 2;
            }
            "--qa-tree-open" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-tree-open 需要一个表名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-tree-open 的表名不能为空".to_owned());
                }
                parsed.tree_open = Some(value.clone());
                index += 2;
            }
            "--qa-refresh" => {
                parsed.refresh = true;
                index += 1;
            }
            "--qa-tree-activate" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-tree-activate 需要一个表名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-tree-activate 的表名不能为空".to_owned());
                }
                parsed.tree_activate = Some(value.clone());
                index += 2;
            }
            "--qa-tree-search" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-tree-search 需要一个关键词".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-tree-search 的关键词不能为空".to_owned());
                }
                parsed.tree_search = Some(value.clone());
                index += 2;
            }
            "--qa-search" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-search 需要一个关键词".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-search 的关键词不能为空".to_owned());
                }
                parsed.search_query = Some(value.clone());
                index += 2;
            }
            "--qa-search-clear" => {
                parsed.search_clear = true;
                index += 1;
            }
            "--qa-search-enter" => {
                parsed.search_enter = true;
                index += 1;
            }
            "--qa-expect-search" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-search 需要 字段=值".to_owned())?;
                parsed.expect_search.push(SearchExpectation::parse(value)?);
                index += 2;
            }
            "--qa-tree-click-result" => {
                parsed.tree_click_result = true;
                index += 1;
            }
            "--qa-search-click-object" => {
                parsed.search_click_object = true;
                index += 1;
            }
            "--qa-tree-load-more" => {
                parsed.tree_load_more = true;
                index += 1;
            }
            "--qa-tree-disconnect" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-tree-disconnect 需要一个连接名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-tree-disconnect 的连接名不能为空".to_owned());
                }
                parsed.tree_disconnect = Some(value.clone());
                index += 2;
            }
            "--qa-tree-select" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-tree-select 需要一个对象名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-tree-select 的对象名不能为空".to_owned());
                }
                parsed.tree_select = Some(value.clone());
                index += 2;
            }
            "--qa-tree-select-connection" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-tree-select-connection 需要一个连接名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-tree-select-connection 的连接名不能为空".to_owned());
                }
                parsed.tree_select_connection = Some(value.clone());
                index += 2;
            }
            "--qa-objects-select" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-objects-select 需要一个对象名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-objects-select 的对象名不能为空".to_owned());
                }
                parsed.objects_select = Some(value.clone());
                index += 2;
            }
            "--qa-objects-activate" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-objects-activate 需要一个对象名".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-objects-activate 的对象名不能为空".to_owned());
                }
                parsed.objects_activate = Some(value.clone());
                index += 2;
            }
            "--qa-tree-select-group" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    "--qa-tree-select-group 需要 tables|views|functions|queries".to_owned()
                })?;
                if !matches!(
                    value.to_ascii_lowercase().as_str(),
                    "tables"
                        | "table"
                        | "views"
                        | "view"
                        | "functions"
                        | "function"
                        | "queries"
                        | "query"
                ) {
                    return Err(format!(
                        "--qa-tree-select-group 只接受 tables|views|functions|queries，实际：{value}"
                    ));
                }
                parsed.tree_select_group = Some(value.clone());
                index += 2;
            }
            "--qa-tree-menu-group" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-tree-menu-group 需要 tables|views".to_owned())?;
                // 分组行一律带「刷新这一组」（`menu_items`），取值就按分组种类给全。
                if !matches!(
                    value.to_ascii_lowercase().as_str(),
                    "tables"
                        | "table"
                        | "views"
                        | "view"
                        | "functions"
                        | "function"
                        | "queries"
                        | "query"
                ) {
                    return Err(format!(
                        "--qa-tree-menu-group 只接受 tables|views|functions|queries，实际：{value}"
                    ));
                }
                parsed.tree_menu_group = Some(value.clone());
                index += 2;
            }
            "--qa-tree-menu-pick" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-tree-menu-pick 需要一个菜单条目文案".to_owned())?;
                if value.trim().is_empty() {
                    return Err("--qa-tree-menu-pick 的条目文案不能为空".to_owned());
                }
                parsed.tree_menu_pick = Some(value.clone());
                index += 2;
            }
            "--qa-kind" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-kind 需要 tables|views|functions|queries".to_owned())?;
                let shortcut = crate::tabs::KindShortcut::parse(value).ok_or_else(|| {
                    format!("--qa-kind 只接受 tables|views|functions|queries，实际：{value}")
                })?;
                parsed.kind_click = Some(shortcut);
                index += 2;
            }
            "--qa-inspector" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-inspector 需要 ddl|info".to_owned())?;
                // v0.2.3.2：信息面板常驻，`hidden` / `shown` / `toggle` 不再有意义。
                // 解析阶段就拒掉，避免脚本以为「折叠 / 展开」被执行过。
                if !matches!(value.as_str(), "ddl" | "info") {
                    return Err(format!(
                        "--qa-inspector 只接受 ddl|info（信息面板常驻，不能折叠），实际：{value}"
                    ));
                }
                parsed.inspector = Some(value.clone());
                index += 2;
            }
            "--qa-expect-tree" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-tree 需要 字段=值".to_owned())?;
                parsed.expect_tree.push(TreeExpectation::parse(value)?);
                index += 2;
            }
            "--qa-inspector-width" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-inspector-width 需要一个像素宽度".to_owned())?;
                let width: i32 = value
                    .parse()
                    .map_err(|_| format!("--qa-inspector-width 不是合法像素宽度：{value}"))?;
                parsed.inspector_width = Some(width);
                index += 2;
            }
            "--qa-expect-inspector" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-inspector 需要 字段=值".to_owned())?;
                parsed
                    .expect_inspector
                    .push(InspectorExpectation::parse(value)?);
                index += 2;
            }
            "--qa-expect-objects" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-objects 需要 字段=值".to_owned())?;
                parsed
                    .expect_objects
                    .push(ObjectsExpectation::parse(value)?);
                index += 2;
            }
            "--qa-expect-ddl" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--qa-expect-ddl 需要 字段=值".to_owned())?;
                parsed.expect_ddl.push(DdlExpectation::parse(value)?);
                index += 2;
            }
            "--window-size" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--window-size 需要 WIDTHxHEIGHT".to_owned())?;
                parsed.window_size = Some(parse_window_size(value)?);
                index += 2;
            }
            "--tier" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--tier 需要 compact|comfortable|large".to_owned())?;
                parsed.tier = Some(crate::layout::Tier::from_id(value).ok_or_else(|| {
                    format!("--tier 只能是 compact|comfortable|large，实际：{value}")
                })?);
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
            // 未知的 GLib 选项（`--window-size` 等由 GApplication 自己消费）照旧跳过；
            // 但未知的 `--qa-*` 必须报错：v0.2.3.2 删掉的参数（如 `--qa-tree-ddl`）不能
            // 被安静地忽略成「脚本跑完了、什么都没做」。
            other if other.starts_with("--qa-") => {
                return Err(format!(
                    "未知的 QA 参数：{other}（v0.2.3.2 移除了旧的对象树 DDL 参数）"
                ));
            }
            _ => index += 1,
        }
    }
    parsed.capture = path.map(|path| Capture { path, delay });
    // 关窗守卫的答案要落在真实脏行上：没有 `--qa-data-table` 就没有编辑步骤，
    // 「跑了但没弹窗」看起来会像通过，所以这里直接报错。
    if parsed.close_dirty.is_some() && parsed.data_table.is_none() {
        return Err(
            "--qa-close-dirty 需要 --qa-data-table（先用编辑 / 标记删除步骤建出脏行）".to_owned(),
        );
    }
    // `--qa-quit` 只改变「由哪个动作触发守卫」，本身不是触发条件：少了 close-dirty
    // 就没有答案可送，跑完会变成一个看不出结论的「什么都没发生」。
    if parsed.quit_action && parsed.close_dirty.is_none() {
        return Err("--qa-quit 需要 --qa-close-dirty answer=discard|cancel".to_owned());
    }
    // 点结果行必须先有结果：没有搜索参数时这一步只会重试 40 次后静悄悄过去，
    // 直接拒掉才能看出是脚本写错了。
    if parsed.search_click_object && parsed.search_query.is_none() && parsed.tree_search.is_none() {
        return Err(
            "--qa-search-click-object 需要 --qa-search 或 --qa-tree-search（先跑一次搜索）"
                .to_owned(),
        );
    }
    if parsed.search_enter && parsed.search_query.is_none() && parsed.tree_search.is_none() {
        return Err(
            "--qa-search-enter 需要 --qa-search 或 --qa-tree-search（先搜一次，再验证文本不变的重复搜索）"
                .to_owned(),
        );
    }
    // 期望值只在截图那一刻校验：没有截图就没有「这一刻」，静默忽略会让
    // 「跑了但没断言」看起来像通过。
    if parsed.capture.is_none()
        && (parsed.expect_data_state.is_some()
            || parsed.expect_txn.is_some()
            || parsed.expect_status_dot.is_some()
            || parsed.expect_begin.is_some()
            || !parsed.expect_columns.is_empty()
            || !parsed.expect_tree.is_empty()
            || !parsed.expect_ddl.is_empty()
            || !parsed.expect_objects.is_empty()
            || !parsed.expect_submit.is_empty())
    {
        return Err("--qa-expect-* 必须与 --screenshot 一起使用（断言发生在截图时刻）".to_owned());
    }
    if parsed.layout_audit && parsed.capture.is_none() {
        return Err(
            "--qa-layout-audit 必须与 --screenshot 一起使用（自检发生在截图时刻）".to_owned(),
        );
    }
    if (parsed.startup_workspace
        || parsed.open_about
        || parsed.open_menu
        || !parsed.expect_about.is_empty())
        && parsed.capture.is_none()
    {
        return Err(
            "--qa-startup-workspace / --qa-open-about / --qa-open-menu / --qa-expect-about 必须与 --screenshot 一起使用"
                .to_owned(),
        );
    }
    if (!parsed.expect_dialog.is_empty() || !parsed.expect_startup_connect.is_empty())
        && parsed.capture.is_none()
    {
        return Err(
            "--qa-expect-startup-connect / --qa-expect-dialog 必须与 --screenshot 一起使用（断言发生在截图时刻）"
                .to_owned(),
        );
    }
    Ok(parsed)
}

/// 读取并解析 QA 档案；任何问题都报错退出，避免「档案没生效却以为通过了」。
///
/// 必填字段按驱动族判定（[`crate::driver_form::fields_for`] 是同一张表）：
/// MariaDB 要主机 / 用户名（库名可选，留空 = 浏览所有库）；SQLite 要一个
/// 存在的数据库文件路径。
fn load_profile(path: &str) -> Result<QaProfile, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("无法读取 --qa-profile 文件 {path}：{error}"))?;
    let profile: QaProfile = serde_json::from_str(&text)
        .map_err(|error| format!("--qa-profile 文件 {path} 不是合法的连接档案：{error}"))?;
    let Some(family) = crate::backend::connection::DriverFamily::for_id(&profile.driver) else {
        // 驱动清单来自后端，但字段集必须由前端认识；不认识就明确报错，
        // 不能拿 MariaDB 的字段去填一个 SQLite 连接。
        return Err(format!(
            "--qa-profile 的 driver「{}」还没有前端连接表单（当前支持 mariadb / sqlite）",
            profile.driver
        ));
    };
    match family {
        crate::backend::connection::DriverFamily::MariaDb => {
            // 库名不是必填（ADR 0024 §3）：档案里省略 `database` 就是「无默认库」，
            // 与连接对话框的校验同一口径。
            if profile.host.trim().is_empty() || profile.username.trim().is_empty() {
                return Err("--qa-profile 缺少 host / username（MariaDB 连接需要）".to_owned());
            }
        }
        crate::backend::connection::DriverFamily::Sqlite => {
            let Some(database_path) = profile.path.as_deref().map(str::trim) else {
                return Err("--qa-profile 缺少 path（SQLite 连接需要数据库文件路径）".to_owned());
            };
            if database_path.is_empty() {
                return Err("--qa-profile 的 path 不能为空".to_owned());
            }
            if database_path != crate::driver_form::SQLITE_MEMORY_PATH
                && !std::path::Path::new(database_path).exists()
            {
                return Err(format!(
                    "--qa-profile 的数据库文件不存在：{database_path}（请先用 sqlite3 创建）"
                ));
            }
            if profile.read_only && database_path == crate::driver_form::SQLITE_MEMORY_PATH {
                return Err("--qa-profile 的 read_only 不能用于内存数据库（:memory:）".to_owned());
            }
        }
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
    // 第一帧可能还拿不到内容（刚切页 / 刚换选中 / 首帧合成未完成，实测偶发且能持续
    // 数秒）：先多次重试，重试要**继续跑事件循环**，光 sleep 不会触发重绘。
    match snapshot_with_retries(target, 12) {
        Ok((node, width, height)) => render_node(anchor, &node, width, height, path),
        Err(error) => {
            // 退路：`GtkWindow` 自身偶尔交不出节点（窗口已 mapped 却没有可渲染内容），
            // 但它的内容子控件能画——拍内容子控件，画面里 headerbar 一样在。
            let widget = target.clone().upcast::<gtk::Widget>();
            let Some(content) = widget.first_child() else {
                return Err(error);
            };
            match snapshot_with_retries(&content, 6) {
                Ok((node, width, height)) => {
                    eprintln!("[qa] 窗口自身拿不到画面，改拍内容子控件（{width}x{height}）");
                    render_node(anchor, &node, width, height, path)
                }
                Err(_) => Err(error),
            }
        }
    }
}

/// 重试 `attempts` 次取快照；每次之间让出 40ms 并继续跑主循环。
fn snapshot_with_retries(
    target: &impl IsA<gtk::Widget>,
    attempts: u32,
) -> Result<(gtk::gsk::RenderNode, i32, i32), String> {
    let mut last_error = String::new();
    for attempt in 0..attempts {
        if attempt > 0 {
            let context = gtk::glib::MainContext::default();
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(40);
            while std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(4));
            }
        }
        match snapshot_node(target) {
            Ok(node) => {
                if attempt > 0 {
                    eprintln!("[qa] 截图重试 {attempt} 次后拿到画面");
                }
                return Ok(node);
            }
            Err(message) => last_error = message,
        }
    }
    Err(last_error)
}

/// 取 `target` 当前的绘制节点与它的尺寸（拿不到内容时给出窗口状态，便于判断是「还没
/// 渲染」还是「窗口根本没映射」）。
fn snapshot_node(
    target: &impl IsA<gtk::Widget>,
) -> Result<(gtk::gsk::RenderNode, i32, i32), String> {
    let width = target.width();
    let height = target.height();
    if width <= 0 || height <= 0 {
        return Err(format!("目标控件尺寸无效：{width}x{height}"));
    }
    let widget = target.clone().upcast::<gtk::Widget>();
    let paintable = gtk::WidgetPaintable::new(Some(&widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node().ok_or_else(|| {
        format!(
            "控件快照为空（界面尚未渲染）：{width}x{height}，mapped={}，realized={}，\
             visible={}，opacity={:.2}；子层：{}",
            widget.is_mapped(),
            widget.is_realized(),
            widget.is_visible(),
            widget.opacity(),
            describe_children(&widget)
        )
    })?;
    Ok((node, width, height))
}

/// 失败诊断：顺着前两层子控件看谁画不出节点、谁被分到 0 尺寸。
fn describe_children(widget: &gtk::Widget) -> String {
    fn walk(widget: &gtk::Widget, depth: u32, parts: &mut Vec<String>) {
        if depth > 3 || parts.len() >= 16 {
            return;
        }
        let mut child = widget.first_child();
        let mut index = 0;
        while let Some(current) = child {
            if index < 3 && parts.len() < 16 {
                let width = current.width().max(1);
                let height = current.height().max(1);
                let paintable = gtk::WidgetPaintable::new(Some(&current));
                let snapshot = gtk::Snapshot::new();
                paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
                parts.push(format!(
                    "{}#{} {} {}x{} node={} vis={} mapped={}",
                    "  ".repeat(depth as usize),
                    index,
                    current.type_().name(),
                    current.width(),
                    current.height(),
                    snapshot.to_node().is_some(),
                    current.is_visible(),
                    current.is_mapped()
                ));
                walk(&current, depth + 1, parts);
            }
            index += 1;
            child = current.next_sibling();
        }
    }
    let mut parts = Vec::new();
    walk(widget, 0, &mut parts);
    if parts.is_empty() {
        "（没有子控件）".to_owned()
    } else {
        parts.join("；")
    }
}

/// 用锚点窗口的 `GskRenderer` 把节点渲染成 PNG。
fn render_node(
    anchor: &(impl IsA<gtk::Widget> + IsA<gtk::Native>),
    node: &gtk::gsk::RenderNode,
    width: i32,
    height: i32,
    path: &std::path::Path,
) -> Result<(), String> {
    let native = anchor
        .native()
        .ok_or_else(|| "锚点窗口还没有 GdkSurface".to_owned())?;
    let renderer = native
        .renderer()
        .ok_or_else(|| "锚点窗口还没有 GskRenderer".to_owned())?;
    let viewport = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(node, Some(&viewport));
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

// ---------------------------------------------------------------------------
// #121：验收用的两个计数/焦点探针（只读，不改任何产品路径）
// ---------------------------------------------------------------------------

/// 对象树真正发出 `ListNodes` 的次数（每次展开一层 +1）。
///
/// 只由 [`crate::tree`] 在展开请求的同一处调用；正常启动不会读它。验收脚本用它
/// 断言「展开一个 catalog 的 RPC 次数 = 分组数」，而不是「预取全部」。
static LIST_NODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `mark-list-nodes` 记下的基线。
static LIST_NODES_MARK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 记一次 `ListNodes` 展开请求（树调用；断言见 [`list_nodes_since_mark`]）。
pub fn note_list_nodes() {
    LIST_NODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// 把当前计数记为基线（脚本步骤 `mark-list-nodes`）。
pub fn mark_list_nodes() {
    LIST_NODES_MARK.store(
        LIST_NODES.load(std::sync::atomic::Ordering::Relaxed),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// 自基线以来发出了多少次 `ListNodes`。
#[must_use]
pub fn list_nodes_since_mark() -> u64 {
    LIST_NODES
        .load(std::sync::atomic::Ordering::Relaxed)
        .saturating_sub(LIST_NODES_MARK.load(std::sync::atomic::Ordering::Relaxed))
}

/// 本次进程累计发出了多少次 `ListNodes`（日志/诊断用）。
#[must_use]
pub fn list_nodes_total() -> u64 {
    LIST_NODES.load(std::sync::atomic::Ordering::Relaxed)
}

/// 只断言（不抓取）焦点在对象树搜索框上。
///
/// 用于验收「产品快捷键 `Ctrl+F` 真的把焦点送过去」：先用 `activate_action` 触发动作，
/// 再调本函数读回真实焦点链。它不 `grab_focus`，所以动作没生效时断言会红。
pub fn assert_tree_search_focused(window: &gtk::Window) -> Result<String, String> {
    let entry = find_tree_search(window.upcast_ref::<gtk::Widget>())
        .ok_or_else(|| "对象树搜索框尚未创建".to_owned())?;
    let widget = entry.clone().upcast::<gtk::Widget>();
    let focus = gtk::prelude::GtkWindowExt::focus(window);
    let focused = focus
        .as_ref()
        .is_some_and(|current| widget == *current || is_descendant_of(current, &widget));
    if !focused {
        let current = focus
            .map(|widget| widget.type_().name().to_string())
            .unwrap_or_else(|| "None".to_owned());
        return Err(format!("焦点不在对象树搜索框上（当前焦点={current}）"));
    }
    Ok(entry
        .placeholder_text()
        .map(|text| text.to_string())
        .unwrap_or_default())
}

// 最近一次呈现的模态提示（导出完成等）：退出前由 QA 收起。
//
// 模态 `AdwAlertDialog` 会拦住 `window.close()`；产品里由用户点「关闭」收场，
// 无人值守时没有这个点击，所以退出步骤需要拿到同一个对象。
thread_local! {
    static MODAL_NOTICE: RefCell<Option<adw::AlertDialog>> = const { RefCell::new(None) };
}

/// 登记最近一次模态提示（只在提示构造处调用；正常交互路径不读它）。
pub fn register_modal(dialog: &adw::AlertDialog) {
    MODAL_NOTICE.with(|slot| *slot.borrow_mut() = Some(dialog.clone()));
}

/// 收起登记的模态提示；返回是否真的收起了一个。
pub fn close_modal() -> bool {
    use adw::prelude::AdwDialogExt;
    MODAL_NOTICE.with(|slot| {
        let Some(dialog) = slot.borrow_mut().take() else {
            return false;
        };
        if dialog.is_visible() {
            dialog.force_close();
            return true;
        }
        false
    })
}

/// 焦点控件是否位于 `ancestor` 之内（`GtkSearchEntry` 的真实焦点是内部的 `GtkText`）。
fn is_descendant_of(widget: &gtk::Widget, ancestor: &gtk::Widget) -> bool {
    let mut current = widget.parent();
    while let Some(parent) = current {
        if parent == *ancestor {
            return true;
        }
        current = parent.parent();
    }
    false
}

/// 深度优先找树顶部的搜索框（按 placeholder 识别，不依赖控件层级/名字）。
fn find_tree_search(widget: &gtk::Widget) -> Option<gtk::SearchEntry> {
    if let Some(entry) = widget.downcast_ref::<gtk::SearchEntry>()
        && entry
            .placeholder_text()
            .is_some_and(|text| text.as_str().starts_with("搜索连接"))
    {
        return Some(entry.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_tree_search(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
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
        assert!(error.contains("缺少"), "{error}");
    }

    #[test]
    fn mariadb_profile_without_a_database_is_valid() {
        // `--qa-profile` 省略 `database` = 无默认库：与连接对话框的可选库名同一口径。
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("profile.json");
        std::fs::write(
            &path,
            r#"{"host":"127.0.0.1","port":53306,"username":"sqarvo_test"}"#,
        )
        .expect("写档案");
        let profile = load_profile(path.to_str().expect("UTF-8 路径")).expect("无默认库档案应有效");
        assert!(profile.database.is_empty());
        assert_eq!(profile.port, 53306);
    }

    #[test]
    fn sqlite_profile_is_parsed_with_path_and_read_only() {
        let dir = tempfile::tempdir().expect("临时目录");
        let database = dir.path().join("demo.db");
        std::fs::write(&database, b"").expect("建库文件");
        let path = dir.path().join("profile.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"driver":"sqlite","path":"{}","read_only":true}}"#,
                database.display()
            ),
        )
        .expect("写档案");

        let profile = load_profile(path.to_str().expect("UTF-8 路径")).expect("解析 SQLite 档案");
        assert_eq!(profile.driver, "sqlite");
        assert_eq!(profile.path.as_deref(), Some(database.to_str().unwrap()));
        assert!(profile.read_only);
        // 服务器型字段在 SQLite 档案里没有意义，缺省为空而不是「必填」。
        assert!(profile.host.is_empty() && profile.database.is_empty());
    }

    #[test]
    fn sqlite_profile_requires_an_existing_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("profile.json");

        std::fs::write(&path, r#"{"driver":"sqlite"}"#).expect("写档案");
        let error = load_profile(path.to_str().expect("UTF-8 路径")).unwrap_err();
        assert!(error.contains("缺少 path"), "{error}");

        let missing = dir.path().join("nope.db");
        std::fs::write(
            &path,
            format!(r#"{{"driver":"sqlite","path":"{}"}}"#, missing.display()),
        )
        .expect("写档案");
        let error = load_profile(path.to_str().expect("UTF-8 路径")).unwrap_err();
        assert!(error.contains("数据库文件不存在"), "{error}");
        assert!(error.contains("nope.db"), "{error}");
    }

    #[test]
    fn profile_with_unknown_driver_is_rejected() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("profile.json");
        std::fs::write(&path, r#"{"driver":"postgresql","host":"h"}"#).expect("写档案");
        let error = load_profile(path.to_str().expect("UTF-8 路径")).unwrap_err();
        // 后端可能列出前端还不认识的驱动：档案必须明确报错，不能拿别的字段集硬填。
        assert!(error.contains("postgresql"), "{error}");
        assert!(error.contains("还没有前端连接表单"), "{error}");
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
    fn tier_flag_takes_one_of_the_three_ids() {
        let parsed = parse(&args(&["app", "--tier", "comfortable"])).expect("解析 --tier");
        assert_eq!(parsed.tier, Some(crate::layout::Tier::Comfortable));
        assert!(enabled_with(&parsed));
        let parsed = parse(&args(&["app", "--tier=large"])).expect("解析 --tier=large");
        assert_eq!(parsed.tier, Some(crate::layout::Tier::Large));
        // 拼错不静默退回默认档：报错文案与 `--window-size` 同一风格。
        let error = parse(&args(&["app", "--tier", "huge"])).expect_err("非法档位要报错");
        assert!(error.contains("compact|comfortable|large"), "{error}");
        assert!(parse(&args(&["app", "--tier"])).is_err(), "缺值要报错");
    }

    /// `parse` 之后的 QA 开关判定（`enabled()` 读全局 OnceLock，单测里直接看字段）。
    fn enabled_with(parsed: &Args) -> bool {
        parsed.tier.is_some() || parsed.capture.is_some() || parsed.report_monitor
    }

    #[test]
    fn report_monitor_is_a_no_argument_switch() {
        assert!(!parse(&args(&["app"])).unwrap().report_monitor);
        let parsed = parse(&args(&["app", "--qa-report-monitor"])).unwrap();
        assert!(parsed.report_monitor);
        // 只读开关：不伪装成截图 / 退出请求。
        assert!(parsed.capture.is_none());
        assert_eq!(parsed.exit_after_ms, None);
    }

    #[test]
    fn close_dirty_flag_is_parsed_and_requires_a_table() {
        // `answer=` 前缀与裸值都接受：命令行里两种写法都见过。
        let parsed = parse(&args(&[
            "app",
            "--qa-data-table",
            "users",
            "--qa-close-dirty",
            "answer=discard",
        ]))
        .unwrap();
        assert_eq!(parsed.close_dirty, Some(CloseDirtyAnswer::Discard));
        assert_eq!(
            CloseDirtyAnswer::parse("cancel").unwrap(),
            CloseDirtyAnswer::Cancel
        );
        assert_eq!(CloseDirtyAnswer::Discard.response(), "discard");
        assert_eq!(CloseDirtyAnswer::Cancel.response(), "cancel");
        // 没有 `--qa-data-table` 就没有脏行：不能安静地跑完还当成「验证过」。
        assert!(parse(&args(&["app", "--qa-close-dirty", "answer=discard"])).is_err());
        assert!(
            parse(&args(&[
                "app",
                "--qa-data-table",
                "users",
                "--qa-close-dirty",
                "answer=maybe",
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "app",
                "--qa-data-table",
                "users",
                "--qa-close-dirty",
            ]))
            .is_err()
        );
    }

    #[test]
    fn quit_flag_reroutes_the_close_guard_trigger() {
        // `--qa-quit` 只换触发的动作（`app.quit` = Ctrl+Q），答案仍由 close-dirty 提供。
        let parsed = parse(&args(&[
            "app",
            "--qa-data-table",
            "users",
            "--qa-close-dirty",
            "answer=discard",
            "--qa-quit",
        ]))
        .unwrap();
        assert!(parsed.quit_action);
        assert_eq!(parsed.close_dirty, Some(CloseDirtyAnswer::Discard));
        // 没有 close-dirty 就没有触发条件与答案：直接报错，不静默跑完。
        assert!(parse(&args(&["app", "--qa-quit"])).is_err());
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

    #[test]
    fn fail_transaction_flag_is_parsed() {
        assert!(!parse(&args(&["app"])).unwrap().data_fail_txn);
        let parsed = parse(&args(&["app", "--qa-data-fail-txn"])).unwrap();
        assert!(parsed.data_fail_txn);
    }

    #[test]
    fn replay_cancelled_flag_is_parsed() {
        assert!(!parse(&args(&["app"])).unwrap().data_replay_cancelled);
        let parsed = parse(&args(&["app", "--qa-data-replay-cancelled"])).unwrap();
        assert!(parsed.data_replay_cancelled);
    }

    #[test]
    fn expectation_flags_are_parsed_with_labels() {
        let parsed = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-expect-data-state",
            "error",
            "--qa-expect-txn",
            "failed",
            "--qa-expect-status-dot",
            "脏行 3 · 待提交 2 条",
            "--qa-expect-begin",
            "disabled",
        ]))
        .unwrap();
        assert_eq!(parsed.expect_data_state, Some(DataExpectation::Error));
        assert_eq!(parsed.expect_txn, Some(TxnExpectation::Failed));
        // 「开始事务」的可用性：有未提交的更改时必须禁用（§5.1）。
        assert_eq!(parsed.expect_begin, Some(false));
        // 状态圆点：tooltip 是原来两段常驻文字的唯一出口，子串原样保留。
        assert_eq!(
            parsed.expect_status_dot.as_deref(),
            Some("脏行 3 · 待提交 2 条")
        );

        // 断言比对的是界面上的真实文案与 CSS 类，文案必须和状态机一致。
        assert_eq!(TxnExpectation::Failed.label(), "事务失败（只能回滚）");
        assert_eq!(TxnExpectation::Failed.css_class(), "sqarvo-txn-failed");
        assert_eq!(TxnExpectation::Active.label(), "事务中");
        assert_eq!(TxnExpectation::ReadOnly.label(), "事务中（只读）");
        assert_eq!(TxnExpectation::AutoCommit.label(), "自动提交");
        assert_eq!(DataExpectation::Loading.name(), "Loading");
        assert_eq!(DataExpectation::Empty.name(), "Empty");
    }

    #[test]
    fn column_info_flags_are_parsed() {
        let parsed = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-column-info",
            "note",
            "--qa-expect-column",
            "note:default=默认 NULL",
            "--qa-expect-column",
            "title:collation=utf8mb4_general_ci",
        ]))
        .unwrap();
        assert_eq!(parsed.column_info.as_deref(), Some("note"));
        assert_eq!(parsed.expect_columns.len(), 2);
        assert_eq!(
            parsed.expect_columns[0],
            ColumnExpectation {
                column: "note".to_owned(),
                field: ColumnField::DefaultValue,
                expected: "默认 NULL".to_owned(),
            }
        );
        assert_eq!(parsed.expect_columns[1].field, ColumnField::Collation);
        // 文案里可以有 `=` 与 `(`：只按第一个 `=` 切分。
        let parsed = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-expect-column",
            "updated:default=current_timestamp()",
        ]))
        .unwrap();
        assert_eq!(parsed.expect_columns[0].expected, "current_timestamp()");
        // 字段名与断言读取用同一份词表。
        for field in [
            ColumnField::Name,
            ColumnField::Type,
            ColumnField::Nullable,
            ColumnField::DefaultValue,
            ColumnField::Charset,
            ColumnField::Collation,
        ] {
            assert_eq!(ColumnField::parse(field.name()).unwrap(), field);
        }
    }

    #[test]
    fn column_info_flags_reject_bad_values() {
        // 列名不能为空；必须是 列名:字段=文案；字段名必须认识。
        assert!(parse(&args(&["app", "--qa-column-info"])).is_err());
        assert!(parse(&args(&["app", "--qa-column-info", " "])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-column"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-column", "note"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-column", "note:default"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-column", ":default=x"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-column", "note:bogus=x"])).is_err());
        // 与其它 --qa-expect-* 一样：没有截图就没有「断言这一刻」。
        assert!(
            parse(&args(&[
                "app",
                "--qa-expect-column",
                "note:default=默认 NULL"
            ]))
            .is_err()
        );
    }

    #[test]
    fn column_width_flag_is_repeatable_and_validated() {
        // 可重复：一次跑里给多列设宽度（都走 fixed-width 属性）。
        let parsed = parse(&args(&[
            "app",
            "--qa-column-width",
            "name:260",
            "--qa-column-width",
            "amount:96",
        ]))
        .unwrap();
        assert_eq!(
            parsed.column_widths,
            vec![("name".to_owned(), 260), ("amount".to_owned(), 96)]
        );
        // 只按第一个 `:` 切分；列名不能为空、宽度必须是正数。
        assert!(
            parse(&args(&["app", "--qa-column-width", "a:b:120"]))
                .unwrap_err()
                .contains("宽度不是合法像素值")
        );
        assert!(parse(&args(&["app", "--qa-column-width"])).is_err());
        assert!(parse(&args(&["app", "--qa-column-width", "name"])).is_err());
        assert!(parse(&args(&["app", "--qa-column-width", ":120"])).is_err());
        assert!(parse(&args(&["app", "--qa-column-width", "name:0"])).is_err());
        assert!(parse(&args(&["app", "--qa-column-width", "name:-3"])).is_err());
    }

    #[test]
    fn objects_expectations_are_parsed_and_require_a_screenshot() {
        let parsed = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-expect-objects",
            "count=3",
            "--qa-expect-objects",
            "first=accept_items",
            "--qa-expect-objects",
            "selected=accept_items",
            "--qa-expect-objects",
            "empty=off",
        ]))
        .unwrap();
        assert_eq!(parsed.expect_objects.len(), 4);
        assert_eq!(
            parsed.expect_objects[0],
            ObjectsExpectation {
                field: "count".to_owned(),
                expected: "3".to_owned(),
            }
        );
        // 没有截图就没有「断言这一刻」；未知字段与缺 `=` 都要报错。
        assert!(
            parse(&args(&["app", "--qa-expect-objects", "count=1"]))
                .unwrap_err()
                .contains("--screenshot")
        );
        assert!(parse(&args(&["app", "--qa-expect-objects"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-objects", "count"])).is_err());
        // v0.2.3.3 删掉了页内作用域文案，`scope` 字段随之删除：旧脚本必须报错，
        // 不能安静地拿一个没有控件承载的值「通过」。
        let error = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-expect-objects",
            "scope=x",
        ]))
        .unwrap_err();
        assert!(
            error.contains("count|first|selected|status|empty"),
            "{error}"
        );
        assert!(parse(&args(&["app", "--qa-expect-objects", "bogus=x"])).is_err());
    }

    #[test]
    fn tree_activate_flag_is_parsed_and_requires_a_name() {
        let parsed = parse(&args(&["app", "--qa-tree-activate", "orders"])).unwrap();
        assert_eq!(parsed.tree_activate.as_deref(), Some("orders"));
        assert!(parse(&args(&["app", "--qa-tree-activate"])).is_err());
        assert!(parse(&args(&["app", "--qa-tree-activate", " "])).is_err());
    }

    #[test]
    fn tree_flags_are_parsed_in_order() {
        let parsed = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-tree-expand",
            "sqarvo_test_86",
            "--qa-tree-expand",
            "orders",
            "--qa-tree-open",
            "orders",
            "--qa-tree-search",
            "ord",
            "--qa-tree-click-result",
            "--qa-search-click-object",
            "--qa-tree-load-more",
            "--qa-expect-tree",
            "selected=orders",
            "--qa-expect-ddl",
            "state=ready",
        ]))
        .unwrap();
        // 展开顺序就是参数顺序：先库、后表，跳转时的逐层展开依赖它。
        assert_eq!(parsed.tree_expand, vec!["sqarvo_test_86", "orders"]);
        assert_eq!(parsed.tree_open.as_deref(), Some("orders"));
        assert_eq!(parsed.tree_search.as_deref(), Some("ord"));
        assert!(parsed.tree_click_result);
        assert!(parsed.search_click_object);
        assert!(parsed.tree_load_more);
        assert_eq!(
            parsed.expect_tree[0],
            TreeExpectation {
                field: TreeField::Selected,
                expected: "orders".to_owned(),
            }
        );
        assert_eq!(
            parsed.expect_ddl[0],
            DdlExpectation {
                field: DdlField::State,
                expected: "ready".to_owned(),
            }
        );
        // 字段名与断言读取用同一份词表。
        for field in [
            TreeField::Health,
            TreeField::Selected,
            TreeField::Search,
            TreeField::Results,
            TreeField::First,
            TreeField::Page,
            TreeField::Loaded,
            TreeField::StatusContains,
        ] {
            assert_eq!(TreeField::parse(field.name()).unwrap(), field);
        }
        for field in [DdlField::Contains, DdlField::State, DdlField::Visible] {
            assert_eq!(DdlField::parse(field.name()).unwrap(), field);
        }
    }

    #[test]
    fn equals_form_is_expanded_like_glib() {
        // `--opt=value` 与 `--opt value` 必须等价（GLib 两种都接受）。
        let parsed = parse(&args(&[
            "app",
            "--screenshot=/tmp/a.png",
            "--screenshot-delay-ms=2500",
            "--qa-expect-ddl=contains=CREATE TABLE",
        ]))
        .unwrap();
        let capture = parsed.capture.expect("应解析出截图请求");
        assert_eq!(capture.path, PathBuf::from("/tmp/a.png"));
        assert_eq!(capture.delay, Duration::from_millis(2500));
        // 值里可以有 `=`：只按第一个切分。
        assert_eq!(parsed.expect_ddl[0].expected, "CREATE TABLE");
        assert_eq!(parsed.expect_ddl[0].field, DdlField::Contains);
    }

    #[test]
    fn export_scope_argument_is_parsed_and_validated() {
        // 默认：当前页（语义与 #120 完全一致）。
        assert_eq!(ExportScopeArg::default(), ExportScopeArg::Page);
        let parsed = parse(&args(&["app"])).unwrap();
        assert_eq!(parsed.export_scope, ExportScopeArg::Page);
        // 显式整表。
        let parsed = parse(&args(&["app", "--qa-export-scope", "table"])).unwrap();
        assert_eq!(parsed.export_scope, ExportScopeArg::Table);
        // `--opt=value` 形式同样接受。
        let parsed = parse(&args(&["app", "--qa-export-scope=page"])).unwrap();
        assert_eq!(parsed.export_scope, ExportScopeArg::Page);
        // 非法值不静默退回默认：QA 参数写错就会让人以为跑的是另一条路径。
        assert!(parse(&args(&["app", "--qa-export-scope"])).is_err());
        assert!(parse(&args(&["app", "--qa-export-scope", "all"])).is_err());
        // 范围与取消行数可以同时给：整表路径的取消断言用的就是这两个。
        let parsed = parse(&args(&[
            "app",
            "--qa-export",
            "/tmp/whole.csv",
            "--qa-export-scope",
            "table",
            "--qa-export-cancel-after-rows",
            "1200",
        ]))
        .unwrap();
        assert_eq!(parsed.export_scope, ExportScopeArg::Table);
        assert_eq!(parsed.export_cancel_after_rows, Some(1200));
        assert_eq!(parsed.export_path, Some(PathBuf::from("/tmp/whole.csv")));
    }

    #[test]
    fn tree_flags_reject_bad_values() {
        assert!(parse(&args(&["app", "--qa-tree-expand"])).is_err());
        assert!(parse(&args(&["app", "--qa-tree-expand", " "])).is_err());
        assert!(parse(&args(&["app", "--qa-tree-open"])).is_err());
        assert!(parse(&args(&["app", "--qa-tree-open", ""])).is_err());
        // `--qa-tree-ddl` 在 v0.2.3.2 随「查看 DDL」入口一起移除：旧脚本必须报错，
        // 不能安静地跑成「什么都没做」。
        assert!(parse(&args(&["app", "--qa-tree-ddl", "orders"])).is_err());
        assert!(parse(&args(&["app", "--qa-tree-search", "  "])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-tree", "selected"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-tree", "bogus=x"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-ddl", "state"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-ddl", "bogus=x"])).is_err());
        // v0.2.3.3 删掉的字段（来源徽标 / 来源说明 / 复制）必须报错：旧脚本不能安静地
        // 跑成「断言通过」，而那条断言背后已经没有控件了。
        for removed in ["provenance=原文", "detail_contains=SHOW CREATE", "copy=on"] {
            assert!(
                parse(&args(&[
                    "app",
                    "--screenshot",
                    "/tmp/a.png",
                    "--qa-expect-ddl",
                    removed
                ]))
                .is_err(),
                "已移除的字段仍然被接受：{removed}"
            );
        }
        // 与其它 --qa-expect-* 一样：没有截图就没有「断言这一刻」。
        assert!(parse(&args(&["app", "--qa-expect-tree", "selected=orders"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-ddl", "contains=CREATE TABLE"])).is_err());
    }

    #[test]
    fn refresh_flag_is_parsed_as_a_bare_switch() {
        // `--qa-refresh` 走的是 `win.refresh` 动作，因此不取值；与 `--qa-tree-open`
        // 组合时两者互不影响（先开后刷）。
        let parsed = parse(&args(&["app", "--qa-tree-open", "users", "--qa-refresh"])).unwrap();
        assert!(parsed.refresh);
        assert_eq!(parsed.tree_open.as_deref(), Some("users"));
        // 单独给出也合法：没有表标签时动作自己走对象树分支。
        assert!(parse(&args(&["app", "--qa-refresh"])).unwrap().refresh);
        assert!(
            !parse(&args(&["app", "--qa-tree-open", "users"]))
                .unwrap()
                .refresh
        );
    }

    #[test]
    fn tree_requested_only_when_an_action_is_registered() {
        // 只有断言没有动作时不该跑序列（否则会空转 6 秒等一个不会发生的点击）。
        let only_expectations = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-expect-tree",
            "health=Ready",
        ]))
        .unwrap();
        assert!(only_expectations.tree_expand.is_empty());
        assert!(!only_expectations.tree_click_result);
    }

    #[test]
    fn expectation_flags_reject_bad_values() {
        assert!(parse(&args(&["app", "--qa-expect-txn"])).is_err());
        assert!(
            parse(&args(&[
                "app",
                "--screenshot",
                "/tmp/a.png",
                "--qa-expect-txn",
                "bogus"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "app",
                "--screenshot",
                "/tmp/a.png",
                "--qa-expect-data-state",
                "bogus"
            ]))
            .is_err()
        );
        // 没有截图就没有「断言这一刻」：静默忽略会让「跑了但没断言」看起来像通过。
        assert!(parse(&args(&["app", "--qa-expect-txn", "failed"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-data-state", "empty"])).is_err());
        // 状态圆点的断言同样依附截图，而且空文案是脚本写错（不是「什么都不用查」）。
        assert!(parse(&args(&["app", "--qa-expect-status-dot"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-status-dot", "  "])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-status-dot", "自动提交"])).is_err());
    }

    // ---- #88：插入行与隔离级别 ----

    #[test]
    fn insert_and_isolation_flags_are_parsed() {
        let parsed = parse(&args(&[
            "app",
            "--qa-data-insert",
            "--qa-data-insert-set",
            "code:qa-new-1",
            "--qa-data-insert-null",
            "amount",
            "--qa-expect-row",
            "code=qa-new-1",
            "--qa-isolation-dialog",
            "--qa-isolation-level",
            "repeatable-read",
            "--qa-isolation-confirm",
        ]))
        .expect("参数应可解析");
        assert!(parsed.data_insert);
        assert_eq!(
            parsed.data_insert_set,
            vec![("code".to_owned(), "qa-new-1".to_owned())]
        );
        assert_eq!(parsed.data_insert_null, vec!["amount".to_owned()]);
        assert_eq!(
            parsed.expect_rows,
            vec![("code".to_owned(), "qa-new-1".to_owned())]
        );
        assert!(parsed.isolation_dialog);
        assert_eq!(
            parsed.isolation_level,
            Some(IsolationChoice::RepeatableRead)
        );
        assert!(parsed.isolation_confirm);
    }

    #[test]
    fn cancel_submit_flags_are_parsed() {
        let parsed = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-data-table",
            "items89",
            "--qa-data-edit-range",
            "0:500:note:bulk-edit",
            "--qa-data-cancel-after-ms",
            "300",
            "--qa-expect-submit",
            "outcome=cancelled",
            "--qa-expect-submit",
            "status=已取消",
            "--qa-expect-submit",
            "button=取消提交",
            "--qa-expect-submit",
            "phase=submitting",
        ]))
        .unwrap();
        assert_eq!(
            parsed.data_edit_ranges,
            vec![(0, 500, "note".to_owned(), "bulk-edit".to_owned())]
        );
        assert_eq!(parsed.data_cancel_after_ms, Some(300));
        // 值里可以带 `=`（状态行文案常常含等号），只按第一个 `=` 切分。
        assert_eq!(
            parsed.expect_submit,
            vec![
                SubmitExpectation {
                    field: SubmitField::Outcome,
                    expected: "cancelled".to_owned()
                },
                SubmitExpectation {
                    field: SubmitField::Status,
                    expected: "已取消".to_owned()
                },
                SubmitExpectation {
                    field: SubmitField::Button,
                    expected: "取消提交".to_owned()
                },
                SubmitExpectation {
                    field: SubmitField::Phase,
                    expected: "submitting".to_owned()
                },
            ]
        );
    }

    #[test]
    fn cancel_submit_flags_reject_malformed_values() {
        // 参数写错必须报错：静默忽略会让人以为「取消已经验证过了」。
        assert!(parse(&args(&["app", "--qa-data-edit-range"])).is_err());
        assert!(parse(&args(&["app", "--qa-data-edit-range", "0:500:note"])).is_err());
        assert!(parse(&args(&["app", "--qa-data-edit-range", "x:500:note:v"])).is_err());
        assert!(parse(&args(&["app", "--qa-data-cancel-after-ms", "soon"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-submit"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-submit", "phase"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-submit", "mood=cancelled"])).is_err());
        // 没有截图就没有「那一刻」：断言不得静默跳过。
        assert!(parse(&args(&["app", "--qa-expect-submit", "phase=done"])).is_err());
    }

    #[test]
    fn insert_and_isolation_flags_reject_malformed_values() {
        // 缺值 / 缺分隔符 / 未知级别都必须报错，不能静默用默认值：
        // 「参数写错了却跑通了」会让人以为界面验证过。
        assert!(parse(&args(&["app", "--qa-data-insert-set"])).is_err());
        assert!(parse(&args(&["app", "--qa-data-insert-set", "code"])).is_err());
        assert!(parse(&args(&["app", "--qa-data-insert-null"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-row", "code"])).is_err());
        assert!(parse(&args(&["app", "--qa-isolation-level", "read-something"])).is_err());
        // 合法取值映射到 proto 枚举（界面发出去的正是这个值）。
        assert_eq!(
            IsolationChoice::parse("serializable")
                .expect("合法级别")
                .level(),
            crate::proto::services::v1::IsolationLevel::Serializable
        );
        assert_eq!(
            IsolationChoice::parse("default").expect("合法级别").level(),
            crate::proto::services::v1::IsolationLevel::Unspecified
        );
    }

    #[test]
    fn layout_audit_flag_is_a_boolean_and_requires_a_screenshot() {
        assert!(!parse(&args(&["app"])).unwrap().layout_audit);
        let parsed = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-layout-audit",
        ]))
        .unwrap();
        assert!(parsed.layout_audit);
        assert!(parse(&args(&["app", "--qa-layout-audit"])).is_err());
    }

    #[test]
    fn startup_workspace_and_about_flags_require_a_screenshot() {
        // 未给出：默认都不启用（正常启动路径不受影响）。
        let plain = parse(&args(&["app"])).unwrap();
        assert!(!plain.startup_workspace);
        assert!(!plain.open_about);
        assert!(plain.expect_about.is_empty());

        let parsed = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-startup-workspace",
            "--qa-open-about",
            "--qa-expect-about",
            "后端版本",
            "--qa-expect-about=SQLite",
        ]))
        .unwrap();
        assert!(parsed.startup_workspace);
        assert!(parsed.open_about);
        assert_eq!(
            parsed.expect_about,
            vec!["后端版本".to_owned(), "SQLite".to_owned()]
        );

        // 断言发生在截图那一刻：没有截图就不能登记。
        assert!(parse(&args(&["app", "--qa-startup-workspace"])).is_err());
        assert!(parse(&args(&["app", "--qa-open-about"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-about", "x"])).is_err());
        // 空子串是参数错误，不是「必然通过」。
        assert!(
            parse(&args(&[
                "app",
                "--screenshot",
                "/tmp/a.png",
                "--qa-expect-about",
                "  "
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "app",
                "--screenshot",
                "/tmp/a.png",
                "--qa-expect-about"
            ]))
            .is_err()
        );
    }

    #[test]
    fn startup_click_flags_parse_and_require_a_screenshot() {
        let plain = parse(&args(&["app"])).unwrap();
        assert!(plain.startup_click.is_none());
        assert!(plain.expect_startup_connect.is_empty());
        assert!(plain.expect_dialog.is_empty());

        let parsed = parse(&args(&[
            "app",
            "--screenshot",
            "/tmp/a.png",
            "--qa-startup-click",
            "v023-mariadb",
            "--qa-expect-startup-connect",
            "sessions=1",
            "--qa-expect-startup-connect=log=browse_opened",
            "--qa-expect-startup-connect",
            "title_contains=mariadb",
            "--qa-expect-dialog",
            "缺少密码",
            "--qa-startup-menu",
            "v023-mariadb",
            "--qa-startup-menu-pick",
            "编辑连接…",
            "--qa-delete-connection",
            "v023-mariadb",
        ]))
        .unwrap();
        assert_eq!(parsed.startup_click.as_deref(), Some("v023-mariadb"));
        assert_eq!(
            parsed.expect_startup_connect,
            vec![
                ("sessions".to_owned(), "1".to_owned()),
                ("log".to_owned(), "browse_opened".to_owned()),
                ("title_contains".to_owned(), "mariadb".to_owned()),
            ]
        );
        assert_eq!(parsed.expect_dialog, vec!["缺少密码".to_owned()]);
        assert_eq!(parsed.startup_menu.as_deref(), Some("v023-mariadb"));
        assert_eq!(parsed.startup_menu_pick.as_deref(), Some("编辑连接…"));
        assert_eq!(parsed.delete_connection.as_deref(), Some("v023-mariadb"));

        // 断言发生在截图那一刻：没有截图就不能登记。
        assert!(parse(&args(&["app", "--qa-expect-startup-connect", "sessions=1"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-dialog", "缺少密码"])).is_err());
        // 缺值 / 缺分隔符都是参数错误，不能静默当成「通过」。
        assert!(parse(&args(&["app", "--qa-startup-click"])).is_err());
        assert!(parse(&args(&["app", "--qa-startup-click", " "])).is_err());
        assert!(
            parse(&args(&[
                "app",
                "--screenshot",
                "/tmp/a.png",
                "--qa-expect-startup-connect",
                "sessions"
            ]))
            .is_err()
        );
        assert!(parse(&args(&["app", "--qa-delete-connection"])).is_err());
    }

    #[test]
    fn window_size_is_optional_and_parsed() {
        // 未给出：用产品默认 1200×800（外壳不传 override）。
        assert!(parse(&args(&["app"])).unwrap().window_size.is_none());
        assert_eq!(
            parse(&args(&["app", "--window-size", "960x640"]))
                .unwrap()
                .window_size,
            Some((960, 640))
        );
        // GLib 接受 `--opt=value`：自解析必须一致，大小写 x 都认。
        assert_eq!(
            parse(&args(&["app", "--window-size=1200X800"]))
                .unwrap()
                .window_size,
            Some((1200, 800))
        );
    }

    #[test]
    fn window_size_rejects_malformed_values() {
        assert!(parse(&args(&["app", "--window-size", "960"])).is_err());
        assert!(parse(&args(&["app", "--window-size", "widex640"])).is_err());
        assert!(parse(&args(&["app", "--window-size", "0x640"])).is_err());
        assert!(parse(&args(&["app", "--window-size"])).is_err());
    }

    #[test]
    fn window_size_below_the_minimum_is_accepted_for_enforcement_checks() {
        // 960×640 由窗口自己强制（`layout::MIN_WINDOW_*`）：更小的请求照收，
        // 窗口会被抬到下限——这个取值就是「强制生效」的取证入口。
        assert_eq!(
            parse(&args(&["app", "--window-size", "800x500"]))
                .unwrap()
                .window_size,
            Some((800, 500))
        );
        assert!(
            parse(&args(&["app", "--window-size", "800x500"]))
                .unwrap()
                .window_size
                .unwrap()
                < (
                    crate::layout::MIN_WINDOW_WIDTH,
                    crate::layout::MIN_WINDOW_HEIGHT
                )
        );
    }

    // ---- #118 浏览脚本（--qa-browse） ----

    #[test]
    fn browse_steps_parse_every_supported_form() {
        assert_eq!(
            BrowseStep::parse("open=demo_items").expect("open"),
            BrowseStep::Open("demo_items".to_owned())
        );
        assert_eq!(
            BrowseStep::parse("filter=name:like:a%").expect("filter"),
            BrowseStep::Filter("name".to_owned(), "like".to_owned(), "a%".to_owned())
        );
        assert_eq!(
            BrowseStep::parse("next").expect("next"),
            BrowseStep::Page("next".to_owned())
        );
        assert_eq!(
            BrowseStep::parse("select=3").expect("select"),
            BrowseStep::Select(3)
        );
        assert_eq!(
            BrowseStep::parse("hide=note").expect("hide"),
            BrowseStep::Visible("note".to_owned(), false)
        );
        assert_eq!(BrowseStep::parse("log").expect("log"), BrowseStep::Log);
    }

    #[test]
    fn browse_steps_reject_malformed_input() {
        assert!(BrowseStep::parse("open=").is_err());
        assert!(BrowseStep::parse("filter=name").is_err());
        assert!(BrowseStep::parse("select=abc").is_err());
        assert!(BrowseStep::parse("next=1").is_err());
        assert!(BrowseStep::parse("nonsense").is_err());
    }

    #[test]
    fn browse_flag_is_repeatable_and_enables_qa_mode() {
        let parsed = parse(&args(&[
            "app",
            "--qa-browse",
            "open=demo_items",
            "--qa-browse",
            "filter=name:like:a%",
            "--qa-browse",
            "next",
        ]))
        .expect("参数应可解析");
        assert_eq!(
            parsed.browse_steps,
            vec![
                BrowseStep::Open("demo_items".to_owned()),
                BrowseStep::Filter("name".to_owned(), "like".to_owned(), "a%".to_owned()),
                BrowseStep::Page("next".to_owned()),
            ]
        );
    }
    /// `--qa-search` 与 `--qa-expect-search` 的解析与取值校验（不接受空关键词 / 空路径）。
    #[test]
    fn search_tree_flags_parse_and_validate() {
        let parsed = parse(&args(&[
            "app",
            "--qa-search",
            "user",
            "--qa-expect-search",
            "hits=3",
            "--qa-expect-search",
            "row=*/main/Tables/users",
            "--qa-expect-search",
            "highlight=*/main/Tables/users,*/main/Views/v_users",
        ]))
        .unwrap();
        assert_eq!(parsed.search_query.as_deref(), Some("user"));
        assert_eq!(parsed.expect_search.len(), 3);
        assert_eq!(parsed.expect_search[0].field, SearchField::Hits);
        assert_eq!(parsed.expect_search[0].expected, "3");

        assert!(parse(&args(&["app", "--qa-search", "  "])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-search", "hits=x"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-search", "hits"])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-search", "row="])).is_err());
        assert!(parse(&args(&["app", "--qa-expect-search", "bogus=1"])).is_err());

        // v0.2.4：`--qa-search-click-object` 要跟着一次搜索一起用（否则只会重试到超时）。
        let clicked = parse(&args(&[
            "app",
            "--qa-search",
            "user",
            "--qa-search-click-object",
        ]))
        .unwrap();
        assert!(clicked.search_click_object);
        assert!(parse(&args(&["app", "--qa-search-click-object"])).is_err());

        // 路径断言：`*` 只匹配一段，不能把 `users` 当成 `user`。
        assert!(search_path_matches(
            "*/main/Tables/users",
            "ui-probe/main/Tables/users"
        ));
        assert!(!search_path_matches(
            "*/main/Tables/users",
            "ui-probe/main/Tables/user"
        ));
        assert!(!search_path_matches(
            "*/main/Tables/users",
            "ui-probe/main/Tables/users/extra"
        ));
        assert_eq!(search_paths("a/b, c/d"), ["a/b", "c/d"]);
    }
}
