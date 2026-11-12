//! 窗口尺寸按显示器逻辑工作区分档、最小窗口收敛与几何记忆（#158）。
//!
//! 口径：`docs/ui-specification/scaling-and-window-sizing.md` §5–§7、
//! [`docs/decisions/0026-ui-scaling-window-sizing.md`](../../../docs/decisions/0026-ui-scaling-window-sizing.md)。
//!
//! 三件事只有**一个**入口 [`relayout`]：初始窗口尺寸、最小窗口、面板钳制区间都由
//! `(work_area, scale, tier)` 算出来。换显示器 / 改分辨率（`GdkMonitor` 的
//! `invalidate` / `geometry` 通知）、系统缩放变化（surface 的 `scale-factor`）、
//! 窗口进入另一块显示器（surface 的 `enter-monitor`）都接到同一个 [`apply`]，
//! 避免「拖到另一块屏后窗口没变、CSS 变了」。
//!
//! 纯算术（[`work_area`] / [`recommended`] / [`min_window`] / [`panel_bounds`]）与
//! GTK 部分分开：前者在没有显示服务器的环境里可单测，后者只负责取值与应用。
//!
//! # 平台边界
//!
//! GTK4 没有设置 / 读取 toplevel 位置的 API（Wayland 明确不允许，X11 后端同样未暴露），
//! 所以 `window_x` / `window_y` 只读不写：读到旧值仍参与「是否还在某块显示器内」的校验，
//! 但无法把窗口挪回去。尺寸与最大化状态可以完整恢复。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;

use crate::layout::{self, Tier};
use crate::window_state;

/// GNOME 顶栏高度估算（§5.1：`GdkMonitor` 没有 `workarea()`，工作区 = 几何 − 顶栏）。
///
/// 只是**估算**：非 GNOME 桌面（任务栏在底部 / 高度不同）会差几十像素，但「不超出
/// 屏幕」这条验收靠它成立——减去一个偏大的值比减去偏小的值安全。
pub const TOP_BAR_HEIGHT: i32 = 32;

/// 工作区任意一维小于它时认为减法不可信，退回显示器几何（§5.1 的防呆）。
const MIN_USABLE: i32 = 200;

/// 矩形的边距：窗口至少留 32 宽 / 48 高给窗口管理器与状态栏（§5.2 / §5.4）。
const WIDTH_MARGIN: i32 = 32;
const HEIGHT_MARGIN: i32 = 48;

/// 恢复几何时允许的越界容差（§7 规则 3）。
pub const GEOMETRY_TOLERANCE: i32 = 32;

/// 逻辑像素矩形：显示器几何或窗口矩形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl From<gdk::Rectangle> for Rect {
    fn from(value: gdk::Rectangle) -> Self {
        Self {
            x: value.x(),
            y: value.y(),
            width: value.width(),
            height: value.height(),
        }
    }
}

/// 面板可拖拽区间（由档位算出，§6.2 的「重设面板钳制区间」）。
///
/// 四个数字直接取 `metrics.toml` 的 `sidebar_min/max`、`inspector_min/max`（不是
/// 「基准 × 档位」）；区间与窗口尺寸从同一个入口出，不再各读一份字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelBounds {
    pub sidebar_min: i32,
    pub sidebar_max: i32,
    pub inspector_min: i32,
    pub inspector_max: i32,
}

impl PanelBounds {
    /// 把侧栏宽度钳制到可拖拽区间。
    #[must_use]
    pub fn clamp_sidebar(&self, width: i32) -> i32 {
        width.clamp(self.sidebar_min, self.sidebar_max)
    }

    /// 把信息面板宽度钳制到可拖拽区间。
    #[must_use]
    pub fn clamp_inspector(&self, width: i32) -> i32 {
        width.clamp(self.inspector_min, self.inspector_max)
    }

    /// 由 `GtkPaned` 的位置反推右侧面板宽度并钳制。
    #[must_use]
    pub fn inspector_width_from_position(&self, pane_width: i32, position: i32) -> i32 {
        self.clamp_inspector(pane_width - position - 1)
    }
}

