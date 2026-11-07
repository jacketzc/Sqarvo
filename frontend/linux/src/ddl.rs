//! DDL 面板（#86）：显示 `GetDdl` 结果，并**显式标注来源**。
//!
//! # 为什么必须显示 `exact`
//!
//! `GetDdl` 的契约允许服务端在拿不到原文时**拼装** DDL（`exact = false`）。
//! 拼装结果有信息损失（例如缺 CHECK 约束、缺索引），用户必须能一眼看出
//! 「这是数据库给的原文」还是「这是拼出来的」，否则会拿不可信的 DDL 去做迁移。
//! 因此面板上有三处固定信息：
//!
//! 1. 徽标：**原文** / **由客户端拼装**（只看 `exact`；`source` 未说明时按 `exact` 判断）；
//! 2. 来源说明（`source_detail`）：原文取自哪里、拼装缺了什么；
//! 3. 拼装时额外一行警示：`generator_version` 与「有信息损失」。
//!
//! # 状态
//!
//! Loading（spinner）/ Empty（DDL 为空）/ Error（对象不存在、后端不可用）/
//! 不支持（`capability.unsupported` 与 `metadata.object_kind_unsupported` 分开）/
//! Ready。长文本可滚动，内容是等宽字体，可一键复制。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::backend::metadata::{DdlView, MetadataError, MetadataHub, NodeView, kind_label};
use crate::proto::services::v1::{DdlSource, ObjectRef};

/// 按对象的会话找元数据通道（多连接下 DDL 走对象**自己**那条浏览会话）。
pub type HubResolver = Rc<dyn Fn(&ObjectRef) -> Option<Rc<MetadataHub>>>;

/// DDL 面板的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdlState {
    /// 还没请求。
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdlObservation {
    /// 面板是否可见。
    pub visible: bool,
    /// 标题上的对象名。
    pub object: String,
    /// 来源徽标文本（**原文** / **由客户端拼装**）。
    pub provenance: String,
    /// 来源说明文本。
    pub detail: String,
    /// DDL 正文（只读文本视图的缓冲区）。
    pub text: String,
    /// 状态。
    pub state: DdlState,
    /// 「复制」按钮是否可点。
    pub copy_enabled: bool,
}

/// 面板 CSS：来源徽标必须**一眼可分辨**（原文 = 绿、拼装 = 琥珀）。
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
    provider.load_from_string(
        r#"
        .sqarvo-ddl-badge { border-radius: 6px; padding: 0 6px; }
        .sqarvo-ddl-badge.sqarvo-ddl-exact { background: alpha(@success_color, 0.15); color: @success_color; }
        .sqarvo-ddl-badge.sqarvo-ddl-synth { background: alpha(@warning_color, 0.18); color: @warning_color; }
        label.sqarvo-ddl-synth { color: @warning_color; }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// 徽标 CSS 类：原文与拼装必须能靠颜色分辨（也便于 QA 断言）。
#[must_use]
pub fn provenance_css(exact: bool) -> &'static str {
    if exact {
        "sqarvo-ddl-exact"
    } else {
        "sqarvo-ddl-synth"
    }
}

/// DDL 面板。
pub struct DdlPanel {
    root: gtk::Box,
    stack: gtk::Stack,
    state_page: adw::StatusPage,
    spinner: gtk::Spinner,
    object_label: gtk::Label,
    badge: gtk::Label,
    detail: gtk::Label,
    warning: gtk::Label,
    text_view: gtk::TextView,
    copy: gtk::Button,
    /// 按对象自己的会话找元数据通道：两条连接各看各的 DDL（#114）。
    hub_of: RefCell<Option<HubResolver>>,
    /// 最近一次的结果：复制按钮用它取**纯 DDL**（不带来源标注）。
    last: RefCell<Option<DdlView>>,
    current: RefCell<Option<NodeView>>,
    state: Cell<DdlState>,
    /// 面板是否被用户打开着（由 ui.rs 的 revealer 状态驱动，这里只记一份）。
    visible: Cell<bool>,
}

impl DdlPanel {
    /// 构建面板；`on_close` 由「关闭」按钮触发。
    pub fn new(on_close: Rc<dyn Fn()>) -> Rc<Self> {
        let object_label = gtk::Label::new(Some("DDL"));
        object_label.set_xalign(0.0);
        object_label.set_hexpand(true);
        object_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        object_label.add_css_class("heading");

        let badge = gtk::Label::new(None);
        badge.add_css_class("caption");
        badge.add_css_class("sqarvo-ddl-badge");
        badge.set_visible(false);

        let detail = gtk::Label::new(None);
        detail.set_xalign(0.0);
        detail.set_wrap(true);
        detail.set_visible(false);
        detail.add_css_class("dim-label");
        detail.add_css_class("caption");

        let warning = gtk::Label::new(None);
        warning.set_xalign(0.0);
        warning.set_wrap(true);
        warning.set_visible(false);
        warning.add_css_class("caption");
        warning.add_css_class("sqarvo-ddl-synth");

        let copy = gtk::Button::with_label("复制");
        copy.set_tooltip_text(Some("复制 DDL 到剪贴板"));
        copy.add_css_class("flat");
        copy.set_sensitive(false);

        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.set_tooltip_text(Some("关闭 DDL 面板"));
        close.add_css_class("flat");
        {
            let on_close = Rc::clone(&on_close);
            close.connect_clicked(move |_| on_close());
        }

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.set_margin_top(6);
        header.set_margin_bottom(4);
        header.set_margin_start(10);
        header.set_margin_end(6);
        header.append(&object_label);
        header.append(&badge);
        header.append(&copy);
        header.append(&close);

        let text_view = gtk::TextView::new();
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_monospace(true);
        text_view.set_wrap_mode(gtk::WrapMode::None);
        text_view.set_top_margin(6);
        text_view.set_bottom_margin(6);
        text_view.set_left_margin(8);
        text_view.set_right_margin(8);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
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
        root.set_size_request(-1, 220);
        root.append(&header);
        root.append(&detail);
        root.append(&warning);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&stack);

