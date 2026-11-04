import SwiftUI

/// Sqarvo macOS 应用入口（骨架）。
///
/// 说明：SwiftPM 可执行目标只能生成命令行可执行文件，不能直接产出带
/// Info.plist 的 .app。正式分发前需要增加 Xcode 应用目标或打包脚本，见 README。
@main
struct SqarvoApp: App {
    var body: some Scene {
        WindowGroup("Sqarvo") {
            ContentView()
                .frame(minWidth: 900, minHeight: 600)
        }
        .commands {
            // 骨架阶段没有「新建」业务，先移除默认菜单项避免误导。
            CommandGroup(replacing: .newItem) {}
        }
    }
}
