#!/usr/bin/env bash
# Sqarvo v0.2.3 验收链（#121）：一条命令跑完「多连接 → 展开树 → 开 3 个表标签 →
# 排序 → 筛选 → 翻页 → 信息面板 DDL/表信息 → 导出当前页 CSV → 关标签」。
# #141 追加一段无默认库用例：不填库名连 MariaDB → 树里两个库 → 各打开一张同名表 →
# 总数/排序/翻页/导出 → 两个标签互不影响（断言含标签名、ListSessions 计数与会话名）。
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
# v0.2.3.2 UI 契约下的假设（脚本按这些行为保持有效；改脚本时先看这里）：
#   * 单击只选中、双击 / 回车才激活：`select-node` 只选中（信息面板与 Objects 页跟着换内容）；
#     `open-table` / `open-node` 直接打开数据标签（终点与树里双击表相同，表名从对象树解析，
#     `open-node` 另按 `ObjectRef.catalog` 精确定位）。
#   * 信息面板常驻：`inspector ddl|info` 只换页；`hide` / `hidden` / `toggle` 现在会明确报错，
#     脚本不再使用面板显隐（`show` 仍被接受但无动作）。
#   * 侧栏平铺列表、页脚「DDL」按钮、右键「查看 DDL」已删除：表名由对象树解析，
#     DDL 只在信息面板的 `[DDL]` 页看，脚本不依赖平铺列表也不点右键菜单。
#   * 对象树：展开 catalog 仍按分组数发 `ListNodes`（Tables / Views 两组 → 2 次）；
#     描述表内部的 `Fields` / `Indexes` / `Foreign Keys` / `Triggers` 四组不影响本链
#     （脚本只展开到 catalog 与表，不展开表内分组）。
#   * 应用内断言条数是**写死**的（主用例 16、跨库用例 22，见 `check_eq`）：增删任何
#     `expect-*` / `focus-search` 步骤都必须同步这两个数字。
#
# 前置条件（脚本会检查并给出可读原因）：
#   * 已构建的前端/后端二进制（默认执行 cargo build；`SQARVO_ACCEPTANCE_NO_BUILD=1` 跳过）；
#   * 可用的图形会话（WAYLAND_DISPLAY 或 DISPLAY）；
#   * MariaDB 测试库 sqarvo_test_121（可用 `SQARVO_TEST_HOST/PORT/DB/USER` 覆盖）；
#   * #141 跨库用例需要两个已有库 sqarvo_test_140a / sqarvo_test_140a_b（账号有 ALL 权限；
#     可用 `SQARVO_CROSS_DB_A/B` 覆盖）：脚本会在其中各建一张同名表 probe_141_items；
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

# #141 跨库用例：两个**已有**的库（账号对它们有 ALL 权限），各放一张同名表。
# 库里已有 #140 的 probe_* 表（2–3 行），不够断言总数/翻页，所以这里另建一张
# 1200 行的同名表；两张表用 a-/b- 前缀区分，导出的 CSV 能证明读的是哪个库。
CROSS_DB_A=${SQARVO_CROSS_DB_A:-sqarvo_test_140a}
CROSS_DB_B=${SQARVO_CROSS_DB_B:-sqarvo_test_140a_b}
CROSS_TABLE=probe_141_items
CROSS_ROWS=1200

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

# 自渲染截图在本机 Wayland 会话里偶发「控件快照为空」（窗口 mapped 却没有渲染节点，见
# frontend/linux README 的「截图偶发失败」一节）；文档给出的处置是切 XWayland。这里默认
# 设成 x11，调用方显式指定的值仍然优先。
export GDK_BACKEND=${GDK_BACKEND:-x11}

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

# 只清理**本轮新出现**的后端进程：验收里应用被 `--qa-exit-after-ms` 收尾或超时中断时，
# 后端会被 reparent 成孤儿继续跑（实测每轮会留下 1–2 个）。跑前先把已在跑的后端记下来，
# EXIT 时只杀新出现的，避免误杀用户自己在用的实例。
backend_pids() { pgrep -f "$BACKEND" 2>/dev/null | sort -u || true; }
BACKEND_BEFORE="$TMP/backend-before.txt"
backend_pids > "$BACKEND_BEFORE"
cleanup_backends() {
    local rc=$?
    local pid
    for pid in $(backend_pids); do
        if ! grep -qx "$pid" "$BACKEND_BEFORE"; then
            kill "$pid" 2>/dev/null || true
        fi
    done
    exit "$rc"
}
trap cleanup_backends EXIT INT TERM

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
# 2b. 跨库用例的测试数据（#141）：两个库各一张 1200 行的同名表
# ---------------------------------------------------------------------------