/// [`relayout`] 的结果：窗口尺寸、最小尺寸、面板区间。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sizing {
    /// 推荐 / 目标窗口尺寸（逻辑像素）。
    pub window: (i32, i32),
    /// 本次显示器下的最小窗口尺寸，交给 `set_size_request`。
    pub min: (i32, i32),
    /// 面板可拖拽区间。
    pub panels: PanelBounds,
    /// 生效的栅格化比例（本平台几何已是逻辑像素，只用于日志 / 形态对齐）。
    pub scale: f64,
}

fn scale_round(value: i32, factor: f64) -> i32 {
    (f64::from(value) * factor).round() as i32
}

/// 工作区估算：`GdkMonitor::geometry()` 是逻辑像素，GTK4 没有 `workarea()`。
///
/// 宽度不减、高度减 [`TOP_BAR_HEIGHT`]；减完任一维小于 [`MIN_USABLE`] 就退回几何——
/// 说明这个估算在这台机器上不可信（例如无顶栏的合成器），宁可让窗口贴边也不要算出负数。
#[must_use]
pub fn work_area(geometry: Rect) -> Rect {
    let height = geometry.height - TOP_BAR_HEIGHT;
    if geometry.width < MIN_USABLE || height < MIN_USABLE {
        return geometry;
    }
    Rect { height, ..geometry }
}

/// 由几何与计算出的工作区得出窗口尺寸 / 最小尺寸 / 面板区间（§6.2 的唯一入口）。
///
/// `scale` 本平台不参与算术（GTK 的逻辑像素已经除过系统缩放），保留参数是为了三端同形：
/// Windows 的 `DisplayArea.WorkArea` 是物理像素，要先除栅格化比例。
#[must_use]
pub fn relayout(work: Rect, scale: f64, tier: Tier) -> Sizing {
    let (width, height) = recommended(work, tier);
    let min = min_window(work, tier);
    Sizing {
        window: (width, height),
        min,
        panels: panel_bounds(tier),
        scale: if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        },
    }
}

/// 基准推荐尺寸（§5.2 四档 × 档位），再按下限收敛到工作区内。
///
/// 下限是 `min(960·T, W-32) × min(640·T, H-48)`（`metrics.toml` 的
/// `min_window_*` 已是写死的 960/1200/1440）：屏比最小窗口还小时，**宁可挤也不
/// 超出屏幕**（ADR 0026 取舍）。上限同样是 `(W-32, H-48)`，所以结果**永远**满足
/// `w ≤ W-32`、`h ≤ H-48`（`--qa-report-monitor` 的验收条目）。
#[must_use]
pub fn recommended(work: Rect, tier: Tier) -> (i32, i32) {
    let tier_scale = tier.scale();
    let width_available = work.width.max(1);
    let height_available = work.height.max(1);
    let (base_width, base_height) = if width_available < 1200 {
        // 塞满：与上层钳制同一口径（`(W-32, H-48)`）。规范 §5.2 原写 `(W-24, H-24)`，
        // 与 §5.3 的上限钳制 / §9 #2 的验收冲突（W=1199 时 1175×976 vs 1167×952），
        // 2026-11-11 规范按钳制口径统一，这里跟着写成同一个式子。
        (
            width_available - WIDTH_MARGIN,
            height_available - HEIGHT_MARGIN,
        )
    } else if width_available < 1700 {
        (
            width_available - 56,
            scale_round(height_available, 0.84).min(860),
        )
    } else if width_available < 2200 {
        (
            (width_available - 120).min(1400),
            scale_round(height_available, 0.80).min(900),
        )
    } else {
        (
            (width_available - 160).min(1680),
            scale_round(height_available, 0.78).min(1000),
        )
    };
    let min = min_window(work, tier);
    (
        fit_axis(
            scale_round(base_width, tier_scale),
            min.0,
            width_available - WIDTH_MARGIN,
        ),
        fit_axis(
            scale_round(base_height, tier_scale),
            min.1,
            height_available - HEIGHT_MARGIN,
        ),
    )
}

/// 最小窗口尺寸（§5.4）：`min(960·T, W-32) × min(640·T, H-48)`。
///
/// 960 / 1200 / 1440 直接取 `metrics.toml` 的 `min_window_width`——不再用倍率算。
#[must_use]
pub fn min_window(work: Rect, tier: Tier) -> (i32, i32) {
    let metrics = layout::Metrics::for_tier(tier);
    (
        metrics
            .min_window_width
            .min(work.width - WIDTH_MARGIN)
            .max(1),
        metrics
            .min_window_height
            .min(work.height - HEIGHT_MARGIN)
            .max(1),
    )
}

