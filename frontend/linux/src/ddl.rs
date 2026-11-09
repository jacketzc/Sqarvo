//! DDL 面板（#86）：显示 `GetDdl` 的结果。
//!
//! v0.2.3.3 起界面只留正文：来源徽标、「复制」按钮、来源说明行与面板自己的标题
//! 全部移除——它们夹在 `[DDL] / [表信息]` 切换和正文之间，而两个驱动都只交付数据库
//! 原文，这几行只是噪音。安全底线保留：`exact = false`（客户端拼装）时仍显示一行
//! 琥珀警示。拼装结果有信息损失（缺 CHECK 约束、缺索引……），用户必须知道，否则会
//! 拿它去做迁移。
//!
//! # 状态
//!
//! Loading（spinner）/ Empty（DDL 为空）/ Error（对象不存在、后端不可用）/
//! 不支持（`capability.unsupported` 与 `metadata.object_kind_unsupported` 分开）/
//! Ready。正文可选中（Ctrl+C 就是复制入口），**不折行**：DDL 是代码，在 280 宽的面板
//! 里按词折行会把一条语句断成看起来像两条；宽语句横向滚动。
//!
//! 字号 / 字重与「表信息」页同一口径（`theme.rs` 的 `.sqarvo-panel-text`，
//! `layout::FONT_PANEL` = 13px / Medium）：两页文字必须看起来是一套东西；12px 会裁
//! CJK 笔画，等宽字在 1x 屏幕上又更细更小，所以不用等宽。横向滚动保留——DDL 是代码，
//! 窄面板里按词折行会把一条语句断成两条。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::backend::metadata::{DdlView, MetadataError, MetadataHub, NodeView, kind_label};
use crate::proto::services::v1::ObjectRef;

/// 按对象的会话找元数据通道（多连接下 DDL 走对象**自己**那条浏览会话）。
pub type HubResolver = Rc<dyn Fn(&ObjectRef) -> Option<Rc<MetadataHub>>>;

/// DDL 面板的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DdlState {
    /// 还没请求。
    #[default]
    Idle,
    /// 正在读取。
    Loading,
    /// 有内容。
    Ready,
    /// 成功但 DDL 为空。
    Empty,
    /// 失败。
    Error,
    /// 该对象 / 该层级不支持 DDL。
    Unsupported,
}

impl DdlState {
    /// 日志与断言用的名字。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Empty => "empty",
            Self::Error => "error",
            Self::Unsupported => "unsupported",
        }
    }
}

/// QA 从面板控件上读回的快照（断言读真实控件，不读状态机）。
///
/// 只保留控件上真实存在的东西：来源徽标、「复制」按钮与来源说明行在 v0.2.3.3 已从
/// 界面移除，对应字段一并删除——留着它们只会让断言读一份没有控件承载的状态，永远
/// 是绿的。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DdlObservation {
    /// 面板是否可见。
    pub visible: bool,
    /// DDL 正文（只读文本视图的缓冲区）。
    pub text: String,
    /// 状态。
    pub state: DdlState,
}

