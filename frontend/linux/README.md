# Linux 前端（GTK4 + libadwaita）

## 状态

窗口骨架。**尚未接入后端 gRPC**，界面固定显示「后端尚未连接」。

## 依赖

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev build-essential pkg-config
```

GTK 4.16 及以上（`gtk4` crate 的 `v4_16` feature）。

## 构建

```bash
cd frontend/linux
cargo build
cargo run
```

> 当前仓库的验证环境没有安装 GTK4/libadwaita 开发包，因此本 crate **未在本机编译验证**。
> 首次在安装依赖的机器上构建时，请修正可能存在的 crate API 差异，并在 PR 中说明。

## 约定

- 所有后端调用必须在后台执行（`tokio` 运行时或 `std::thread`），通过 channel 把结果
  派发回 GTK 主线程；禁止阻塞主循环。
- UI 必须能表达 Loading / Empty / Success / Error / Cancelled / Disconnected 六种状态。
- 遵循 GNOME HIG；非 GNOME 桌面环境下也要有合理回退（不假设 GNOME Shell）。
- 不引入 WebKitGTK 或任何 Web 视图实现界面。
