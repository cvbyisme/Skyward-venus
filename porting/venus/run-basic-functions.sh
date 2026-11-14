#!/usr/bin/env bash
# Swap only the UI process in the current temporary Recovery. No reboot/flash.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
SERIAL="${ANDROID_SERIAL:-535e451a}"
BIN=target/aarch64-linux-android/release/skyward
adb -s "$SERIAL" shell 'grep -q "^venus-ui-autostart-v1$" /skyward/ui-autostart.txt && grep -q "^venus-entryfix-v1$" /skyward/probe-entryfix.txt'
adb -s "$SERIAL" shell '
if [ -f /tmp/skyward-basic/pid ]; then
    p=$(cat /tmp/skyward-basic/pid)
    case "$p" in ""|*[!0-9]*) exit 1;; esac
    if [ -r /proc/$p/cmdline ] && grep -q skyward-basic-v1 /proc/$p/cmdline; then
        echo "Basic menu already running: $p"; exit 1
    fi
fi'
adb -s "$SERIAL" push "$BIN" /tmp/skyward-basic-v1 >/dev/null
adb -s "$SERIAL" shell 'chmod 0755 /tmp/skyward-basic-v1; /tmp/skyward-basic-v1 ui-probe --help' | grep -q -- --basic-features
adb -s "$SERIAL" shell '
setprop ctl.stop venus_ui
n=0
while [ "$(getprop init.svc.venus_ui)" != stopped ] && [ "$n" -lt 20 ]; do
    sleep 0.2; n=$((n+1))
done
[ "$(getprop init.svc.venus_ui)" = stopped ] || exit 1
umask 077
mkdir -p /tmp/skyward-basic
[ ! -f /tmp/skyward-basic/ui.log ] || cp -p /tmp/skyward-basic/ui.log /tmp/skyward-basic/ui.previous.log
nohup /system/bin/sh -c '\''echo $$ > /tmp/skyward-basic/pid; exec /tmp/skyward-basic-v1 ui-probe --config /skyward/device.toml --until-stopped --basic-features'\'' > /tmp/skyward-basic/ui.log 2>&1 < /dev/null &
'
for _ in $(seq 1 40); do
    if adb -s "$SERIAL" shell 'grep -q UI_PROBE_FIRST_FRAME_SETTLED /tmp/skyward-basic/ui.log'; then
        echo 'Basic menu first frame confirmed; ADB retained.'
        adb -s "$SERIAL" shell 'head -n 22 /tmp/skyward-basic/ui.log; getprop init.svc.adbd'
        exit 0
    fi
    sleep 0.5
done
echo 'Basic UI did not confirm a first frame. Inspect /tmp/skyward-basic/ui.log before retrying.' >&2
# Do not restart a second competing UI if this one remains alive/hung.
adb -s "$SERIAL" shell '
p=$(cat /tmp/skyward-basic/pid 2>/dev/null)
case "$p" in ""|*[!0-9]*) setprop ctl.start venus_ui; exit 0;; esac
if [ ! -r /proc/$p/cmdline ] || ! grep -q skyward-basic-v1 /proc/$p/cmdline; then
    setprop ctl.start venus_ui
fi'
exit 1
