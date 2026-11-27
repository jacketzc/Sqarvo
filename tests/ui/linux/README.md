# UI 功能测试 — Linux（GTK4 / AT-SPI）

Active 前端共同规范见 `../README.md`（伞层）；本文件只写 Linux 端实现与环境限制。

用可重复的自动化功能测试替代 agent 反复交互式点界面。只验证**交互与功能**
（控件响应、状态流转、可访问性名称、真实数据），不做像素/样式/截图断言
（ADR 0029、前端 AGENTS §13）。

## 依赖与准备

- Linux 需有可访问的 systemd 254+ 用户管理器、cgroup v2 以及已下放的 CPU 和内存
  controller。默认允许用户级 runner 缺少 I/O controller。
- Ubuntu 26.04 的系统依赖见下方清单。现有开发机只缺 `xvfb`、`openbox` 时，
  由维护者执行 `sudo apt-get update && sudo apt-get install --no-install-recommends xvfb openbox`。
- 在已有 systemd 用户会话的 Ubuntu 主机上，可按同一清单检查全部系统依赖，
  并按需安装缺失的包：

```sh
dpkg -l bash coreutils grep util-linux systemd dbus-user-session dbus-daemon \
  xvfb openbox xauth x11-utils libglib2.0-bin at-spi2-core gnome-keyring \
  python3 python3-gi gir1.2-atspi-2.0 gir1.2-secret-1 libgtk-4-1 libadwaita-1-0
command -v Xvfb openbox xauth xdpyinfo xprop xwininfo dbus-run-session gdbus gnome-keyring-daemon flock
systemctl --user show -p ControlGroup -- -.slice
/usr/bin/python3 -c 'import gi; gi.require_version("Atspi", "2.0"); gi.require_version("Secret", "1")'
```

  `dpkg -l` 的 `ii` 表示已安装。`systemctl --user` 应返回用户管理器的 cgroup 路径。
  需要一次安装整套系统依赖时，用以下命令；`apt` 会解析传递依赖。

```sh
sudo apt-get update
sudo apt-get install --no-install-recommends bash coreutils grep util-linux systemd \
  dbus-user-session dbus-daemon xvfb openbox xauth x11-utils libglib2.0-bin \
  at-spi2-core gnome-keyring python3 python3-gi gir1.2-atspi-2.0 gir1.2-secret-1 \
  libgtk-4-1 libadwaita-1-0
```

- 在 scope 外构建前端和后端、执行 `just ui-test-env`（需 `uv`）。入口依次查找仓库
  release/debug 产物及已安装前端，可用 `SQARVO_UI_BIN`、`SQARVO_BACKEND_BIN`、
  `SQARVO_MCP_BIN` 明确选择。HTTP E2E 还需 PATH 上的 Codex CLI 和 MCP 二进制。
  安装系统包后，现有开发环境使用普通用户的 PATH 即可运行，无需添加临时工具目录。
  缺少必要依赖时，入口报告 BLOCKED 及安装指引。

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
默认发现排除 `results/` 运行产物目录。需单独运行的探针通过显式文件参数传入。

用真实 MariaDB 补测查询目标时，使用专用合成实例，绑定本地回环端口 55306。按
[集成测试说明](../../integration/README.md) 建立测试账号 `sqarvo_test` 与两个测试库，
再向临时实例加载 `tests/integration/mariadb_ui_catalog_fixture.sql`。运行
`scripts/run-ui-tests-isolated.sh tests/ui/linux/mariadb_catalog_probe.py`，验证初始库确认、权限拒绝、
失败后的会话释放及同名表数据归属。

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
| `SQARVO_UI_IO_WEIGHT` | `auto` | 有 I/O controller 时应用并读回权重 `20`；缺少 controller 时警告并继续。 |
| `SQARVO_UI_TIMEOUT` | `900` | 单次执行时限（秒，包含隔离启动） |

