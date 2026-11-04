#!/usr/bin/env bash
# Sqarvo Linux 前端基准脚本（REQ-PERF-001 / REQ-PERF-002）。
#
# 一条命令跑完四组场景，输出 Markdown 表到 stdout 并写入文件：
#   1. 冷启动：进程入口 → UI 进入 Connected，重复 3 次取中位数；
#   2. 空闲内存：连接数据库后静置 5 秒的 VmRSS / VmHWM；
#   3. 10 万行：首批到达、全量加载耗时与加载后 VmRSS；
#   4. 200 列宽表：加载耗时、VmRSS、列数。
# 另记录「10 万行但不滚动」：证明没有用户滚动就不会继续预取（背压）。
#
# 前提：后端二进制与 MariaDB 测试库已就绪（见 frontend/linux/README.md）。
# 脚本自身会清理残留进程与 runtime 目录，可重复执行；任一步失败即非 0 退出并指明步骤。
#
# 用法：
#   frontend/linux/scripts/bench.sh [--out FILE]
# 环境变量：
#   SQARVO_BENCH_BIN       前端二进制（默认 target/debug/sqarvo-desktop）
#   SQARVO_BENCH_BACKEND   后端二进制（默认 backend/target/debug/sqarvo-backend）
#   SQARVO_BENCH_DB_HOST / _PORT / _NAME / _USER / _PASSWORD
#   SQARVO_BENCH_OUT       输出文件（默认 /tmp/sqarvo-bench-<时间戳>.md）

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FRONTEND_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$FRONTEND_DIR/../.." && pwd)"

BIN="${SQARVO_BENCH_BIN:-$FRONTEND_DIR/target/debug/sqarvo-desktop}"
BACKEND="${SQARVO_BENCH_BACKEND:-$REPO_DIR/backend/target/debug/sqarvo-backend}"
DB_HOST="${SQARVO_BENCH_DB_HOST:-127.0.0.1}"
DB_PORT="${SQARVO_BENCH_DB_PORT:-53306}"
DB_NAME="${SQARVO_BENCH_DB_NAME:-sqarvo_test_29}"
DB_USER="${SQARVO_BENCH_DB_USER:-sqarvo_test}"
DB_PASSWORD="${SQARVO_BENCH_DB_PASSWORD:-sqarvo_test_only}"

OUT=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out)
            [[ $# -ge 2 ]] || { echo "bench: --out 需要一个路径" >&2; exit 2; }
            OUT="$2"
            shift 2
            ;;
        -h|--help)
            sed -n '2,30p' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *)
            echo "bench: 未知参数 $1" >&2
            exit 2
            ;;
    esac
done

STAMP="$(date +%Y%m%d-%H%M%S)"
OUT="${OUT:-/tmp/sqarvo-bench-$STAMP.md}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sqarvo-bench.XXXXXX")"
APP_PID=""
KEEP_WORK=0

# ---------------------------------------------------------------- 基础设施

log() { printf '%s\n' "$*" >&2; }

fail() {
    log "BENCH 失败：$*"
    log "BENCH 日志保留在：$WORK"
    KEEP_WORK=1
    exit 1
}

stop_app() {
    if [[ -n "$APP_PID" ]] && kill -0 "$APP_PID" 2>/dev/null; then
        kill -TERM "$APP_PID" 2>/dev/null || true
        for _ in $(seq 1 60); do
            kill -0 "$APP_PID" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$APP_PID" 2>/dev/null || true
    fi
    if [[ -n "$APP_PID" ]]; then
        wait "$APP_PID" 2>/dev/null || true
    fi
    APP_PID=""
}

cleanup() {
    stop_app
    pkill -x sqarvo-desktop 2>/dev/null || true
    pkill -x sqarvo-backend 2>/dev/null || true
    rm -rf "${XDG_RUNTIME_DIR:-/run/user/1000}/sqarvo" 2>/dev/null || true
}
trap 'stop_app; cleanup; [[ "$KEEP_WORK" == "1" ]] || rm -rf "$WORK"' EXIT

reset_env() {
    stop_app
    pkill -x sqarvo-desktop 2>/dev/null || true
    pkill -x sqarvo-backend 2>/dev/null || true
    rm -rf "${XDG_RUNTIME_DIR:-/run/user/1000}/sqarvo"
    mkdir -p "$WORK/config"
}

