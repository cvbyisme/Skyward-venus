#!/system/bin/sh
export PATH=/system/bin:/system/xbin
umask 077
D=/tmp/skyward-ui-probe
mkdir -p "$D" || exit 1
# Keep one previous launch in RAM; never append across starts.
[ ! -f "$D/ui.log" ] || mv -f "$D/ui.log" "$D/ui.previous.log"
exec >"$D/ui.log" 2>&1
printf '%s\n' "$$" >"$D/pid"
mark() {
    printf '<6>skyward-venus-ui: %s\n' "$*" >/dev/kmsg 2>/dev/null || :
}
echo 'VENUS_UI_AUTOSTART_WAIT v1'
mark 'VENUS_UI_AUTOSTART_WAIT v1'
# Waiting happens in this service, not in init's action queue.
n=0
while [ "$n" -lt 45 ]; do
    if [ -f /tmp/skyward-probe/done ] && [ -c /dev/dri/card0 ]; then
        break
    fi
    n=$((n + 1))
    sleep 1
done
if [ ! -f /tmp/skyward-probe/done ] || [ ! -c /dev/dri/card0 ]; then
    echo 'VENUS_UI_AUTOSTART_NOT_READY: diagnostics/display unavailable; ADB remains independent'
    mark 'VENUS_UI_AUTOSTART_NOT_READY'
    exit 1
fi
mark 'VENUS_UI_AUTOSTART_EXEC v1'
echo 'VENUS_UI_AUTOSTART_EXEC: isolated menu; no storage/system effects'
exec /system/bin/skyward ui-probe --config /skyward/device.toml --until-stopped