/// 把一个轴上「下限 / 上限」同时生效：上限优先（装不下时宁可小于最小值）。
fn fit_axis(value: i32, floor: i32, ceiling: i32) -> i32 {
    let ceiling = ceiling.max(1);
    value.clamp(floor.min(ceiling), ceiling)
}

/// 档位可用性守卫（§2.2）：工作区逻辑宽高都不小于该档的**未收敛**最小窗口。
///
/// 用 `metrics.toml` 的 `min_window_width/height`，不用收敛后的值——否则任何屏幕都
/// 能声称自己支持 150%。
#[must_use]
pub fn available(tier: Tier, work: Rect) -> bool {
    let metrics = layout::Metrics::for_tier(tier);
    work.width >= metrics.min_window_width && work.height >= metrics.min_window_height
}

/// 从 `tier` 起向下一档找一个可用档（含自身）；都不可用时返回 `None`。
#[must_use]
pub fn first_available_from(tier: Tier, work: Rect) -> Option<Tier> {
    Tier::ALL
        .iter()
        .rev()
        .filter(|candidate| **candidate <= tier)
        .find(|candidate| available(**candidate, work))
        .copied()
}

/// 面板可拖拽区间（直接取 `metrics.toml`，§6.2）。
#[must_use]
pub fn panel_bounds(tier: Tier) -> PanelBounds {
    let metrics = layout::Metrics::for_tier(tier);
    PanelBounds {
        sidebar_min: metrics.sidebar_min,
        sidebar_max: metrics.sidebar_max,
        inspector_min: metrics.inspector_min,
        inspector_max: metrics.inspector_max,
    }
}

/// 目标尺寸的来源（`--qa-report-monitor` 的「来源=」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 按工作区分档算出来。
    Tier,
    /// `window.json` 记着的几何。
    Remembered,
    /// `--window-size`（QA 取证）。
    Qa,
}

impl Source {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tier => "分档",
            Self::Remembered => "记忆",
            Self::Qa => "QA",
        }
    }
}

/// 一次 relayout 的结论（`--qa-report-monitor` 的输出就是它）。
#[derive(Debug, Clone)]
pub struct Report {
    pub connector: String,
    pub geometry: Rect,
    pub scale: f64,
    pub work: Rect,
    pub window: (i32, i32),
    pub tier: f64,
    pub source: Source,
}

/// 上一次打印的结论：`present()` 之后 `enter-monitor` / `scale-factor` 会立刻再触发一次
/// 相同的 relayout，同样的结论不重复刷屏（只影响日志，不跳过任何计算）。
static LAST_REPORT: Mutex<Option<String>> = Mutex::new(None);

impl Report {
    /// 固定一行，字段顺序与 Issue #158 / 规范一致（机器可 grep）。
    pub fn print(&self) {
        let line = format!(
            "[monitor] {} 几何 {}x{} scale={:.2} 工作区 {}x{} → 窗口 {}x{} 档位={:.2} 来源={}",
            self.connector,
            self.geometry.width,
            self.geometry.height,
            self.scale,
            self.work.width,
            self.work.height,
            self.window.0,
            self.window.1,
            self.tier,
            self.source.label()
        );
        let mut last = LAST_REPORT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.as_deref() == Some(line.as_str()) {
            return;
        }
        eprintln!("{line}");
        *last = Some(line);
    }
}

/// 显示器矩形列表（恢复几何前的越界校验用）。
fn monitor_rects(display: &gdk::Display) -> Vec<Rect> {
    let model = display.monitors();
    (0..model.n_items())
        .filter_map(|index| model.item(index))
        .filter_map(|object| object.downcast::<gdk::Monitor>().ok())
        .map(|monitor| Rect::from(monitor.geometry()))
        .collect()
}

/// 窗口当前所在显示器的取值（surface / monitor 未就绪时为 `None`）。
struct MonitorSnapshot {
    monitor: gdk::Monitor,
    geometry: Rect,
    work: Rect,
    scale: f64,
}

fn monitor_snapshot(window: &adw::ApplicationWindow) -> Option<MonitorSnapshot> {
    let surface = window.surface()?;
    let display = gtk::prelude::WidgetExt::display(window);
    let monitor = display.monitor_at_surface(&surface)?;
    let geometry = Rect::from(monitor.geometry());
    Some(MonitorSnapshot {
        work: work_area(geometry),
        geometry,
        scale: monitor.scale(),
        monitor,
    })
}

