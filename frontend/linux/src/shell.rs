//! 六区域工作区外壳（`docs/ui-specification/workspace-model.md` §2，ADR 0023）。
//!
//! 本模块只负责**区域与槽位**：谁在哪里、多宽多高、怎么显隐。内容仍由各页面
//! （对象树 [`crate::tree`]、SQL 工作区 [`crate::sql_page`]、数据页 [`crate::data_page`]、
//! 信息面板 [`crate::inspector`]）自己负责。#114（多连接）、#116（标签工作区）与
//! #119（信息面板）都已落在各自的槽位里。
//!
//! # 槽位一览
//!
//! ```text
//! 01 窗口外壳   AdwHeaderBar + AdwWindowTitle（在 ui.rs 里，含标题规则与主菜单）
//! ┌──────────────────────────────────────────────────────────────────────────┐
//! 02 主工具条   Shell::toolbar   高 76：左组 连接 / 新建查询 │ 右组 种类高亮
//! ├────────────────┬────────────────────────────────────────────────────────┤
//! 03 对象树       │ 04 工作区                                              │ 06 信息面板
//!    side_pane     │    work_pane（起始子控件）                              │    inspector
//!    左栏 237      │    标签条 24 → 标签页（表格工具条 → 数据网格）           │    Revealer
//!    可拖 180–420  │                                                        │    280（240–520）
//! ├────────────────┴────────────────────────────────────────────────────────┤
//! 05 状态栏     Shell::status    高 56：行1 连接过滤 │ 行操作 │ 分页器
//!                                行2 SQL 摘要 │ 记录位置 │ 最后刷新 · 连接·事务·耗时
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! | 区域 | 字段 | 后续扩展 |
//! |---|---|---|
//! | 01 | 无（`ui.rs` 的 `AdwHeaderBar`） | 标题跟随当前连接（#114 已接连接注册表） |
//! | 02 | `Shell::toolbar`（[`Toolbar`]） | #116 已把「新建查询」接到标签 |
//! | 03 | `Shell::sidebar_pane` 起始子控件 | #114 已实现：多连接树（仍是左栏） |
//! | 04 | `Shell::work_pane` 起始子控件 | #116 标签条 + [`crate::tabs::TabSet`] |
//! | 05 | `Shell::status`（[`StatusBar`]） | #118 分页器接真实页码 |
//! | 06 | `Shell::inspector_box` | #119 已填入连接 / 库 / 表内容（常驻、宽度窗口级记忆） |
//!
//! # 不做什么
//!
//! - 不自绘窗口控制按钮，不设窗口内菜单行（ADR 0023：Linux 用 headerbar 主菜单）；
//! - 不自绘标签条：#116 用 `AdwTabView` + `AdwTabBar`，外壳只给它位置；
//! - 不实现信息面板内容（#119）：外壳只给位置与显隐，内容在 [`crate::inspector`]；
//! - 状态栏不放「对象树 / 平铺」切换：960×640 下右组会被窗口右边缘裁掉（半个「平铺」），
//!   该动作原本就在侧栏页脚（#113 之前的位置），这里不再重复一份；
//! - 工具条与菜单里没有实现支撑的按钮/项**禁用并说明原因**，不摆占位动作。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use gtk::glib;

use crate::browse_view::relative_age;
use crate::data_page::{DataState, PagerAction, RowOp, RowOpsState, StatusSnapshot};
use crate::layout;

/// 状态栏「连接过滤」的提示：过滤未实现，树已按连接分组。
const CONNECTION_FILTER_NOTE: &str = "连接过滤未实现；对象树已按连接分组（#114）";

/// 分页器的提示：offset 分页，页码只读，只提供首页/上页/下页/末页。
const PAGER_NOTE: &str =
    "offset 分页（#118）：首页 / 上页 / 下页 / 末页；页码只读，跳转到远处页码不在本版提供";

/// 外壳需要的回调。
pub struct ShellHandlers {
    /// 左栏内容（对象树 / 平铺列表 + 页脚）。
    pub sidebar: gtk::Widget,
    /// 工作区内容（SQL / 数据页栈）。
    pub work_area: gtk::Widget,
    /// 信息面板内容（#119：连接 / 库 / 表三态检查器）。
    pub inspector: gtk::Widget,
    /// 信息面板的初始宽度（窗口级记忆，从窗口状态文件读回，已钳制）。
    pub inspector_width: i32,
    /// 面板宽度变化（拖拽后）→ 调用方写入窗口状态文件（窗口级记忆）。
    pub on_inspector_width: Rc<dyn Fn(i32)>,
    /// 行操作（状态栏按钮组）。
    pub row_op: Rc<dyn Fn(RowOp)>,
    /// 分页器动作（首页 / 上页 / 下页 / 末页）。
    pub page_op: Rc<dyn Fn(PagerAction)>,
    /// 左组「连接」。
    pub connect: Rc<dyn Fn()>,
    /// 左组「新建查询」。
    pub new_query: Rc<dyn Fn()>,
    /// 主工具条的种类入口（表 / 视图 / 函数 / 查询）：打开 `Objects` 页并列出该种类。
    pub on_kind: Rc<dyn Fn(crate::tabs::KindShortcut)>,
}

/// 窗口内的外壳（区域 02–06）。
pub struct Shell {
    root: gtk::Box,
    toolbar: Rc<Toolbar>,
    sidebar_pane: gtk::Paned,
    sidebar: gtk::Widget,
    work_pane: gtk::Paned,
    work_area: gtk::Widget,
    status: Rc<StatusBar>,
    inspector: gtk::Revealer,
    inspector_box: gtk::Box,
    /// 信息面板当前宽度（拖拽后记住，折叠再展开时恢复）。
    inspector_width: Rc<Cell<i32>>,
    /// 信息面板是否展开。
    inspector_visible: Rc<Cell<bool>>,
    /// 正在程序化分栏（展开 / 折叠 / 恢复宽度）：这期间的分栏位置变化不是用户拖拽，
    /// 不能当成新的宽度记下来。
    inspector_placing: Rc<Cell<bool>>,
    /// 用户把面板拖到新宽度 → 窗口级记住（写独立的窗口状态文件）。
    on_inspector_width: Rc<dyn Fn(i32)>,
}

