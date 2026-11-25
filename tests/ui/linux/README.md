# UI 功能测试 — Linux（GTK4 / AT-SPI）

三端共同规范见 `../README.md`（伞层）；本文件只写 Linux 端实现与环境限制。

用可重复的自动化功能测试替代 agent 反复交互式点界面。只验证**交互与功能**
（控件响应、状态流转、可访问性名称、真实数据），不做像素/样式/截图断言
（ADR 0029、前端 AGENTS §13）。

## 依赖与准备

- Linux 需有可访问的 systemd 254+ 用户管理器、cgroup v2 和已下放的 CPU、内存与 I/O
  controller。支持使用无宿主图形会话的用户级 runner。
- 图形与会话依赖为 Xvfb、Openbox、xauth、xdpyinfo、xprop、D-Bus、AT-SPI 和 GNOME
  Keyring。Ubuntu/Debian 对应包为 `xvfb openbox xauth x11-utils dbus-daemon libglib2.0-bin
  at-spi2-core gnome-keyring python3-gi gir1.2-atspi-2.0 gir1.2-secret-1`。先确认本机缺哪些包，再安装。
- 在 scope 外构建前端和后端、执行 `just ui-test-env`（需 `uv`）。入口依次查找仓库
  release/debug 产物及已安装前端，可用 `SQARVO_UI_BIN`、`SQARVO_BACKEND_BIN`、
  `SQARVO_MCP_BIN` 明确选择。HTTP E2E 还需 PATH 上的 Codex CLI 和 MCP 二进制。

## 命令

```sh
just ui-test-smoke
just ui-test-sql
just ui-test-all
scripts/run-ui-tests-isolated.sh tests/ui/linux/test_03_sql.py::test_execute_sql -q
scripts/run-ui-tests-isolated.sh -k 'inspector or refresh'
bash tests/integration/run_gtk_mcp_http.sh
```

入口把额外参数交给 pytest，无场景参数时按 `pytest.ini` 发现全量用例。可从仓库根传
`tests/ui/linux/…` 或传绝对路径。每个入口调用独立运行，支持并行，单次运行内按 pytest
默认顺序执行（夹具与 AT-SPI 选择器按单 worker 设计）。

可一起运行 `test_14_inspector_no_cache.py` 与 `test_15_refresh_regressions.py`。
后者通过 SQL 编辑器执行 CREATE、RENAME、DROP，用独立 SQLite 连接只读确认提交，
再检查重入 Objects 的列表与请求次数。SQLite 场景从连接行建立 SQL 标签，再选回
Tables 作用域。

需要显示的 Rust 忽略测试也走同一入口。先在 scope 外构建并读取 Cargo 输出的测试二进制
绝对路径，再在 scope 内执行测试：

```sh
cargo test --manifest-path frontend/linux/Cargo.toml --bin sqarvo-desktop --no-run
scripts/run-ui-tests-isolated.sh --gtk-tests /绝对路径/测试二进制 --ignored --test-threads=1
```

### Scope 配置

| 环境变量 | 默认值 | 作用 |
|---|---:|---|
| `SQARVO_UI_CPU_QUOTA` | `200%` | CPU 时间上限 |
| `SQARVO_UI_CPU_WEIGHT` | `20` | CPU 相对权重 |
| `SQARVO_UI_MEMORY_HIGH` | `3G` | 内存回收与节流阈值 |
| `SQARVO_UI_MEMORY_MAX` | `5G` | 内存硬上限 |
| `SQARVO_UI_IO_WEIGHT` | `20` | I/O 相对权重 |
| `SQARVO_UI_TIMEOUT` | `900` | 单次执行时限（秒，包含隔离启动） |

作用域内会从 cgroup v2 读回并检查配置。缺失或不匹配时返回 BLOCKED。若用户级 cgroup
没有 I/O controller，可显式设置 `SQARVO_UI_IO_WEIGHT=off`，入口会报告该限制已关闭；
CPU、内存与会话隔离依然是必需项。无 systemd 用户管理器或缺少所需 controller 的 CI
runner 会明确报告 BLOCKED。测试入口不修改宿主 systemd 配置。

## 输出契约

成功（stdout 只有摘要，无树、无日志、无截图）：

