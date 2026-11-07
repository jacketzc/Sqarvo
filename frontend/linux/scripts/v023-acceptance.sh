#!/usr/bin/env bash
# Sqarvo v0.2.3 验收链（#121）：一条命令跑完「多连接 → 展开树 → 开 3 个表标签 →
# 排序 → 筛选 → 翻页 → 信息面板 DDL/表信息 → 导出当前页 CSV → 关标签」。
#
# 形态：本脚本负责建库、隔离配置、按顺序跑 SQLite / MariaDB 两个用例，并把每一步的
# 断言结果与截图路径打到 stdout。界面步骤全部写在 app 内的 `--qa-tabs-file` 脚本里，
# 走的是用户会走的同一条路径（打开表、点列头、点分页器、点面板切换、点导出）。
#
# 关键约束：
#   * `XDG_CONFIG_HOME` 指向临时目录：验收产生的连接配置绝不写进用户真实配置；
#     结束时断言真实 `~/.config/sqarvo/Sqarvo/connections.json` 条目数与开始时一致。
#   * 截图与日志落在仓库外（默认 `/home/jack/sqarvo-v023-shots/acceptance-<时间戳>/`），
#     不提交进仓库。
#   * 任何一步失败都以非零退出，末尾打印「N 项断言全过」。
#
# 前置条件（脚本会检查并给出可读原因）：
#   * 已构建的前端/后端二进制（默认执行 cargo build；`SQARVO_ACCEPTANCE_NO_BUILD=1` 跳过）；
#   * 可用的图形会话（WAYLAND_DISPLAY 或 DISPLAY）；
#   * MariaDB 测试库 sqarvo_test_121（可用 `SQARVO_TEST_HOST/PORT/DB/USER` 覆盖）；
#   * 密码从环境变量 `SQARVO_QA_PASSWORD` 读取（**不写进脚本**）；
#   * Python 3 且可 `import pymysql`（用于建测试数据；也可 `PYTHON=/path/to/python3` 指定）。
#
# 用法：
#   SQARVO_QA_PASSWORD=... ./scripts/v023-acceptance.sh
#   SQARVO_ACCEPTANCE_DIR=/tmp/shots SQARVO_ACCEPTANCE_NO_BUILD=1 ./scripts/v023-acceptance.sh

set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)
FRONTEND_DIR="$REPO_ROOT/frontend/linux"

OUT_BASE=${SQARVO_ACCEPTANCE_DIR:-/home/jack/sqarvo-v023-shots}
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="$OUT_BASE/acceptance-$STAMP"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/sqarvo-acceptance.XXXXXX")
export XDG_CONFIG_HOME="$TMP/config"
mkdir -p "$XDG_CONFIG_HOME" "$OUT"

TAB_ROWS=1200
TAB_KEEP=1100

MYSQL_HOST=${SQARVO_TEST_HOST:-127.0.0.1}
MYSQL_PORT=${SQARVO_TEST_PORT:-53306}
MYSQL_DB=${SQARVO_TEST_DB:-sqarvo_test_121}
MYSQL_USER=${SQARVO_TEST_USER:-sqarvo_test}

REAL_CONFIG="$HOME/.config/sqarvo/Sqarvo/connections.json"
PASS_COUNT=0
FAIL_COUNT=0
IN_APP_TOTAL=0

say() { printf '%s\n' "$*"; }
ok()  { PASS_COUNT=$((PASS_COUNT + 1)); say "[assert] OK   $*"; }
bad() { FAIL_COUNT=$((FAIL_COUNT + 1)); say "[assert] FAIL $*"; }

check_eq() {
    local name=$1 expected=$2 actual=$3
    if [ "$expected" = "$actual" ]; then
        ok "$name（期望 $expected，实际 $actual）"
    else
        bad "$name（期望 $expected，实际 $actual）"
    fi
}

check_contains() {
    local name=$1 needle=$2 file=$3
    if grep -qF -- "$needle" "$file" 2>/dev/null; then
        ok "$name（命中「$needle」）"
    else
        bad "$name（未命中「$needle」：$file）"
    fi
}

check_count() {
    local name=$1 expected=$2 pattern=$3 file=$4 actual
    actual=$(grep -cF -- "$pattern" "$file" 2>/dev/null || true)
    check_eq "$name" "$expected" "$actual"
}

