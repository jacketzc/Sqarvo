# Linux 打包

## 当前内容

- `io.sqarvo.Sqarvo.desktop`：桌面入口文件，`Exec` 指向 `sqarvo-desktop`
  （由 `frontend/linux` 构建的二进制重命名安装）。

## 安装布局（规划）

```text
/usr/bin/sqarvo-desktop
/usr/bin/sqarvo-backend
/usr/share/applications/io.sqarvo.Sqarvo.desktop
/usr/share/icons/hicolor/scalable/apps/io.sqarvo.Sqarvo.svg
```

## 未完成

- 图标资源（当前没有设计资产，不能使用第三方产品图标）；
- Flatpak / AppImage 清单；
- 后端随前端启动的生命周期管理（D-Bus 或直接子进程）；
- libadwaita 版本要求与各发行版兼容性测试。

打包产物必须在目标发行版上实际安装并启动验证后才能标记为可用。
