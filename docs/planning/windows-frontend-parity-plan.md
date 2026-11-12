# Windows 前端补偿开发计划（对齐 Linux 客户端行为）

状态：进行中（2026-11）。本文件是 Windows 端从「静态外壳」补齐到「与 Linux 客户端行为一致」的
唯一排期口径。行为细节以 `docs/ui-specification/ui-behavior-contract.md` 为准；本文件只规定
**怎么做、按什么顺序做、用什么证据验收**。

## 1. 现状与差距（实测结论）

Linux 客户端已实现：连接管理、对象树、多 SQL 标签、执行/取消、结果集分页、表数据浏览与编辑、
事务、导出、DDL、日志面板、信息面板、状态栏、快捷键、密度档、窗口几何记忆。

Windows 端（`frontend/windows/`，约 2470 行）只有「窗口层 + 六区域静态骨架」：

| 有真实现 | 只有占位/假实现 | 完全没有 |
|---|---|---|
| 六区域宿主控件、`metrics.toml` 构建期生成、主题四套 `ThemeDictionaries`、窗口尺寸/DIP/最小尺寸、`window.json` v2 持久化、6 个诊断开关 | 对象树单硬编码根、`Objects` 页两行文字、信息面板空 `Grid`、树搜索禁用、分页器写死 `0 / 0` | gRPC 通道、后端进程管理、Transport 抽象、`connections.json`、凭据存储、连接对话框、SQL 编辑器、结果网格、表数据浏览/编辑、事务、导出、日志、六态中的 Error/Cancelled/ReadOnly |

结论：缺的不是「界面细节」，是**整个业务层**。因此不能靠继续改 XAML 补，必须先落通信与进程层。

## 2. 后端已具备 Windows 传输（重要，决定了工作量）

后端 `LocalListener` 在非 Unix 上自动走**回环 TCP + 随机端口 + 每进程 token**，前端通过端点
描述文件发现端点。也就是说 Windows 不需要 Named Pipe 也能连通，差距集中在以下几处（明细见
`03-backend-contract.md`）：父进程消失检测（后端已实现，待 Windows 实机验证）、pid 判活、
SIGTERM 退出、端点文件 ACL、Windows Credential Manager。

| Linux 机制 | 位置 | Windows 替换 |
|---|---|---|
| UDS 监听/连接 | `crates/rpc-server/src/transport/mod.rs` | 已支持：`127.0.0.1:0` TCP（客户端只实现 TCP 分支） |
| 端点文件 `0600`/目录 `0700` | `local-store/src/endpoint.rs`、`paths.rs` | 显式 DACL：仅当前用户 SID 完全控制、断继承 |
| `PR_SET_PDEATHSIG` | `crates/parent-watch/src/unix.rs` | 已实现（`crates/parent-watch/src/windows.rs`）：`OpenProcess` + 创建时间校验 + `WaitForSingleObject`；Job Object（`KILL_ON_JOB_CLOSE`）归前端，见 ADR 0002 附记 |
| `getppid()` 轮询 | 同上（每 500 ms 一次） | 已实现：Windows 不轮询，父进程句柄 signal 即退出（`--parent-pid` 未给出时不装任何监视） |
| `/proc/<pid>` 判活 | `frontend/linux/src/backend/endpoint.rs` | `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `GetExitCodeProcess` |
| `SIGTERM` → 5s → `SIGKILL` | `frontend/linux/src/backend/supervisor.rs` | 先 gRPC 优雅退出（若有）→ `TerminateProcess`；前端兜底删端点文件 |
| Secret Service | `local-store/src/credentials/mod.rs` | Windows Credential Manager（`CredWrite/CredRead/CredDelete`，`CRED_TYPE_GENERIC`） |

## 3. 架构决定：新增 `Sqarvo.Core` 类库（不引入 Web/新框架）

**问题**：WinUI 3 的 XAML 与 code-behind 在 Linux 上无法编译，而本仓库的主要开发与 CI 在 Linux
（`ci.yml` 不含 Windows）。若业务逻辑全部写在 WinUI 工程里，则每一行都无法在合入前验证。

**决定**：把「与 UI 无关」的部分放进一个普通 `net8.0` 类库 `frontend/windows/Sqarvo.Core`，
WinUI 工程只保留窗口、控件、绑定与平台外观适配。

```
frontend/windows/
├── Sqarvo.Core/                 # net8.0，无 WinUI/Windows SDK 依赖
│   ├── Ipc/                     # 端点文件读取、Transport 抽象（TCP 具体实现，管道留接口）
│   ├── Process/                 # BackendSupervisor（spawn/就绪/健康/退出）
│   ├── Protocol/                # gRPC channel 装配、token metadata、deadline、取消
│   ├── Clients/                 # 7 个 service 的封装（Connection/Metadata/Query/Transaction/TableData/DataTransfer/System）
│   ├── Platform/                # 可注入的平台策略：IProcessLauncher、IPidLiveness、IPathLayout
│   └── Model/                   # 连接配置、结果集页、脏行等纯模型
├── Sqarvo.Core.Tests/           # xunit；Linux 上真跑（含对真后端进程的集成测试）
└── Sqarvo.Windows/              # 现有的 WinUI 工程，引用 Sqarvo.Core
```

依赖方向单向：`Sqarvo.Windows → Sqarvo.Core → Grpc.Net.Client / Google.Protobuf`。Core 不得
`using` 任何 `Microsoft.UI.*`。平台差异通过 `Platform/` 下的接口注入，Windows 实现在 WinUI 工程
（或 `Sqarvo.Core` 内的 `#if WINDOWS` 文件，二选一，按实现体量定）。

