//! 六区域外壳的布局算术与密度档度量（`docs/ui-specification/workspace-model.md` §1、§2.1）。
//!
//! 尺寸的**唯一真值**是 `docs/ui-specification/metrics.toml`：本模块用
//! `include_str!` 把它编进二进制，启动后解析一次（`toml` 依赖）。三档
//! （`compact` 100% / `comfortable` 125% / `large` 150%）的每一个值都直接读表里
//! 写死的整数，**不做「基准 × 缩放」**——三端取整规则不同，算出来的 1px 偏差肉眼
//! 发现不了（规范 §3.1）。
//!
//! ```text
//! contentHeight = windowHeight - headerHeight - toolbarHeight - statusHeight
//! contentWidth  = windowWidth  - sidebarWidth
//! gridWidth     = contentWidth - gridPaddingStart - gridPaddingEnd
//!                 - [inspectorGap + inspectorWidth]   # 面板折叠时两项一起收回
//! gridHeight    = contentHeight - tabStripHeight - tableActionsHeight
//! ```
//!
//! 100% 档、1200×800 基准下 `gridWidth`：折叠 942、面板 280 时 652、面板 320 时 612
//! （与规范表格逐字一致，见本模块的测试）。
//!
//! 纯算术放在这里是为了能在没有 GTK 显示服务器的环境里单测；运行期切换档位时对
//! 已建成控件重新落数字的逻辑在 [`crate::density`]。

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::Deserialize;

/// `metrics.toml` 原文（唯一真值，编进二进制；路径相对本文件）。
const METRICS_TOML: &str = include_str!("../../../docs/ui-specification/metrics.toml");

/// 窗口外壳（区域 01）高度：`AdwHeaderBar` 40 + 菜单行 24（100% 档）。
///
/// Linux 不设菜单行（ADR 0023），但外壳高度按规范保持 64，工具条与状态栏的位置
/// 才不会因平台而异——菜单行走 headerbar 主菜单。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const HEADER_HEIGHT: i32 = 64;

/// 主工具条（区域 02）高度（100% 档）。
///
/// v0.2.3.3：76 → 44。工具条只放一行 24 高的按钮，44 = 24 + 上下各 10 的呼吸；
/// 省下的 32 px 全部还给数据区（用户要求「让目光更聚焦在数据区」）。
pub const TOOLBAR_HEIGHT: i32 = 44;

/// 状态栏（区域 05）高度（100% 档）：两行各 24（24 × 2 = 48），不再另加上下留白。
///
/// v0.2.4：56 → 48。搜索框搬到左边沟后沟里只放一个 24 高的入口，原来多出来的
/// 8px 留白没有内容可撑，全部还给内容区（1200×800 下 +8）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const STATUS_HEIGHT: i32 = 48;

/// 窗口最小宽度（100% 档，workspace-model §1）。
///
/// 960×640 是 100% 档的**强制下限**：`window_size::min_window` 按档位取
/// `min(960·T, 工作区宽-32)` 后交给 `GtkWidget::set_size_request`，GTK 会把它转成
/// toplevel 的最小尺寸（Wayland 走 `xdg_toplevel.set_min_size`、X11 走 WM normal
/// hints），窗口管理器不允许再缩小。布局算术在这一档以下没有保证。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const MIN_WINDOW_WIDTH: i32 = 960;

/// 窗口最小高度（100% 档），与 [`MIN_WINDOW_WIDTH`] 同一条强制路径。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const MIN_WINDOW_HEIGHT: i32 = 640;

/// 外壳 + 工具条的合计高度（100% 档）。
pub const CHROME_HEIGHT: i32 = HEADER_HEIGHT + TOOLBAR_HEIGHT;

/// 侧栏（区域 03）默认宽度（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const SIDEBAR_WIDTH: i32 = 237;

/// 侧栏可拖拽下限（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const SIDEBAR_MIN: i32 = 180;

/// 侧栏可拖拽上限（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const SIDEBAR_MAX: i32 = 420;

/// 信息面板（区域 06）默认宽度（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const INSPECTOR_WIDTH: i32 = 280;

/// 信息面板可拖拽下限（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const INSPECTOR_MIN: i32 = 240;

/// 信息面板可拖拽上限（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const INSPECTOR_MAX: i32 = 520;

/// 标签条（区域 04 顶部）高度（100% 档，workspace-model §2.1 的 `tabStrip`）。
pub const TAB_STRIP_HEIGHT: i32 = 24;

/// 表格工具条（区域 04 中部）高度（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const TABLE_ACTIONS_HEIGHT: i32 = 46;

/// 表格左内边距（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const GRID_PADDING_START: i32 = 18;

/// 表格右内边距（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const GRID_PADDING_END: i32 = 3;

/// 表格与信息面板之间的间隙（100% 档；面板折叠时一并收回）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const INSPECTOR_GAP: i32 = 10;

/// 树行高（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const ROW_HEIGHT_TREE: i32 = 24;

/// 数据行高（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const ROW_HEIGHT_DATA: i32 = 32;

/// 界面字号（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const FONT_UI: i32 = 12;

/// 标题字号（100% 档；semibold 由 CSS 给出）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const FONT_TITLE: i32 = 13;

