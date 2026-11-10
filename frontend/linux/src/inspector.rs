//! 信息面板（#119）：右侧按**当前选中对象种类**切换内容的检查器。
//!
//! 结构来自 `docs/ui-specification/workspace-model.md` §5：面板宽度 280（可拖
//! 240–520、可折叠），内容由选中种类决定：
//!
//! | 选中 | 内容 | 数据来源 |
//! |---|---|---|
//! | 连接 | 驱动、地址、默认库、用户、TLS、凭据引用、服务端版本、连接延迟、会话数 | `connections.json` + `ConnectOutcome` + `ListSessions` |
//! | 库 / catalog | 名称 + 已展开结果的表 / 视图计数 | 树的已加载层（不主动全量扫描） |
//! | 表 / 视图 | `[DDL]` / `[表信息]` 两页 | `GetDdl` / `GetColumns` / `GetPrimaryKey` / `ListNodes` |
//! | 列 | 该列的完整属性 | `GetColumns`（父表的列清单） |
//! | 索引 / 触发器 | 仅 DDL；不支持的驱动给稳定错误码 | `GetDdl` |
//!
//! 两条硬规则：
//!
//! 1. **拿不到的字段不出现**——不写 `—` / `N/A` / 空占位。字段为空串就是「没有这个
//!    事实」，这一行整行不渲染（见本模块的单测）。
//! 2. **DDL 与表信息是面板内切换**：不重建标签、不重发浏览请求；选择按标签生命周期
//!    记忆（[`Inspector::show_object`] 的 `tab_id` 键）。
//!
//! 本模块的文案层（[`Fact`]、[`connection_facts`]、[`catalog_facts`]、
//! [`primary_key_label`]、[`state_for_error`]）不创建 GTK 对象，可脱离显示服务器单测；
//! GTK 部分只负责把这些文字贴到控件上。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;

use crate::backend::metadata::{MetadataError, MetadataHub, NodeView, ddl_supported, kind_label};
use crate::backend::table::TableColumn;
use crate::column_info::{column_info_rows, default_label, nullable_label};
use crate::config::SavedConnection;
use crate::ddl::DdlPanel;
use crate::layout::{Gap, gap};
use crate::proto::services::v1::ObjectKind;

/// DDL 面板的通道解析器（按对象自己的浏览会话找元数据通道）。
pub type HubResolver =
    Rc<dyn Fn(&crate::proto::services::v1::ObjectRef) -> Option<Rc<MetadataHub>>>;

/// 面板内的两页：「DDL」与「表信息」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TablePage {
    /// `GetDdl` 原文 + 来源标注。
    Ddl,
    /// 列 / 主键 / 索引与触发器 / 已加载行数。
    Info,
}

impl TablePage {
    /// 日志与断言用的名字。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Ddl => "ddl",
            Self::Info => "info",
        }
    }

    /// 解析命令行 / 状态文件里的取值（未知值按默认页处理）。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ddl" => Some(Self::Ddl),
            "info" | "table" | "表信息" => Some(Self::Info),
            _ => None,
        }
    }
}

/// 面板上的一行：字段名 + 值。
///
/// 值为空串时**整行不渲染**（`workspace-model` §5：拿不到的字段不出现）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// 字段名。
    pub label: &'static str,
    /// 值。
    pub value: String,
}

impl Fact {
    #[must_use]
    fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
        }
    }

    /// 保留「值非空」的行：空值不是事实，不该以占位形式出现。
    #[must_use]
    fn keep(facts: Vec<Self>) -> Vec<Self> {
        facts
            .into_iter()
            .filter(|fact| !fact.value.trim().is_empty())
            .collect()
    }
}

/// 连接节点上能从「运行时」拿到的部分（其余来自保存的配置）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionLive {
    /// 服务端版本（`TestConnection`）；空 = 拿不到，不显示。
    pub server_version: String,
    /// 连接延迟（`TestConnection` 的往返毫秒）。
    pub latency_millis: Option<u64>,
    /// 后端会话数（最近一次 `ListSessions`；没有读过就是 `None`）。
    pub sessions: Option<usize>,
}

/// 连接的端点文案：文件型是路径，服务器型是 `host:port`。
///
/// 不拼用户名与库名：§5 把它们拆成独立字段，重复写会让「地址」看起来是连接串。
#[must_use]
pub fn endpoint_text(saved: &SavedConnection) -> Option<String> {
    if let Some(path) = saved.file_target() {
        return Some(path.to_owned());
    }
    if saved.host.trim().is_empty() {
        return None;
    }
    Some(format!("{}:{}", saved.host, saved.port))
}

/// 连接节点的字段（顺序与 §5 表格一致）。
///
/// **不含密码**：`SavedConnection` 里本来就没有密码，凭据引用是另一回事。
#[must_use]
pub fn connection_facts(saved: &SavedConnection, live: &ConnectionLive) -> Vec<Fact> {
    let mut facts = vec![Fact::new("驱动", saved.driver.clone())];
    if let Some(endpoint) = endpoint_text(saved) {
        facts.push(Fact::new("地址", endpoint));
    }
    // 服务器型驱动的库名可选（ADR 0024）：没有默认库时说「未指定」，
    // 而不是把这一行整行吞掉（用户会以为界面漏了字段）。
    // 文件型驱动的 database 本来就没有意义，保持「空值不渲染」。
    if saved.database.trim().is_empty() && saved.file_target().is_none() {
        facts.push(Fact::new("默认库", "未指定"));
    } else {
        facts.push(Fact::new("默认库", saved.database.clone()));
    }
    facts.push(Fact::new("用户", saved.username.clone()));
    // 文件型驱动没有 TLS 这一层：不适用就该整行不出现，而不是写「disabled」。
    if saved.file_target().is_none() {
        facts.push(Fact::new("TLS", saved.tls_mode.clone()));
    }
    facts.push(Fact::new("凭据引用", saved.credential_ref.clone()));
    facts.push(Fact::new("服务端版本", live.server_version.clone()));
    if let Some(millis) = live.latency_millis {
        facts.push(Fact::new("连接延迟", format!("{millis} ms")));
    }
    if let Some(count) = live.sessions {
        facts.push(Fact::new("会话数", format!("{count} 条")));
    }
    Fact::keep(facts)
}

