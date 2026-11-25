#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ui="$root/tests/ui/linux"

blocked() {
    printf 'UI TEST: BLOCKED\nError: %s\n' "$*" >&2
    exit 3
}

missing_dependency() {
    printf 'Install on Ubuntu/Debian: sudo apt-get install --no-install-recommends %s\n' "$2" >&2
    blocked "$1 (see tests/ui/linux/README.md)"
}

read_cgroup() {
    local hierarchy
    IFS= read -r hierarchy < /proc/self/cgroup
    printf '/sys/fs/cgroup%s\n' "${hierarchy#0::}"
}

wait_for() {
    local pid=$1 description=$2
    shift 2
    local deadline=$((SECONDS + 15))
    until "$@" >/dev/null 2>&1; do
        if ! kill -0 "$pid" 2>/dev/null || (( SECONDS >= deadline )); then
            blocked "$description did not become ready"
        fi
        sleep 0.1
    done
}

run_session() {
    # Only this session's services inherit its display and directories.
    local keyring_pid collection
    timeout 15s gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
        --method org.a11y.Bus.GetAddress >/dev/null || blocked "private AT-SPI bus unavailable"
    mkdir "$XDG_RUNTIME_DIR/keyring"
    gnome-keyring-daemon --foreground --unlock --components=secrets \
        --control-directory="$XDG_RUNTIME_DIR/keyring" <<<synthetic-sqarvo-ui-test >/dev/null &
    keyring_pid=$!
    secret_alias() {
        collection=$(timeout 2s gdbus call --session --dest org.freedesktop.secrets \
            --object-path /org/freedesktop/secrets --method org.freedesktop.Secret.Service.ReadAlias login) &&
            [[ "$collection" == *"'/org/freedesktop/secrets/collection/login'"* ]]
    }
    wait_for "$keyring_pid" "private Secret Service" secret_alias

    # Pytest also checks the AT-SPI registry before launching the app.
    export SQARVO_UI_PRIVATE_MCP_KEYRING=1
    touch "$SQARVO_UI_RUN_DIR/test-started"
    cd "${SQARVO_UI_TEST_CWD:-$SQARVO_UI_RUN_DIR}"
    "$@"
}

verify_io_weight() {
    local cg=$1 mode=$2
    [[ "$mode" != off ]] || return 0
    local available requested=$mode
    available=$(cat "$cg/cgroup.controllers") || blocked "cannot read cgroup controllers"
    if [[ "$requested" == auto ]]; then requested=20; fi
    if [[ " $available " != *" io "* ]]; then
        available=$(cat "${cg%/*}/cgroup.controllers") || blocked "cannot read slice cgroup controllers"
        [[ " $available " != *" io "* ]] || blocked "cgroup I/O controller available to the slice but inactive in the scope"
        [[ "$mode" == auto ]] || blocked "cgroup I/O controller unavailable for explicit IOWeight=$requested"
        local warning='Warning: cgroup I/O controller unavailable; IOWeight=auto continues without an I/O weight'
        printf '%s\n' "$warning"
        printf '%s\n' "$warning" >&2
        return
    fi
    local label weight
    read -r label weight < "$cg/io.weight" &&
        [[ "$label" == default && "$weight" == "$requested" ]] ||
        blocked "requested IOWeight=$requested is not active"
    printf 'io.weight=default %s\n' "$weight" >&2
}

