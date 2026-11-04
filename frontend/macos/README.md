# macOS 前端（SwiftUI + AppKit）

## 状态

界面骨架。**尚未连接后端**。

## 构建

```bash
cd frontend/macos
swift build
swift run
```

`swift run` 会启动一个无 .app 包装的进程，适合验证布局。正式运行与分发需要
Xcode 应用目标（或 XcodeGen 工程），用于提供 Info.plist、图标、签名与沙盒配置。

## 后续工作

1. 增加 `Sqarvo.xcodeproj` 或 `project.yml`（XcodeGen），把 SwiftPM 目标包进 .app；
2. 生成 gRPC Swift 客户端（`grpc-swift` + `protoc-gen-grpc-swift`），并封装在通信层，
   UI 不直接依赖传输细节；
3. 后端进程由应用启动，读取 `~/Library/Application Support/io.sqarvo.Sqarvo/run/backend-endpoint.json`
   获取 socket 路径与 token；
4. 密码交由 Keychain 管理；SwiftUI 视图禁止执行阻塞数据库操作。

## 未验证

本仓库的验证环境是 Linux，**本目录未在 macOS/Xcode 上构建过**。
