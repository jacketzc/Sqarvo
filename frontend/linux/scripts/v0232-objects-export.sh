#!/usr/bin/env bash
# Sqarvo v0.2.3.2 验收：`Objects` 固定页 ↔ 对象树联动 + `EX` 导出范围三条路径。
#
# 自包含：只用 `sqlite3` 在 mktemp 目录里建一个临时库，**不需要 MariaDB、不需要密码**，
# 也不碰任何用户数据。串行跑 5 个应用实例（一次只有一个 GUI 进程），每个实例用独立的
# `SQARVO_QA_APP_ID`，避免单实例转发把后一个进程变成「激活旧窗口后退出」（那样日志与
# 截图都是空的）。
#
# 覆盖的用例（断言读的是控件文本与文件本身，不看「状态机里应该是」）：
#   A. Objects 页与对象树联动
#      A0（v0.2.3.3）双击库只在树里展开库本身：`--qa-tree-activate main` → 建两个分组行、
#         **没有**拉分组（`Tables` / `Views` 保持收起），截图取证。
#      A1 `--qa-tree-expand main` → `--qa-tree-select main`：行数 1、首行 probe_items、
#         `empty=off`、状态行「共 1 个对象」（不含「还有更多」）；v0.2.3.3 起页内不再有
#         标题与列表之间的作用域说明行（`scope` 断言字段已删除，旧脚本会直接报错）。
#      A2 选中视图节点：行数 1、首行 probe_recent_items。
#      A1b（v0.2.3.3）单列表格的单击选中：`--qa-objects-select probe_items` → 该行高亮
#         （`selected=probe_items`），信息面板同步成这张表。
#      两张截图分别对应 A1 / A2，路径打到 stdout。
#   A3（v0.2.3.3）Objects 页双击 / 回车激活：`--qa-objects-activate` 发
#      `GtkListView::activate` → 日志出现「[objects] 激活行」与「[tabs] 新建表标签」。
#   A4（v0.2.3.3）作用域没变不重发：树里先选库（main · 表）再选同一库的表 → 日志出现
#      「作用域未变」，列表仍是 1 行（不清空、不闪）。
#   A5（v0.2.3.3）重新进入 Objects 页不选中任何行：列表选中 → 打开表标签 → 切回 Objects
#      → 日志「重新进入 Objects 页：清掉行选中」，`selected=`（空串）断言成立。
#   A6（v0.2.3.3）主工具条种类入口：点「表」→ 树里选中库 + 列表出表；点「函数」→ 如实说明
#      未实现（两张截图）。
#   A7（v0.2.3.3）种类入口跟着当前选中的库走：选 temp 再点「表」→ 列出 temp（不是默认的 main）。
#   B. EX 导出范围
#      B1 整表：行数 = 数据行 + 1（表头）、目标权限 0600、目录里没有 `*.sqarvo-tmp`、
#         日志命中「整表导出完成：」「整表进度：」。
#      B2 取消：`--qa-export-cancel-after-rows 10` → 目标**不存在**、无临时文件、
#         日志命中「整表导出已取消：已写入」「未创建」「已写入 10 行后取消」。
#      B3 对照：默认 `page` 范围仍成功（表头 + 首页行数），日志范围是「当前页」。
#   C. 双击 / 回车激活：表有四组子节点时也必须打开表标签（v0.2.3.2 修正）
#      C1 `--qa-tree-activate` 走与双击相同的 `GtkListView::activate` 信号 →
#         日志出现「新建表标签」，且**没有**落到「展开 <表>：建立 4 个二级分组」。
#   D. 960×640 打开表时的布局自检
#      D1 `--qa-layout-audit` 报「20 项中 0 项超限」——行变更工具条第 2 行在 960 宽下
#         需求 391 ≤ 分配 417（修正前 439 > 417）。
#
# 形如 v023-acceptance.sh 的约束（改动本脚本时先看这里）：
#   * `XDG_CONFIG_HOME` 指向 mktemp 临时目录（每个用例一个子目录），验收产生的
#     连接配置绝不写进用户真实配置；结束时用 sha256 断言真实 `connections.json` 逐字节未变。
#   * 截图与日志落在仓库外（默认 `/home/jack/sqarvo-v0232-shots/<时间戳>/`，
#     可用 `SQARVO_ACCEPTANCE_DIR` 改基目录，仍然会追加 `<时间戳>/`）。目录落在仓库内即失败。
#   * 任何一步失败都以非零退出，末尾打印「N 项断言全过」。
#   * `--qa-expect-objects` 必须与 `--screenshot` 一起用（qa.rs 的解析强制），所以
#     A1 / A2 都是「截图时刻断言」的运行。
#   * 导出完成 / 取消会弹模态提示：脚本**不点任何按钮**，靠 `--qa-exit-after-ms` 的既有机制
#     （ui.rs 先 `qa::close_modal()` 收起登记的模态，再关窗）收尾。
#   * 每个应用运行都设了不同的 `SQARVO_QA_APP_ID`，并把 `SQARVO_ENDPOINT_FILE` 指到
#     自己的 run 目录（`<runtime_root>/sqarvo/backend-endpoint.json` 布局）。
#
# 暂缺（按当前代码做不到，没有为此改 src/）：
#   * Objects 页自己的「加载更多…」没有 QA 动作——`--qa-tree-load-more` 点的是**对象树**的
#     占位行（ui.rs:2313 → tree.rs:3086），Objects 页的按钮（tabs.rs:518）无人值守点不到；
#     而且每页 500 条（tabs.rs:621，服务端单页上限），要用约定里的 1 表 1 视图触发第二页
#     不现实。因此 A1 只断言状态行「共 1 个对象」且日志里**没有**「还有更多」。
#   * 对象树的分页（`--qa-tree-load-more`）需要某个分组 > 500 个节点；本脚本按约定只建
#     1 张表 1 个视图，故未覆盖。若将来要测，需要另一个把 500+ 对象塞进同一 catalog 的库，
#     且会与「count=1」的断言口径冲突，所以没有塞进同一份测试数据。
#   * 需求文案里的「已取消导出：已写入」在代码中的实际字符串是
#     「整表导出已取消：已写入 N 行，临时文件已删除」（worker.rs:3405 与 export_ui.rs:1189），
#     「未创建」只出现在 export_ui.rs:1189 的那一行；脚本按实际字符串断言。
#   * 需求里提到的 `--qa-tables...` 参数在 qa.rs 中不存在（没有任何 `--qa-tables*`），
#     打开表用的是 `--qa-tree-open <表名>`（qa.rs:2062 / tree.rs:2959）。
#
# 前置条件（脚本会检查并给出可读原因）：
#   * 已构建的前端 / 后端二进制（默认执行 cargo build；`SQARVO_ACCEPTANCE_NO_BUILD=1` 跳过）；
#   * 可用的图形会话（WAYLAND_DISPLAY 或 DISPLAY）；
#   * `sqlite3` 命令行（测试库只用它建）。
#
# 用法：
#   ./scripts/v0232-objects-export.sh                     # 全量（发布门禁）
#   SQARVO_ACCEPTANCE_ONLY=objects ./scripts/v0232-objects-export.sh
#                                                          # 只跑 Objects 段（A0–A7）
#   SQARVO_ACCEPTANCE_DIR=/tmp/shots SQARVO_ACCEPTANCE_NO_BUILD=1 ./scripts/v0232-objects-export.sh

