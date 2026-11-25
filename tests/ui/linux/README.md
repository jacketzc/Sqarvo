# UI 功能测试 — Linux（GTK4 / AT-SPI）

三端共同规范见 `../README.md`（伞层）；本文件只写 Linux 端实现与环境限制。

用可重复的自动化功能测试替代 agent 反复交互式点界面。只验证**交互与功能**
（控件响应、状态流转、可访问性名称、真实数据），不做像素/样式/截图断言
（ADR 0029、前端 AGENTS §13）。

## 依赖与准备

- 运行环境：GNOME Wayland 桌面会话（AT-SPI 总线可用，`toolkit-accessibility=true`）。
- 被测程序：`frontend/linux/target/release/sqarvo-desktop`（缺省自动发现；
  可用 `SQARVO_UI_BIN=<path>` 覆盖，如指向 `~/.local/bin/sqarvo-desktop`）。
- Python：系统 `/usr/bin/python3` 的 `gi` + `Atspi 2.0`；venv 只装 pytest：
  `just ui-test-env`（需 `uv`）。

## 命令

```
just ui-test-smoke   # 启动 / 主窗口可访问性 / 对话框 / 关窗断开
just ui-test-sql     # SQLite 连接、SQL 执行、结果校验、对象树导航
just ui-test-all     # 全部
```

单条场景：`tests/ui/linux/.venv/bin/python -m pytest tests/ui/linux/test_03_sql.py::test_execute_sql -c tests/ui/linux/pytest.ini -q`

`test_14_inspector_no_cache.py` 与 `test_15_refresh_regressions.py` 可一起执行。后者通过
SQL 编辑器执行并验证 CREATE、RENAME、DROP，使用独立 SQLite 连接只读确认提交，
再检查重入 Objects 的列表与请求次数。SQLite 不支持显式切换请求库，测试先从连接行
建立 SQL 标签，再选回 Tables 作用域。确定性的分页错误、列响应与同名库统计测试
在 GTK 会话里用 `cargo test --manifest-path frontend/linux/Cargo.toml -- --ignored --test-threads=1` 执行。

`test_06_mcp_access.py` 的外部刷新用例需要 `SQARVO_MCP_BIN`，或与
`SQARVO_BACKEND_BIN` 位于同一目录的 `sqarvo-mcp`。

## 输出契约

成功（stdout 只有摘要，无树、无日志、无截图）：

```
UI TEST: PASS
Passed: 10
Failed: 0
Duration: 41.2s
```

失败（只展开失败项；完整可访问性子树落盘 `tests/ui/linux/logs/`）：

```
UI TEST: FAIL
Test: test_execute_sql
Step: verify_result
Expected: 3 rows
Actual: 0 rows
Error: Query result mismatch
Diagnostic: tests/ui/linux/logs/test_03_sql.py_test_execute_sql.txt
```

机器可读报告：`tests/ui/linux/results/report.json`（每条用例 outcome/duration/step/expected/actual）。
退出码：0 全过；1 有失败；3 环境不可行（AT-SPI 不可用 / 找不到二进制 / 残留实例）。

## 分层

| 层 | 文件 | 职责 |
|---|---|---|
| 自动化适配 | `adapter.py` | 按 role+name 语义定位、do_action 点击、EditableText 输入、状态读取、显式等待；失败诊断 dump |
| 可复用操作 | `ops.py` | 打开连接对话框、建 SQLite 连接、执行 SQL、读结果网格、树导航、关窗；`step()`/`expect()` 结构化失败 |
| 场景 | `test_*.py` | 组合 ops 断言用户可观察行为；夹具在 `conftest.py` |

选择器命名一律对齐 `docs/ui-specification/components/*.toml` 的 `a11y.name`；不依赖
坐标、颜色、字号、布局位置。

## 隔离与确定性

- 每个实例独立 `XDG_CONFIG_HOME/DATA/STATE`（`tests/ui/linux/.work/<tag>`）+ 独立 SQLite
  测试库（夹具建表 `widgets` 三行合成数据）；后端使用独立临时套接字目录。测试结束删目录、清理进程组。
- 实例经 `SQARVO_QA_APP_ID` 与用户正在运行的窗口共存；`--qa-exit-after-ms` 看门狗
  兜底防止测试崩溃留下常驻进程。
- 等待全部是「条件 + 超时轮询」；无裸 `sleep`。
- 绝不触碰用户 `~/.config/sqarvo`、真实数据库与系统凭据库（SQLite 无密码路径）。

## 已知限制（如实记录，不要绕过）

1. **Wayland 禁止输入合成**：`Atspi.generate_keyboard_event` / `Component.click` /
   `grab_focus` 均返回 `atspi_error`（实测）。因此无法模拟按键与坐标点击。
   受影响场景改走等价语义通道：快捷键 → 窗口 GAction（`win.execute` 等，AT-SPI 直接
   可触发，与快捷键落点是同一个 action）。
2. **`adw::ComboRow` 驱动下拉无法外部切换**：列表虚拟折叠、无 Action、点不开弹窗
   （限制 1 的后果）。SQLite 连接的创建经应用自带的 `--qa-profile` 验收通道
   （前端 AGENTS §13：填相同控件、触发相同回调），其余对话框操作（打开/取消/
   控件在位与可用性）全部是真实 AT-SPI 点击。
3. **主动「断开连接」只有手势右键菜单入口**：外部无法触发（限制 1）。
   `test_05_disconnect.py` 以「关窗 → 会话回收 → 重启未连接」覆盖可观察的断开语义，
   菜单入口用 skip + 原因显式挂账，等应用提供非手势入口后补测。
4. 通过功能测试 ≠ 视觉正确（任务 §9 原文）。

## Agent 使用约定（任务 §9）

改 UI 功能后：优先复用 ops 里已有操作 → 增/改对应场景 → 跑最小相关子集 → 只看摘要；
失败时只读该用例的 `Diagnostic` 文件。已有测试覆盖的行为不要再手工点界面，
不要为测试截图或做视觉分析。

用例与组件的对应：每条用例在 docstring 标注对应注册表组件 `id`；改组件先按 `id`
grep 本目录找现有用例，该端缺失覆盖时**先补用例再验收**（根 AGENTS §13、
frontend/AGENTS §13）。本框架是三端功能用例库模式的首发实现，Windows（UIA）/
macOS（XCUITest）建库时以本目录三层结构为参照。

HTTP E2E 需要 GTK、Codex CLI、GNOME Keyring 和系统 Python 的 Secret typelib。
构建前端及两个后端后，从 GNOME Wayland 会话运行
`bash tests/integration/run_gtk_mcp_http.sh`。脚本使用隔离的会话总线、可访问性注册表、
Secret Service 和 SQLite 数据。使用 `SQARVO_UI_BIN`、`SQARVO_BACKEND_BIN`、
`SQARVO_MCP_BIN` 可指定被测二进制。复制用例验收 GDK 接受内容后的界面反馈，复制文本另由 GTK 单测断言。
