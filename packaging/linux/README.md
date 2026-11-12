# Linux 打包

## 当前内容

- `io.sqarvo.Sqarvo.desktop`：桌面入口文件，`Exec` 指向 `sqarvo-desktop`
  （由 `frontend/linux` 构建的二进制重命名安装）。
- 图标源文件在 [`docs/assets/brand/sqarvo-logo.svg`](../../docs/assets/brand/sqarvo-logo.svg)，打包时装成 `io.sqarvo.Sqarvo.svg`（与 `.desktop` 的 `Icon=` 对应）。项目自绘，非第三方产品图标。
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

## CI（`.github/workflows/packaging.yml`）

作业名 **`打包（Linux .deb）` / `Linux .deb 打包与安装校验`**（作业 id `deb`），
`runs-on: ubuntu-26.04`（与 `ci.yml` 的前端作业同因：GTK ≥ 4.16、libadwaita ≥ 1.6）。

触发条件：

| 事件 | 条件 |
|---|---|
| `push` | 分支 `main`，或标签 `v*` |
| `workflow_dispatch` | 手动触发 |

**不在 `pull_request` 上跑**：release 构建两个 workspace 各一次，约 5 分钟；PR 阶段的编译
与测试已由 `ci.yml` 的 `后端（Linux）` / `前端（Linux）` 覆盖，这个作业要验证的是
「打包脚本 + 安装布局 + 装完能跑」。改动打包脚本的 PR 请用 `workflow_dispatch` 手动跑一次。

作业步骤（按执行顺序）：

1. `actions/checkout@v4`；
2. 安装依赖 `libgtk-4-dev libadwaita-1-dev build-essential pkg-config dpkg-dev`
   （`dpkg-dev` 提供脚本算 `Depends` 用的 `dpkg-shlibdeps`）；
3. 打印并校验 GTK / libadwaita 版本（低于 4.16 / 1.6 直接失败，而不是留一句 pkg-config 报错）；
4. `dtolnay/rust-toolchain@stable`；
5. `Swatinem/rust-cache@v2`，`workspaces: backend` 与 `frontend/linux` 两行，两个 workspace 各缓存一份；
6. `cargo build --release -p sqarvo-backend-service`（`backend/`）、`cargo build --release`（`frontend/linux/`）；
7. `BUILD=0 packaging/linux/build-deb.sh`；
8. 打印 `ls -l dist/`、`dpkg-deb -I`、`dpkg-deb -c`；
9. `sudo apt-get install -y ./dist/sqarvo_*_*.deb`，然后断言 `dpkg -s sqarvo` 为
   `Status: install ok installed`、`/usr/bin/sqarvo-{desktop,backend}` 存在且可执行、
   桌面入口文件的 `Exec=` 指向 `sqarvo-desktop`；
10. **启动冒烟（不依赖显示）**：在临时 `XDG_RUNTIME_DIR` 里跑 `/usr/bin/sqarvo-backend`，
    断言 10 秒内写出 `sqarvo/backend-endpoint.json`（权限 0600、`sqarvo/` 目录 0700）、
    创建了 `sqarvo/backend.sock`，`SIGTERM` 后 5 秒内退出且端点文件与套接字都被删除；
11. `actions/upload-artifact@v4` 上传 `dist/*.deb`（artifact 名 `sqarvo-deb-<ref>`，保留 14 天；
    找不到文件即失败）。

**CI 里没有 GUI 冒烟**：runner 没有 Wayland/X 会话，只能靠 `xvfb-run`，而验证机上没有
`xvfb-run`，无法预验这条路径是否稳定；前端是单实例 `GtkApplication` + 子进程后端，
在无会话总线的环境里失败原因很容易被误判成「打包坏了」。GUI 启动验证目前只在真机上做
（见下节），等 CI 里能稳定跑通再搬进来。第 10 步的后端冒烟是不依赖显示的替代证据。

## 本地验证记录（2026-11-06，Ubuntu 26.04）

环境：Ubuntu 26.04（resolute）、GTK 4.22.4、libadwaita 1.9.1、cargo 1.99.1、16 核 / 24 GiB，
基线 `main` fee2c34。

### 构建与包内容

```bash
CARGO_BUILD_JOBS=8 BUILD=1 packaging/linux/build-deb.sh   # 退出码 0
```

- 产物：`dist/sqarvo_0.2.2_amd64.deb`，6,212,836 字节（约 5.9 MiB），
  sha256 `3cc9cb410762b8134fea95f2b837bd4faa863f87e63781f2bf4463eb61cee64d`；
- `dpkg-deb -I` 关键字段：`Package: sqarvo`、`Version: 0.2.2`、`Architecture: amd64`、
  `Installed-Size: 25188`、
  `Depends: libadwaita-1-0 (>= 1.6), libc6 (>= 2.39), libgcc-s1 (>= 4.2), libglib2.0-0t64 (>= 2.54.0), libgraphene-1.0-0 (>= 1.5.4), libgtk-4-1 (>= 4.16)`、
  `Recommends: gnome-keyring`；