impl Shell {
    /// 组装五个区域（区域 01 的 headerbar 由 `ui.rs` 提供，不在本模块内）。
    pub fn new(handlers: ShellHandlers) -> Rc<Self> {
        let toolbar = Toolbar::new(
            Rc::clone(&handlers.connect),
            Rc::clone(&handlers.new_query),
            Rc::clone(&handlers.on_kind),
        );
        let status = StatusBar::new(Rc::clone(&handlers.row_op), Rc::clone(&handlers.page_op));
        {
            // 相对时间（<1m / Nm）不会自己变：每 30 秒重算一次标签文本。
            let weak = Rc::downgrade(&status);
            glib::timeout_add_seconds_local(30, move || {
                if let Some(status) = weak.upgrade() {
                    status.tick_relative_time();
                }
                glib::ControlFlow::Continue
            });
        }

        // ---- 区域 06：信息面板（#119 提供连接 / 库 / 表内容） -----------------
        let inspector_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        inspector_box.add_css_class("sqarvo-inspector");
        inspector_box.append(&handlers.inspector);

        let inspector = gtk::Revealer::new();
        inspector.set_transition_type(gtk::RevealerTransitionType::SlideLeft);
        // v0.2.3.2：信息面板常驻（不再有折叠入口），宽度仍可拖、窗口级记忆。
        inspector.set_reveal_child(true);
        inspector.set_child(Some(&inspector_box));

        // ---- 区域 04 | 06：工作区与信息面板共用一个 GtkPaned --------------------
        let work_pane = gtk::Paned::new(gtk::Orientation::Horizontal);
        work_pane.set_start_child(Some(&handlers.work_area));
        work_pane.set_end_child(Some(&inspector));
        work_pane.set_resize_start_child(true);
        work_pane.set_shrink_start_child(false);
        work_pane.set_resize_end_child(false);
        work_pane.set_shrink_end_child(false);
        work_pane.set_wide_handle(false);

        // ---- 区域 03 | 04：侧栏与右侧工作区 ------------------------------------
        let sidebar_pane = gtk::Paned::new(gtk::Orientation::Horizontal);
        sidebar_pane.set_start_child(Some(&handlers.sidebar));
        sidebar_pane.set_end_child(Some(&work_pane));
        sidebar_pane.set_position(layout::SIDEBAR_WIDTH);
        sidebar_pane.set_resize_start_child(false);
        sidebar_pane.set_shrink_start_child(false);
        sidebar_pane.set_resize_end_child(true);
        sidebar_pane.set_shrink_end_child(false);
        sidebar_pane.set_wide_handle(false);

        // 拖拽钳制：把位置拉回 180–420，避免拖成 0 宽或吃掉整个工作区。
        {
            let guard = Cell::new(false);
            sidebar_pane.connect_notify_local(Some("position"), move |pane, _| {
                if guard.get() {
                    return;
                }
                let clamped = layout::clamp_sidebar(pane.position());
                if clamped != pane.position() {
                    guard.set(true);
                    pane.set_position(clamped);
                    guard.set(false);
                }
            });
        }

        let inspector_width = Rc::new(Cell::new(layout::clamp_inspector(handlers.inspector_width)));
        // 面板常驻：初始化即 true，`set_inspector_visible(false)` 不再有调用者。
        let inspector_visible = Rc::new(Cell::new(true));
        // 首帧的分栏不是用户拖的：从构造起就认作「正在放置」，直到第一次落位后
        // 稳定一段时间。否则首帧的 240（分栏 clamp 下限）会被当成用户宽度记进
        // window.json，默认 280 从此永远回不来。
        let inspector_placing = Rc::new(Cell::new(true));
        {
            let guard = Cell::new(false);
            let width_cell = Rc::clone(&inspector_width);
            let visible = Rc::clone(&inspector_visible);
            let placing = Rc::clone(&inspector_placing);
            // 拖拽是逐像素的：宽度先记在内存里，停手 400ms 后再落盘（窗口级记忆）。
            let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
            let pending_on_fire = Rc::clone(&pending);
            let on_width = Rc::clone(&handlers.on_inspector_width);
            work_pane.connect_notify_local(Some("position"), move |pane, _| {
                if guard.get() || !visible.get() || placing.get() {
                    return;
                }
                let pane_width = pane.width();
                if pane_width <= 0 {
                    return;
                }
                let clamped = layout::inspector_width_from_position(pane_width, pane.position());
                if clamped != width_cell.get() {
                    width_cell.set(clamped);
                }
                let position = pane_width - clamped - 1;
                if position != pane.position() {
                    guard.set(true);
                    pane.set_position(position);
                    guard.set(false);
                }
                if let Some(source) = pending.borrow_mut().take() {
                    source.remove();
                }
                let on_width = Rc::clone(&on_width);
                let width = clamped;
                let pending_for_timer = Rc::clone(&pending_on_fire);
                *pending.borrow_mut() = Some(glib::timeout_add_local_once(
                    std::time::Duration::from_millis(400),
                    move || {
                        // 定时器已经触发：先把登记清掉，否则下一次 position 变化会对一个
                        // 已经消耗掉的 SourceId 调 remove()，GLib 直接 abort（QA 链上实测）。
                        pending_for_timer.borrow_mut().take();
                        on_width(width);
                    },
                ));
            });
        }

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root.append(toolbar.widget());
        root.append(&sidebar_pane);
        root.append(status.widget());

        let shell = Rc::new(Self {
            root,
            toolbar,
            sidebar_pane,
            sidebar: handlers.sidebar,
            work_pane,
            work_area: handlers.work_area,
            status,
            inspector,
            inspector_box,
            inspector_width,
            inspector_visible,
            inspector_placing,
            on_inspector_width: Rc::clone(&handlers.on_inspector_width),
        });
        {
            // 首次拿到尺寸时把布局算式的实际结果写进日志：截图之外还有可复核的数字。
            // `map` 的时刻分配还没发生（宽高都是 0），所以推迟一拍再取尺寸。
            let weak = Rc::downgrade(&shell);
            shell.root.connect_map(move |_| {
                let weak = weak.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(0), move || {
                    if let Some(shell) = weak.upgrade() {
                        // 面板常驻：首次映射后按记忆宽度把分栏落位一次（宽度未就绪时
                        // 短轮询，不能把 0 宽算成一次落位）。
                        shell.place_inspector_when_ready(0);
                        let weak = Rc::downgrade(&shell);
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(400),
                            move || {
                                if let Some(shell) = weak.upgrade() {
                                    shell.log_layout();
                                }
                            },
                        );
                    }
                });
            });
        }
        shell
    }

    /// 布局算术自检（§2.1）：按**真实窗口尺寸**打印规范公式算出的结果。
    ///
    /// 取的是 `GtkRoot` 的宽高（含 headerbar），不是外壳根控件的分配高度——
    /// 后者从 headerbar 之下开始，直接套公式会把外壳算少一次。
    fn log_layout(&self) {
        let window = self
            .root
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());
        let (width, window_height) = match window {
            Some(window) if window.width() > 0 && window.height() > 0 => {
                (window.width(), window.height())
            }
            _ => (
                self.root.width(),
                self.root.height() + layout::HEADER_HEIGHT,
            ),
        };
        if width <= 0 || window_height <= 0 {
            return;
        }
        let sidebar = layout::clamp_sidebar(self.sidebar_pane.position());
        let inspector = if self.inspector_visible.get() {
            Some(layout::clamp_inspector(self.inspector_width.get()))
        } else {
            None
        };
        let inspector_label =
            inspector.map_or_else(|| "折叠".to_owned(), |value| value.to_string());
        eprintln!(
            "[shell] 布局 {width}x{window_height}：工具条 {}，状态栏 {}（y={}），内容 {}x{}，表格 {}x{}，侧栏 {sidebar}，信息面板 {inspector_label}",
            self.toolbar.root.height(),
            self.status.root.height(),
            layout::status_y(window_height),
            layout::content_width(width, sidebar),
            layout::content_height(window_height),
            layout::grid_width(width, sidebar, inspector),
            layout::grid_height(window_height),
        );
    }

    /// 外壳根控件（区域 02–06 的容器）。
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 区域 02：主工具条。
    pub fn toolbar(&self) -> &Rc<Toolbar> {
        &self.toolbar
    }

    /// 区域 03：侧栏（对象树 / 平铺 + 页脚）。
    pub fn sidebar(&self) -> &gtk::Widget {
        &self.sidebar
    }

    /// 区域 05：状态栏。
    pub fn status(&self) -> &Rc<StatusBar> {
        &self.status
    }

    /// 折叠 / 展开信息面板，返回新的可见性。
    ///
    /// 折叠时把焦点交回工作区：焦点不能留在不可见控件上（§11）。
    ///
    /// **折叠必须把宽度还回去**：只把 revealer 收起而让分栏位置停在原处，表格仍然
    /// 少 280px（`workspace-model` §2.1 的「折叠 942」，比 652 多 290）。因此折叠时
    /// 把分隔条推到最右（起始子控件占满），展开时再恢复到记忆的宽度。
    pub fn set_inspector_visible(&self, visible: bool) -> bool {
        if visible == self.inspector_visible.get() {
            // 常驻面板：重复「展开」只需确保分栏位置正确（首次映射后尺寸才有效）。
            if visible {
                self.place_inspector();
            }
            return visible;
        }
        self.inspector_visible.set(visible);
        // 展开 / 折叠都先关掉「宽度采样」：这一拍的位置由下面这次分栏决定，
        // 不是用户在拖。
        self.inspector_placing.set(true);
        if visible {
            // 先显示内容再滑出：否则动画期间是一块空白。
            self.inspector_box.set_visible(true);
        }
        self.inspector.set_reveal_child(visible);
        if !visible {
            // 动画结束后把面板内容整个隐藏：`GtkRevealer` 收起时仍会报子控件的最小
            // 宽度（实测 198px），分栏会把这 198px 继续留给面板——折叠就不算「不占宽」。
            let box_ = self.inspector_box.clone();
            let pane = self.work_pane.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(260), move || {
                box_.set_visible(false);
                // 最小宽度变成 0 之后再分一次栏：否则位置还停在「面板 min = 198」那次
                // 钳制的结果上，表格仍然少 198px。
                let pane_width = pane.width();
                if pane_width > 0 {
                    pane.set_position(pane_width);
                }
            });
        }
        eprintln!(
            "[shell] 信息面板：{}（宽 {}）",
            if visible { "展开" } else { "折叠" },
            self.inspector_width.get()
        );
        let width = layout::clamp_inspector(self.inspector_width.get());
        let pane = self.work_pane.clone();
        let place = Rc::new(move || {
            let pane_width = pane.width();
            if pane_width <= 0 {
                return;
            }
            let position = if visible {
                pane_width - width - 1
            } else {
                // 全部给工作区：面板不占宽（间隙一并收回）。
                pane_width
            };
            pane.set_position(position);
        });
        // 已分配尺寸时立即分栏；首次展开（宽度还是 0）时等一次布局再做。
        place();
        let later = Rc::clone(&place);
        glib::timeout_add_local_once(std::time::Duration::from_millis(0), move || later());
        {
            let placing = Rc::clone(&self.inspector_placing);
            let pane = self.work_pane.clone();
            let remembered = self.inspector_width.get();
            glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                placing.set(false);
                // 折叠 / 展开之后表格实际拿到多少宽：截图之外还有可复核的数字。
                let pane_width = pane.width();
                let allocated = if visible && pane_width > 0 {
                    (pane_width - pane.position() - 1).max(0)
                } else {
                    0
                };
                // 起始子控件的**分配**宽度才是表格真正拿到的宽度（分栏位置只是请求值：
                // 折叠时 GTK 还会按面板的最小宽度再钳一次）。折叠 / 展开相差 280。
                let work_width = pane.start_child().map_or(0, |child| child.width());
                let (min_start, _, _, _) = pane.start_child().map_or((0, 0, 0, 0), |child| {
                    child.measure(gtk::Orientation::Horizontal, -1)
                });
                eprintln!(
                    "[shell] 面板{}：工作区宽 {pane_width}，表格区实宽 {work_width}，面板占 {allocated}（记忆宽 {remembered}；起始 min {min_start}）",
                    if visible { "展开" } else { "折叠" },
                );
                // 再取一次：revealer 的滑动动画约 200ms，动画结束后才是最终宽度。
                let pane = pane.clone();
                let label = if visible { "展开" } else { "折叠" };
                glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    let work_width = pane.start_child().map_or(0, |child| child.width());
                    eprintln!("[shell] 面板{label}（动画后）：表格区实宽 {work_width}");
                });
            });
        }
        if !visible
            && let Some(focus) = self.root.root().and_then(|root| root.focus())
            && focus.is_ancestor(&self.inspector_box)
        {
            self.work_area.grab_focus();
        }
        visible
    }

    /// 把分栏放到「右侧留出记忆宽度」的位置（首次映射与宽度变化都走这里）。
    ///
    /// 分栏还没有真实宽度时（首帧）直接返回，由 [`Self::place_inspector_when_ready`]
    /// 短轮询到就绪再落位：否则 `pane_width - width` 会算成负数、被 clamp 到 240，
    /// 再把 240 当成用户选择记进 `window.json`。
    /// 落位后 600ms 才允许「拖拽采样」生效（记录防抖是 400ms）。
    pub fn place_inspector(&self) {
        let pane = &self.work_pane;
        let pane_width = pane.width();
        if pane_width <= 0 {
            // 还没有分配尺寸：保持「正在放置」，等 map / 重试回调来落位。
            self.inspector_placing.set(true);
            return;
        }
        let width = layout::clamp_inspector(self.inspector_width.get());
        self.inspector_placing.set(true);
        pane.set_position(pane_width - width - 1);
        let placing = Rc::clone(&self.inspector_placing);
        glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
            placing.set(false);
        });
    }

    /// 等分栏拿到真实宽度后再落位（首帧尺寸是 0；最多轮询 ~1 秒）。
    pub fn place_inspector_when_ready(self: &Rc<Self>, attempts: u32) {
        if self.work_pane.width() > 0 {
            self.place_inspector();
            return;
        }
        if attempts >= 25 {
            eprintln!("[shell] 分栏宽度一直为 0：信息面板未落位（宽度保持记忆值）");
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(40), move || {
            if let Some(shell) = weak.upgrade() {
                shell.place_inspector_when_ready(attempts + 1);
            }
        });
    }

    /// 信息面板当前是否展开（布局自检与 QA 用）。
    pub fn inspector_visible(&self) -> bool {
        self.inspector_visible.get()
    }

    /// 信息面板当前记忆的宽度（折叠时仍然保留，展开时用它）。
    pub fn inspector_width(&self) -> i32 {
        self.inspector_width.get()
    }

    /// 把面板设到指定宽度（拖拽结束后的落点；`--qa-inspector-width` 也走这里）。
    ///
    /// 钳制到 240–520，展开状态下立刻重新分栏，并窗口级记下来。
    pub fn set_inspector_width(&self, width: i32) {
        let width = layout::clamp_inspector(width);
        if width == self.inspector_width.get() {
            return;
        }
        self.inspector_width.set(width);
        self.inspector_placing.set(true);
        let pane = self.work_pane.clone();
        if pane.width() > 0 {
            pane.set_position(pane.width() - width - 1);
        }
        let placing = Rc::clone(&self.inspector_placing);
        glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            placing.set(false);
        });
        (self.on_inspector_width)(width);
        eprintln!("[shell] 信息面板宽度改为 {width}（已窗口级记忆）");
    }

    /// 显隐对象树侧栏，返回新的可见性。
    pub fn set_sidebar_visible(&self, visible: bool) -> bool {
        self.sidebar.set_visible(visible);
        if visible {
            self.sidebar_pane.set_position(layout::SIDEBAR_WIDTH);
        }
        visible
    }

    /// 切换对象树侧栏，返回新的可见性。
    pub fn toggle_sidebar(&self) -> bool {
        let next = !self.sidebar.is_visible();
        self.set_sidebar_visible(next)
    }
}

