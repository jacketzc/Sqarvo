//! 紧凑密度（`workspace-model.md` §11）：全局 CSS，只在 Linux 启用。
//!
//! 行高 24（树）/32（数据行）、字号 12（界面）/13（标题）/11（状态栏）全部来自
//! [`crate::layout`]，CSS 用 `format!` 拼出来——尺寸只有一个来源，改规范时不会出现
//! 「CSS 里还是旧值」的情况。
//!
//! 两条硬约束（§11）：
//!
//! - **不删除焦点环**：本模块只设 `min-height` / `font-size` / 背景与边框，不写
//!   `outline: none`；
//! - **最小点击区 24×24**：所有按钮类规则的下限都不低于 [`layout::MIN_TARGET`]。
//!
//! 紧凑化作用在窗口的 `sqarvo-compact` class 下，不影响同一进程里没有这个 class
//! 的其它窗口（对话框、浮层仍按 libadwaita 默认密度，避免对话框里挤成一行）。

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use crate::layout::{self, Gap, Tier};

// 应用级 provider 的句柄：切档时用它重载同一份样式表（`load_from_string`），
// 不再重复注册 provider。GTK 对象只在主线程用，所以放在 `thread_local`。
thread_local! {
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

/// 给窗口加 class 的名字：`GtkApplicationWindow` 上由 [`crate::ui`] 设置。
pub const WINDOW_CLASS: &str = "sqarvo-compact";

/// 表格工具条状态圆点的命中区 class（`data_page` 挂在圆点按钮上）。
///
/// 与 [`crate::icons::BUTTON_CLASS`] 分开：那一个 class 的按钮必须装一个可测量的
/// `GtkImage`（`icons::audit` 会断言），而圆点是 CSS 画的实心块、不是图标主题的字形。
pub const STATUS_DOT_BUTTON_CLASS: &str = "sqarvo-status-dot-button";

/// 状态圆点「有未提交的更改」态（灰 → accent 底色）。
pub const STATUS_DOT_DIRTY_CLASS: &str = "sqarvo-status-dot-dirty";

/// 状态圆点「事务已打开」态（灰 → 与状态行 `.warning` 文字同源的棕色）。
///
/// 与 [`STATUS_DOT_DIRTY_CLASS`] 互斥：脏行压过事务态（见 `data_page::sync_status_dot`），
/// 两枚 class 每次刷新都显式 add / remove 一次，不会同时残留。
pub const STATUS_DOT_TXN_CLASS: &str = "sqarvo-status-dot-txn";

/// 一档描边 / 分隔线 / 状态栏前景在 `@window_fg_color` 上的**不透明度**。
///
/// 不写 hex：四个值都是同一枚前景 token 的透明度变体，所以深浅色、系统强调色
/// 与高对比主题（前景 / 底色由主题给）都不需要第二份色板。数值按 WCAG 2.1
/// 的相对亮度反算（L = 0.2126R + 0.7152G + 0.0722B，各通道先做 sRGB 线性化；
/// 对比度 = (L1 + 0.05) / (L2 + 0.05)）；每个取值旁边的注释是改动前后实测像素算出
/// 来的数字：表内分隔线只要「看得出格子」，外框与浮层描边按图形对象 3:1 取。
struct Tones {
    /// 分隔线：工具条 / 标签条 / 状态栏 / 信息面板的分界线，以及网格的行列线。
    separator: &'static str,
    /// 表格外框（比同档分隔线实一档，2px 宽由度量表给）。
    frame: &'static str,
    /// 浮层描边：列显隐 / 事务卡片 / 筛选条件条。暗底上黑色阴影等于没有边界，
    /// 边界改由这条 1px 线承担。
    overlay: &'static str,
    /// 状态栏前景（11px 小字与不可用按钮共用同一个灰度）。
    status_fg: &'static str,
    /// 标签条 hover 底（`@window_fg_color` 的透明度变体，中性灰）。
    tab_hover: &'static str,
    /// 标签条当前标签底（同上，比 hover 实一档）。
    tab_selected: &'static str,
}

/// 三档品牌色板的一份完整取值（底色 + 与之配对的前景 + 两套描边强度）。
///
/// 浅色与暗色是**同一个色相的两套亮度**，不是两套色相：深档 `#5B3256` → `#7A456F`、
/// 中档 `#A66FA0` → `#B98AB2`、浅档 `#D8B7D3` → `#4A2E45`（暗界面里亮底会刺眼，
/// 所以「淡填充」在暗色下是**暗**填充）。前景随底色一起换，`on_soft` 用 `@theme_fg_color`
/// / `#F3E7EF` 这类现有 token 表示，不新造色相。
struct Palette {
    deep: &'static str,
    mid: &'static str,
    soft: &'static str,
    on_deep: &'static str,
    on_mid: &'static str,
    on_soft: &'static str,
    /// 普通模式的描边 / 前景强度。
    normal: Tones,
    /// 高对比模式的描边 / 前景强度（`AdwStyleManager::is_high_contrast()` 为真时）。
    /// 两套色板都定义同名 token，取值相同：高对比下前景与底色由 HighContrast 主题
    /// 给出（黑 / 白），这里只决定「线有多实」。
    high: Tones,
}

const LIGHT_PALETTE: Palette = Palette {
    deep: "#5B3256",
    mid: "#A66FA0",
    soft: "#D8B7D3",
    on_deep: "#FFFFFF",
    on_mid: "#FFFFFF",
    on_soft: "@theme_fg_color",
    normal: Tones {
        // 浅色：**外框**收到 WCAG 1.4.11 的 3:1（它承担「这是一块表格」的识别），
        // **行/列分隔线**留在 1.5:1 左右——严格按条文，行靠内容识别、分隔线不是
        // 「识别所必需」的视觉信息，拉满 3:1 只会让外框与格线同一深度、层次塌掉。
        // 实测（#fafafa 底，打开表后量像素）：0.63 → #838383（3.63:1）、0.22 → #d0d0d0
        // （1.48:1）；原来的 0.30 / 0.10 是 1.72:1 与 1.18:1。
        separator: "0.22",
        frame: "0.63",
        overlay: "0.20",
        status_fg: "0.82",
        tab_hover: "0.06",
        tab_selected: "0.12",
    },
    high: Tones {
        // 浅色普通档已经提到 0.52 / 0.62（3:1 下限），高对比必须比它更实。
        separator: "0.70",
        frame: "0.76",
        overlay: "0.55",
        status_fg: "0.90",
        tab_hover: "0.12",
        tab_selected: "0.24",
    },
};

const DARK_PALETTE: Palette = Palette {
    deep: "#7A456F",
    mid: "#B98AB2",
    soft: "#4A2E45",
    on_deep: "#FFFFFF",
    // 中档在暗色下被提亮到 `#B98AB2`，白字只剩 2.86:1：这里改用暗色底的前景色
    // （`@view_bg_color`，Yaru 暗色 = #1d1d20，5.88:1），是「颜色跟着底色走」的必然结果。
    on_mid: "@view_bg_color",
    on_soft: "#F3E7EF",
    normal: Tones {
        // 暗底上 10% 前景的线只有 1.1–1.4:1（实测：状态栏上边线 1.17、网格右/下边线
        // 1.35），表格看起来是敞开的：#161 把它提到 16%，与 Adwaita 自己画的网格内线
        // （实测 15% / #3e3e40）同一档，行 / 列线不再一条深一条浅（1.63:1）。
        separator: "0.16",
        // 外框是「这是一块表格」的唯一提示，按图形对象 3:1 的下限取 45%（实测
        // #878787 叠窗口底色 3.89:1；原来的 30% 只有 2.54:1）。
        frame: "0.45",
        // 浮层底色 = 窗口底色（暗色下同为 #2c2c2c），阴影是黑的、叠在暗底上等于没有：
        // 边界只能靠这条 1px 线，取 36%（实测 #757575 叠卡片底色 3.03:1、叠表格 3.73:1）。
        overlay: "0.36",
        // 11px 小字要求 4.5:1；55% 的 #9c9c9c 只有 5.09:1（抗锯齿后更淡），62% 的
        // #aaaaaa 到 6.01:1。
        status_fg: "0.62",
        // 暗底上同样的百分比要更实一档才看得出：8% 是 hover 的一层薄膜，
        // 18% 让当前标签与非选中标签拉开（实测 #525252 vs 标签条底色 #2c2c2c）。
        tab_hover: "0.08",
        tab_selected: "0.18",
    },
    high: Tones {
        separator: "0.45",
        frame: "0.72",
        overlay: "0.55",
        status_fg: "0.85",
        tab_hover: "0.14",
        tab_selected: "0.30",
    },
};

// 当前深浅色 / 高对比（由 [`install`] 从 `AdwStyleManager` 写入；测试环境下没有显示服务 → 都是假）。
//
// 用 `Cell` 而不是 `OnceLock`：深浅色与高对比都是**可变**状态，`OnceLock::set` 在
// 第一次写入之后会静默失败，切档重载时就会读回装窗口那一刻的旧值（暗色下切档
// 退回浅色样式表的正是这条）。GTK 对象只在主线程用，所以放在 `thread_local`。
thread_local! {
    static DARK: Cell<bool> = const { Cell::new(false) };
    static HIGH_CONTRAST: Cell<bool> = const { Cell::new(false) };
}

fn is_dark() -> bool {
    DARK.with(Cell::get)
}

/// 当前是否高对比（`AdwStyleManager::is_high_contrast()`）。
///
/// 与 [`is_dark`] 同源：启动时读一次，`dark` / `high-contrast` 两个通知都重载样式表
/// 并把值写回这里，所以切档（[`reload`]）读到的也是最新值。
fn is_high_contrast() -> bool {
    HIGH_CONTRAST.with(Cell::get)
}

/// 安装紧凑密度 CSS（进程内只装一次）。
///
/// 三档色板要跟着系统的深浅色走：**`@media (prefers-color-scheme: dark)` 里写
/// `@define-color` 在应用级 provider 里不生效**（实测：`ADW_DEBUG_COLOR_SCHEME=prefer-dark`
/// 下窗口已经是暗色，`suggested-action` 仍是浅色的 `#5B3256`），所以改为在这里读
/// `AdwStyleManager::is_dark()` 生成对应的一套 token，并在 `dark` 变化时重载同一份
/// provider——色值仍然只在 [`LIGHT_PALETTE`] / [`DARK_PALETTE`] 定义一次。
///
/// 高对比同理：`AdwStyleManager::is_high_contrast()` 决定描边 / 分隔线 / 状态栏前景
/// 取 `Tones::high` 还是 `Tones::normal`，`high-contrast` 的通知与 `dark` 的通知走
/// 同一条重载路径（见 [`apply_style_manager`]）。
pub fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let manager = adw::StyleManager::default();
    let provider = gtk::CssProvider::new();
    apply_style_manager(&provider, &manager);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    PROVIDER.with(|slot| *slot.borrow_mut() = Some(provider.clone()));
    // 深浅色切换（系统设置 / libadwaita 的 prefer-dark）时重载同一份 provider。
    let dark_provider = provider.clone();
    manager.connect_dark_notify(move |manager| {
        apply_style_manager(&dark_provider, manager);
    });
    // 高对比（系统设置 → 无障碍 → 高对比）同样重载：`AdwStyleManager::high-contrast`
    // 自带 notify（libadwaita 0.9 的 `connect_high_contrast_notify`），不需要轮询。
    let contrast_provider = provider.clone();
    manager.connect_high_contrast_notify(move |manager| {
        apply_style_manager(&contrast_provider, manager);
    });
}