```
UI TEST: PASS
Passed: 10
Failed: 0
Duration: 41.2s
```

失败（只展开失败项；可访问性子树与应用文本日志落盘到本次运行的 `logs/`）：

```
UI TEST: FAIL
Test: test_execute_sql
Step: verify_result
Expected: 3 rows
Actual: 0 rows
Error: Query result mismatch
Diagnostic: tests/ui/linux/results/run.…/logs/test_03_sql.py_test_execute_sql.txt
```

入口打印独立的 `tests/ui/linux/results/run.*/` 路径，其中 `report.json` 记录用例
outcome/duration/step/expected/actual，`runner.log` 记录资源读回与服务文本诊断。
启动失败或中断时可能只有诊断日志。退出码 0 为通过，1 为用例失败，3 为环境 BLOCKED，
124 为超时，130/143 分别为 SIGINT/SIGTERM。其余 pytest 退出码原样传递。

## 分层

| 层 | 文件 | 职责 |
|---|---|---|
| 自动化适配 | `adapter.py` | 按 role+name 语义定位、do_action 点击、EditableText 输入、状态读取、显式等待；失败诊断 dump |
| 可复用操作 | `ops.py` | 打开连接对话框、建 SQLite 连接、执行 SQL、读结果网格、树导航、关窗；`step()`/`expect()` 结构化失败 |
| 场景 | `test_*.py` | 组合 ops 断言用户可观察行为；夹具在 `conftest.py` |

选择器命名一律对齐 `docs/ui-specification/components/*.toml` 的 `a11y.name`；不依赖
坐标、颜色、字号、布局位置。

## 隔离与确定性

- 每次运行自动分配 Xvfb DISPLAY，禁止监听 X11 TCP，使用运行专属 Xauthority。
  Openbox、GTK 和 AT-SPI 共用该 X11 显示，GTK 使用 Cairo，宿主 Wayland 与 D-Bus
  地址不进入测试环境。
- 每次运行使用临时 HOME、XDG config/data/cache/state/runtime、私有会话 D-Bus 与
  专用 Secret Service。只启动所需的 AT-SPI 和凭据服务，凭据来自合成测试数据。
- 每个应用实例另有独立 XDG 配置层、合成 SQLite 库、显式 Unix Socket 端点和 QA 应用
  ID。连接档案随实例保存，后端由前端启动，MCP HTTP 用例在本地回环动态选择端口。
- 正常退出、失败、超时和信号中断均按本次 scope 停止进程并清除临时目录。应用看门狗与
  scope 运行时限提供兜底。外层入口被 SIGKILL 时，scope 到期会回收进程，临时目录
  可能仍需清理。测试通过语义 API 操作控件，无 `/dev/uinput` 依赖。

## 现有用例限制

- 测试沿用 AT-SPI Action、EditableText 和 GAction。建 SQLite 档案走 `--qa-profile`，
  由同一控件回调处理。鼠标手势入口仍未纳入现有覆盖，主动断开菜单保留显式 skip，
  关窗、后端会话回收及重启未连接仍由现有用例验证。
- 新行编辑器双击入口保留原有 skip，初始状态规则由 Linux Rust 单测覆盖。
- 功能验收范围为交互、状态及可访问性语义。
- MCP HTTP 用例的端口由内核临时分配，释放后再交给应用绑定。并行时若被其他本机进程
  抢占，相关用例会报告失败。

## Agent 使用约定（任务 §9）

改 UI 功能后：优先复用 ops 里已有操作 → 增/改对应场景 → 跑最小相关子集 → 只看摘要；
失败时只读该用例的 `Diagnostic` 文件。已有测试覆盖的行为不要再手工点界面，
不要为测试截图或做视觉分析。

用例与组件的对应：每条用例在 docstring 标注对应注册表组件 `id`；改组件先按 `id`
grep 本目录找现有用例，该端缺失覆盖时**先补用例再验收**（根 AGENTS §13、
frontend/AGENTS §13）。本框架是三端功能用例库模式的首发实现，Windows（UIA）/
macOS（XCUITest）建库时以本目录三层结构为参照。

HTTP E2E 中，复制场景验收 GDK 接受内容后的界面反馈；实际复制文本另由 GTK 单测断言。
