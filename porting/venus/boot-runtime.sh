#!/system/bin/sh
# Installed AIDL BootControl, owned by venus_security independently of the UI.
# Registration and readiness use read-only methods; no slot or BCB writes.
set -eu
export PATH=/system/bin:/system/xbin
umask 077
R=/tmp/skyward-km41
A=/skyward/runtime
NAME=android.hardware.boot.IBootControl/default
[ "$(id -u)" = 0 ]
grep -q '^venus-recovery-rw-v1$' /skyward/venus-recovery.txt
if grep -q ' /data ' /proc/mounts; then
    echo 'Refusing to create BootControl runtime with live Data'; exit 1
fi
for role in servicemanager boot; do
    [ ! -e "$R/$role.pid" ] || { echo "Existing control runtime receipt: $role"; exit 1; }
done
if pidof servicemanager >/dev/null 2>&1; then
    echo 'Existing service manager is not owned by this runtime'; exit 1
fi
# Follow this device's installed init.qti.qcv.sh mapping without executing
# its unrelated property actions or guessing a VINTF declaration.
[ "$(cat /sys/devices/soc0/soc_id)" = 415 ]
grep -q 'setprop ro.vendor.qti.soc_name lahaina' /mnt/skyward-ro/vendor/bin/init.qti.qcv.sh
[ -f /mnt/skyward-ro/vendor/etc/vintf/manifest_lahaina.xml ]
sku=$(getprop ro.boot.product.vendor.sku)
case "$sku" in
    '') setprop ro.boot.product.vendor.sku lahaina;;
    lahaina) :;;
    *) echo "Unexpected installed vendor SKU: $sku"; exit 1;;
esac
sdk=$(sed -n 's/^ro.build.version.sdk=//p' /mnt/skyward-ro/system/system/build.prop)
cr=$(printf '\r'); sdk=${sdk%"$cr"}
case "$sdk" in ''|*[!0-9]*) echo 'Invalid installed service manager SDK'; exit 1;; esac
[ "$sdk" -ge 30 ] && [ "$sdk" -le 99 ]
setprop skyward.servicemanager.sdk "$sdk"
setprop skyward.binder.domain system
# BOOT_READINESS_BEGIN
running() {
    role=$1
    [ -r "$R/$role.pid" ] || return 1
    p=$(cat "$R/$role.pid")
    case "$p" in ''|*[!0-9]*) return 1;; esac
    kill -0 "$p" 2>/dev/null
}
alive() {
    running "$1" || return 1
    [ "$(readlink /proc/$p/root)" = "$R/root-$role" ]
}
start_role() {
    role=$1
    /system/bin/toybox nohup "$A/km41-launch" "$role" </dev/null >"$R/$role.log" 2>&1 &
    printf '%s\n' "$!" >"$R/$role.pid"
}
wait_service() {
    expected=$1; wait_role=$2; n=0
    while [ "$n" -lt 20 ]; do
        running servicemanager && running "$wait_role" || { echo "Control runtime exited: $wait_role"; exit 1; }
        # A new launcher is alive before it has completed its mounts/chroot.
        # Query only after both owned processes reached their expected root.
        if alive servicemanager && alive "$wait_role" &&
            timeout 2 /system/bin/skyward service "$NAME" >"$R/control-readiness.txt" 2>&1 &&
            grep -q "$expected" "$R/control-readiness.txt"; then return 0; fi
        n=$((n+1)); sleep 1
    done
    [ ! -r "$R/control-readiness.txt" ] || cat "$R/control-readiness.txt"
    echo "Control runtime not ready: $wait_role"; exit 1
}
# BOOT_READINESS_END
start_role servicemanager
wait_service 'is described and ' servicemanager
start_role boot
wait_service 'is described and registered$' boot
timeout 5 /system/bin/skyward slot >"$R/boot-slots.txt" 2>&1
cat "$R/boot-slots.txt"
echo "VENUS_BOOTCONTROL_READY sdk=$sdk vendor_sku=lahaina domain=system registration=verified slot_write=false"
