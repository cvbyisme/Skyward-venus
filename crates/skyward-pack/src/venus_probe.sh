#!/system/bin/sh
export PATH=/system/bin:/system/xbin
umask 077
D=/tmp/skyward-probe
mkdir -p "$D"
mark() {
    printf '<6>skyward-venus-probe: %s\n' "$*" >/dev/kmsg 2>/dev/null || :
}
mark 'VENUS_PROBE_BEGIN entryfix-v1'
exec >"$D/probe.log" 2>&1

echo 'VENUS_PROBE_BEGIN entryfix-v1'
cat /skyward/probe-entryfix.txt
id
uname -a
getprop >"$D/properties.txt"
cat /proc/cmdline >"$D/cmdline.txt"
[ ! -r /proc/bootconfig ] || cat /proc/bootconfig >"$D/bootconfig.txt"
cat /proc/mounts >"$D/mounts.txt"
cat /proc/modules >"$D/modules.txt"
cat /proc/bus/input/devices >"$D/input-devices.txt"
ls -l /dev/dri /dev/input /sys/class/backlight /sys/class/udc >"$D/nodes.txt" 2>&1

# argv[0] is skyward, NOT recovery: these do not enter recovery().
timeout 10 /system/bin/skyward --version
rc=$?
echo "SKYWARD_VERSION_RC=$rc"
mark "SKYWARD_VERSION_RC=$rc"
timeout 10 /system/bin/skyward --help >"$D/skyward-help.txt" 2>&1
rc=$?
echo "SKYWARD_HELP_RC=$rc"
mark "SKYWARD_HELP_RC=$rc"

for card in /dev/dri/card*; do
    [ -e "$card" ] || continue
    name=${card##*/}
    timeout 10 /system/bin/skyward kms --card "$card" >"$D/kms-$name.txt" 2>&1
    rc=$?
    echo "KMS_${name}_RC=$rc"
    mark "KMS_${name}_RC=$rc"
done

echo 'VENUS_PROBE_END'
echo 'No UI, installer, BCB processing, or userdata decryption was started by this script.'
mark 'VENUS_PROBE_END'
: >"$D/done"
