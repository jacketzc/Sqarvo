# 界面缩放、窗口尺寸与显示器适配

本文件定义三端（Linux GTK / Windows WinUI 3 / macOS SwiftUI）**界面密度档、初始窗口尺寸、
显示器识别与窗口几何持久化**的规范口径。数字不在本文件里，也不在代码里各写一份：唯一真值是
[`metrics.toml`](metrics.toml)。本文件说明这些数字怎么用、什么时候用、用错了会坏在哪。

配套决策：[ADR 0026](../decisions/0026-ui-scaling-window-sizing.md)。

## 分工

| 文件 | 管什么 |
|---|---|
| [`metrics.toml`](metrics.toml) | **数字**：三档密度下每一个尺寸的取值 |
| **本文件** | **规则**：三档怎么选、窗口选多大、换显示器时怎么重算、怎么持久化 |
| [`workspace-model.md`](workspace-model.md) | 版式：六区域结构、布局算术、参考窗口 1200×800 |
| [`ui-behavior-contract.md`](ui-behavior-contract.md) | 行为：缩放怎么触发、档位切换后发生什么、最小值怎么生效 |
| [`design-principles.md`](design-principles.md) | 取向：为什么不做像素级一致性、图标与密度的关系 |

冲突时：数字以 `metrics.toml` 为准；窗口与缩放规则以本文件为准；版式以 `workspace-model.md`
为准；行为以 `ui-behavior-contract.md` 为准。三份文档都不得与代码事实矛盾。

## 1. 先分清三个轴

「缩放」这个词在本项目里指三件互不相干的事，混在一起做必然出错。

| 轴 | 谁提供 | 应用侧怎么处理 | 会不会进代码 |
|---|---|---|---|
| **A 系统 DPI / 缩放因子** | 操作系统：GNOME 的缩放设置、Windows 的「缩放与布局」 | **不处理**。GTK 的逻辑像素与 XAML 的 DIP 已经是「与屏幕密度无关」的单位，框架会把它们换成物理像素 | **永远不乘进密度档**，否则双重缩放 |
| **B 界面密度档** | 本产品：`compact` / `comfortable` / `large` | 换一套 [`metrics.toml`](metrics.toml) 的数字 + 重载样式 | 是 |
| **C 字号（无障碍）** | 操作系统：`text-scaling-factor` / Windows「文本大小」 | 跟随系统，**不另设应用内字号设置**（见 §3.3） | 否 |

两个可直接照抄的事实：

- GTK 4 在 `GDK_SCALE=2` 下，应用侧读到的仍是 **1200×800 逻辑像素**（实测：布局日志与 1× 完全
  一致，`--qa-layout-audit` 也报 `窗口 1200x800`）。也就是说 A 轴对布局算术透明。
- WinUI 3 的 XAML 布局单位是 DIP（1/96 英寸），同样是密度无关的；只有 `AppWindow` /
  `DisplayArea` 那一层用物理像素。

## 2. 密度档

三档，取值见 `metrics.toml`：

| 档位 id | 标签 | scale | 适用 |
|---|---|---:|---|
| `compact` | 紧凑（100%） | 1.0 | 默认值。1080p 及以下、以及习惯紧凑密度的用户 |
| `comfortable` | 舒适（125%） | 1.25 | 1080p / 2K |
| `large` | 大（150%） | 1.5 | 2K / 4K；1080p 上只在「操作系统缩放 100%」时可用 |

### 2.1 不提供小于 100% 的档位

不是没做，是**在现有基准值上做不出来**，理由记录在此以免以后重复讨论：

- 100% 档已经贴在下限上：`row_height_tree = min_target = icon_button_height = 24`，
  `font_data_cell = 13`（12px 渲染 CJK 会裁笔画，`layout.rs` 有实测注释）。
- 80% 会得到 19px 命中区与 10.4px 单元格字：同时违反 `workspace-model.md` §11 的最小点击区、
  `design-principles.md` 的图标描边下限、以及 `layout.rs` 的编译期断言与单测。
