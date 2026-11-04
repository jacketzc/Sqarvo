# Linux 前端（GTK4 + libadwaita）

## 状态

**已接入后端 gRPC**：启动后按下面「启动顺序」完成握手，窗口在
Loading / Connected / Error / Disconnected 四态之间切换，Connected 时显示
后端版本、协议版本与传输方式。

数据库连接、SQL 执行等能力**尚未实现**（见 #27、#28）；本轮只做接入与握手，
界面不会假装数据库功能可用。

## 依赖

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev build-essential pkg-config
```

GTK 4.16 及以上（`gtk4` crate 的 `v4_16` feature），libadwaita 1.6 及以上。
本机已验证版本（2026-11-04，Ubuntu resolute）：GTK 4.22.4、libadwaita 1.9.1。

构建时**不需要**系统安装 `protoc`：`build.rs` 使用 `protoc-bin-vendored` 提供
的 protoc 生成描述符，再交给 `tonic-prost-build` 生成客户端代码到 `OUT_DIR`。
生成文件不得手工提交或修改，契约以 `proto/` 为准。

## 构建与运行

```bash
cd frontend/linux
cargo build
cargo run
```

## 启动顺序

1. 读取端点描述文件（默认 `$XDG_RUNTIME_DIR/sqarvo/backend-endpoint.json`，
   由 `directories::ProjectDirs::from("io", "sqarvo", "Sqarvo")` 推导，与后端一致）；
2. 校验 `protocol_version` 主版本（客户端支持 `v1`）；**不兼容直接进入 Error 并提示
   升级/降级，不新建后端进程**——多一个进程无法修复版本错配，还会与正在运行的后端
   争抢 socket；
3. 校验端点里的 `pid` 是否存活（Linux 读 `/proc/<pid>`）；存活则复用该后端，
   已退出则视为陈旧端点、启动自己的后端；
4. 端点文件不存在时启动 `sqarvo-backend`，轮询等待端点文件（10 秒超时，100 ms 一次），
   且必须等到 `pid` 等于自己启动的子进程，避免读到上一轮遗留文件；
5. 建立 gRPC Channel（Unix socket 自定义连接器 / 回环 TCP），每个请求携带
   `x-sqarvo-token`；
6. 调用 `SystemService.GetVersion`，用返回值再次校验协议主版本，并与端点里的
   `service_version` 对照；不一致时在界面上明确标注（不静默）；
7. 进入 Connected 后每 5 秒调用一次 `GetHealth`，失败即转为 Disconnected。

## 后端可执行文件查找顺序

1. 环境变量 `SQARVO_BACKEND_BIN` 指向的路径；
2. 与前端可执行文件同目录的 `sqarvo-backend`；
3. 仓库相对路径 `backend/target/debug/sqarvo-backend`：依次尝试可执行文件目录与
   当前工作目录的上若干层（覆盖 `cargo run` 在 `frontend/linux` 下的场景）。

显式设置了 `SQARVO_BACKEND_BIN` 但文件不可用时**不会**回退到其他候选，而是直接报错。
全部候选都找不到时，界面会列出已尝试的路径并提示设置 `SQARVO_BACKEND_BIN` 或先在
`backend/` 下执行 `cargo build -p sqarvo-backend-service`；不会 panic。

## 环境变量

| 变量 | 作用 |
|---|---|
| `SQARVO_BACKEND_BIN` | 指定后端可执行文件，优先于所有自动查找 |
| `SQARVO_ENDPOINT_FILE` | 覆盖端点描述文件路径（测试/自定义部署） |

`SQARVO_ENDPOINT_FILE` 的约束：后端自身不支持覆盖端点路径，它总是写到
`$XDG_RUNTIME_DIR/sqarvo/backend-endpoint.json`。因此当覆盖路径指向的文件不存在、
需要前端自己拉起后端时，覆盖路径必须保持 `<runtime_root>/sqarvo/backend-endpoint.json`
布局；前端会把**子进程**的 `XDG_RUNTIME_DIR` 设为 `<runtime_root>`，让后端把 socket
与端点写到同一目录。不符合该布局时前端直接报错说明原因，而不是空等超时。

## 模块结构

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 应用入口与动作（Ctrl+Q 退出、Ctrl+W 关闭窗口） |
| `src/ui.rs` | 窗口、四态状态页、事件泵与关闭流程 |
| `src/backend/endpoint.rs` | 端点定位、解析、协议主版本校验、pid 存活探测 |
| `src/backend/client.rs` | tonic Channel（UDS / TCP 自定义连接器）、token 拦截器、`SystemService` 调用 |
| `src/backend/supervisor.rs` | 后端可执行文件查找、启动、等待就绪、退出清理 |
| `src/backend/worker.rs` | 后台 tokio 线程、命令/事件通道、握手与健康探测循环 |
| `src/backend/error.rs` | 可直接展示的错误文案（绝不含 token） |
| `src/proto.rs` | 由 `proto/` 生成、位于 `OUT_DIR` 的客户端代码（勿手改） |

UI 不直接接触 socket、进程或端点文件，只与 `backend::worker` 收发消息；后续的连接
对话框（#27）与 SQL 执行（#28）应复用 `BackendClient`（`channel()` 返回可克隆的
Channel）和同一套命令/事件通道。

## 线程模型

- tokio `current_thread` 运行时独占一个后台线程（`sqarvo-backend-worker`）；
- UI → worker：`tokio::sync::mpsc`（发送不阻塞主线程）；
- worker → UI：`async_channel`，主线程用 `glib::spawn_future_local` 消费后更新界面；
- 所有 GTK 对象只在主线程操作。

## 退出清理

- 关闭窗口（标题栏或 Ctrl+W）与应用退出（Ctrl+Q、会话结束）走同一条清理路径：
  先让 worker 停止，收到 `ShutdownComplete` 后才销毁窗口；
- 只终止**自己启动**的后端：先 `SIGTERM`，最多等 5 秒，超时再 `SIGKILL`，并如实
  报告「清理未完成」；不是自己启动的后端只断开连接，绝不删除其端点文件或 socket；
- 后端收到 `SIGTERM` 后自行删除 socket 与端点文件。

## 安全约定

- 端点文件里的 token 只保存在 `BackendClient` 内部；`Debug` 实现与所有日志/错误文案
  只输出 `<redacted>` 或 token 长度；
- 不引入 WebKitGTK 或任何 Web 视图。

## 已知限制

- 数据库连接/SQL 执行未实现；Connected 只代表后端 gRPC 可用（后端可能没有任何驱动）；
- 只支持本地传输（Unix socket、回环 TCP），无 TLS / 远程后端；
- 端点覆盖（`SQARVO_ENDPOINT_FILE`）仅在标准目录布局下可用于自动拉起后端；
- 平台判定 pid 存活依赖 Linux `/proc`，其他平台保守视为存活，由连接失败暴露问题。

## 手工验证

```bash
# 1. 清理残留
pkill -x sqarvo-desktop; pkill -x sqarvo-backend; rm -rf /run/user/1000/sqarvo

# 2. 启动（终端会同时显示前端与后端日志）
cd frontend/linux && cargo run

# 3. 期望：窗口显示「已连接」与后端版本；runtime 目录出现 0600 端点文件与 socket
ls -l /run/user/1000/sqarvo

# 4. 关闭窗口后：无 sqarvo-backend 进程，端点文件与 socket 均已消失
pgrep -x sqarvo-backend; ls -A /run/user/1000/sqarvo
```