/// 状态栏字号（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const FONT_STATUS: i32 = 11;

/// 数据网格单元格字号（100% 档）。
///
/// 不能低于 13px：GTK4 在 Linux 上以 12px 渲染 CJK 单元格时，会把「三 / 五 / 张」
/// 这类字的笔画裁掉。13px 起完整，但在 1x 屏幕上 Regular 横画仍容易发虚；
/// 单元格统一用 Medium（500）加实笔画，字号保持 13px。32px 数据行仍能容纳。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const FONT_DATA_CELL: i32 = 13;

/// 信息面板正文（DDL 与「表信息」两页）字号（100% 档）。
///
/// 与 `FONT_DATA_CELL` **解耦**：面板是次要信息，视觉重心在数据网格，所以正文比网格
/// 小得多（10px），字重也不加粗（400）——面板不该和表格抢注意力。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const FONT_PANEL: i32 = 10;

/// 可焦点控件的最小点击区（100% 档，§11：紧凑化不得小于它）。
pub const MIN_TARGET: i32 = 24;

/// 图标的绘制边长（100% 档）：**所有**图标按钮都以它为唯一尺寸。
///
/// v0.2.3.3 曾按墨迹分三档（窄字形 +2、环形 −2）追求「光学等重」，结果同一排里量出来
/// 的长宽不一致，看着就是一排大小不一的图标。现在改成**几何一致**：每个图标都画在
/// `icon_box × icon_box` 里——长宽都不超过基准，且两维都与基准相等；形状差异由图标
/// 本身承担，不再靠改尺寸补偿。三档取值 16 / 20 / 24（`metrics.toml`）。
pub const ICON_BOX: i32 = 16;

/// 图标按钮的命中区宽度（100% 档；统一，不按单个图标调）。
pub const ICON_BUTTON_WIDTH: i32 = 28;

/// 图标按钮的命中区高度（100% 档）：等于 §11 的最小点击区。
pub const ICON_BUTTON_HEIGHT: i32 = MIN_TARGET;

/// 表格工具条**状态圆点**的视觉直径（100% 档，px）。
///
/// 口径是「漏斗图标的一半」：`sqarvo-filter-symbolic` 画在 16 × 16 的图标盒里，墨迹
/// 实测 12 × 11；一半是 6，图标盒的一半是 8，取 7 —— 两种读法都在「约一半」以内。
///
/// 视觉小、命中区不小：圆点挂在一个 [`MIN_TARGET`] × [`MIN_TARGET`] 的扁平按钮里
/// （§11：紧凑化不得把可交互控件压到 24 以下），鼠标悬停读到 tooltip、读屏读到同一句话。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const STATUS_DOT_DIAMETER: i32 = 7;

// ---------------------------------------------------------------------------
// 密度档与度量门面（#159 / #160）
// ---------------------------------------------------------------------------

/// 界面密度档（`metrics.toml` 的 `available_tiers`，规则见
/// `docs/ui-specification/scaling-and-window-sizing.md` §2）。
///
/// 三档固定，不许再增（§3.3）：要新增必须先满足九条不变量并回填 `metrics.toml`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Tier {
    /// 紧凑（100%）：默认档。
    #[default]
    Compact,
    /// 舒适（125%）。
    Comfortable,
    /// 大（150%）。
    Large,
}

impl Tier {
    /// 全部档位，从密到松（菜单顺序、升降档、回退都读这一份）。
    pub const ALL: [Self; 3] = [Self::Compact, Self::Comfortable, Self::Large];

    /// `metrics.toml` / `window.json` 里的档位 id。
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Comfortable => "comfortable",
            Self::Large => "large",
        }
    }

    /// 菜单与日志里的中文标签。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "紧凑（100%）",
            Self::Comfortable => "舒适（125%）",
            Self::Large => "大（150%）",
        }
    }

    /// 百分比后缀（日志用，如 `100%`）。
    #[must_use]
    pub fn percent(self) -> &'static str {
        match self {
            Self::Compact => "100%",
            Self::Comfortable => "125%",
            Self::Large => "150%",
        }
    }

    /// 档位倍率（**只**用于规范 §5.3 的窗口尺寸公式与宽度归一化；控件尺寸一律读
    /// `metrics.toml` 的整数）。
    #[must_use]
    pub fn scale(self) -> f64 {
        match self {
            Self::Compact => 1.0,
            Self::Comfortable => 1.25,
            Self::Large => 1.5,
        }
    }

    /// 解析档位 id（`--tier`、`window.json`）；未知值返回 `None`，由调用方决定报错
    /// 还是回退默认档。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim() {
            "compact" => Some(Self::Compact),
            "comfortable" => Some(Self::Comfortable),
            "large" => Some(Self::Large),
            _ => None,
        }
    }

    /// 升一档（到顶返回 `None`）。
    #[must_use]
    pub fn up(self) -> Option<Self> {
        match self {
            Self::Compact => Some(Self::Comfortable),
            Self::Comfortable => Some(Self::Large),
            Self::Large => None,
        }
    }

    /// 降一档（到底返回 `None`）。
    #[must_use]
    pub fn down(self) -> Option<Self> {
        match self {
            Self::Compact => None,
            Self::Comfortable => Some(Self::Compact),
            Self::Large => Some(Self::Comfortable),
        }
    }

    fn index(self) -> u8 {
        match self {
            Self::Compact => 0,
            Self::Comfortable => 1,
            Self::Large => 2,
        }
    }

    fn from_index(index: u8) -> Self {
        match index {
            1 => Self::Comfortable,
            2 => Self::Large,
            _ => Self::Compact,
        }
    }
}