/// 区域 02：主工具条。
///
/// 左组是真实动作（连接 / 新建查询）；分割线右侧是**种类入口**（表 / 视图 / 函数 / 查询），
/// 与「新建查询」同一套按钮样式（flat、同高同内距）。点一下切到 `Objects` 页并列出该种类
/// （v0.2.3.3：等同在对象树里选中对应的库 / 分组）。函数 / 查询没有可拉取的列表，点开后
/// 页面如实说明原因，不摆假数据。
pub struct Toolbar {
    root: gtk::Box,
    left: gtk::Box,
    kinds: gtk::Box,
    new_query: gtk::Button,
    table: gtk::Button,
    view: gtk::Button,
    function: gtk::Button,
    query: gtk::Button,
}

impl Toolbar {
    fn new(
        on_connect: Rc<dyn Fn()>,
        on_new_query: Rc<dyn Fn()>,
        on_kind: Rc<dyn Fn(crate::tabs::KindShortcut)>,
    ) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.add_css_class("sqarvo-toolbar");
        root.set_valign(gtk::Align::Center);

        let connect = gtk::Button::with_label("连接");
        connect.add_css_class("suggested-action");
        connect.set_valign(gtk::Align::Center);
        connect.set_tooltip_text(Some("打开连接对话框（Ctrl+N）"));
        {
            let handler = Rc::clone(&on_connect);
            connect.connect_clicked(move |_| handler());
        }