- 收益被高估：窗口固定 1200×800 时，80% 只是 17 行 → 23 行、表格宽 +17%。
- 真要「更密」的正确做法是先把基准字号抬上去再往下缩，那是一次视觉重构而不是一个缩放旋钮。

「屏幕太小装不下 1200×800」这个真实诉求由 §5 的分档规则解决（窗口跟着工作区走），不靠缩到 80%。

### 2.2 档位可用性守卫（必须实现）

选定档位 T 后，先判断它在当前显示器上是否可用：

    可用(T) ⟺ 工作区逻辑宽 ≥ min_window_width(T) 且 工作区逻辑高 ≥ min_window_height(T)

- 不可用时不写入偏好，菜单里该项禁用，tooltip 写明「当前显示器放不下 150%：需要 1440×960 逻辑工作区」。
- 已保存的偏好不可用时，回退到下一个可用档位并记一行日志；**不静默钳制到 100%**（用户会以为设置丢了）。
- 最小窗口本身也要按工作区收敛（§5.3），但「可用性守卫」用的是**未收敛**的
  `min_window_width/height(T)`——否则任何屏幕都能声称自己支持 150%。

### 2.3 切换档位时窗口跟着放大（关键，别漏）

侧栏与信息面板也按档位放大，实测后果很硬：

    窗口固定 1200×800、只放大界面
     档位    表格高  数据行  行高  单元格字  表格宽  约列数
     100%     574     17    32      13      652    3.26
     125%     518     12    40      16      515    2.06
     150%     461      8    48      20      378    1.26

150% 时侧栏 356 + 面板 420 占掉 776px，1200 宽的窗口只剩 378px 给表格。所以：

- 切换档位时窗口尺寸**按 §5 的规则重算并一起改**，不是只重排内容；
- 若重算后的窗口超出工作区，窗口收到工作区，同时把侧栏 / 面板收到各自下限（`sidebar_min` /
  `inspector_min`），并把这一状态写进日志；
- 用户手动拖过的面板宽度要按档位**归一化保存**（§7），否则 1.5× 记成 630、切回 1.0× 又会被
  `inspector_max = 520` 钳成 520，看起来像设置被吃掉。

### 2.4 触发入口与快捷键

| 动作 | 键 | 行为 |
|---|---|---|
| 密度档 + | `Ctrl+Plus` / `Ctrl+KeyPadAdd` | 提升一档（到顶禁用） |
| 密度档 − | `Ctrl+minus` / `Ctrl+KeyPadSubtract` | 降低一档（到底禁用） |
| 密度档重置 | `Ctrl+0` | 回到 `compact` |
| 菜单 | 视图 → 界面缩放（三个单选项 + 当前项打勾） | 与快捷键同一 action |

不用 `Ctrl+滚轮`：滚轮在数据网格与 SQL 编辑器里已有语义（滚动、编辑器内字号是另一件事）。

## 3. 度量真值的使用方式

### 3.1 各端只读、不计算

`metrics.toml` 里三档的每一个值都是**写死的整数**。各端不要在运行时用「基准 × scale」算：

    Rust `12.5f64.round()` → 13     (half-away-from-zero)
    C#   `Math.Round(12.5)`  → 12    (half-to-even，默认)
    Swift `12.5.rounded()`   → 13

三端会静默漂移 1px，而这种偏差只在特定档位出现、肉眼几乎发现不了。接入方式：

| 端 | 方式 |
|---|---|
| Linux（Rust） | `include_str!("../../docs/ui-specification/metrics.toml")` + `toml` 解析，启动时解析一次；单测断言不变量（§3.3）与「与 baseline 完全一致」 |
| Windows（C#） | 构建期源生成器读同一份 TOML 生成 `Metrics.g.cs`（三档常量 + `ClearValues()`），**不在运行时解析**，失败要让构建红 |
| macOS（Swift） | 构建脚本生成 `Metrics.swift`（同上）；未接入前不写死第二套数字 |