**可验证性收益**（这是选它的核心理由）：

- `dotnet build` / `dotnet test` 在 Linux 上可跑 Core 与测试工程；
- 后端在 Linux 上也能 `--listen 127.0.0.1:0` 起 TCP，因此**同一份 Windows 通信代码可以在 Linux
  上对真后端做集成测试**（连接测试 → 对象树 → SQL → 分页 → 表数据全链路）；
- Rust 侧 `cargo check --target x86_64-pc-windows-msvc` 在本机可用（已验证 `sqarvo-local-store`），
  所以 `cfg(windows)` 代码在合入前能通过类型检查。

## 4. 工作包

顺序即依赖顺序；`WP1`/`WP2` 可与 `WP3` 并行。

| ID | 分支 | 范围 | 验收证据 |
|---|---|---|---|
| WP1 | `agent/win-backend-port` | 后端 Windows 适配：端点文件 DACL、父进程监视（`OpenProcess`+创建时间）、`pid_alive`、退出路径（`TerminateProcess` + 前端兜底）、`--endpoint-file` 显式参数替代 `XDG_RUNTIME_DIR`、Credential Manager（`CredWrite/CredRead/CredDelete`） | ✅ 已合并（#165/#166）：`local-store` Windows DACL + Credential Manager、`parent-watch`、`--endpoint-file` 优先级；真机 0/0 构建、退出无残留进程 |
| WP2 | `agent/win-core-ipc` | `Sqarvo.Core`：`Transport` 抽象 + TCP 实现、端点文件读取（严格 schema）、`BackendSupervisor`（spawn `--parent-pid`、100ms 轮询端点、pid 匹配、10s 超时、5s 健康、退出停机）、token metadata 注入 | ✅ 已合并（#167）：端点文件读取、`BackendSupervisor`、运行时分派（`OperatingSystem.IsWindows()`）；107 测试通过，含禁用 `#if WINDOWS` 的架构测试 |
| WP3 | `agent/win-window-fixes` | 窗口层修补：`XamlRoot.Changed`、`DidSizeChange`、「拖动窗口被拉回中央」修正、`Maximized` 恢复、`GridSplitter` 侧栏/面板、`SidebarWidthFor/InspectorWidthFor` 接线、未消费度量接线、`MinBase*` 改读生成值 | ✅ 已合并（#168）：DPI 变化、拖动/尺寸变化区分、最大化恢复、GridSplitter 两处、`Metrics.*MinWindow*`；真机 1280/1920/3840 三档复验 |
| WP4 | `agent/win-connections` | 连接面：`connections.json` 模型（身份键去重）、凭据引用、连接对话框（驱动选择/TLS/测试连接）、对象树连接层与右键动作 | ✅ 已合并（#169）：真机 `[connect] backend=Ready drivers=2 version=0.1.0` → 连库成功、树出现 `dev-mariadb · 已连接`、断开后重建为 `未连接`；凭据写进 Windows 凭据管理器后重连不再要密码 |
| WP5 | `agent/win-metadata` | 元数据面：树的分组层与分页（`ListNodes` + 每层 token）、`Objects` 页三列表格、信息面板三态（连接/库/表 + DDL 切换） | ✅ 已合并（#171）：真机 `--qa-metadata` 打出 `catalogs=2 / tree-tables=2 / ddl-exact=True`，截图可见树展开到表 + Objects 三列 + 面板列信息与 DDL |
| WP6 | `agent/win-query` | 查询面：标签条（新建/关闭/固定 `Objects`）、SQL 编辑器（行号、选中优先执行；**语法高亮不在本片**，如实标注纯文本）、执行/取消、结果网格（虚拟化 + 200 行分批 + 多结果集段数）、五态（Loading/Empty/Error/Cancelled/ReadOnly） | ✅ 已合并（`39f7e12` + `5586615`）：Linux Core 0 error / 107 tests、真类型探针 0 error；Windows 0 warning 0 error、三种配置 3×8s 启动门槛通过、`[query]` 首批 200 行 + `--qa-query-pages=6` 续拉到 1200 行 `has_more=False`、状态栏计数同步 |
| WP7 | `agent/win-tabledata` | 表数据面：`BrowseTable` 分页（offset 500）、排序/筛选下推、只读原因、脏行编辑、`ApplyRowChanges` 提交/撤销/确认阈值、事务状态圆点 | ✅ 已合并（#174）：真机全链路 `edit → 未提交 1 行 → submit applied=1 → verify → restore → revert 已回滚 → verify-after-revert 原值`；NULL 弱色斜体、空串按契约区分 |
| WP8 | `agent/win-surface` | 表面收尾：导出 CSV、日志面板、窗口标题规则、`--qa-report-*` 对齐、Windows CI 构建任务 | ⏳ 未做：导出 CSV、日志面板、Windows CI 构建任务 |