set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)
FRONTEND_DIR="$REPO_ROOT/frontend/linux"

OUT_BASE=${SQARVO_ACCEPTANCE_DIR:-/home/jack/sqarvo-v0232-shots}
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="$OUT_BASE/$STAMP"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/sqarvo-v0232.XXXXXX")

# 测试数据：1 张表（30 行，含 NULL 与文本）+ 1 个索引 + 1 个触发器 + 1 个视图。
# Tables/Views 两个分组各有内容；表下的 Fields/Indexes/Triggers 也有内容。
TABLE=probe_items
VIEW=probe_recent_items
ROWS=30
CANCEL_AFTER=10

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

# 日志里**不应**出现某段文字（用来证明没有走到另一条路径）。
check_absent() {
    local name=$1 needle=$2 file=$3
    if grep -qF -- "$needle" "$file" 2>/dev/null; then
        bad "$name（不应出现「$needle」：$file）"
    else
        ok "$name（没有出现「$needle」）"
    fi
}

# 目标文件必须不存在（取消路径：不留半截文件）。
check_missing() {
    local name=$1 path=$2
    if [ -e "$path" ]; then
        bad "$name（目标不该存在：$path）"
    else
        ok "$name（目标不存在：$path）"
    fi
}

# 文件权限（GNU stat）。
check_mode() {
    local name=$1 expected=$2 file=$3 actual="缺失"
    [ -e "$file" ] && actual=$(stat -c %a "$file" 2>/dev/null || echo "?")
    check_eq "$name" "$expected" "$actual"
}