### 3.2 不随档位缩放的量

- **1px 描边**：`border: 1px` 在所有档位保持 1px。1.5× 的 1.5px 细线会被舍入成 1px 或 2px，
  要么消失要么变粗，两头都难看得明显。
- **图标描边**：符号图标来自 16px 网格（描边约 1.6px），图标**盒**按档位缩放（16 / 20 / 24），
  但描边不单独调。
- **图标光学补偿**（`icons.rs` 的 `OPTICAL_PX`）：必须**按比例**缩放而不是加固定像素。18px 的
  「取消 ■」在 1.5× 下应是 27 而不是 34，否则比基准盒（24）还大一圈，`icons.rs` 的
  `-6..=+2` 断言会直接失败。

### 3.3 不变量（每档都必须成立，写成测试）

| 不变量 | 200% 档 |
|---|---|
| `row_height_tree = min_target = icon_button_height` | 命中区不低于最小点击区 |
| `font_title > font_ui` | 标题层级还在 |
| `font_data_cell ≥ 13` 且 `font_data_cell ≥ font_ui` | CJK 笔画不裁、网格比界面醒目 |
| `font_panel ≤ font_ui` | 信息面板不抢注意力 |
| `icon_button_width - icon_box ≥ 4` | 图标两侧各留 2px |
| `icon_button_height ≤ row_height_tree` | 24 高的行里装得下按钮 |
| `sidebar_min < sidebar_width < sidebar_max` | 默认值在可拖区间内 |
| `inspector_min < inspector_width < inspector_max` | 同上 |
| `row_height_data > row_height_tree` | 数据行比树行松 |

三档之外不许新增档位；要新增必须先满足上表并回填 `metrics.toml`。

## 4. 字号（C 轴）的处理

- 界面上跟随系统字号缩放，不自建「应用内字号」设置——两个来源会让「到底是谁在放大」无法解释。
- 系统字号放大后，行高必须跟着走，否则 24px 行里塞 16px 字会把 CJK 底横裁掉（项目踩过：
  `layout.rs` 的 `FONT_DATA_CELL` 注释）。具体做法：行高 = `max(档位行高, 系统字号 × 1.9)` 向上取整到 4 的倍数。
- 数据网格另有一条独立的公制要求：数字列统一 `font-variant-numeric: tabular-nums`，等宽字体取自
  系统偏好（Linux 用 `adw::StyleManager::monospace_font_name()`），不写死字体族。

## 5. 初始窗口尺寸

### 5.1 轴是「逻辑工作区」，不是分辨率

4K@200% 的逻辑工作区 = 1920×1048，和 1080p@100% 一样；1080p@150% = 1280×698。所以拿
「1080p / 2K / 4K」当轴会得出自相矛盾的结论。代码里只认两个数：**逻辑工作区宽高**。

逻辑工作区 = 显示器几何（GTK `GdkMonitor::geometry()` / WinUI `DisplayArea.WorkArea ÷ 栅格化比例`）
减去顶栏 / 任务栏。三端各自按平台估算（Linux GNOME 顶栏 32、Windows 任务栏 48、macOS 菜单栏 25
+ Dock 按是否自动隐藏），估算值写在平台适配层，不在本文件。

### 5.2 基准推荐尺寸（100% 档）

按逻辑工作区宽 W 分四档取 `(宽度, 高度)`：

    W < 1200        → (W - 24, H - 24)                     # 塞满
    1200 ≤ W < 1700 → (W - 56, min(0.84·H, 860))
    1700 ≤ W < 2200 → (min(W - 120, 1400), min(0.80·H, 900))
    W ≥ 2200        → (min(W - 160, 1680), min(0.78·H, 1000))

再按下限收敛：`w ≥ min(960, W - 32)`、`h ≥ min(640, H - 48)`。