/// 库 / catalog 节点的字段。
///
/// 计数来自**已经展开过的层**（树的 `Tables` / `Views` 分组各自记了已加载条数）：
/// 打开一个库不该顺带把整库对象扫一遍（§4 的懒加载契约）。某个分组还没展开过时
/// 传 `None`——那一行不出现，而不是写 0。
#[must_use]
pub fn catalog_facts(
    name: &str,
    tables: Option<usize>,
    views: Option<usize>,
    complete: bool,
) -> Vec<Fact> {
    let mut facts = vec![Fact::new("名称", name)];
    if let Some(tables) = tables {
        facts.push(Fact::new("表", format!("{tables} 个")));
    }
    if let Some(views) = views {
        facts.push(Fact::new("视图", format!("{views} 个")));
    }
    if (tables.is_some() || views.is_some()) && !complete {
        // 已加载 ≠ 全部：分页还有下一页时必须说清楚，不能把当前页当总数。
        facts.push(Fact::new(
            "计数说明",
            "按已展开的结果统计（该层还有下一页）",
        ));
    }
    Fact::keep(facts)
}

/// 主键的展示文案（按主键内顺序；空列表 = 确实没有主键）。
#[must_use]
pub fn primary_key_label(columns: &[(u32, String)]) -> String {
    if columns.is_empty() {
        return "无主键".to_owned();
    }
    let mut sorted: Vec<&(u32, String)> = columns.iter().collect();
    sorted.sort_by_key(|(position, _)| *position);
    sorted
        .iter()
        .map(|(position, name)| format!("{name}（{position}）"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 列表类事实（索引 / 触发器）：空列表是「确实没有」，不是「不知道」。
#[must_use]
pub fn list_fact(label: &'static str, names: &[String], empty: &str) -> Fact {
    if names.is_empty() {
        Fact::new(label, empty.to_owned())
    } else {
        Fact::new(label, names.join(" · "))
    }
}

/// 已加载行数：只有该表在当前窗口的标签里打开过才有这个事实。
#[must_use]
pub fn loaded_rows_fact(rows: Option<usize>) -> Option<Fact> {
    rows.map(|rows| Fact::new("已加载行数", format!("{rows} 行")))
}

/// 面板「状态页」的文案：权限不足 / 对象不存在 / 不支持种类各有独立结论，
/// 并且都带稳定错误码（§5 规则）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateText {
    /// 结论。
    pub headline: &'static str,
    /// 说明（含错误码）。
    pub detail: String,
    /// 稳定错误码。
    pub code: String,
}

impl StateText {
    /// 由元数据错误生成（保留后端文案与错误码，不合并分类）。
    #[must_use]
    pub fn from_error(error: &MetadataError) -> Self {
        Self {
            headline: error.headline(),
            detail: format!("{}（{}）。{}", error.message(), error.code(), error.hint()),
            code: error.code().to_owned(),
        }
    }

    /// 该对象种类没有独立 DDL（例如 MariaDB 的索引 / 触发器）。
    ///
    /// 这是**客户端能确定的结论**：契约里这类对象只有「表 / 视图 / 序列 / 例程 / 类型」
    /// 有 DDL，索引与触发器的定义在所属表的 DDL 里。错误码用契约的
    /// `metadata.object_kind_unsupported`，与后端返回该码时的文案一致。
    #[must_use]
    pub fn unsupported_kind(kind: ObjectKind) -> Self {
        Self {
            headline: "该对象种类不支持查看 DDL",
            detail: format!(
                "{}没有独立的 DDL：定义在所属表的 DDL 里（metadata.object_kind_unsupported）。\
                 换一个表 / 视图再试。",
                kind_label(kind)
            ),
            code: "metadata.object_kind_unsupported".to_owned(),
        }
    }
}

/// QA 读取的列页六项文案（与 `column_info_rows` 同序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnFacts {
    /// 列名。
    pub name: String,
    /// 原始类型。
    pub type_label: String,
    /// 可空文案。
    pub nullable: String,
    /// 默认值文案。
    pub default_value: String,
    /// 字符集文案。
    pub charset: String,
    /// 排序规则文案。
    pub collation: String,
}

/// QA 读取的面板快照（全部来自真实控件文本）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InspectorObservation {
    /// 面板是否可见。
    pub visible: bool,
    /// 标题。
    pub title: String,
    /// 副标题（种类 / 路径）。
    pub subtitle: String,
    /// 当前页（`connection` / `catalog` / `table` / `column` / `ddl-only` / `state` / `empty`）。
    pub page: String,
    /// 表页内的子页（`ddl` / `info`；不是表页时为空）。
    pub table_page: String,
    /// 面板上的字段行（`字段=值`，按渲染顺序）。
    pub facts: Vec<String>,
    /// 跨标签事务提示（没有提示为空串）。
    pub txn_hint: String,
    /// 表信息页里的列数。
    pub columns: usize,
    /// DDL 页的状态名。
    pub ddl_state: String,
    /// DDL 正文（面板里那一段）。
    pub ddl_text: String,
    /// 状态页的说明（含错误码；不是状态页时为空）。
    pub state_detail: String,
}

/// 信息面板。
pub struct Inspector {
    root: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    txn_hint: gtk::Label,
    switch_bar: gtk::Box,
    ddl_toggle: gtk::ToggleButton,
    info_toggle: gtk::ToggleButton,
    stack: gtk::Stack,
    // 连接 / 库 / 列：三个「字段行」容器。
    connection_rows: gtk::Box,
    catalog_rows: gtk::Box,
    column_rows: gtk::Box,
    // 表 / 视图：DDL 与表信息两页。
    table_stack: gtk::Stack,
    ddl: Rc<DdlPanel>,
    info_box: gtk::Box,
    info_status: gtk::Label,
    // 索引 / 触发器：仅 DDL。
    ddl_only: Rc<DdlPanel>,
    state_page: adw::StatusPage,
    hub_of: RefCell<Option<HubResolver>>,
    /// 当前对象（响应回来时比对，防止把 A 的列填进 B）。
    generation: Cell<u64>,
    /// 表 / 视图页内选择的记忆（键：标签 ID，或 `tree` 表示还没有标签）。
    pane_memory: RefCell<HashMap<String, TablePage>>,
    /// 当前页名（QA 断言用）。
    page: RefCell<String>,
    /// 当前表页的子页。
    current_pane: Cell<TablePage>,
    /// 表信息页当前展示的列（QA 统计用）。
    column_count: Cell<usize>,
    /// 最近一次表信息的数据（重建时用）。
    info: RefCell<TableInfo>,
    /// 列页当前展示的列（QA 从控件与这份数据读回六项文案）。
    column: RefCell<Option<TableColumn>>,
    /// 正在看的对象。
    object: RefCell<Option<NodeView>>,
    /// 当前标签 ID（记忆键）。
    tab_key: RefCell<String>,
}

