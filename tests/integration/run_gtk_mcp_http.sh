#!/usr/bin/env bash
# Run the GTK/Secret Service/Codex MCP workflow through the Linux UI runner.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
exec "$root/scripts/run-ui-tests-isolated.sh" \
    tests/ui/linux/test_06_mcp_access.py tests/ui/linux/test_07_mcp_http.py "$@"