| 逻辑工作区 | 100% 档推荐 | 表格宽 | 约列数 | 数据行 |
|---|---|---:|---:|---:|
| 1366×736（768p@100%） | 1310×640 | 762 | 3.8 | 12 |
| 1280×698（1080p@150%） | 1224×640 | 676 | 3.4 | 12 |
| 1536×838（1080p@125%） | 1480×704 | 932 | 4.7 | 14 |
| 1920×1048（1080p@100%） | 1400×838 | 852 | 4.3 | 18 |
| 2048×1120（2K@125%） | 1400×896 | 852 | 4.3 | 20 |
| 2560×1408（2K@100% / 4K@150%） | 1680×1000 | 1132 | 5.7 | 23 |

设计约束（这三条就是验收标准）：表格区宽 ≥ 600（约 3 个 200px 列）、可见数据行 ≥ 12、
初始不最大化。

### 5.3 其它档位 = 基准 × scale，再钳到工作区

    recommended(T) = clamp(base_recommendation(W, H) × T, 下限, (W-32, H-48))

| 逻辑工作区 | 100% | 125% | 150% |
|---|---|---|---|
| 1366×736 | 1310×640 · 12 行 | — 不可用 | — 不可用 |
| 1536×838 | 1480×704 · 14 行 | — 不可用 | — 不可用 |
| 1920×1048 | 1400×838 · 18 行 | 1750×1000 · 16 行 | 1888×1000 · 12 行 |
| 2048×1120 | 1400×896 · 20 行 | 1750×1072 · 18 行 | 2016×1072 · 14 行 |
| 2560×1408 | 1680×1000 · 23 行 | 2100×1250 · 23 行 | 2520×1360 · 20 行 |

「不可用」= §2.2 的可用性守卫不通过（例如 1080p@150% 的逻辑高只有 698，放不下 125% 档的
800 高最小窗口）。

### 5.4 最小窗口按工作区收敛（P0，现在就是坏的）

`min_window = (min(960·T, W-32), min(640·T, H-48))`。

不收敛的话，`960×640` 在 1920×1080@200%（逻辑 960×540）上比屏幕还高，底部状态栏永远看不到；
1366×768@125%（逻辑 1092×614）同理。这条与档位无关，是**独立缺陷**，先修。

### 5.5 参考窗口不变

`workspace-model.md` §1 的「参考窗口 1200×800」继续作为原型、截图基线与布局算术的基准，
**不改**。改的只是「默认窗口」——它现在由 §5.2 的规则决定。

## 6. 显示器识别与重算

### 6.1 三个平台各自的取值方式

| 需要的东西 | Linux（GTK4） | Windows（WinUI 3） | macOS（AppKit/SwiftUI） |
|---|---|---|---|
| 窗口所在显示器 | `display.monitor_at_surface(&surface)` | `DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Nearest)` | `window.screen` / `NSWindow.screen` |
| 工作区 | **`GdkMonitor` 没有 `workarea()`**（gdk4 0.11.5 全量方法已核）：用 `geometry()` 减平台顶栏估算 | `DisplayArea.WorkArea`（**物理像素**，先除栅格化比例） | `NSScreen.visibleFrame`（已是逻辑点） |
| 栅格化比例 | `GdkMonitor::scale_factor()`（整数）/ `scale()`（f64，GNOME 分数缩放） | `XamlRoot.RasterizationScale` | `NSScreen.backingScaleFactor` |
| DPI / 缩放变化 | surface 的 `notify::scale-factor` | `XamlRoot.Changed`（`DisplayInformation.GetForCurrentView()` 在 WinUI 3 抛异常） | `NSApplication.didChangeScreenParametersNotification` |
| 分辨率 / 热插拔 | `GdkMonitor::connect_invalidate` + `connect_geometry_notify` | 同上（`XamlRoot.Changed`）+ `AppWindow.Changed` | 同上 |

### 6.2 一个入口：`relayout(work_area, scale, tier)`