/// 表信息页的数据（每一项 `None` = 还没拿到，不渲染）。
#[derive(Debug, Clone, Default)]
struct TableInfo {
    columns: Option<Vec<TableColumn>>,
    primary_key: Option<Vec<(u32, String)>>,
    indexes: Option<Vec<String>>,
    triggers: Option<Vec<String>>,
    loaded_rows: Option<usize>,
    /// 索引 / 触发器读取失败的说明（保留错误码）。
    children_error: Option<String>,
}

/// 面板内容列的左边距。
///
/// 分隔线是 root 的直接子控件，从左端铺满内容区；标题块 / 切换条 / 表信息 / DDL 正文
/// 原先各自留 10px，读起来像每一块都被单独缩进，而且都比横线左顶点右移一格。统一贴到
/// 内容区左端（0）后，整块面板只有一条左基准线。
///
/// 切换按钮自身还有 4px 横向内边距（CSS），所以选中块的**左边缘**贴基准线、标签文字
/// 在块内缩进 4px——这是块状选中态无法避免的一点内缩。
const PANEL_CONTENT_LEFT: i32 = 0;

impl Inspector {
    /// 构建面板。
    ///
    /// v0.2.3.2 起面板**常驻**：没有折叠按钮，也没有 `Ctrl+I`（`shell.set_inspector_visible`
    /// 不再是用户可达的动作）。宽度仍可拖、窗口级记忆。
    pub fn new() -> Rc<Self> {
        let title = gtk::Label::new(Some("信息面板"));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.add_css_class("heading");

        let subtitle = gtk::Label::new(None);
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subtitle.add_css_class("dim-label");
        subtitle.add_css_class("caption");

        let header_text = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        header_text.append(&title);
        header_text.append(&subtitle);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Xs));
        // 标题块与下方横线之间留白：4px 时副标题（`表`）几乎贴住分隔线，面板头部看起来
        // 被线切了一刀。10px 让「标题 + 副标题」自成一组（实测副标题笔画底到线约 13px）。
        header.set_margin_top(gap(Gap::Sm));
        header.set_margin_bottom(gap(Gap::Lg));
        header.set_margin_start(PANEL_CONTENT_LEFT);
        header.set_margin_end(gap(Gap::Lg));
        header.append(&header_text);

        // 跨标签事务提示：同一 catalog 里别的标签还有未提交事务时出现（#116 待定项）。
        let txn_hint = gtk::Label::new(None);
        txn_hint.set_xalign(0.0);
        txn_hint.set_wrap(true);
        txn_hint.set_margin_start(PANEL_CONTENT_LEFT);
        txn_hint.set_margin_end(gap(Gap::Lg));
        txn_hint.set_margin_bottom(gap(Gap::Md));
        txn_hint.add_css_class("caption");
        txn_hint.add_css_class("sqarvo-inspector-warning");
        txn_hint.set_visible(false);

        // `[DDL] / [表信息]`：面板内切换，只在表 / 视图页出现。
        //
        // 两个 toggle 是**独立**按钮（不用 `.linked` 分段容器）：分段样式只圆外侧两个
        // 角、选中块比文字大一圈；这里要的是贴着文字的小圆角块，四个角都圆。
        //
        // 水平起点按「文字列」对齐：按钮自身有 8px 横向内边距，所以容器左侧只留
        // 10 - 8 = 2px —— 选中块的文字与面板标题、DDL 正文落在同一条竖线上，块本身
        // 向左边距里探出 8px。留 10px 会让「DDL」比上方标题右缩一格，看着不对齐。
        let ddl_toggle = gtk::ToggleButton::with_label("DDL");
        ddl_toggle.add_css_class("flat");
        ddl_toggle.set_tooltip_text(Some("显示该对象的 DDL"));
        // 拉丁字母与 CJK 在同一行里光学中心差约 1.5px（「DDL」没有下缘、字身短，
        // 「表信息」字形更高，两者的行盒居中后看起来「DDL」偏高）。给拉丁标签加 2px
        // 上边距 → 标签在按钮内下移 1px，两个标签的视觉中心对齐。
        if let Some(label) = ddl_toggle.child().and_downcast::<gtk::Label>() {
            label.set_margin_top(gap(Gap::Xxs));
        }
        let info_toggle = gtk::ToggleButton::with_label("表信息");
        info_toggle.add_css_class("flat");
        info_toggle.set_tooltip_text(Some("列、主键、索引与触发器、已加载行数"));
        info_toggle.set_group(Some(&ddl_toggle));
        ddl_toggle.set_active(true);
        let switch_bar = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Xxs));
        switch_bar.add_css_class("sqarvo-pane-switch");
        switch_bar.set_margin_top(gap(Gap::Sm));
        switch_bar.set_margin_start(PANEL_CONTENT_LEFT);
        switch_bar.set_margin_end(gap(Gap::Lg));
        switch_bar.set_margin_bottom(gap(Gap::Sm));
        switch_bar.append(&ddl_toggle);
        switch_bar.append(&info_toggle);
        switch_bar.set_visible(false);

        // ---- 连接 / 库 / 列：字段行容器 -------------------------------------
        let connection_rows = fact_container();
        let catalog_rows = fact_container();
        let column_rows = fact_container();

        // ---- 表 / 视图：DDL（迁入 ddl.rs 的面板）+ 表信息 --------------------
        let ddl = DdlPanel::new();
        let info_status = gtk::Label::new(None);
        info_status.set_xalign(0.0);
        info_status.set_wrap(true);
        info_status.add_css_class("dim-label");
        info_status.add_css_class("sqarvo-panel-text");
        info_status.set_visible(false);
        let info_box = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::Lg));
        info_box.set_hexpand(true);
        info_box.set_margin_top(gap(Gap::Sm));
        info_box.set_margin_bottom(gap(Gap::Lg));
        info_box.set_margin_start(PANEL_CONTENT_LEFT);
        info_box.set_margin_end(gap(Gap::Lg));
        info_box.append(&info_status);
        // 横向 Never 时子控件只拿到自己的最小宽度（折行标签的最小宽度可以很小），
        // 280 宽的面板会变成一条窄条：改用 Automatic——内容装得下就没有横向滚动条，
        // 装不下时滚动而不是把字段挤成两三个字一行。
        let info_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&info_box)
            .build();

        let table_stack = gtk::Stack::new();
        table_stack.set_vexpand(true);
        table_stack.add_named(ddl.widget(), Some("ddl"));
        table_stack.add_named(&info_scroll, Some("info"));
        table_stack.set_visible_child_name("ddl");

        let connection_page = scrolled(&connection_rows);
        let catalog_page = scrolled(&catalog_rows);
        let column_page = scrolled(&column_rows);
        let ddl_only = DdlPanel::new();
        let ddl_only_box = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        ddl_only_box.append(ddl_only.widget());
        ddl_only_box.set_vexpand(true);

        let state_page = adw::StatusPage::new();
        state_page.set_vexpand(true);
        state_page.set_title(" ");

        // 空态页：留白，不写任何说明（面板头部已经显示「信息面板」标题）。
        let empty_page = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        empty_page.set_vexpand(true);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&connection_page, Some("connection"));
        stack.add_named(&catalog_page, Some("catalog"));
        stack.add_named(&table_stack, Some("table"));
        stack.add_named(&column_page, Some("column"));
        stack.add_named(&ddl_only_box, Some("ddl-only"));
        stack.add_named(&state_page, Some("state"));
        stack.add_named(&empty_page, Some("empty"));
        stack.set_visible_child_name("empty");

        let root = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        root.add_css_class("sqarvo-inspector");
        root.append(&header);
        root.append(&txn_hint);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&switch_bar);
        root.append(&stack);

        let panel = Rc::new(Self {
            root,
            title,
            subtitle,
            txn_hint,
            switch_bar,
            ddl_toggle,
            info_toggle,
            stack,
            connection_rows,
            catalog_rows,
            column_rows,
            table_stack,
            ddl,
            info_box,
            info_status,
            ddl_only,
            state_page,
            hub_of: RefCell::new(None),
            generation: Cell::new(0),
            pane_memory: RefCell::new(HashMap::new()),
            page: RefCell::new("empty".to_owned()),
            current_pane: Cell::new(TablePage::Ddl),
            column_count: Cell::new(0),
            info: RefCell::new(TableInfo::default()),
            column: RefCell::new(None),
            object: RefCell::new(None),
            tab_key: RefCell::new("tree".to_owned()),
        });
        panel.install_toggles();
        panel
    }

    fn install_toggles(self: &Rc<Self>) {
        for (button, pane) in [
            (&self.ddl_toggle, TablePage::Ddl),
            (&self.info_toggle, TablePage::Info),
        ] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.set_pane(pane);
                }
            });
        }
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 接线到「按会话找元数据通道」的解析器（DDL / 列 / 主键都要走对象自己的会话）。
    pub fn attach_resolver(&self, resolver: HubResolver) {
        self.hub_of.replace(Some(resolver.clone()));
        self.ddl.attach_resolver(Rc::clone(&resolver));
        self.ddl_only.attach_resolver(resolver);
    }

    /// 设为「没有选中对象」：保留面板头部（标题「信息面板」+ 分隔线），内容区留白。
    pub fn show_empty(&self) {
        self.generation.set(self.generation.get() + 1);
        self.object.replace(None);
        self.title.set_text("信息面板");
        self.subtitle.set_text("");
        self.switch_bar.set_visible(false);
        self.set_page("empty");
    }

    /// 选中一条连接。
    pub fn show_connection(&self, saved: Option<&SavedConnection>, live: &ConnectionLive) {
        self.generation.set(self.generation.get() + 1);
        self.object.replace(None);
        self.switch_bar.set_visible(false);
        let Some(saved) = saved else {
            self.title.set_text("连接");
            self.subtitle.set_text("");
            self.set_state(&StateText {
                headline: "连接已不在配置里",
                detail: "connections.json 里已经没有这条连接（config.invalid）。刷新对象树后重试。"
                    .to_owned(),
                code: "config.invalid".to_owned(),
            });
            return;
        };
        self.title.set_text(&saved.name);
        self.subtitle.set_text(&saved.driver);
        let facts = connection_facts(saved, live);
        fill_facts(&self.connection_rows, &facts);
        self.set_page("connection");
    }

    /// 选中一个库 / catalog。计数来自**已展开**的分组层（`None` = 该分组没展开过）。
    pub fn show_catalog(
        &self,
        name: &str,
        tables: Option<usize>,
        views: Option<usize>,
        complete: bool,
    ) {
        self.generation.set(self.generation.get() + 1);
        self.object.replace(None);
        self.switch_bar.set_visible(false);
        self.title.set_text(name);
        self.subtitle.set_text("库");
        let facts = catalog_facts(name, tables, views, complete);
        fill_facts(&self.catalog_rows, &facts);
        self.set_page("catalog");
    }

    /// 选中一个服务端对象：按种类分派到表 / 列 / 仅 DDL / 状态页。
    ///
    /// `tab_id` 是表页选择的记忆键（标签生命周期内有效）；`loaded_rows` 只有该对象
    /// 已在某个表标签里打开时才有值。
    pub fn show_object(
        self: &Rc<Self>,
        node: NodeView,
        parent: Option<&NodeView>,
        loaded_rows: Option<usize>,
        tab_id: &str,
    ) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.tab_key.replace(tab_id.to_owned());
        self.title.set_text(&node.display_name);
        self.subtitle.set_text(kind_label(node.kind));
        self.title.set_tooltip_text(Some(&node.path_label()));
        self.object.replace(Some(node.clone()));

        match node.kind {
            ObjectKind::Catalog | ObjectKind::Schema => {
                // 计数由调用方按「已展开结果」给出；这里不主动扫库。
                self.switch_bar.set_visible(false);
                self.title.set_text(&node.display_name);
                self.subtitle.set_text(kind_label(node.kind));
                fill_facts(
                    &self.catalog_rows,
                    &catalog_facts(&node.display_name, None, None, false),
                );
                self.set_page("catalog");
            }
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
                self.ddl.set_visible(true);
                self.switch_bar.set_visible(true);
                let pane = self
                    .pane_memory
                    .borrow()
                    .get(tab_id)
                    .copied()
                    .unwrap_or(TablePage::Ddl);
                self.apply_pane(pane, false);
                self.set_page("table");
                // 先推一份只有对象身份的快照；表信息（列/主键/索引）回来时
                // `render_table_info` 会用同一份数据补上关键属性。
                self.load_table(node, generation, loaded_rows);
            }
            ObjectKind::Column => {
                self.switch_bar.set_visible(false);
                self.set_page("column");
                fill_facts(
                    &self.column_rows,
                    &[Fact::new("列名", node.display_name.clone())],
                );
                self.column_count.set(0);
                match parent {
                    Some(table) if matches!(table.kind, ObjectKind::Table | ObjectKind::View) => {
                        self.load_columns_for_column(
                            table.clone(),
                            node.display_name.clone(),
                            generation,
                        );
                    }
                    _ => self.set_state(&StateText {
                        headline: "列的详细属性需要知道所属的表",
                        detail: "从对象树里表的子节点进入列，才能读到 GetColumns 的完整属性。\
                                 （metadata.object_not_found）"
                            .to_owned(),
                        code: "metadata.object_not_found".to_owned(),
                    }),
                }
            }
            kind if ddl_supported(kind) => {
                // 序列 / 函数 / 过程 / 类型：只有 DDL。
                self.ddl_only.set_visible(true);
                self.switch_bar.set_visible(false);
                self.set_page("ddl-only");
                self.ddl_only.show_for(node);
            }
            kind => {
                // 索引 / 触发器（MariaDB 无独立 DDL）：明确「不支持」，不是空面板。
                self.switch_bar.set_visible(false);
                let text = StateText::unsupported_kind(kind);
                self.set_state(&text);
            }
        }
    }

    /// 直接展示某一列（数据网格类型行点击 → 与树上的列节点同一个页面）。
    ///
    /// 用的就是 `GetColumns` 已经拿到的列模型：不重发 RPC，也不复制一份文案口径。
    pub fn show_column(&self, column: &TableColumn) {
        self.generation.set(self.generation.get() + 1);
        self.switch_bar.set_visible(false);
        self.title.set_text(&column.name);
        self.subtitle
            .set_text(&format!("列 · {}", column.type_label));
        let facts: Vec<Fact> = column_info_rows(column)
            .into_iter()
            .map(|(label, value)| Fact::new(label, value))
            .collect();
        fill_facts(&self.column_rows, &facts);
        self.column.replace(Some(column.clone()));
        self.column_count.set(1);
        self.set_page("column");
        eprintln!(
            "[inspector] 列页：{} · {}（来自表头点击）",
            column.name, column.type_label
        );
    }

    /// QA：列页的六项文案（`None` = 当前不在列页）。
    pub fn qa_observe_column(&self) -> Option<ColumnFacts> {
        if self.page.borrow().as_str() != "column" {
            return None;
        }
        let column = self.column.borrow().clone()?;
        let rows = column_info_rows(&column);
        Some(ColumnFacts {
            name: rows[0].1.clone(),
            type_label: rows[1].1.clone(),
            nullable: rows[2].1.clone(),
            default_value: rows[3].1.clone(),
            charset: rows[4].1.clone(),
            collation: rows[5].1.clone(),
        })
    }

    /// 嵌在本面板里的 DDL 面板（QA 的 `--qa-expect-ddl` 读它的控件）。
    pub fn ddl_panel(&self) -> Rc<DdlPanel> {
        Rc::clone(&self.ddl)
    }

    /// 当前标签换了（切标签）：记忆键跟着换，并按新标签记住的页恢复。
    ///
    /// 换标签**不重发**任何请求：面板内容还是同一个对象，只是「这个标签上次看的是
    /// 哪一页」变了（§5：表页选择在标签生命周期内记忆）。
    pub fn retarget(&self, tab_id: &str) {
        if self.tab_key.borrow().as_str() == tab_id {
            return;
        }
        self.tab_key.replace(tab_id.to_owned());
        if self.page.borrow().as_str() == "table" {
            let pane = self
                .pane_memory
                .borrow()
                .get(tab_id)
                .copied()
                .unwrap_or(self.current_pane.get());
            self.apply_pane(pane, false);
        }
    }

    /// 切换表页内的 `[DDL] / [表信息]`，并写入当前标签的记忆。
    pub fn set_pane(&self, pane: TablePage) {
        self.apply_pane(pane, true);
    }

    fn apply_pane(&self, pane: TablePage, remember: bool) {
        self.current_pane.set(pane);
        let name = match pane {
            TablePage::Ddl => "ddl",
            TablePage::Info => "info",
        };
        self.table_stack.set_visible_child_name(name);
        self.ddl_toggle.set_active(pane == TablePage::Ddl);
        self.info_toggle.set_active(pane == TablePage::Info);
        if remember {
            self.pane_memory
                .borrow_mut()
                .insert(self.tab_key.borrow().clone(), pane);
        }
        eprintln!(
            "[inspector] 表页切换到 {}{}",
            pane.name(),
            if remember {
                "（已按标签记忆）"
            } else {
                ""
            }
        );
    }

    /// 跨标签「同一 catalog 还有其它未提交事务」提示（#116 待定项）。
    pub fn set_txn_hint(&self, text: Option<String>) {
        match text {
            Some(text) if !text.trim().is_empty() => {
                self.txn_hint.set_text(&text);
                self.txn_hint.set_visible(true);
            }
            _ => {
                self.txn_hint.set_text("");
                self.txn_hint.set_visible(false);
            }
        }
    }

    fn set_page(&self, name: &str) {
        self.page.replace(name.to_owned());
        self.stack.set_visible_child_name(name);
        // 面板换了对象：上一次的状态页文案不该留在别的页上。
        if name != "state" {
            self.state_page.set_title(" ");
            self.state_page.set_description(None);
        }
    }

    fn set_state(&self, text: &StateText) {
        self.ddl.set_visible(false);
        self.ddl_only.set_visible(false);
        self.state_page.set_title(text.headline);
        self.state_page.set_description(Some(&text.detail));
        self.state_page.set_vexpand(true);
        self.state_page.set_size_request(-1, -1);
        self.page.replace("state".to_owned());
        self.stack.set_visible_child_name("state");
        // 状态页也要推快照：Objects 页不能停留在上一个对象上（否则两处说法不一致）。
        eprintln!("[inspector] 状态页：{} [{}]", text.headline, text.code);
    }

    fn show_state_error(&self, error: &MetadataError) {
        self.set_state(&StateText::from_error(error));
    }

    fn hub(&self, node: &NodeView) -> Option<Rc<MetadataHub>> {
        self.hub_of
            .borrow()
            .as_ref()
            .and_then(|resolver| resolver(&node.reference))
    }

    // -- 表 / 视图 ----------------------------------------------------------

    fn load_table(self: &Rc<Self>, node: NodeView, generation: u64, loaded_rows: Option<usize>) {
        // DDL 复用面板：原文 / 拼装 / 状态页都在那里判定，不重复一份。
        self.ddl.show_for(node.clone());
        *self.info.borrow_mut() = TableInfo {
            columns: None,
            primary_key: None,
            indexes: None,
            triggers: None,
            loaded_rows,
            children_error: None,
        };
        self.column_count.set(0);
        self.render_table_info();
        self.info_status.set_visible(true);
        self.info_status.set_text("正在读取列、主键与索引…");
        let Some(hub) = self.hub(&node) else {
            self.info_status.set_visible(true);
            self.info_status.set_text(
                "这条连接没有浏览会话：列属性读不到（metadata.object_not_found）。在对象树里重新连接后再试。",
            );
            return;
        };
        let weak = Rc::downgrade(self);
        hub.request_columns(
            node.reference.clone(),
            Rc::new(move |result| {
                let Some(panel) = weak.upgrade() else { return };
                if panel.generation.get() != generation {
                    return;
                }
                match result {
                    Ok(columns) => {
                        panel.info.borrow_mut().columns = Some(columns);
                        panel.render_table_info();
                    }
                    Err(error) => panel.info_note(&StateText::from_error(&error)),
                }
            }),
        );
        let weak = Rc::downgrade(self);
        hub.request_primary_key(
            node.reference.clone(),
            Rc::new(move |result| {
                let Some(panel) = weak.upgrade() else { return };
                if panel.generation.get() != generation {
                    return;
                }
                match result {
                    Ok(columns) => {
                        panel.info.borrow_mut().primary_key = Some(
                            columns
                                .iter()
                                .map(|column| (column.position, column.name.clone()))
                                .collect(),
                        );
                        panel.render_table_info();
                    }
                    Err(error) => panel.info_note(&StateText::from_error(&error)),
                }
            }),
        );
        // 索引与触发器是表的直接子节点（`ListNodes`，按种类过滤）。
        let weak = Rc::downgrade(self);
        hub.request_group(
            node.reference.clone(),
            &[ObjectKind::Index, ObjectKind::Trigger],
            "",
            Rc::new(move |result| {
                let Some(panel) = weak.upgrade() else { return };
                if panel.generation.get() != generation {
                    return;
                }
                match result {
                    Ok(page) => {
                        let mut indexes = Vec::new();
                        let mut triggers = Vec::new();
                        for child in &page.nodes {
                            match child.kind {
                                ObjectKind::Index => indexes.push(child.display_name.clone()),
                                ObjectKind::Trigger => triggers.push(child.display_name.clone()),
                                _ => {}
                            }
                        }
                        let mut info = panel.info.borrow_mut();
                        info.indexes = Some(indexes);
                        info.triggers = Some(triggers);
                        drop(info);
                        panel.render_table_info();
                    }
                    Err(error) => {
                        let text = StateText::from_error(&error);
                        panel.info.borrow_mut().children_error =
                            Some(format!("{}（{}）", text.headline, text.code));
                        panel.render_table_info();
                    }
                }
            }),
        );
    }

    /// 列节点：读父表的列清单，再挑出这一列（`GetColumns` 的粒度是表，不是列）。
    fn load_columns_for_column(self: &Rc<Self>, table: NodeView, name: String, generation: u64) {
        let Some(hub) = self.hub(&table) else {
            self.set_state(&StateText {
                headline: "这条连接没有浏览会话",
                detail: "列属性要经对象所属连接的浏览会话读取（backend.unavailable）。".to_owned(),
                code: "backend.unavailable".to_owned(),
            });
            return;
        };
        let weak = Rc::downgrade(self);
        hub.request_columns(
            table.reference.clone(),
            Rc::new(move |result| {
                let Some(panel) = weak.upgrade() else { return };
                if panel.generation.get() != generation {
                    return;
                }
                match result {
                    Ok(columns) => {
                        let Some(column) = columns.iter().find(|column| column.name == name) else {
                            panel.set_state(&StateText {
                                headline: "这一列已不存在",
                                detail: format!(
                                    "表 {} 的列清单里没有 {name}：对象可能在读取期间被修改\
                                     （metadata.object_not_found）。",
                                    table.display_name
                                ),
                                code: "metadata.object_not_found".to_owned(),
                            });
                            return;
                        };
                        panel.subtitle.set_text(&format!(
                            "列 · {} · {}",
                            table.display_name, column.type_label
                        ));
                        let facts: Vec<Fact> = column_info_rows(column)
                            .into_iter()
                            .map(|(label, value)| Fact::new(label, value))
                            .collect();
                        fill_facts(&panel.column_rows, &facts);
                        panel.column_count.set(1);
                        panel.set_page("column");
                    }
                    Err(error) => panel.show_state_error(&error),
                }
            }),
        );
    }

    fn info_note(&self, text: &StateText) {
        self.info_status.set_visible(true);
        self.info_status
            .set_text(&format!("{}（{}）", text.headline, text.code));
    }

    /// 重建表信息页：每一节只在有数据时出现，空值不占位。
    fn render_table_info(&self) {
        while let Some(child) = self.info_box.first_child() {
            if child == *self.info_status.upcast_ref::<gtk::Widget>() {
                break;
            }
            self.info_box.remove(&child);
        }
        // 先摘掉旧内容，再按当前数据重排（提示行保持在最上面）。
        let mut children = Vec::new();
        let mut child = self.info_box.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if current != *self.info_status.upcast_ref::<gtk::Widget>() {
                children.push(current);
            }
        }
        for child in children {
            self.info_box.remove(&child);
        }

        let info = self.info.borrow();
        let mut rendered = 0usize;
        if let Some(columns) = info.columns.as_ref() {
            self.column_count.set(columns.len());
            if columns.is_empty() {
                self.info_box
                    .append(&section_note("列", "服务端返回了空列清单"));
                rendered += 1;
            } else {
                let section = section("列");
                for column in columns {
                    let card = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::Xxs));
                    card.set_hexpand(true);
                    card.add_css_class("sqarvo-inspector-column");
                    let heading =
                        gtk::Label::new(Some(&format!("{} · {}", column.name, column.type_label)));
                    heading.set_xalign(0.0);
                    heading.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    heading.add_css_class("sqarvo-inspector-column-name");
                    heading.add_css_class("sqarvo-panel-text");
                    heading.set_tooltip_text(Some(&column.name));
                    card.append(&heading);
                    for (label, value) in [
                        ("可空", nullable_label(column.nullable).to_owned()),
                        ("默认值", default_label(&column.default_value)),
                        (
                            "字符集",
                            crate::column_info::charset_label(column.character_set.as_deref()),
                        ),
                        (
                            "排序规则",
                            crate::column_info::collation_label(column.collation.as_deref()),
                        ),
                    ] {
                        card.append(&compact_row(label, &value));
                    }
                    section.append(&card);
                }
                self.info_box.append(&section);
                rendered += 1;
            }
        }
        if let Some(primary_key) = info.primary_key.as_ref() {
            let section = section("主键");
            section.append(&compact_row("主键列", &primary_key_label(primary_key)));
            self.info_box.append(&section);
            rendered += 1;
        }
        if let Some(indexes) = info.indexes.as_ref() {
            let section = section("索引");
            section.append(&compact_row(
                "索引",
                &list_fact("索引", indexes, "无索引").value,
            ));
            self.info_box.append(&section);
            rendered += 1;
        }
        if let Some(triggers) = info.triggers.as_ref() {
            let section = section("触发器");
            section.append(&compact_row(
                "触发器",
                &list_fact("触发器", triggers, "无触发器").value,
            ));
            self.info_box.append(&section);
            rendered += 1;
        }
        if let Some(fact) = loaded_rows_fact(info.loaded_rows) {
            let section = section("行");
            section.append(&compact_row(fact.label, &fact.value));
            self.info_box.append(&section);
            rendered += 1;
        }
        if let Some(note) = info.children_error.as_ref() {
            let section = section("索引与触发器");
            section.append(&section_note("读取失败", note));
            self.info_box.append(&section);
            rendered += 1;
        }
        if rendered > 0 {
            self.info_status.set_visible(false);
        }
    }

    /// QA：面板快照（全部来自真实控件）。
    pub fn qa_observe(&self) -> InspectorObservation {
        let page = self.page.borrow().clone();
        let state_detail = if page == "state" {
            self.state_page
                .description()
                .map(|text| text.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let ddl_observation = match page.as_str() {
            "table" => self.ddl.qa_observe(),
            "ddl-only" => self.ddl_only.qa_observe(),
            _ => Default::default(),
        };
        InspectorObservation {
            visible: self.root.is_mapped(),
            title: self.title.text().to_string(),
            subtitle: self.subtitle.text().to_string(),
            page,
            table_page: if self.current_pane.get() == TablePage::Ddl {
                "ddl".to_owned()
            } else {
                "info".to_owned()
            },
            facts: collect_facts(self.root.upcast_ref::<gtk::Widget>()),
            txn_hint: if self.txn_hint.is_visible() {
                self.txn_hint.text().to_string()
            } else {
                String::new()
            },
            columns: self.column_count.get(),
            ddl_state: ddl_observation.state.name().to_owned(),
            ddl_text: ddl_observation.text,
            state_detail,
        }
    }

    /// 布局自检目标：只测面板自身的头部与切换条（宽度由外壳记账）。
    pub fn layout_audit_targets(&self) -> Vec<crate::layout_audit::Target> {
        vec![crate::layout_audit::Target::row(
            "信息面板 头部",
            self.title
                .parent()
                .unwrap_or_else(|| self.title.clone().upcast()),
        )]
    }
}

/// 字段行容器（可滚动、右内边距 10）。
fn fact_container() -> gtk::Box {
    let rows = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::Sm));
    rows.set_margin_top(gap(Gap::Md));
    rows.set_margin_bottom(gap(Gap::Md));
    rows.set_margin_start(PANEL_CONTENT_LEFT);
    rows.set_margin_end(gap(Gap::Lg));
    rows.set_valign(gtk::Align::Start);
    rows
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(child)
        .build()
}

