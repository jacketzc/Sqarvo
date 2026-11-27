//! 六区域外壳的布局算术与密度档度量（`docs/ui-specification/workspace-model.md` §1、§2.1）。
//!
//! 尺寸的**唯一真值**是 `docs/ui-specification/metrics.toml`：本模块用
//! `include_str!` 把它编进二进制，启动后解析一次（`toml` 依赖）。三档
//! （`compact` 100% / `comfortable` 125% / `large` 150%）的每一个值都直接读表里
//! 写死的整数，**不做「基准 × 缩放」**——三端取整规则不同，算出来的 1px 偏差肉眼
//! 发现不了（规范 §3.1）。
//!
//! PanelLayout reserves the central target and charges space only for visible
//! panels and their dividers. Density changes remap persistent user widths
//! without changing the saved Compact units.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::Deserialize;

/// `metrics.toml` 原文（唯一真值，编进二进制；路径相对本文件）。
const METRICS_TOML: &str = include_str!("../../../docs/ui-specification/metrics.toml");

/// 紧凑档标题栏目标高度。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const HEADER_HEIGHT: i32 = 44;

/// 主工具条（区域 02）高度（100% 档）。
///
/// v0.2.3.3：76 → 44。工具条只放一行 24 高的按钮，44 = 24 + 上下各 10 的呼吸；
/// 省下的 32 px 全部还给数据区（用户要求「让目光更聚焦在数据区」）。
#[cfg(test)]
pub const TOOLBAR_HEIGHT: i32 = 44;

/// 紧凑档全局状态栏目标高度。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const STATUS_HEIGHT: i32 = 24;

/// 紧凑档最小窗口宽度基准。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const MIN_WINDOW_WIDTH: i32 = 840;

/// 窗口最小高度（100% 档），与 [`MIN_WINDOW_WIDTH`] 同一条强制路径。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const MIN_WINDOW_HEIGHT: i32 = 600;

/// 侧栏（区域 03）默认宽度（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const SIDEBAR_WIDTH: i32 = 260;

/// 侧栏可拖拽下限（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const SIDEBAR_MIN: i32 = 180;

/// 侧栏可拖拽上限（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const SIDEBAR_MAX: i32 = 500;

/// 信息面板（区域 06）默认宽度（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const INSPECTOR_WIDTH: i32 = 280;

/// 信息面板可拖拽下限（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const INSPECTOR_MIN: i32 = 220;

/// 信息面板可拖拽上限（100% 档）。
#[allow(dead_code)] // 100% 档的规范锚点：非测试代码全部门面化后只剩测试引用
pub const INSPECTOR_MAX: i32 = 600;

/// 标签条（区域 04 顶部）高度（100% 档，workspace-model §2.1 的 `tabStrip`）。
pub const TAB_STRIP_HEIGHT: i32 = 40;

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

    /// 「视图 → 界面密度」小节条目的 catalog key（`l10n/shell.toml`，#199）。
    ///
    /// 三档菜单文字（「紧凑（100%）」等）已整体迁入 catalog，zh 与迁移前逐字节
    /// 相等；日志继续用 [`id`]（ASCII 稳定标识），不需要中文标签。
    #[must_use]
    pub fn menu_key(self) -> &'static str {
        match self {
            Self::Compact => "shell.menu.tier_compact",
            Self::Comfortable => "shell.menu.tier_comfortable",
            Self::Large => "shell.menu.tier_large",
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
/// 取值是 [`Metrics`] 的 `gap_*` 字段（100% 档 0 / 2 / 4 / 6 / 8 / 10 / 12 / 14 / 18），
/// 三档的数字都写在 `docs/ui-specification/metrics.toml` 里，代码不做倍率计算。
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
}