        let new_query = gtk::Button::with_label("新建查询");
        new_query.add_css_class("flat");
        new_query.set_valign(gtk::Align::Center);
        new_query.set_tooltip_text(Some("新建查询标签（Ctrl+T）"));
        {
            let handler = Rc::clone(&on_new_query);
            new_query.connect_clicked(move |_| handler());
        }

        let left = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        left.set_valign(gtk::Align::Center);
        left.append(&connect);
        left.append(&new_query);

        let separator = gtk::Separator::new(gtk::Orientation::Vertical);
        separator.set_size_request(-1, 28);
        separator.set_valign(gtk::Align::Center);
        separator.set_margin_start(6);
        separator.set_margin_end(6);

        // 种类入口：与「新建查询」同一套按钮（flat / 同高同内距），只是动作不同。
        let kind =
            |label: &str, shortcut: crate::tabs::KindShortcut, tooltip: &str| -> gtk::Button {
                let button = gtk::Button::with_label(label);
                button.add_css_class("flat");
                button.set_valign(gtk::Align::Center);
                button.set_tooltip_text(Some(tooltip));
                let handler = Rc::clone(&on_kind);
                button.connect_clicked(move |_| handler(shortcut));
                button
            };
        let table = kind(
            "表",
            crate::tabs::KindShortcut::Tables,
            "在 Objects 页列出当前库的表（等同在对象树里选中该库）",
        );
        let view = kind(
            "视图",
            crate::tabs::KindShortcut::Views,
            "在 Objects 页列出当前库的视图（等同在对象树里选中 Views 分组）",
        );
        let function = kind(
            "函数",
            crate::tabs::KindShortcut::Functions,
            "函数列表：驱动尚未提供（点开看说明）",
        );
        let query = kind(
            "查询",
            crate::tabs::KindShortcut::Queries,
            "已保存的查询：尚未实现（点开看说明）",
        );

        let kinds = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        kinds.set_valign(gtk::Align::Center);
        kinds.append(&table);
        kinds.append(&view);
        kinds.append(&function);
        kinds.append(&query);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);

        root.append(&left);
        root.append(&separator);
        root.append(&kinds);
        root.append(&spacer);

        Rc::new(Self {
            root,
            left,
            kinds,
            new_query,
            table,
            view,
            function,
            query,
        })
    }

    /// 「新建查询」的可用性：未连接数据库时禁用并说明原因（不摆点了没反应的按钮）。
    pub fn set_new_query_enabled(&self, enabled: bool) {
        self.new_query.set_sensitive(enabled);
        self.new_query.set_tooltip_text(Some(if enabled {
            "新建查询标签（Ctrl+T）"
        } else {
            "尚未连接数据库：先连接再新建查询标签"
        }));
    }

    fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 左组（连接 / 新建查询）：布局自检按组比较需求宽度与分配宽度。
    pub fn left_group(&self) -> &gtk::Box {
        &self.left
    }

    /// 种类入口组（表 / 视图 / 函数 / 查询）：同上。
    pub fn kinds_group(&self) -> &gtk::Box {
        &self.kinds
    }

    /// 高亮「当前种类」：`Objects` 页列的种类，或当前表标签的种类。
    pub fn set_active(&self, shortcut: Option<crate::tabs::KindShortcut>) {
        use crate::tabs::KindShortcut;
        for (button, choice) in [
            (&self.table, KindShortcut::Tables),
            (&self.view, KindShortcut::Views),
            (&self.function, KindShortcut::Functions),
            (&self.query, KindShortcut::Queries),
        ] {
            if Some(choice) == shortcut {
                button.add_css_class("sqarvo-kind-active");
            } else {
                button.remove_css_class("sqarvo-kind-active");
            }
        }
    }

    /// QA：点一下某个种类入口（`emit_clicked`，与用户点击同一条信号）。
    pub fn qa_click(&self, shortcut: crate::tabs::KindShortcut) {
        use crate::tabs::KindShortcut;
        let button = match shortcut {
            KindShortcut::Tables => &self.table,
            KindShortcut::Views => &self.view,
            KindShortcut::Functions => &self.function,
            KindShortcut::Queries => &self.query,
        };
        button.emit_clicked();
    }
}