换显示器、改分辨率、系统缩放变化、切档位，**全部**走同一个函数：重算窗口尺寸、重载样式、
重设面板钳制区间。给它们各写一套逻辑，早晚会出现「拖到另一块屏后窗口没变、CSS 变了」。

### 6.3 顺序

启动时先在默认尺寸下 `present()`，拿到 surface / XamlRoot 之后才做第一次 `relayout`——
surface 未就绪时读不到显示器。第一次 `relayout` 会改窗口尺寸，允许有一帧的跳动；不要为了
消掉这一帧去猜显示器（猜错后跳得更狠）。

## 7. 持久化

窗口级偏好写 `window.json`（Linux `$XDG_CONFIG_HOME/sqarvo/Sqarvo/window.json`，Windows
`%LOCALAPPDATA%\Sqarvo\window.json`，macOS 对应 Application Support）。**未打包的 WinUI 应用
拿不到 `ApplicationData`**（需要 package identity），所以三端都用同一份手写 JSON，schema 一致。

```json
{
  "version": 2,
  "tier": "compact",
  "last_inspector_width": 280,
  "last_sidebar_width": 237,
  "window_width": 1400,
  "window_height": 838,
  "window_x": 260,
  "window_y": 120,
  "maximized": false
}
```

规则：

1. 宽度类字段一律存 **`compact` 档的归一化值**（实际值 ÷ 档位 scale，四舍五入）。读的时候乘以当前档位。
   不归一化就会在切档时来回钳制（§2.3 最后一条）。
2. `version` 从 1 升到 2：v1 只有 `version` / `last_inspector_width` / 旧版 `inspector_width`。
   读 v1 时按原逻辑迁移，写回时升到 v2。
3. 恢复几何前**必须校验**：保存的矩形要落在某块当前显示器的几何内（允许 ±32 的容差），
   否则丢弃几何、回退到 §5.2 的推荐尺寸（显示器被拔掉是最常见的场景）。
4. 写入用「临时文件 + rename」，进程崩溃不留半个 JSON；读失败一律回退默认值，不报错弹窗。
5. 文件里不含任何凭据（这条与连接配置分离的原则见 `window_state.rs` 模块注释）。

## 8. 平台映射要点（Windows 原生支持）

这一节写给 Windows 端的实现者：这些能力在 WinUI 3 上都是平台自带的，但单位与主题时机有坑。

1. **两套像素单位**：XAML 用 DIP，`AppWindow.Resize` / `MoveAndResize` / `DisplayArea.WorkArea`
   用**物理像素**。所有「选多大」的计算都在 DIP 里做（与 GTK 的逻辑像素同一套数字，§5.2 的表
   可以直接照抄），只在喂给 `AppWindow` 之前乘一次 `XamlRoot.RasterizationScale`。
   不乘的症状是「150% 屏上窗口小一号」，在 100% 缩放的机器上永远看不到。
2. **DPI 变化只能从 `XamlRoot.Changed` 拿**，`DisplayInformation.GetForCurrentView()` 会抛异常。
3. **最小尺寸**用 `OverlappedPresenter.PreferredMinimumWidth/Height`（物理像素、含边框）；
   新版 WinUI 另有 `Window.MinWidth/MinHeight`（DIP、仅客户区）。**两者不要同时设**，
   WinUI 会互相覆盖，症状是「设了最小值还能拖更小」。
4. **主题**：`Application.RequestedTheme` 只能在启动时设（运行中设抛 `NotSupportedException`）；
   运行中切换用元素级 `((FrameworkElement)Content).RequestedTheme = ElementTheme.Default / Dark / Light`。
5. **标题栏要单独处理**，否则暗色窗口顶上是白的（Linux/macOS 没有这条，最容易漏）：
   `AppWindow.TitleBar.PreferredTheme = TitleBarTheme.UseDefaultAppMode`；该 API 不存在时回退
   `DwmSetWindowAttribute(hwnd, 20, &dark, sizeof(int))`（Win10 2004 之前是 20 → 19）。