/// 把 `AdwStyleManager` 的当前取值写进两枚状态位并重载样式表。
///
/// `dark` 与 `high-contrast` 两个通知共用：任一变化都要重新生成整份样式表（色板与
/// 描边强度都在里面），所以只留这一条写回 + 重载路径。
fn apply_style_manager(provider: &gtk::CssProvider, manager: &adw::StyleManager) {
    let dark = manager.is_dark();
    let high_contrast = manager.is_high_contrast();
    DARK.with(|slot| slot.set(dark));
    HIGH_CONTRAST.with(|slot| slot.set(high_contrast));
    provider.load_from_string(&stylesheet_for(dark, high_contrast, layout::current_tier()));
    eprintln!(
        "[theme] 样式重载：dark={dark} high_contrast={high_contrast} tier={}",
        layout::current_tier().id()
    );
}

/// 切档时重载应用级样式表（同一个 provider，不重复注册）。
///
/// 尺寸字段在 [`stylesheet_for`] 里按**当前档位**取值；只重载字符串不会动控件上
/// 已经写死的 `set_margin_*` / 列宽，那些由 [`crate::density`] 的重排补齐。
pub fn reload() {
    let css = stylesheet();
    PROVIDER.with(|slot| {
        if let Some(provider) = slot.borrow().as_ref() {
            provider.load_from_string(&css);
        }
    });
}

/// 当前档位的紧凑密度样式表（按当前深浅色 / 高对比取对应色板）。测试可以检查关键尺寸与色值。
#[must_use]
pub fn stylesheet() -> String {
    stylesheet_for(is_dark(), is_high_contrast(), layout::current_tier())
}