# 目录里不能留 `.….sqarvo-tmp`（CsvSink 的临时文件命名见 export.rs:530）。
check_no_tmp() {
    local name=$1 dir=$2 count
    count=$(find "$dir" -maxdepth 1 -name '*.sqarvo-tmp' 2>/dev/null | wc -l)
    check_eq "$name" 0 "$count"
}

# 截图必须存在且不是空壳（与 v023 的 1000 字节门槛一致）。
check_screenshot() {
    local name=$1 path=$2 size=0
    [ -f "$path" ] && size=$(wc -c < "$path")
    if [ "$size" -gt 1000 ]; then
        ok "$name（$size 字节）"
    else
        bad "$name 缺失或过小（$size 字节）：$path"
    fi
    say "       截图：$path（$size 字节）"
}

check_config_count() {
    local name=$1 file=$2
    check_eq "$name" 1 "$(grep -cF '"driver"' "$file" 2>/dev/null || true)"
}

real_config_hash() {
    if [ -f "$REAL_CONFIG" ]; then
        sha256sum "$REAL_CONFIG" | cut -d' ' -f1
    else
        printf 'absent'
    fi
}

fail_early() {
    say "验收无法开始：$*"
    exit 1
}

# ---------------------------------------------------------------------------
# 0. 前置检查 + 构建
# ---------------------------------------------------------------------------

if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ]; then
    fail_early "没有图形会话（WAYLAND_DISPLAY / DISPLAY 均为空）：截图与界面断言需要真实 GTK 窗口"
fi

command -v sqlite3 >/dev/null 2>&1 || fail_early "找不到 sqlite3（本脚本只用它建测试库）"

if [ "${SQARVO_ACCEPTANCE_NO_BUILD:-0}" != "1" ]; then
    say "构建后端与前端（SQARVO_ACCEPTANCE_NO_BUILD=1 可跳过）…"
    ( cd "$REPO_ROOT/backend" && cargo build -q -p sqarvo-backend-service ) || fail_early "后端构建失败"
    ( cd "$FRONTEND_DIR" && cargo build -q ) || fail_early "前端构建失败"
fi

BIN="$FRONTEND_DIR/target/debug/sqarvo-desktop"
BACKEND="$REPO_ROOT/backend/target/debug/sqarvo-backend"
[ -x "$BIN" ] || fail_early "找不到前端二进制：$BIN"
[ -x "$BACKEND" ] || fail_early "找不到后端二进制：$BACKEND"

mkdir -p "$OUT"

# 只清理**本轮新出现**的后端进程：应用被 `--qa-exit-after-ms` 收尾或超时中断时后端可能
# 变成孤儿继续跑；跑前把已在跑的记下来，EXIT 时只杀新出现的，避免误杀用户自己的实例。
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

REAL_BEFORE=$(real_config_hash)

say "验收目录：$OUT"
say "临时目录：$TMP"
say "测试库：$TMP/probe.db（$TABLE $ROWS 行 + 索引 + 触发器 + 视图 $VIEW）"

# ---------------------------------------------------------------------------
# 1. 建测试库（纯 sqlite3，无需 MariaDB）
# ---------------------------------------------------------------------------

DB="$TMP/probe.db"
sqlite3 "$DB" <<SQL
DROP VIEW IF EXISTS $VIEW;
DROP TRIGGER IF EXISTS ${TABLE}_touch;
DROP INDEX IF EXISTS ${TABLE}_name_idx;
DROP TABLE IF EXISTS $TABLE;

CREATE TABLE $TABLE(
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    amount INTEGER NOT NULL,
    note TEXT
);

WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < $ROWS)
INSERT INTO $TABLE(id, name, amount, note)
SELECT n, printf('item-%02d', n), n * 3,
       CASE WHEN n % 5 = 0 THEN NULL ELSE 'note-' || n END
FROM seq;

CREATE INDEX ${TABLE}_name_idx ON $TABLE(name);
CREATE VIEW $VIEW AS SELECT id, name FROM $TABLE WHERE amount > 10;
CREATE TRIGGER ${TABLE}_touch AFTER INSERT ON $TABLE
BEGIN
    UPDATE $TABLE SET note = COALESCE(note, '') WHERE id = NEW.id;
END;
SQL
if [ $? -ne 0 ]; then
    fail_early "建测试库失败：$DB"
fi

check_eq "测试库 $TABLE 行数" "$ROWS" "$(sqlite3 "$DB" "SELECT COUNT(*) FROM $TABLE;")"
check_eq "测试库表数（Objects 页「表」作用域的预期行数）" 1 \
    "$(sqlite3 "$DB" "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%';")"