check_file_lines() {
    local name=$1 expected=$2 file=$3 actual=0
    [ -f "$file" ] && actual=$(wc -l < "$file")
    check_eq "$name" "$expected" "$actual"
}

config_count() {
    local file=$1
    if [ ! -f "$file" ]; then
        printf '0'
        return
    fi
    "$PYTHON" - "$file" <<'PY'
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        print(len(json.load(handle).get("connections", [])))
except Exception:
    print("?")
PY
}

pick_python() {
    local candidate
    for candidate in "${PYTHON:-}" python3 python3.12 python3.11 python3.10; do
        [ -n "$candidate" ] || continue
        command -v "$candidate" >/dev/null 2>&1 || continue
        if "$candidate" -c 'import sqlite3, pymysql' >/dev/null 2>&1; then
            printf '%s' "$candidate"
            return 0
        fi
    done
    # 本机验收环境的 pyenv 解释器（不写死具体版本）。
    for candidate in "$HOME"/.pyenv/versions/*/bin/python3; do
        [ -x "$candidate" ] || continue
        if "$candidate" -c 'import sqlite3, pymysql' >/dev/null 2>&1; then
            printf '%s' "$candidate"
            return 0
        fi
    done
    return 1
}

fail_early() {
    say "验收无法开始：$*"
    exit 1
}

# ---------------------------------------------------------------------------
# 0. 前置检查
# ---------------------------------------------------------------------------

if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ]; then
    fail_early "没有图形会话（WAYLAND_DISPLAY / DISPLAY 均为空）：截图与界面断言需要真实 GTK 窗口"
fi

if ! PYTHON=$(pick_python); then
    fail_early "找不到可 import pymysql 的 Python 3（用 PYTHON=/path/to/python3 指定；本机可用 ~/.pyenv/versions/3.11.*/bin/python3）"
fi

if [ -z "${SQARVO_QA_PASSWORD:-}" ]; then
    fail_early "缺少 SQARVO_QA_PASSWORD（MariaDB 测试账号密码；脚本不内置明文）"
fi

if [ "${SQARVO_ACCEPTANCE_NO_BUILD:-0}" != "1" ]; then
    say "构建后端与前端（SQARVO_ACCEPTANCE_NO_BUILD=1 可跳过）…"
    ( cd "$REPO_ROOT/backend" && cargo build -q -p sqarvo-backend-service ) || fail_early "后端构建失败"
    ( cd "$FRONTEND_DIR" && cargo build -q ) || fail_early "前端构建失败"
fi

BIN="$FRONTEND_DIR/target/debug/sqarvo-desktop"
BACKEND="$REPO_ROOT/backend/target/debug/sqarvo-backend"
[ -x "$BIN" ] || fail_early "找不到前端二进制：$BIN"
[ -x "$BACKEND" ] || fail_early "找不到后端二进制：$BACKEND"

say "验收目录：$OUT"
say "临时配置目录：$XDG_CONFIG_HOME"
say "Python：$PYTHON"

# ---------------------------------------------------------------------------
# 1. 配置隔离基线
# ---------------------------------------------------------------------------

REAL_BEFORE=$(config_count "$REAL_CONFIG")
say "真实连接配置开始时：$REAL_BEFORE 条（$REAL_CONFIG）"
if [ "$REAL_BEFORE" = "?" ]; then
    fail_early "无法读取真实连接配置：$REAL_CONFIG"
fi

# ---------------------------------------------------------------------------
# 2. 建测试数据（SQLite + MariaDB，幂等）
# ---------------------------------------------------------------------------

SQLITE_DB="$TMP/accept.db"
"$PYTHON" - "$SQLITE_DB" "$MYSQL_HOST" "$MYSQL_PORT" "$MYSQL_DB" "$MYSQL_USER" "$SQARVO_QA_PASSWORD" "$TAB_ROWS" "$TAB_KEEP" <<'PY'
import sqlite3
import sys

import pymysql

(
    sqlite_path,
    host,
    port,
    database,
    user,
    password,
    rows,
    keep,
) = sys.argv[1:]
rows = int(rows)
keep = int(keep)

items = [
    (i, f"item-{i:04d}", "keep" if i <= keep else "drop", i * 3 % 97)
    for i in range(1, rows + 1)
]
notes = [(1, "第一行", "正文"), (2, "第二行", None), (3, "第三行", "")]
events = [(i, "open", "2026-11-07 10:00:00") for i in range(1, 6)]

# ---- SQLite ----
conn = sqlite3.connect(sqlite_path)
conn.executescript(
    """
    DROP VIEW IF EXISTS accept_item_names;
    DROP TABLE IF EXISTS accept_events;
    DROP TABLE IF EXISTS accept_notes;
    DROP TABLE IF EXISTS accept_items;
    CREATE TABLE accept_items(
        id INTEGER PRIMARY KEY, name TEXT NOT NULL, category TEXT NOT NULL, amount INTEGER NOT NULL);
    CREATE TABLE accept_notes(id INTEGER PRIMARY KEY, title TEXT NOT NULL, body TEXT);
    CREATE TABLE accept_events(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, at TEXT);
    """
)
conn.executemany("INSERT INTO accept_items VALUES (?,?,?,?)", items)
conn.executemany("INSERT INTO accept_notes VALUES (?,?,?)", notes)
conn.executemany("INSERT INTO accept_events VALUES (?,?,?)", events)
conn.execute(
    "CREATE VIEW accept_item_names AS SELECT id, name FROM accept_items WHERE category='keep'"
)
conn.commit()
conn.close()

# ---- MariaDB ----
conn = pymysql.connect(
    host=host,
    port=int(port),
    user=user,
    password=password,
    database=database,
    charset="utf8mb4",
    autocommit=True,
    connect_timeout=10,
)
with conn.cursor() as cur:
    cur.execute("DROP VIEW IF EXISTS accept_item_names")
    for table in ("accept_events", "accept_notes", "accept_items"):
        cur.execute(f"DROP TABLE IF EXISTS {table}")
    cur.execute(
        "CREATE TABLE accept_items(id INT PRIMARY KEY, name VARCHAR(64) NOT NULL, "
        "category VARCHAR(16) NOT NULL, amount INT NOT NULL)"
    )
    cur.execute(
        "CREATE TABLE accept_notes(id INT PRIMARY KEY, title VARCHAR(64) NOT NULL, body TEXT NULL)"
    )
    cur.execute(
        "CREATE TABLE accept_events(id INT PRIMARY KEY, kind VARCHAR(16) NOT NULL, at DATETIME NULL)"
    )
    cur.executemany("INSERT INTO accept_items VALUES (%s,%s,%s,%s)", items)
    cur.executemany("INSERT INTO accept_notes VALUES (%s,%s,%s)", notes)
    cur.executemany("INSERT INTO accept_events VALUES (%s,%s,%s)", events)
    cur.execute(
        "CREATE VIEW accept_item_names AS SELECT id, name FROM accept_items "
        "WHERE category='keep'"
    )
conn.close()
PY
if [ $? -ne 0 ]; then
    fail_early "建测试数据失败（SQLite: $SQLITE_DB；MariaDB: $MYSQL_HOST:$MYSQL_PORT/$MYSQL_DB）"
fi
say "测试数据就绪：SQLite $SQLITE_DB；MariaDB $MYSQL_DB.accept_items（$TAB_ROWS 行，其中 keep $TAB_KEEP 行）"

# ---------------------------------------------------------------------------
# 3. 一个用例：跑 app、收集日志、核对关键证据
# ---------------------------------------------------------------------------

# 参数：用例名 驱动 profile 文件 密码环境变量名（空 = 不需要） catalog
run_case() {
    local case_name=$1 driver=$2 profile=$3 password_env=$4 catalog=$5
    local case_out="$OUT/$case_name"
    local run_root="$TMP/run-$case_name"
    local log="$case_out/run.log"
    local tabs="$case_out/tabs.txt"
    mkdir -p "$case_out" "$run_root/sqarvo"

    cat > "$tabs" <<EOF
# v0.2.3 验收链（$driver）：每一步都走用户路径
# --- 对象树：展开 catalog，ListNodes 次数按分组数计 ---
mark-list-nodes
expand $catalog
expect-list-nodes 2
# --- §10 Ctrl+F 的焦点目标 ---
focus-search
expect-focus search
screenshot $case_out/01-tree.png
# --- 信息面板：选中表 → DDL ---
select-node accept_items
inspector ddl
expect-inspector table_page=ddl
# --- 三个表标签：1 条浏览会话 + 3 条标签会话 ---
open-table accept_items
open-table accept_notes
open-table accept_events
expect-tabs 4
sessions 打开 3 个标签后
expect-sessions 4
screenshot $case_out/02-tabs.png
# --- 切回第一个表标签：排序 / 筛选 / 翻页都在同一个标签上做 ---
select 1
sort 1 name
expect-browse 1 sort=name:asc
filter 1 category eq keep
expect-browse 1 filters=1
expect-browse 1 total=$TAB_KEEP
page 1 next
expect-browse 1 page=2
expect-browse 1 rows=500
page 1 next
expect-browse 1 page=3
expect-browse 1 rows=100
screenshot $case_out/03-page3.png
# --- 信息面板切到「表信息」 ---
inspector info
expect-inspector table_page=info
expect-inspector columns=4
screenshot $case_out/04-info.png
# --- 导出当前页（第 3 页，100 行） ---
export 1 $case_out/page3.csv
expect-file $case_out/page3.csv 1000
screenshot $case_out/05-export.png
# --- 关一个标签：会话回到 3 ---
close 3 discard
expect-tabs 3
sessions 关闭 1 个后
expect-sessions 3
quit
EOF

    local -a env_args=(
        "XDG_CONFIG_HOME=$XDG_CONFIG_HOME"
        "SQARVO_ENDPOINT_FILE=$run_root/sqarvo/backend-endpoint.json"
        "SQARVO_BACKEND_BIN=$BACKEND"
    )
    local -a extra_args=()
    if [ -n "$password_env" ]; then
        env_args+=("$password_env=$SQARVO_QA_PASSWORD")
        extra_args+=(--qa-password-env "$password_env")
    fi
    say ""
    say "== 用例 $case_name（驱动 $driver，catalog $catalog） =="
    env "${env_args[@]}" timeout 150 "$BIN" \
        --window-size 1200x800 \
        --qa-profile "$profile" \
        "${extra_args[@]}" \
        --qa-tabs-file "$tabs" > "$log" 2>&1
    local code=$?
    check_eq "$case_name 进程退出码" 0 "$code"

    # 应用内断言：每一行 `[qa] 断言 OK：` 是一条真实控件/服务端计数断言。
    local in_app
    in_app=$(grep -cF '断言 OK：' "$log" 2>/dev/null || true)
    IN_APP_TOTAL=$((IN_APP_TOTAL + in_app))
    check_eq "$case_name 应用内断言条数" 15 "$in_app"

    check_contains "$case_name 展开 catalog 的 ListNodes 次数" \
        '断言 OK：展开 catalog 的 ListNodes 次数=2' "$log"
    check_count "$case_name ListNodes 发送次数（连接层 1 + 分组 2）" 3 \
        '（发送 ListNodes）' "$log"
    check_contains "$case_name 树搜索框获得焦点" '断言 OK：焦点在对象树搜索框' "$log"
    check_contains "$case_name 标签数（3 表 + Objects）" '标签数=4' "$log"
    check_contains "$case_name ListSessions 打开后" 'ListSessions（打开 3 个标签后）：后端共 4 条会话' "$log"
    check_contains "$case_name 排序" '断言 OK：第 1 个标签 sort=name:asc' "$log"
    check_contains "$case_name 筛选" '断言 OK：第 1 个标签 filters=1' "$log"
    check_contains "$case_name 第 2 页" '断言 OK：第 1 个标签 page=2' "$log"
    check_contains "$case_name 第 3 页" '断言 OK：第 1 个标签 page=3' "$log"
    check_contains "$case_name 导出完成" '[export] 完成：100 行' "$log"
    check_contains "$case_name 信息面板表信息" '断言 OK：信息面板 table_page=info' "$log"
    check_contains "$case_name 关闭标签后标签数" '标签数=3' "$log"
    check_contains "$case_name ListSessions 关闭后" 'ListSessions（关闭 1 个后）：后端共 3 条会话' "$log"

    check_file_lines "$case_name 导出 CSV 行数（表头 + 100 行）" 101 "$case_out/page3.csv"
    check_contains "$case_name CSV 表头" 'id,name,category,amount' "$case_out/page3.csv"
    check_contains "$case_name CSV 第一行来自第 3 页" '1001,item-1001,keep' "$case_out/page3.csv"

    local png
    for png in 01-tree 02-tabs 03-page3 04-info 05-export; do
        local path="$case_out/$png.png"
        local size=0
        [ -f "$path" ] && size=$(wc -c < "$path")
        if [ "$size" -gt 1000 ]; then
            ok "$case_name 截图 $png（$size 字节）"
        else
            bad "$case_name 截图 $png 缺失或过小（$size 字节）：$path"
        fi
        say "       截图：$path（$size 字节）"
    done

    say "       日志：$log（$(wc -l < "$log") 行）"
}

# ---------------------------------------------------------------------------
# 4. SQLite 用例
# ---------------------------------------------------------------------------

cat > "$TMP/profile-sqlite.json" <<EOF
{"driver":"sqlite","path":"$SQLITE_DB","read_only":false,"name":"v023-sqlite"}
EOF
run_case "sqlite" "sqlite" "$TMP/profile-sqlite.json" "" "main"

ISOLATED_AFTER_SQLITE=$(config_count "$XDG_CONFIG_HOME/sqarvo/Sqarvo/connections.json")
check_eq "隔离配置在 SQLite 用例后只有 1 条连接" 1 "$ISOLATED_AFTER_SQLITE"

# ---------------------------------------------------------------------------
# 5. MariaDB 用例（同一隔离配置：树里因此有 2 条连接）
# ---------------------------------------------------------------------------

cat > "$TMP/profile-mariadb.json" <<EOF
{"driver":"mariadb","host":"$MYSQL_HOST","port":$MYSQL_PORT,"database":"$MYSQL_DB","username":"$MYSQL_USER","tls_mode":"preferred","name":"v023-mariadb"}
EOF
run_case "mariadb" "mariadb" "$TMP/profile-mariadb.json" "SQARVO_QA_PASSWORD" "$MYSQL_DB"

check_contains "多连接：MariaDB 用例的注册表里有 2 条连接" \
    '[browse] 连接注册表：2 条连接' "$OUT/mariadb/run.log"

ISOLATED_CONFIG="$XDG_CONFIG_HOME/sqarvo/Sqarvo/connections.json"
ISOLATED_AFTER=$(config_count "$ISOLATED_CONFIG")
check_eq "隔离配置累计 2 条连接（SQLite + MariaDB）" 2 "$ISOLATED_AFTER"

DRIVERS=$("$PYTHON" - "$ISOLATED_CONFIG" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    print(",".join(sorted(item["driver"] for item in json.load(handle)["connections"])))
PY
)
check_eq "隔离配置里的驱动集合" "mariadb,sqlite" "$DRIVERS"
if grep -qiE '"(password|secret|token)"' "$ISOLATED_CONFIG"; then
    bad "隔离配置里不应出现 password / secret / token 字段"
else
    ok "隔离配置里没有 password / secret / token 字段"
fi

# ---------------------------------------------------------------------------
# 6. 配置隔离复核：真实连接配置一条都不能多
# ---------------------------------------------------------------------------

REAL_AFTER=$(config_count "$REAL_CONFIG")
check_eq "真实连接配置条目数（结束时 = 开始时）" "$REAL_BEFORE" "$REAL_AFTER"
case "$REAL_AFTER" in
    ''|*[!0-9]*) bad "真实连接配置条目数不可解析：$REAL_AFTER" ;;
    *)
        if [ "$REAL_AFTER" -gt "$REAL_BEFORE" ]; then
            bad "验收往真实配置写入了 $((REAL_AFTER - REAL_BEFORE)) 条残留"
        else
            ok "真实配置没有新增 QA 残留"
        fi
        ;;
esac

case "$OUT" in
    "$REPO_ROOT"/*) bad "证据目录落在仓库内：$OUT" ;;
    *) ok "证据目录在仓库外：$OUT" ;;
esac

# ---------------------------------------------------------------------------
# 7. 汇总
# ---------------------------------------------------------------------------

say ""
say "证据目录：$OUT"
say "应用内断言：$IN_APP_TOTAL 条；脚本断言：$((PASS_COUNT + FAIL_COUNT)) 条"
if [ "$FAIL_COUNT" -gt 0 ]; then
    say "验收失败：$FAIL_COUNT 项断言未通过（通过 $PASS_COUNT 项）"
    exit 1
fi
say "验收结果：$((PASS_COUNT + IN_APP_TOTAL)) 项断言全过"