/// 一个档位的全部布局度量（[`metrics`] 的返回值）。
///
/// 字段名与 `docs/ui-specification/metrics.toml` 同名同义；**所有**数值都直接从 TOML 来，
/// 包括末尾的间距阶梯 `gap_*` 与 CSS 专用度量（圆角、表格外框线宽、分页按钮最小宽度等）：
/// 三档全部写死整数，代码里不做「基准 × 倍率」。原因是各语言取整规则不同（Rust / Swift
/// half-away-from-zero、C# half-to-even），同一份 26×1.25 会算成 33 与 32，三端静默漂移。
/// 例外是**1px 描边与 2px 表格外框线宽**：TOML 里三档同值，本来就不随密度变（§3.2）。
/// `query_editor_ratio` 是无量纲比值（f64，三档同值），因此本结构只派生 `PartialEq`
/// 而不派生 `Eq`（浮点没有全等关系；没有任何容器拿 `Metrics` 当键）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    // ---- metrics.toml 的同名项 ----
    pub header_height: i32,
    pub transaction_popover_top_margin: i32,
    pub toolbar_height: i32,
    pub toolbar_button_height: i32,
    pub toolbar_layout_icon: i32,
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
    pub tab_height: i32,
    pub tab_min_width: i32,
    pub tab_max_width: i32,
    pub tab_radius: i32,
    pub tab_padding_h: i32,
    pub tab_close_icon: i32,
    pub table_actions_height: i32,
    pub grid_padding_start: i32,
    pub grid_padding_end: i32,
    pub workspace_target_width: i32,
    pub workspace_target_height: i32,
    pub panel_handle: i32,
    pub panel_recovery: i32,
    pub inspector_gap: i32,
    pub row_height_tree: i32,
    pub row_height_data: i32,
    pub column_header_min_height: i32,
    pub font_ui: i32,
    pub font_title: i32,
    pub font_status: i32,
    pub font_data_cell: i32,
    pub font_panel: i32,
    /// 字重五档（不随密度缩放）：正文 400 / 单元格 500 / 菜单条目 500 / 标题 600 /
    /// 卡片与面板标题 700。
    pub font_weight_ui: i32,
    pub font_weight_cell: i32,
    /// 菜单 / 弹出层条目（`font_weight_menu`）：400 时「色」这类细撇在暗底上只剩 1px，
    /// 读成缺一笔；500 让同一笔回到 2px。
    pub font_weight_menu: i32,
    pub font_weight_title: i32,
    pub font_weight_heading: i32,
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
    pub navigation_arrow_icon: i32,
    pub navigation_indent: i32,
    pub objects_filter_height: i32,
    pub objects_filter_width: i32,
    pub objects_filter_min_width: i32,
    pub objects_filter_max_width: i32,
    pub navigation_row_height: i32,
    pub navigation_header_button: i32,
    pub navigation_header_height: i32,
    pub row_number_width: i32,
    pub column_width: i32,
    pub filter_panel_width: i32,
    pub filter_column_width: i32,
    /// 列显隐弹层的内容宽（`metrics.toml` 的 `column_panel_width`）。
    pub column_panel_width: i32,
    /// 列显隐清单的最大可见高度，超出滚动。
    pub column_list_max_height: i32,

    // ---- 间距阶梯（`metrics.toml` 的 `gap_*`，三档写死整数） ----
    pub gap_none: i32,
    pub gap_xxs: i32,
    pub gap_xs: i32,
    pub gap_sm: i32,
    pub gap_md: i32,
    pub gap_lg: i32,
    pub gap_xl: i32,
    pub gap_xxl: i32,
    pub gap_page: i32,

    // ---- CSS 专用度量（`metrics.toml` 的圆角 / 线宽 / 按钮 / 空态与加载） ----
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

    // ---- 查询标签布局（#189）：编辑器 / 结果区 / 分隔把手 ----
    /// 查询编辑器的最小高度：140 / 175 / 210。
    pub query_editor_min_height: i32,
    /// 查询结果区的最小高度：120 / 150 / 180。
    pub query_results_min_height: i32,
    /// 查询分隔把手的厚度（GtkPaned separator 的 CSS min-height）：6 / 8 / 9。
    pub query_splitter_width: i32,
    /// 编辑器占「编辑器 + 结果区」总高度的默认比例（无量纲，三档同值 0.4 = 2:3）。
    /// 只作首次显示结果区时的默认分割；用户拖动后按标签内存态保持，不持久化。
    pub query_editor_ratio: f64,
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
    mcp_dialog_width: i32,
}