- `dpkg-deb -c`：`/usr/bin/sqarvo-backend`（19,359,728 字节）、`/usr/bin/sqarvo-desktop`
  （6,367,248 字节）均为 `root/root 0755`，另有 `.desktop`、SVG 图标与
  `usr/share/doc/sqarvo/{copyright,changelog.Debian.gz}`。

### 干净前缀安装 + 启动（不装进 `/usr`）

```bash
PREFIX=/tmp/pkg104-root
dpkg-deb -x dist/sqarvo_0.2.2_amd64.deb "$PREFIX"
flock -w 900 /tmp/sqarvo-agent-gui.lock -c '
  env -i \
    PATH=/tmp/pkg104-root/usr/bin:/usr/bin:/bin \
    HOME=/tmp/pkg104-home \
    DISPLAY=:0 WAYLAND_DISPLAY=wayland-0 XDG_RUNTIME_DIR=/run/user/1000 \
    XDG_CONFIG_HOME=/tmp/pkg104-cfg \
    SQARVO_ENDPOINT_FILE=/tmp/pkg104/sqarvo/backend-endpoint.json \
    /tmp/pkg104-root/usr/bin/sqarvo-desktop \
      --screenshot /tmp/pkg104/connected.png --screenshot-delay-ms 4000 \
      --qa-exit-after-ms 6000'
```

`env -i` 只保留这些变量，没有保留 `DBUS_SESSION_BUS_ADDRESS`——于是系统凭据库不可用，
日志里出现 `凭据操作将如实报错：无法与系统凭据服务通信 [credential.unavailable]`；
握手与界面不受影响，这正好顺带验证了「凭据库不可用时不阻塞启动」的降级路径。

三类证据：

- **日志**：`[sqarvo-desktop] 后端已就绪：transport=Unix pid=… protocol=v1 service=0.1.0`、
  `[sqarvo-desktop] 握手完成：后端版本 0.1.0，协议 v1，构建 release`、
  `[bench] t=+438ms state=Connected`、`[qa] 界面截图已写入 /tmp/pkg104/connected.png`；
  截图（1100×720 PNG）显示「已连接」页：后端版本 0.1.0、协议 v1（release 构建）、
  传输方式 Unix 域套接字 `/tmp/pkg104/sqarvo/backend.sock`、已注册驱动 MariaDB / SQLite；
- **权限**：运行时目录 `drwx------`（0700），`backend-endpoint.json` `-rw-------`
  （`mode=600 owner=jack:jack`），`backend.sock` `srw-------`（`mode=600`）；
- **清理**：前端退出码 0；退出后没有引用 `/tmp/pkg104-root` 的进程残留（`pgrep -af
  'sqarvo-(desktop|backend)'` 也无匹配）；`backend.sock` 与 `backend-endpoint.json` 都被删除，
  只留下空的 0700 目录 `/tmp/pkg104/sqarvo`。

### 自检

- `bash -n packaging/linux/build-deb.sh`：通过；
- `.github/workflows/packaging.yml`：用 PyYAML 6.0.3（`python3 -m venv` + `pip install pyyaml`，
  系统 Python 没有 `yaml` 模块）解析通过，触发键/作业/步骤结构如预期；每个 `run:` 块
  `bash -n` 通过；第 10 步的冒烟脚本用**包内的** `sqarvo-backend` 实际执行过一次，
  输出「运行时目录 700 / 端点文件 600 / 套接字 600 / token 打码 / 后端已退出并清理」。
- `actionlint` 未安装，**没有**运行。

## 已知限制

- 只做过 Ubuntu 26.04（GTK 4.22.4 / libadwaita 1.9.1，release 构建）的实测；
  24.04 的 GTK 4.14 低于 crate 要求，`Depends` 会拦住；
- 图标是占位资产；
- 没有 Flatpak / AppImage 清单；
- 后端由前端以子进程方式拉起与回收（见 `frontend/linux/README.md`「启动顺序」
  与「退出清理」），包内不含 systemd 单元或 D-Bus 服务；
- 包版本取发布名（0.2.2），而各 crate 的版本仍是 0.1.0——界面上「后端版本 0.1.0」
  来自 crate 版本，两者不一致是仓库现状，不是打包错误；
- **`packaging.yml` 还没有在 GitHub 上真正跑过**：工作流首次生效需要 push 到 `main`
  或推 `v*` 标签之后；在那之前只有上节的本地自检（YAML 解析、`bash -n`、冒烟脚本实跑）
  作为依据，不能算「CI 已通过」；
- CI 覆盖范围只有 Linux / Ubuntu 26.04 的 .deb；没有 GUI 冒烟（原因见「CI」节），
  也没做从旧版本升级（`dpkg -i` 覆盖安装）与卸载后残留检查；
- 后端退出时删除 socket 与端点文件，但保留运行时目录本身（`$XDG_RUNTIME_DIR/sqarvo/`
  为空目录 0700）——实测行为，不是打包问题。

打包产物必须在目标发行版上实际安装并启动验证后才能标记为可用。
