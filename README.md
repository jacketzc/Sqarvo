<p align="center">
  <img src="docs/assets/brand/sqarvo-logo.svg" width="112" height="112" alt="Sqarvo logo">
</p>

<h1 align="center">Sqarvo</h1>

<p align="center">开源、轻量、跨平台的原生桌面数据库客户端</p>

## 为什么是 Sqarvo

**平台原生 UI，不是网页套壳。**
Windows 用 WinUI 3 + C#，Linux 用 GTK4 + libadwaita，macOS 用 SwiftUI —— 每个平台都用自己的原生
控件与交互约定。不使用 Electron / Chromium / CEF / WebView，界面里没有一行 JavaScript。
数据库能力全部在独立的 Rust 后端进程里，前端只通过 gRPC + Protobuf 与它通信。

**低占用。**
单个 Rust 后端进程 + 原生前端，启动快、常驻内存小。查询结果分批拉取、表格虚拟化，大表与大结果集
不会整块读进内存；连接、游标、临时文件都有明确的生命周期与清理路径。

**简洁 UI。**
六区域工作区（工具条 · 对象树 · 标签条 · 工作区 · 信息面板 · 状态栏），一个标签一条数据库会话，
切标签不重建连接。常用操作键盘可达；加载 / 空 / 错误 / 取消 / 断连五态在界面上明确可见，
不用弹窗替代状态说明。

![Sqarvo 主窗口：对象树、表数据网格与信息面板 DDL（Linux / GTK4）](docs/assets/screenshots/linux-main-window.png)

## 当前状态

Linux 端已可端到端使用（SQLite 与 MariaDB）；Windows / macOS 仍是窗口骨架，尚未接后端。

- **已有**：连接与测试连接、密码只写系统凭据库、对象树与元数据（逐层懒加载、按名搜索）、
  SQL 编辑器与分批结果、表数据浏览 / 编辑 / 插入 / 显式事务、导出 CSV / INSERT / 转储表、
  DDL 只读查看。
- **未做**：PostgreSQL 驱动、多语句执行、DDL 修改、导入、结构对比与 ER 图、Windows / macOS
  接入、远程 TLS。未实现的能力返回明确的 `UNIMPLEMENTED` / `capability.unsupported`，不伪造成功。
- 能力清单与支持矩阵以 [产品需求](docs/product/requirements.md)、
  [发布计划](docs/planning/release-plan.md) 为准；版本说明见 [`docs/releases/`](docs/releases/)。

## 目录结构

```text
proto/                 Proto 契约（前后端唯一事实来源）
backend/               Rust workspace：Driver SPI、驱动（SQLite / MariaDB）、
                       查询与事务、元数据、凭据、本地传输、gRPC 服务
frontend/
  linux/               GTK4 + libadwaita + Rust（已接入后端）
  windows/             WinUI 3 + C#（窗口骨架）
  macos/               SwiftUI + Swift（窗口骨架）
docs/                  产品、规划、架构、API、驱动、UI 规范与决策记录
tests/                 契约 / 集成测试与测试数据
packaging/             各平台打包
```

分层与职责边界（以及不允许出现的东西）写在 [AGENTS.md](AGENTS.md)。

## 构建与运行

需要 Rust 1.88+；**不需要**预装 `protoc`（构建脚本内置）。

```bash
cd backend
cargo build && cargo test
cargo clippy --all-targets && cargo fmt --check
```

运行后端（启动后写入含 token 的端点文件，权限 0600，正常退出时删除）：

```bash
cargo run -p sqarvo-backend-service
# --socket /tmp/sqarvo.sock   --listen 127.0.0.1:0（调试）   --log-filter debug
```

运行 Linux 前端（GTK 4.16 / libadwaita 1.6+，会自行拉起后端并完成握手）：

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev build-essential pkg-config
cd frontend/linux && cargo run
```

MariaDB 集成测试需要真实数据库（未设置环境变量时用例跳过并打印原因）：

```bash
cd backend
SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test' \
  cargo test -p sqarvo-driver-mariadb --test integration
```

测试库启动方式见 [tests/integration/README.md](tests/integration/README.md)。

> 目前只在 Linux 上做过构建与测试；Windows / macOS 的传输与 UI 代码未在本机验证。

## 文档

- 产品：[愿景](docs/product/vision.md) · [定位](docs/product/positioning.md) ·
  [需求](docs/product/requirements.md) · [路线图](docs/product/roadmap.md)
- 规划：[开发计划](docs/planning/development-plan.md) · [里程碑](docs/planning/milestones.md) ·
  [发布计划](docs/planning/release-plan.md) · [风险](docs/planning/risk-register.md)
- 技术：[架构总览](docs/architecture/overview.md) · [本地 IPC](docs/architecture/local-ipc.md) ·
  [gRPC 约定](docs/api/grpc-conventions.md) · [Driver SPI](docs/database/driver-spi.md)
- UI：[设计原则](docs/ui-specification/design-principles.md) ·
  [区域与版式](docs/ui-specification/workspace-model.md) ·
  [行为契约与全局规则](docs/ui-specification/ui-behavior-contract.md) ·
  [组件注册表](docs/ui-specification/components/README.md) ·
  [缩放与窗口尺寸](docs/ui-specification/scaling-and-window-sizing.md)
- 决策：[ADR](docs/decisions/)

## 任务与进度

任务看板 [Sqarvo 开发路线](https://github.com/users/jacketzc/projects/2)，
任务列表 [Issues](https://github.com/jacketzc/Sqarvo/issues)。
长期规划随代码放在 `docs/`，GitHub 只保存动态执行状态。

## 安全

数据库客户端处理高度敏感的凭据与生产数据。请不要在 Issue、PR、日志或测试快照里提交真实密码、
连接串、库结构或数据。漏洞报告方式见 [SECURITY.md](SECURITY.md)。

## 参与贡献

见 [CONTRIBUTING.md](CONTRIBUTING.md)。提交前必须跑格式化、Clippy 与测试；任务要关联 Issue 与
里程碑，PR 使用仓库模板。

## 许可

[Apache License 2.0](LICENSE)。本项目为独立实现，与 Navicat 等商业产品无任何关联，不使用其代码、
图标、文案或界面资源。