/// 当前窗口所在显示器的工作区（档位可用性守卫与 `density` 共用，不再各写一份减顶栏）。
#[must_use]
pub fn current_work(window: &adw::ApplicationWindow) -> Option<Rect> {
    monitor_snapshot(window).map(|snapshot| snapshot.work)
}

/// 按窗口当前所在显示器重算并应用尺寸（启动与每次 relayout 都走这里）。
///
/// 顺序按规范 §6.3：先 `present()`，拿到 surface 后调本函数。surface / monitor 尚未
/// 就绪时返回 `None`（调用方 [`watch`] 会短轮询），不猜显示器。
#[must_use]
pub fn apply(window: &adw::ApplicationWindow) -> Option<Report> {
    apply_inner(window, true)
}

fn apply_inner(window: &adw::ApplicationWindow, restore_saved: bool) -> Option<Report> {
    let snapshot = monitor_snapshot(window)?;
    let display = gtk::prelude::WidgetExt::display(window);
    let tier = layout::current_tier();
    let sizing = relayout(snapshot.work, snapshot.scale, tier);

    let mut size = sizing.window;
    let mut source = Source::Tier;
    let mut maximize = false;

    // 档位可用性只在启动时决定一次（`density::ensure_startup_tier`）：之后的换屏若
    // 放不下当前档，窗口仍按工作区钳制（§5.3），菜单里该项会显示为不可用。
    if !restore_saved && !available(tier, snapshot.work) {
        eprintln!(
            "[tier] {} 在当前工作区 {}x{} 不可用（需要 {}×{}）：窗口按工作区钳制",
            tier.id(),
            snapshot.work.width,
            snapshot.work.height,
            layout::Metrics::for_tier(tier).min_window_width,
            layout::Metrics::for_tier(tier).min_window_height
        );
    }

    // `--window-size` 优先于算出来的尺寸：取证要在固定尺寸下比对像素。
    if let Some((width, height)) = crate::qa::window_size() {
        size = (width, height);
        source = Source::Qa;
        if restore_saved {
            eprintln!("[monitor] --window-size {width}x{height} 优先于分档推荐（来自 QA）");
        }
    } else if restore_saved {
        // 只有启动时恢复几何：之后的换屏 / 缩放变化按新工作区重新分档，
        // 否则换到小屏后仍会套用另一块屏记下的尺寸。
        if let Some(saved) = window_state::saved_geometry() {
            if saved.tier == tier {
                if saved.fits_in(&monitor_rects(&display)) {
                    size = (saved.width, saved.height);
                    source = Source::Remembered;
                    maximize = saved.maximized;
                    eprintln!(
                        "[monitor] 窗口几何来自记忆：{}x{}{}（最大化={}）",
                        saved.width,
                        saved.height,
                        saved.origin_label(),
                        maximize
                    );
                } else {
                    eprintln!(
                        "[monitor] 几何越界已丢弃：记忆的窗口 {}x{}{} 不在任何当前显示器几何内（±{} 容差），回退推荐尺寸 {}x{}",
                        saved.width,
                        saved.height,
                        saved.origin_label(),
                        GEOMETRY_TOLERANCE,
                        sizing.window.0,
                        sizing.window.1
                    );
                }
            } else {
                // 记忆的尺寸属于另一档：按当前档位重算推荐尺寸（§2.3）。这条日志就是
                // 「1.5 档下用 1200 宽的窗口启动 → 自动放大」的证据。
                let verb = if sizing.window.0 > saved.width || sizing.window.1 > saved.height {
                    "档位导致窗口放大"
                } else {
                    "档位导致窗口重算"
                };
                eprintln!(
                    "[monitor] {verb}：{}x{} → {}x{}（记忆尺寸属于 {}，当前 {}{}）",
                    saved.width,
                    saved.height,
                    sizing.window.0,
                    sizing.window.1,
                    saved.tier.id(),
                    tier.id(),
                    if saved.maximized {
                        "，保持最大化"
                    } else {
                        ""
                    }
                );
                maximize = saved.maximized;
            }
        }
    }

    // 先定最小尺寸：窗口管理器据此把过小的请求抬上来，`--window-size` 也走这条限制。
    window.set_size_request(sizing.min.0, sizing.min.1);
    let width = size.0.max(sizing.min.0);
    let height = size.1.max(sizing.min.1);
    if (width, height) != size {
        eprintln!(
            "[monitor] 目标窗口 {}x{} 低于最小窗口 {}x{}：窗口会被抬到下限",
            size.0, size.1, sizing.min.0, sizing.min.1
        );
    }
    // GTK4 没有 `gtk_window_resize`：present 之后改窗口尺寸只有 `default_size` 这一条路
    // （X11 后端会据此重设 toplevel 尺寸；Wayland 由合成器决定是否遵守）。
    window.set_default_size(width, height);
    if maximize && !window.is_maximized() {
        window.maximize();
    }

    let report = Report {
        connector: snapshot
            .monitor
            .connector()
            .map_or_else(|| "未知".to_owned(), |connector| connector.to_string()),
        geometry: snapshot.geometry,
        scale: sizing.scale,
        work: snapshot.work,
        window: (width, height),
        tier: tier.scale(),
        source,
    };
    // 只读开关：打印一行，不改变任何尺寸（尺寸已经在上面按同一份结论应用）。
    if crate::qa::report_monitor() {
        report.print();
    }
    Some(report)
}

