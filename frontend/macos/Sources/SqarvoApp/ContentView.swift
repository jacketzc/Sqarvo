import SwiftUI

/// 主界面骨架：明确标注后端未连接，不做任何数据库操作。
struct ContentView: View {
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "cylinder.split.1x2")
                .font(.system(size: 48))
                .foregroundStyle(.secondary)

            Text("Sqarvo")
                .font(.largeTitle)

            Text("界面骨架 · 后端尚未连接")
                .foregroundStyle(.secondary)

            Label("数据库功能未实现", systemImage: "exclamationmark.triangle")
                .foregroundStyle(.orange)
        }
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

#Preview {
    ContentView()
}