start_app() {
    local logfile="$1"
    shift
    reset_env
    : > "$logfile"
    local -a envs=(
        "DISPLAY=${DISPLAY:-:0}"
        "WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-wayland-0}"
        "XDG_CONFIG_HOME=$WORK/config"
        "SQARVO_QA_PASSWORD=$DB_PASSWORD"
    )
    # 只在显式设置时传入：空字符串会被前端当作「非法值」并打印警告。
    [[ -n "${SQARVO_RESULT_ROW_LIMIT:-}" ]] && envs+=("SQARVO_RESULT_ROW_LIMIT=$SQARVO_RESULT_ROW_LIMIT")
    [[ -n "${SQARVO_RESULT_BYTES_LIMIT:-}" ]] && envs+=("SQARVO_RESULT_BYTES_LIMIT=$SQARVO_RESULT_BYTES_LIMIT")
    env "${envs[@]}" stdbuf -oL -eL "$BIN" "$@" >>"$logfile" 2>&1 &
    APP_PID=$!
}

# wait_for <logfile> <regex> <timeout-seconds>
wait_for() {
    local logfile="$1" pattern="$2" timeout="$3"
    local deadline=$((SECONDS + timeout))
    while (( SECONDS < deadline )); do
        if grep -Eq "$pattern" "$logfile" 2>/dev/null; then
            return 0
        fi
        if ! kill -0 "$APP_PID" 2>/dev/null; then
            log "BENCH 应用提前退出（等待 /$pattern/ 时）"
            return 1
        fi
        sleep 0.05
    done
    log "BENCH 等待超时（${timeout}s）：/$pattern/"
    return 1
}

# ms_of <logfile> <regex>：取最后一条匹配行里的 t=+Nms
ms_of() {
    local logfile="$1" pattern="$2"
    grep -E "$pattern" "$logfile" 2>/dev/null | tail -1 \
        | sed -nE 's/.*t=\+([0-9]+)ms.*/\1/p'
}

# rss_mb <pid> <VmRSS|VmHWM>
rss_mb() {
    awk -v key="$2:" '$1 == key { printf "%.1f", $2 / 1024 }' "/proc/$1/status" 2>/dev/null
}

write_profile() {
    cat > "$WORK/profile.json" <<EOF
{"driver":"mariadb","host":"$DB_HOST","port":$DB_PORT,"database":"$DB_NAME",
 "username":"$DB_USER","tls_mode":"preferred","name":"bench@$DB_HOST"}
EOF
}

# ---------------------------------------------------------------- 前置检查

[[ -x "$BIN" ]] || fail "前端二进制不存在或不可执行：$BIN（先 cd frontend/linux && cargo build）"
[[ -x "$BACKEND" ]] || fail "后端二进制不存在或不可执行：$BACKEND（先 cd backend && cargo build -p sqarvo-backend-service）"
write_profile

if ! (exec 3<>"/dev/tcp/$DB_HOST/$DB_PORT") 2>/dev/null; then
    fail "无法连接 MariaDB $DB_HOST:$DB_PORT（容器 sqarvo-mariadb-test 是否在运行？）"
fi

# ---------------------------------------------------------------- 机器信息

CPU_MODEL="$(awk -F': ' '/model name/ { print $2; exit }' /proc/cpuinfo)"
CPU_CORES="$(nproc)"
MEM_TOTAL="$(awk '/^MemTotal:/ { printf "%.1f GiB", $2 / 1048576 }' /proc/meminfo)"
KERNEL="$(uname -srm)"
RUSTC="$(rustc --version 2>/dev/null || echo '未安装')"
GTK_VERSION="$(pkg-config --modversion gtk4 2>/dev/null || echo '未知')"
ADW_VERSION="$(pkg-config --modversion libadwaita-1 2>/dev/null || echo '未知')"
if [[ "$BIN" == *"/release/"* ]]; then
    BUILD_PROFILE="release"
else
    BUILD_PROFILE="debug"
fi
BIN_SIZE="$(du -h "$BIN" | cut -f1)"

RESULT_LIMIT_DESC="默认（行数 500000 / 估算 512.0 MiB）"
[[ -n "${SQARVO_RESULT_ROW_LIMIT:-}" ]] && RESULT_LIMIT_DESC="SQARVO_RESULT_ROW_LIMIT=$SQARVO_RESULT_ROW_LIMIT"
[[ -n "${SQARVO_RESULT_BYTES_LIMIT:-}" ]] && RESULT_LIMIT_DESC="$RESULT_LIMIT_DESC，SQARVO_RESULT_BYTES_LIMIT=$SQARVO_RESULT_BYTES_LIMIT"