/// 切档时按规范 §5.3 重算窗口尺寸并应用（不用记忆几何，也不用 `--window-size`）。
///
/// 「切换档位时窗口跟着放大」是 §2.3 的硬要求：只放大界面不放大窗口，150% 档的表格
/// 只剩 378px。放大到的尺寸仍受工作区钳制（`w ≤ W-32`、`h ≤ H-48`）。
pub fn apply_for_tier(window: &adw::ApplicationWindow) -> Option<Report> {
    let snapshot = monitor_snapshot(window)?;
    let tier = layout::current_tier();
    let sizing = relayout(snapshot.work, snapshot.scale, tier);
    let previous = (window.default_width(), window.default_height());
    window.set_size_request(sizing.min.0, sizing.min.1);
    window.set_default_size(sizing.window.0, sizing.window.1);
    let verb = if sizing.window.0 > previous.0 || sizing.window.1 > previous.1 {
        "档位导致窗口放大"
    } else {
        "档位导致窗口重算"
    };
    eprintln!(
        "[monitor] {verb}：{}x{} → {}x{}（档位 {}）",
        previous.0,
        previous.1,
        sizing.window.0,
        sizing.window.1,
        tier.id()
    );
    let report = Report {
        connector: snapshot
            .monitor
            .connector()
            .map_or_else(|| "未知".to_owned(), |connector| connector.to_string()),
        geometry: snapshot.geometry,
        scale: sizing.scale,
        work: snapshot.work,
        window: sizing.window,
        tier: tier.scale(),
        source: Source::Tier,
    };
    if crate::qa::report_monitor() {
        report.print();
    }
    Some(report)
}

/// 把 relayout 接到窗口：首次取值 + 之后所有触发源（§6.1 / §6.2）。
pub fn watch(window: &adw::ApplicationWindow) {
    let state = Rc::new(WatchState::default());
    start(window, &state, 0);
}

#[derive(Default)]
struct WatchState {
    /// 一次事件循环内只重算一次：invalidate 与 geometry 往往同时到。
    pending: Cell<bool>,
    /// 当前监听的显示器（换屏时先断开旧的）。
    monitor: RefCell<Option<(gdk::Monitor, Vec<glib::SignalHandlerId>)>>,
    /// 几何记忆的防抖定时器。
    persist: RefCell<Option<glib::SourceId>>,
    /// 上一次 relayout 用的（显示器几何, scale）：`present()` 之后 `enter-monitor` /
    /// `scale-factor` 会立刻用同一份输入再触发一次，同样的输入不重复改尺寸——
    /// 否则刚恢复的记忆几何会被「分档推荐」立刻顶掉。
    key: RefCell<Option<(Rect, u64)>>,
}

/// 当前窗口所在显示器的（几何, scale 位模式）：判断一次触发是不是「输入没变」。
fn current_key(window: &adw::ApplicationWindow) -> Option<(Rect, u64)> {
    let surface = window.surface()?;
    let display = gtk::prelude::WidgetExt::display(window);
    let monitor = display.monitor_at_surface(&surface)?;
    Some((Rect::from(monitor.geometry()), monitor.scale().to_bits()))
}