作用域内会从 cgroup v2 读回并检查配置。`SQARVO_UI_IO_WEIGHT=off` 显式关闭 I/O
权重。设置 `1`–`10000` 启用严格模式，controller 不可用或读回不符均返回 BLOCKED。
`auto` 只在 I/O controller 未下放时继续，已下放但无法应用权重时仍返回 BLOCKED。
用户管理器、CPU、内存和会话隔离始终是必需项。测试入口使用用户级 scope 配置。

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
硬编码坐标、颜色、字号、布局位置。鼠标手势用例从已定位控件的 AT-SPI 边界换算输入点。

## 隔离与确定性

- 每次运行自动分配 Xvfb DISPLAY，禁止监听 X11 TCP，使用运行专属 Xauthority。
  Openbox、GTK 和 AT-SPI 共用该 X11 显示，GTK 使用 Cairo，宿主 Wayland 与 D-Bus
  地址不进入测试环境。
- 每次运行使用临时 HOME、XDG config/data/cache/state/runtime、私有会话 D-Bus 与
  专用 Secret Service。只启动所需的 AT-SPI 和凭据服务，凭据来自合成测试数据。
- 每个应用实例另有独立 XDG 配置层、合成 SQLite 库、显式 Unix Socket 端点和 QA 应用
  ID。连接档案随实例保存，后端由前端启动，MCP HTTP 用例在本地回环动态选择端口。
- 正常退出、失败、超时和信号中断均按本次 scope 停止进程并清除临时目录。外层入口
  被 SIGKILL 时，scope 有运行时限。后续启动会检查当前用户的旧运行目录，仅在取得
  实例锁、确认 scope 已结束且 cgroup 不存在后删除。无法确认的目录会保留并提示，
  包括旧版入口遗留、没有本版标记的目录。测试通过语义 API 定位控件。SQL 快捷键和鼠标手势
  通过同一私有 Xvfb 的 XTEST 发送，注入前核对运行作用域、DISPLAY 和 Xauthority，
  无 `/dev/uinput` 依赖。GTK 的 AT-SPI 提供窗口内边界，鼠标适配层通过
  `xprop` 和 `xwininfo` 获取私有显示上的活动窗口原点。

入口回归测试可用 `/usr/bin/python3 tests/ui/linux/runner_regression.py` 独立执行。
在依赖安装完成、systemd 用户会话可用的主机上，可加 `--host` 实测并行与 SIGKILL 回收。

## 现有用例限制

- 测试沿用 AT-SPI Action、EditableText 和 GAction。建 SQLite 档案走 `--qa-profile`，
  由同一控件回调处理。SQL 选区执行覆盖工具栏按钮和编辑器上下文菜单的鼠标路径。
  主动断开从隔离 X11 的真实指针右键打开资源菜单，并经确认框断开。关窗、会话回收及重启状态仍由独立用例验证。
- 新行编辑器用 X11 实际双击语义网格单元格，通过 AT-SPI 焦点事件定位弹层。用例核对
  默认省略状态、输入和取消，初始状态规则另由 Linux Rust 单测覆盖。
- 功能验收范围为交互、状态及可访问性语义。
- MCP HTTP 用例的端口由内核临时分配，释放后再交给应用绑定。并行时若被其他本机进程
  抢占，相关用例会报告失败。

## Agent 使用约定（任务 §9）

改 UI 功能后：优先复用 ops 里已有操作 → 增/改对应场景 → 跑最小相关子集 → 只看摘要；
失败时只读该用例的 `Diagnostic` 文件。已有测试覆盖的行为不要再手工点界面，
不要为测试截图或做视觉分析。

用例与组件的对应：每条用例在 docstring 标注对应注册表组件 `id`；改组件先按 `id`
grep 本目录找现有用例，该端缺失覆盖时**先补用例再验收**（根 AGENTS §13、
frontend/AGENTS §13）。

HTTP E2E 中，复制场景验收 GDK 接受内容后的界面反馈；实际复制文本另由 GTK 单测断言。
