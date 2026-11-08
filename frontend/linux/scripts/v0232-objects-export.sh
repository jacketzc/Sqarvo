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
#      A1 `--qa-tree-expand main` → `--qa-tree-select main`：作用域「main · 表」、行数 1、
#         首行 probe_items、`empty=off`、状态行「共 1 个对象」（不含「还有更多」）。
#      A2 选中视图节点：作用域「main · 视图」、行数 1、首行 probe_recent_items。
#      两张截图分别对应 A1 / A2，路径打到 stdout。
#   B. EX 导出范围
#      B1 整表：行数 = 数据行 + 1（表头）、目标权限 0600、目录里没有 `*.sqarvo-tmp`、
#         日志命中「整表导出完成：」「整表进度：」。
#      B2 取消：`--qa-export-cancel-after-rows 10` → 目标**不存在**、无临时文件、
#         日志命中「整表导出已取消：已写入」「未创建」「已写入 10 行后取消」。
#      B3 对照：默认 `page` 范围仍成功（表头 + 首页行数），日志范围是「当前页」。
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
#   ./scripts/v0232-objects-export.sh
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

launch() {
    local case_name=$1 app_id=$2
    shift 2
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
        --window-size 1200x800 \
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

# A1：选中库 → 列出该库的表（`objects_scope` 对 Catalog 给出的文案是「<库> · 表」，ui.rs:991）。
launch objects-tables io.sqarvo.Sqarvo.QA024ObjectsTables \
    --qa-tree-expand main \
    --qa-tree-select main \
    --qa-expect-objects "scope=main · 表" \
    --qa-expect-objects count=1 \
    --qa-expect-objects "first=$TABLE" \
    --qa-expect-objects empty=off \
    --qa-expect-objects "status=共 1 个对象" \
    --screenshot "$OUT/objects-tables/01-scope-tables.png" \
    --screenshot-delay-ms 9000 \
    --qa-exit-after-ms 11000
check_contains "objects-tables 展开 catalog（Tables/Views 各自建组）" '展开 main' "$CASE_LOG"
check_contains "objects-tables 树里选中库节点" '选中节点 main' "$CASE_LOG"
check_contains "objects-tables Objects 页作用域=main · 表" \
    '断言通过：Objects scope=main · 表' "$CASE_LOG"
check_contains "objects-tables Objects 页行数" 'Objects count=1' "$CASE_LOG"
check_contains "objects-tables Objects 页首行" "Objects first=$TABLE" "$CASE_LOG"
check_contains "objects-tables Objects 页非空态" 'Objects empty=off' "$CASE_LOG"
check_contains "objects-tables Objects 状态行" 'Objects status=共 1 个对象' "$CASE_LOG"
# 单页数据不该出现「还有更多」：这条同时盯住「没有第二页时不要谎称还有」。
check_absent "objects-tables Objects 状态行不谎称还有更多" '还有更多' "$CASE_LOG"
check_screenshot "objects-tables 截图（选中库 → 表列表）" "$OUT/objects-tables/01-scope-tables.png"

# A2：选中视图 → 列出该库的视图（ui.rs:993-997 的文案是「<库> · 视图」）。
launch objects-views io.sqarvo.Sqarvo.QA024ObjectsViews \
    --qa-tree-expand main \
    --qa-tree-select "$VIEW" \
    --qa-expect-objects "scope=main · 视图" \
    --qa-expect-objects count=1 \
    --qa-expect-objects "first=$VIEW" \
    --qa-expect-objects empty=off \
    --qa-expect-objects "status=共 1 个对象" \
    --screenshot "$OUT/objects-views/02-scope-views.png" \
    --screenshot-delay-ms 9000 \
    --qa-exit-after-ms 11000
check_contains "objects-views 树里选中视图节点" "选中节点" "$CASE_LOG"
check_contains "objects-views 选中视图的路径带对象名" "$VIEW" "$CASE_LOG"
check_contains "objects-views Objects 页作用域=main · 视图" \
    '断言通过：Objects scope=main · 视图' "$CASE_LOG"
check_contains "objects-views Objects 页行数（该库只有 1 个视图）" 'Objects count=1' "$CASE_LOG"
check_contains "objects-views Objects 页首行" "Objects first=$VIEW" "$CASE_LOG"
check_contains "objects-views Objects 状态行" 'Objects status=共 1 个对象' "$CASE_LOG"
check_screenshot "objects-views 截图（选中视图 → 视图列表）" "$OUT/objects-views/02-scope-views.png"

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
# 5. 配置隔离与取证目录复核
# ---------------------------------------------------------------------------

REAL_AFTER=$(real_config_hash)
check_eq "真实连接配置逐字节未变（sha256）" "$REAL_BEFORE" "$REAL_AFTER"

case "$OUT" in
    "$REPO_ROOT"/*) bad "证据目录落在仓库内：$OUT" ;;
    *) ok "证据目录在仓库外：$OUT" ;;
esac

# ---------------------------------------------------------------------------
# 6. 汇总
# ---------------------------------------------------------------------------

say ""
say "证据目录：$OUT"
say "应用内断言：$IN_APP_TOTAL 条；脚本断言：$((PASS_COUNT + FAIL_COUNT)) 条"
if [ "$FAIL_COUNT" -gt 0 ]; then
    say "验收失败：$FAIL_COUNT 项断言未通过（通过 $PASS_COUNT 项）"
    exit 1
fi
say "验收结果：$((PASS_COUNT + IN_APP_TOTAL)) 项断言全过"
