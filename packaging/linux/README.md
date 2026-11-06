# Linux 打包

## 当前内容

- `io.sqarvo.Sqarvo.desktop`：桌面入口文件，`Exec` 指向 `sqarvo-desktop`
  （由 `frontend/linux` 构建的二进制重命名安装）。
- `io.sqarvo.Sqarvo.svg`：**占位**图标（项目自绘的数据库圆柱，非第三方产品图标）。
  正式的品牌设计资产尚未提供，替换它即可，不需要改脚本。
- `build-deb.sh`：一条命令产出 Debian / Ubuntu 安装包。

## 构建 .deb

```bash
packaging/linux/build-deb.sh            # 先 cargo build --release，再打包
BUILD=0 packaging/linux/build-deb.sh    # 复用已有 release 二进制
VERSION=0.2.3 OUT_DIR=/tmp/out packaging/linux/build-deb.sh
```

产物 `dist/sqarvo_<VERSION>_<ARCH>.deb`（`dist/` 已在 `.gitignore` 里）。
依赖由 `dpkg-shlibdeps` 从二进制实际链接的库算出，再把 GTK / libadwaita 的下限
抬到编译期真实要求（GTK ≥ 4.16、libadwaita ≥ 1.6，见 `frontend/linux/README.md`）。

安装：

```bash
sudo apt install ./dist/sqarvo_0.2.2_amd64.deb   # 会一并装依赖
```

不装系统级也可以只解包到用户目录试跑（布局相同，前端会在自己所在目录找到后端）：

```bash
dpkg-deb -x dist/sqarvo_0.2.2_amd64.deb /tmp/sqarvo-pkg
install -m755 /tmp/sqarvo-pkg/usr/bin/sqarvo-* ~/.local/bin/
```

## 安装布局

```text
/usr/bin/sqarvo-desktop
/usr/bin/sqarvo-backend
/usr/share/applications/io.sqarvo.Sqarvo.desktop
/usr/share/icons/hicolor/scalable/apps/io.sqarvo.Sqarvo.svg
/usr/share/doc/sqarvo/{copyright,changelog.Debian.gz}
```

## 已知限制

- 只做过 Ubuntu 26.04（GTK 4.22.4 / libadwaita 1.9.1，release 构建）的实测；
  24.04 的 GTK 4.14 低于 crate 要求，`Depends` 会拦住；
- 图标是占位资产；
- 没有 Flatpak / AppImage 清单；
- 后端由前端以子进程方式拉起与回收（见 `frontend/linux/README.md`「启动顺序」
  与「退出清理」），包内不含 systemd 单元或 D-Bus 服务；
- 包版本取发布名（0.2.2），而各 crate 的版本仍是 0.1.0——界面上「后端版本 0.1.0」
  来自 crate 版本，两者不一致是仓库现状，不是打包错误。

打包产物必须在目标发行版上实际安装并启动验证后才能标记为可用。
