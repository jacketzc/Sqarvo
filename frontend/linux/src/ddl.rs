//! DDL 面板（#86）：显示 `GetDdl` 的结果。
//!
//! v0.2.3.3 起表 / 视图 DDL 页只留正文：来源徽标、「复制」按钮、来源说明行与面板自己的标题
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
//!
//! **无缓存**：每次 `show_for` 都发一次 `GetDdl`；过期的成功 / 错误响应在碰任何控件
//! 之前被丢弃（见 [`LoadEpoch`]）。面板不保存历史对象的正文，也不替信息面板写任何
//! 跨访问结果。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::backend::metadata::{DdlView, MetadataError, MetadataHub, NodeView, kind_label};
use crate::l10n;
use crate::layout::{Gap, gap};
use crate::proto::services::v1::ObjectRef;

/// 按对象的会话找元数据通道（多连接下 DDL 走对象**自己**那条浏览会话）。
pub type HubResolver = Rc<dyn Fn(&ObjectRef) -> Option<Rc<MetadataHub>>>;

/// 一次加载周期的令牌。
///
/// 与对象身份**正交**：A → B → A 的第三次加载会拿到新令牌，第一次 A 的迟到响应
/// 依然过期。任何新加载、换对象、换子页、主动刷新、断连都会开启新周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadToken(u64);

/// 加载周期计数器：新周期使此前的全部令牌失效。
///
/// 信息面板与它内嵌的两个 DDL 面板共用同一份计数器，因此表信息页的加载也会让
/// 上一条 DDL 请求失效，反之亦然。
#[derive(Debug, Default)]
pub struct LoadEpoch(std::cell::Cell<u64>);

impl LoadEpoch {
    /// 开启一个新周期，返回该周期的令牌。
    #[must_use]
    pub fn begin(&self) -> LoadToken {
        let next = self.0.get() + 1;
        self.0.set(next);
        LoadToken(next)
    }

    /// 令牌是否仍属于当前周期。
    #[must_use]
    pub fn is_current(&self, token: LoadToken) -> bool {
        self.0.get() == token.0
    }

    /// 响应是否可应用到当前展示：令牌必须是当前周期，且仍是该面板最近一次加载发出的。
    #[must_use]
    pub fn accepts(&self, token: LoadToken, expected: Option<LoadToken>) -> bool {
        expected == Some(token) && self.is_current(token)
    }
}

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

// 面板 CSS：拼装警示必须一眼可分辨（琥珀）。
//
// 与 `results.rs` / `data_page.rs` 一样：每个模块自己装一次，用 `OnceLock`
// 保证不会重复添加（重复添加会让样式叠加，颜色越来越深）。
thread_local! {
    /// DDL 面板附加样式的 provider（切档时用同一个句柄重载）。
    static STYLE_PROVIDER: std::cell::RefCell<Option<gtk::CssProvider>> =
        const { std::cell::RefCell::new(None) };
}

const STYLESHEET: &str = r#"label.sqarvo-ddl-synth { color: @warning_color; }"#;

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
    provider.load_from_string(STYLESHEET);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    STYLE_PROVIDER.with(|slot| *slot.borrow_mut() = Some(provider));
}

/// 切档时重载 DDL 面板样式（这份样式没有尺寸，但仍与其他附加样式同一条重载路径，
/// 将来加尺寸时不会漏掉）。
pub(crate) fn reload_css() {
    STYLE_PROVIDER.with(|slot| {
        if let Some(provider) = slot.borrow().as_ref() {
            provider.load_from_string(STYLESHEET);
        }
    });
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
    /// 与信息面板共用的加载周期：过期响应不得碰控件。
    epoch: Rc<LoadEpoch>,
    /// 本面板最近一次加载的令牌。
    token: Cell<Option<LoadToken>>,
    trigger_actions: Option<TriggerDdlActions>,
}

struct TriggerDdlActions {
    copy: gtk::Button,
    retry: gtk::Button,
}

impl DdlPanel {
    /// 构建面板（与信息面板共用一份加载周期计数器）。
    pub fn new(epoch: Rc<LoadEpoch>) -> Rc<Self> {
        Self::build(epoch, false)
    }

    /// Table Info's trigger viewer has its own request lifetime and can copy raw DDL.
    pub fn for_trigger() -> Rc<Self> {
        Self::build(Rc::new(LoadEpoch::default()), true)
    }