#[derive(Debug, Deserialize)]
struct RawTier {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    label: String,
    /// 档位倍率。解析出来只用于「文件自洽性」检查与日志；**不参与任何尺寸计算**
    /// （三档数字都写死在表里，见 [`Metrics::from_raw`] 的注释）。
    #[allow(dead_code)]
    scale: f64,
    header_height: i32,
    transaction_popover_top_margin: i32,
    toolbar_height: i32,
    toolbar_button_height: i32,
    toolbar_layout_icon: i32,
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
    tab_height: i32,
    tab_min_width: i32,
    tab_max_width: i32,
    tab_radius: i32,
    tab_padding_h: i32,
    tab_close_icon: i32,
    table_actions_height: i32,
    grid_padding_start: i32,
    grid_padding_end: i32,
    workspace_target_width: i32,
    workspace_target_height: i32,
    panel_handle: i32,
    panel_recovery: i32,
    inspector_gap: i32,
    row_height_tree: i32,
    row_height_data: i32,
    column_header_min_height: i32,
    font_ui: i32,
    font_title: i32,
    font_status: i32,
    font_data_cell: i32,
    font_panel: i32,
    font_weight_ui: i32,
    font_weight_cell: i32,
    font_weight_menu: i32,
    font_weight_title: i32,
    font_weight_heading: i32,
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
    navigation_arrow_icon: i32,
    navigation_indent: i32,
    objects_filter_height: i32,
    objects_filter_width: i32,
    objects_filter_min_width: i32,
    objects_filter_max_width: i32,
    navigation_row_height: i32,
    navigation_header_button: i32,
    navigation_header_height: i32,
    row_number_width: i32,
    column_width: i32,
    filter_panel_width: i32,
    filter_column_width: i32,
    column_panel_width: i32,
    column_list_max_height: i32,
    gap_none: i32,
    gap_xxs: i32,
    gap_xs: i32,
    gap_sm: i32,
    gap_md: i32,
    gap_lg: i32,
    gap_xl: i32,
    gap_xxl: i32,
    gap_page: i32,
    frame_border_width: i32,
    radius_lg: i32,
    pill_radius: i32,
    pager_button_min_width: i32,
    fs_button_min_height: i32,
    fs_action_min_height: i32,
    empty_icon_px: i32,
    spinner_px: i32,
    query_editor_min_height: i32,
    query_results_min_height: i32,
    query_splitter_width: i32,
    query_editor_ratio: f64,
}

/// 三档解析结果（顺序 = [`Tier::ALL`]）。
static CATALOG: OnceLock<([Metrics; 3], i32)> = OnceLock::new();

/// Preferred logical width of the native MCP dialog.
pub fn mcp_dialog_width() -> i32 {
    parsed_catalog().1
}

fn catalog() -> &'static [Metrics; 3] {
    &parsed_catalog().0
}

fn parsed_catalog() -> &'static ([Metrics; 3], i32) {
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
        (Tier::ALL.map(build), file.meta.mcp_dialog_width)
    })
}

