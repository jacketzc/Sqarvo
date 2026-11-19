#!/usr/bin/env bash
# Run the actual GTK/Secret Service/Codex MCP workflow with isolated state.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"
base=$(mktemp -d "${TMPDIR:-/tmp}/sqarvo-mcp-ui.XXXXXX")
trap 'rm -rf "$base"' EXIT
mkdir -m 700 "$base/data" "$base/config" "$base/control"
if [[ "$WAYLAND_DISPLAY" != /* ]]; then
    export WAYLAND_DISPLAY="$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY"
fi
export XDG_RUNTIME_DIR="$base" XDG_DATA_HOME="$base/data" XDG_CONFIG_HOME="$base/config"
export GDK_BACKEND=wayland
export SQARVO_UI_BIN="${SQARVO_UI_BIN:-$root/frontend/linux/target/debug/sqarvo-desktop}"
export SQARVO_BACKEND_BIN="${SQARVO_BACKEND_BIN:-$root/backend/target/debug/sqarvo-backend}"
export SQARVO_MCP_BIN="${SQARVO_MCP_BIN:-$root/backend/target/debug/sqarvo-mcp}"
export SQARVO_UI_PRIVATE_MCP_KEYRING=1
config=(--config-file="${SQARVO_QA_DBUS_CONFIG:-/usr/share/dbus-1/session.conf}")
dbus-run-session "${config[@]}" -- bash -c '
    set -euo pipefail
    export AT_SPI_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"
    printf "%s" "synthetic-sqarvo-mcp-test" | gnome-keyring-daemon --unlock --components=secrets --control-directory="$XDG_RUNTIME_DIR/control" >/dev/null
    /usr/libexec/at-spi2-registryd &
    registry=$!
    trap '\''kill "$registry" 2>/dev/null || true'\'' EXIT
    tests/ui/linux/.venv/bin/python -m pytest -c tests/ui/linux/pytest.ini -q "$@"
' _ tests/ui/linux/test_06_mcp_access.py tests/ui/linux/test_07_mcp_http.py "$@"