check_eq "测试库视图数（Objects 页「视图」作用域的预期行数）" 1 \
    "$(sqlite3 "$DB" "SELECT COUNT(*) FROM sqlite_master WHERE type='view';")"
check_eq "测试库索引数（Fields/Indexes 分组有内容）" 1 \
    "$(sqlite3 "$DB" "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%';")"
check_eq "测试库触发器数（Triggers 分组有内容）" 1 \
    "$(sqlite3 "$DB" "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger';")"

PROFILE="$TMP/profile-sqlite.json"
cat > "$PROFILE" <<EOF
{"driver":"sqlite","path":"$DB","read_only":false,"name":"v0232-sqlite"}
EOF

# ---------------------------------------------------------------------------
# 2. 一个应用运行：独立配置目录 / 端点 / application-id，日志落到用例目录
# ---------------------------------------------------------------------------

CASE_OUT=""
CASE_LOG=""
LAST_CODE=0

# 窗口尺寸：默认 1200×800；用例需要在 960×640 下取证时在 launch 前设 CASE_WINDOW=960x640。
CASE_WINDOW=""

launch() {
    local case_name=$1 app_id=$2
    shift 2
    local size="${CASE_WINDOW:-1200x800}"
    CASE_OUT="$OUT/$case_name"
    CASE_LOG="$CASE_OUT/run.log"
    local run_root="$TMP/run-$case_name"
    local cfg="$TMP/config-$case_name"
    mkdir -p "$CASE_OUT" "$run_root/sqarvo" "$cfg"

    say ""
    say "== 用例 $case_name（application-id $app_id）=="
    env "XDG_CONFIG_HOME=$cfg" \
        "SQARVO_ENDPOINT_FILE=$run_root/sqarvo/backend-endpoint.json" \
        "SQARVO_BACKEND_BIN=$BACKEND" \
        "SQARVO_QA_APP_ID=$app_id" \
        timeout 180 "$BIN" \
        --window-size "$size" \
        --qa-profile "$PROFILE" \
        "$@" > "$CASE_LOG" 2>&1
    LAST_CODE=$?
    check_eq "$case_name 进程退出码" 0 "$LAST_CODE"

    local in_app
    in_app=$(grep -cF '[qa] 断言通过：' "$CASE_LOG" 2>/dev/null || true)
    IN_APP_TOTAL=$((IN_APP_TOTAL + in_app))
    check_config_count "$case_name 隔离配置只有 1 条连接" \
        "$cfg/sqarvo/Sqarvo/connections.json"
    say "       日志：$CASE_LOG（$(wc -l < "$CASE_LOG") 行）"
}

# ---------------------------------------------------------------------------
# 3. Case A：Objects 固定页 ↔ 对象树联动
# ---------------------------------------------------------------------------

# A0（v0.2.3.3）：双击库只展开库本身，`Tables` / `Views` 保持收起。走 `activate` 信号
# （与双击 / 回车同一条路径）——这是产品行为；`--qa-tree-expand` 是 QA 便捷（会连分组
# 一起展开），所以这一条不能拿它取证。
launch tree-catalog io.sqarvo.Sqarvo.QA024TreeCatalog \
    --qa-tree-activate main \
    --screenshot "$OUT/tree-catalog/00-catalog-collapsed.png" \
    --screenshot-delay-ms 6000 \
    --qa-exit-after-ms 8000
check_contains "tree-catalog 双击库建立了两个分组行（保持收起）" \
    '建立 2 个分组（Tables / Views，保持收起）' "$CASE_LOG"
check_absent "tree-catalog 没有自动展开分组" '分组一并展开' "$CASE_LOG"
check_absent "tree-catalog 没有拉取分组" '分组 Tables 拉取第 1 页' "$CASE_LOG"
check_screenshot "tree-catalog 截图（Tables / Views 收起）" \
    "$OUT/tree-catalog/00-catalog-collapsed.png"

# A1：选中库 → 列出该库的表（v0.2.3.3 起页内不再有「<库> · 表」说明行）。
launch objects-tables io.sqarvo.Sqarvo.QA024ObjectsTables \
    --qa-tree-expand main \
    --qa-tree-select main \
    --qa-objects-select "$TABLE" \
    --qa-expect-objects count=1 \
    --qa-expect-objects "first=$TABLE" \
    --qa-expect-objects "selected=$TABLE" \
    --qa-expect-objects empty=off \
    --qa-expect-objects "status=共 1 个对象" \
    --screenshot "$OUT/objects-tables/01-scope-tables.png" \
    --screenshot-delay-ms 9000 \
    --qa-exit-after-ms 11000