impl Metrics {
    /// 把 TOML 的一档转成门面结构。表里没有的项在这里按档位倍率取整——这是**唯一**
    /// 允许出现「基准 × 缩放」的地方，且只覆盖 TOML 未收录的间距阶梯与装饰度量
    /// （规范 §3.1 的「各端只读、不计算」针对的是表里已经写死的值）。
    fn from_raw(raw: &RawTier) -> Self {
        Self {
            header_height: raw.header_height,
            transaction_popover_top_margin: raw.transaction_popover_top_margin,
            toolbar_height: raw.toolbar_height,
            toolbar_button_height: raw.toolbar_button_height,
            toolbar_layout_icon: raw.toolbar_layout_icon,
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
            tab_height: raw.tab_height,
            tab_min_width: raw.tab_min_width,
            tab_max_width: raw.tab_max_width,
            tab_radius: raw.tab_radius,
            tab_padding_h: raw.tab_padding_h,
            tab_close_icon: raw.tab_close_icon,
            table_actions_height: raw.table_actions_height,
            grid_padding_start: raw.grid_padding_start,
            grid_padding_end: raw.grid_padding_end,
            workspace_target_width: raw.workspace_target_width,
            workspace_target_height: raw.workspace_target_height,
            panel_handle: raw.panel_handle,
            panel_recovery: raw.panel_recovery,
            inspector_gap: raw.inspector_gap,
            row_height_tree: raw.row_height_tree,
            row_height_data: raw.row_height_data,
            column_header_min_height: raw.column_header_min_height,
            font_ui: raw.font_ui,
            font_title: raw.font_title,
            font_status: raw.font_status,
            font_data_cell: raw.font_data_cell,
            font_panel: raw.font_panel,
            font_weight_ui: raw.font_weight_ui,
            font_weight_cell: raw.font_weight_cell,
            font_weight_menu: raw.font_weight_menu,
            font_weight_title: raw.font_weight_title,
            font_weight_heading: raw.font_weight_heading,
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
            navigation_arrow_icon: raw.navigation_arrow_icon,
            navigation_indent: raw.navigation_indent,
            objects_filter_height: raw.objects_filter_height,
            objects_filter_width: raw.objects_filter_width,
            objects_filter_min_width: raw.objects_filter_min_width,
            objects_filter_max_width: raw.objects_filter_max_width,
            navigation_row_height: raw.navigation_row_height,
            navigation_header_button: raw.navigation_header_button,
            navigation_header_height: raw.navigation_header_height,
            row_number_width: raw.row_number_width,
            column_width: raw.column_width,
            filter_panel_width: raw.filter_panel_width,
            filter_column_width: raw.filter_column_width,
            column_panel_width: raw.column_panel_width,
            column_list_max_height: raw.column_list_max_height,
            // 间距阶梯与 CSS 专用度量现在也来自 `metrics.toml`：三档全部写死整数，
            // 代码里不再「基准 × 倍率」——Rust / C# / Swift 的取整规则不同（half-away
            // vs half-to-even），同一份 26×1.25 会得到 33 和 32，三端会静默漂移 1px。
            gap_none: raw.gap_none,
            gap_xxs: raw.gap_xxs,
            gap_xs: raw.gap_xs,
            gap_sm: raw.gap_sm,
            gap_md: raw.gap_md,
            gap_lg: raw.gap_lg,
            gap_xl: raw.gap_xl,
            gap_xxl: raw.gap_xxl,
            gap_page: raw.gap_page,
            // 描边类不随档位（§3.2）：1px 分隔线由 CSS 常量固定，2px 外框同理。
            frame_border_width: raw.frame_border_width,
            radius_lg: raw.radius_lg,
            pill_radius: raw.pill_radius,
            pager_button_min_width: raw.pager_button_min_width,
            fs_button_min_height: raw.fs_button_min_height,
            fs_action_min_height: raw.fs_action_min_height,
            empty_icon_px: raw.empty_icon_px,
            spinner_px: raw.spinner_px,
            // 查询标签布局（#189）：三档值直接来自 metrics.toml，代码不做 base×scale。
            query_editor_min_height: raw.query_editor_min_height,
            query_results_min_height: raw.query_results_min_height,
            query_splitter_width: raw.query_splitter_width,
            query_editor_ratio: raw.query_editor_ratio,
        }
    }
}

// ---------------------------------------------------------------------------
// 当前档位下的布局算术（自由函数：调用点不必先取 Metrics）
// ---------------------------------------------------------------------------