# ---------------------------------------------------------------- 场景 1：冷启动

COLD_SAMPLES=()
for run in 1 2 3; do
    LOGFILE="$WORK/cold-$run.log"
    start_app "$LOGFILE" --qa-exit-after-ms 15000
    wait_for "$LOGFILE" 'state=Connected' 30 || fail "冷启动第 $run 次：未在 30s 内进入 Connected（见 $LOGFILE）"
    SAMPLE="$(ms_of "$LOGFILE" 'state=Connected')"
    [[ -n "$SAMPLE" ]] || fail "冷启动第 $run 次：日志里没有 t=+Nms state=Connected"
    COLD_SAMPLES+=("$SAMPLE")
    log "冷启动第 $run 次：${SAMPLE} ms"
    stop_app
done

# 中位数：排序后取中间值（不取最好值）。
mapfile -t COLD_SORTED < <(printf '%s\n' "${COLD_SAMPLES[@]}" | sort -n)
COLD_MEDIAN="${COLD_SORTED[1]}"
log "冷启动中位数：${COLD_MEDIAN} ms（样本 ${COLD_SAMPLES[*]}）"

# ---------------------------------------------------------------- 场景 2：空闲内存

LOG_IDLE="$WORK/idle.log"
start_app "$LOG_IDLE" --qa-profile "$WORK/profile.json" --qa-password-env SQARVO_QA_PASSWORD \
    --qa-action save --qa-exit-after-ms 30000
wait_for "$LOG_IDLE" 'state=Workspace' 40 || fail "空闲内存：未在 40s 内连接数据库（见 $LOG_IDLE）"
sleep 5
IDLE_RSS="$(rss_mb "$APP_PID" VmRSS)"
IDLE_HWM="$(rss_mb "$APP_PID" VmHWM)"
[[ -n "$IDLE_RSS" && -n "$IDLE_HWM" ]] || fail "空闲内存：无法读取 /proc/$APP_PID/status"
IDLE_CONNECTED_MS="$(ms_of "$LOG_IDLE" 'state=Workspace')"
log "空闲内存：VmRSS ${IDLE_RSS} MB / VmHWM ${IDLE_HWM} MB（连接后静置 5s）"
stop_app

# ---------------------------------------------------------------- 场景 3：10 万行

cat > "$WORK/rows100k.sql" <<'EOF'
SELECT seq AS n, CONCAT('row-', seq) AS label FROM seq_1_to_100000;
EOF

LOG_100K="$WORK/100k.log"
start_app "$LOG_100K" --qa-profile "$WORK/profile.json" --qa-password-env SQARVO_QA_PASSWORD \
    --qa-action save --qa-sql-file "$WORK/rows100k.sql" --qa-execute \
    --qa-scroll-to-row 99999 --qa-exit-after-ms 180000
wait_for "$LOG_100K" '首批到达' 60 || fail "10 万行：首批未在 60s 内到达（见 $LOG_100K）"
FIRST_BATCH_MS="$(ms_of "$LOG_100K" '首批到达')"
FIRST_BATCH_RSS="$(rss_mb "$APP_PID" VmRSS)"
wait_for "$LOG_100K" '结果读完' 180 || fail "10 万行：全量加载未在 180s 内完成（见 $LOG_100K）"
FULL_LOAD_MS="$(ms_of "$LOG_100K" '结果读完')"
FULL_LOAD_RSS="$(rss_mb "$APP_PID" VmRSS)"
FULL_LOAD_HWM="$(rss_mb "$APP_PID" VmHWM)"
ROWS_LOADED="$(grep -oE '结果读完，共 [0-9]+ 行' "$LOG_100K" | tail -1 | grep -oE '[0-9]+' | head -1)"
[[ "$ROWS_LOADED" == "100000" ]] || fail "10 万行：日志报告共 ${ROWS_LOADED} 行，不是 100000（见 $LOG_100K）"
FETCH_BATCHES="$(grep -c '批次到达' "$LOG_100K")"
EST_100K="$(grep '结果集计量' "$LOG_100K" | tail -1 | sed -nE 's/.*估算 ([^（]+)（.*/\1/p')"
log "10 万行：首批 ${FIRST_BATCH_MS} ms，全量 ${FULL_LOAD_MS} ms，VmRSS ${FULL_LOAD_RSS} MB"
stop_app