/// 当前档位（进程级）：启动时按 `--tier` / `window.json` 定，切档由
/// [`crate::density`] 写入。默认 `compact`。
static CURRENT_TIER: AtomicU8 = AtomicU8::new(0);

/// 当前生效的密度档。
#[must_use]
pub fn current_tier() -> Tier {
    Tier::from_index(CURRENT_TIER.load(Ordering::Relaxed))
}

/// 设置当前密度档。**只**应由 [`crate::density`] 调用：它还要重载 CSS、重排控件、
/// 重算窗口；单独改这里会让三者不一致。
pub fn set_current_tier(tier: Tier) {
    CURRENT_TIER.store(tier.index(), Ordering::Relaxed);
}

/// 间距刻度：界面里的留白**只**从这几档取，不再写「随手一个 6px」。
///
/// 取值是 [`Metrics`] 的 `gap_*` 字段（100% 档 0 / 2 / 4 / 6 / 8 / 10 / 12 / 16 / 18，
/// 以及 18 = 页面 / 对话框内容内边距）。`metrics.toml` 没有 gap 阶梯，三档取值按
/// 档位倍率取整（见 [`Metrics::for_tier`] 的注释），语义（哪一档用在哪儿）不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    /// 0：贴着排（列表行内、无间距的盒子）。
    None,
    /// 2：最小的一档（行内左右、字形的光学补偿）。
    Xxs,
    /// 4：小间距（行内左右、徽标内距）。
    Xs,
    /// 6：紧凑间距（控件之间的默认值）。
    Sm,
    /// 8：中等间距（成组的控件之间）。
    Md,
    /// 10：较宽间距（区块之间）。
    Lg,
    /// 12：宽间距（对话框 / 页面内容的左右内边距）。
    Xl,
    /// 14：动作按钮的横向内距。
    Xxl,
    /// 18：页面 / 对话框内容的整圈内边距（与 2px 步进分开的一档）。
    Page,
}

impl Gap {
    /// 全部刻度（重排时按序映射旧值 → 新值）。
    pub const ALL: [Self; 9] = [
        Self::None,
        Self::Xxs,
        Self::Xs,
        Self::Sm,
        Self::Md,
        Self::Lg,
        Self::Xl,
        Self::Xxl,
        Self::Page,
    ];

    /// 100% 档的基准值（间距阶梯的语义来源）。
    #[must_use]
    pub fn base_px(self) -> i32 {
        match self {
            Self::None => 0,
            Self::Xxs => 2,
            Self::Xs => 4,
            Self::Sm => 6,
            Self::Md => 8,
            Self::Lg => 10,
            Self::Xl => 12,
            Self::Xxl => 14,
            Self::Page => 18,
        }
    }
}

/// 一个档位的全部布局度量（[`metrics`] 的返回值）。
///
/// 字段名与 `docs/ui-specification/metrics.toml` 同名同义；数值直接从 TOML 来。
/// 表里没有的只有两类，集中在本结构末尾：
///
/// 1. 间距阶梯 `gap_*`（0 / 2 / 4 / 6 / 8 / 10 / 12 / 14 / 18）：按档位倍率取整；
/// 2. CSS 专用度量（圆角、表格外框线宽、分页按钮最小宽度等）：同样按倍率取整，
///    **1px 描边与 2px 表格外框线宽不缩放**（§3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    // ---- metrics.toml 的同名项 ----
    pub header_height: i32,
    pub toolbar_height: i32,
    pub status_height: i32,
    pub min_window_width: i32,
    pub min_window_height: i32,
    pub sidebar_width: i32,
    pub sidebar_min: i32,
    pub sidebar_max: i32,
    pub inspector_width: i32,
    pub inspector_min: i32,
    pub inspector_max: i32,
    pub tab_strip_height: i32,
    pub table_actions_height: i32,
    pub grid_padding_start: i32,
    pub grid_padding_end: i32,
    pub inspector_gap: i32,
    pub row_height_tree: i32,
    pub row_height_data: i32,
    pub column_header_min_height: i32,
    pub font_ui: i32,
    pub font_title: i32,
    pub font_status: i32,
    pub font_data_cell: i32,
    pub font_panel: i32,
    pub min_target: i32,
    pub icon_box: i32,
    pub icon_button_width: i32,
    pub icon_button_height: i32,
    pub status_dot_diameter: i32,
    pub toolbar_padding_h: i32,
    pub objects_min_width: i32,
    pub pager_font: i32,
    pub badge_min_height: i32,
    pub secondary_min_height: i32,
    pub radius_sm: i32,
    pub radius_md: i32,
    pub tree_arrow_box: i32,
    pub row_number_width: i32,
    pub column_width: i32,
    pub filter_panel_width: i32,
    pub filter_column_width: i32,

    // ---- 间距阶梯（TOML 无同名项，按档位倍率取整） ----
    pub gap_none: i32,
    pub gap_xxs: i32,
    pub gap_xs: i32,
    pub gap_sm: i32,
    pub gap_md: i32,
    pub gap_lg: i32,
    pub gap_xl: i32,
    pub gap_xxl: i32,
    pub gap_page: i32,

    // ---- CSS 专用度量（TOML 无同名项） ----
    /// 表格外框线宽：2px 在**所有档位**保持 2px（描边不随密度变，§3.2 的同一条理由）。
    pub frame_border_width: i32,
    /// 大圆角（列信息 / 事务卡片的浮层）：12 / 15 / 18。
    pub radius_lg: i32,
    /// 饱和圆角：给圆点用，等于半个盒子（任何档位都不需要更大）。
    pub pill_radius: i32,
    /// 分页器按钮最小宽度：26 / 33 / 39。
    pub pager_button_min_width: i32,
    /// 筛选面板里的按钮最小高度：26 / 33 / 39。
    pub fs_button_min_height: i32,
    /// 筛选面板「应用」按钮最小高度：28 / 35 / 42。
    pub fs_action_min_height: i32,
    /// 空态占位图标的绘制边长：48 / 60 / 72。
    pub empty_icon_px: i32,
    /// 加载 spinner 的请求边长：32 / 40 / 48。
    pub spinner_px: i32,
}

