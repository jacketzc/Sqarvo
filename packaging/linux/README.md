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
VERSION=0.2.3.3~nightly.20261118020000.abc1234 OUT_DIR=/tmp/out packaging/linux/build-deb.sh
```

版本单一来源是仓库根 `VERSION` 文件（当前 `0.2.3.3` = 下一待发版的基线号；
`packaging.yml` 的 push/tag 构建与本地直接调用都取它），env `VERSION` 覆盖仅用于
Nightly 注入完整版本号；脚本内不再硬编码任何版本号（原默认 `0.2.2` 是过时发布名，
已移除）。

产物 `dist/sqarvo_<VERSION>_<ARCH>.deb`（`dist/` 已在 `.gitignore` 里）。
依赖由 `dpkg-shlibdeps` 从二进制实际链接的库算出，再把 GTK / libadwaita 的下限
抬到编译期真实要求（GTK ≥ 4.16、libadwaita ≥ 1.6，见 `frontend/linux/README.md`）。

安装：

```bash
sudo apt install ./dist/sqarvo_0.2.3.3_amd64.deb   # 会一并装依赖
```

不装系统级也可以只解包到用户目录试跑（布局相同，前端会在自己所在目录找到后端）：

```bash
dpkg-deb -x dist/sqarvo_0.2.3.3_amd64.deb /tmp/sqarvo-pkg
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

## Nightly（`.github/workflows/nightly.yml`）

每天 10:00 北京时间（UTC 02:00，cron `0 2 * * *`）从 `main` 最新提交构建
Linux amd64 `.deb`，通过全部验证后发布/刷新到固定的 **pre-release**
（tag `nightly`，名称 `Sqarvo Nightly`）；也支持 `workflow_dispatch` 手动触发
（带 `force_republish` 布尔输入）。工作流只监听 `schedule` 与 `workflow_dispatch`，
不挂 push/PR/tags；`ci.yml`、`packaging.yml` 与正式发版互不影响。整个工作流一个
concurrency 组串行执行（不取消在跑的构建），发布环节不会并发竞态。

构建与校验步骤与上文 `packaging.yml` 的 `deb` 作业一致（GitHub Actions 没有跨
workflow 复用步骤的机制，两处改动要同步）：同用 `packaging/linux/build-deb.sh`
（`BUILD=0` + env `VERSION` 注入），不重复实现编译/打包；在此之上多两步——

1. **版本计算**：`<VERSION 文件基线>~nightly.<main 头提交的 UTC 时间 YYYYMMDDHHMMSS>.<短SHA 7 位>`，
   例如 `0.2.3.3~nightly.20261118020000.abcdef0`。用「提交时间 + 提交号」而不是构建时间：
   同一 commit 重复构建得到同一版本（幂等，发布守卫可判）、同日多个 commit 按提交时间升序、
   跨天按日期升序、同刻并列的 commit 靠短 SHA 后缀保持互不相等。Debian 语义下 `~` 排在
   一切之前，所以 Nightly 严格低于对应正式版 `0.2.3.3`、又高于已发布的 `0.2.3.2`
   （stable 用户可直接升级到 Nightly）。发布前用 `dpkg --compare-versions` 把这五类
   排序断言逐条跑一遍，失败即不发。
2. **SHA-256**：`.deb` 与 `.sha256` 一起生成、一起发布。GitHub 会把 Release 资产文件名
   里的 `~` 转义成 `.`（包内 `Version` 字段不受影响），因此校验文件按**转义后的资产名**
   生成，用户下载后 `sha256sum -c` 可直接通过；发布前工作流先比对 hash 值。

发布策略（只用内置 `GITHUB_TOKEN` + 预装的 `gh`，无第三方动作；`deb` 作业只读，
写权限仅 `publish` 作业持有且只在 `main` 上运行）：

- 固定入口 `releases/tag/nightly` 永远指向最新通过验证的构建。
- GitHub 的 Release 不允许改写 `target_commitish`，"tag、源码 commit、.deb 来源
  三者一致"只能靠**删旧建新**实现：删除旧 Release/tag（删除失败则旧状态原样保留），
  用本次 commit 创建新 Release 并上传产物，创建失败重试一次。