# 场景 3b：不滚动（背压证据：没有用户滚动就不再拉取）
LOG_NOSCROLL="$WORK/100k-noscroll.log"
start_app "$LOG_NOSCROLL" --qa-profile "$WORK/profile.json" --qa-password-env SQARVO_QA_PASSWORD \
    --qa-action save --qa-sql-file "$WORK/rows100k.sql" --qa-execute --qa-exit-after-ms 12000
wait_for "$LOG_NOSCROLL" '首批到达' 60 || fail "10 万行（不滚动）：首批未到达（见 $LOG_NOSCROLL）"
sleep 3
NOSCROLL_RSS="$(rss_mb "$APP_PID" VmRSS)"
NOSCROLL_BATCHES="$(grep -c '批次到达' "$LOG_NOSCROLL")"
NOSCROLL_ROWS="$(grep -oE '首批到达：[0-9]+ 行' "$LOG_NOSCROLL" | tail -1 | grep -oE '[0-9]+' | head -1)"
PAUSE_LINE="$(grep -m1 '未滚动到新区域所以不预取' "$LOG_NOSCROLL" || true)"
if [[ -z "$PAUSE_LINE" ]]; then
    PAUSE_LINE="$(grep -m1 '暂停预取' "$LOG_NOSCROLL" || true)"
fi
stop_app
[[ -n "$PAUSE_LINE" ]] || fail "背压：不滚动场景没有出现「暂停预取」日志（见 $LOG_NOSCROLL）"
log "不滚动 3s：已加载 ${NOSCROLL_ROWS} 行，后续批次 ${NOSCROLL_BATCHES}，暂停原因见日志"

# ---------------------------------------------------------------- 场景 4：200 列宽表

python3 - "$WORK/wide200.sql" <<'PY'
import sys
columns = ", ".join(f"seq + {i} AS c{i + 1}" for i in range(200))
sql = f"SELECT {columns} FROM seq_1_to_2000;\n"
open(sys.argv[1], "w").write(sql)
PY

LOG_WIDE="$WORK/wide200.log"
start_app "$LOG_WIDE" --qa-profile "$WORK/profile.json" --qa-password-env SQARVO_QA_PASSWORD \
    --qa-action save --qa-sql-file "$WORK/wide200.sql" --qa-execute \
    --qa-scroll-to-row 1999 --qa-exit-after-ms 180000
wait_for "$LOG_WIDE" '首批到达' 90 || fail "200 列：首批未在 90s 内到达（见 $LOG_WIDE）"
WIDE_FIRST_MS="$(ms_of "$LOG_WIDE" '首批到达')"
wait_for "$LOG_WIDE" '结果读完' 180 || fail "200 列：全量加载未在 180s 内完成（见 $LOG_WIDE）"
WIDE_LOAD_MS="$(ms_of "$LOG_WIDE" '结果读完')"
WIDE_RSS="$(rss_mb "$APP_PID" VmRSS)"
WIDE_HWM="$(rss_mb "$APP_PID" VmHWM)"
WIDE_COLUMNS="$(grep -oE '：[0-9]+ 列' "$LOG_WIDE" | tail -1 | grep -oE '[0-9]+' || true)"
WIDE_ROWS="$(grep -oE '结果读完，共 [0-9]+ 行' "$LOG_WIDE" | tail -1 | grep -oE '[0-9]+' | head -1)"
EST_WIDE="$(grep '结果集计量' "$LOG_WIDE" | tail -1 | sed -nE 's/.*估算 ([^（]+)（.*/\1/p')"
stop_app

log "200 列：全量 ${WIDE_LOAD_MS} ms，VmRSS ${WIDE_RSS} MB，列数 ${WIDE_COLUMNS}"
[[ "$WIDE_COLUMNS" == "200" ]] || fail "200 列：日志报告列数 ${WIDE_COLUMNS:-未知}，不是 200（见 $LOG_WIDE）"

# ---------------------------------------------------------------- 输出

