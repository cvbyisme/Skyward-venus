#!/system/bin/sh
# Self-contained HAL setup used by the integrated Recovery decrypt worker.
# No host adb, user credential, formatting, key upgrades or implicit unmount.
set -eu
export PATH=/system/bin:/system/xbin
umask 077
R=/tmp/skyward-km41
D=/tmp/skyward-recovery
A=/skyward/runtime
mkdir -p "$D"
chmod 0700 "$D"
grep -q '^venus-recovery-rw-v1$' /skyward/venus-recovery.txt
[ "$(id -u)" = 0 ]
role_alive() {
    role=$1
    [ -r "$R/$role.pid" ] || return 1
    p=$(cat "$R/$role.pid")
    case "$p" in ''|*[!0-9]*) return 1;; esac
    [ "$(readlink /proc/$p/root)" = "$R/root-$role" ] || return 1
    kill -0 "$p" 2>/dev/null
}
if [ -f "$D/runtime-ready" ]; then
    [ "$(cat "$D/runtime-ready")" = venus-runtime-ready-v2 ] || {
        echo 'An older runtime remains; boot the current integrated image without restarting live HALs'; exit 1;
    }
    for role in hwmanager qseecomd keymaster gatekeeper servicemanager boot; do
        role_alive "$role" || { echo "HAL exited: $role; no automatic restart on live Data"; exit 1; }
    done
    [ "$(getprop hwservicemanager.ready)" = true ]
    [ "$(getprop vendor.sys.listeners.registered)" = true ]
    [ "$(getprop skyward.binder.domain)" = system ]
    "$A/km41-rpmb" --check
    exit 0
fi
# Never change a previous diagnostic session into RW behind its owner's back.
[ ! -f /tmp/skyward-km41-data-session.txt ] || { echo 'Existing RO diagnostics: boot this integrated image afresh'; exit 1; }
if grep -q ' /data ' /proc/mounts; then
    echo 'Existing /data mount: refusing to recreate HAL runtime'; exit 1
fi
mkdir "$D/setup.lock" 2>/dev/null || { echo 'HAL setup already running or interrupted'; exit 1; }
trap 'rmdir "$D/setup.lock"' EXIT
exec >>"$D/runtime.log" 2>&1
runtime_mark() {
    read -r runtime_uptime runtime_idle </proc/uptime
    printf 'VENUS_RUNTIME_PHASE %s boot_seconds=%s\n' "$1" "$runtime_uptime"
}
runtime_mark ASSETS_VERIFY_BEGIN
(cd "$A" && sha256sum -c SHA256SUMS)
runtime_mark ASSETS_VERIFY_READY
for stage in /tmp/skyward-km41-stage /tmp/skyward-ce-stage; do
    mkdir -p "$stage"
    chmod 0700 "$stage"
    for name in binderfs-setup km41-launch km41-rpmb km41-kmsg runtime.img vndk30.img framework-manifest.xml km41-runtime.sh ce-runtime.sh; do
        # /tmp is a fresh ramdisk in a cold boot; refuse foreign regular files.
        if [ -e "$stage/$name" ] || [ -L "$stage/$name" ]; then
            [ -L "$stage/$name" ] && [ "$(readlink "$stage/$name")" = "$A/$name" ] || { echo "Foreign runtime asset: $name"; exit 1; }
        else
            ln -s "$A/$name" "$stage/$name"
        fi
    done
    if [ -e "$stage/skyward" ] || [ -L "$stage/skyward" ]; then
        [ -L "$stage/skyward" ] && [ "$(readlink "$stage/skyward")" = /system/bin/skyward ] || { echo 'Foreign Skyward runtime'; exit 1; }
    else
        ln -s /system/bin/skyward "$stage/skyward"
    fi
done
sh /tmp/skyward-km41-stage/km41-runtime.sh
runtime_mark KEYMASTER_READY
sh /tmp/skyward-ce-stage/ce-runtime.sh full-recovery
runtime_mark GATEKEEPER_READY
sh "$A/boot-runtime.sh"
runtime_mark BOOTCONTROL_READY
printf '%s\n' 'venus-runtime-ready-v2' >"$D/runtime-ready"
echo 'VENUS_RECOVERY_RUNTIME_READY; Data opening is a separate worker step'