6. **密度档切换走「重建页面」**，不要指望改资源字典后界面立刻变：`StaticResource` 只解析一次，
   `ThemeResource` 只在主题切换时重算。重建前把「当前标签 / 未提交编辑 / 滚动位置」序列化到模型，
   重建后回放——这个能力多窗口与会话恢复本来也需要。
7. **Windows 用系统标准密度，不复刻 Linux 的紧凑档**（ADR 0023 已授权：三端区域一致、控件原生，
   密度不必一致）。`compact` 档在 Windows 上映射为「系统标准密度」，`comfortable` / `large`
   映射为系统密度的 1.25 / 1.5 倍（仅缩放在本文件管辖范围内的自有元素）。
8. **颜色一律走 `ThemeResource`**，XAML 里不写 `#RRGGBB`；高对比用
   `AccessibilitySettings.HighContrast`，系统强调色用 `UISettings.GetColorValue(UIColorType.Accent)`。

## 9. 验收

| # | 检查 | 方式 |
|---|---|---|
| 1 | 三档 `metrics.toml` 解析成功且不变量全成立 | Linux 单测（Rust 解析）+ Windows 生成器自检 |
| 2 | 每档窗口不超出工作区 | `--qa-report-monitor` 打印工作区、档位、最终窗口；断言 `w ≤ W-32 且 h ≤ H-48` |
| 3 | 工作区小于默认下限时窗口仍完整可见 | 用 `--window-size` 与 `--tier` 组合跑 960×640 / 1280×698 / 1920×1048 三档 |
| 4 | 档位切换后表格仍 ≥ 3 列 | `--qa-layout-audit` 在 1.0 / 1.25 / 1.5 各跑一次，记录「需求 ≤ 分配」与列数 |
| 5 | 最小窗口 = `min(960·T, W-32)` | X11 实测窗口尺寸；Wayland 只验证 `set_size_request` 已下发 |
| 6 | 面板宽度跨档来回切换不漂移 | 在 1.5× 设 630 → 切回 1.0× → 再切 1.5×，宽度仍是 630 |
| 7 | 几何恢复的越界回退 | 手改 `window.json` 的 `window_x = 99999`，启动后窗口落在主显示器内 |
| 8 | 暗色 / 高对比下三档都无对比度回归 | 三档 × {浅色, 暗色, 高对比} 截图 |

`--qa-layout-audit` 的断言比的是「控件需求 ≤ 分配」，两者都随档位放大，因此**断言本身在每档
都成立**；但契约里贴的具体数字（如「需求 409 ≤ 分配 417」「17 项 0 超限」）是 100% 档的实测值，
换档要重取并写明档位。

## 10. 已知边界

| 边界 | 现状 | 处置 |
|---|---|---|
| Wayland 下的最小尺寸 | 依赖合成器遵守 `xdg_toplevel.set_min_size`，未逐一手测 | 与 `ui-behavior-contract.md` §9 同一条，保留 |
| GNOME 分数缩放 | `GdkMonitor::scale()` 是 f64（1.25 等）；`geometry()` 给整数逻辑像素 | 允许 ±1px 误差，验收按「不超工作区」判定，不按精确像素 |
| 系统字号放大 | 本机 `text-scaling-factor = 1.0`，未实测 1.5 下的行高自适应 | §4 的公式先落地，实测后回填 |
| macOS | 只有骨架，未验证 | 本文件的 macOS 列属未验证口径，接入时复核 |
| 高对比（Linux） | `adw::StyleManager::is_high_contrast()` 存在但代码未用（0 命中） | 见 issue「暗色与高对比修补」 |

## 变更记录

| 日期 | 变更 |
|---|---|
| 2026-11-10 | 首次成文：三轴拆分、三档密度、按逻辑工作区分档的窗口尺寸、最小窗口收敛、`metrics.toml` 成为数字唯一真值 |