{
cat <<EOF
# Sqarvo Linux 前端基准（$STAMP）

## 环境

| 项 | 值 |
|---|---|
| 内核 | \`$KERNEL\` |
| CPU | $CPU_MODEL（$CPU_CORES 核） |
| 内存 | $MEM_TOTAL |
| Rust | $RUSTC |
| GTK | $GTK_VERSION |
| libadwaita | $ADW_VERSION |
| 构建 | $BUILD_PROFILE（\`$BIN\`，$BIN_SIZE） |
| 结果集上限 | $RESULT_LIMIT_DESC |
| 数据库 | MariaDB $DB_HOST:$DB_PORT/$DB_NAME |
| 批次 | 500 行/批 |

## 结果

| 场景 | 指标 | 数值 |
|---|---|---|
| 冷启动（3 次） | 进程入口→Connected 中位数 | $COLD_MEDIAN ms |
| 冷启动（3 次） | 样本 | ${COLD_SAMPLES[*]} ms |
| 空闲内存 | 连接后静置 5s VmRSS | $IDLE_RSS MB |
| 空闲内存 | 连接后静置 5s VmHWM | $IDLE_HWM MB |
| 10 万行 | 首批到达（500 行） | $FIRST_BATCH_MS ms |
| 10 万行 | 前端估算字节（不含 GTK 渲染开销） | ${EST_100K:-未记录} |
| 10 万行 | 首批后 VmRSS | $FIRST_BATCH_RSS MB |
| 10 万行 | 全量加载（$ROWS_LOADED 行，$FETCH_BATCHES 个后续批次） | $FULL_LOAD_MS ms |
| 10 万行 | 全量加载后 VmRSS | $FULL_LOAD_RSS MB |
| 10 万行 | 全量加载后 VmHWM | $FULL_LOAD_HWM MB |
| 10 万行（不滚动 3s） | 已加载行数 / 后续批次 | $NOSCROLL_ROWS 行 / $NOSCROLL_BATCHES 批 |
| 10 万行（不滚动 3s） | VmRSS | $NOSCROLL_RSS MB |
| 200 列宽表（2000 行） | 首批到达 | $WIDE_FIRST_MS ms |
| 200 列宽表（2000 行） | 全量加载（$WIDE_ROWS 行 × $WIDE_COLUMNS 列） | $WIDE_LOAD_MS ms |
| 200 列宽表（2000 行） | 前端估算字节（不含 GTK 渲染开销） | ${EST_WIDE:-未记录} |
| 200 列宽表（2000 行） | 加载后 VmRSS | $WIDE_RSS MB |
| 200 列宽表（2000 行） | 加载后 VmHWM | $WIDE_HWM MB |

## 背压证据（10 万行但从不滚动）

\`\`\`text
$PAUSE_LINE
\`\`\`

## 机器可读记录

\`\`\`text
BENCH kernel=$KERNEL
BENCH cpu="$CPU_MODEL" cores=$CPU_CORES
BENCH mem_total="$MEM_TOTAL"
BENCH rustc="$RUSTC"
BENCH gtk=$GTK_VERSION
BENCH libadwaita=$ADW_VERSION
BENCH build=$BUILD_PROFILE
BENCH result_limit="$RESULT_LIMIT_DESC"
BENCH cold_start_ms_samples=${COLD_SAMPLES[*]}
BENCH cold_start_ms_median=$COLD_MEDIAN
BENCH idle_vm_rss_mb=$IDLE_RSS
BENCH idle_vm_hwm_mb=$IDLE_HWM
BENCH idle_connected_ms=$IDLE_CONNECTED_MS
BENCH rows100k_first_batch_ms=$FIRST_BATCH_MS
BENCH rows100k_first_batch_rss_mb=$FIRST_BATCH_RSS
BENCH rows100k_full_load_ms=$FULL_LOAD_MS
BENCH rows100k_full_load_rss_mb=$FULL_LOAD_RSS
BENCH rows100k_full_load_hwm_mb=$FULL_LOAD_HWM
BENCH rows100k_loaded_rows=$ROWS_LOADED
BENCH rows100k_noscroll_loaded_rows=$NOSCROLL_ROWS
BENCH rows100k_noscroll_batches=$NOSCROLL_BATCHES
BENCH rows100k_noscroll_rss_mb=$NOSCROLL_RSS
BENCH wide200_first_batch_ms=$WIDE_FIRST_MS
BENCH wide200_full_load_ms=$WIDE_LOAD_MS
BENCH wide200_rss_mb=$WIDE_RSS
BENCH wide200_hwm_mb=$WIDE_HWM
BENCH wide200_columns=$WIDE_COLUMNS
BENCH wide200_rows=$WIDE_ROWS
BENCH wide200_estimated_bytes="$EST_WIDE"
BENCH rows100k_estimated_bytes="$EST_100K"
\`\`\`
EOF
} > "$OUT"

cat "$OUT"
log "BENCH 结果已写入 $OUT"