/// 区域 05：状态栏（两行 56）。
///
/// 三段归属（§7）：
///
/// ```text
/// 行1：[连接过滤] │ [新增 删除 应用 撤销 刷新 取消]                 [分页器]
/// 行2：[SQL 摘要]              [已加载 N 行 · 第 P 页]  [最后刷新] [连接·事务·耗时]
/// ```
///
/// 视图切换（对象树 / 平铺）**不在状态栏**：它在侧栏页脚（#113 之前的位置），
/// 960×640 下放两处会把右组顶出窗口。这里只保留分页器。
///
/// 读数全部来自数据页 / 查询事件：拿不到的（总数、页码跳转）不出现，不写 `—` 冒充。
pub struct StatusBar {
    root: gtk::Box,
    add: gtk::Button,
    delete: gtk::Button,
    apply: gtk::Button,
    discard: gtk::Button,
    refresh: gtk::Button,
    cancel: gtk::Button,
    pager_summary: gtk::Label,
    /// 分页器按钮（首页 / 上页 / 下页 / 末页），顺序与 `PagerAction` 对应。
    pager_buttons: RefCell<Vec<gtk::Button>>,
    /// 选中行数（`N selected`）。
    selected_label: gtk::Label,
    /// 最后刷新时刻（相对时间由定时器重算）。
    last_refresh_at: Cell<Option<Instant>>,
    row_one: gtk::Box,
    row_one_left: gtk::Box,
    row_one_middle: gtk::Box,
    row_one_right: gtk::Box,
    row_two: gtk::Box,
    row_two_left: gtk::Box,
    row_two_middle: gtk::Box,
    row_two_right: gtk::Box,
    sql_summary: gtk::Label,
    record: gtk::Label,
    last_refresh: gtk::Label,
    session: gtk::Label,
    connection: RefCell<Option<String>>,
    /// 当前表标签所属的库（`对象 @ 库` 的库）；没有表标签 / 查询标签时为空。
    catalog: RefCell<Option<String>>,
    /// 启动态补充信息（驱动数 / 浏览会话数）；连接数据库后清空。
    idle_summary: RefCell<Option<String>>,
    txn: RefCell<String>,
    timing_ms: Cell<Option<u64>>,
}

