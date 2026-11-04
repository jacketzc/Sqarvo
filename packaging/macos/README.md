# macOS 打包

## 规划

- `.app` 包：SwiftUI 主程序 + 内嵌的 `sqarvo-backend` 可执行文件（`Contents/MacOS/`）；
- 签名与公证（notarization）流程；
- 后端由应用通过 `Process`/XPC 启动，退出时清理 socket 与端点文件。

## 待完成

- Xcode 工程与 Info.plist（SwiftPM 可执行目标不足以打包 .app）；
- 沙盒与网络/文件权限声明（本地 socket 通信需要评估 entitlements）；
- Universal Binary（arm64 + x86_64）构建；
- 公证与自动更新方案。

当前仓库不包含可用的 .app；本目录仅为占位与决策记录。