/// 面板 CSS：拼装警示必须一眼可分辨（琥珀）。
///
/// 与 `results.rs` / `data_page.rs` 一样：每个模块自己装一次，用 `OnceLock`
/// 保证不会重复添加（重复添加会让样式叠加，颜色越来越深）。
fn install_css() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(r#"label.sqarvo-ddl-synth { color: @warning_color; }"#);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// DDL 面板（嵌在信息面板里：没有自己的标题、关闭按钮与固定高度）。
pub struct DdlPanel {
    root: gtk::Box,
    stack: gtk::Stack,
    state_page: adw::StatusPage,
    spinner: gtk::Spinner,
    /// 只在 `exact = false`（客户端拼装）时出现的警示行。
    warning: gtk::Label,
    text_view: gtk::TextView,
    /// 按对象自己的会话找元数据通道：两条连接各看各的 DDL（#114）。
    hub_of: RefCell<Option<HubResolver>>,
    current: RefCell<Option<NodeView>>,
    state: Cell<DdlState>,
    /// 面板是否被用户打开着（由 ui.rs 的 revealer 状态驱动，这里只记一份）。
    visible: Cell<bool>,
}

impl DdlPanel {
    /// 构建面板。
    pub fn new() -> Rc<Self> {
        let warning = gtk::Label::new(None);
        warning.set_xalign(0.0);
        warning.set_wrap(true);
        warning.set_visible(false);
        warning.set_margin_top(6);
        warning.set_margin_bottom(2);
        warning.set_margin_start(0);
        warning.set_margin_end(10);
        warning.add_css_class("caption");
        warning.add_css_class("sqarvo-ddl-synth");

        let text_view = gtk::TextView::new();
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        // DDL 是代码：不折行，宽语句横向滚动（与数据网格同一读法）。
        text_view.set_wrap_mode(gtk::WrapMode::None);
        text_view.set_top_margin(8);
        text_view.set_bottom_margin(8);
        // 左端贴内容区边缘：面板只有一条左基准线（与分隔线左顶点、标题同一列）。
        text_view.set_left_margin(0);
        text_view.set_right_margin(10);
        text_view.add_css_class("sqarvo-panel-text");

        // DDL 不折行：长语句必须能横向滚到，滚动条不能靠悬停才出现——这里关掉
        // overlay 自动淡出（与数据网格同一口径：能滚到的地方要看得见）。
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .overlay_scrolling(false)
            .vexpand(true)
            .hexpand(true)
            .child(&text_view)
            .build();

        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        spinner.set_margin_top(18);
        spinner.set_margin_bottom(18);

        let state_page = adw::StatusPage::new();
        state_page.set_vexpand(true);
        state_page.set_child(Some(&spinner));

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&scroll, Some("text"));
        stack.add_named(&state_page, Some("state"));
        stack.set_visible_child_name("state");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&warning);
        root.append(&stack);

        let panel = Rc::new(Self {
            root,
            stack,
            state_page,
            spinner,
            warning,
            text_view,
            hub_of: RefCell::new(None),
            current: RefCell::new(None),
            state: Cell::new(DdlState::Idle),
            visible: Cell::new(false),
        });

        install_css();
        panel
    }

    /// 根控件。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 接线到「按会话找元数据通道」的解析器。
    ///
    /// 不直接持有一个 hub：一个窗口里可以有多条连接，每条连接一条浏览会话
    /// （ADR 0021）；DDL 必须走**对象自己那条**会话。
    pub fn attach_resolver(&self, resolver: HubResolver) {
        self.hub_of.replace(Some(resolver));
    }

    /// 标记面板可见性（ui.rs 的 revealer 变化时调用）。
    pub fn set_visible(&self, visible: bool) {
        self.visible.set(visible);
    }

    /// 查看某个对象的 DDL（清空上一次内容并进入 Loading）。
    pub fn show_for(self: &Rc<Self>, node: NodeView) {
        self.warning.set_visible(false);
        self.warning.set_text("");
        self.buffer().set_text("");
        self.current.replace(Some(node.clone()));
        self.set_state(DdlState::Loading, "正在读取 DDL…", "等待 GetDdl 返回");
        eprintln!(
            "[ddl] t=+{}ms 请求 DDL：{}{}",
            crate::qa::elapsed_ms(),
            kind_label(node.kind),
            node.path_label()
        );
        let hub = self
            .hub_of
            .borrow()
            .as_ref()
            .and_then(|resolver| resolver(&node.reference));
        let Some(hub) = hub else {
            self.set_state(
                DdlState::Error,
                "这条连接没有浏览会话",
                "对象所属连接的会话已断开：在对象树里点该连接重新连接后再试。",
            );
            return;
        };
        let weak = Rc::downgrade(self);
        let object = node.path_label();
        hub.request_ddl(
            node.reference.clone(),
            Rc::new(move |result| {
                if let Some(panel) = weak.upgrade() {
                    panel.finish(&object, result);
                }
            }),
        );
    }

    fn finish(self: &Rc<Self>, object: &str, result: Result<DdlView, MetadataError>) {
        match result {
            Ok(view) => {
                if view.ddl.trim().is_empty() {
                    self.set_state(
                        DdlState::Empty,
                        "服务端返回了空 DDL",
                        "这不代表对象没有结构：契约要求不返回伪造或截断的 DDL，请换一个对象或检查服务端日志。",
                    );
                    return;
                }
                self.buffer().set_text(&view.ddl);
                if view.exact {
                    // 原文：不再写「数据库原文（DDL_SOURCE_DATABASE）」这类说明行。
                    self.warning.set_text("");
                    self.warning.set_visible(false);
                } else {
                    // 拼装结果：必须带警示，且要能看出缺了什么、由哪版拼装器生成。
                    self.warning.set_text(&format!(
                        "⚠ 这份 DDL 由客户端拼装，不是数据库原文；信息有损失 · {}",
                        view.detail_text()
                    ));
                    self.warning.set_visible(true);
                }
                self.stack.set_visible_child_name("text");
                self.state.set(DdlState::Ready);
                eprintln!(
                    "[ddl] t=+{}ms DDL {object}：{} 行，exact={}，source_detail={}",
                    crate::qa::elapsed_ms(),
                    view.ddl.lines().count(),
                    view.exact,
                    view.source_detail
                );
            }
            Err(error) => {
                let (state, title) = if error.is_unsupported()
                    || matches!(error, MetadataError::KindUnsupported { .. })
                {
                    (DdlState::Unsupported, error.headline())
                } else if error.is_session_lost() {
                    (DdlState::Error, "数据库会话已失效")
                } else {
                    (DdlState::Error, error.headline())
                };
                self.set_state(
                    state,
                    title,
                    &format!("{}（{}）。{}", error.message(), error.code(), error.hint()),
                );
                eprintln!(
                    "[ddl] {object} 读取失败：[{}] {}",
                    error.code(),
                    error.message()
                );
            }
        }
    }

    fn set_state(&self, state: DdlState, title: &str, description: &str) {
        self.state.set(state);
        self.state_page.set_title(title);
        self.state_page.set_description(Some(description));
        self.spinner.set_visible(state == DdlState::Loading);
        if state == DdlState::Loading {
            self.spinner.start();
        } else {
            self.spinner.stop();
        }
        self.stack.set_visible_child_name("state");
    }

    fn buffer(&self) -> gtk::TextBuffer {
        self.text_view.buffer()
    }

    /// QA：面板快照（全部来自真实控件文本）。
    pub fn qa_observe(&self) -> DdlObservation {
        DdlObservation {
            visible: self.visible.get(),
            text: self
                .buffer()
                .text(
                    &self.buffer().start_iter(),
                    &self.buffer().end_iter(),
                    false,
                )
                .to_string(),
            state: self.state.get(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_names_are_stable_for_qa() {
        assert_eq!(DdlState::Ready.name(), "ready");
        assert_eq!(DdlState::Unsupported.name(), "unsupported");
        assert_eq!(DdlState::Empty.name(), "empty");
        assert_eq!(DdlState::Error.name(), "error");
        assert_eq!(DdlState::Loading.name(), "loading");
        assert_eq!(DdlState::Idle.name(), "idle");
    }
}