- 两道守卫：`nightly` tag 已指向本次 commit → 幂等跳过（不重复发布相同 commit）；
  已发布 Nightly 的提交时间不比本次旧 → 跳过并保留旧 Nightly（旧构建/重跑的 run
  绝不覆盖新 Nightly）。`force_republish` 可越过两道守卫。
- 发布后**回读断言**：Release 存在且为 pre-release、资产恰好是 `.deb` + `.sha256`
  两个、tag `nightly` 指向本次构建的 commit；任一不符作业失败。
- 失败恢复：本 run 的 `sqarvo-nightly-deb` artifact（14 天保留）就是完整产物。若
  "删旧成功、建新失败"（日志有明确 `::error::` 提示），下载 artifact 后手工恢复，
  或直接对该 commit 重跑一次 workflow_dispatch：

  ```bash
  gh release create nightly sqarvo_<version>_amd64.deb sqarvo_<version>_amd64.deb.sha256 \
    --repo jacketzc/Sqarvo --target <commit-sha> --title "Sqarvo Nightly" \
    --prerelease --notes-file notes.md
  ```

Release 说明由工作流自动生成：构建时间（UTC）、commit 链接、Debian 包版本、
支持平台、SHA-256、安装命令、实验性提示与 Issue 入口；不做自动 changelog。

本地自检（2026-11-18）：`nightly.yml` PyYAML 6.0.3 解析通过，触发键/作业/权限结构如预期，
全部 `run:` 块 `bash -n` 通过；版本公式与「五类排序断言」在本机 `dpkg --compare-versions`
全部通过；`VERSION=0.2.3.3~nightly.20261118094549.eb5c4e1 BUILD=0 packaging/linux/build-deb.sh`
实际产出 `.deb`（元数据 `Package/Version/Architecture/Depends` 正确），`dpkg-deb -x` 解包后用
包内 `sqarvo-backend` 跑通与 CI 同款的启动冒烟（端点 0600 / 套接字 0600 / SIGTERM 后清理），
`sha256sum -c` 匹配；发布步骤的 `notes.md` 模板经离线渲染核对。**GitHub 发布链
（gh api、删旧建新、回读断言）尚未经真实 run 验证**，见「已知限制」。

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
- 包版本取仓库根 `VERSION` 文件（下一待发版基线，当前 0.2.3.3），而各 crate 的版本仍是
  0.1.0——界面上「后端版本 0.1.0」来自 crate 版本，两者不一致是仓库现状，不是打包错误；
- **`nightly.yml` 还没有在 GitHub 上真正跑过**：首次真实验证需要在合入 `main` 后用
  `workflow_dispatch` 手动触发一次（发布环节还依赖 `GITHUB_TOKEN` 对 Releases 的写权限，
  见仓库 Settings → Actions）；在那之前只有上面的本地自检作为依据，不能算「Nightly 已通过」。
  `packaging.yml` 本身已在 `main` 的每次 push 上实际运行（guard 对纯文档提交跳过打包）；
- Nightly 发布采用「删旧 Release → 建新 Release」：新旧交替之间存在数秒的入口空窗，且
  若删除成功而创建失败（已重试一次仍失败），固定入口会临时缺失——产物完整保留在当次
  run 的 Artifact 里，可按上文「失败恢复」手工重建；不存在「发布了一半」的 Release，
  回读断言不齐即作业失败；
- CI 覆盖范围只有 Linux / Ubuntu 26.04 的 .deb；没有 GUI 冒烟（原因见「CI」节），
  也没做从旧版本升级（`dpkg -i` 覆盖安装）与卸载后残留检查；
- 后端退出时删除 socket 与端点文件，但保留运行时目录本身（`$XDG_RUNTIME_DIR/sqarvo/`
  为空目录 0700）——实测行为，不是打包问题。

打包产物必须在目标发行版上实际安装并启动验证后才能标记为可用。
