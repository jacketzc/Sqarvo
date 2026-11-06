#!/usr/bin/env bash
# 构建 Linux .deb 安装包（Debian / Ubuntu，GTK4 + libadwaita 前端 + 本地后端）。
#
# 用法：
#   packaging/linux/build-deb.sh              # 先 cargo build --release，再打包
#   BUILD=0 packaging/linux/build-deb.sh      # 复用已有 release 二进制，只打包
#   VERSION=0.2.3 OUT_DIR=/tmp/out packaging/linux/build-deb.sh
#
# 产物：dist/sqarvo_<VERSION>_<ARCH>.deb
#
# 安装布局（与 packaging/linux/README.md 一致）：
#   /usr/bin/sqarvo-desktop
#   /usr/bin/sqarvo-backend              （前端在同目录找到它，无需 SQARVO_BACKEND_BIN）
#   /usr/share/applications/io.sqarvo.Sqarvo.desktop
#   /usr/share/icons/hicolor/scalable/apps/io.sqarvo.Sqarvo.svg
set -euo pipefail

VERSION="${VERSION:-0.2.2}"
ARCH="${ARCH:-amd64}"
BUILD="${BUILD:-1}"

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../.." && pwd)
OUT_DIR="${OUT_DIR:-$REPO_ROOT/dist}"

FRONTEND_BIN="$REPO_ROOT/frontend/linux/target/release/sqarvo-desktop"
BACKEND_BIN="$REPO_ROOT/backend/target/release/sqarvo-backend"

step() { printf '\n== %s ==\n' "$*"; }
die() { printf 'build-deb: %s\n' "$*" >&2; exit 1; }

command -v dpkg-deb >/dev/null || die "缺少 dpkg-deb（apt install dpkg）"
command -v dpkg-shlibdeps >/dev/null || die "缺少 dpkg-shlibdeps（apt install dpkg-dev）"

if [ "$BUILD" = "1" ]; then
  step "构建后端（release）"
  ( cd "$REPO_ROOT/backend" && cargo build --release -p sqarvo-backend-service )
  step "构建前端（release）"
  ( cd "$REPO_ROOT/frontend/linux" && cargo build --release )
else
  step "跳过构建（BUILD=0）"
fi

[ -x "$FRONTEND_BIN" ] || die "找不到前端二进制：$FRONTEND_BIN"
[ -x "$BACKEND_BIN" ] || die "找不到后端二进制：$BACKEND_BIN"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/sqarvo-deb.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
PKG="$WORK/pkg"
mkdir -p "$PKG/DEBIAN" \
         "$PKG/usr/bin" \
         "$PKG/usr/share/applications" \
         "$PKG/usr/share/icons/hicolor/scalable/apps" \
         "$PKG/usr/share/doc/sqarvo"

step "安装文件到暂存目录"
install -Dm755 "$FRONTEND_BIN" "$PKG/usr/bin/sqarvo-desktop"
install -Dm755 "$BACKEND_BIN"  "$PKG/usr/bin/sqarvo-backend"
install -Dm644 "$SCRIPT_DIR/io.sqarvo.Sqarvo.desktop" \
  "$PKG/usr/share/applications/io.sqarvo.Sqarvo.desktop"
install -Dm644 "$SCRIPT_DIR/io.sqarvo.Sqarvo.svg" \
  "$PKG/usr/share/icons/hicolor/scalable/apps/io.sqarvo.Sqarvo.svg"

cat > "$PKG/usr/share/doc/sqarvo/copyright" <<'EOF'
Sqarvo — 开源、轻量的原生数据库客户端
Copyright (c) Sqarvo contributors

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

On Debian systems the full text of the Apache-2.0 license is available at
/usr/share/common-licenses/Apache-2.0.
EOF
chmod 644 "$PKG/usr/share/doc/sqarvo/copyright"

{
  printf 'sqarvo (%s) unstable; urgency=low\n\n' "$VERSION"
  printf '  * 本地构建的试用包（main %s）。\n' "$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
  printf '\n -- Sqarvo contributors <noreply@sqarvo.invalid>  %s\n' "$(LC_ALL=C date -R)"
} > "$WORK/changelog"
gzip -9n -c "$WORK/changelog" > "$PKG/usr/share/doc/sqarvo/changelog.Debian.gz"
chmod 644 "$PKG/usr/share/doc/sqarvo/changelog.Debian.gz"

step "计算运行时依赖（dpkg-shlibdeps）"
mkdir -p "$WORK/shlibs/debian"
printf 'Source: sqarvo\n\nPackage: sqarvo\nArchitecture: any\n' > "$WORK/shlibs/debian/control"
SHLIBS=$( cd "$WORK/shlibs" && dpkg-shlibdeps -O \
  "$PKG/usr/bin/sqarvo-desktop" "$PKG/usr/bin/sqarvo-backend" 2>/dev/null \
  | sed -n 's/^shlibs:Depends=//p' )

[ -n "$SHLIBS" ] || die "dpkg-shlibdeps 未给出依赖"

# shlibs 只声明库 ABI 的最低版本；这里按实际编译期要求抬到 GTK 4.16 / libadwaita 1.6
# （见 frontend/linux/README.md「依赖」），其余保持 shlibs 的结果。
DEPS=$(printf '%s' "$SHLIBS" \
  | sed -E 's/libgtk-4-1 \(>= [^)]*\)/libgtk-4-1 (>= 4.16)/; s/libadwaita-1-0 \(>= [^)]*\)/libadwaita-1-0 (>= 1.6)/')
case "$DEPS" in
  *"libgtk-4-1 (>= 4.16)"*) ;;
  *) DEPS="$DEPS, libgtk-4-1 (>= 4.16)" ;;
esac
case "$DEPS" in
  *"libadwaita-1-0 (>= 1.6)"*) ;;
  *) DEPS="$DEPS, libadwaita-1-0 (>= 1.6)" ;;
esac

INSTALLED_SIZE=$(du -sk --exclude=DEBIAN "$PKG" | cut -f1)

step "写 control"
cat > "$PKG/DEBIAN/control" <<EOF
Package: sqarvo
Version: $VERSION
Section: database
Priority: optional
Architecture: $ARCH
Maintainer: Sqarvo contributors <noreply@sqarvo.invalid>
Installed-Size: $INSTALLED_SIZE
Depends: $DEPS
Recommends: gnome-keyring
Description: 轻量原生数据库客户端（GTK4 + libadwaita）
 Sqarvo is an open-source, lightweight, native desktop database client.
 The Linux frontend is written with GTK4 + libadwaita and talks to a local
 Rust backend over a Unix domain socket (gRPC).
 .
 本包同时安装前端与后端；当前版本提供 SQLite 与 MariaDB 驱动、SQL 工作区、
 对象树、DDL 查看与表数据浏览/编辑。Windows 与 macOS 前端尚未提供。
EOF

step "打包"
mkdir -p "$OUT_DIR"
DEB="$OUT_DIR/sqarvo_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group -Zxz --build "$PKG" "$DEB" >/dev/null

printf '\n产物：%s\n' "$DEB"
dpkg-deb -f "$DEB" Package Version Architecture Depends