impl Metrics {
    /// 取一档的度量（`metrics.toml` 解析一次后缓存）。
    #[must_use]
    pub fn for_tier(tier: Tier) -> Self {
        catalog()[tier.index() as usize]
    }

    /// 间距刻度的像素值。
    #[must_use]
    pub fn gap_px(&self, step: Gap) -> i32 {
        match step {
            Gap::None => self.gap_none,
            Gap::Xxs => self.gap_xxs,
            Gap::Xs => self.gap_xs,
            Gap::Sm => self.gap_sm,
            Gap::Md => self.gap_md,
            Gap::Lg => self.gap_lg,
            Gap::Xl => self.gap_xl,
            Gap::Xxl => self.gap_xxl,
            Gap::Page => self.gap_page,
        }
    }

    /// 内容区高度：两侧在状态栏之上、工具条之下。
    #[must_use]
    pub fn content_height(&self, window_height: i32) -> i32 {
        window_height - self.header_height - self.toolbar_height - self.status_height
    }

    /// 内容区宽度：右侧工作区（不含侧栏）。
    #[must_use]
    pub fn content_width(&self, window_width: i32, sidebar_width: i32) -> i32 {
        window_width - sidebar_width
    }

    /// 数据网格宽度；`inspector_width` 为 `None` 表示信息面板折叠。
    #[must_use]
    pub fn grid_width(
        &self,
        window_width: i32,
        sidebar_width: i32,
        inspector_width: Option<i32>,
    ) -> i32 {
        let base = self.content_width(window_width, sidebar_width)
            - self.grid_padding_start
            - self.grid_padding_end;
        match inspector_width {
            Some(width) => base - self.inspector_gap - width,
            None => base,
        }
    }

    /// 数据网格高度：标签条与表格工具条之下。
    #[must_use]
    pub fn grid_height(&self, window_height: i32) -> i32 {
        self.content_height(window_height) - self.tab_strip_height - self.table_actions_height
    }

    /// 状态栏顶边 y 坐标（用于检查表格不压在状态栏上）。
    #[must_use]
    pub fn status_y(&self, window_height: i32) -> i32 {
        window_height - self.status_height
    }

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
}

/// 按档位倍率取整（**只**用于 TOML 没有同名项的间距 / 圆角 / 装饰度量）。
fn scaled(base: i32, scale: f64) -> i32 {
    (f64::from(base) * scale).round() as i32
}

/// 当前档位的度量。界面里要留白 / 固定尺寸时从这里取，比如
/// `widget.set_margin_start(gap(Gap::Page))`。
#[must_use]
pub fn metrics() -> Metrics {
    Metrics::for_tier(current_tier())
}

/// 当前档位下某一档间距刻度的像素值（[`metrics`] 的简写）。
#[must_use]
pub fn gap(step: Gap) -> i32 {
    metrics().gap_px(step)
}

/// 估算的「compact 档 → 目标档」间距值（重排已建控件时按旧值查表用）。
///
/// 输入是**旧档位**的间距像素值；落在阶梯上就返回新档位同一刻度的值，否则 `None`
/// （不是间距，别动它）。这是 [`crate::density`] 重排的关键：所有 `set_margin_*`
/// 与 `Box` spacing 都由 [`Gap`] 产生，因此旧值一定能在阶梯上找到。
#[must_use]
pub fn remap_gap(old: &Metrics, new: &Metrics, value: i32) -> Option<i32> {
    Gap::ALL
        .iter()
        .find(|step| old.gap_px(**step) == value)
        .map(|step| new.gap_px(*step))
}

// ---------------------------------------------------------------------------
// metrics.toml 解析
// ---------------------------------------------------------------------------

/// TOML 的原始结构：只做反序列化，字段与文件一一对应。
#[derive(Debug, Deserialize)]
struct CatalogFile {
    #[serde(default)]
    meta: MetaRaw,
    tier: BTreeMap<String, RawTier>,
}