impl StatusBar {
    fn new(on_row_op: Rc<dyn Fn(RowOp)>, on_page: Rc<dyn Fn(PagerAction)>) -> Rc<Self> {
        // ---- 行 1 ------------------------------------------------------------
        let filter = gtk::SearchEntry::new();
        filter.set_placeholder_text(Some("连接过滤"));
        filter.set_width_chars(14);
        filter.set_sensitive(false);
        filter.set_tooltip_text(Some(CONNECTION_FILTER_NOTE));
        filter.set_valign(gtk::Align::Center);

        let ops_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        ops_row.add_css_class("linked");
        ops_row.set_valign(gtk::Align::Center);

        // v0.2.3.3：行操作只用图标 + tooltip（同一段文字同时登记为可访问名）。
        // 六个动作语义固定：新增 / 删除 / 应用 / 撤销 / 刷新 / 取消。图标尺寸不再
        // 逐个「看着调」，而是按 `crate::icons::OpticalWeight` 的三档光学标准取
        // （满盒 / 单笔画 / 环形），命中区统一。
        let make_op =
            |icon: &str, weight: crate::icons::OpticalWeight, tooltip: &str, op: RowOp| {
                let button = crate::icons::button(icon, weight, tooltip);
                button.set_sensitive(false);
                let handler = Rc::clone(&on_row_op);
                button.connect_clicked(move |_| handler(op));
                ops_row.append(&button);
                button
            };
        let add = make_op(
            "list-add-symbolic",
            crate::icons::OpticalWeight::Box,
            "新增行：在当前表末尾新增一行",
            RowOp::Add,
        );
        let delete = make_op(
            "list-remove-symbolic",
            // 单笔画：横杠墨迹比 `+` 少，按标准放大一档才与基准等重。
            crate::icons::OpticalWeight::Stroke,
            "删除选中行：标记为待删除（提交前可撤销）",
            RowOp::Delete,
        );
        let apply = make_op(
            "object-select-symbolic",
            crate::icons::OpticalWeight::Box,
            "应用：提交所有脏行的变更",
            RowOp::Apply,
        );
        let discard = make_op(
            "edit-undo-symbolic",
            // 单笔画：撤销弧线比满盒字形窄，同样放大一档。
            crate::icons::OpticalWeight::Stroke,
            "撤销：放弃全部未提交的编辑与删除标记",
            RowOp::Discard,
        );
        let refresh = make_op(
            "view-refresh-symbolic",
            // 环形：外框接近整盒，缩一档才不会比 `+` 显大。
            crate::icons::OpticalWeight::Ring,
            "刷新：重新读取当前表的第一页",
            RowOp::Refresh,
        );
        let cancel = make_op(
            // 「取消」用实心方块（原本的 `process-stop-symbolic` 在 Yaru 是八边形、
            // 在 Adwaita 是叉号，两套主题两种样子，也不是「停止」的通用形态）。
            "media-playback-stop-symbolic",
            crate::icons::OpticalWeight::Box,
            "取消：中止正在执行的提交批次（已执行的行会整批回滚）",
            RowOp::Cancel,
        );

        let ops_separator = gtk::Separator::new(gtk::Orientation::Vertical);
        ops_separator.set_size_request(-1, 18);
        ops_separator.set_valign(gtk::Align::Center);
        ops_separator.add_css_class("sqarvo-status-sep");

        // 分页器（#118）：offset 分页 + 页码只读显示。按钮把动作交给当前数据页，
        // 由数据页决定目标 offset（并在有未提交更改时拒绝）。
        let pager_summary = gtk::Label::new(Some("未打开表"));
        pager_summary.add_css_class("dim-label");
        pager_summary.set_valign(gtk::Align::Center);
        let pager = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pager.add_css_class("linked");
        pager.set_valign(gtk::Align::Center);
        let mut pager_buttons: Vec<gtk::Button> = Vec::new();
        for (icon, action, tip) in [
            (
                "go-first-symbolic",
                PagerAction::First,
                "首页（offset = 0）",
            ),
            ("go-previous-symbolic", PagerAction::Previous, "上一页"),
            ("go-next-symbolic", PagerAction::Next, "下一页"),
            ("go-last-symbolic", PagerAction::Last, "末页（需要总数）"),
        ] {
            let button = gtk::Button::from_icon_name(icon);
            button.add_css_class("flat");
            button.set_sensitive(false);
            button.set_tooltip_text(Some(tip));
            let pager_handler = Rc::clone(&on_page);
            button.connect_clicked(move |_| pager_handler(action));
            pager.append(&button);
            pager_buttons.push(button);
        }
        let pager_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        pager_box.set_valign(gtk::Align::Center);
        pager_box.append(&pager);
        pager_box.append(&pager_summary);
        pager_box.set_tooltip_text(Some(PAGER_NOTE));

        // 「N selected」：多选时必须能一眼看到选了几行（状态栏右）。
        let selected_label = gtk::Label::new(None);
        selected_label.add_css_class("dim-label");
        selected_label.set_valign(gtk::Align::Center);
        selected_label.set_visible(false);

        // 行 1 分三组：连接过滤 │ 行操作 │ 选中数 + 分页器。
        //
        // 「对象树 / 平铺」切换**不放这里**：960×640 下它会把右组顶出窗口右边缘
        // （「平铺」只剩半个字）。该动作在侧栏页脚（#113 之前的位置）已有入口，
        // 这里不重复；状态栏只保留分页器。
        let row_one_left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row_one_left.append(&filter);

        let row_one_middle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row_one_middle.append(&ops_separator);
        row_one_middle.append(&ops_row);

        let row_one_right = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row_one_right.append(&selected_label);
        row_one_right.append(&pager_box);

        let row_one = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row_one.add_css_class("sqarvo-status-row");
        row_one.append(&row_one_left);
        row_one.append(&row_one_middle);
        let spacer_one = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer_one.set_hexpand(true);
        row_one.append(&spacer_one);
        row_one.append(&row_one_right);

        // ---- 行 2 ------------------------------------------------------------
        let sql_summary = gtk::Label::new(Some("SQL：尚未执行"));
        sql_summary.set_xalign(0.0);
        sql_summary.set_hexpand(true);
        sql_summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        sql_summary.set_valign(gtk::Align::Center);
        sql_summary.add_css_class("dim-label");

        // 右段三个标签都必须能压缩：960 宽加上长连接名时宁可省略，也不能溢出窗口。
        let record = gtk::Label::new(Some("未打开表"));
        record.set_valign(gtk::Align::Center);
        record.set_ellipsize(gtk::pango::EllipsizeMode::End);
        record.set_max_width_chars(46);
        record.add_css_class("dim-label");

        let last_refresh = gtk::Label::new(None);
        last_refresh.set_valign(gtk::Align::Center);
        last_refresh.add_css_class("dim-label");
        last_refresh.set_visible(false);

        // 刷新入口就放在「Last Refresh」旁边（ADR 0022 §4：offset 分页不保证跨页稳定）。
        let refresh_now = crate::icons::button(
            "view-refresh-symbolic",
            crate::icons::OpticalWeight::Ring,
            "刷新当前表（保持排序/筛选，回到第 1 页）",
        );
        refresh_now.set_valign(gtk::Align::Center);
        refresh_now.set_sensitive(false);
        {
            let handler = Rc::clone(&on_row_op);
            refresh_now.connect_clicked(move |_| handler(RowOp::Refresh));
        }

        let session = gtk::Label::new(None);
        session.set_valign(gtk::Align::Center);
        session.set_ellipsize(gtk::pango::EllipsizeMode::End);
        session.set_max_width_chars(48);
        // 窗口变窄时这一格最先被压：留出右边距，别让最后一个字贴着窗口边。
        session.set_margin_end(6);
        session.add_css_class("dim-label");
        session.set_visible(false);

        let row_two_left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row_two_left.set_hexpand(true);
        row_two_left.append(&sql_summary);

        let row_two_middle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row_two_middle.append(&record);

        let row_two_right = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row_two_right.append(&last_refresh);
        row_two_right.append(&refresh_now);
        row_two_right.append(&session);

        let row_two = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row_two.add_css_class("sqarvo-status-row");
        row_two.append(&row_two_left);
        row_two.append(&row_two_middle);
        row_two.append(&row_two_right);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("sqarvo-status-bar");
        root.set_valign(gtk::Align::End);
        root.append(&row_one);
        root.append(&row_two);

        Rc::new(Self {
            root,
            add,
            delete,
            apply,
            discard,
            refresh,
            cancel,
            pager_summary,
            pager_buttons: RefCell::new(pager_buttons),
            selected_label,
            last_refresh_at: Cell::new(None),
            row_one,
            row_one_left,
            row_one_middle,
            row_one_right,
            row_two,
            row_two_left,
            row_two_middle,
            row_two_right,
            sql_summary,
            record,
            last_refresh,
            session,
            connection: RefCell::new(None),
            catalog: RefCell::new(None),
            idle_summary: RefCell::new(None),
            txn: RefCell::new(String::new()),
            timing_ms: Cell::new(None),
        })
    }

    fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// 布局自检用的两行与三组（行 1：过滤 / 行操作 / 分页器；行 2：SQL 摘要 / 记录 / 右组）。
    ///
    /// 返回控件克隆，调用方只在布局稳定后读取几何，不参与显隐与业务逻辑。
    pub fn audit_groups(&self) -> Vec<(&'static str, gtk::Widget)> {
        vec![
            ("状态栏 行1 整行", self.row_one.clone().upcast()),
            (
                "状态栏 行1 左组(连接过滤)",
                self.row_one_left.clone().upcast(),
            ),
            (
                "状态栏 行1 中组(行操作)",
                self.row_one_middle.clone().upcast(),
            ),
            (
                "状态栏 行1 右组(分页器)",
                self.row_one_right.clone().upcast(),
            ),
            ("状态栏 行2 整行", self.row_two.clone().upcast()),
            (
                "状态栏 行2 左组(SQL 摘要)",
                self.row_two_left.clone().upcast(),
            ),
            (
                "状态栏 行2 中组(记录位置)",
                self.row_two_middle.clone().upcast(),
            ),
            (
                "状态栏 行2 右组(刷新·会话)",
                self.row_two_right.clone().upcast(),
            ),
        ]
    }