## 5. 验收口径（所有片共用）

- 证据必须是**真实命令输出**：远程 Windows 上 `dotnet build` / `dotnet run -- --report-*` /
  截图；Linux 上 `dotnet test`、`cargo test`、`cargo check --target`。
- 行为按语义验收不按像素（ADR 0023 §7）：优先 `--report-shell` 的 `ControlType + Name` 树，
  其次截图。
- 键盘可达性与禁用原因逐条对照行为契约；未实现的入口保持禁用并说明原因，禁止假数据。
- 每片必须同步 `docs/ui-specification/ui-behavior-contract.md`（若改变了行为）或明确声明「无契约变化」。

### 5.1 Windows 端的启动回归门槛（2026-11 加入）

WinUI 3 的启动崩溃可以是**延迟且抖动**的（同一份构建分别死在 1s / 3.5s / 4.5s / 7s），单次「活到 X 秒」
不构成证据。凡改动 `frontend/windows/Sqarvo.Windows` 的 XAML 或窗口层：

1. **同一份构建产物连跑 3 次**，每次采样 8 秒，要求 3 次都存活且 `%LOCALAPPDATA%\Sqarvo\crash.log`
   无新的 `unhandled` 行；
2. 崩溃取证先接 `Application.UnhandledException` + `AppDomain.UnhandledException` +
   `FirstChanceException`，并可用 `SQARVO_TRACE=1` 打开启动检查点；
3. `0x80040111 + Windows.ApplicationModel.LimitedAccessFeatures` 是 `RoOriginateError` 的调试噪声
   （微软官方答复：调用实际返回 S_OK），不要据此定位；
4. 二分用「整体回退到已知稳定版本 + 一块一块加回」，`Visibility="Collapsed"` 不算排除
   （控件对象仍被构造），要排除控件类型必须真换掉或删掉。
5. **改 code-behind 前后在 Linux 上跑一次「真类型」编译探针**：`net8.0-windows10.0.19041.0` +
   `EnableWindowsTargeting=true` + 真实 `Microsoft.WindowsAppSDK` 引用程序集 + 从 XAML 生成的
   `InitializeComponent()`/`x:Name` stub，可以捕获命名空间/类型/成员错误（实测能复现 `Rectangle` 缺
   `using Microsoft.UI.Xaml.Shapes;` 与 `Window` 上没有 `Shell` 这两类）。它不验 XAML 属性与事件签名。

### 5.2 #170 的回退记录

`agent/win-visual`（#170 六区域控件形态与密度）在真机上稳定秒级崩溃（5/5、3/3 两轮），
已整体回退 `Views/ShellView.xaml` 与 `Views/ShellView.xaml.cs` 到 #170 之前（commit 95d4c04），
回退后 0/3 崩。`Views/ConnectionRow.xaml` 与 `Tokens.xaml` 的新增别名保留待重建。
视觉对齐改为按区块重建（状态栏 → 信息面板 → 标签条 → 树行 → 工具条），每块过上述 3 次门槛。

## 6. 风险

| 风险 | 影响 | 处理 |
|---|---|---|
| 本机无法编译 WinUI XAML | UI 片只能靠远程构建收敛 | 把可编译部分下沉到 `Sqarvo.Core`；UI 片必须在远程实跑后才算完成 |
| Windows 凭据存储缺失 | 连接对话框主路径不可用 | 列 WP1 必做项，不留「先跳过」 |
| `Execute` 在途无法取消（协议缺口） | Windows 端取消体验与 Linux 一致地受限 | 与 Linux 同样处理（10s 超时 + 服务端 `timeout_millis`），不单方面发明协议 |
| 8 并发查询上限、`Close` 必须显式调用 | 泄漏会撞上限 | Core 里用 `IAsyncDisposable` 包查询句柄，测试覆盖「取消后必须 Close」 |
| 两个 Hermes（Linux/Windows）并行改同一仓库 | 冲突 | 每片独立 worktree + 分支；远程只做构建与验证，或按指定分支工作 |

## 7. 未决

- Windows 上端点文件的确切落地路径（`ProjectDirs` 实测值）：WP1 里由远程实测并写回文档。
- Named Pipe 传输是否需要（当前 TCP+token 已够用）：暂不做，保留 `Transport` 抽象以便后续接入。
- Windows CI：GitHub 托管 runner 有 `windows-latest`，WP8 评估是否把 `dotnet build` 加入 CI。