check_contains "objects-tables 展开 catalog（Tables/Views 各自建组）" '展开 main' "$CASE_LOG"
check_contains "objects-tables 树里选中库节点" '选中节点 main' "$CASE_LOG"
check_contains "objects-tables Objects 页选中行日志" "Objects 页选中「$TABLE」" "$CASE_LOG"
check_contains "objects-tables 选中行同步信息面板" "选中行 $TABLE" "$CASE_LOG"
check_contains "objects-tables Objects 页行数" 'Objects count=1' "$CASE_LOG"
check_contains "objects-tables Objects 页首行" "Objects first=$TABLE" "$CASE_LOG"
check_contains "objects-tables Objects 页选中行" "Objects selected=$TABLE" "$CASE_LOG"
check_contains "objects-tables Objects 页非空态" 'Objects empty=off' "$CASE_LOG"
check_contains "objects-tables Objects 状态行" 'Objects status=共 1 个对象' "$CASE_LOG"
# 页内不再有作用域说明行：截图路径与断言都证明不了它，所以直接断言字段被拒。
check_absent "objects-tables 页内不再输出作用域文案" 'Objects scope=' "$CASE_LOG"
# 单页数据不该出现「还有更多」：这条同时盯住「没有第二页时不要谎称还有」。
check_absent "objects-tables Objects 状态行不谎称还有更多" '还有更多' "$CASE_LOG"
check_screenshot "objects-tables 截图（选中库 → 表列表）" "$OUT/objects-tables/01-scope-tables.png"

# A2：选中视图 → 列出该库的视图。
launch objects-views io.sqarvo.Sqarvo.QA024ObjectsViews \
    --qa-tree-expand main \
    --qa-tree-select "$VIEW" \
    --qa-expect-objects count=1 \
    --qa-expect-objects "first=$VIEW" \
    --qa-expect-objects empty=off \
    --qa-expect-objects "status=共 1 个对象" \
    --screenshot "$OUT/objects-views/02-scope-views.png" \
    --screenshot-delay-ms 9000 \
    --qa-exit-after-ms 11000
check_contains "objects-views 树里选中视图节点" "选中节点" "$CASE_LOG"
check_contains "objects-views 选中视图的路径带对象名" "$VIEW" "$CASE_LOG"
check_contains "objects-views Objects 页行数（该库只有 1 个视图）" 'Objects count=1' "$CASE_LOG"
check_contains "objects-views Objects 页首行" "Objects first=$VIEW" "$CASE_LOG"
check_contains "objects-views Objects 状态行" 'Objects status=共 1 个对象' "$CASE_LOG"
check_screenshot "objects-views 截图（选中视图 → 视图列表）" "$OUT/objects-views/02-scope-views.png"

# A3（v0.2.3.3）：Objects 页行双击 / 回车 = 激活。`--qa-objects-activate` 发的是
# `GtkListView::activate`（与双击 / 回车同一条信号），走同一个打开回调 → 新建表标签。
launch objects-activate io.sqarvo.Sqarvo.QA024ObjectsActivate \
    --qa-tree-expand main \
    --qa-tree-select main \
    --qa-objects-activate "$TABLE" \
    --qa-exit-after-ms 12000
check_contains "objects-activate 激活行日志" "[objects] 激活行 $TABLE" "$CASE_LOG"
check_contains "objects-activate 新建了表标签" '[tabs] 新建表标签' "$CASE_LOG"

# A4（v0.2.3.3）：作用域没变不重发。树里先选库（作用域 main · 表），在列表里点亮一行，
# 再选同一库里的表——作用域仍是 (main, [Table])，列表原样保留（日志「作用域未变」），
# 行数仍是 1，但行高亮被清掉（高亮属于上一次浏览，不该和右侧面板错位）。
cat > "$TMP/objects-keep.txt" <<STEPS
expand main
select-node main
sleep 1200
objects-select $TABLE
sleep 500
select-node $TABLE
sleep 500
STEPS
launch objects-keep io.sqarvo.Sqarvo.QA024ObjectsKeep \
    --qa-tabs-file "$TMP/objects-keep.txt" \
    --qa-expect-objects count=1 \
    --qa-expect-objects "first=$TABLE" \
    --qa-expect-objects "selected=" \
    --screenshot "$OUT/objects-keep/03-scope-unchanged.png" \
    --screenshot-delay-ms 7000 \
    --qa-exit-after-ms 9000
check_contains "objects-keep 作用域未变（保留列表）" \
    '[objects] 作用域未变' "$CASE_LOG"
check_contains "objects-keep 树选中变化清掉行高亮" \
    '[objects] 树选中变化：清掉行高亮' "$CASE_LOG"
