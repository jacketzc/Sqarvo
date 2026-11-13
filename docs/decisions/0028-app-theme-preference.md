# ADR 0028：应用内主题偏好（视图 → 深色模式）与「视图」三级菜单

- 状态：已接受（2026-11-13，维护者确认「改动确认单 #1」）
- 日期：2026-11-13
- 关联：Linux 前端 `theme.rs` / `window_state.rs` / `ui.rs`；AGENTS.md §8.6、§14.2；
  组件 `shell.theme`、`shell.density`、`shell.menu`
- 上游：[scaling-and-window-sizing.md](../ui-specification/scaling-and-window-sizing.md) §7、
  [ui-behavior-contract.md](../ui-specification/ui-behavior-contract.md) §8（全局规则）

## 背景

深色模式在此之前只有「跟随系统」一条路：`theme.rs` 读 `AdwStyleManager::is_dark()`，系统切换时
经 `dark_notify` 重载样式表，两套品牌色板（`LIGHT_PALETTE` / `DARK_PALETTE`）都已就位。缺的是
**应用内覆盖**：用户无法在系统浅色下单独让 Sqarvo 用深色，也无法在系统深色下临时切回浅色。

同时「视图」子菜单只为「界面密度」而存在，而且这一项是 GMenu 的占位项 + `PopoverMenu::add_child`
的自绘小节（三个 `GtkCheckButton`），理由是「GMenu 没有逐项 tooltip、也不能逐项禁用」。这个理由只
对了一半：GMenu 确实没有逐项 tooltip，但**逐项禁用是可以的**——每个档位各有自己的动作，`enabled=false`
就让对应条目变灰。自绘小节因此多背了一层「菜单模型 + 真实控件两套状态要手动同步」的复杂度。

需求：深色模式进「视图」，并与原有的「界面密度」合并成同一级结构（两个三级菜单）。

## 备选方案

1. **系统设置里改（不改代码）**：满足不了「只让 Sqarvo 深色」这个最常见的诉求。
2. **二态开关（深色开 / 关）**：state 只有真假两值，无法表达「跟随系统」，用户失去默认行为；也
   与 Windows 已有的 `theme = system|light|dark` 字段对不上。
3. **三档单选（跟随系统 / 浅色 / 深色）落 `AdwStyleManager::set_color_scheme`**（本 ADR 采用）。
4. **自建主题引擎（自己读 gsettings / 自己算色板）**：重复 libadwaita 的活，且系统强调色、高对比
   都要自己接；与本项目「用平台原生能力」的原则冲突。
5. **保留自绘密度小节，深色模式另起一个自绘小节**：两个小节都要手写状态同步，且与菜单里其它项
   的键盘 / 悬停行为不一致。

## 决策

1. **三档，默认跟随系统**：`system` / `light` / `dark`。`system` 映射 `AdwStyleManager` 的
   `Default`，`light` / `dark` 映射 `ForceLight` / `ForceDark`——用 `Force*` 而不是 `Prefer*`，
   因为用户在菜单里点的是明确选择，不该再被系统偏好改回去。
2. **生效路径只走 libadwaita**：`set_color_scheme` → `dark` 通知 → 已有的 `apply_style_manager`
   重载样式表。四套 CSS（主壳 / 数据页 / 对象树 / DDL）不各自监听主题：它们引用的
   `@theme_*` / `@warning_color` 等 token 由 GTK / libadwaita 在主题切换时重算。
3. **持久化**：`window.json` 的 `theme` 字段（`system` / `light` / `dark`），与 Windows 同键同值；
   可缺，缺 = 跟随系统；加法字段不升 `version`。启动优先级 `--theme`（QA）→ `window.json` →
   跟随系统，并且**在 `theme::install()` 之前**设定，避免「先浅色再闪成深色」。
4. **菜单结构**：`视图 ▸ 深色模式 ▸ {跟随系统, 浅色, 深色}`、`视图 ▸ 界面密度 ▸ {紧凑, 舒适, 大}`。
   两项都是真子菜单（条目绑 stateful `win.*` 动作，当前值打勾，不可用项由动作 `enabled` 置灰），
   删掉「界面密度」的自绘小节与它的占位项。
5. **GMenu 没有逐项 tooltip 的补偿**：密度档放不下时把原因写进条目文字（「大（150%），放不下」），
   完整原因（需要 W×H、当前工作区多大）进日志。GMenu 在 `append` 时复制条目属性，所以换文字靠
   重填这一节（`fill_density_section`），不是改 `gio::MenuItem`。
6. **范围只做 Linux**：Windows 的 `Theming/ThemeService.cs` 已有同一套三档与 `theme` 字段，但没有
   任何界面入口、`Preference` 也没有 setter；在注册表里记 `sync.windows = none` 并写清差异，
   菜单入口另开 Issue。macOS 未接入。

## 理由

- 三档是「默认行为 + 明确覆盖」的最小完整表达：默认不抢系统的决定，用户要覆盖时一步到位。
- 复用 libadwaita 的 `color-scheme` 意味着深色 / 浅色 / 高对比 / 系统强调色只有一条真值链，
  不会出现「我们自己算的深色」和「系统深色」两套观感。
- 密度档改真子菜单不是审美选择：它消掉了「占位项 + 自绘控件 + guard 标志」这套同步机制，同时
  保留了逐项禁用；唯一损失（tooltip）用条目文字补上，且这件事在菜单里本来就该是「读一眼就知道」。

## 代价与边界

- **tooltip 丢失**：密度档放不下时只能说「放不下」，说不出「需要 1440×960」。完整原因在日志与
  本文档里；如果以后需要「菜单里看到数字」，得换 L3 自绘菜单（需要 ADR）。
- **重填子菜单模型**：每次菜单弹出与切档后会 `remove_all` + 重新 `append` 三个条目。条目只有三
  个，代价可忽略；但如果以后这一节变多，要考虑换成按需建模型。
- **`--theme` 是 QA 开关**：与 `--tier` 同形，只用于取证（`--qa-open-menu` 截图与 `--theme dark`
  渲染），产品入口只有菜单。
- **Windows 仍是「只能跟随系统」**：同一份 `window.json` 会被 Windows 原样保留 `theme` 字段，但
  它读到的 `dark` / `light` 只能在启动期生效，运行中没有 UI 可改。

## 复评条件

- 需要在菜单里显示「需要多大工作区」这类逐项说明 → 重新评估 tooltip 与菜单控件（可能触发 L3 ADR）。
- Windows 接入选单后，三端行为必须一致：条目、顺序、勾选、写入的 `theme` 值都对齐本 ADR。
- 若产品要求「跟随系统但覆盖强调色」这类组合，`AdwStyleManager` 之外的能力需要新 ADR。