/// 重填字段行：整块替换，不做增量比对（一屏十几行，成本可忽略）。
fn fill_facts(container: &gtk::Box, facts: &[Fact]) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    for fact in facts {
        if fact.value.trim().is_empty() {
            continue;
        }
        let row = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        let label = gtk::Label::new(Some(fact.label));
        label.set_xalign(0.0);
        label.add_css_class("dim-label");
        label.add_css_class("sqarvo-fact-label");
        label.add_css_class("sqarvo-panel-text");
        let value = gtk::Label::new(Some(&fact.value));
        value.set_xalign(0.0);
        value.set_wrap(true);
        // 逐字折行：凭据引用这种长 token 没有空格，按词折行会把面板的最小宽度
        // 顶到 299px（面板本该 280）——那 19px 是标签的最小宽度，不是用户拖的。
        value.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        value.set_selectable(true);
        value.set_hexpand(true);
        value.add_css_class("sqarvo-panel-text");
        row.append(&label);
        row.append(&value);
        container.append(&row);
    }
}

/// 一个带标题的分节。
fn section(title: &str) -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::Xs));
    box_.set_hexpand(true);
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.add_css_class("sqarvo-panel-text");
    box_.append(&label);
    box_
}

fn section_note(title: &str, note: &str) -> gtk::Box {
    let box_ = section(title);
    let label = gtk::Label::new(Some(note));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("dim-label");
    label.add_css_class("sqarvo-panel-text");
    box_.append(&label);
    box_
}