#[derive(Debug, Default, Deserialize)]
struct MetaRaw {
    #[serde(default)]
    default_tier: String,
}

#[derive(Debug, Deserialize)]
struct RawTier {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    label: String,
    scale: f64,
    header_height: i32,
    toolbar_height: i32,
    status_height: i32,
    min_window_width: i32,
    min_window_height: i32,
    sidebar_width: i32,
    sidebar_min: i32,
    sidebar_max: i32,
    inspector_width: i32,
    inspector_min: i32,
    inspector_max: i32,
    tab_strip_height: i32,
    table_actions_height: i32,
    grid_padding_start: i32,
    grid_padding_end: i32,
    inspector_gap: i32,
    row_height_tree: i32,
    row_height_data: i32,
    column_header_min_height: i32,
    font_ui: i32,
    font_title: i32,
    font_status: i32,
    font_data_cell: i32,
    font_panel: i32,
    min_target: i32,
    icon_box: i32,
    icon_button_width: i32,
    icon_button_height: i32,
    status_dot_diameter: i32,
    toolbar_padding_h: i32,
    objects_min_width: i32,
    pager_font: i32,
    badge_min_height: i32,
    secondary_min_height: i32,
    radius_sm: i32,
    radius_md: i32,
    tree_arrow_box: i32,
    row_number_width: i32,
    column_width: i32,
    filter_panel_width: i32,
    filter_column_width: i32,
}

/// 三档解析结果（顺序 = [`Tier::ALL`]）。
static CATALOG: OnceLock<[Metrics; 3]> = OnceLock::new();

fn catalog() -> &'static [Metrics; 3] {
    CATALOG.get_or_init(|| {
        let file: CatalogFile = toml::from_str(METRICS_TOML).unwrap_or_else(|error| {
            panic!("metrics.toml 无法解析（编进二进制的唯一真值）：{error}")
        });
        let default = Tier::from_id(&file.meta.default_tier).unwrap_or_default();
        assert_eq!(
            default,
            Tier::Compact,
            "metrics.toml 的 default_tier 必须是 compact（规范 §2）"
        );
        let build = |tier: Tier| {
            let raw = file
                .tier
                .get(tier.id())
                .unwrap_or_else(|| panic!("metrics.toml 缺少 [tier.{}]（三档缺一档）", tier.id()));
            Metrics::from_raw(raw)
        };
        Tier::ALL.map(build)
    })
}

impl Metrics {
    /// 把 TOML 的一档转成门面结构。表里没有的项在这里按档位倍率取整——这是**唯一**
    /// 允许出现「基准 × 缩放」的地方，且只覆盖 TOML 未收录的间距阶梯与装饰度量
    /// （规范 §3.1 的「各端只读、不计算」针对的是表里已经写死的值）。
    fn from_raw(raw: &RawTier) -> Self {
        let scale = raw.scale;
        let gap = |step: Gap| scaled(step.base_px(), scale);
        Self {
            header_height: raw.header_height,
            toolbar_height: raw.toolbar_height,
            status_height: raw.status_height,
            min_window_width: raw.min_window_width,
            min_window_height: raw.min_window_height,
            sidebar_width: raw.sidebar_width,
            sidebar_min: raw.sidebar_min,
            sidebar_max: raw.sidebar_max,
            inspector_width: raw.inspector_width,
            inspector_min: raw.inspector_min,
            inspector_max: raw.inspector_max,
            tab_strip_height: raw.tab_strip_height,
            table_actions_height: raw.table_actions_height,
            grid_padding_start: raw.grid_padding_start,
            grid_padding_end: raw.grid_padding_end,
            inspector_gap: raw.inspector_gap,
            row_height_tree: raw.row_height_tree,
            row_height_data: raw.row_height_data,
            column_header_min_height: raw.column_header_min_height,
            font_ui: raw.font_ui,
            font_title: raw.font_title,
            font_status: raw.font_status,
            font_data_cell: raw.font_data_cell,
            font_panel: raw.font_panel,
            min_target: raw.min_target,
            icon_box: raw.icon_box,
            icon_button_width: raw.icon_button_width,
            icon_button_height: raw.icon_button_height,
            status_dot_diameter: raw.status_dot_diameter,
            toolbar_padding_h: raw.toolbar_padding_h,
            objects_min_width: raw.objects_min_width,
            pager_font: raw.pager_font,
            badge_min_height: raw.badge_min_height,
            secondary_min_height: raw.secondary_min_height,
            radius_sm: raw.radius_sm,
            radius_md: raw.radius_md,
            tree_arrow_box: raw.tree_arrow_box,
            row_number_width: raw.row_number_width,
            column_width: raw.column_width,
            filter_panel_width: raw.filter_panel_width,
            filter_column_width: raw.filter_column_width,
            gap_none: gap(Gap::None),
            gap_xxs: gap(Gap::Xxs),
            gap_xs: gap(Gap::Xs),
            gap_sm: gap(Gap::Sm),
            gap_md: gap(Gap::Md),
            gap_lg: gap(Gap::Lg),
            gap_xl: gap(Gap::Xl),
            gap_xxl: gap(Gap::Xxl),
            gap_page: gap(Gap::Page),
            // 描边类不随档位（§3.2）：1px 分隔线由 CSS 常量固定，2px 外框同理。
            frame_border_width: 2,
            radius_lg: scaled(12, scale),
            pill_radius: 99,
            pager_button_min_width: scaled(26, scale),
            fs_button_min_height: scaled(26, scale),
            fs_action_min_height: scaled(28, scale),
            empty_icon_px: scaled(48, scale),
            spinner_px: scaled(32, scale),
        }
    }
}