run_display() {
    local cg
    cg=$(read_cgroup)
    [[ "$cg" == "$SQARVO_UI_CGROUP" && -f "$cg/cgroup.events" ]] ||
        blocked "worker outside its systemd scope"
    [[ ! -e "$SQARVO_UI_RUN_DIR/canceled" ]] || blocked "runner canceled during scope startup"
    local controller
    for controller in cpu.max cpu.weight memory.high memory.max; do
        [[ -r "$cg/$controller" ]] || blocked "cgroup controller unavailable: $controller"
        printf '%s=%s\n' "$controller" "$(cat "$cg/$controller")" >&2
    done
    local quota period
    read -r quota period < "$cg/cpu.max"
    local cpu_percent=${SQARVO_UI_CPU_QUOTA%\%}
    [[ "$quota" =~ ^[0-9]+$ ]] && (( quota * 100 == period * cpu_percent )) &&
        [[ $(cat "$cg/cpu.weight") == "$SQARVO_UI_CPU_WEIGHT" &&
        $(cat "$cg/memory.high") == "$(numfmt --from=iec "$SQARVO_UI_MEMORY_HIGH")" &&
        $(cat "$cg/memory.max") == "$(numfmt --from=iec "$SQARVO_UI_MEMORY_MAX")" ]] ||
        blocked "requested CPU or memory limits are not active"
    verify_io_weight "$cg" "$SQARVO_UI_IO_WEIGHT"

    local run=$SQARVO_UI_RUN_DIR
    export HOME="$run/home" TMPDIR="$run/tmp"
    export XDG_CONFIG_HOME="$run/config" XDG_DATA_HOME="$run/data"
    export XDG_CACHE_HOME="$run/cache" XDG_STATE_HOME="$run/state" XDG_RUNTIME_DIR="$run/runtime"
    export DBUS_SYSTEM_BUS_ADDRESS="unix:path=$run/no-system-bus"
    export GDK_BACKEND=x11 GSK_RENDERER=cairo GTK_A11Y=atspi GTK_USE_PORTAL=0
    export GSETTINGS_BACKEND=memory LIBGL_ALWAYS_SOFTWARE=1
    export XAUTHORITY="$run/Xauthority"
    mkdir -m 700 "$HOME" "$TMPDIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" \
        "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR" "$run/services" "$run/work"

    local cookie xvfb_pid wm_pid display_number
    cookie=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
    # Xvfb loads the cookie before displayfd tells us its server number.
    # The provisional entry is only a server-side cookie source.
    touch "$run/server.auth" "$XAUTHORITY"
    printf 'add :0 MIT-MAGIC-COOKIE-1 %s\n' "$cookie" | xauth -q -f "$run/server.auth"
    Xvfb -displayfd 3 -screen 0 1600x1000x24 -nolisten tcp -noreset -extension GLX \
        -auth "$run/server.auth" 3>"$run/display" &
    xvfb_pid=$!
    wait_for "$xvfb_pid" Xvfb test -s "$run/display"
    read -r display_number < "$run/display"
    [[ "$display_number" =~ ^[0-9]+$ ]] || blocked "invalid Xvfb display number"
    export DISPLAY=":$display_number"
    printf 'add %s MIT-MAGIC-COOKIE-1 %s\n' "$DISPLAY" "$cookie" | xauth -q
    wait_for "$xvfb_pid" Xvfb xdpyinfo
    openbox --sm-disable >&2 &
    wm_pid=$!
    wm_ready() {
        xprop -root -notype _NET_SUPPORTING_WM_CHECK | grep -Eq '0x[1-9a-fA-F][0-9a-fA-F]*$'
    }
    wait_for "$wm_pid" Openbox wm_ready

    # Restrict activation to the distro's AT-SPI launcher, without its systemd
    # activation stanza. The keyring is started explicitly on the new bus.
    grep -E '^\[D-BUS Service\]$|^Name=org\.a11y\.Bus$|^Exec=' "$SQARVO_UI_ATSPI_SERVICE" \
        > "$run/services/org.a11y.Bus.service"
    cat > "$run/session.conf" <<BUS
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>unix:path=$run/session</listen>
  <auth>EXTERNAL</auth>
  <servicedir>$run/services</servicedir>
  <policy context="default">
    <allow user="$(id -un)"/>
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <allow own="*"/>
  </policy>
</busconfig>
BUS
    exec dbus-run-session --config-file="$run/session.conf" -- "$0" --session-worker "$@"
}

# Only the outer invocation has access to the host's user systemd bus.
[[ "${BASH_SOURCE[0]}" == "$0" ]] || return 0
case "${1:-}" in
    --display-worker) shift; run_display "$@"; exit ;;
    --session-worker)
        shift
        [[ -n "${SQARVO_UI_SCOPE:-}" && "$(read_cgroup)" == */"$SQARVO_UI_SCOPE" &&
            "${DBUS_SESSION_BUS_ADDRESS:-}" == "unix:path=$SQARVO_UI_RUN_DIR/session,"* ]] ||
            blocked "session worker outside private session"
        run_session "$@"
        exit ;;
    --help)
        printf '%s\n' 'Usage: scripts/run-ui-tests-isolated.sh [pytest options or tests/ui/linux/test_*.py[::case]]' \
            '       scripts/run-ui-tests-isolated.sh --gtk-tests /absolute/test-binary [libtest args]' \
            'See tests/ui/linux/README.md for resource settings.'
        exit ;;
esac

umask 077
cd "$root"
[[ -f /sys/fs/cgroup/cgroup.controllers ]] || blocked "cgroup v2 unavailable"
for entry in systemctl:systemd systemd-run:systemd Xvfb:xvfb openbox:openbox xauth:xauth \
    xdpyinfo:x11-utils xprop:x11-utils dbus-run-session:dbus-daemon gdbus:libglib2.0-bin \
    gnome-keyring-daemon:gnome-keyring flock:util-linux grep:grep; do
    tool=${entry%%:*}
    command -v "$tool" >/dev/null || missing_dependency "missing executable: $tool" "${entry#*:}"