    /// 数据页快照：行操作可用性、记录位置、事务徽标、分页器与选中数。
    pub fn apply_snapshot(&self, snapshot: &StatusSnapshot) {
        self.apply_row_ops(&snapshot.row_ops);
        // 分页器只在有表时出现：没有表就整格隐藏，不用「未打开表」占位。
        let pager_text = match (&snapshot.table, snapshot.state) {
            (None, _) => None,
            (Some(_), DataState::Loading) => Some("读取中…".to_owned()),
            (Some(_), DataState::Error) => Some("读取失败".to_owned()),
            (Some(_), DataState::Disconnected) => Some("已断开".to_owned()),
            (Some(_), _) => {
                // 页码只读显示；总数按三态（共 N / 约 N / 无）——能力位为 false 时不显示总数。
                let mut text = format!("第 {} 页", snapshot.page_index.max(1));
                if let Some(count) = snapshot.page_count {
                    text = format!("第 {} / {} 页", snapshot.page_index.max(1), count.max(1));
                }
                if let Some(total) = &snapshot.total_text {
                    text.push_str(&format!(" · {total}"));
                }
                Some(text)
            }
        };
        self.pager_summary
            .set_text(pager_text.as_deref().unwrap_or(""));
        self.pager_summary.set_visible(pager_text.is_some());
        // 翻页按钮：只按快照里的可用性；没有表时全部禁用。
        let pager_buttons = self.pager_buttons.borrow().clone();
        for (button, enabled, tip) in [
            (&pager_buttons[0], snapshot.can_first, "首页（offset = 0）"),
            (&pager_buttons[1], snapshot.can_previous, "上一页"),
            (&pager_buttons[2], snapshot.can_next, "下一页"),
            (&pager_buttons[3], snapshot.can_last, "末页（需要总数）"),
        ] {
            button.set_sensitive(enabled && snapshot.table.is_some());
            button.set_tooltip_text(Some(tip));
        }
        // `N selected`：只在多选时出现（单选与 0 选都不占位置）。
        let selected = snapshot.selected_count;
        self.selected_label
            .set_text(&format!("{selected} selected"));
        self.selected_label.set_visible(selected > 0);
        self.selected_label.set_tooltip_text(Some(&format!(
            "已选中 {selected} 行：Ctrl/Shift 可多选，删除按批量语义提交"
        )));
        // 记录位置只在有表时出现：SQL 页没有「第 N 行」可报，就不占这一格。
        self.record.set_visible(snapshot.table.is_some());
        if snapshot.table.is_some() {
            self.record.set_text(&record_text(snapshot));
            self.record
                .set_tooltip_text(Some(&record_tooltip(snapshot)));
        }
        self.last_refresh_at.set(snapshot.last_refresh);
        self.last_refresh
            .set_visible(snapshot.last_refresh.is_some());
        if let Some(instant) = snapshot.last_refresh {
            self.last_refresh.set_text(&format!(
                "Last Refresh: {}",
                relative_age(instant.elapsed().as_secs())
            ));
            self.last_refresh.set_tooltip_text(Some(&format!(
                "上次读取时间：{}（offset 分页不保证跨页稳定，必要时点旁边的刷新）",
                clock_time(instant)
            )));
        }
        self.txn.replace(snapshot.txn_label.clone());
        // 表标签的 SQL 摘要 = 当前筛选/排序拼出的只读 `SELECT … LIMIT …`（不执行）。
        if snapshot.table.is_some() && !snapshot.summary.is_empty() {
            self.set_sql_summary(&snapshot.summary);
        }
        self.refresh_session();
    }

    /// 定时重算「Last Refresh」的相对时间（<1m / Nm）；数据页推送新快照时也会重算。
    pub fn tick_relative_time(&self) {
        let Some(instant) = self.last_refresh_at.get() else {
            return;
        };
        self.last_refresh.set_text(&format!(
            "Last Refresh: {}",
            relative_age(instant.elapsed().as_secs())
        ));
    }

    /// 行操作按钮组：可用性与页内按钮完全一致，全部禁用时用 tooltip 说明原因。
    fn apply_row_ops(&self, ops: &RowOpsState) {
        for (button, enabled, base) in [
            (&self.add, ops.add, "在当前表末尾新增一行"),
            (&self.delete, ops.delete, "标记删除选中行（提交前可撤销）"),
            (&self.apply, ops.apply, "提交所有脏行的变更"),
            (&self.discard, ops.discard, "放弃全部未提交的编辑与删除标记"),
            (&self.refresh, ops.refresh, "重新读取当前表的第一页"),
            (&self.cancel, ops.cancel, "取消在途的提交批次"),
        ] {
            button.set_sensitive(enabled);
            let tooltip = match &ops.reason {
                Some(reason) if !enabled => format!("{base}（当前不可用：{reason}）"),
                _ => base.to_owned(),
            };
            button.set_tooltip_text(Some(&tooltip));
        }
    }

    /// 当前标签没有表格（Objects / 查询标签）：分页器与记录位置整格隐藏，
    /// 行操作全部禁用并用 tooltip 说明原因——不留下「看起来能点」的按钮。
    pub fn show_no_table(&self, reason: &str) {
        self.pager_summary.set_visible(false);
        self.record.set_visible(false);
        self.last_refresh.set_visible(false);
        self.selected_label.set_visible(false);
        // 没有表时翻页按钮一律禁用（不是「看起来能点」）。
        for button in self.pager_buttons.borrow().iter() {
            button.set_sensitive(false);
        }
        self.apply_row_ops(&RowOpsState {
            add: false,
            delete: false,
            apply: false,
            discard: false,
            refresh: false,
            cancel: false,
            begin_txn: false,
            commit_txn: false,
            rollback_txn: false,
            reason: Some(reason.to_owned()),
        });
        self.txn.replace(String::new());
        self.refresh_session();
    }

    /// SQL 摘要（行 2 左段）：来自查询事件，不推测。
    pub fn set_sql_summary(&self, text: &str) {
        self.sql_summary.set_text(&format!("SQL：{text}"));
        self.sql_summary.set_tooltip_text(Some(text));
    }

    /// 上一段查询的耗时（从开始到结束事件的毫秒数）。
    pub fn set_timing(&self, millis: Option<u64>) {
        self.timing_ms.set(millis);
        self.refresh_session();
    }

    /// 当前连接名（连接建立后设置；断连时清空）。
    pub fn set_connection(&self, name: Option<&str>) {
        self.connection.replace(name.map(str::to_owned));
        self.refresh_session();
    }

    /// 启动态补充信息（`驱动 N · 浏览会话 M`）：只在未连接时出现。
    pub fn set_idle_summary(&self, text: Option<&str>) {
        self.idle_summary.replace(text.map(str::to_owned));
        self.refresh_session();
    }

    /// 当前表标签所属的库（标签名 `对象 @ 库` 里的库）。
    ///
    /// 无默认库连接也能从树里打开不同库的表，状态栏必须跟着**当前标签**说库，
    /// 而不是说连接的默认库——两者在跨库浏览时可以完全不同（ADR 0024 §3）。
    pub fn set_catalog(&self, catalog: Option<&str>) {
        let next = catalog.map(str::to_owned);
        if *self.catalog.borrow() == next {
            return;
        }
        if let Some(catalog) = &next {
            // 只在值变化时记一行：验收据此确认状态栏真的跟着标签换库。
            eprintln!("[status] 状态栏显示所属库：{catalog}");
        }
        self.catalog.replace(next);
        self.refresh_session();
    }