// ---------------------------------------------------------------------------
// 当前档位下的布局算术（自由函数：调用点不必先取 Metrics）
// ---------------------------------------------------------------------------

/// 内容区高度：两侧在状态栏之上、工具条之下。
#[must_use]
pub fn content_height(window_height: i32) -> i32 {
    metrics().content_height(window_height)
}

/// 内容区宽度：右侧工作区（不含侧栏）。
#[must_use]
pub fn content_width(window_width: i32, sidebar_width: i32) -> i32 {
    metrics().content_width(window_width, sidebar_width)
}

/// 数据网格宽度；`inspector_width` 为 `None` 表示信息面板折叠。
#[must_use]
pub fn grid_width(window_width: i32, sidebar_width: i32, inspector_width: Option<i32>) -> i32 {
    metrics().grid_width(window_width, sidebar_width, inspector_width)
}

/// 数据网格高度：标签条与表格工具条之下。
#[must_use]
pub fn grid_height(window_height: i32) -> i32 {
    metrics().grid_height(window_height)
}

/// 状态栏顶边 y 坐标（用于检查表格不压在状态栏上）。
#[must_use]
pub fn status_y(window_height: i32) -> i32 {
    metrics().status_y(window_height)
}

/// 把侧栏宽度钳制到当前档位的可拖拽区间。
#[must_use]
pub fn clamp_sidebar(width: i32) -> i32 {
    metrics().clamp_sidebar(width)
}

/// 把信息面板宽度钳制到当前档位的可拖拽区间。
#[must_use]
pub fn clamp_inspector(width: i32) -> i32 {
    metrics().clamp_inspector(width)
}

