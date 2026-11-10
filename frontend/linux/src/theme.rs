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

use std::sync::OnceLock;

use crate::layout;

/// 给窗口加 class 的名字：`GtkApplicationWindow` 上由 [`crate::ui`] 设置。
pub const WINDOW_CLASS: &str = "sqarvo-compact";

/// 表格工具条状态圆点的命中区 class（`data_page` 挂在圆点按钮上）。
///
/// 与 [`crate::icons::BUTTON_CLASS`] 分开：那一个 class 的按钮必须装一个可测量的
/// `GtkImage`（`icons::audit` 会断言），而圆点是 CSS 画的实心块、不是图标主题的字形。
pub const STATUS_DOT_BUTTON_CLASS: &str = "sqarvo-status-dot-button";

/// 状态圆点「有未提交的更改」态（灰 → accent 底色）。
pub const STATUS_DOT_DIRTY_CLASS: &str = "sqarvo-status-dot-dirty";

/// 安装紧凑密度 CSS（进程内只装一次）。
pub fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&stylesheet());
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// 紧凑密度样式表。单独成函数，测试可以检查关键尺寸是否真的写进去了。
#[must_use]
pub fn stylesheet() -> String {
    format!(
        r#"
        window.{window_class} {{ font-size: {ui}px; }}

        /* 区域 01：标题 13 semibold（标题控件在 headerbar 里） */
        .sqarvo-title {{ font-size: {title}px; font-weight: 600; }}

        /* 区域 02：主工具条 44 高（24 的按钮 + 上下各 10），按钮不小于 24 */
        .sqarvo-toolbar {{
            min-height: {toolbar}px;
            padding: 0 12px;
            border-bottom: 1px solid alpha(currentColor, 0.10);
        }}
        .sqarvo-toolbar button {{ min-height: {target}px; padding: 2px 10px; }}
        .sqarvo-toolbar label {{ font-size: {ui}px; }}
        /* 种类入口是纯拉丁文字（Tables / Views / Functions / Queries），和中文
           「新建查询」同一行混排时，Pango 的中文 em 盒比拉丁大写字高更靠下：
           盒子同高同 y 也会看起来「一高一低」。这里只把**字**下移 2px
           （实测：中文 63–74、拉丁 62–71，视觉中线差 2px），命中区不变。 */
        .sqarvo-toolbar button.sqarvo-kind label {{ margin-top: 2px; }}

        /* 区域 03 / 04：树行 24，数据行 32，表头 24 */
        window.{window_class} listview > row {{ min-height: {tree_row}px; }}
        window.{window_class} columnview.sqarvo-grid > listview > row {{
            min-height: {data_row}px;
        }}
        window.{window_class} columnview.sqarvo-grid header button {{
            min-height: {tree_row}px;
            font-size: {ui}px;
        }}

        /* 表格右边界：Adwaita 的列分隔线画在「每个单元格 / 表头按钮的 border-left」上，
           最后一列因此没有右边的竖线——窄表看起来像缺一条边，宽表被视口裁切时也看不出
           表格在哪里结束。这里给最后一列补一条右侧竖线，颜色与列分隔线一致；类型条带的
           最后一格同样补上，让表头 / 类型行 / 数据行在同一条竖线上收口。 */
        window.{window_class} columnview.sqarvo-grid > listview > row > cell:last-child,
        window.{window_class} columnview.sqarvo-grid > header > button:last-child,
        window.{window_class} .sqarvo-type-row > box.sqarvo-type-cell:last-child {{
            border-right: 1px solid alpha(currentColor, 0.10);
        }}

        /* 表格下边界（v0.2.3.3）：Adwaita 的行分隔线是每行的 `border-top`，最后一行
           因此没有下边线——数据不足一屏时表格底边是敞开的。给最后一行补一条同色的
           下边线：列表虚拟化只在内容全部实现（也就是内容不足一屏、正好需要这条线）
           时才有 `:last-child`，长表滚到底也不会在视口中间凭空多出一条。
           `columnview` / `listview` 自己的 `border-bottom` 不会画出来（它们是滚动
           内容节点），所以这条线必须挂在行上。 */
        window.{window_class} columnview.sqarvo-grid > listview > row:last-child {{
            border-bottom: 1px solid alpha(currentColor, 0.10);
        }}

        /* 表格外框（v0.2.4）：`Objects` 列表与表数据网格各套一圈 2px 线 + 8px 圆角，
           让表格从页面底色里跳出来，两块表格同一份口径。颜色与行 / 列分隔线同族
           （`currentColor` 30%，分隔线 10%）：比分隔线实一档才看得出「这是一块表格」，
           但仍弱于文字与选中态（accent 25% 起的选区底色照样压得住）。
           线只画在**容器**上（`.sqarvo-table-frame` 挂在包住表格的盒子 /
           `ScrolledWindow`），行与单元格不加线，避免与 `cell:last-child` 的右边界
           叠成双线。
           圆角靠 `GtkWidget:overflow = hidden`（`data_page.rs` / `tabs.rs` 各自在挂
           class 的控件上设）：GTK4 默认不裁子节点，上一版因此只能退回直角——首行
           选中底色会把左上角填成方角。现在容器的子节点按圆角裁切，首行 / 末行的
           底色跟着边框收圆。 */
        window.{window_class} .sqarvo-table-frame {{
            border: 2px solid alpha(currentColor, 0.30);
            border-radius: 8px;
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
        /* 灰 = 没有未提交的更改（不是可点动作，别抢注意力）。 */
        .sqarvo-status-dot {{
            border-radius: 99px;
            background-color: alpha(@window_fg_color, 0.35);
        }}
        /* 红 = 有未提交的更改：用 `@accent_bg_color`（suggested-action 的底色 token），
           与「提交」按钮同一色系——Yaru 下就是橙红，Adwaita 下随系统强调色走。 */
        .sqarvo-status-dot.{status_dot_dirty} {{
            background-color: @accent_bg_color;
        }}

        /* 区域 04 顶部：标签条 24（workspace-model §2.1 的 tabStrip） */
        .sqarvo-tab-strip {{
            min-height: {tab_strip}px;
            padding: 0 6px;
            border-bottom: 1px solid alpha(currentColor, 0.10);
        }}
        /* AdwTabBar 默认把标签条撑到 ~47px；这里压到规范口径附近（24px 行高）。
           标签本身的最小点击区仍不小于 24（§11），缩的是外层留白。 */
        .sqarvo-tab-bar {{
            padding: 0 2px;
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
            padding: 0 8px;
            font-size: {ui}px;
        }}
        .sqarvo-tab-strip button {{ min-height: {target}px; min-width: {target}px; }}
        /* 固定 `Objects` 页（v0.2.3.2）：钉住的页永远排在最前，宽度按规范口径 84px。
           `AdwTabBar` 没有 per-page 宽度 API；实测钉住页带 `pinned` 样式类
           （`[tabs] 标签实际宽度：pinned=26px`），所以按它选中。
           内容盒 84 + padding 0，避免被默认内边距撑大。 */
        .sqarvo-tab-bar tab.pinned {{
            min-width: 84px;
            padding: 0;
        }}

        /* 区域 04：固定 `Objects` 页的**单列表格**（v0.2.3.3）。
           四样口径都跟数据网格一致：行高 32、行分隔线 1px / 10%、hover 4%、
           选中 accent 25%（与 `row:selected` 在 Adwaita 里的算法同色）。
           不挂 `.navigation-sidebar`——它把选中态改成 10% 灰，与网格不是一套。 */
        window.{window_class} listview.sqarvo-objects > row {{
            min-height: {data_row}px;
            /* 覆盖 Adwaita 的 `padding: 2px`：上下为零，行距才能与数据行同为 32（+1px 线）； */
            /* 左右 8 是单元格内距（网格首列字距内容边约 14，这里图标起于 8）。 */
            padding: 0 8px;
            border-bottom: 1px solid alpha(currentColor, 0.10);
        }}
        window.{window_class} listview.sqarvo-objects > row:hover {{
            background-color: alpha(currentColor, 0.04);
        }}
        window.{window_class} listview.sqarvo-objects > row:selected {{
            background-color: alpha(@accent_color, 0.25);
        }}

        /* 区域 05：状态栏 48 高（两行各 24），字号 11 */
        .sqarvo-status-bar {{
            min-height: {status}px;
            font-size: {status_font}px;
            border-top: 1px solid alpha(currentColor, 0.10);
        }}
        /* 状态栏只有两种前景色（v0.2.3.4）：可用的按钮一个色（`@window_fg_color`），
           文字与不可用（:disabled）的按钮同一个灰（55% 前景）。三档以上会让同一行里
           `+` 很黑、`−` 半灰、页码文字又另一个灰，看起来像三套控件。
           两个坑都在 `:disabled` 上：Adwaita 的 `.flat:disabled` 会再叠一层 opacity
           （实测黑色图标被压到 #AFAFAF），`dim-label` 自己也带 0.55 透明度——全部用
           `opacity: 1` 关掉，颜色才由这两条规则唯一决定。 */
        window.{window_class} .sqarvo-status-bar {{
            color: alpha(@window_fg_color, 0.55);
        }}
        window.{window_class} .sqarvo-status-bar label {{
            color: alpha(@window_fg_color, 0.55);
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
        window.{window_class} .sqarvo-status-bar button:disabled,
        window.{window_class} .sqarvo-status-bar button.flat:disabled,
        window.{window_class} .sqarvo-status-bar button.sqarvo-icon-button:disabled {{
            color: alpha(@window_fg_color, 0.55);
            opacity: 1;
            filter: none;
        }}
        /* v0.2.3.3：状态栏分成「对象树功能区的左边沟 | 竖分割线 | 右列」。
           左边沟宽度由 shell 跟着侧栏分栏位置设（`set_gutter_width`），右列自己带
           左右 12px 内距——这样分割线与上面的分栏线在同一根轴上。 */
        .sqarvo-status-gutter {{ min-height: {status}px; }}
        .sqarvo-status-divider {{
            min-width: 1px;
            background: alpha(currentColor, 0.10);
        }}
        .sqarvo-status-right {{ padding: 0 12px; }}
        .sqarvo-status-row {{ min-height: {tree_row}px; }}
        .sqarvo-status-bar button {{
            min-height: {target}px;
            min-width: {target}px;
            padding: 0 8px;
            font-size: {status_font}px;
        }}
        .sqarvo-status-bar entry {{ min-height: {target}px; font-size: {status_font}px; }}
        /* 分页器（v0.2.3.4）：尺寸对齐状态栏行操作里的 `+`（16px 图标盒、墨迹 11×11、
           命中区 24 高）。图标 14px 时墨迹正好落在 11×11；按钮回到 24 高，与 `+` 同一档。
           页码文字同步放大到 12px（沿用状态栏的 UI 字族，中文走系统 CJK 回退），
           与图标的视觉高度对齐。 */
        window.{window_class} .sqarvo-pager button {{
            min-height: {target}px;
            min-width: 26px;
            padding: 0 4px;
        }}
        window.{window_class} .sqarvo-pager-label {{
            font-size: 12px;
        }}
        .sqarvo-status-sep {{ min-height: {tree_row}px; }}

        /* 区域 06：信息面板（空槽，内容由 #119 填） */
        .sqarvo-inspector {{
            border-left: 1px solid alpha(currentColor, 0.10);
            padding: 12px;
        }}
        .sqarvo-slot-note {{ font-size: {status_font}px; }}

        /* 数据网格单元格（结果表 / 表数据）：12px 下 GTK4 会裁掉部分 CJK 字形
           笔画（用户反馈「张三 / 王五」显示残缺）。13px 起完整；为在 1x 屏幕上
           避免 Regular 横画发虚，字号保持 13px、字重提升到 Medium（500）。 */
        .sqarvo-cell {{ font-size: {data_font}px; font-weight: 500; }}

        /* 信息面板正文（[DDL] 与 [表信息] 两页**同一口径**）：10px、不加粗。面板是
           次要信息，视觉重心在数据网格——字号和字重都不许跟表格抢注意力。分层只靠
           颜色（标签 dim）与分节间距，不再用 Medium / Semibold。 */
        .sqarvo-panel-text {{ font-size: {panel_font}px; font-weight: 400; }}

        /* 信息面板的 [DDL] / [表信息]（v0.2.3.3）：两个独立 toggle，选中态是贴着
           文字的**小圆角块，四个角都圆**。不用 `.linked` 分段容器：分段样式只圆外侧
           两个角、命中块比文字大一圈。命中区仍不小于 24（§11），缩的是内边距。 */
        .sqarvo-pane-switch > button {{
            min-height: {target}px;
            min-width: {target}px;
            padding: 0 4px;
            border: none;
            border-radius: 6px;
            background: transparent;
            box-shadow: none;
        }}
        .sqarvo-pane-switch > button:hover {{ background: alpha(currentColor, 0.07); }}
        .sqarvo-pane-switch > button:checked {{
            background: alpha(currentColor, 0.10);
            box-shadow: none;
        }}

        /* 筛选与排序面板（popover）：密度跟对象树一致——12px 正文、24px 行高、
           11px 说明字。面板里不用 13px 标题，否则它在表格旁边比树「大一号」。 */
        .sqarvo-fs .fs-title {{ font-weight: 600; }}
        /* 面板里**只有一种字号** = 12px（与对象树一致）：正文、按钮、下拉、输入框、
           说明字统一，层级只靠字重与颜色。11/12/13/14 混用正是「看起来不统一」的来源。 */
        .sqarvo-fs label,
        .sqarvo-fs button,
        .sqarvo-fs entry,
        .sqarvo-fs dropdown {{ font-size: {ui}px; }}
        .sqarvo-fs button {{ min-height: 26px; padding: 2px 10px; }}
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
           圆角的「实体条」，点它展开编辑器时形态连续，不会一行文字突然炸成控件。 */
        .sqarvo-fs .fs-row {{
            min-height: 24px;
            padding: 0 4px;
            border-radius: 6px;
            background: alpha(currentColor, 0.06);
            box-shadow: 0 1px 2px alpha(black, 0.06);
        }}
        .sqarvo-fs .fs-row:hover {{ background: alpha(currentColor, 0.10); }}
        /* 行内控件 / 按钮都压在 24px：编辑态与只读态行高相同，点一下就换内容，
           整行不跳高。 */
        .sqarvo-fs .fs-row entry,
        .sqarvo-fs .fs-row dropdown {{
            min-height: 24px;
            padding-top: 0;
            padding-bottom: 0;
        }}
        .sqarvo-fs .fs-row button {{
            min-height: 22px;
            min-width: 22px;
            padding: 0 2px;
        }}
        .sqarvo-fs .fs-action {{ min-height: 28px; padding: 2px 14px; }}

        /* 当前种类高亮：工具条上的种类入口（表 / 视图 / 函数 / 查询）不再禁用，
           高亮只表示「现在列的是这一类」（v0.2.3.3 起它们会打开 Objects 页）。 */
        .sqarvo-kind-active {{ background: alpha(@accent_color, 0.20); }}

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
            padding: 2px 2px;
        }}
        "#,
        window_class = WINDOW_CLASS,
        ui = layout::FONT_UI,
        title = layout::FONT_TITLE,
        status = layout::STATUS_HEIGHT,
        status_font = layout::FONT_STATUS,
        data_font = layout::FONT_DATA_CELL,
        panel_font = layout::FONT_PANEL,
        toolbar = layout::TOOLBAR_HEIGHT,
        tree_row = layout::ROW_HEIGHT_TREE,
        data_row = layout::ROW_HEIGHT_DATA,
        target = layout::MIN_TARGET,
        tab_strip = layout::TAB_STRIP_HEIGHT,
        icon_button_class = crate::icons::BUTTON_CLASS,
        icon_button_width = layout::ICON_BUTTON_WIDTH,
        icon_button_height = layout::ICON_BUTTON_HEIGHT,
        status_dot = layout::STATUS_DOT_DIAMETER,
        status_dot_button = STATUS_DOT_BUTTON_CLASS,
        status_dot_dirty = STATUS_DOT_DIRTY_CLASS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
                ".sqarvo-cell {{ font-size: {}px; font-weight: 500; }}",
                layout::FONT_DATA_CELL
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
            ".sqarvo-panel-text {{ font-size: {}px; font-weight: 400; }}",
            layout::FONT_PANEL
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
        assert!(
            checked_rule.contains("background: alpha(currentColor, 0.10);"),
            "选中态应是淡底：{checked_rule}"
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
        // 最后一列没有右边线。规则要覆盖数据行、表头与类型条带三处，否则三者右侧不齐。
        for selector in [
            "listview > row > cell:last-child",
            "header > button:last-child",
            ".sqarvo-type-row > box.sqarvo-type-cell:last-child",
        ] {
            assert!(css.contains(selector), "缺少表格右边界规则：{selector}");
        }
        assert!(
            css.contains("border-right: 1px solid alpha(currentColor, 0.10)"),
            "右边界线宽 / 颜色要与列分隔线一致"
        );
    }

    #[test]
    fn grid_draws_its_bottom_edge() {
        let css = stylesheet();
        // 用户反馈「数据少时表格底侧边框遗漏」：Adwaita 的行分隔线是 `border-top`，
        // 最后一行没有下边线。规则必须挂在**行**上（`columnview` / `listview` 自己的
        // `border-bottom` 不会渲染），而且只在内容不足一屏时命中 `:last-child`。
        assert!(
            css.contains(
                "listview > row:last-child {\n            border-bottom: 1px solid alpha(currentColor, 0.10);"
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
            rule.contains("border: 2px solid alpha(currentColor, 0.30)"),
            "外框要比分隔线实一档、粗一档（2px / 30%）：{rule}"
        );
        assert!(
            rule.contains("border-radius: 8px"),
            "外框用 8px 圆角（子节点由 `overflow: hidden` 裁切，不再是直角）：{rule}"
        );
        // 行级规则只允许分隔线方向（border-bottom / border-right），不能出现整圈边框。
        for selector in [
            "listview.sqarvo-objects > row {",
            "columnview.sqarvo-grid > listview > row {",
        ] {
            let start = css
                .find(selector)
                .unwrap_or_else(|| panic!("缺少规则：{selector}"));
            let end = start + css[start..].find('}').expect("规则应当闭合");
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
        // 两态只有两个颜色 token：未激活 = 前景色 35% 的灰；有脏行 = suggested-action
        // 用的 `@accent_bg_color`（Yaru 下橙红）。不许出现第三态颜色。
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
        assert!(
            css.contains(&format!(
                ".sqarvo-status-dot.{} {{\n            background-color: @accent_bg_color;",
                STATUS_DOT_DIRTY_CLASS
            )),
            "有脏行必须是 accent 底色（与 suggested-action 同一 token）"
        );
    }

    #[test]
    fn objects_list_uses_the_grid_row_metrics() {
        // 用户要求：`Objects` 块做成单列表格，鼠标移入移出 / 点击选中的效果与数据网格一致。
        // 行高、分隔线、hover、选中四样必须与网格同一口径；改网格时这一条会一起失败。
        let css = stylesheet();
        let rule = |selector: &str| -> String {
            let start = css
                .find(selector)
                .unwrap_or_else(|| panic!("缺少规则：{selector}"));
            let end = start + css[start..].find('}').expect("规则应当闭合");
            css[start..end].to_owned()
        };
        let row = rule("listview.sqarvo-objects > row {");
        assert!(
            row.contains(&format!("min-height: {}px;", layout::ROW_HEIGHT_DATA)),
            "行高必须跟数据行同档：{row}"
        );
        assert!(
            row.contains("border-bottom: 1px solid alpha(currentColor, 0.10)"),
            "行分隔线要跟网格同色同宽：{row}"
        );
        assert!(
            rule("listview.sqarvo-objects > row:hover").contains("alpha(currentColor, 0.04)"),
            "hover 底色要与网格一致（4%）"
        );
        assert!(
            rule("listview.sqarvo-objects > row:selected").contains("alpha(@accent_color, 0.25)"),
            "选中底色要与网格一致（accent 25%）"
        );
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
            css[start..end].contains("border-bottom: 1px solid alpha(currentColor, 0.10)"),
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
}
