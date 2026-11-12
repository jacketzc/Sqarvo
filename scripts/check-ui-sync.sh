#!/usr/bin/env bash
# UI 组件注册表的入口：check / gen / mark。逻辑在 check-ui-sync.py（需要 python ≥ 3.11）。
# 规范见 docs/ui-specification/components/README.md，决策见 ADR 0027。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "${HERE}/check-ui-sync.py" "$@"