"$PYTHON" - "$MYSQL_HOST" "$MYSQL_PORT" "$MYSQL_USER" "$SQARVO_QA_PASSWORD" \
    "$CROSS_DB_A" "$CROSS_DB_B" "$CROSS_TABLE" "$CROSS_ROWS" <<'PY'
import sys

import pymysql

host, port, user, password, db_a, db_b, table, rows = sys.argv[1:9]
rows = int(rows)
conn = pymysql.connect(
    host=host,
    port=int(port),
    user=user,
    password=password,
    charset="utf8mb4",
    autocommit=True,
    connect_timeout=10,
)
with conn.cursor() as cur:
    for database, prefix in ((db_a, "a"), (db_b, "b")):
        cur.execute(f"DROP TABLE IF EXISTS `{database}`.`{table}`")
        cur.execute(
            f"CREATE TABLE `{database}`.`{table}`("
            "id INT PRIMARY KEY, name VARCHAR(64) NOT NULL, "
            "category VARCHAR(16) NOT NULL, amount INT NOT NULL)"
        )
        cur.executemany(
            f"INSERT INTO `{database}`.`{table}` VALUES (%s,%s,%s,%s)",
            [
                (i, f"{prefix}-item-{i:04d}", "keep", i * 3 % 97)
                for i in range(1, rows + 1)
            ],
        )
conn.close()
PY
if [ $? -ne 0 ]; then
    fail_early "跨库用例建数据失败：需要已存在并已授权的库 $CROSS_DB_A / $CROSS_DB_B（账号 $MYSQL_USER）。用 root 执行：CREATE DATABASE $CROSS_DB_A; GRANT ALL ON $CROSS_DB_A.* TO '$MYSQL_USER'@'%';（$CROSS_DB_B 同理）"
fi
say "跨库测试数据就绪：$CROSS_DB_A.$CROSS_TABLE / $CROSS_DB_B.$CROSS_TABLE（各 $CROSS_ROWS 行，前缀 a-/b-）"

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
# v0.2.3.3 移除表头点击排序（产品决定）：排序只走筛选与排序面板，这里用面板的
# \`sort-panel\` 步骤（点「添加排序 + 应用」同一条路径）取代旧的列头 \`sort\` 步骤。
# （本 heredoc 未加引号，需要 $变量 展开；正文里的反引号必须转义，否则会被当命令执行。）
select 1
sort-panel 1 name asc
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
# 导出完成会弹「导出完成」模态：AdwAlertDialog 的首帧尺寸是 0x0，那一刻整窗取不到渲染
# 节点，截图会报「控件快照为空」（实测一次 1.5 s 重试内都没恢复）。等模态分配出真实尺寸
# 再拍，画面上才有那张「导出完成」提示（run 成功的截图里可以看到模态正文与路径）。
sleep 1500
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
        # 用户在用的 Sqarvo 实例占着 `io.sqarvo.Sqarvo`：验收必须用自己的 application-id，
        # 否则新进程只会把激活转给用户窗口然后以 0 退出（日志空白、截图全缺）。
        "SQARVO_QA_APP_ID=io.sqarvo.Sqarvo.QA141Acceptance"
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
    # 16 = 15 项原断言 + 1 项「Ctrl+F 经产品动作把焦点送到树搜索框」（#121 的残余项落地后
    # 该步骤从「只抓焦点」变成「先激活 win.focus-search 再只读断言」，多出一条断言行）。
    # v0.2.3.2 逐条复核后仍为 16：`expect-tabs` 只打印「标签数=N」、不产出「断言 OK：」，不计入。
    check_eq "$case_name 应用内断言条数" 16 "$in_app"

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
# 3b. #141 无默认库跨库用例：不填库名连 MariaDB → 树里两个库 → 各开一张同名表
# ---------------------------------------------------------------------------