check_contains "objects-keep 列表没有重发（行数仍是 1）" 'Objects count=1' "$CASE_LOG"
check_contains "objects-keep 行高亮为空" 'Objects selected=' "$CASE_LOG"
check_screenshot "objects-keep 截图（作用域未变后列表仍在）" \
    "$OUT/objects-keep/03-scope-unchanged.png"

# A5（v0.2.3.3）：重新进入 `Objects` 页不选中任何行。
# 在列表里点亮一行 → 新建查询标签（切走，且**不经过树选中**，否则高亮会先被树清掉）
# → `select 0` 切回 Objects：行高亮必须清空，断言 `selected=`（空串）成立。
cat > "$TMP/objects-reentry.txt" <<STEPS
expand main
select-node main
sleep 1200
objects-select $TABLE
sleep 500
query-tab
sleep 900
select 0
STEPS
launch objects-reentry io.sqarvo.Sqarvo.QA024ObjectsReentry \
    --qa-tabs-file "$TMP/objects-reentry.txt" \
    --qa-expect-objects count=1 \
    --qa-expect-objects "selected=" \
    --screenshot "$OUT/objects-reentry/04-reentry-clears-selection.png" \
    --screenshot-delay-ms 9000 \
    --qa-exit-after-ms 11000
check_contains "objects-reentry 切回 Objects 页清掉行选中" \
    '[objects] 重新进入 Objects 页：清掉行选中' "$CASE_LOG"
check_contains "objects-reentry 行选中为空" 'Objects selected=' "$CASE_LOG"
check_screenshot "objects-reentry 截图（重新进入后无高亮）" \
    "$OUT/objects-reentry/04-reentry-clears-selection.png"

# A6（v0.2.3.3）：主工具条的种类入口。`--qa-kind` 发的是按钮自己的 `clicked` 信号。
# 表：选中树里的库 + Objects 页列出表；函数：如实显示「尚未实现」，不摆假数据。
launch kind-tables io.sqarvo.Sqarvo.QA024KindTables \
    --qa-kind tables \
    --qa-expect-objects count=1 \
    --qa-expect-objects "first=$TABLE" \
    --screenshot "$OUT/kind-tables/05-toolbar-tables.png" \
    --screenshot-delay-ms 6000 \
    --qa-exit-after-ms 8000
check_contains "kind-tables 点了种类入口「表」" '[toolbar] 种类入口「表」' "$CASE_LOG"
check_contains "kind-tables 树里选中了库" '选中对象：main' "$CASE_LOG"
check_contains "kind-tables Objects 页列出表" 'Objects first=probe_items' "$CASE_LOG"
check_screenshot "kind-tables 截图（工具条 → 表列表）" \
    "$OUT/kind-tables/05-toolbar-tables.png"

launch kind-functions io.sqarvo.Sqarvo.QA024KindFunctions \
    --qa-kind functions \
    --screenshot "$OUT/kind-functions/06-toolbar-functions.png" \
    --screenshot-delay-ms 5000 \
    --qa-exit-after-ms 7000
check_contains "kind-functions 函数列表如实说明未实现" \
    '[toolbar] 种类入口「函数」：列表尚未实现' "$CASE_LOG"
check_screenshot "kind-functions 截图（未实现说明）" \
    "$OUT/kind-functions/06-toolbar-functions.png"

# A7（v0.2.3.3）：种类入口跟着**当前选中项所在的库**走，不锁死在默认库。
# SQLite 目录层固定有 main 与 temp：先选 temp 再点「表」，列表必须是 temp 的（0 个对象）。
launch kind-follows io.sqarvo.Sqarvo.QA024KindFollows \
    --qa-tree-select temp \
    --qa-kind tables \
    --qa-expect-objects count=0 \
    --screenshot "$OUT/kind-follows/07-toolbar-follows-library.png" \
    --screenshot-delay-ms 6000 \
    --qa-exit-after-ms 8000
check_contains "kind-follows 列出的是 temp（不是 main）" \
    '[toolbar] 种类入口「表」：列出 temp' "$CASE_LOG"
check_absent "kind-follows 没有跳回默认库 main" \
    '[toolbar] 种类入口「表」：列出 main' "$CASE_LOG"
check_contains "kind-follows 树里选中的仍是 temp" '选中对象：temp（库）' "$CASE_LOG"
check_screenshot "kind-follows 截图（跟着选中的库走）" \
    "$OUT/kind-follows/07-toolbar-follows-library.png"

