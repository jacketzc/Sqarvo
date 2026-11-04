# 前端

三个平台各自使用原生技术实现，功能与核心交互语义保持一致，不共享 UI 代码。

| 平台 | 技术 | 目录 | 状态 |
|---|---|---|---|
| Windows | WinUI 3 / Windows App SDK / C# | [windows](windows/) | 窗口骨架，未接后端，未构建验证 |
| Linux | GTK4 + libadwaita / Rust | [linux](linux/) | 已接后端：握手 / 进程管理 / 连接对话框 / SQL 执行与取消 / 结果虚拟化（v0.2，见 `docs/releases/v0.2.md`） |
| macOS | SwiftUI / Swift | [macos](macos/) | 窗口骨架，未接后端，未构建验证 |

共同约束：

- 禁止 Electron / Chromium / CEF / WebView 作为 UI 运行时；
- 不实现数据库协议，不直连数据库；一切数据库操作经 gRPC 调用后端；
- 后端地址与 token 从端点描述文件读取（见 `docs/architecture/local-ipc.md`）；
- UI 线程禁止阻塞操作；必须能表达 Loading / Empty / Success / Error / Cancelled / Disconnected；
- 未实现功能必须明确标注，不得用占位数据伪装成可用。

## 后端启动顺序（规划）

1. 前端检查端点描述文件是否存在、`protocol_version` 主版本是否兼容、`pid` 是否存活；
2. 不存在或不兼容时启动 `sqarvo-backend`（随应用分发）；
3. 轮询等待端点文件出现（带超时），读取 socket 路径与 token；
4. 建立 gRPC Channel（Unix socket / 回环 TCP 自定义连接器），携带 `x-sqarvo-token`；
5. 退出时向子进程发送终止信号，由后端清理 socket 与端点文件。

以上流程已在 Linux 前端实现（`frontend/linux/src/backend/endpoint.rs`、
`src/backend/supervisor.rs`、`src/backend/worker.rs`，PR #36），细节与实测记录见
[`frontend/linux/README.md`](linux/README.md)。Windows 与 macOS 前端**尚未实现**该流程，
仍是窗口骨架，未构建、未验证。