/// 指定深浅色、高对比与档位生成样式表：测试直接调它，断言两套色板、两档描边与三档尺寸都在。
#[must_use]
pub fn stylesheet_for(dark: bool, high_contrast: bool, tier: Tier) -> String {
    let palette = if dark { &DARK_PALETTE } else { &LIGHT_PALETTE };
    // 高对比只换「线有多实」，不换色相：前景 / 底色由 HighContrast 主题决定。
    let tones = if high_contrast {
        &palette.high
    } else {
        &palette.normal
    };
    // 尺寸 / 字号 / 圆角 / 间距一律按档位取（`metrics.toml`），样式表里不再写死像素值。
    let m = layout::Metrics::for_tier(tier);
    format!(
        r#"
        /* 品牌三档色板：界面上**所有**强调底色只从这里取，别处不许写 hex。
           深 = 实底强调（主操作按钮）；中 = 选中态（哪一行 / 哪一个当前选中）；
           浅 = 淡填充（hover 底、脏行单元格底、徽标与面板的弱底）。
           前景色与底色成对定义（on_*），对比度是按 WCAG 算过的：
             深档 + 白字 10.32:1；中档 + 白字 3.89:1（见 design-principles 的色板表，
             这是本轮唯一低于正文 4.5 的组合，仍高于图形 3.0）；浅档 + @theme_fg_color 6.98:1。
           暗色不是另一套色相：同一组紫，提亮度、浅档改成暗填充（亮底在暗界面里刺眼），
           由 Rust 侧按 `AdwStyleManager::is_dark()` 选一套写进来（CSS 媒体查询在本
           provider 里对 `@define-color` 无效，实测见 `install` 的注释）。 */
        @define-color sqarvo_brand_deep {deep};
        @define-color sqarvo_brand_mid {mid};
        @define-color sqarvo_brand_soft {soft};
        @define-color sqarvo_brand_on_deep {on_deep};
        @define-color sqarvo_brand_on_mid {on_mid};
        @define-color sqarvo_brand_on_soft {on_soft};

        /* 描边 / 分隔线的四枚 token（#161）：都是 `@window_fg_color` 的透明度变体，
           深浅色与高对比各一套取值，来自 `Palette::normal` / `Palette::high`。
           线宽仍是 1px（外框按度量表的 2px），只有浓淡变——所以切档不会重排。
             separator：工具条 / 标签条 / 状态栏 / 信息面板的分界线 + 网格行列线
             frame：表格外框（比同档分隔线实一档）
             overlay：浮层（列显隐 / 事务卡片 / 筛选条件条）的 1px 描边
             status_fg：状态栏 11px 小字与不可用按钮共用的前景 */
        @define-color sqarvo_separator alpha(@window_fg_color, {separator});
        @define-color sqarvo_frame alpha(@window_fg_color, {frame});
        @define-color sqarvo_overlay alpha(@window_fg_color, {overlay});
        @define-color sqarvo_status_fg alpha(@window_fg_color, {status_fg});

        /* 主操作按钮（「连接」/「新建连接」/「保存」/「提交」/「提交事务」/「执行」）：
           实底 = 深档。libadwaita 的 hover / active 是用 `background-image` 叠一层
           半透明色，所以只换 `background-color` 就覆盖了全部状态，不会在 hover 时
           退回系统强调色（Yaru 下是橙红 #E95420）。 */
        button.suggested-action {{
            background-color: @sqarvo_brand_deep;
            color: @sqarvo_brand_on_deep;
        }}

        /* 选中态（中档）：对象树节点、检索结果行、数据网格行、`Objects` 三列表格行
           都是「当前选中的是哪一个」。原来这四处吃的是 Adwaita 默认
           （`row:selected` = accent 25%），两张表因此会一个品牌色一个系统蓝 ——
           这里把两张 `GtkColumnView` 与树写进同一条规则，四处的选中底色只有一份定义。
           hover 在前、selected 在后：`:selected:hover` 同时命中两条，靠顺序让中档赢。 */
        window.{window_class} listview.sqarvo-tree > row:hover,
        window.{window_class} list.sqarvo-search-list > row:hover,
        window.{window_class} columnview.sqarvo-grid > listview > row:hover,
        window.{window_class} columnview.sqarvo-objects > listview > row:hover {{
            background-color: alpha(@sqarvo_brand_soft, 0.55);
        }}
        window.{window_class} listview.sqarvo-tree > row:selected,
        window.{window_class} list.sqarvo-search-list > row:selected,
        window.{window_class} columnview.sqarvo-grid > listview > row:selected,
        window.{window_class} columnview.sqarvo-objects > listview > row:selected,
        window.{window_class} listview.sqarvo-tree > row:selected:hover,
        window.{window_class} columnview.sqarvo-grid > listview > row:selected:hover,
        window.{window_class} columnview.sqarvo-objects > listview > row:selected:hover {{
            background-color: @sqarvo_brand_mid;
            color: @sqarvo_brand_on_mid;
        }}
        /* 选中行里的语义文字（行号列的 ●脏 / ✕删除）保留记号本身，但换前景色：
           原来的橙红 / 错误红压在中档选中底上只有 1.6–1.9:1，读不出来。 */
        window.{window_class} columnview.sqarvo-grid > listview > row:selected .sqarvo-dirty,
        window.{window_class} columnview.sqarvo-grid > listview > row:selected .sqarvo-deleted,
        window.{window_class} columnview.sqarvo-objects > listview > row:selected .sqarvo-dirty {{
            color: @sqarvo_brand_on_mid;
        }}

        window.{window_class} {{ font-size: {ui}px; }}

        /* 区域 01：标题 13 semibold（标题控件在 headerbar 里） */
        .sqarvo-title {{ font-size: {title}px; font-weight: {fw_title}; }}
        /* 卡片 / 面板 / 对象树的连接与分组名：原来是 Adwaita 的 `.heading`（它只有
           font-weight: 700 一条规则）。改成自有类，字重由 metrics.toml 的
           font_weight_heading 决定，不再随 libadwaita 版本变。 */
        .sqarvo-heading {{ font-weight: {fw_heading}; }}

        /* 区域 02：主工具条 44 高（24 的按钮 + 上下各 10），按钮不小于 24 */
        .sqarvo-toolbar {{
            min-height: {toolbar}px;
            padding: 0 {toolbar_padding_h}px;
            border-bottom: 1px solid @sqarvo_separator;
        }}
        .sqarvo-toolbar button {{ min-height: {target}px; padding: {gap_xxs}px {gap_lg}px; }}
        .sqarvo-toolbar label {{ font-size: {ui}px; }}
        /* 种类入口是纯拉丁文字（Tables / Views / Functions / Queries），和中文
           「新建查询」同一行混排时，Pango 的中文 em 盒比拉丁大写字高更靠下：
           盒子同高同 y 也会看起来「一高一低」。这里只把**字**下移 2px
           （实测：中文 63–74、拉丁 62–71，视觉中线差 2px），命中区不变。 */
        .sqarvo-toolbar button.sqarvo-kind label {{ margin-top: {gap_xxs}px; }}

        /* 区域 03 / 04：树行 24，数据行 32，表头 24。
           `Objects` 固定页（v0.2.4）与数据网格同一套渲染：同一个 `GtkColumnView`
           口径（`columnview` + 行 / 列分隔线），所以这两条规则同时选中
           `.sqarvo-objects`——两张表的行高、表头高度只有一份定义，改一处两边一起变。 */
        window.{window_class} listview > row {{ min-height: {tree_row}px; }}
        window.{window_class} columnview.sqarvo-grid > listview > row,
        window.{window_class} columnview.sqarvo-objects > listview > row {{
            min-height: {data_row}px;
        }}
        /* 表头只有**一行**（「列名 · 原始类型」）。底色取原来那条类型提示行的实测色：
           `#fafafa` = `@theme_bg_color`（原 `sqarvo-type-row` 没有自己的底色，露出的
           就是窗口 / 表格底色）；表头按钮原本的白底（`@headerbar_bg_color`）因此被
           压平，表头与类型提示合成一条 30px 的条带（原来是 24px 类型行 + 30px 列名行
           = 54px）。点击列头 = 查看列属性，hover 只做轻量反馈。
           `Objects` 三列表格（行号 | 表名 | 类型）用同一组选择器——两张表头同一高度。 */
        /* `Objects` 的单元格里是「[图标] + 名称」的 `GtkBox`（数据网格的单元格里只有一个
           Label）：Adwaita 给 `cell` 的上下内距会把这一类行从 32 撑到 35（实测），
           两张表格就此差 3px。上下内距归零后行高回到 `min-height`（32），与数据网格
           逐像素同高；左右内距保留（列内距仍由 Adwaita 给）。 */
        window.{window_class} columnview.sqarvo-objects > listview > row > cell {{
            padding-top: 0;
            padding-bottom: 0;
        }}
        window.{window_class} columnview.sqarvo-grid > header > button,
        window.{window_class} columnview.sqarvo-objects > header > button {{
            min-height: {column_header_min_height}px;
            padding: {gap_xxs}px {gap_md}px;
            font-size: {ui}px;
            font-weight: {fw_title};
            color: @theme_fg_color;
            background-color: @theme_bg_color;
            background-image: none;
            box-shadow: none;
            border-radius: 0;
        }}
        /* Adwaita 给列表头的是弱化前景色（实测 #b4b4b4 上下），而且套在**标题 label**
           这一层：只在按钮上给 `color` 不够。表头现在只剩这一行、又承载原始类型，
           必须与数据行同一档可读性（实测与数据行同为 #3d3d3d）。 */
        window.{window_class} columnview.sqarvo-grid > header > button label,
        window.{window_class} columnview.sqarvo-objects > header > button label {{
            color: @theme_fg_color;
        }}
        window.{window_class} columnview.sqarvo-grid > header > button:hover,
        window.{window_class} columnview.sqarvo-objects > header > button:hover {{
            background-color: alpha(@sqarvo_brand_soft, 0.55);
        }}
        window.{window_class} columnview.sqarvo-grid > header > button:active,
        window.{window_class} columnview.sqarvo-objects > header > button:active {{
            background-color: @sqarvo_brand_soft;
        }}

        /* 表格右边界：Adwaita 的列分隔线画在「每个单元格 / 表头按钮的 border-left」上，
           最后一列因此没有右边的竖线——窄表看起来像缺一条边，宽表被视口裁切时也看不出
           表格在哪里结束。这里给最后一列补一条右侧竖线，颜色与列分隔线一致；数据行与
           表头在同一条竖线上收口（表头只剩一行，没有第二条类型条带要对齐）。
           `Objects` 三列表格（行号 | 表名 | 类型）走同一组选择器。 */
        window.{window_class} columnview.sqarvo-grid > listview > row > cell:last-child,
        window.{window_class} columnview.sqarvo-objects > listview > row > cell:last-child,
        window.{window_class} columnview.sqarvo-grid > header > button:last-child,
        window.{window_class} columnview.sqarvo-objects > header > button:last-child {{
            border-right: 1px solid @sqarvo_separator;
        }}

        /* 表格下边界（v0.2.3.3）：Adwaita 的行分隔线是每行的 `border-top`，最后一行
           因此没有下边线——数据不足一屏时表格底边是敞开的。给最后一行补一条同色的
           下边线：列表虚拟化只在内容全部实现（也就是内容不足一屏、正好需要这条线）
           时才有 `:last-child`，长表滚到底也不会在视口中间凭空多出一条。
           `columnview` / `listview` 自己的 `border-bottom` 不会画出来（它们是滚动
           内容节点），所以这条线必须挂在行上。 */
        window.{window_class} columnview.sqarvo-grid > listview > row:last-child,
        window.{window_class} columnview.sqarvo-objects > listview > row:last-child {{
            border-bottom: 1px solid @sqarvo_separator;
        }}

        /* 表格外框（v0.2.4）：`Objects` 列表与表数据网格各套一圈 2px 线 + 8px 圆角，
           让表格从页面底色里跳出来，两块表格同一份口径。颜色取 `@sqarvo_frame`
           （比同档分隔线实一档，浅色 30% / 暗色 45% / 高对比 72%）：暗色下 30% 的
           前景叠在窗口底色上只有 2.8:1，表格几乎看不出是个「块」。
           线只画在**容器**上（`.sqarvo-table-frame` 挂在包住表格的盒子 /
           `ScrolledWindow`），行与单元格不加线，避免与 `cell:last-child` 的右边界
           叠成双线。
           圆角靠 `GtkWidget:overflow = hidden`（`data_page.rs` / `tabs.rs` 各自在挂
           class 的控件上设）：GTK4 默认不裁子节点，上一版因此只能退回直角——首行
           选中底色会把左上角填成方角。现在容器的子节点按圆角裁切，首行 / 末行的
           底色跟着边框收圆。 */
        window.{window_class} .sqarvo-table-frame {{
            border: {frame_border_width}px solid @sqarvo_frame;
            border-radius: {radius_md}px;
        }}

        /* 浮层边界（#161）：列显隐 / 事务卡片与筛选条件条都只靠
           `box-shadow: 0 8px 24px alpha(black, 0.25)` 划边界——阴影是**黑**的，叠在
           暗色底（#2c2c2c）上等于没有，卡片与表格糊成一片。阴影保留，另加一条 1px
           描边（`@sqarvo_overlay`，暗色 36% ≈ 3.1:1）承担边界；高对比下提到 55%。
           `data_page.rs` 里只留阴影与圆角，描边放在这里——色值只从 token 取。
           条件条（`.fs-row`）的描边写在下面它自己的规则里（用 inset 环而不是 border：
           行高是 24px 的密度口径，`min-height` 是内容盒高，border 会把每行撑到 26，
           inset 环不占尺寸）。 */
        .sqarvo-column-info {{
            border: 1px solid @sqarvo_overlay;
        }}

        /* 图标按钮（v0.2.3.3）：命中区固定（§图标），内边距归零——图标自身的像素尺寸
           由 `icons::OpticalWeight` 决定，容器不再给默认内边距，否则同一档图标在不同
           容器里宽度会不一样。 */
        window.{window_class} button.{icon_button_class} {{
            padding: 0;
            min-width: {icon_button_width}px;
            min-height: {icon_button_height}px;
        }}

        /* 表格工具条状态圆点（v0.2.4）：原来那两段常驻文字（「自动提交」事务徽标 +
           「脏行 N · 待提交 M 条」）收成一枚直径 {status_dot}px 的实心圆点——约漏斗
           墨迹（12 × 11 @ 16px 图标盒）的一半。命中区仍是 {target} × {target}：
           视觉可以小，hover / 点击区不许小（§11）。两段文案改由 tooltip 承载。 */
        window.{window_class} .sqarvo-table-actions button.{status_dot_button} {{
            padding: 0;
            min-width: {target}px;
            min-height: {target}px;
        }}
        /* 灰 = 没有未提交的更改、也没有打开的事务（不是可点动作，别抢注意力）。 */
        .sqarvo-status-dot {{
            border-radius: {pill_radius}px;
            background-color: alpha(@window_fg_color, 0.35);
        }}
        /* 棕 = 事务已打开（含只读 / 失败态）：token 与状态行 `.warning` 文字同源
           （Adwaita / Yaru 的 `.warning` 是 `color: var(--warning-color)`，
           `@warning_color` 就是它；Yaru 浅色下 = #9f4700）。这句状态原来以
           「事务状态：事务中」的橙棕文字挂在动作行上，v0.2.4 后收进这枚圆点：
           文字不写了，事务在不在靠颜色 + tooltip 读。规则放在脏行之前——
           两枚 class 本已互斥，这里只是让 accent 在极端情况下也压得住。 */
        .sqarvo-status-dot.{status_dot_txn} {{
            background-color: @warning_color;
        }}
        /* 红 = 有未提交的更改：用 `@accent_bg_color`（系统强调色，Yaru 下橙红）。
           这里是**刻意保留的语义色**：脏行记号在本轮换色之前就是 accent 红，用户
           要求三档品牌色不得改写它（它与「提交」按钮的深档紫已经不是同一色系，
           这是本轮之后的新事实——脏行靠这枚红点区分「有改动」）。
           优先级最高：脏行本身就是事务里的改动，「有待提交的东西」比「正在事务里」
           更需要被看见。 */
        .sqarvo-status-dot.{status_dot_dirty} {{
            background-color: @accent_bg_color;
        }}

        /* 区域 04 顶部：标签条 24（workspace-model §2.1 的 tabStrip） */
        .sqarvo-tab-strip {{
            min-height: {tab_strip}px;
            padding: 0 {gap_sm}px;
            border-bottom: 1px solid @sqarvo_separator;
        }}
        /* AdwTabBar 默认把标签条撑到 ~47px；这里压到规范口径附近（24px 行高）。
           标签本身的最小点击区仍不小于 24（§11），缩的是外层留白。 */
        .sqarvo-tab-bar {{
            padding: 0 {gap_xxs}px;
            min-height: {tab_strip}px;
        }}
        /* 标签条只许有一条下边线（v0.2.4 用户反馈「分割线像两条线叠在一起」）。
           `AdwTabBar` 内部的 `.box` 自带 `box-shadow: inset 0 -1px
           var(--headerbar-shade-color)`（见 libadwaita 的 `tabbar .box`），它画在
           `.sqarvo-tab-strip` 的 `border-bottom` 正上方：两条 1px 线紧贴，颜色还不同
           （shade 偏深、本项目的 10% 偏浅），看上去就是一条线被描了两遍。
           标签条在工作区里只是一条普通行、不是 headerbar，所以去掉 libadwaita 那条
           shade，只留上面那条 10% 的下边线——与工具条、状态栏同一口径。
           选择器比 `tabbar .box` 多一层类（且本样式表优先级更高），高对比度模式里
           同名 shade 的 `@media` 变体同样被压掉。 */
        .sqarvo-tab-strip tabbar .box {{
            box-shadow: none;
        }}
        .sqarvo-tab-bar tab {{
            min-height: {target}px;
            padding: 0 {gap_md}px;
            font-size: {ui}px;
        }}
        /* 标签条（v0.2.5）：选中 / hover 都是**中性灰**，不再用品牌紫。
           标签回答的是「现在看的是哪一页」这种位置问题，不是「点这个」这种强调动作；
           紫底会和工具条的主操作按钮、表格行选中抢同一层视觉。灰底仍由前景色透明度
           派生（`@window_fg_color` + `Tones::tab_*`），深浅色与高对比各一套强度，
           文字色不单独指定、跟随主题正文，对比度不用另算。
           `tab:selected:hover` 也要写，否则 hover 时退回主题自己的底色。 */
        .sqarvo-tab-bar tab:hover {{
            background-color: alpha(@window_fg_color, {tab_hover});
        }}
        .sqarvo-tab-bar tab:selected,
        .sqarvo-tab-bar tab:selected:hover,
        .sqarvo-tab-bar tab:selected:active {{
            background-color: alpha(@window_fg_color, {tab_selected});
        }}
        .sqarvo-tab-bar tab:selected indicator {{
            background: @window_fg_color;
        }}
        .sqarvo-tab-strip button {{ min-height: {target}px; min-width: {target}px; }}
        /* 固定 `Objects` 页（v0.2.3.2）：钉住的页永远排在最前，宽度按规范口径 84px。
           `AdwTabBar` 没有 per-page 宽度 API；实测钉住页带 `pinned` 样式类
           （`[tabs] 标签实际宽度：pinned=26px`），所以按它选中。
           内容盒 84 + padding 0，避免被默认内边距撑大。 */
        .sqarvo-tab-bar tab.pinned {{
            min-width: {objects_min_width}px;
            padding: 0;
        }}

        /* 区域 04：固定 `Objects` 页（v0.2.4 起与数据网格同一套三列表格）。
           它的行高 / 表头高度 / 右边界 / 下边界直接由上面 `columnview.sqarvo-objects`
           那组规则给出，与 `columnview.sqarvo-grid` 并列；hover（浅档）与选中（中档）
           写在文件开头的品牌色板规则里——两张表和对象树共用同一条，不再各自吃
           Adwaita 的 `row:hover` / `row:selected` 默认色（那是 accent 25%，会与
           对象树的选中态不是一个颜色）。 */

        /* 区域 05：状态栏 48 高（两行各 24），字号 11 */
        .sqarvo-status-bar {{
            min-height: {status}px;
            font-size: {status_font}px;
            border-top: 1px solid @sqarvo_separator;
        }}
        /* 状态栏只有两种前景色（v0.2.3.4）：可用的按钮一个色（`@window_fg_color`），
           文字与不可用（:disabled）的按钮同一个灰（`@sqarvo_status_fg`）。三档以上会让
           同一行里 `+` 很黑、`−` 半灰、页码文字又另一个灰，看起来像三套控件。
           这个灰的浓淡按 WCAG 反算（#161）：11px 是「小字」，下限 4.5:1；旧值 55%
           在浅色下只有 2.98:1（#929292 压 #fafafa，实测才是真正读不动的那一档）、
           暗色下 5.09:1，分别提到 82% / 62%——实测浅色 #5f5f5f 6.12:1、暗色 #aaaaaa
           6.01:1，仍明显弱于正文（13:1），但不再「看得见但费劲」。
           两个坑都在 `:disabled` 上：Adwaita 的 `.flat:disabled` 会再叠一层 opacity
           （实测黑色图标被压到 #AFAFAF），`dim-label` 自己也带 0.55 透明度——全部用
           `opacity: 1` 关掉，颜色才由这两条规则唯一决定。 */
        window.{window_class} .sqarvo-status-bar {{
            color: @sqarvo_status_fg;
        }}
        window.{window_class} .sqarvo-status-bar label {{
            color: @sqarvo_status_fg;
            opacity: 1;
        }}
        window.{window_class} .sqarvo-status-bar button {{
            color: @window_fg_color;
        }}
        window.{window_class} .sqarvo-status-bar button label {{
            color: inherit;
        }}
        /* 搜索框里用户输入的字按「激活」色：它是输入值，不是状态说明；占位符仍是淡色。 */
        window.{window_class} .sqarvo-status-bar entry {{
            color: @window_fg_color;
        }}
        /* v0.2.5：只有颜色差（6:1 的灰 vs 正文色）在 11px 图标上看着「一样亮」——用户反馈
           「激活 / 非激活看起来一样」。补一档不透明度让不可用态一眼可辨：0.45 时浅色下
           实际渲染约 #a8a8a8，与可用态（#1f1f1f）拉开到 5 倍以上对比。 */
        window.{window_class} .sqarvo-status-bar button:disabled,
        window.{window_class} .sqarvo-status-bar button.flat:disabled,
        window.{window_class} .sqarvo-status-bar button.sqarvo-icon-button:disabled {{
            color: @sqarvo_status_fg;
            opacity: 0.45;
            filter: none;
        }}
        /* v0.2.3.3：状态栏分成「对象树功能区的左边沟 | 竖分割线 | 右列」。
           左边沟宽度由 shell 跟着侧栏分栏位置设（`set_gutter_width`），右列自己带
           左右 12px 内距——这样分割线与上面的分栏线在同一根轴上。 */
        .sqarvo-status-gutter {{ min-height: {status}px; }}
        .sqarvo-status-divider {{
            min-width: 1px;
            background: @sqarvo_separator;
        }}
        .sqarvo-status-right {{ padding: 0 {gap_xl}px; }}
        .sqarvo-status-row {{ min-height: {tree_row}px; }}
        .sqarvo-status-bar button {{
            min-height: {target}px;
            min-width: {target}px;
            padding: 0 {gap_md}px;
            font-size: {status_font}px;
        }}
        .sqarvo-status-bar entry {{ min-height: {target}px; font-size: {status_font}px; }}
        /* 分页器（v0.2.3.4）：尺寸对齐状态栏行操作里的 `+`（16px 图标盒、墨迹 11×11、
           命中区 24 高）。图标 14px 时墨迹正好落在 11×11；按钮回到 24 高，与 `+` 同一档。
           页码文字同步放大到 12px（沿用状态栏的 UI 字族，中文走系统 CJK 回退），
           与图标的视觉高度对齐。 */
        window.{window_class} .sqarvo-pager button {{
            min-height: {target}px;
            min-width: {pager_button_min_width}px;
            padding: 0 {gap_xs}px;
        }}
        window.{window_class} .sqarvo-pager-label {{
            font-size: {pager_font}px;
        }}
        .sqarvo-status-sep {{ min-height: {tree_row}px; }}

        /* 区域 06：信息面板（空槽，内容由 #119 填）
           v0.2.5：不再画 `border-left`。工作区与面板之间是 `GtkPaned`，它自己就有一条
           1px 分栏线；面板上再加一条，两条并排就是一堵双线（用户：「有点多余」）。
           分隔只由 Paned 提供，面板只留内边距。 */
        .sqarvo-inspector {{
            padding: {gap_xl}px;
        }}
        .sqarvo-slot-note {{ font-size: {status_font}px; }}

        /* 数据网格单元格（结果表 / 表数据）：12px 下 GTK4 会裁掉部分 CJK 字形
           笔画（用户反馈「张三 / 王五」显示残缺）。13px 起完整；为在 1x 屏幕上
           避免 Regular 横画发虚，字号保持 13px、字重提升到 Medium（500）。 */
        .sqarvo-cell {{ font-size: {data_font}px; font-weight: {fw_cell}; }}
        /* 布尔值单元格（true / false）比普通值重一档标题字重，便于扫读。 */
        .sqarvo-cell-bool {{ font-weight: {fw_title}; }}

        /* 行号列（32px；v0.2.4 起数据网格与 `Objects` 三列表格共用同一份度量）：
           11px monospace、右对齐、色弱一档。原来只写在 `data_page` 的附加样式里，
           `Objects` 页先于任何表标签出现时拿不到这份规则，行号会和网格不一样。 */
        .sqarvo-row-number {{
            font-family: monospace;
            font-size: {status_font}px;
            opacity: 0.7;
            padding-right: {gap_xs}px;
        }}

        /* 信息面板正文（[DDL] 与 [表信息] 两页**同一口径**）：10px、不加粗。面板是
           次要信息，视觉重心在数据网格——字号和字重都不许跟表格抢注意力。分层只靠
           颜色（标签 dim）与分节间距，不再用 Medium / Semibold。 */
        .sqarvo-panel-text {{ font-size: {panel_font}px; font-weight: {fw_ui}; }}

        /* 信息面板的 [DDL] / [表信息]（v0.2.3.3）：两个独立 toggle，选中态是贴着
           文字的**小圆角块，四个角都圆**。不用 `.linked` 分段容器：分段样式只圆外侧
           两个角、命中块比文字大一圈。命中区仍不小于 24（§11），缩的是内边距。 */
        .sqarvo-pane-switch > button {{
            min-height: {target}px;
            min-width: {target}px;
            padding: 0 {gap_xs}px;
            border: none;
            border-radius: {radius_sm}px;
            background: transparent;
            box-shadow: none;
        }}
        .sqarvo-pane-switch > button:hover {{ background: alpha(@sqarvo_brand_soft, 0.55); }}
        /* 选中态用中档的**半透明**（45%）：这是 10px 正文旁边的小切换块，
           实底中档会重演一次「比文字大一圈的砖」。色相仍只来自 `sqarvo_brand_mid`，
           不引入第四种颜色。 */
        .sqarvo-pane-switch > button:checked {{
            background: alpha(@sqarvo_brand_mid, 0.45);
            box-shadow: none;
        }}

        /* 筛选与排序面板（popover）：密度跟对象树一致——12px 正文、24px 行高、
           11px 说明字。面板里不用 13px 标题，否则它在表格旁边比树「大一号」。 */
        .sqarvo-fs .fs-title {{ font-weight: {fw_title}; }}
        /* 面板里**只有一种字号** = 12px（与对象树一致）：正文、按钮、下拉、输入框、
           说明字统一，层级只靠字重与颜色。11/12/13/14 混用正是「看起来不统一」的来源。 */
        .sqarvo-fs label,
        .sqarvo-fs button,
        .sqarvo-fs entry,
        .sqarvo-fs dropdown {{ font-size: {ui}px; }}
        .sqarvo-fs button {{ min-height: {fs_button_min_height}px; padding: {gap_xxs}px {gap_lg}px; }}
        /* 纯符号下拉（算子 / 方向）不显示右侧箭头：旁边就是值，箭头只是噪音。
           `arrow` 是 GTK 的下拉箭头 CSS **节点名**（`dropdown arrow`，见 GTK 默认主题
           里的 `dropdown arrow {{ -gtk-icon-source: ... }}`），不是类名。 */
        .sqarvo-fs dropdown.fs-symbol arrow {{
            -gtk-icon-source: none;
            min-width: 0;
            min-height: 0;
            padding: 0;
            margin: 0;
        }}
        /* 条件 / 排序清单的行：24px 行高（与对象树一致）。已加的条件 / 排序键是淡底色 +
           圆角的「实体条」，点它展开编辑器时形态连续，不会一行文字突然炸成控件。
           底色取浅档（55%）：条目是常驻的，实底浅档会盖过面板本身的层次。
           描边（#161）用 `@sqarvo_overlay` 的 inset 环：面板底色与条件条只差一档浅紫，
           暗色下几乎看不出是一条「条」；inset 环不占尺寸，24px 行高与切档口径都不动。 */
        .sqarvo-fs .fs-row {{
            min-height: {tree_row}px;
            padding: 0 {gap_xs}px;
            border-radius: {radius_sm}px;
            background: alpha(@sqarvo_brand_soft, 0.55);
            box-shadow: inset 0 0 0 1px @sqarvo_overlay, 0 1px 2px alpha(black, 0.06);
        }}
        .sqarvo-fs .fs-row:hover {{ background: @sqarvo_brand_soft; }}
        /* 行内控件 / 按钮都压在 24px：编辑态与只读态行高相同，点一下就换内容，
           整行不跳高。 */
        .sqarvo-fs .fs-row entry,
        .sqarvo-fs .fs-row dropdown {{
            min-height: {target}px;
            padding-top: 0;
            padding-bottom: 0;
        }}
        .sqarvo-fs .fs-row button {{
            min-height: {secondary_min_height}px;
            min-width: {secondary_min_height}px;
            padding: 0 {gap_xxs}px;
        }}
        .sqarvo-fs .fs-action {{ min-height: {fs_action_min_height}px; padding: {gap_xxs}px {gap_xxl}px; }}

        /* 当前种类高亮：工具条上的种类入口（表 / 视图 / 函数 / 查询）不再禁用，
           高亮只表示「现在列的是这一类」（v0.2.3.3 起它们会打开 Objects 页）。
           这是「当前选中的是哪一个」→ 中档实底；原来是 accent 20% 的淡 tint，
           和标签 / 表格行的选中态不是同一个颜色。 */
        .sqarvo-kind-active {{
            background: @sqarvo_brand_mid;
            color: @sqarvo_brand_on_mid;
        }}

        /* 检索结果行（v0.2.3.4）：与树行同高、同左边距。
           ListBox 默认按侧栏行排版（最小高度更大、左右各 6px 内边距），直接放进来
           会比树行更高、图标右移——检索结果要读成「同一棵树，只是无关项不出现」。 */
        window.{window_class} list.sqarvo-search-list > row {{
            min-height: {tree_row}px;
            /* 侧栏行默认左右各 6px 外边距 + 圆角内边距：两者都要归零，结果行才与
               树行对齐（树是 ListView，行本身没有这两样）。 */
            margin: 0;
            /* 2px 左内边距与 `listview > row`（树）的默认内边距一致：少了它整列
               内容会比树左移 2px，缩放后看得很清楚。 */
            padding: {gap_xxs}px {gap_xxs}px;
        }}

        /* 数据页的强调底（原来是 `data_page::install_css` 里按系统强调色算的半透明底）。
           规则搬到这里是因为色值只在本样式表定义：另一个 provider 里的 `@sqarvo_brand_*`
           解析不到（GTK 的命名颜色是 per-provider 的），写 hex 又会变成第二份色板。
           这些类都带文字，所以底色 + 前景成对写：`@sqarvo_brand_on_soft` 在浅 / 暗两套
           色板里分别是深色文字与近白，不会出现「白字压浅紫」。 */
        .sqarvo-cell-dirty {{
            background: @sqarvo_brand_soft;
            color: @sqarvo_brand_on_soft;
        }}
        /* 标记删除的行（未提交）：删除线 + 压暗。v0.2.5 之前这条规则只在数据页自己的
           provider 里，查询结果区没有打开表数据页时那条规则根本没装——查询标签里标删
           一行看不到删除线（用户反馈「没有脏行交互」）。搬到全局样式表，两处共用。 */
        .sqarvo-row-deleted {{
            text-decoration: line-through;
            opacity: 0.55;
        }}
        .sqarvo-txn-commit {{
            background: @sqarvo_brand_soft;
            color: @sqarvo_brand_on_soft;
        }}
        .sqarvo-column-info-focus {{ background: @sqarvo_brand_soft; }}
        .sqarvo-column-header:hover {{ background: alpha(@sqarvo_brand_soft, 0.55); }}
        .sqarvo-badge-view {{
            background: @sqarvo_brand_soft;
            color: @sqarvo_brand_on_soft;
        }}
        /* 文本选区块（输入框 / 可复制文本里拖选）：libadwaita 默认是 accent 30%。
           原先是**只给底色**（浅档淡紫）且不动前景——暗色下浅色正文压在淡紫底上几乎
           读不出来（「选中底色盖住文字」）。现在底色与前景成对取自中档：
           `@sqarvo_brand_mid` + `@sqarvo_brand_on_mid`（标签选中态已经在用这一对，
           浅色 3.89:1、暗色取 `@view_bg_color`），两套色板下选中的字都是可读的。 */
        selection:focus-within {{
            background-color: @sqarvo_brand_mid;
            color: @sqarvo_brand_on_mid;
        }}
        "#,
        deep = palette.deep,
        mid = palette.mid,
        soft = palette.soft,
        tab_hover = tones.tab_hover,
        tab_selected = tones.tab_selected,
        on_deep = palette.on_deep,
        on_mid = palette.on_mid,
        on_soft = palette.on_soft,
        separator = tones.separator,
        frame = tones.frame,
        overlay = tones.overlay,
        status_fg = tones.status_fg,
        window_class = WINDOW_CLASS,
        // 尺寸与字号一律从当前档位的度量取（#159）：样式表里不再写死像素值。
        // 例外只有两类：1px 描边 / 分隔线（§3.2：任何档位都保持 1px），
        // 以及两处阴影的偏移 / 模糊值（不是间距，度量表里没有这一项）。
        ui = m.font_ui,
        title = m.font_title,
        status = m.status_height,
        status_font = m.font_status,
        data_font = m.font_data_cell,
        panel_font = m.font_panel,
        fw_ui = m.font_weight_ui,
        fw_cell = m.font_weight_cell,
        fw_title = m.font_weight_title,
        // 高对比下不施加 heading 字重：HC 主题靠描边与色差表达层级，而原 Adwaita
        // `.heading` 在 HC 主题里的实际字重不归本仓库所有（libadwaita 的 HC 样式表
        // 不在本机可核对范围内）。显式回到正文字重，保证「HC 下标题不会比改动前更粗」。
        fw_heading = if high_contrast {
            m.font_weight_ui
        } else {
            m.font_weight_heading
        },
        toolbar = m.toolbar_height,
        toolbar_padding_h = m.toolbar_padding_h,
        tree_row = m.row_height_tree,
        data_row = m.row_height_data,
        target = m.min_target,
        tab_strip = m.tab_strip_height,
        icon_button_class = crate::icons::BUTTON_CLASS,
        icon_button_width = m.icon_button_width,
        icon_button_height = m.icon_button_height,
        status_dot = m.status_dot_diameter,
        status_dot_button = STATUS_DOT_BUTTON_CLASS,
        status_dot_dirty = STATUS_DOT_DIRTY_CLASS,
        status_dot_txn = STATUS_DOT_TXN_CLASS,
        column_header_min_height = m.column_header_min_height,
        frame_border_width = m.frame_border_width,
        radius_sm = m.radius_sm,
        radius_md = m.radius_md,
        pill_radius = m.pill_radius,
        objects_min_width = m.objects_min_width,
        pager_button_min_width = m.pager_button_min_width,
        pager_font = m.pager_font,
        fs_button_min_height = m.fs_button_min_height,
        fs_action_min_height = m.fs_action_min_height,
        secondary_min_height = m.secondary_min_height,
        gap_xxs = m.gap_px(Gap::Xxs),
        gap_xs = m.gap_px(Gap::Xs),
        gap_sm = m.gap_px(Gap::Sm),
        gap_md = m.gap_px(Gap::Md),
        gap_lg = m.gap_px(Gap::Lg),
        gap_xl = m.gap_px(Gap::Xl),
        gap_xxl = m.gap_px(Gap::Xxl),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_weights_come_from_metrics() {
        // 四档字重是规范值（metrics.toml 的 font_weight_*）：样式表里不许再有字面量，
        // 也不许再借 Adwaita 的 `.heading`（它只设 700，但值归依赖所有）。
        let css = stylesheet();
        let m = layout::Metrics::for_tier(Tier::Compact);
        assert_eq!(m.font_weight_ui, 400, "正文字重是 400");
        assert_eq!(m.font_weight_cell, 500, "单元格字重是 500");
        assert_eq!(m.font_weight_title, 600, "标题字重是 600");
        assert_eq!(m.font_weight_heading, 700, "卡片 / 面板标题字重是 700");

        assert!(
            css.contains(&format!(
                ".sqarvo-heading {{ font-weight: {}; }}",
                m.font_weight_heading
            )),
            "缺少 .sqarvo-heading 规则"
        );

        // 高对比下不许加粗：HC 主题靠描边与色差表达层级，且原 Adwaita `.heading`
        // 在 HC 里的字重不归本仓库所有。这里锁住「HC 样式表里 heading = 正文字重」。
        let hc = stylesheet_for(false, true, Tier::Compact);
        assert!(
            hc.contains(&format!(
                ".sqarvo-heading {{ font-weight: {}; }}",
                m.font_weight_ui
            )),
            "高对比下 .sqarvo-heading 必须回到正文字重"
        );
        assert!(
            !hc.contains(&format!(
                ".sqarvo-heading {{ font-weight: {}; }}",
                m.font_weight_heading
            )),
            "高对比下不许再出现 heading 的 700 字重"
        );
        assert!(css.contains(&format!(
            ".sqarvo-title {{ font-size: {}px; font-weight: {}; }}",
            m.font_title, m.font_weight_title
        )));
        assert!(css.contains(&format!(
            ".sqarvo-cell {{ font-size: {}px; font-weight: {}; }}",
            m.font_data_cell, m.font_weight_cell
        )));
        assert!(css.contains(&format!(
            ".sqarvo-panel-text {{ font-size: {}px; font-weight: {}; }}",
            m.font_panel, m.font_weight_ui
        )));
    }

    #[test]
    fn stylesheet_carries_the_compact_metrics() {
        let css = stylesheet();
        assert!(
            css.contains(&format!("min-height: {}px", layout::TOOLBAR_HEIGHT)),
            "工具条高度缺失：{css}"
        );
        assert!(css.contains("min-height: 48px"), "状态栏高度缺失");
        assert!(css.contains("min-height: 24px"), "树行高缺失");
        assert!(css.contains("min-height: 32px"), "数据行高缺失");
        assert!(css.contains("font-size: 12px"), "界面字号缺失");
        assert!(css.contains("font-size: 13px"), "标题字号缺失");
        assert!(css.contains("font-size: 11px"), "状态栏字号缺失");
        assert!(
            css.contains(&format!(
                ".sqarvo-cell {{ font-size: {}px; font-weight: {}; }}",
                layout::FONT_DATA_CELL,
                layout::Metrics::for_tier(Tier::Compact).font_weight_cell
            )),
            "数据单元格字号 / 字重缺失（12px 会裁掉 CJK 笔画，13px Regular 会发虚）"
        );
        assert!(
            css.contains(&format!("min-height: {}px", layout::TAB_STRIP_HEIGHT)),
            "标签条高度缺失"
        );
    }

    #[test]
    fn panel_text_is_one_metric() {
        // 用户要求：DDL 正文与「表信息」内容的字体 / 字号 / 格式一致，不加粗、10px——
        // 视觉重心在数据网格，面板文字不许抢注意力。
        let css = stylesheet();
        let text = format!(
            ".sqarvo-panel-text {{ font-size: {}px; font-weight: {}; }}",
            layout::FONT_PANEL,
            layout::Metrics::for_tier(Tier::Compact).font_weight_ui
        );
        assert!(css.contains(&text), "面板正文类缺失：{css}");
        assert_eq!(layout::FONT_PANEL, 10, "面板正文是 10px 档");
        // 面板里不能再出现更大 / 更粗的正文类，也不再用数据网格的类。
        assert!(!css.contains(".sqarvo-panel-heading"));
        assert!(
            !css.contains(".sqarvo-ddl-text"),
            "面板不应再挂数据网格的类"
        );
    }

    #[test]
    fn pane_switch_is_a_small_round_block() {
        // 用户要求：选中态环绕文字、四个圆角、更小更贴近字体。分段容器（`.linked`）
        // 只圆外侧两个角，所以规则必须挂在独立 toggle 上。
        let css = stylesheet();
        let start = css
            .find(".sqarvo-pane-switch > button {")
            .expect("缺少 [DDL] / [表信息] 切换的按钮规则");
        let rule = &css[start..start + css[start..].find('}').expect("规则应当是闭合的")];
        assert!(
            rule.contains("border-radius: 6px;"),
            "选中块要有圆角：{rule}"
        );
        assert!(
            rule.contains("padding: 0 4px;"),
            "内边距要收小（贴着文字）：{rule}"
        );
        assert!(
            rule.contains(&format!("min-height: {}px;", layout::MIN_TARGET)),
            "命中区不能低于最小点击区：{rule}"
        );
        let checked = css
            .find(".sqarvo-pane-switch > button:checked {")
            .expect("缺少选中态规则");
        let checked_rule = &css[checked..checked + css[checked..].find('}').unwrap_or(0)];
        // Adwaita 的 `button:checked` 是 30% 的整块底色，在 280 宽的面板里像一块砖。
        // 选中态现在是中档品牌色的 45%（色相只来自 `sqarvo_brand_mid`，不引入第四种颜色）。
        assert!(
            checked_rule.contains("background: alpha(@sqarvo_brand_mid, 0.45);"),
            "选中态应是中档品牌色的半透明底：{checked_rule}"
        );
        assert!(
            checked_rule.contains("box-shadow: none;"),
            "不要整块阴影：{checked_rule}"
        );
    }

    #[test]
    fn grid_draws_its_right_edge() {
        let css = stylesheet();
        // 用户反馈「表格绘制时右侧边框遗漏」：Adwaita 的列分隔线是单元格的 border-left，
        // 最后一列没有右边线。规则要覆盖数据行与表头两处（表头已合并成一行，没有独立的
        // 类型条带），否则两者右侧不齐。
        for selector in [
            "listview > row > cell:last-child",
            "header > button:last-child",
        ] {
            assert!(css.contains(selector), "缺少表格右边界规则：{selector}");
        }
        assert!(
            css.contains("border-right: 1px solid @sqarvo_separator"),
            "右边界线宽 / 颜色要与列分隔线一致"
        );
    }

    #[test]
    fn grid_header_is_one_row_with_type_strip_background() {
        let css = stylesheet();
        // 表头合并成一行（「列名 · 原始类型」）：底色取原来那条类型提示行的窗口底色，
        // 前景必须与数据行同档（Adwaita 的表头按钮默认是弱化前景色，实测 #b4b4b4）。
        // 表头规则把数据网格与 `Objects` 两个选择器写在一起（组内以逗号结尾），
        // 所以锚点取带逗号的那一行。
        let start = css
            .find("columnview.sqarvo-grid > header > button,")
            .expect("缺少合并后表头按钮的规则");
        let rule = &css[start..start + css[start..].find('}').expect("规则应当是闭合的")];
        assert!(
            rule.contains("background-color: @theme_bg_color;"),
            "表头底色要取原类型条带的窗口底色：{rule}"
        );
        assert!(
            rule.contains("color: @theme_fg_color;"),
            "表头文字必须可读，不能沿用 Adwaita 的弱化前景色：{rule}"
        );
        assert!(
            rule.contains("min-height: 26px;"),
            "表头是单行 30px 条带（26 + 上下 2px）：{rule}"
        );
        // 旧的两行表头 / 类型条带必须连样式一起消失，不能留孤立的类。
        for dead in [
            ".sqarvo-type-row",
            ".sqarvo-type-cell",
            ".sqarvo-header-name",
            ".sqarvo-header-type",
        ] {
            assert!(!css.contains(dead), "两行表头已删除，样式里不该再有 {dead}");
        }
    }

    #[test]
    fn grid_draws_its_bottom_edge() {
        let css = stylesheet();
        // 用户反馈「数据少时表格底侧边框遗漏」：Adwaita 的行分隔线是 `border-top`，
        // 最后一行没有下边线。规则必须挂在**行**上（`columnview` / `listview` 自己的
        // `border-bottom` 不会渲染），而且只在内容不足一屏时命中 `:last-child`。
        assert!(
            css.contains(
                "listview > row:last-child {\n            border-bottom: 1px solid @sqarvo_separator;"
            ),
            "缺少表格下边界规则"
        );
    }

    #[test]
    fn both_tables_share_one_frame_rule() {
        // `Objects` 列表与表数据网格各自套一圈 2px / 8px 圆角外框，两块表格同一份口径：
        // 线挂在容器上（`.sqarvo-table-frame`），行 / 单元格不加整圈边框，否则会和
        // `cell:last-child` 的右边界叠成双线。
        let css = stylesheet();
        let start = css.find(".sqarvo-table-frame {").expect("缺少表格外框规则");
        let end = start + css[start..].find('}').expect("规则应当闭合");
        let rule = &css[start..end];
        assert!(
            rule.contains("border: 2px solid @sqarvo_frame"),
            "外框要比分隔线实一档、粗一档（2px / 30%）：{rule}"
        );
        assert!(
            rule.contains("border-radius: 8px"),
            "外框用 8px 圆角（子节点由 `overflow: hidden` 裁切，不再是直角）：{rule}"
        );
        // 行级规则只允许分隔线方向（border-bottom / border-right），不能出现整圈边框。
        for selector in [
            "columnview.sqarvo-objects > listview > row {",
            "columnview.sqarvo-grid > listview > row,",
        ] {
            let start = css
                .find(selector)
                .unwrap_or_else(|| panic!("缺少规则：{selector}"));
            let brace = start + css[start..].find('{').expect("规则应当有块");
            let end = brace + css[brace..].find('}').expect("规则应当闭合");
            let rule = &css[start..end];
            assert!(
                !rule.contains("\n            border: "),
                "行上不能加整圈边框，会和最后一列的右边界叠成双线：{rule}"
            );
        }
    }

    #[test]
    fn icon_buttons_take_their_size_from_one_standard() {
        let css = stylesheet();
        // 命中区与内边距只有一份口径：图标的像素档位由 icons::OpticalWeight 决定。
        assert!(css.contains(&format!(
            "button.{} {{\n            padding: 0;\n            min-width: {}px;\n            min-height: {}px;",
            crate::icons::BUTTON_CLASS,
            layout::ICON_BUTTON_WIDTH,
            layout::ICON_BUTTON_HEIGHT
        )));
    }

    #[test]
    fn status_dot_is_half_the_funnel_in_a_full_target() {
        let css = stylesheet();
        // 圆点 = 「事务徽标 + 脏行计数」两段常驻文字的唯一可见形态：视觉是一枚 7px 的
        // 实心圆（约漏斗墨迹 12×11 的一半），命中区仍是 24×24（§11：视觉可以小，
        // hover / 点击区不许小）。
        assert!(
            css.contains(&format!(
                "button.{} {{\n            padding: 0;\n            min-width: {}px;\n            min-height: {}px;",
                STATUS_DOT_BUTTON_CLASS,
                layout::MIN_TARGET,
                layout::MIN_TARGET
            )),
            "圆点命中区必须是 24×24、内边距 0：{css}"
        );
        // 圆点直径必须是 6–7px（用户口径），且不超过漏斗图标盒的一半。
        const { assert!(layout::STATUS_DOT_DIAMETER * 2 <= layout::ICON_BOX) };
        const { assert!(layout::STATUS_DOT_DIAMETER == 6 || layout::STATUS_DOT_DIAMETER == 7) };
        // 三态各用一个颜色 token，不许写死 hex：未激活 = 前景色 35% 的灰；事务已打开
        // = `@warning_color`（状态行 `.warning` 文字用的就是 `var(--warning-color)`，
        // 所以圆点棕色与原来那句「事务状态：事务中」同源）；有脏行 = suggested-action
        // 用的 `@accent_bg_color`（Yaru 下橙红）。
        let start = css.find(".sqarvo-status-dot {").expect("缺少状态圆点规则");
        let rule = &css[start..start + css[start..].find('}').expect("规则应当闭合")];
        assert!(
            rule.contains("background-color: alpha(@window_fg_color, 0.35);"),
            "未激活态是灰点：{rule}"
        );
        assert!(
            rule.contains("border-radius: 99px;"),
            "圆点必须是圆的（999 会被钳到半个盒子）：{rule}"
        );
        let rule = |selector: String| -> &str {
            let start = css
                .find(&selector)
                .unwrap_or_else(|| panic!("缺少规则：{selector}"));
            &css[start..start + css[start..].find('}').expect("规则应当闭合")]
        };
        let txn = rule(format!(
            ".sqarvo-status-dot.{} {{\n            background-color: @warning_color;",
            STATUS_DOT_TXN_CLASS
        ));
        assert!(
            !txn.contains('#'),
            "事务激活态必须用 token（@warning_color），不许写死 hex：{txn}"
        );
        let dirty = rule(format!(
            ".sqarvo-status-dot.{} {{\n            background-color: @accent_bg_color;",
            STATUS_DOT_DIRTY_CLASS
        ));
        assert!(
            !dirty.contains('#'),
            "有脏行必须是 accent 底色（与 suggested-action 同一 token）：{dirty}"
        );
        // 优先级写进 CSS 顺序：脏行规则排在事务规则之后，两枚 class 万一同时出现
        // （状态刷新与脏行刷新交错）也是 accent 赢——与 `sync_status_dot` 的
        // 「脏行 > 事务 > 灰」同一口径。
        assert!(
            css.find(&format!(".sqarvo-status-dot.{STATUS_DOT_DIRTY_CLASS} {{"))
                > css.find(&format!(".sqarvo-status-dot.{STATUS_DOT_TXN_CLASS} {{")),
            "脏行规则必须排在事务规则之后（accent 压过 warning）"
        );
    }

    #[test]
    fn objects_table_uses_the_grid_row_metrics() {
        // 用户要求（v0.2.4）：`Objects` 页改成与数据网格同一套三列表格（行号 | 表名 | 类型）。
        // 行高 / 表头高度不各写一份——两条规则必须把 `columnview.sqarvo-grid` 与
        // `columnview.sqarvo-objects` 并列，改网格时两张表一起变。
        let css = stylesheet();
        // 返回「选择器组 + 规则体」：规则可能把数据网格与 `Objects` 两个选择器写在一起，
        // 所以从命中的选择器向上把以逗号结尾的上一行也一起吃进来。
        let rule = |selector: &str| -> String {
            let mut start = css
                .find(selector)
                .unwrap_or_else(|| panic!("缺少规则：{selector}"));
            while let Some(newline) = css[..start].rfind('\n') {
                let line_start = css[..newline].rfind('\n').map_or(0, |i| i + 1);
                if !css[line_start..newline].trim_end().ends_with(',') {
                    break;
                }
                start = line_start;
            }
            let brace = start + css[start..].find('{').expect("规则应当有块");
            let end = brace + css[brace..].find('}').expect("规则应当闭合");
            css[start..end].to_owned()
        };
        let row = rule("columnview.sqarvo-objects > listview > row {");
        assert!(
            row.contains("columnview.sqarvo-grid > listview > row"),
            "行高规则必须与数据网格共用同一条：{row}"
        );
        assert!(
            row.contains(&format!("min-height: {}px;", layout::ROW_HEIGHT_DATA)),
            "行高必须跟数据行同档：{row}"
        );
        let header = rule("columnview.sqarvo-objects > header > button");
        assert!(
            header.contains("columnview.sqarvo-grid > header > button"),
            "表头高度必须与数据网格共用同一条：{header}"
        );
        // 表头已合并成一行（「列名 · 类型」），两张表都取同一条 26px 内容高
        // （+ 上下 2px 内距 = 30px 条带），见 `grid_header_is_one_row_with_type_strip_background`。
        assert!(
            header.contains("min-height: 26px;"),
            "表头必须跟网格的单行表头同档（26）：{header}"
        );
        // 单元格上下内距必须归零：`Objects` 的单元格是「图标 + 名称」的 Box，Adwaita
        // 给的上下内距会把行撑到 35（实测），与网格的 32 差 3px。
        let cell = rule("columnview.sqarvo-objects > listview > row > cell {");
        assert!(
            cell.contains("padding-top: 0") && cell.contains("padding-bottom: 0"),
            "单元格上下内距要归零，行高才由 min-height 决定（32）：{cell}"
        );
        let bottom = rule("columnview.sqarvo-objects > listview > row:last-child");
        assert!(
            bottom.contains("border-bottom: 1px solid @sqarvo_separator"),
            "不足一屏时表格下边界要与网格同色同宽：{bottom}"
        );
        // 旧单列列表（`GtkListView`）的规则已经删掉：留着就是第二套口径。
        assert!(
            !css.contains("listview.sqarvo-objects"),
            "旧单列列表的 CSS 仍有残留"
        );
    }

    #[test]
    fn stylesheet_tracks_the_tier_metrics() {
        // 三档的样式表必须真的不同，且数字来自 `metrics.toml`：行高 24/30/36、
        // 数据行 32/40/48、界面字号 12/15/18、标签条 24/30/36。
        for (tier, tree_row, data_row, font_ui, tab_strip) in [
            (Tier::Compact, 24, 32, 12, 24),
            (Tier::Comfortable, 30, 40, 15, 30),
            (Tier::Large, 36, 48, 18, 36),
        ] {
            let css = stylesheet_for(false, false, tier);
            assert!(
                css.contains(&format!("listview > row {{ min-height: {tree_row}px; }}")),
                "{}：树行高不是 {tree_row}px",
                tier.id()
            );
            assert!(
                css.contains(&format!("min-height: {data_row}px;")),
                "{}：数据行高不是 {data_row}px",
                tier.id()
            );
            assert!(
                css.contains(&format!(
                    "window.sqarvo-compact {{ font-size: {font_ui}px; }}"
                )),
                "{}：界面字号不是 {font_ui}px",
                tier.id()
            );
            assert!(
                css.contains(&format!(
                    ".sqarvo-tab-strip {{\n            min-height: {tab_strip}px;"
                )),
                "{}：标签条高度不是 {tab_strip}px",
                tier.id()
            );
            // 1px 描边在任何档位都不变（§3.2）。
            assert!(css.contains("border-bottom: 1px solid @sqarvo_separator"));
        }
    }

    #[test]
    fn stylesheet_never_disables_the_focus_ring() {
        let css = stylesheet();
        // 紧凑化不能靠抹掉焦点可见性换密度（§11）。
        assert!(!css.contains("outline: none"));
        assert!(!css.contains("outline: 0"));
    }

    #[test]
    fn toolbar_kind_labels_are_optically_centered_against_cjk() {
        // 种类入口是纯拉丁文字，和中文「新建查询」同一行：Pango 的中文 em 盒更靠下，
        // 盒子同高同 y 也会差 2px。这条补偿规则不能悄悄丢（丢了就是「一高一低」）。
        let css = stylesheet();
        assert!(
            css.contains(".sqarvo-toolbar button.sqarvo-kind label { margin-top: 2px; }"),
            "缺少种类入口的字面补偿规则"
        );
        // 命中区不变：按钮本身仍是同一个最小高度与内距。
        assert!(css.contains(&format!(
            ".sqarvo-toolbar button {{ min-height: {}px; padding: 2px 10px; }}",
            crate::layout::MIN_TARGET
        )));
    }

    #[test]
    fn compact_rules_are_scoped_to_the_window_class() {
        let css = stylesheet();
        assert!(css.contains("window.sqarvo-compact"));
        // 行高规则必须挂在窗口 class 下，否则对话框里的列表也会被压扁。
        for rule in ["listview > row", "columnview.sqarvo-grid > listview > row"] {
            let index = css.find(rule).expect("规则应当存在");
            let line_start = css[..index].rfind('\n').map_or(0, |newline| newline + 1);
            assert!(
                css[line_start..index].contains("window.sqarvo-compact"),
                "规则 {rule} 没有挂在窗口 class 下"
            );
        }
    }

    #[test]
    fn tab_strip_keeps_exactly_one_bottom_line() {
        // 标签条与下方动作行之间只能有一条线：`AdwTabBar` 的 `.box` 自带
        // `box-shadow: inset 0 -1px var(--headerbar-shade-color)`，与
        // `.sqarvo-tab-strip` 的 `border-bottom` 紧贴成两条 1px 线（用户反馈
        // 「分割线像两条线叠在一起」）。这条覆盖保证「压掉 shade、只留 10%」不被删。
        let css = stylesheet();
        let start = css
            .find(".sqarvo-tab-strip {\n")
            .expect("标签条规则应当存在");
        let end = start + css[start..].find('}').expect("规则应当闭合");
        assert!(
            css[start..end].contains("border-bottom: 1px solid @sqarvo_separator"),
            "标签条要保留自己那条 10% 下边线（与工具条 / 状态栏同口径）"
        );
        assert!(
            css.contains(".sqarvo-tab-strip tabbar .box {\n            box-shadow: none;"),
            "缺少压掉 AdwTabBar headerbar shade 的规则：标签条下会出现两条叠在一起的线"
        );
    }

    /// 检索结果行必须与树行**同高、同左边距**（v0.2.3.4）：结果要读成同一棵树，
    /// 只是无关项不出现。删掉这条覆盖，ListBox 侧栏行会把结果撑得比树行高。
    #[test]
    fn search_rows_use_the_tree_row_metrics() {
        let css = stylesheet();
        let start = css
            .find("list.sqarvo-search-list > row {")
            .expect("缺少检索结果行的高度覆盖");
        let end = start + css[start..].find('}').expect("规则应当闭合");
        let rule = &css[start..end];
        assert!(
            rule.contains("min-height: 24px"),
            "检索结果行应当与树行同为 24px：{rule}"
        );
        assert!(
            rule.contains("margin: 0"),
            "侧栏行的左右外边距必须归零，否则整列比树右移 6px：{rule}"
        );
        assert!(
            rule.contains("padding: 2px 2px"),
            "内边距要与 listview 行一致（上下 2、左右 2）：{rule}"
        );
        assert!(
            !css.contains("sqarvo-search-hit"),
            "命中高亮已删除：检索结果不再与树里其它行看起来不一样"
        );
    }

    /// 品牌三档色板：色值只在 `theme.rs` 定义一次，强调 / 选中 / hover 三类
    /// 背景全部从 token 取。
    ///
    /// 两条口径钉死：① 深档只给实底强调（`suggested-action`），中档只给「当前选中是哪个」
    /// （对象树 / 检索结果 / 数据网格 / `Objects` 表格 / 标签 / 种类入口），浅档只给淡填充；
    /// ② 除状态圆点的「脏行 = accent 红」以外，样式表里不许再出现 `alpha(@accent_*)`
    /// 派生的底色——那正是这一轮要收掉的一批系统强调色。
    #[test]
    fn brand_palette_is_defined_once_and_covers_every_fill() {
        let css = stylesheet();
        for token in [
            "@define-color sqarvo_brand_deep #5B3256;",
            "@define-color sqarvo_brand_mid #A66FA0;",
            "@define-color sqarvo_brand_soft #D8B7D3;",
            "@define-color sqarvo_brand_on_deep #FFFFFF;",
            "@define-color sqarvo_brand_on_mid #FFFFFF;",
            "@define-color sqarvo_brand_on_soft @theme_fg_color;",
        ] {
            assert!(css.contains(token), "缺少品牌色 token：{token}");
        }
        // 色值只有一份：同一个 hex 在样式表里出现两次就是第二份色板。
        for hex in ["#5B3256", "#A66FA0", "#D8B7D3"] {
            assert_eq!(
                css.matches(hex).count(),
                1,
                "{hex} 应当只在 token 处出现一次"
            );
        }
        // 暗色：同一组 token 换一套亮度。选择由 Rust 侧按 `AdwStyleManager::is_dark()` 做，
        // 不是 CSS 媒体查询——实测 `@media (prefers-color-scheme: dark)` 里的
        // `@define-color` 在应用级 provider 里不生效（窗口已暗、按钮仍是浅档色）。
        let dark = stylesheet_for(true, false, Tier::Compact);
        for token in [
            "@define-color sqarvo_brand_deep #7A456F;",
            "@define-color sqarvo_brand_mid #B98AB2;",
            "@define-color sqarvo_brand_soft #4A2E45;",
            "@define-color sqarvo_brand_on_mid @view_bg_color;",
        ] {
            assert!(dark.contains(token), "暗色缺档：{token}");
        }
        // 两套色板互不泄漏：浅色表里不该出现暗色值，暗色表里也不该出现浅色值。
        for light_hex in ["#5B3256", "#A66FA0", "#D8B7D3"] {
            assert!(
                !dark.contains(light_hex),
                "暗色样式表里混进了浅色值 {light_hex}"
            );
        }
        for dark_hex in ["#7A456F", "#B98AB2", "#4A2E45"] {
            assert!(
                !css.contains(dark_hex),
                "浅色样式表里混进了暗色值 {dark_hex}"
            );
        }

        // 深档：实底强调按钮。
        assert!(
            css.contains(
                "button.suggested-action {\n            background-color: @sqarvo_brand_deep;"
            ),
            "主操作按钮要取深档实底"
        );
        // 中档：五处「当前选中是哪个」写进同一条规则（两张表 / 树 / 检索结果）。
        let start = css
            .find("listview.sqarvo-tree > row:selected,")
            .expect("缺少选中态规则（对象树）");
        let mid_rule = &css[start..start + css[start..].find('}').expect("规则应当闭合")];
        assert!(
            mid_rule.contains("background-color: @sqarvo_brand_mid;")
                && mid_rule.contains("color: @sqarvo_brand_on_mid;"),
            "选中态要用中档 + 配对前景：{mid_rule}"
        );
        for selector in [
            "list.sqarvo-search-list > row:selected",
            "columnview.sqarvo-grid > listview > row:selected",
            "columnview.sqarvo-objects > listview > row:selected",
        ] {
            assert!(
                mid_rule.contains(selector),
                "选中态漏了这个控件：{selector}"
            );
        }
        // 标签条与种类入口同样是中档。
        assert!(
            css.contains(".sqarvo-tab-bar tab:selected,"),
            "标签选中底要取中档"
        );
        let kind = css
            .find(".sqarvo-kind-active {")
            .expect("缺少种类入口高亮规则");
        assert!(
            css[kind..kind + css[kind..].find('}').unwrap()]
                .contains("background: @sqarvo_brand_mid;"),
            "种类入口高亮要取中档"
        );

        // 浅档：hover 与各类淡填充。
        for rule in [
            "alpha(@sqarvo_brand_soft, 0.55)",
            "background: @sqarvo_brand_soft;",
        ] {
            assert!(css.contains(rule), "缺少浅档填充：{rule}");
        }

        // 不再有 accent 派生的**底色**（前景 token 不算）：圆点的脏行红是刻意的语义色。
        assert!(
            !css.contains("alpha(@accent_bg_color"),
            "还有 accent 派生的半透明底"
        );
        assert!(
            !css.contains("alpha(@accent_color"),
            "还有 accent 派生的半透明底"
        );
        let dot = css
            .find(".sqarvo-status-dot.sqarvo-status-dot-dirty {")
            .expect("缺少脏行圆点规则");
        assert!(
            css[dot..dot + css[dot..].find('}').unwrap()].contains("@accent_bg_color"),
            "脏行圆点保持 accent 红（语义色，本轮不动）"
        );
    }

    /// 从样式表里取一枚描边 token 的不透明度（`@define-color x alpha(@window_fg_color, 0.16)`）。
    fn tone(css: &str, token: &str) -> f32 {
        let head = format!("@define-color {token} alpha(@window_fg_color, ");
        let start = css
            .find(&head)
            .unwrap_or_else(|| panic!("缺少 token：{token}"));
        let rest = &css[start + head.len()..];
        let end = rest.find(')').expect("token 应当是闭合的 alpha()");
        rest[..end].parse().expect("不透明度应当是数字")
    }

    /// #161：高对比 = 同一组描边 token 更实一档，不是第二套色板。
    ///
    /// 三个验收点：① 高对比下四枚 token 都比同色板的普通值大；② 暗色的分隔线 / 外框 /
    /// 浮层描边比浅色实（暗底上原来的线看不见）；③ 状态栏前景不再写死 55%。
    #[test]
    fn high_contrast_thickens_the_same_border_tokens() {
        for dark in [false, true] {
            let normal = stylesheet_for(dark, false, Tier::Compact);
            let high = stylesheet_for(dark, true, Tier::Compact);
            for token in [
                "sqarvo_separator",
                "sqarvo_frame",
                "sqarvo_overlay",
                "sqarvo_status_fg",
            ] {
                assert!(
                    tone(&high, token) > tone(&normal, token),
                    "{token}：高对比必须比普通模式更实（{} → {}）",
                    tone(&normal, token),
                    tone(&high, token)
                );
            }
            // 只有浓度变：品牌色板（hex）在高对比下逐字相同。
            for hex in ["#5B3256", "#7A456F", "#A66FA0", "#B98AB2"] {
                if (dark && hex == "#5B3256") || (!dark && hex == "#7A456F") {
                    continue;
                }
                assert_eq!(
                    normal.matches(hex).count(),
                    high.matches(hex).count(),
                    "高对比不该换色相：{hex}"
                );
            }
        }

        let light = stylesheet_for(false, false, Tier::Compact);
        let dark = stylesheet_for(true, false, Tier::Compact);
        // 两套调色板的**背景亮度差太多**，alpha 数值之间没有可比性（浅色 #fafafa 上
        // 0.67 才到 3:1，暗色 #2c2c2c 上 0.16 就到 1.6:1），所以这里按各自的 WCAG 下限
        // 断言，不再要求「暗色数字必须比浅色大」：
        //   浅色：分隔线/外框要满足 1.4.11 的 3:1（实测 0.67 → 3.0:1、0.72 → 3.4:1）
        //   暗色：#161 定在 0.16 / 0.45（实测 1.63:1 与 3.89:1，与 Adwaita 网格内线同档）
        // 浅色：外框要满足 1.4.11 的 3:1（0.63 ≈ 3.14:1）；行/列分隔线不是
        // 「识别所必需」的信息，留在 0.22（≈1.5:1）以保住与外框的层次差。
        assert!(
            tone(&light, "sqarvo_frame") >= 0.63,
            "浅色表格外框要达到 3:1（≈0.63）"
        );
        assert!(
            tone(&light, "sqarvo_separator") >= 0.22,
            "浅色分隔线不低于 0.22（≈1.5:1）"
        );
        assert!(
            tone(&dark, "sqarvo_separator") >= 0.16,
            "暗色分隔线不低于 #161 的实测值（0.16）"
        );
        assert!(
            tone(&dark, "sqarvo_frame") >= 0.45,
            "暗色外框不低于 #161 的实测值（0.45）"
        );
        // 状态栏前景：浅色底更亮，反而需要更深的字（82% > 62%）。
        assert!(
            tone(&light, "sqarvo_status_fg") > tone(&dark, "sqarvo_status_fg"),
            "浅色状态栏前景要比暗色深"
        );

        // 状态栏三处（容器 / label / 不可用按钮）都取 token，不再写死 55%。
        for css in [&light, &dark] {
            assert!(
                !css.contains("alpha(@window_fg_color, 0.55)"),
                "状态栏前景不该再写死 55%"
            );
            assert_eq!(
                css.matches("color: @sqarvo_status_fg;").count(),
                3,
                "状态栏三处前景（容器 / label / :disabled 按钮）都要取同一个 token"
            );
            // 浮层边界：卡片给 1px 描边，条件条给不占尺寸的 inset 环。
            assert!(
                css.contains(
                    ".sqarvo-column-info {\n            border: 1px solid @sqarvo_overlay;"
                ),
                "浮层卡片缺少描边（暗底下黑色阴影等于没有边界）"
            );
            assert!(
                css.contains(
                    "box-shadow: inset 0 0 0 1px @sqarvo_overlay, 0 1px 2px alpha(black, 0.06);"
                ),
                "筛选条件条缺少 inset 描边（不能把 24px 行高撑大）"
            );
            // 外框与分隔线只从 token 取，样式表里不该再有 currentColor 派生的线。
            assert!(
                !css.contains("alpha(currentColor"),
                "描边不该再按 currentColor 算透明度"
            );
        }
    }

    /// #161：高对比分支必须真的出自 `AdwStyleManager::is_high_contrast()`。
    ///
    /// `stylesheet_for` 只负责取参数；读 `StyleManager` 的那一步在 [`apply_style_manager`]，
    /// 它同时写回 [`is_high_contrast`]，切档（[`reload`]）读到的才是最新值。
    #[test]
    fn high_contrast_is_read_from_the_style_manager() {
        let source = include_str!("theme.rs");
        assert!(
            source.contains("manager.is_high_contrast()"),
            "高对比必须读 AdwStyleManager，不能自己猜"
        );
        assert!(
            source.contains("connect_high_contrast_notify"),
            "高对比切换要接 StyleManager 的 notify 才会即时重载"
        );
        assert!(
            source
                .contains("stylesheet_for(is_dark(), is_high_contrast(), layout::current_tier())"),
            "切档重载要带上高对比（否则切档会把高对比分支丢掉）"
        );
    }
}