/// first relayout：surface 未就绪时短轮询（规范 §6.3 允许首帧跳动，不允许猜显示器）。
fn start(window: &adw::ApplicationWindow, state: &Rc<WatchState>, attempts: u32) {
    if let Some(report) = apply(window) {
        state
            .key
            .replace(Some((report.geometry, report.scale.to_bits())));
        bind_surface(window, state);
        bind_monitor(window, state);
        bind_persist(window, state);
        return;
    }
    if attempts >= 25 {
        eprintln!("[monitor] 拿不到窗口所在显示器（surface 未就绪）：保持当前窗口尺寸");
        return;
    }
    let weak = window.downgrade();
    let state = Rc::clone(state);
    glib::timeout_add_local_once(Duration::from_millis(40), move || {
        if let Some(window) = weak.upgrade() {
            start(&window, &state, attempts + 1);
        }
    });
}

/// 系统缩放变化与「窗口换了显示器」都接到同一个 [`apply`]。
fn bind_surface(window: &adw::ApplicationWindow, state: &Rc<WatchState>) {
    let Some(surface) = window.surface() else {
        return;
    };
    {
        let weak = window.downgrade();
        let state = Rc::clone(state);
        surface.connect_notify_local(Some("scale-factor"), move |_, _| {
            if let Some(window) = weak.upgrade() {
                schedule(&window, &state);
            }
        });
    }
    {
        let weak = window.downgrade();
        let state = Rc::clone(state);
        surface.connect_enter_monitor(move |_, _| {
            if let Some(window) = weak.upgrade() {
                schedule(&window, &state);
            }
        });
    }
}

/// 显示器几何 / 失效通知（分辨率、热插拔）接到同一个 [`apply`]。
fn bind_monitor(window: &adw::ApplicationWindow, state: &Rc<WatchState>) {
    let Some(surface) = window.surface() else {
        return;
    };
    let display = gtk::prelude::WidgetExt::display(window);
    let Some(monitor) = display.monitor_at_surface(&surface) else {
        return;
    };
    let mut slot = state.monitor.borrow_mut();
    if let Some((old, handlers)) = slot.take() {
        for handler in handlers {
            old.disconnect(handler);
        }
    }
    let mut handlers = Vec::with_capacity(2);
    {
        let weak = window.downgrade();
        let state = Rc::clone(state);
        handlers.push(monitor.connect_invalidate(move |_| {
            if let Some(window) = weak.upgrade() {
                schedule(&window, &state);
            }
        }));
    }
    {
        let weak = window.downgrade();
        let state = Rc::clone(state);
        handlers.push(monitor.connect_geometry_notify(move |_| {
            if let Some(window) = weak.upgrade() {
                schedule(&window, &state);
            }
        }));
    }
    *slot = Some((monitor, handlers));
}

/// 合并同一次事件循环里的多次触发，并避开「在显示器自己的信号里断开自己」。
fn schedule(window: &adw::ApplicationWindow, state: &Rc<WatchState>) {
    if state.pending.replace(true) {
        return;
    }
    let weak = window.downgrade();
    let state = Rc::clone(state);
    glib::idle_add_local_once(move || {
        state.pending.set(false);
        let Some(window) = weak.upgrade() else {
            return;
        };
        if let Some(key) = current_key(&window) {
            if state.key.borrow().as_ref() == Some(&key) {
                // 输入没变（就是刚 present 的那块屏）：不改尺寸，避免刚恢复的记忆几何
                // 被同一份输入重新分档顶掉。
                bind_monitor(&window, &state);
                return;
            }
            apply_inner(&window, false);
            state.key.replace(Some(key));
        }
        bind_monitor(&window, &state);
    });
}

/// 用户改变窗口尺寸 / 最大化状态后记入 `window.json`（v2，§7）。
fn bind_persist(window: &adw::ApplicationWindow, state: &Rc<WatchState>) {
    for property in ["default-width", "default-height", "maximized"] {
        let weak = window.downgrade();
        let state = Rc::clone(state);
        window.connect_notify_local(Some(property), move |_, _| {
            if let Some(window) = weak.upgrade() {
                persist_later(&window, &state);
            }
        });
    }
    // 首帧的尺寸是程序化落位的，不是用户选择：稳定 600ms 后才开始采样。
    let state = Rc::clone(state);
    let weak = window.downgrade();
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        if weak.upgrade().is_some() {
            state.persist.borrow_mut().take();
        }
    });
}