        let panel = Rc::new(Self {
            root,
            stack,
            state_page,
            spinner,
            object_label,
            badge,
            detail,
            warning,
            text_view,
            copy,
            hub_of: RefCell::new(None),
            last: RefCell::new(None),
            current: RefCell::new(None),
            state: Cell::new(DdlState::Idle),
            visible: Cell::new(false),
        });

        panel.install_copy();
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
        self.object_label.set_text(&format!(
            "{} · {}",
            node.display_name,
            kind_label(node.kind)
        ));
        self.object_label.set_tooltip_text(Some(&node.path_label()));
        self.badge.set_visible(false);
        self.badge.set_text("");
        self.detail.set_visible(false);
        self.detail.set_text("");
        self.warning.set_visible(false);
        self.warning.set_text("");
        self.copy.set_sensitive(false);
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
                self.last.replace(Some(view.clone()));
                self.badge.set_text(view.provenance_label());
                self.badge.remove_css_class(provenance_css(true));
                self.badge.remove_css_class(provenance_css(false));
                self.badge.add_css_class(provenance_css(view.exact));
                self.badge.set_visible(true);
                // 来源说明**始终**展示：exact=false 时用户必须知道缺了什么；
                // source 与 exact 不一致（例如旧后端的 UNSPECIFIED）也要说明。
                let mut detail_text = view.detail_text();
                if let Some(note) = view.provenance_note() {
                    detail_text.push_str(&format!(" · {note}"));
                }
                self.detail.set_text(&detail_text);
                self.detail.set_visible(true);
                if view.exact {
                    self.warning.set_text(&format!(
                        "数据库原文（{}）",
                        match view.source {
                            DdlSource::Database => "DDL_SOURCE_DATABASE",
                            _ => "来源未说明",
                        }
                    ));
                    self.warning.remove_css_class("sqarvo-ddl-synth");
                    self.warning.add_css_class("dim-label");
                } else {
                    // 拼装结果：必须带警示，且要有生成器版本可追溯。
                    self.warning.set_text(&format!(
                        "⚠ 这份 DDL 由客户端拼装，不是数据库原文；信息有损失{}",
                        if view.generator_version.trim().is_empty() {
                            String::new()
                        } else {
                            format!("（拼装器 {}）", view.generator_version)
                        }
                    ));
                    self.warning.remove_css_class("dim-label");
                    self.warning.add_css_class("sqarvo-ddl-synth");
                }
                self.warning.set_visible(true);
                self.copy.set_sensitive(true);
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

    fn install_copy(self: &Rc<Self>) {
        self.copy.connect_clicked({
            let weak = Rc::downgrade(self);
            move |button| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                // 复制的是 DDL 本身，不含「原文 / 由客户端拼装」标注：
                // 标注是给人看的，粘到迁移脚本里只会变成语法错误。
                let text = panel
                    .last
                    .borrow()
                    .as_ref()
                    .map(DdlView::clipboard_text)
                    .unwrap_or_default();
                if text.trim().is_empty() {
                    return;
                }
                button.display().clipboard().set_text(&text);
                let unit = text.len();
                eprintln!("[ddl] 已复制 DDL 到剪贴板（{unit} 字节）");
                button.set_label("已复制");
                let button = button.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(1_500), move || {
                    button.set_label("复制");
                });
            }
        });
    }

    /// QA：面板快照（全部来自真实控件文本）。
    pub fn qa_observe(&self) -> DdlObservation {
        DdlObservation {
            visible: self.visible.get(),
            object: self.object_label.text().to_string(),
            provenance: self.badge.text().to_string(),
            detail: self.detail.text().to_string(),
            text: self
                .buffer()
                .text(
                    &self.buffer().start_iter(),
                    &self.buffer().end_iter(),
                    false,
                )
                .to_string(),
            state: self.state.get(),
            copy_enabled: self.copy.is_sensitive(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_badge_classes_are_distinct() {
        // 原文与拼装必须能靠样式区分：QA 会断言徽标文本，人靠颜色。
        assert_ne!(provenance_css(true), provenance_css(false));
        assert_eq!(provenance_css(true), "sqarvo-ddl-exact");
        assert_eq!(provenance_css(false), "sqarvo-ddl-synth");
    }

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
