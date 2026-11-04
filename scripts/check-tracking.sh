#!/usr/bin/env bash
# 校验 Issue 是否符合仓库的任务跟踪约定（AGENTS.md §14、docs/planning/README.md）。
#
# 硬失败：
#   - Issue 未挂里程碑；
#   - 里程碑名不在 docs/planning/milestones.md 的表格中（文档与 GitHub 不一致）；
#   - Issue 未加入 Project 看板。
# 警告：
#   - 缺少优先级标签（P0/P1/P2）；
#   - 缺少模块标签（除优先级与 task 外的标签）。
#
# 依赖：gh（仓库读权限；看板检查还需 project 作用域）。本地使用当前 gh 登录身份，
# CI 中使用具有 Projects: read 权限的 PAT（secret 名 PROJECT_TOKEN）。
set -euo pipefail

REPO=""
PROJECT_OWNER="${TRACKING_PROJECT_OWNER:-jacketzc}"
PROJECT_NUMBER="${TRACKING_PROJECT_NUMBER:-2}"
MILESTONES_FILE="${TRACKING_MILESTONES_FILE:-docs/planning/milestones.md}"
INPUT=""
STATE="open"
WARN_ONLY=0
REQUIRE_PROJECT="${TRACKING_REQUIRE_PROJECT:-0}"

usage() {
  cat <<'USAGE'
用法：scripts/check-tracking.sh [选项]

  --repo <owner/name>     目标仓库（默认取 GITHUB_REPOSITORY 或 gh repo view）
  --owner <login>         Project 所有者（默认 jacketzc）
  --project <number>      Project 编号（默认 2）
  --milestones-file <p>   里程碑文档路径（默认 docs/planning/milestones.md）
  --input <file>          用 JSON 夹具代替 gh 查询（自测用，形如 gh issue list --json 输出）
  --state <open|all>      检查范围（默认 open）
  --require-project       无法读取 Project 时视为失败（CI 配置了 PROJECT_TOKEN 时使用）
  --warn-only             有硬失败时仍返回 0
  -h, --help              显示本帮助
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --repo) REPO="${2:?--repo 需要值}"; shift 2 ;;
    --owner) PROJECT_OWNER="${2:?--owner 需要值}"; shift 2 ;;
    --project) PROJECT_NUMBER="${2:?--project 需要值}"; shift 2 ;;
    --milestones-file) MILESTONES_FILE="${2:?--milestones-file 需要值}"; shift 2 ;;
    --input) INPUT="${2:?--input 需要值}"; shift 2 ;;
    --state) STATE="${2:?--state 需要值}"; shift 2 ;;
    --require-project) REQUIRE_PROJECT=1; shift ;;
    --warn-only) WARN_ONLY=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) echo "未知参数：$1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ -z "$REPO" ]; then
  if [ -n "${GITHUB_REPOSITORY:-}" ]; then
    REPO="$GITHUB_REPOSITORY"
  else
    REPO="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
  fi
fi

TMP="$(mktemp)"
PROJ_TMP="$(mktemp)"
trap 'rm -f "$TMP" "$PROJ_TMP"' EXIT

# 采集 Issue：gh api 的 /issues 会混入 PR，需要剔除。
if [ -n "$INPUT" ]; then
  jq '.' "$INPUT" >"$TMP"
else
  gh api --paginate "repos/$REPO/issues?state=$STATE&per_page=100" |
    jq -s 'add // [] | map(select(has("pull_request") | not))' >"$TMP"
fi
COUNT="$(jq 'length' "$TMP")"

# 里程碑白名单来自文档表格：| M2 首个驱动切片 | ... |
DOC_MILESTONES=""
if [ -f "$MILESTONES_FILE" ]; then
  DOC_MILESTONES="$(grep -E '^\|[[:space:]]*M[0-9]+[[:space:]]' "$MILESTONES_FILE" |
    awk -F'|' '{gsub(/^[[:space:]]+|[[:space:]]+$/,"",$2); print $2}' || true)"