fn persist_later(window: &adw::ApplicationWindow, state: &Rc<WatchState>) {
    if let Some(source) = state.persist.borrow_mut().take() {
        source.remove();
    }
    let weak = window.downgrade();
    let closure_state = Rc::clone(state);
    let source = glib::timeout_add_local_once(Duration::from_millis(400), move || {
        closure_state.persist.borrow_mut().take();
        if let Some(window) = weak.upgrade() {
            persist(&window);
        }
    });
    *state.persist.borrow_mut() = Some(source);
}

fn persist(window: &adw::ApplicationWindow) {
    let (width, height) = (window.default_width(), window.default_height());
    let (width, height) = if width > 0 && height > 0 {
        (width, height)
    } else {
        (window.width(), window.height())
    };
    if width <= 0 || height <= 0 {
        return;
    }
    let geometry = window_state::WindowGeometry {
        width,
        height,
        // GTK4 读不到 toplevel 位置（见模块注释）：位置字段留空，尺寸仍然记忆。
        x: None,
        y: None,
        maximized: window.is_maximized(),
        tier: layout::current_tier(),
    };
    match window_state::remember_geometry(&geometry) {
        Ok(path) => eprintln!(
            "[monitor] 窗口几何 {width}x{height}（最大化={}）已记入 {}",
            geometry.maximized,
            path.display()
        ),
        Err(error) => eprintln!("[monitor] 窗口几何未能记住：{error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(width: i32, height: i32) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn work_area_subtracts_only_the_top_bar() {
        let geometry = work(1920, 1080);
        assert_eq!(work_area(geometry), work(1920, 1048));
        // 减法不可信（任一维 < 200）时退回几何，不产生负数 / 挤扁的工作区。
        let tiny = work(800, 150);
        assert_eq!(work_area(tiny), tiny);
        let narrow = work(150, 900);
        assert_eq!(work_area(narrow), narrow);
    }

    #[test]
    fn recommendation_matches_the_spec_table() {
        // §5.2 的六行参考值（100% 档）：三个设计约束的算术来源。
        for (area, expected) in [
            (work(1366, 736), (1310, 640)),
            (work(1280, 698), (1224, 640)),
            (work(1536, 838), (1480, 704)),
            (work(1920, 1048), (1400, 838)),
            (work(2048, 1120), (1400, 896)),
            (work(2560, 1408), (1680, 1000)),
        ] {
            assert_eq!(
                recommended(area, Tier::Compact),
                expected,
                "工作区 {area:?}"
            );
        }
    }

    #[test]
    fn recommendation_switches_at_the_four_boundaries() {
        // W = 1199 / 1200 / 1699 / 1700 / 2199 / 2200，H 固定 1000。
        // W=1199 走 §5.2 的「塞满」档：2026-11-11 起该档与上限钳制同一口径
        // `(W-32, H-48)`，所以就是 1167×952（原写的 1175×976 与 §9 #2
        // 「w ≤ W-32 且 h ≤ H-48」冲突，已按钳制统一）。
        assert_eq!(recommended(work(1199, 1000), Tier::Compact), (1167, 952));
        assert_eq!(recommended(work(1200, 1000), Tier::Compact), (1144, 840));
        assert_eq!(recommended(work(1699, 1000), Tier::Compact), (1643, 840));
        assert_eq!(recommended(work(1700, 1000), Tier::Compact), (1400, 800));
        assert_eq!(recommended(work(2199, 1000), Tier::Compact), (1400, 800));
        assert_eq!(recommended(work(2200, 1000), Tier::Compact), (1680, 780));
    }

    #[test]
    fn minimum_window_collapses_on_a_short_screen() {
        // 1080p@200%：逻辑工作区 960×508 → 最小窗口比 960×640 矮，也窄（W-32）。
        let area = work(960, 508);
        assert_eq!(min_window(area, Tier::Compact), (928, 460));
        // 1366×768@125%：逻辑工作区 1092×582 → 高度收敛，宽度仍是 960。
        assert_eq!(min_window(work(1092, 582), Tier::Compact), (960, 534));
        // 正常屏上就是 960×640。
        assert_eq!(min_window(work(1920, 1048), Tier::Compact), (960, 640));
    }

    #[test]
    fn recommendation_never_exceeds_the_work_area() {
        // 验收条目：「三档工作区下窗口不超工作区」——把边界与极端值一起钉住。
        for area in [
            work(960, 508),
            work(1092, 582),
            work(1366, 736),
            work(1536, 838),
            work(1920, 1048),
            work(2048, 1120),
            work(2560, 1408),
            work(3840, 2120),
            work(200, 200),
        ] {
            let (width, height) = recommended(area, Tier::Compact);
            assert!(
                width <= area.width - 32,
                "宽度 {width} 超出工作区 {} 的上限",
                area.width
            );
            assert!(
                height <= area.height - 48,
                "高度 {height} 超出工作区 {} 的上限",
                area.height
            );
            assert!(width > 0 && height > 0, "尺寸必须为正：{area:?}");
        }
        // H 小于最小窗口高度（逻辑 960×540 屏）：高度贴住 H-48 而不是 640。
        let short = work(960, 508);
        assert_eq!(recommended(short, Tier::Compact), (928, 460));
        assert_eq!(min_window(short, Tier::Compact).1, 460);
    }

    #[test]
    fn tier_scales_recommendation_and_panels() {
        // §5.3 的三档表：基准 × T 再钳到工作区。
        let area = work(1920, 1048);
        assert_eq!(recommended(area, Tier::Comfortable), (1750, 1000));
        assert_eq!(recommended(area, Tier::Large), (1888, 1000));
        assert_eq!(
            recommended(work(2048, 1120), Tier::Comfortable),
            (1750, 1072)
        );
        assert_eq!(recommended(work(2048, 1120), Tier::Large), (2016, 1072));
        assert_eq!(
            recommended(work(2560, 1408), Tier::Comfortable),
            (2100, 1250)
        );
        assert_eq!(recommended(work(2560, 1408), Tier::Large), (2520, 1360));
        // 最小窗口直接取 `metrics.toml` 的 960 / 1200 / 1440，不再用倍率算。
        assert_eq!(min_window(area, Tier::Comfortable), (1200, 800));
        assert_eq!(min_window(area, Tier::Large), (1440, 960));
        let panels = panel_bounds(Tier::Comfortable);
        assert_eq!(panels.sidebar_min, 225);
        assert_eq!(panels.inspector_max, 650);
        let large = panel_bounds(Tier::Large);
        assert_eq!((large.sidebar_min, large.sidebar_max), (270, 630));
        assert_eq!((large.inspector_min, large.inspector_max), (360, 780));
    }

    #[test]
    fn tier_availability_uses_the_unclamped_minimum_window() {
        // §2.2：1080p@150%（逻辑 1280×698）只有 compact 可用；菜单里另外两项禁用。
        let short = work(1280, 698);
        assert!(available(Tier::Compact, short));
        assert!(!available(Tier::Comfortable, short));
        assert!(!available(Tier::Large, short));
        // 1366×768@125%（逻辑 1092×582）同理。
        assert!(!available(Tier::Comfortable, work(1092, 582)));
        // 1920×1048 三档都可用（large 需要 1440×960）。
        assert!(available(Tier::Comfortable, work(1920, 1048)));
        assert!(available(Tier::Large, work(1920, 1048)));
        // 已保存档不可用时回退到下一个可用档（不静默钳到 100% 以外的档）。
        assert_eq!(
            first_available_from(Tier::Large, short),
            Some(Tier::Compact)
        );
        // 1536×838 按 §2.2 的公式（高 ≥ 800）**可以**用 125% 档——§5.3 的表格把这一行
        // 标成「不可用」，与 §2.2 冲突；实现按 §2.2 的显式判定规则（见汇报的待定项）。
        assert_eq!(
            first_available_from(Tier::Comfortable, work(1536, 838)),
            Some(Tier::Comfortable)
        );
        // 1366×768@100%（逻辑 1366×736）：高 736 < 800，125% 不可用。
        assert_eq!(
            first_available_from(Tier::Comfortable, work(1366, 736)),
            Some(Tier::Compact)
        );
        assert_eq!(
            first_available_from(Tier::Large, work(1920, 1048)),
            Some(Tier::Large)
        );
    }
}
