#!/system/bin/sh
# Start only the original Gatekeeper HAL, inside the existing private library
# namespace. No gatekeeperd, enroll, delete, credential submission or /data bind.
set -eu
export PATH=/system/bin:/system/xbin
umask 077
R=/tmp/skyward-km41
S=/tmp/skyward-ce-stage
[ "$(id -u)" = 0 ] && grep -q '^venus-entryfix-v1$' /skyward/probe-entryfix.txt
case "${1:-diagnostic}" in
    diagnostic)
        [ -f /tmp/skyward-km41-data-session.txt ] || { echo 'Retained Data session is required'; exit 1; }
        ;;
    full-recovery)
        grep -q '^venus-recovery-rw-v1$' /skyward/venus-recovery.txt
        [ ! -f /tmp/skyward-km41-data-session.txt ] || { echo 'Old RO session exists; refusing mixed modes'; exit 1; }
        ;;
    *) echo 'Unknown CE runtime mode'; exit 2;;
esac
if grep -q ' /data ' /proc/mounts; then
    echo 'Existing /data mount: refusing to recreate HAL runtime'; exit 1
fi
[ "$(getenforce)" = Permissive ] || { echo 'Gatekeeper runtime is not validated in enforcing mode'; exit 1; }
role_alive() {
    role=$1
    [ -f "$R/$role.pid" ] || return 1
    p=$(cat "$R/$role.pid")
    case "$p" in ''|*[!0-9]*) return 1;; esac
    [ "$(readlink /proc/$p/root)" = "$R/root-$role" ] || return 1
    kill -0 "$p" 2>/dev/null
}
for role in hwmanager qseecomd keymaster; do
    role_alive "$role" || { echo "Required owned service is unavailable: $role"; exit 1; }
done
[ "$(getprop hwservicemanager.ready)" = true ]
[ "$(getprop vendor.sys.listeners.registered)" = true ]
# Check as system even when reusing HALs; root access does not prove QSEE access.
"$S/km41-rpmb" --apply
mkdir "$R/ce-setup.lock" 2>/dev/null || { echo 'Gatekeeper setup is active or was interrupted'; exit 1; }
trap 'rmdir "$R/ce-setup.lock"' EXIT
if ! role_alive gatekeeper; then
    if pidof android.hardware.gatekeeper@1.0-service-qti >/dev/null 2>&1; then
        echo 'Unowned Gatekeeper service exists; refusing to replace it'; exit 1
    fi
    [ -x /tmp/skyward-km41-stage/km41-launch ]
    /system/bin/toybox nohup /tmp/skyward-km41-stage/km41-launch gatekeeper \
        </dev/null >"$R/gatekeeper.log" 2>&1 &
    printf '%s\n' "$!" >"$R/gatekeeper.pid"
fi
n=0
while [ "$n" -lt 10 ]; do
    if timeout 3 "$S/skyward" gatekeeper --hidl >"$R/gatekeeper-query.txt" 2>&1; then
        role_alive gatekeeper || { echo 'Gatekeeper exited after query'; exit 1; }
        cat "$R/gatekeeper-query.txt"
        echo 'VENUS_CE_RUNTIME_READY; credential_submitted=false'
        exit 0
    fi
    n=$((n+1)); sleep 1
done
echo 'Gatekeeper readiness query failed. No credential was submitted. Inspect the private service log.'
exit 1
