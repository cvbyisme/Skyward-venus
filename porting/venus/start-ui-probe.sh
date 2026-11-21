#!/usr/bin/env bash
# Deploy to RAM and launch the isolated menus. No flash/reboot/mount commands.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
SERIAL="${ANDROID_SERIAL:-535e451a}"
SECONDS_TO_RUN="${1:-900}"
[[ "$SECONDS_TO_RUN" =~ ^[0-9]+$ ]] && ((SECONDS_TO_RUN >= 1 && SECONDS_TO_RUN <= 1800)) || {
    echo 'Duration must be 1..1800 seconds' >&2; exit 1;
}
BINARY=target/aarch64-linux-android/release/skyward
CONFIG=porting/venus/ui-probe.toml
test -s "$BINARY" && test -s "$CONFIG"
adb -s "$SERIAL" shell 'test -f /skyward/probe-entryfix.txt && grep -q "^venus-entryfix-v1$" /skyward/probe-entryfix.txt'
# Refuse a second copy; don't kill an existing user test or overwrite its ELF.
adb -s "$SERIAL" shell '
    if [ -r /skyward/run/logs/ui-probe/pid ]; then
        p=$(cat /skyward/run/logs/ui-probe/pid)
        case "$p" in ""|*[!0-9]*) exit 1;; esac
        if [ -r /proc/$p/cmdline ] && grep -q skyward-ui-probe-v1 /proc/$p/cmdline; then
            echo "UI probe already running: $p"; exit 1
        fi
    fi
    if pidof venus-panel-probe >/dev/null; then
        echo "Display-only test is still running"; exit 1
    fi
'
adb -s "$SERIAL" push "$BINARY" /skyward/run/logs/ui-probe-v1
adb -s "$SERIAL" push "$CONFIG" /skyward/run/tmp/venus-ui-probe.toml
adb -s "$SERIAL" shell "
    umask 077
    mkdir -p /skyward/run/logs/ui-probe
    chmod 0700 /skyward/run/logs/ui-probe-v1
    nohup /skyward/run/logs/ui-probe-v1 ui-probe --config /skyward/run/tmp/venus-ui-probe.toml --seconds '$SECONDS_TO_RUN' \
        </dev/null >/skyward/run/logs/ui-probe/ui.log 2>&1 &
    echo \$! >/skyward/run/logs/ui-probe/pid
"
printf 'Menus started for at most %s seconds. Log: /skyward/run/logs/ui-probe/ui.log\n' "$SECONDS_TO_RUN"