else
  echo "! 未找到 $MILESTONES_FILE，跳过里程碑白名单校验" >&2
fi

# 看板成员：需要 project 作用域，缺失时按配置降级为跳过或失败。
MEMBERS=""
PROJECT_OK=0
PROJECT_ERR="无法读取 Project #$PROJECT_NUMBER（owner $PROJECT_OWNER），token 缺少 project 作用域"
PROJECT_SKIP=""
if [ -n "$INPUT" ] && [ "$REQUIRE_PROJECT" != "1" ]; then
  PROJECT_SKIP="使用 --input 夹具，跳过看板成员检查"
elif gh project item-list "$PROJECT_NUMBER" --owner "$PROJECT_OWNER" \
  --format json --limit 1000 >"$PROJ_TMP" 2>/dev/null; then
  PROJECT_OK=1
  MEMBERS="$(jq -r '.items[] | .content.number? // empty' "$PROJ_TMP")"
fi

is_ci() { [ "${GITHUB_ACTIONS:-}" = "true" ]; }

FAILS=0
WARNS=0
REPORT=""

add_fail() {
  FAILS=$((FAILS + 1))
  if is_ci; then REPORT+="::error file=$MILESTONES_FILE::$1"$'\n'; else REPORT+="✗ $1"$'\n'; fi
}
add_warn() {
  WARNS=$((WARNS + 1))
  if is_ci; then REPORT+="::warning::$1"$'\n'; else REPORT+="! $1"$'\n'; fi
}

if [ "$PROJECT_OK" != "1" ] && [ "$REQUIRE_PROJECT" = "1" ]; then
  add_fail "$PROJECT_ERR"
fi

while IFS=$'\x1f' read -r NUM TITLE MS LABELS; do
  [ -n "$NUM" ] || continue
  REF="#$NUM $TITLE"

  if [ -z "$MS" ]; then
    add_fail "$REF：缺少里程碑（须挂 milestones.md 中的 Mx）"
  elif [ -n "$DOC_MILESTONES" ] && ! printf '%s\n' "$DOC_MILESTONES" | grep -Fxq "$MS"; then
    add_fail "$REF：里程碑「$MS」不在 $MILESTONES_FILE 中"
  fi

  if [ "$PROJECT_OK" = "1" ] && ! printf '%s\n' "$MEMBERS" | grep -Fxq "$NUM"; then
    add_fail "$REF：未加入 Project #$PROJECT_NUMBER"
  fi

  if ! printf '%s' "$LABELS" | tr ',' '\n' | grep -Eq '^P[012]$'; then
    add_warn "$REF：缺少优先级标签 P0/P1/P2"
  fi

  if [ -z "$(printf '%s' "$LABELS" | tr ',' '\n' | grep -Ev '^(P[012]|task)$' || true)" ]; then
    add_warn "$REF：缺少模块标签（如 sqlite / proto / backend）"
  fi
done < <(jq -r '.[] | [
    (.number | tostring),
    (.title // "" | gsub("[\\t\\n]"; " ")),
    (.milestone.title // ""),
    ([.labels[].name] | join(","))
  ] | map(tostring) | join("\u001f")' "$TMP")

if [ "$PROJECT_OK" != "1" ] && [ "$REQUIRE_PROJECT" != "1" ]; then
  if [ -n "$PROJECT_SKIP" ]; then
    REPORT+="! $PROJECT_SKIP"$'\n'
  else
    REPORT+="! $PROJECT_ERR，跳过看板检查（本地可用 gh auth login --scopes project 授权）"$'\n'
  fi
fi

printf '%s' "$REPORT"
echo "检查 $REPO 的 $COUNT 个 Issue（state=$STATE）：失败 $FAILS 项，警告 $WARNS 项。"

if [ "$FAILS" -gt 0 ] && [ "$WARN_ONLY" = "0" ]; then
  exit 1
fi
exit 0