/// 由 `GtkPaned` 的位置反推右侧面板宽度并钳制（当前档位）。
///
/// 运行期走 [`crate::window_size::PanelBounds::inspector_width_from_position`]（同一算式
/// 按档位缩放）；这份实现留给布局算术的单测与对照。
#[allow(dead_code)]
#[must_use]
pub fn inspector_width_from_position(pane_width: i32, position: i32) -> i32 {
    clamp_inspector(pane_width - position - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW_1200: (i32, i32) = (1200, 800);
    const WINDOW_960: (i32, i32) = (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT);

    #[test]
    fn minimum_window_is_960x640() {
        // 规范 §1 的 100% 档最小窗口：`ui::activate` 用它强制首帧下限，单测在这里
        // 钉死数值，防止有人只改一边（窗口与布局算术读同一份常量）。
        assert_eq!((MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT), (960, 640));
    }

    #[test]
    fn content_height_matches_the_spec_for_both_target_sizes() {
        assert_eq!(content_height(WINDOW_1200.1), 644);
        assert_eq!(content_height(WINDOW_960.1), 484);
    }

    #[test]
    fn header_toolbar_and_status_bar_add_up_to_the_chrome() {
        assert_eq!(HEADER_HEIGHT + TOOLBAR_HEIGHT, 108);
        assert_eq!(CHROME_HEIGHT + STATUS_HEIGHT, 156);
    }

    #[test]
    fn grid_width_matches_the_spec_table_at_1200x800() {
        // 折叠 942 / 280 时 652 / 320 时 612 —— 规范 §2.1 的表格。
        assert_eq!(grid_width(1200, 237, None), 942);
        assert_eq!(grid_width(1200, 237, Some(280)), 652);
        assert_eq!(grid_width(1200, 237, Some(320)), 612);
    }

    #[test]
    fn collapsing_the_inspector_reclaims_the_gap_too() {
        assert_eq!(
            grid_width(1200, 237, None),
            grid_width(1200, 237, Some(280)) + 280 + INSPECTOR_GAP
        );
    }

    #[test]
    fn grid_stays_positive_at_960x640_even_with_the_widest_inspector() {
        assert_eq!(grid_width(960, 237, Some(280)), 412);
        // 侧栏拖到最窄、面板拖到最宽：仍然必须给表格留出正宽度，
        // 否则列会被静默裁掉而不是横向滚动。
        assert_eq!(grid_width(960, SIDEBAR_MIN, Some(INSPECTOR_MAX)), 229);
        assert!(grid_width(960, 237, Some(INSPECTOR_MAX)) > 0);
    }

    #[test]
    fn grid_height_and_status_position_do_not_overlap() {
        for window in [WINDOW_1200, WINDOW_960] {
            let grid = grid_height(window.1);
            let end_of_grid = HEADER_HEIGHT + TOOLBAR_HEIGHT + grid;
            assert!(
                end_of_grid <= status_y(window.1),
                "表格底边 {end_of_grid} 压在状态栏顶部 {} 上",
                status_y(window.1)
            );
            assert_eq!(status_y(window.1), window.1 - 48);
        }
        // 644 − 24（标签条） − 46（一行工具条 + 上下留白）= 574；960 档 484 − 24 − 46 = 414。
        assert_eq!(grid_height(800), 574);
        assert_eq!(grid_height(640), 414);
    }

    #[test]
    fn sidebar_and_inspector_clamp_to_their_drag_ranges() {
        assert_eq!(clamp_sidebar(100), 180);
        assert_eq!(clamp_sidebar(237), 237);
        assert_eq!(clamp_sidebar(600), 420);
        assert_eq!(clamp_inspector(100), 240);
        assert_eq!(clamp_inspector(280), 280);
        assert_eq!(clamp_inspector(900), 520);
    }

    #[test]
    fn inspector_width_from_position_round_trips() {
        // 1200 宽的工作区、面板 280：分隔条位置 = 1200 - 280 - 1。
        assert_eq!(inspector_width_from_position(1200, 919), 280);
        // 拖到上限外：钳到 520。
        assert_eq!(inspector_width_from_position(1200, 400), 520);
        // 拖到下限外：钳到 240。
        assert_eq!(inspector_width_from_position(1200, 1100), 240);
    }

    /// `metrics.toml` 的 compact 档必须与 `layout.rs` 的 100% 常量逐一相同：
    /// 这是「compact 档视觉零变化」在代码侧的硬证据，也是常量与真值不许漂移的守卫。
    #[test]
    fn parsed_compact_tier_matches_the_100_percent_constants() {
        let compact = Metrics::for_tier(Tier::Compact);
        assert_eq!(compact.header_height, HEADER_HEIGHT);
        assert_eq!(compact.toolbar_height, TOOLBAR_HEIGHT);
        assert_eq!(compact.status_height, STATUS_HEIGHT);
        assert_eq!(compact.min_window_width, MIN_WINDOW_WIDTH);
        assert_eq!(compact.min_window_height, MIN_WINDOW_HEIGHT);
        assert_eq!(compact.sidebar_width, SIDEBAR_WIDTH);
        assert_eq!(compact.sidebar_min, SIDEBAR_MIN);
        assert_eq!(compact.sidebar_max, SIDEBAR_MAX);
        assert_eq!(compact.inspector_width, INSPECTOR_WIDTH);
        assert_eq!(compact.inspector_min, INSPECTOR_MIN);
        assert_eq!(compact.inspector_max, INSPECTOR_MAX);
        assert_eq!(compact.tab_strip_height, TAB_STRIP_HEIGHT);
        assert_eq!(compact.table_actions_height, TABLE_ACTIONS_HEIGHT);
        assert_eq!(compact.grid_padding_start, GRID_PADDING_START);
        assert_eq!(compact.grid_padding_end, GRID_PADDING_END);
        assert_eq!(compact.inspector_gap, INSPECTOR_GAP);
        assert_eq!(compact.row_height_tree, ROW_HEIGHT_TREE);
        assert_eq!(compact.row_height_data, ROW_HEIGHT_DATA);
        assert_eq!(
            (compact.font_ui, compact.font_title, compact.font_status),
            (FONT_UI, FONT_TITLE, FONT_STATUS)
        );
        assert_eq!(
            (compact.font_data_cell, compact.font_panel),
            (FONT_DATA_CELL, FONT_PANEL)
        );
        assert_eq!(compact.min_target, MIN_TARGET);
        assert_eq!(compact.icon_box, ICON_BOX);
        assert_eq!(
            (compact.icon_button_width, compact.icon_button_height),
            (ICON_BUTTON_WIDTH, ICON_BUTTON_HEIGHT)
        );
        assert_eq!(compact.status_dot_diameter, STATUS_DOT_DIAMETER);
        // 间距刻度：0 / 2 / 4 / 6 / 8 / 10 / 12 / 14，页面档 18（100% 档不变）。
        assert_eq!(
            Gap::ALL.map(|step| compact.gap_px(step)),
            [0, 2, 4, 6, 8, 10, 12, 14, 18]
        );
        // 与运行期门面一致：默认档（compact）下 `metrics()` 就是这一组值。
        assert_eq!(current_tier(), Tier::Compact);
        assert_eq!(metrics(), compact);
    }

    /// 规范 §3.3 的九条不变量：三档都必须成立，数字取 `metrics.toml` 的实际值。
    #[test]
    fn every_tier_keeps_the_nine_density_invariants() {
        for tier in Tier::ALL {
            let m = Metrics::for_tier(tier);
            let label = tier.id();
            // ① 命中区不低于最小点击区，且三者在同一档行高里对齐。
            assert_eq!(
                (m.row_height_tree, m.min_target, m.icon_button_height),
                (m.min_target, m.min_target, m.min_target),
                "{label}：树行 / 最小点击区 / 图标按钮高度必须同值"
            );
            // ② 标题层级还在。
            assert!(m.font_title > m.font_ui, "{label}：标题必须大于界面字号");
            // ③ 网格字不裁 CJK 笔画、且比界面醒目。
            assert!(
                m.font_data_cell >= 13 && m.font_data_cell >= m.font_ui,
                "{label}：单元格字号 {} 违反下限",
                m.font_data_cell
            );
            // ④ 信息面板不抢注意力。
            assert!(
                m.font_panel <= m.font_ui,
                "{label}：面板字号 {} 大于界面字号",
                m.font_panel
            );
            // ⑤ 图标两侧各留 2px。
            assert!(
                m.icon_button_width - m.icon_box >= 4,
                "{label}：图标按钮 {} - 图标盒 {} 不足 4",
                m.icon_button_width,
                m.icon_box
            );
            // ⑥ 行里装得下图标按钮。
            assert!(
                m.icon_button_height <= m.row_height_tree,
                "{label}：图标按钮高于树行"
            );
            // ⑦⑧ 默认宽度在可拖区间内（严格不等）。
            assert!(
                m.sidebar_min < m.sidebar_width && m.sidebar_width < m.sidebar_max,
                "{label}：侧栏默认 {} 不在 {}..{} 内",
                m.sidebar_width,
                m.sidebar_min,
                m.sidebar_max
            );
            assert!(
                m.inspector_min < m.inspector_width && m.inspector_width < m.inspector_max,
                "{label}：面板默认 {} 不在 {}..{} 内",
                m.inspector_width,
                m.inspector_min,
                m.inspector_max
            );
            // ⑨ 数据行比树行松。
            assert!(
                m.row_height_data > m.row_height_tree,
                "{label}：数据行不比树行高"
            );
        }
    }

    /// 三档的数字必须真的不同（否则等于没接档位）；关键几项钉住 TOML 的取值。
    #[test]
    fn the_three_tiers_have_distinct_values_from_the_toml() {
        let c = Metrics::for_tier(Tier::Compact);
        let m = Metrics::for_tier(Tier::Comfortable);
        let l = Metrics::for_tier(Tier::Large);
        assert_ne!(c, m);
        assert_ne!(m, l);
        assert_eq!(
            (c.row_height_data, m.row_height_data, l.row_height_data),
            (32, 40, 48)
        );
        assert_eq!((c.font_ui, m.font_ui, l.font_ui), (12, 15, 18));
        assert_eq!((c.icon_box, m.icon_box, l.icon_box), (16, 20, 24));
        assert_eq!(
            (c.sidebar_width, m.sidebar_width, l.sidebar_width),
            (237, 296, 356)
        );
        assert_eq!(
            (c.inspector_width, m.inspector_width, l.inspector_width),
            (280, 350, 420)
        );
        assert_eq!(
            (c.min_window_width, m.min_window_width, l.min_window_width),
            (960, 1200, 1440)
        );
        assert_eq!(
            (
                c.min_window_height,
                m.min_window_height,
                l.min_window_height
            ),
            (640, 800, 960)
        );
        // 间距阶梯随档位放大，但语义（哪一档用在哪）不变：100% 档 18 → 125% 档 23。
        assert_eq!(
            (
                c.gap_px(Gap::Page),
                m.gap_px(Gap::Page),
                l.gap_px(Gap::Page)
            ),
            (18, 23, 27)
        );
        assert_eq!(
            (c.gap_px(Gap::Xxl), m.gap_px(Gap::Xxl), l.gap_px(Gap::Xxl)),
            (14, 18, 21)
        );
    }

    #[test]
    fn remap_gap_moves_values_between_ladders_and_ignores_others() {
        let c = Metrics::for_tier(Tier::Compact);
        let l = Metrics::for_tier(Tier::Large);
        assert_eq!(remap_gap(&c, &l, 0), Some(0));
        assert_eq!(remap_gap(&c, &l, 2), Some(3));
        assert_eq!(remap_gap(&c, &l, 18), Some(27));
        // 1px 描边 / 2px 外框不在阶梯上：保持原值。
        assert_eq!(remap_gap(&c, &l, 1), None);
        assert_eq!(remap_gap(&c, &l, 2 + 2), Some(6));
        assert_eq!(remap_gap(&l, &c, 27), Some(18));
    }

    #[test]
    fn compact_metrics_keep_the_24px_target_floor() {
        // 常量约束写成编译期断言：改字号/行高时先在这里失败，而不是靠肉眼发现。
        const { assert!(ROW_HEIGHT_TREE >= MIN_TARGET) };
        const { assert!(ROW_HEIGHT_DATA >= MIN_TARGET) };
        const { assert!(FONT_TITLE > FONT_UI) };
        const { assert!(FONT_UI >= FONT_STATUS) };
        // 12px 会裁掉部分 CJK 底横（用户反馈 demo_users 的「三 / 五」缺笔画）。
        const { assert!(FONT_DATA_CELL >= 13) };
        const { assert!(FONT_DATA_CELL > FONT_UI) };
        // 面板正文比界面字号更小：它是次要信息，视觉重心在数据网格。
        const { assert!(FONT_PANEL <= FONT_UI) };
    }

    #[test]
    fn icon_scale_is_one_box_for_every_icon() {
        // 只有一个尺寸：图标盒 = icon_box（基准是行操作的 `+`），命中区固定。
        // 「给某个图标单独调大小」是这一版明确要消除的做法。
        const { assert!(ICON_BUTTON_WIDTH - ICON_BOX >= 4) };
        // 状态栏 24 高的行里装得下按钮高度。
        const { assert!(ICON_BUTTON_HEIGHT == MIN_TARGET) };
        const { assert!(ICON_BUTTON_HEIGHT <= ROW_HEIGHT_TREE) };
    }
}