    fn build(epoch: Rc<LoadEpoch>, trigger: bool) -> Rc<Self> {
        let warning = gtk::Label::new(None);
        warning.set_xalign(0.0);
        warning.set_wrap(true);
        warning.set_visible(false);
        warning.set_margin_top(gap(Gap::Sm));
        warning.set_margin_bottom(gap(Gap::Xxs));
        warning.set_margin_start(gap(Gap::None));
        warning.set_margin_end(gap(Gap::Lg));
        warning.add_css_class("caption");
        warning.add_css_class("sqarvo-ddl-synth");

        let text_view = gtk::TextView::new();
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        // DDL 是代码：不折行，宽语句横向滚动（与数据网格同一读法）。
        text_view.set_wrap_mode(gtk::WrapMode::None);
        text_view.set_top_margin(gap(Gap::Md));
        text_view.set_bottom_margin(gap(Gap::Md));
        // 左端贴内容区边缘：面板只有一条左基准线（与分隔线左顶点、标题同一列）。
        text_view.set_left_margin(gap(Gap::None));
        text_view.set_right_margin(gap(Gap::Lg));
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
        spinner.set_margin_top(gap(Gap::Page));
        spinner.set_margin_bottom(gap(Gap::Page));

        let state_page = adw::StatusPage::new();
        state_page.set_vexpand(true);
        state_page.set_child(Some(&spinner));

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&scroll, Some("text"));
        stack.add_named(&state_page, Some("state"));
        stack.set_visible_child_name("state");