# ---------------------------------------------------------------------------
# 3b. 只跑 Objects 段（SQARVO_ACCEPTANCE_ONLY=objects）
# ---------------------------------------------------------------------------
# B 段（EX 导出 CSV，#120）、C 段（树激活回归）与 D 段（960 布局自检）都是发布门禁的
# 一部分，但与 `Objects` 页无关：调 Objects 时跑它们每次多花几分钟、还会把无关用例的
# 失败混进结论。要全量证据时不要设这个变量。
if [ -n "${SQARVO_ACCEPTANCE_ONLY:-}" ] && [ "$SQARVO_ACCEPTANCE_ONLY" != "all" ]; then
    if [ "$SQARVO_ACCEPTANCE_ONLY" != "objects" ]; then
        say "SQARVO_ACCEPTANCE_ONLY 只接受 objects|all，实际：$SQARVO_ACCEPTANCE_ONLY"
        exit 1
    fi
    REAL_AFTER=$(real_config_hash)
    check_eq "真实连接配置逐字节未变（sha256）" "$REAL_BEFORE" "$REAL_AFTER"
    case "$OUT" in
        "$REPO_ROOT"/*) bad "证据目录落在仓库内：$OUT" ;;
        *) ok "证据目录在仓库外：$OUT" ;;
    esac
    say ""
    say "SQARVO_ACCEPTANCE_ONLY=objects：只跑了 Objects 段（A0–A7）"
    say "跳过：B 段 EX 导出、C 段树激活、D 段 960 布局（与 Objects 无关）"
    say "证据目录：$OUT"
    say "应用内断言：$IN_APP_TOTAL 条；脚本断言：$((PASS_COUNT + FAIL_COUNT)) 条"
    if [ "$FAIL_COUNT" -gt 0 ]; then
        say "验收失败：$FAIL_COUNT 项断言未通过（通过 $PASS_COUNT 项）"
        exit 1
    fi
    say "验收结果：$((PASS_COUNT + IN_APP_TOTAL)) 项断言全过"
    exit 0
fi

# ---------------------------------------------------------------------------
# 4. Case B：EX 导出范围（整表 / 取消 / 当前页）
# ---------------------------------------------------------------------------

# B1：整表导出。`--qa-tree-open` 打开数据标签，数据就绪 700 ms 后触发导出
# （data_page.rs:2472-2484），确认对话框由 QA 自动点「导出」（export_ui.rs:595-602）。
launch export-whole io.sqarvo.Sqarvo.QA024ExportWhole \
    --qa-tree-expand main \
    --qa-tree-open "$TABLE" \
    --qa-export "$OUT/export-whole/whole.csv" \
    --qa-export-scope table \
    --qa-exit-after-ms 15000
check_contains "export-whole 导出范围=整张表" '范围 整张表' "$CASE_LOG"
check_contains "export-whole 交给后台线程" '整表导出已交给后台' "$CASE_LOG"
check_contains "export-whole 进度日志（单页 500，累计 30 行）" \
    "整表进度：offset=0 页大小 500 本页 $ROWS 行，累计 $ROWS 行" "$CASE_LOG"
check_contains "export-whole 完成日志行数" "整表导出完成：$ROWS 行" "$CASE_LOG"
check_file_lines "export-whole CSV 行数（表头 + $ROWS 行）" "$((ROWS + 1))" \
    "$OUT/export-whole/whole.csv"
check_contains "export-whole CSV 表头" 'id,name,amount,note' "$OUT/export-whole/whole.csv"
check_contains "export-whole CSV 第一行" '1,item-01,3,note-1' "$OUT/export-whole/whole.csv"
check_contains "export-whole CSV 末页行" '29,item-29,87,note-29' "$OUT/export-whole/whole.csv"
check_mode "export-whole 目标权限" 600 "$OUT/export-whole/whole.csv"
check_no_tmp "export-whole 目录没有 .sqarvo-tmp 残留" "$OUT/export-whole"

# B2：取消路径。`--qa-export-cancel-after-rows 10` 在写满 10 行后走与点「取消」同一条
# 清理路径（worker.rs:3335 reached → report_export_cancelled；export_ui.rs:1183-1199）：
# 临时文件删除、目标不创建。模态提示由 `--qa-exit-after-ms` 的既有机制收起。
launch export-cancel io.sqarvo.Sqarvo.QA024ExportCancel \
    --qa-tree-expand main \
    --qa-tree-open "$TABLE" \
    --qa-export "$OUT/export-cancel/cancel.csv" \
    --qa-export-scope table \
    --qa-export-cancel-after-rows "$CANCEL_AFTER" \
    --qa-exit-after-ms 15000
check_contains "export-cancel 进度日志（取消前累计 10 行）" \
    "累计 $CANCEL_AFTER 行" "$CASE_LOG"
check_contains "export-cancel 后台取消日志" \
    "整表导出已取消：已写入 $CANCEL_AFTER 行" "$CASE_LOG"
check_contains "export-cancel 日志写明目标未创建" '未创建' "$CASE_LOG"
check_contains "export-cancel 提示写明「已写入 N 行后取消」" \
    "已写入 $CANCEL_AFTER 行后取消" "$CASE_LOG"
check_absent "export-cancel 没有报成功" '整表导出完成' "$CASE_LOG"
check_missing "export-cancel 目标没有被创建" "$OUT/export-cancel/cancel.csv"
check_no_tmp "export-cancel 目录没有 .sqarvo-tmp 残留" "$OUT/export-cancel"

# B3：对照——默认 `page` 范围（不传 `--qa-export-scope`）仍然成功。行数 = 首页行数 + 1；
# 30 行的表首页就是全部 30 行，所以与 B1 行数相同，但日志里的**范围**必须不同。
launch export-page io.sqarvo.Sqarvo.QA024ExportPage \
    --qa-tree-expand main \
    --qa-tree-open "$TABLE" \
    --qa-export "$OUT/export-page/page.csv" \
    --qa-exit-after-ms 15000
check_contains "export-page 导出范围=当前页（默认值）" '范围 当前页' "$CASE_LOG"
check_absent "export-page 没有走整表路径" '范围 整张表' "$CASE_LOG"
check_contains "export-page 完成日志行数" "[export] 完成：$ROWS 行" "$CASE_LOG"
check_file_lines "export-page CSV 行数（表头 + 首页 $ROWS 行）" "$((ROWS + 1))" \
    "$OUT/export-page/page.csv"
check_contains "export-page CSV 表头" 'id,name,amount,note' "$OUT/export-page/page.csv"
check_mode "export-page 目标权限" 600 "$OUT/export-page/page.csv"
check_no_tmp "export-page 目录没有 .sqarvo-tmp 残留" "$OUT/export-page"

# ---------------------------------------------------------------------------
# 5. Case C：双击 / 回车激活（v0.2.3.2 修正）
# ---------------------------------------------------------------------------

# 表在 v0.2.3.2 起有四组子节点（`has_children=true`），激活时若先判 `expandable()`
# 就只会展开、不会打开（用户反馈「双击表应该打开该表」）。这里走的是与双击 / 回车
# 完全相同的 `GtkListView::activate` 信号（`--qa-tree-activate`），断言三件事：
# 真的开了数据标签、日志里说明走的是 activate 信号、并且**没有**落到「展开表」那条路。
launch tree-activate io.sqarvo.Sqarvo.QA0232Activate \
    --qa-tree-expand main \
    --qa-tree-activate "$TABLE" \
    --qa-exit-after-ms 12000
check_contains "tree-activate 新建了表标签" '[tabs] 新建表标签' "$CASE_LOG"
check_contains "tree-activate 走的是 activate 信号" '双击 / 回车同一条 activate 信号' "$CASE_LOG"
check_absent "tree-activate 没有落到「展开表」" "展开 $TABLE：建立" "$CASE_LOG"

# ---------------------------------------------------------------------------
# 5b. Case D：960×640 打开表时的布局自检
# ---------------------------------------------------------------------------

# 行变更工具条第 2 行在 960 宽下曾经需求 439 > 分配 417（超 22 px）。这里在 960 下真的
# 打开表再跑 `--qa-layout-audit`：它逐区域比较最小/自然宽度与实际分配，超限时以 5 退出。
CASE_WINDOW=960x640 launch layout-960 io.sqarvo.Sqarvo.QA0232Layout960 \
    --qa-tree-expand main \
    --qa-tree-open "$TABLE" \
    --screenshot "$OUT/layout-960/01-wide-960.png" \
    --screenshot-delay-ms 6000 \
    --qa-layout-audit \
    --qa-exit-after-ms 8000
CASE_WINDOW=""
check_contains "layout-960 20 项 0 超限" '20 项中 0 项超限' "$CASE_LOG"
check_contains "layout-960 第 2 行工具条在分配宽度内" '第2行(行变更·次要动作)' "$CASE_LOG"

# ---------------------------------------------------------------------------
# 6. 配置隔离与取证目录复核
# ---------------------------------------------------------------------------

REAL_AFTER=$(real_config_hash)
check_eq "真实连接配置逐字节未变（sha256）" "$REAL_BEFORE" "$REAL_AFTER"

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