    /// 「连接 · 事务 · 耗时」：只显示已知项，不写占位符。
    fn refresh_session(&self) {
        let mut parts: Vec<String> = Vec::new();
        if let Some(name) = self.connection.borrow().as_ref() {
            parts.push(name.clone());
        }
        if let Some(catalog) = self.catalog.borrow().as_ref() {
            parts.push(format!("库：{catalog}"));
        }
        if let Some(summary) = self.idle_summary.borrow().as_ref() {
            parts.push(summary.clone());
        }
        let txn = self.txn.borrow();
        if !txn.is_empty() {
            parts.push(format!("事务：{txn}"));
        }
        drop(txn);
        if let Some(millis) = self.timing_ms.get() {
            parts.push(format!("上次查询 {millis} ms"));
        }
        let text = parts.join(" · ");
        self.session.set_text(&text);
        self.session.set_tooltip_text(Some(&text));
        self.session.set_visible(!parts.is_empty());
    }
}

/// 行 2 中段：记录位置 = 本页在表里的**行号区间**。
///
/// 页码与总数只在分页器旁（行 1 右组）出现一次；这一格重复它们就成了同一句话说三遍
/// （工具条下方还写过第三遍）。行号区间是分页器给不出的信息，两者互补而不重复。
fn record_text(snapshot: &StatusSnapshot) -> String {
    let Some(table) = snapshot.table.as_ref() else {
        return "未打开表".to_owned();
    };
    let mut text = format!("{} · {}", table.display_name, snapshot.position_text);
    if snapshot.pending > 0 {
        text.push_str(&format!(" · 待提交 {} 条", snapshot.pending));
    }
    text
}

/// 记录位置的 tooltip：数据页状态行 + 脏行，解释数字的来历。
fn record_tooltip(snapshot: &StatusSnapshot) -> String {
    let mut text = snapshot.status_text.clone();
    if snapshot.dirty > 0 {
        text.push_str(&format!("\n脏行 {}", snapshot.dirty));
    }
    if text.is_empty() {
        text.push_str("未打开表");
    }
    text
}

/// 把「某个 `Instant` 的墙上时间」格式化成 `HH:MM:SS`（本地时区）。
///
/// `Instant` 本身没有日历语义，因此用「现在」减去它已经过去的时长反推墙上时间；
/// 时区转换用 `libc::localtime_r`（`libc` 已是本工程的依赖），不引第三方时间库。
/// 万一转换失败，退回「N 秒前」——状态栏宁可显示相对时间，也不显示错误的时间戳。
fn clock_time(instant: Instant) -> String {
    let wall = std::time::SystemTime::now()
        .checked_sub(instant.elapsed())
        .unwrap_or_else(std::time::SystemTime::now);
    let seconds = wall
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let stamp = libc::time_t::try_from(seconds).unwrap_or(libc::time_t::MAX);
    // SAFETY: `localtime_r` 只写我们提供的 `tm`（POD），不返回指向静态区的指针，
    // 因此可以安全地在多线程进程里使用。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let converted = unsafe { libc::localtime_r(&stamp, &mut tm) };
    if converted.is_null() {
        return format!("{} 秒前", instant.elapsed().as_secs());
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::table::TableEntry;
    use crate::browse_view::{PagerState, TotalInfo};
    use crate::data_page::RowOpsState;
    use crate::proto::services::v1::{ObjectKind, ObjectRef, TotalKind};

    fn table(name: &str) -> TableEntry {
        TableEntry {
            reference: ObjectRef::default(),
            display_name: name.to_owned(),
            kind: ObjectKind::Table,
        }
    }

    fn ops(all: bool) -> RowOpsState {
        RowOpsState {
            add: all,
            delete: all,
            apply: all,
            discard: all,
            refresh: all,
            cancel: all,
            begin_txn: all,
            commit_txn: all,
            rollback_txn: all,
            reason: if all {
                None
            } else {
                Some("尚未打开表".to_owned())
            },
        }
    }

    fn snapshot(state: DataState, table: Option<TableEntry>) -> StatusSnapshot {
        StatusSnapshot {
            state,
            table,
            rows: 0,
            page_index: 1,
            page_count: None,
            total_text: None,
            position_text: PagerState::new(0, 500, TotalInfo::unknown(), false, 0).position_text(),
            selected_count: 0,
            dirty: 0,
            pending: 0,
            txn_label: "自动提交".to_owned(),
            row_ops: ops(false),
            last_refresh: None,
            status_text: String::new(),
            summary: String::new(),
            can_first: false,
            can_previous: false,
            can_next: false,
            can_last: false,
            sort_filter_total: true,
        }
    }

    #[test]
    fn record_text_carries_the_row_span_without_repeating_page_or_total() {
        let mut snapshot = snapshot(DataState::Ready, Some(table("items")));
        snapshot.rows = 500;
        snapshot.position_text = PagerState::new(
            0,
            500,
            TotalInfo::from_proto(TotalKind::Unknown as i32, 0, false),
            true,
            500,
        )
        .position_text();
        let text = record_text(&snapshot);
        assert_eq!(text, "items · 第 1–500 行");
        // 页码与总数只在分页器旁出现一次：这一格重复它们就是同一句话说三遍。
        assert!(!text.contains("页"), "实际：{text}");
        assert!(!text.contains("共 "));
        assert!(!text.contains(" of "));
    }

    #[test]
    fn record_text_follows_the_offset_so_the_span_moves_with_the_page() {
        let mut snapshot = snapshot(DataState::Ready, Some(table("items")));
        snapshot.rows = 500;
        snapshot.page_index = 2;
        snapshot.page_count = Some(3);
        snapshot.total_text = Some("共 1200 行".to_owned());
        snapshot.position_text = PagerState::new(
            500,
            500,
            TotalInfo::from_proto(TotalKind::Exact as i32, 1200, false),
            true,
            500,
        )
        .position_text();
        let text = record_text(&snapshot);
        assert_eq!(text, "items · 第 501–1000 行");
        assert!(!text.contains("页"), "页码不在这里重复：{text}");
    }

    #[test]
    fn record_text_stops_at_the_loaded_rows_for_empty_tables() {
        let snapshot = snapshot(DataState::Empty, Some(table("empty_items")));
        assert_eq!(record_text(&snapshot), "empty_items · 空表");
    }

    #[test]
    fn record_text_without_a_table_says_so_instead_of_zero() {
        let text = record_text(&snapshot(DataState::Idle, None));
        assert_eq!(text, "未打开表");
    }

    #[test]
    fn record_tooltip_carries_the_data_page_status_line() {
        let mut snapshot = snapshot(DataState::Ready, Some(table("items")));
        snapshot.status_text = "已加载 500 行 · 1 页".to_owned();
        snapshot.dirty = 2;
        let tooltip = record_tooltip(&snapshot);
        assert!(tooltip.contains("已加载 500 行"));
        assert!(tooltip.contains("脏行 2"));
    }

    #[test]
    fn clock_time_formats_as_a_wall_clock_stamp() {
        let text = clock_time(Instant::now());
        assert_eq!(text.len(), 8, "应为 HH:MM:SS：{text}");
        assert_eq!(text.matches(':').count(), 2);
    }
}