fn compact_row(label: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, gap(Gap::Sm));
    row.set_hexpand(true);
    let key = gtk::Label::new(Some(label));
    key.set_xalign(0.0);
    key.set_valign(gtk::Align::Start);
    key.set_size_request(48, -1);
    key.add_css_class("dim-label");
    key.add_css_class("sqarvo-panel-text");
    let text = gtk::Label::new(Some(value));
    text.set_xalign(0.0);
    text.set_wrap(true);
    text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    text.set_hexpand(true);
    text.set_selectable(true);
    text.add_css_class("sqarvo-panel-text");
    row.append(&key);
    row.append(&text);
    row
}

/// 收集面板上所有「字段 = 值」的文本（QA 断言读真实控件）。
fn collect_facts(root: &gtk::Widget) -> Vec<String> {
    let mut facts = Vec::new();
    walk(root, &mut facts);
    facts
}

fn walk(widget: &gtk::Widget, facts: &mut Vec<String>) {
    if let Some(box_) = widget.downcast_ref::<gtk::Box>()
        && box_.orientation() == gtk::Orientation::Vertical
        && let (Some(first), Some(second)) = (box_.first_child(), box_.last_child())
        && first != second
        && let (Some(label), Some(value)) = (
            first.downcast_ref::<gtk::Label>(),
            second.downcast_ref::<gtk::Label>(),
        )
        && label.has_css_class("sqarvo-fact-label")
    {
        facts.push(format!("{}={}", label.text(), value.text()));
        return;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        walk(&current, facts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(driver: &str) -> SavedConnection {
        SavedConnection {
            name: "本地测试库".to_owned(),
            driver: driver.to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 53306,
            database: "sqarvo_test".to_owned(),
            username: "sqarvo_test".to_owned(),
            file_path: String::new(),
            read_only: false,
            tls_mode: "preferred".to_owned(),
            credential_ref: "sqarvo:conn/abc".to_owned(),
        }
    }

    fn sqlite(path: &str) -> SavedConnection {
        SavedConnection {
            name: "demo.db".to_owned(),
            driver: crate::config::SQLITE_DRIVER_ID.to_owned(),
            host: String::new(),
            port: 0,
            database: String::new(),
            username: String::new(),
            file_path: path.to_owned(),
            read_only: true,
            tls_mode: "disabled".to_owned(),
            credential_ref: String::new(),
        }
    }

    #[test]
    fn connection_facts_cover_the_spec_fields_in_order() {
        let live = ConnectionLive {
            server_version: "10.6.16-MariaDB".to_owned(),
            latency_millis: Some(12),
            sessions: Some(3),
        };
        let facts = connection_facts(&saved("mariadb"), &live);
        let labels: Vec<&str> = facts.iter().map(|fact| fact.label).collect();
        assert_eq!(
            labels,
            [
                "驱动",
                "地址",
                "默认库",
                "用户",
                "TLS",
                "凭据引用",
                "服务端版本",
                "连接延迟",
                "会话数"
            ]
        );
        assert_eq!(facts[1].value, "127.0.0.1:53306");
        assert_eq!(facts[7].value, "12 ms");
        assert_eq!(facts[8].value, "3 条");
    }

    #[test]
    fn connection_facts_show_unspecified_without_a_default_database() {
        let mut entry = saved("mariadb");
        entry.database = String::new();
        let facts = connection_facts(&entry, &ConnectionLive::default());
        let database = facts
            .iter()
            .find(|fact| fact.label == "默认库")
            .expect("服务器型连接的库一栏必须存在");
        assert_eq!(database.value, "未指定");
        // 地址不因缺库变成 `host:port/`。
        let endpoint = facts
            .iter()
            .find(|fact| fact.label == "地址")
            .expect("地址");
        assert_eq!(endpoint.value, "127.0.0.1:53306");

        // SQLite：database 为空是常态，这一行不该以「未指定」出现。
        let sqlite_facts = connection_facts(&sqlite("/tmp/demo.db"), &ConnectionLive::default());
        assert!(
            sqlite_facts.iter().all(|fact| fact.label != "默认库"),
            "SQLite 不应出现「默认库」行：{sqlite_facts:?}"
        );
    }

    #[test]
    fn connection_facts_never_contain_a_password() {
        let facts = connection_facts(&saved("mariadb"), &ConnectionLive::default());
        for fact in &facts {
            assert!(!fact.value.to_lowercase().contains("password"));
            assert!(!fact.label.contains("密码"));
        }
        // 凭据引用在，但它只是引用（不含明文）。
        assert!(facts.iter().any(|fact| fact.label == "凭据引用"));
    }

    #[test]
    fn unknown_connection_fields_are_omitted_not_placeholders() {
        // 没有服务端版本 / 延迟 / 会话数：这三行整行不出现，不写「未知」或「N/A」。
        let facts = connection_facts(&saved("mariadb"), &ConnectionLive::default());
        for label in ["服务端版本", "连接延迟", "会话数"] {
            assert!(
                !facts.iter().any(|fact| fact.label == label),
                "{label} 没有数据时不该出现：{facts:?}"
            );
        }
        for value in facts.iter().map(|fact| fact.value.as_str()) {
            assert!(!matches!(value, "—" | "N/A" | "未知"), "{value}");
        }
    }

    #[test]
    fn sqlite_connection_uses_the_file_path_and_skips_tls() {
        let facts = connection_facts(&sqlite("/tmp/demo.db"), &ConnectionLive::default());
        assert_eq!(facts[1].label, "地址");
        assert_eq!(facts[1].value, "/tmp/demo.db");
        assert!(!facts.iter().any(|fact| fact.label == "TLS"));
        assert!(!facts.iter().any(|fact| fact.label == "凭据引用"));
        assert!(!facts.iter().any(|fact| fact.label == "用户"));
    }

    #[test]
    fn catalog_counts_are_reported_only_when_the_layer_is_loaded() {
        let facts = catalog_facts("shop", None, None, false);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].value, "shop");

        let facts = catalog_facts("shop", Some(12), Some(3), true);
        assert_eq!(facts[1].value, "12 个");
        assert_eq!(facts[2].value, "3 个");
        assert!(!facts.iter().any(|fact| fact.label == "计数说明"));

        // 只展开了 Tables：视图那一行不出现（不是 0）。
        let facts = catalog_facts("shop", Some(12), None, false);
        assert!(!facts.iter().any(|fact| fact.label == "视图"));
        assert!(facts.iter().any(|fact| fact.label == "计数说明"));

        // 还有下一页：必须说明这是「已展开」的计数，不能冒充总数。
        let facts = catalog_facts("shop", Some(500), Some(0), false);
        assert!(facts.iter().any(|fact| fact.label == "计数说明"));
    }

    #[test]
    fn primary_key_label_keeps_the_composite_order() {
        let key = vec![(2, "tenant".to_owned()), (1, "id".to_owned())];
        assert_eq!(primary_key_label(&key), "id（1） · tenant（2）");
        assert_eq!(primary_key_label(&[]), "无主键");
    }

    #[test]
    fn empty_index_and_trigger_lists_are_said_out_loud() {
        assert_eq!(list_fact("索引", &[], "无索引").value, "无索引");
        let names = vec!["idx_name".to_owned(), "idx_created".to_owned()];
        assert_eq!(
            list_fact("索引", &names, "无索引").value,
            "idx_name · idx_created"
        );
    }

    #[test]
    fn loaded_rows_fact_is_optional() {
        assert!(loaded_rows_fact(None).is_none());
        assert_eq!(loaded_rows_fact(Some(500)).unwrap().value, "500 行");
    }

    #[test]
    fn unsupported_kind_text_carries_the_stable_code() {
        let text = StateText::unsupported_kind(ObjectKind::Index);
        assert_eq!(text.code.as_str(), "metadata.object_kind_unsupported");
        assert!(text.headline.contains("不支持"));
        assert!(text.detail.contains("索引"));
    }

    #[test]
    fn error_state_texts_are_distinct_for_the_three_failure_classes() {
        let denied = StateText::from_error(&MetadataError::PermissionDenied {
            message: "SELECT command denied".to_owned(),
        });
        let missing = StateText::from_error(&MetadataError::NotFound {
            message: "table does not exist".to_owned(),
        });
        let unsupported = StateText::from_error(&MetadataError::KindUnsupported {
            message: "no ddl for index".to_owned(),
        });
        assert_eq!(denied.code.as_str(), "metadata.permission_denied");
        assert_eq!(missing.code.as_str(), "metadata.object_not_found");
        assert_eq!(
            unsupported.code.as_str(),
            "metadata.object_kind_unsupported"
        );
        assert_ne!(denied.headline, missing.headline);
        assert_ne!(missing.headline, unsupported.headline);
        // 文案里必须带错误码，截图之外还能被 grep 到。
        assert!(denied.detail.contains(&denied.code));
        assert!(missing.detail.contains(&missing.code));
    }

    #[test]
    fn table_page_names_are_stable_for_qa() {
        assert_eq!(TablePage::Ddl.name(), "ddl");
        assert_eq!(TablePage::Info.name(), "info");
        assert_eq!(TablePage::parse("表信息"), Some(TablePage::Info));
        assert_eq!(TablePage::parse("DDL"), Some(TablePage::Ddl));
        assert_eq!(TablePage::parse("nope"), None);
    }
}