done
[[ -x /usr/bin/python3 ]] || missing_dependency "missing executable: /usr/bin/python3" python3
manager_group=$(systemctl --user show -p ControlGroup --value -- -.slice) ||
    blocked "systemd user manager unavailable; check systemctl --user status (see tests/ui/linux/README.md)"
[[ "$manager_group" == /* && -d "/sys/fs/cgroup$manager_group" ]] ||
    blocked "cannot verify systemd user manager cgroup"
scope_parent="/sys/fs/cgroup${manager_group%/}/app.slice"
atspi_service=
for directory in /usr/local/share /usr/share; do
    candidate="$directory/dbus-1/services/org.a11y.Bus.service"
    if [[ -r "$candidate" ]]; then atspi_service=$candidate; break; fi
done
[[ -n "$atspi_service" ]] || missing_dependency "missing org.a11y.Bus service" at-spi2-core

seconds=${SQARVO_UI_TIMEOUT:-900}
[[ "$seconds" =~ ^[1-9][0-9]{0,7}$ ]] || blocked "SQARVO_UI_TIMEOUT must be positive seconds"
cpu_quota=${SQARVO_UI_CPU_QUOTA:-200%}
cpu_weight=${SQARVO_UI_CPU_WEIGHT:-20}
memory_high=${SQARVO_UI_MEMORY_HIGH:-3G}
memory_max=${SQARVO_UI_MEMORY_MAX:-5G}
io_weight=${SQARVO_UI_IO_WEIGHT-auto}
[[ "$cpu_quota" =~ ^[1-9][0-9]*%$ && "$memory_high" =~ ^[1-9][0-9]*[KMGT]?$ &&
    "$memory_max" =~ ^[1-9][0-9]*[KMGT]?$ ]] || blocked "invalid CPU or memory limits"
valid_weight() {
    local weight=$1
    [[ "$weight" =~ ^[1-9][0-9]{0,4}$ ]] && (( weight <= 10000 )) ||
        blocked "CPU and IO weights must be between 1 and 10000"
}
valid_weight "$cpu_weight"
case "$io_weight" in
    auto) io_property=(-p IOWeight=20) ;;
    off)
        printf 'Resource limit disabled explicitly: IOWeight=off\n' >&2
        io_property=() ;;
    *) valid_weight "$io_weight"; io_property=(-p "IOWeight=$io_weight") ;;
esac

binary() {
    local candidate
    for candidate; do
        if [[ -x "$candidate" && -f "$candidate" ]]; then
            realpath -- "$candidate"
            return
        fi
    done
    return 1
}

extra_env=()
if [[ "${1:-}" == --gtk-tests ]]; then
    shift
    [[ "${1:-}" == /* && -x "$1" && -f "$1" ]] || blocked "provide an absolute GTK test binary path"
    command=("$@")
else
    python="$ui/.venv/bin/python"
    [[ -x "$python" ]] || blocked "pytest environment missing; run just ui-test-env"
    "$python" -I -c 'import gi; gi.require_version("Atspi", "2.0"); gi.require_version("Secret", "1")' ||
        missing_dependency "Python GI, AT-SPI or Secret typelib unavailable" "python3-gi gir1.2-atspi-2.0 gir1.2-secret-1"
    "$python" -I -c 'import pytest' || blocked "pytest unavailable; run just ui-test-env"
    desktop=$(binary "${SQARVO_UI_BIN:-$root/frontend/linux/target/release/sqarvo-desktop}" \
        "${SQARVO_UI_BIN:-$root/frontend/linux/target/debug/sqarvo-desktop}" \
        "${SQARVO_UI_BIN:-$HOME/.local/bin/sqarvo-desktop}") || blocked "sqarvo-desktop unavailable"
    backend=$(binary "${SQARVO_BACKEND_BIN:-${desktop%/*}/sqarvo-backend}" \
        "${SQARVO_BACKEND_BIN:-$root/backend/target/release/sqarvo-backend}" \
        "${SQARVO_BACKEND_BIN:-$root/backend/target/debug/sqarvo-backend}") ||
        blocked "sqarvo-backend unavailable"
    mcp=$(binary "${SQARVO_MCP_BIN:-${backend%/*}/sqarvo-mcp}") || mcp=
    [[ -z "${SQARVO_MCP_BIN:-}" || -n "$mcp" ]] || blocked "configured sqarvo-mcp unavailable"
    extra_env+=("SQARVO_UI_BIN=$desktop" "SQARVO_BACKEND_BIN=$backend" "SQARVO_MCP_BIN=$mcp"
        "SQARVO_UI_TEST_CWD=$ui")
    selectors=()
    for arg; do
        if [[ "${arg%%::*}" == tests/ui/linux || "${arg%%::*}" == tests/ui/linux/* ]]; then
            arg="$root/$arg"
        fi
        selectors+=("$arg")
    done
    command=("$python" -m pytest -c "$ui/pytest.ini" "${selectors[@]}")
fi

scavenger="$root/scripts/cleanup-ui-runs.py"
/usr/bin/python3 -I "$scavenger" "$scope_parent" ||
    printf 'Warning: could not inspect stale UI run directories\n' >&2
mkdir -p "$ui/results"
artifacts=$(mktemp -d "$ui/results/run.XXXXXXXX")
run=$(mktemp -d /tmp/sqarvo-ui.XXXXXXXX)
scope="${run##*/}.scope"
exec {lease}<"$run"
flock -x "$lease"
printf '%s\n' "$scope" "$scope_parent" > "$run/.sqarvo-ui-lease"
runner=
scope_requested=0
cleanup() {
    local status=$?
    trap '' INT TERM HUP
    if (( scope_requested )); then
        # A signal can arrive between the background launch and runner=$!.
        runner=${runner:-${!:-}}
        if [[ -n "$runner" ]] && jobs -p | grep -qx "$runner"; then
            # A just-registered worker checks this before creating any services.
            touch "$run/canceled"
        else
            runner=
        fi
        timeout 15s systemctl --user stop "$scope" 2>>"$artifacts/runner.log" || true
    fi
    if [[ -n "$runner" ]]; then
        # Give the launch client time to finish while it still holds its lease.
        for _ in {1..20}; do
            if ! jobs -p | grep -qx "$runner"; then
                wait "$runner" 2>/dev/null || true
                runner=
                break
            fi
            sleep .1
        done
    fi
    if (( status != 0 )); then
        for log in "$run"/work/*/app.log; do
            if [[ -f "$log" ]]; then
                cp "$log" "$artifacts/app-$(basename "$(dirname "$log")").log"
            fi
        done
        printf 'Diagnostic: %s/runner.log\n' "$artifacts" >&2
    fi
    if [[ -n "$runner" ]]; then
        printf 'Warning: retaining %s: scope launcher still running\n' "$run" >&2
    elif ! /usr/bin/python3 -I "$scavenger" "$scope_parent" "${run##*/}" "$lease" && (( status == 0 )); then
        status=1
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

scope_requested=1
# Keep a copy of the lease in the launch shell until systemd-run returns,
# even if the outer shell is killed during scope registration.
(
    status=0
    systemd-run --user --scope --quiet --expand-environment=no --slice=app.slice --unit="$scope" \
        -p "CPUQuota=$cpu_quota" -p "CPUWeight=$cpu_weight" \
        -p "MemoryHigh=$memory_high" -p "MemoryMax=$memory_max" "${io_property[@]}" \
        -p "RuntimeMaxSec=$((seconds + 15))s" -p TimeoutStopSec=5s \
        /usr/bin/env -i "PATH=$PATH" "LANG=${LANG:-C.UTF-8}" "LC_ALL=${LC_ALL:-}" \
        "SQARVO_UI_RUN_DIR=$run" "SQARVO_UI_ARTIFACTS=$artifacts" "SQARVO_UI_SCOPE=$scope" \
        "SQARVO_UI_CGROUP=$scope_parent/$scope" \
        "SQARVO_UI_ATSPI_SERVICE=$atspi_service" \
        "SQARVO_UI_CPU_WEIGHT=$cpu_weight" "SQARVO_UI_IO_WEIGHT=$io_weight" \
        "SQARVO_UI_CPU_QUOTA=$cpu_quota" "SQARVO_UI_MEMORY_HIGH=$memory_high" "SQARVO_UI_MEMORY_MAX=$memory_max" \
        "SQARVO_UI_WATCHDOG_MS=${SQARVO_UI_WATCHDOG_MS:-420000}" \
        "SQARVO_UI_READY_TIMEOUT=${SQARVO_UI_READY_TIMEOUT:-40}" \
        "${extra_env[@]}" timeout --kill-after=5s "${seconds}s" \
        "$root/scripts/run-ui-tests-isolated.sh" --display-worker "${command[@]}" \
        || status=$?
    exit "$status"
) 2>"$artifacts/runner.log" </dev/null &
runner=$!
status=0
wait "$runner" || status=$?
runner=
if (( status == 124 )); then
    printf 'UI TEST: FAIL\nError: isolation run timed out after %ss\n' "$seconds" >&2
elif [[ ! -f "$run/test-started" ]]; then
    tail -n 15 "$artifacts/runner.log" >&2
    blocked "isolation initialization failed (exit $status)"
elif (( status == 3 )); then
    tail -n 15 "$artifacts/runner.log" >&2
fi
printf 'Artifacts: %s\n' "$artifacts"
exit "$status"