        let root = gtk::Box::new(gtk::Orientation::Vertical, gap(Gap::None));
        let trigger_actions = trigger.then(|| {
            let copy = gtk::Button::with_label(l10n::t("inspector.copy_trigger_ddl"));
            copy.set_sensitive(false);
            let retry = gtk::Button::with_label(l10n::t("inspector.retry_trigger_ddl"));
            retry.set_visible(false);
            root.append(&copy);
            root.append(&retry);
            TriggerDdlActions { copy, retry }
        });
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
            epoch,
            token: Cell::new(None),
            trigger_actions,
        });

        if let Some(actions) = &panel.trigger_actions {
            let weak = Rc::downgrade(&panel);
            actions.copy.connect_clicked(move |button| {
                let Some(panel) = weak.upgrade() else { return };
                if panel.state.get() == DdlState::Ready
                    && panel
                        .token
                        .get()
                        .is_some_and(|token| panel.epoch.is_current(token))
                    && panel
                        .current
                        .borrow()
                        .as_ref()
                        .is_some_and(|node| panel.hub(node).is_some())
                {
                    let buffer = panel.buffer();
                    button.clipboard().set_text(&buffer.text(
                        &buffer.start_iter(),
                        &buffer.end_iter(),
                        false,
                    ));
                }
            });
            let weak = Rc::downgrade(&panel);
            actions.retry.connect_clicked(move |_| {
                let Some(panel) = weak.upgrade() else { return };
                let current = panel.current.borrow().clone();
                if panel.state.get() == DdlState::Error
                    && let Some(node) = current
                {
                    panel.show_trigger(node);
                }
            });
        }
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

    pub fn show_trigger(self: &Rc<Self>, node: NodeView) {
        self.show_for(node, self.epoch.begin());
    }

    fn hub(&self, node: &NodeView) -> Option<Rc<MetadataHub>> {
        self.hub_of
            .borrow()
            .as_ref()
            .and_then(|resolver| resolver(&node.reference))
    }

    /// 查看某个对象的 DDL（清空上一次内容并进入 Loading，必发一次 `GetDdl`）。
    ///
    /// `token` 由调用方从共用的 [`LoadEpoch`] 开启：换对象 / 换子页 / 刷新 / 断连后，
    /// 这条请求的成功与错误响应都会被 [`Self::finish`] 丢弃。
    pub fn show_for(self: &Rc<Self>, node: NodeView, token: LoadToken) {
        self.warning.set_visible(false);
        self.warning.set_text("");
        self.buffer().set_text("");
        self.current.replace(Some(node.clone()));
        self.token.replace(Some(token));
        self.set_state(
            DdlState::Loading,
            l10n::t("data_page.ddl_loading_title"),
            l10n::t("data_page.ddl_loading_desc"),
        );
        eprintln!(
            "[ddl] t=+{}ms 请求 DDL：{}{}",
            crate::qa::elapsed_ms(),
            kind_label(node.kind),
            node.path_label()
        );
        let Some(hub) = self.hub(&node) else {
            self.set_state(
                DdlState::Error,
                l10n::t("data_page.ddl_no_session_title"),
                l10n::t("data_page.ddl_no_session_desc"),
            );
            return;
        };
        let weak = Rc::downgrade(self);
        hub.request_ddl(
            node.reference.clone(),
            Rc::new(move |result| {
                if let Some(panel) = weak.upgrade() {
                    panel.finish(&node, token, result);
                }
            }),
        );
    }

    /// 清掉本次展示的数据与错误状态（断连时用；不发起请求）。
    pub fn clear(&self) {
        self.token.set(None);
        self.current.replace(None);
        self.buffer().set_text("");
        self.warning.set_text("");
        self.warning.set_visible(false);
        self.state.set(DdlState::Idle);
        self.stack.set_visible_child_name("state");
        self.state_page.set_title(" ");
        self.state_page.set_description(None);
        self.spinner.set_visible(false);
        self.spinner.stop();
        self.update_trigger_actions(DdlState::Idle);
    }

    fn finish(
        self: &Rc<Self>,
        node: &NodeView,
        token: LoadToken,
        result: Result<DdlView, MetadataError>,
    ) {
        // 有效性校验必须先于任何控件写入：迟到响应（含错误响应）不能污染当前面板。
        if !self.epoch.accepts(token, self.token.get())
            || self.current.borrow().as_ref() != Some(node)
            || (self.trigger_actions.is_some() && self.hub(node).is_none())
        {
            eprintln!(
                "[ddl] 丢弃过期响应（Token={:?}）：{}{}",
                token.0,
                kind_label(node.kind),
                node.path_label()
            );
            return;
        }
        let object = node.path_label();
        match result {
            Ok(view) => {
                if view.ddl.trim().is_empty() {
                    self.set_state(
                        DdlState::Empty,
                        l10n::t("data_page.ddl_empty_title"),
                        l10n::t("data_page.ddl_empty_desc"),
                    );
                    return;
                }
                self.buffer().set_text(&view.ddl);
                let warning = if view.exact {
                    // 原文：不再写「数据库原文（DDL_SOURCE_DATABASE）」这类说明行。
                    None
                } else {
                    // 拼装结果：必须带警示，且要能看出缺了什么、由哪版拼装器生成。
                    Some(l10n::tf(
                        "data_page.ddl_synth_warning",
                        &[("detail", view.detail_text())],
                    ))
                };
                match warning.as_deref() {
                    Some(warning) => {
                        self.warning.set_text(warning);
                        self.warning.set_visible(true);
                    }
                    None => {
                        self.warning.set_text("");
                        self.warning.set_visible(false);
                    }
                }
                self.stack.set_visible_child_name("text");
                self.state.set(DdlState::Ready);
                self.update_trigger_actions(DdlState::Ready);
                eprintln!(
                    "[ddl] t=+{}ms DDL {object}：{} 行，exact={}，source_detail={}",
                    crate::qa::elapsed_ms(),
                    view.ddl.lines().count(),
                    view.exact,
                    view.source_detail
                );
            }
            Err(error) => {
                // `SessionLost` 走与 `headline()` 同一句 catalog 条目（「数据库会话已失效」），
                // 不再在面板里重复中文字面量；Unsupported 只改状态页归类。
                let (state, title) = if error.is_unsupported()
                    || matches!(error, MetadataError::KindUnsupported { .. })
                {
                    (DdlState::Unsupported, error.headline())
                } else if error.is_session_lost() {
                    (DdlState::Error, l10n::t("data_page.err_session_lost_head"))
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
        self.update_trigger_actions(state);
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

    fn update_trigger_actions(&self, state: DdlState) {
        if let Some(actions) = &self.trigger_actions {
            actions.copy.set_sensitive(state == DdlState::Ready);
            actions.retry.set_visible(state == DdlState::Error);
        }
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

    #[test]
    fn a_new_load_invalidates_every_earlier_token_even_for_the_same_object() {
        let epoch = LoadEpoch::default();
        let first = epoch.begin();
        assert!(epoch.accepts(first, Some(first)));
        // 同一次用户操作里的重复事件没有新令牌，不会被误当成新批次。
        assert!(epoch.accepts(first, Some(first)));
        let second = epoch.begin();
        assert!(!epoch.is_current(first));
        assert!(epoch.accepts(second, Some(second)));
        // A → B → A：第三批的令牌与第一批不同，旧响应一律丢弃。
        let third = epoch.begin();
        assert!(!epoch.accepts(first, Some(first)));
        assert!(!epoch.accepts(second, Some(second)));
        assert!(epoch.accepts(third, Some(third)));
    }

    #[test]
    fn a_token_from_another_panel_or_a_cleared_panel_is_rejected() {
        let epoch = LoadEpoch::default();
        let token = epoch.begin();
        assert!(!epoch.accepts(token, None), "clear 之后没有在途加载");
        let other = epoch.begin();
        assert!(!epoch.accepts(token, Some(token)));
        assert!(epoch.accepts(other, Some(other)));
    }
}
