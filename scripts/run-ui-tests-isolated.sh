#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ui="$root/tests/ui/linux"

blocked() {
    printf 'UI TEST: BLOCKED\nError: %s\n' "$*" >&2
    exit 3
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

run_display() {
    local cg
    cg=$(read_cgroup)
    [[ "${cg##*/}" == "$SQARVO_UI_SCOPE" && -f "$cg/cgroup.events" ]] ||
        blocked "worker outside its systemd scope"
    printf '%s\n' "$cg" > "$SQARVO_UI_RUN_DIR/cgroup"
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
    if [[ "$SQARVO_UI_IO_WEIGHT" != off ]]; then
        [[ -r "$cg/io.weight" ]] || blocked "cgroup controller unavailable: io.weight"
        local label weight
        read -r label weight < "$cg/io.weight"
        [[ "$label" == default && "$weight" == "$SQARVO_UI_IO_WEIGHT" ]] ||
            blocked "requested IOWeight is not active"
        printf 'io.weight=default %s\n' "$weight" >&2
    fi

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
    awk '/^\[D-BUS Service\]$|^Name=org\.a11y\.Bus$|^Exec=/' "$SQARVO_UI_ATSPI_SERVICE" \
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
for tool in systemctl systemd-run Xvfb xauth xdpyinfo openbox xprop dbus-run-session gdbus gnome-keyring-daemon; do
    command -v "$tool" >/dev/null || blocked "missing executable: $tool"
done
systemctl --user show -p ControlGroup user.slice >/dev/null ||
    blocked "systemd user manager unavailable"
atspi_service=
for directory in /usr/local/share /usr/share; do
    candidate="$directory/dbus-1/services/org.a11y.Bus.service"
    if [[ -r "$candidate" ]]; then atspi_service=$candidate; break; fi
done
[[ -n "$atspi_service" ]] || blocked "missing org.a11y.Bus service"

seconds=${SQARVO_UI_TIMEOUT:-900}
[[ "$seconds" =~ ^[1-9][0-9]{0,7}$ ]] || blocked "SQARVO_UI_TIMEOUT must be positive seconds"
cpu_quota=${SQARVO_UI_CPU_QUOTA:-200%}
cpu_weight=${SQARVO_UI_CPU_WEIGHT:-20}
memory_high=${SQARVO_UI_MEMORY_HIGH:-3G}
memory_max=${SQARVO_UI_MEMORY_MAX:-5G}
io_weight=${SQARVO_UI_IO_WEIGHT:-20}
[[ "$cpu_quota" =~ ^[1-9][0-9]*%$ && "$memory_high" =~ ^[1-9][0-9]*[KMGT]?$ &&
    "$memory_max" =~ ^[1-9][0-9]*[KMGT]?$ ]] || blocked "invalid CPU or memory limits"
io_property=()
if [[ "$io_weight" == off ]]; then
    printf 'Resource limit disabled explicitly: IOWeight=off\n' >&2
    weights=("$cpu_weight")
else
    io_property=(-p "IOWeight=$io_weight")
    weights=("$cpu_weight" "$io_weight")
fi
for weight in "${weights[@]}"; do
    [[ "$weight" =~ ^[1-9][0-9]{0,4}$ ]] && (( weight <= 10000 )) ||
        blocked "CPU and IO weights must be between 1 and 10000"
done

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
    "$python" -I -c 'import gi, pytest; gi.require_version("Atspi", "2.0"); gi.require_version("Secret", "1")' ||
        blocked "pytest, AT-SPI or Secret typelib unavailable"
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

mkdir -p "$ui/results"
artifacts=$(mktemp -d "$ui/results/run.XXXXXXXX")
run=$(mktemp -d /tmp/sqarvo-ui.XXXXXXXX)
scope="${run##*/}.scope"
runner=
scope_requested=0
cleanup() {
    local status=$?
    trap '' INT TERM HUP
    if [[ -n "$runner" ]]; then
        # Stop the launch client first so a late launch cannot outlive cleanup.
        kill -TERM "$runner" 2>/dev/null || true
        wait "$runner" 2>/dev/null || true
    fi
    if (( scope_requested )); then
        timeout 15s systemctl --user stop "$scope" 2>>"$artifacts/runner.log" || true
        local cg
        cg=$(cat "$run/cgroup" 2>/dev/null || true)
        if [[ -n "$cg" && -r "$cg/cgroup.events" ]] && ! grep -qx 'populated 0' "$cg/cgroup.events"; then
            printf 'UI TEST: FAIL\nError: test scope still populated: %s\n' "$scope" >&2
            status=1
        fi
    fi
    if (( status != 0 )); then
        for log in "$run"/work/*/app.log; do
            if [[ -f "$log" ]]; then
                cp "$log" "$artifacts/app-$(basename "$(dirname "$log")").log"
            fi
        done
        printf 'Diagnostic: %s/runner.log\n' "$artifacts" >&2
    fi
    rm -rf -- "$run"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

scope_requested=1
systemd-run --user --scope --quiet --expand-environment=no --unit="$scope" \
    -p "CPUQuota=$cpu_quota" -p "CPUWeight=$cpu_weight" \
    -p "MemoryHigh=$memory_high" -p "MemoryMax=$memory_max" "${io_property[@]}" \
    -p "RuntimeMaxSec=$((seconds + 15))s" -p TimeoutStopSec=5s \
    /usr/bin/env -i "PATH=$PATH" "LANG=${LANG:-C.UTF-8}" "LC_ALL=${LC_ALL:-}" \
    "SQARVO_UI_RUN_DIR=$run" "SQARVO_UI_ARTIFACTS=$artifacts" "SQARVO_UI_SCOPE=$scope" \
    "SQARVO_UI_ATSPI_SERVICE=$atspi_service" \
    "SQARVO_UI_CPU_WEIGHT=$cpu_weight" "SQARVO_UI_IO_WEIGHT=$io_weight" \
    "SQARVO_UI_CPU_QUOTA=$cpu_quota" "SQARVO_UI_MEMORY_HIGH=$memory_high" "SQARVO_UI_MEMORY_MAX=$memory_max" \
    "SQARVO_UI_WATCHDOG_MS=${SQARVO_UI_WATCHDOG_MS:-420000}" \
    "SQARVO_UI_READY_TIMEOUT=${SQARVO_UI_READY_TIMEOUT:-40}" \
    "${extra_env[@]}" timeout --kill-after=5s "${seconds}s" \
    "$root/scripts/run-ui-tests-isolated.sh" --display-worker "${command[@]}" \
    2>"$artifacts/runner.log" </dev/null &
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
