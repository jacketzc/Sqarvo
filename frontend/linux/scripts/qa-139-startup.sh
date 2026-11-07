#!/usr/bin/env bash
# Issue #139 验收：启动握手成功后直接落地工作区，后端诊断信息在「关于 Squarvo」。
#
# 跑三组无人值守取证（截图全部由 GTK 自渲染，不经过桌面截屏接口）：
#   1. 有已保存连接：主区域是 Objects 页（空态「从左侧选择一个连接开始」），
#      主界面不含「后端版本 / 端点文件」；
#   2. 打开「关于 Squarvo」：显示后端版本与驱动清单；
#   3. 空 connections.json：同样落到同一个空态，不崩溃、不退回诊断页。
#
# 用法：
#   ./scripts/qa-139-startup.sh
#   ./scripts/qa-139-startup.sh --no-build
#
# 环境变量：
#   SQARVO_QA139_DIR      输出目录（默认 /home/jack/sqarvo-v023-shots/139-startup-workspace）
#   SQARVO_QA139_NO_BUILD 设为 1 跳过 cargo build
#
# 断言失败退出码非 0；截图与 run.log 保留在输出目录（仓库外，不提交）。

set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)
FRONTEND_DIR="$REPO_ROOT/frontend/linux"
OUT=${SQARVO_QA139_DIR:-/home/jack/sqarvo-v023-shots/139-startup-workspace}
BIN="$FRONTEND_DIR/target/debug/sqarvo-desktop"
BACKEND="$REPO_ROOT/backend/target/debug/sqarvo-backend"

PASS=0
FAIL=0
say() { printf '%s\n' "$*"; }
ok()  { PASS=$((PASS + 1)); say "[assert] OK   $*"; }
bad() { FAIL=$((FAIL + 1)); say "[assert] FAIL $*"; }

if [ "${SQARVO_QA139_NO_BUILD:-0}" != "1" ]; then
    say "构建前端与后端（SQARVO_QA139_NO_BUILD=1 可跳过）…"
    ( cd "$REPO_ROOT/backend" && cargo build -q -p sqarvo-backend-service ) || {
        say "后端构建失败"; exit 1;
    }
    ( cd "$FRONTEND_DIR" && cargo build -q ) || { say "前端构建失败"; exit 1; }
fi
[ -x "$BIN" ] || { say "找不到前端二进制：$BIN"; exit 1; }
[ -x "$BACKEND" ] || { say "找不到后端二进制：$BACKEND"; exit 1; }
if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ]; then
    say "没有图形会话（WAYLAND_DISPLAY / DISPLAY 均为空）：GTK 自渲染截图需要真实窗口"
    exit 1
fi

TMP=$(mktemp -d "${TMPDIR:-/tmp}/sqarvo-qa139.XXXXXX")
mkdir -p "$OUT" "$TMP/config/sqarvo/Sqarvo" "$TMP/run"
export XDG_CONFIG_HOME="$TMP/config"
say "验收目录：$OUT"
say "临时配置目录：$XDG_CONFIG_HOME"

# 只清理本轮新出现的后端进程，避免误杀用户自己的实例。
backend_pids() { pgrep -f "$BACKEND" 2>/dev/null | sort -u || true; }
BACKEND_BEFORE="$TMP/backend-before.txt"
backend_pids > "$BACKEND_BEFORE"
cleanup() {
    local rc=$?
    local pid
    for pid in $(backend_pids); do
        grep -qx "$pid" "$BACKEND_BEFORE" || kill "$pid" 2>/dev/null || true
    done
    exit "$rc"
}
trap cleanup EXIT INT TERM

write_config() {
    cat > "$TMP/config/sqarvo/Sqarvo/connections.json" <<'EOF'
{
  "version": 1,
  "connections": [
    {
      "name": "本地 SQLite",
      "driver": "sqlite",
      "host": "",
      "port": 0,
      "database": "",
      "username": "",
      "file_path": "/tmp/sqarvo-qa139-demo.sqlite",
      "read_only": false,
      "tls_mode": "disabled",
      "credential_ref": ""
    },
    {
      "name": "测试 MariaDB",
      "driver": "mariadb",
      "host": "127.0.0.1",
      "port": 53306,
      "database": "sqarvo_test",
      "username": "sqarvo_test",
      "file_path": "",
      "read_only": false,
      "tls_mode": "preferred",
      "credential_ref": "sqarvo:conn/00000000-0000-4000-8000-000000000139"
    }
  ]
}
EOF
}

run_app() {
    local name=$1
    local log="$OUT/$name.log"
    local run_root="$TMP/run/$name"
    mkdir -p "$run_root/sqarvo"
    shift
    say ""
    say "== $name =="
    env XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
        SQARVO_ENDPOINT_FILE="$run_root/sqarvo/backend-endpoint.json" \
        SQARVO_BACKEND_BIN="$BACKEND" \
        timeout 90 "$BIN" --window-size 1200x800 --screenshot-delay-ms 5000 "$@" \
        > "$log" 2>&1
    local code=$?
    if [ "$code" -eq 0 ]; then
        ok "$name 进程退出码 0"
    else
        bad "$name 进程退出码 $code（日志：$log）"
    fi
    if grep -qF '[qa] 断言通过：' "$log"; then
        ok "$name 应用内断言通过（$(grep -cF '[qa] 断言通过：' "$log") 条）"
    else
        bad "$name 没有应用内断言（日志：$log）"
    fi
    say "       日志：$log"
}

write_config

# ---- 1. 有已保存连接：启动态工作区 ----------------------------------------
run_app startup-workspace \
    --screenshot "$OUT/01-startup-workspace.png" \
    --qa-startup-workspace
if grep -qF '断言通过：启动态=Objects 空态（已保存连接 2 条' "$OUT/startup-workspace.log"; then
    ok "启动态：Objects 页 + 2 条已保存连接"
else
    bad "启动态断言未命中（见 startup-workspace.log）"
fi

# ---- 2. 「关于 Squarvo」：后端版本 + 驱动清单 ------------------------------
run_app about \
    --screenshot "$OUT/02-about.png" \
    --qa-open-about \
    --qa-expect-about '已注册驱动' \
    --qa-expect-about 'MariaDB' \
    --qa-expect-about 'SQLite'
if grep -qF '[qa] 截图目标：关于 Squarvo' "$OUT/about.log"; then
    ok "截图目标是「关于 Squarvo」对话框"
else
    bad "没有打开「关于 Squarvo」（见 about.log）"
fi

# ---- 3. 空 connections.json：同一个空态 -----------------------------------
rm -f "$TMP/config/sqarvo/Sqarvo/connections.json"
run_app startup-empty \
    --screenshot "$OUT/03-startup-empty.png" \
    --qa-startup-workspace
if grep -qF '断言通过：启动态=Objects 空态（已保存连接 0 条' "$OUT/startup-empty.log"; then
    ok "空配置：仍落到 Objects 空态（0 条连接）"
else
    bad "空配置断言未命中（见 startup-empty.log）"
fi

# ---- 截图文件 --------------------------------------------------------------
for png in 01-startup-workspace 02-about 03-startup-empty; do
    path="$OUT/$png.png"
    size=0
    [ -f "$path" ] && size=$(wc -c < "$path")
    if [ "$size" -gt 1000 ]; then
        ok "截图 $png（$size 字节）：$path"
    else
        bad "截图 $png 缺失或过小（$size 字节）：$path"
    fi
done

say ""
say "结果：$PASS 项通过，$FAIL 项失败"
[ "$FAIL" -eq 0 ]
