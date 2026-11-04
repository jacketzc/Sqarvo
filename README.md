# Sqarvo

开源、轻量、跨平台的原生桌面数据库客户端。目标是提供日常数据库管理所需的能力，
同时保持低资源占用、快速启动和原生桌面体验。

- 不使用 Electron / Chromium / WebView 作为 UI 运行时；
- 数据库能力集中在独立的 Rust 后端进程中；
- 前端与后端通过 gRPC + Protobuf 通信；
- 数据库通过可扩展的 Driver SPI 接入。

> 当前仓库处于**骨架阶段**。传输层、协议契约与部分通用逻辑已可编译、可测试，
> 但**尚未实现任何数据库驱动**，因此还不能连接真实数据库。见下方「当前状态」。

## 目录结构

```text
.
├── AGENTS.md                 # 仓库级开发准则
├── proto/                    # Protobuf 契约（前后端唯一事实来源）
│   ├── common/v1/            # 错误、分页、值/列类型
│   └── services/v1/          # system / connection / metadata / query / transaction
├── backend/                  # Rust workspace
│   ├── apps/backend-service/ # 后端进程入口（本地 IPC + token 认证）
│   └── crates/
│       ├── common/           # 错误模型与标识符
│       ├── driver-api/       # Driver SPI：能力、值模型、元数据、会话
│       ├── drivers/          # 驱动注册表（当前无内置驱动）
│       ├── connection/       # 连接与会话生命周期
│       ├── query-engine/     # 查询登记、并发上限、幂等取消
│       ├── metadata/         # 元数据分页逻辑
│       ├── transaction/      # 显式事务状态机
│       ├── local-store/      # 路径、端点文件、token、凭据引用
│       └── rpc-server/       # gRPC 服务实现与本地传输层
├── frontend/                 # 三个平台的原生前端（UI 骨架，未接后端）
│   ├── windows/              # WinUI 3 + C#
│   ├── linux/                # GTK4 + libadwaita + Rust
│   └── macos/                # SwiftUI + Swift
├── docs/                     # 产品、架构、API、驱动、UI、决策记录
├── tests/                    # 契约、集成测试与测试数据
└── packaging/                # 各平台打包骨架
```

## 当前状态

已实现且**有测试覆盖**（`cargo test`）：

| 能力 | 说明 |
|---|---|
| Protobuf/gRPC 契约 | 5 个域服务，构建时由 `proto/` 生成 Rust 代码 |
| 本地传输 | Linux/macOS Unix 域套接字（目录 0700、socket 0600）；回环 TCP 仅用于 Windows/调试 |
| 访问控制 | 每进程随机 256 位 token，写入 0600 端点文件；所有 RPC 经拦截器校验 |
| Driver SPI | v0.2：多语句/语句摘要、事务与元数据接口、单元格截断 |
| SQLite 驱动 | 连接 / 只读 / 单语句执行 / 分批游标（未注册，gRPC 接线中） |
| 驱动测试基架 | 脚本化测试会话 + 最小符合性检查 `smoke`（SQLite 已通过） |
| 连接管理 | 会话建立/断开/列表、驱动不存在时明确报错 |
| 查询登记 | 并发上限、批次上限、幂等取消 |
| 元数据分页 | 不透明 token、页大小上限、非法 token 拒绝 |
| 本地存储 | 目录解析与权限、端点文件原子写入、凭据引用接口 |
| gRPC 冒烟测试 | 真实 UDS + token 的端到端调用 |

**尚未实现**（调用会得到 `UNIMPLEMENTED` 或明确的能力错误，不会伪造成功）：

- 任何数据库驱动（PostgreSQL / MySQL / SQLite 等）；
- SQL 执行与结果集分批拉取；
- 元数据对象树、列信息、DDL 生成；
- 显式事务绑定到会话；
- 操作系统凭据存储（当前仅有接口与「不可用」实现）；
- 前端与后端的实际连接（三端 UI 仅为骨架）；
- Windows Named Pipe + ACL 传输（当前回退为回环 TCP + token）。

## 构建与测试

需要 Rust 1.88 及以上（`rust-toolchain.toml` 固定为 stable）。**不需要**预装 `protoc`：
构建脚本使用 `protoc-bin-vendored` 生成描述符。

```bash
cd backend
cargo build
cargo test
cargo clippy --all-targets
cargo fmt --check
```

运行后端：

```bash
cargo run -p sqarvo-backend-service
# 指定 socket：   --socket /tmp/sqarvo.sock
# 调试用回环 TCP：--listen 127.0.0.1:0
# 日志级别：      --log-filter debug  或环境变量 SQARVO_LOG
```

启动后会写入端点描述文件（含 token，权限 0600）：

- Linux：`$XDG_RUNTIME_DIR/sqarvo/backend-endpoint.json`（无 `XDG_RUNTIME_DIR` 时位于数据目录 `run/`）
- macOS：`~/Library/Application Support/io.sqarvo.Sqarvo/run/backend-endpoint.json`

正常退出（Ctrl-C / SIGTERM）会删除 socket 与端点文件。

> 验证范围：本项目当前只在 Linux 上执行过构建与测试。Windows 与 macOS 的传输/UI
> 代码尚未在本机验证，详见各平台 README。

## 文档

规划与决策（随代码版本化，是长期有效的唯一来源）：

- [产品愿景与范围](docs/product/vision.md)
- [产品定位与竞品差异](docs/product/positioning.md)
- [产品需求（含优先级与实现状态）](docs/product/requirements.md)
- [路线图](docs/product/roadmap.md)
- [开发实施计划](docs/planning/development-plan.md)
- [里程碑](docs/planning/milestones.md)
- [发布计划](docs/planning/release-plan.md)
- [风险登记](docs/planning/risk-register.md)
- [规划文档与任务管理约定](docs/planning/README.md)

技术与设计：

- [架构总览](docs/architecture/overview.md)
- [本地 IPC 与访问控制](docs/architecture/local-ipc.md)
- [gRPC 约定](docs/api/grpc-conventions.md)
- [Driver SPI](docs/database/driver-spi.md)
- [UI 设计原则](docs/ui-specification/design-principles.md)
- [决策记录](docs/decisions/)

## 任务与进度

- 任务看板：[Sqarvo 开发路线](https://github.com/users/jacketzc/projects/2)
- 任务列表：[Issues](https://github.com/jacketzc/Sqarvo/issues)（模板见 `.github/ISSUE_TEMPLATE/`）；
- 里程碑与版本目标：仓库内 `docs/planning/`；
- 同一份计划只在一个地方维护：仓库保存长期规划，GitHub 保存动态执行状态。


## 安全

数据库客户端会处理高度敏感的凭据与生产数据。请不要在 Issue、PR、日志或测试快照中
提交真实密码、连接串、数据库结构或数据。漏洞报告方式见 [SECURITY.md](SECURITY.md)。

## 参与贡献

见 [CONTRIBUTING.md](CONTRIBUTING.md)。提交前必须运行格式化、Clippy 与测试；
任务需关联 GitHub Issue 与里程碑，PR 使用仓库模板。

## 许可

[Apache License 2.0](LICENSE)。本项目为独立实现，与 Navicat 等商业产品无任何关联，
不使用其代码、图标、文案或界面资源。