/// 把信息面板宽度钳制到当前档位的可拖拽区间。
#[must_use]
pub fn clamp_inspector(width: i32) -> i32 {
    metrics().clamp_inspector(width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_layout_thresholds_and_hysteresis() {
        let mut state = PanelLayout::default();
        for (width, left, right, center) in [
            (1440, 260, 280, 888),
            (1152, 260, 280, 600),
            (1092, 260, 220, 600),
            (1091, 260, 0, 825),
            (1115, 260, 0, 849),
            (1116, 260, 244, 600),
            (840, 234, 0, 600),
            (786, 180, 0, 600),
            (785, 0, 0, 785),
            (809, 0, 0, 809),
            (810, 204, 0, 600),
            (1, 0, 0, 1),
        ] {
            assert_eq!(
                state.solve(width, Tier::Compact),
                Allocation {
                    panels: [left, right],
                    workspace: center
                },
                "{width}"
            );
        }
        assert_eq!(state.preferences, PanelPreferences::default());
    }

    #[test]
    fn temporary_priority_focus_and_drag_preserve_other_preferences() {
        let mut state = PanelLayout::default();
        let before = state.preferences;
        assert_eq!(state.solve(840, Tier::Compact).panels, [234, 0]);
        state.request(Panel::Inspector);
        assert_eq!(state.solve(840, Tier::Compact).panels, [0, 234]);
        state.focus = true;
        assert_eq!(state.solve(840, Tier::Compact).panels, [0, 0]);
        state.focus = false;
        assert_eq!(state.solve(840, Tier::Compact).panels, [0, 234]);
        let full = state.solve(1116, Tier::Compact);
        assert_eq!(state.priority, None);
        assert_eq!(state.preferences, before);
        let drag = state.drag(Panel::Inspector, 600, 1116, Tier::Compact, full);
        assert_eq!(drag.panels, [180, 324]);
        assert_eq!(drag.workspace, 600);
        assert_eq!(state.preferences.preferred, [260, 324]);
    }

    #[test]
    fn density_and_single_panel_thresholds_follow_explicit_metrics() {
        for tier in Tier::ALL {
            let m = Metrics::for_tier(tier);
            let mut state = PanelLayout::default();
            assert_eq!(state.preferred(tier), [m.sidebar_width, m.inspector_width]);
            state.preferences.shown = [false, true];
            let threshold = m.workspace_target_width + m.inspector_min + m.panel_handle;
            assert_eq!(state.solve(threshold, tier).panels, [0, m.inspector_min]);
            assert_eq!(state.solve(threshold - 1, tier).panels, [0, 0]);
            assert_eq!(
                state.solve(threshold + m.panel_recovery - 1, tier).panels,
                [0, 0]
            );
            assert!(state.solve(threshold + m.panel_recovery, tier).panels[1] > 0);
        }
    }

    #[test]
    fn drag_clamps_both_panels_at_their_limits() {
        let tier = Tier::Compact;
        let m = Metrics::for_tier(tier);
        for (panel, min, max) in [
            (Panel::Navigation, m.sidebar_min, m.sidebar_max),
            (Panel::Inspector, m.inspector_min, m.inspector_max),
        ] {
            let mut state = PanelLayout::default();
            let actual = state.solve(2000, tier);
            let other = 1 - panel.index();
            for (requested, bound) in [(0, min), (2000, max)] {
                let resized = state.drag(panel, requested, 2000, tier, actual);
                assert_eq!(resized.panels[panel.index()], bound);
                assert_eq!(resized.panels[other], actual.panels[other]);
                assert_eq!(state.preferences.preferred[other], actual.panels[other]);
            }
        }
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
            // ⑩（#189）查询分隔四键存在且自洽：两端最小高度为正、把手比两端
            // 最小高度都薄（否则拖到底会吞掉一端）、默认比例在 (0,1) 开区间。
            assert!(
                m.query_editor_min_height > 0 && m.query_results_min_height > 0,
                "{label}：查询分隔两端最小高度必须为正"
            );
            assert!(
                m.query_splitter_width > 0
                    && m.query_splitter_width < m.query_editor_min_height
                    && m.query_splitter_width < m.query_results_min_height,
                "{label}：把手厚度必须为正且薄于两端最小高度"
            );
            assert!(
                m.query_editor_ratio > 0.0 && m.query_editor_ratio < 1.0,
                "{label}：默认比例 {} 不在 (0,1) 内",
                m.query_editor_ratio
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
            (260, 325, 390)
        );
        assert_eq!(
            (c.inspector_width, m.inspector_width, l.inspector_width),
            (280, 350, 420)
        );
        assert_eq!(
            (c.min_window_width, m.min_window_width, l.min_window_width),
            (840, 1050, 1260)
        );
        assert_eq!(
            (
                c.min_window_height,
                m.min_window_height,
                l.min_window_height
            ),
            (600, 750, 900)
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
        // #189 查询分隔：最小高度 / 把手厚度随档位单调放大（钉住 TOML 取值）；
        // 比例是布局语义不是密度尺寸，三档同值 0.4。
        assert_eq!(
            (
                c.query_editor_min_height,
                m.query_editor_min_height,
                l.query_editor_min_height
            ),
            (140, 175, 210)
        );
        assert_eq!(
            (
                c.query_results_min_height,
                m.query_results_min_height,
                l.query_results_min_height
            ),
            (120, 150, 180)
        );
        assert_eq!(
            (
                c.query_splitter_width,
                m.query_splitter_width,
                l.query_splitter_width
            ),
            (6, 8, 9)
        );
        assert_eq!(
            (
                c.query_editor_ratio,
                m.query_editor_ratio,
                l.query_editor_ratio
            ),
            (0.4, 0.4, 0.4)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Navigation = 0,
    Inspector = 1,
}

impl Panel {
    pub const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelPreferences {
    // Widths are stored in Compact units, even while operating at another density.
    pub preferred: [i32; 2],
    pub shown: [bool; 2],
}

impl Default for PanelPreferences {
    fn default() -> Self {
        let m = Metrics::for_tier(Tier::Compact);
        Self {
            preferred: [m.sidebar_width, m.inspector_width],
            shown: [true; 2],
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PanelLayout {
    pub preferences: PanelPreferences,
    pub focus: bool,
    pub priority: Option<Panel>,
    pub folded: [bool; 2],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Allocation {
    pub panels: [i32; 2],
    pub workspace: i32,
}

impl PanelLayout {
    pub fn preferred(self, tier: Tier) -> [i32; 2] {
        let m = Metrics::for_tier(tier);
        let defaults = PanelPreferences::default().preferred;
        let explicit = [m.sidebar_width, m.inspector_width];
        let [left, right] = std::array::from_fn(|i| {
            let preferred = self.preferences.preferred[i];
            if preferred == defaults[i] {
                explicit[i]
            } else {
                crate::window_state::rescale(preferred, Tier::Compact, tier)
            }
        });
        [m.clamp_sidebar(left), m.clamp_inspector(right)]
    }

    pub fn request(&mut self, panel: Panel) {
        self.focus = false;
        let i = panel.index();
        self.preferences.shown[i] = true;
        self.folded[i] = false;
        self.priority = Some(panel);
    }

    pub fn toggle(&mut self, panel: Panel, actual: Allocation) {
        let i = panel.index();
        if !self.focus && actual.panels[i] > 0 {
            self.preferences.shown[i] = false;
            self.folded[i] = false;
            if self.priority == Some(panel) {
                self.priority = None;
            }
        } else {
            self.request(panel);
        }
    }

    pub fn set_preferred(&mut self, panel: Panel, width: i32, tier: Tier) {
        self.preferences.preferred[panel.index()] =
            crate::window_state::rescale(width, tier, Tier::Compact);
    }

    pub fn drag(
        &mut self,
        panel: Panel,
        requested: i32,
        width: i32,
        tier: Tier,
        actual: Allocation,
    ) -> Allocation {
        let m = Metrics::for_tier(tier);
        let i = panel.index();
        let other = 1 - i;
        let min = [m.sidebar_min, m.inspector_min];
        let max = [m.sidebar_max, m.inspector_max];
        let reserved = if actual.panels[other] > 0 {
            min[other] + m.panel_handle
        } else {
            0
        };
        let limit =
            (width - m.workspace_target_width - m.panel_handle - reserved).clamp(min[i], max[i]);
        let requested = requested.clamp(min[i], limit);
        self.set_preferred(panel, requested, tier);
        let mut panels = actual.panels;
        panels[i] = requested;
        if panels[other] > 0 {
            panels[other] = self.preferred(tier)[other]
                .min(width - m.workspace_target_width - 2 * m.panel_handle - requested)
                .max(min[other]);
        }
        let workspace = (width
            - panels
                .iter()
                .filter(|&&p| p > 0)
                .map(|p| p + m.panel_handle)
                .sum::<i32>())
        .max(0);
        Allocation { panels, workspace }
    }

    pub fn solve(&mut self, width: i32, tier: Tier) -> Allocation {
        let width = width.max(0);
        if self.focus {
            return Allocation {
                workspace: width,
                ..Allocation::default()
            };
        }
        let m = Metrics::for_tier(tier);
        let preferred = self.preferred(tier);
        let min = [m.sidebar_min, m.inspector_min];
        if self.preferences.shown == [true; 2]
            && width
                >= m.workspace_target_width
                    + preferred[0]
                    + min[1]
                    + 2 * m.panel_handle
                    + m.panel_recovery
        {
            self.priority = None;
        }
        let order = if self.priority == Some(Panel::Inspector) {
            [1, 0]
        } else {
            [0, 1]
        };
        let mut panels = [0; 2];
        let mut remaining = width;
        for i in order {
            if !self.preferences.shown[i] {
                continue;
            }
            let available = remaining - m.workspace_target_width - m.panel_handle;
            let restoring = self.folded[i] && self.priority.map(Panel::index) != Some(i);
            if available >= min[i] + if restoring { m.panel_recovery } else { 0 } {
                panels[i] = available.min(preferred[i]);
                remaining -= panels[i] + m.panel_handle;
            }
            self.folded[i] = panels[i] == 0;
        }
        Allocation {
            panels,
            workspace: remaining,
        }
    }
}