# 与 run_case 分开写：既有用例的断言条数是写死的，跨库用例的步骤与断言完全不同；
# 混进同一个函数只会让两边的口径互相牵制。
run_cross_catalog_case() {
    local case_name="mariadb-nodb"
    local case_out="$OUT/$case_name"
    local run_root="$TMP/run-$case_name"
    local log="$case_out/run.log"
    local tabs="$case_out/tabs.txt"
    local conn_name="$MYSQL_USER@$MYSQL_HOST"
    local profile="$TMP/profile-mariadb-nodb.json"
    mkdir -p "$case_out" "$run_root/sqarvo"

    # 档案里**没有 database 字段**：对话框的库名留空，连接后按库列表浏览。
    # 也没有 name：会话名交给 default_name 派生（断言退化为 user@host）。
    cat > "$profile" <<EOF
{"driver":"mariadb","host":"$MYSQL_HOST","port":$MYSQL_PORT,"username":"$MYSQL_USER","tls_mode":"preferred"}
EOF

    cat > "$tabs" <<EOF
# #141 无默认库：树根层直接是库列表，不伪造「当前库」层
mark-list-nodes
select-node $CROSS_DB_B
expect-inspector page=catalog
expect-inspector title=$CROSS_DB_B
# 先截图再展开：展开会把目标滚进视口，连接节点会被挤出画面；
# 这一张要同时看到连接节点和两个库。
screenshot $case_out/01-tree-catalogs.png
expand $CROSS_DB_B
expect-list-nodes 2
# 打开 B 库的表：标签名 = 对象 @ 库（状态栏自 v0.2.3.3 起不再显示库名）
open-node $CROSS_DB_B $CROSS_TABLE
expect-tabs 2
expect-tab-title 1 $CROSS_TABLE @ $CROSS_DB_B
expect-browse 1 total=$CROSS_ROWS
expect-browse 1 rows=500
# 排序走筛选与排序面板（v0.2.3.3 起表头不可点，没有列头排序步骤）。
sort-panel 1 name asc
expect-browse 1 sort=name:asc
page 1 next
expect-browse 1 page=2
sessions 打开 B 库的表后
expect-sessions 2
screenshot $case_out/02-b-table.png
# 再打开 A 库的同名表：两个标签互不影响
select-node $CROSS_DB_A
expect-inspector page=catalog
expect-inspector title=$CROSS_DB_A
expand $CROSS_DB_A
open-node $CROSS_DB_A $CROSS_TABLE
expect-tabs 3
expect-tab-title 2 $CROSS_TABLE @ $CROSS_DB_A
expect-browse 2 total=$CROSS_ROWS
expect-browse 2 page=1
expect-browse 2 rows=500
expect-browse 2 sort=
sessions 打开 A 库的表后
expect-sessions 3
# 切回 B 标签：它的排序与页码都还在（没有共享任何「当前库」状态）
select 1
expect-browse 1 page=2
expect-browse 1 sort=name:asc
screenshot $case_out/03-two-catalogs-tabs.png
# 信息面板「连接」态：没有默认库时库一栏是「未指定」
select-connection $conn_name
expect-inspector page=connection
expect-inspector facts=默认库=未指定
screenshot $case_out/04-connection-unspecified.png
# 导出放在截图之后：导出完成的模态框会盖住界面（quit 会收起它）
export 1 $case_out/b-page2.csv
expect-file $case_out/b-page2.csv 5000
quit
EOF

    say ""
    say "== 用例 $case_name（驱动 mariadb，无默认库，库 $CROSS_DB_A / $CROSS_DB_B） =="
    env "XDG_CONFIG_HOME=$XDG_CONFIG_HOME" \
        "SQARVO_ENDPOINT_FILE=$run_root/sqarvo/backend-endpoint.json" \
        "SQARVO_BACKEND_BIN=$BACKEND" \
        "SQARVO_QA_APP_ID=io.sqarvo.Sqarvo.QA141Acceptance" \
        "SQARVO_QA_PASSWORD=$SQARVO_QA_PASSWORD" \
        timeout 150 "$BIN" \
        --window-size 1200x800 \
        --qa-profile "$profile" \
        --qa-password-env SQARVO_QA_PASSWORD \
        --qa-tabs-file "$tabs" > "$log" 2>&1
    local code=$?
    check_eq "$case_name 进程退出码" 0 "$code"

    local in_app
    in_app=$(grep -cF '断言 OK：' "$log" 2>/dev/null || true)
    IN_APP_TOTAL=$((IN_APP_TOTAL + in_app))
    # 22 = 两个库各 2 条（page/title）+ ListNodes 1 + 两个标签各 2 条（标题/总数）
    # + 分页/排序/行数/会话数 + 导出文件 + 连接面板 2 条。改动脚本时必须同步这里。
    # v0.2.3.2 复核：仍然成立（`expect-tabs` 不产出「断言 OK：」，信息面板常驻不影响这些步骤）。
    check_eq "$case_name 应用内断言条数" 22 "$in_app"

    check_contains "$case_name 档案以「无默认库」填入对话框" '（无默认库）' "$log"
    check_contains "$case_name 展开一个 catalog 的 ListNodes 次数" \
        '断言 OK：展开 catalog 的 ListNodes 次数=2' "$log"
    # 连接层 1 次 + 两个 catalog 各 2 个分组 = 5 次。
    check_count "$case_name ListNodes 发送次数（连接层 1 + 两组 ×2）" 5 \
        '（发送 ListNodes）' "$log"

    check_contains "$case_name B 库标签名带库" \
        "：$CROSS_TABLE @ $CROSS_DB_B" "$log"
    check_contains "$case_name A 库标签名带库" \
        "：$CROSS_TABLE @ $CROSS_DB_A" "$log"
    check_contains "$case_name 标签数（B 库表 + Objects）" '标签数=2' "$log"
    check_contains "$case_name 标签数（两个库的表 + Objects）" '标签数=3' "$log"
    check_contains "$case_name ListSessions 打开 B 库后" \
        'ListSessions（打开 B 库的表后）：后端共 2 条会话' "$log"
    check_contains "$case_name ListSessions 打开 A 库后" \
        'ListSessions（打开 A 库的表后）：后端共 3 条会话' "$log"
    # v0.2.3.3：状态栏不再显示库名（行 2 只留 SQL + Last Refresh），换库改由上面的标签名
    # 断言（`：$CROSS_TABLE @ 库`）覆盖——同一个事实只在一处取证。

    check_contains "$case_name B 库总数" "断言 OK：第 1 个标签 total=" "$log"
    check_contains "$case_name B 库排序" '断言 OK：第 1 个标签 sort=name:asc' "$log"
    check_contains "$case_name B 库第 2 页" '断言 OK：第 1 个标签 page=2' "$log"
    check_contains "$case_name A 库第 1 页" '断言 OK：第 2 个标签 page=1' "$log"
    check_contains "$case_name 标签标题（B 库）" \
        "断言 OK：第 1 个标签标题=$CROSS_TABLE @ $CROSS_DB_B" "$log"
    check_contains "$case_name 标签标题（A 库）" \
        "断言 OK：第 2 个标签标题=$CROSS_TABLE @ $CROSS_DB_A" "$log"
    check_contains "$case_name 连接面板库=未指定" '断言 OK：信息面板 facts=' "$log"

    check_file_lines "$case_name 导出 CSV 行数（表头 + 500 行）" 501 "$case_out/b-page2.csv"
    check_contains "$case_name CSV 表头" 'id,name,category,amount' "$case_out/b-page2.csv"
    check_contains "$case_name CSV 第 2 页首行来自 B 库" '501,b-item-0501,keep' \
        "$case_out/b-page2.csv"
    # 同名表在两个库内容不同：导出的必须只有 B 库前缀。
    if grep -qF 'a-item-' "$case_out/b-page2.csv"; then
        bad "$case_name B 库导出混进了 A 库的数据"
    else
        ok "$case_name B 库导出只有 B 库数据（无 a-item- 行）"
    fi

    local png
    for png in 01-tree-catalogs 02-b-table 03-two-catalogs-tabs 04-connection-unspecified; do
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

    # 会话名退化：无默认库时是 user@host，不是 user@host/。
    local saved_name
    saved_name=$("$PYTHON" - "$ISOLATED_CONFIG" "$MYSQL_USER@$MYSQL_HOST" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    entries = json.load(handle)["connections"]
# SQLite 的 database 也是空串：只看服务器型驱动的条目，否则会拿到 SQLite 那条。
names = [
    item["name"]
    for item in entries
    if item.get("driver") == "mariadb" and item.get("database", "") == ""
]
print(names[0] if len(names) == 1 else "?" + repr(names))
PY
)
    check_eq "$case_name 无默认库会话名退化为 用户@主机" "$conn_name" "$saved_name"
    if grep -qF '"name": "'"$conn_name"'/"' "$ISOLATED_CONFIG"; then
        bad "$case_name 隔离配置里出现了残缺会话名 user@host/"
    else
        ok "$case_name 隔离配置里没有残缺会话名 user@host/"
    fi
    check_eq "$case_name 隔离配置累计 3 条连接（+ 无默认库）" 3 \
        "$(config_count "$ISOLATED_CONFIG")"

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
# 5b. #141 无默认库跨库用例（不填库名连 MariaDB，同一隔离配置 → 第 3 条连接）
# ---------------------------------------------------------------------------

run_cross_catalog_case

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
