#!/system/bin/sh
# Recreate the diagnostic HAL environment in the temporary Recovery only.
# Private mount namespaces isolate library versions, NOT a security sandbox.
# No password verification, key upgrades or data mount happens in this script.
set -eu
export PATH=/system/bin:/system/xbin
umask 077
S=/tmp/skyward-km41-stage
R=/tmp/skyward-km41
[ "$(id -u)" = 0 ] || exit 1
grep -q '^venus-entryfix-v1$' /skyward/probe-entryfix.txt || exit 1
[ "$(getenforce)" = Permissive ] || { echo 'Diagnostic HAL setup has not been validated in enforcing mode'; exit 1; }
grep -q ' /data ' /proc/mounts && { echo 'Refusing: /data is already mounted'; exit 1; }
mkdir -p "$R"
chmod 0700 "$R"
role_alive() {
    role="$1"
    [ -f "$R/$role.pid" ] || return 1
    p=$(cat "$R/$role.pid")
    case "$p" in ''|*[!0-9]*) return 1;; esac
    [ -r "/proc/$p/cmdline" ] || return 1
    [ "$(readlink /proc/$p/root)" = "$R/root-$role" ] || return 1
    kill -0 "$p" 2>/dev/null
}
if [ "${1:-start}" = stop ]; then
    for role in gatekeeper keymaster hwmanager qseecomd; do
        if role_alive "$role"; then kill -TERM "$p"; fi
    done
    echo 'Owned HAL processes were asked to stop. Binder endpoints and read-only mounts remain for inspection.'
    echo 'No system partitions or key files were changed. Reboot clears this temporary environment.'
    exit 0
fi
[ "${1:-start}" = start ] || { echo 'Use start or stop'; exit 1; }
# Refuse missing userspace tools before changing mounts or properties.
for required in id grep getenforce mkdir chmod cat readlink rmdir pidof timeout mount sha256sum cp stat chown ln getprop setprop sleep printf toybox; do
    command -v "$required" >/dev/null 2>&1 || { echo "Missing Recovery command: $required" >&2; exit 1; }
done
/system/bin/toybox nohup --help >/dev/null 2>&1 || { echo 'Missing Recovery toybox nohup' >&2; exit 1; }
mkdir "$R/setup.lock" 2>/dev/null || { echo 'Setup already active or interrupted; inspect setup.lock'; exit 1; }
trap 'rmdir "$R/setup.lock"' EXIT
# Do not restart another hardware service directory.
if role_alive keymaster; then
    role_alive hwmanager && role_alive qseecomd || { echo 'Owned Keymaster lost a dependency'; exit 1; }
    [ "$(getprop hwservicemanager.ready)" = true ] &&
        [ "$(getprop vendor.sys.listeners.registered)" = true ] || { echo 'HAL dependencies are not ready'; exit 1; }
    "$S/km41-rpmb" --apply
    timeout 10 "$S/skyward" keymaster41
    exit $?
fi
for role in keymaster hwmanager qseecomd; do
    if role_alive "$role"; then echo "Partial setup: $role remains; inspect or stop first"; exit 1; fi
done
for name in hwservicemanager android.hardware.keymaster@4.1-service-qti qseecomd; do
    if pidof "$name" >/dev/null 2>&1; then echo "Existing $name is not owned by this setup"; exit 1; fi
done
km41_mark() {
    read -r km41_uptime km41_idle </proc/uptime
    printf 'VENUS_RUNTIME_PHASE %s boot_seconds=%s\n' "$1" "$km41_uptime"
}
km41_mark SOURCE_MOUNTS_BEGIN
"$S/skyward" venus-basic status > "$R/partition-status.txt"
for part in system system_ext vendor odm product; do
    "$S/skyward" venus-basic mount --name "$part"
done
km41_mark SOURCE_MOUNTS_READY
# QSEE drops to system. Match the installed vendor RPMB rule and test
# only open/close as system before any credential can be submitted.
"$S/km41-rpmb" --apply
# These are the exact APEX payloads tested on this installed ROM. Refuse
# a ROM change instead of mixing a different bionic/VNDK into the HAL.
echo '021646695ba127260ea9cc96df6c924edfeb86ba783781a9cc276db232443fc4  /mnt/skyward-ro/system/system/apex/com.android.runtime.apex' | sha256sum -c -
echo 'eb7dd17338eae20c8ea897f878a74ff519f24481eeacf13be11b895eb8a9cc38  /mnt/skyward-ro/system_ext/apex/com.android.vndk.v30.apex' | sha256sum -c -
echo "9776b642132872ce4931218694c345231ecaac4490ead5d7c2f7c24e17481c17  $S/runtime.img" | sha256sum -c -
echo "8948745de8d27cae3fbdf23f6185ae4b6aec680b6a6febf2a2f0eaba1ab59b9e  $S/vndk30.img" | sha256sum -c -
km41_mark IMAGES_VERIFY_READY
mkdir -p /tmp/venus-runtime /tmp/venus-vndk30
if ! grep -q ' /tmp/venus-runtime ' /proc/mounts; then
    mount -t erofs -o ro,loop,nosuid,nodev "$S/runtime.img" /tmp/venus-runtime
fi
if ! grep -q ' /tmp/venus-vndk30 ' /proc/mounts; then
    mount -t ext4 -o ro,loop,noload,nosuid,nodev "$S/vndk30.img" /tmp/venus-vndk30
fi
if [ -f /skyward/venus-recovery.txt ]; then
    sh /skyward/runtime/cache-runtime.sh
    "$S/skyward" venus-firmware
fi
km41_mark FIRMWARE_READY
"$S/binderfs-setup"
cp "$S/framework-manifest.xml" "$R/framework-manifest.xml"
chmod 0644 "$R/framework-manifest.xml"
# Follow the real vendor ueventd permissions, not chmod 777.
for node in /dev/ion /dev/qseecom /dev/smcinvoke; do
    [ -c "$node" ] || { echo "Missing $node"; exit 1; }
    stat -c '%n %a %u %g' "$node" >> "$R/node-permissions-before.txt"
done
chown system:system /dev/ion
chmod 0664 /dev/ion
chown system:drmrpc /dev/qseecom /dev/smcinvoke
chmod 0660 /dev/qseecom /dev/smcinvoke
ln -sf skyward "$S/resetprop"
# KM41_PROPERTY_READER_BEGIN
# Build-property files are data, never shell input. Match the exact key,
# preserve everything after the first '=', and handle CRLF / no final LF.
# Shell read/case plus the existing printf: no awk or other text processor.
read_prop() {
    (
        [ "$#" -eq 2 ] && [ -n "$1" ] || exit 2
        [ -r "$2" ] || { printf 'Cannot read property file: %s\n' "$2" >&2; exit 2; }
        prop_key=$1
        prop_cr=$(printf '\r')
        prop_line=
        while IFS= read -r prop_line || [ -n "$prop_line" ]; do
            prop_line=${prop_line%"$prop_cr"}
            case "$prop_line" in
                "$prop_key="*)
                    printf '%s\n' "${prop_line#*=}"
                    exit 0
                    ;;
            esac
        done < "$2"
        exit 1
    )
}
# KM41_PROPERTY_READER_END
set_from_file() {
    name="$1"; file="$2"
    if ! value=$(read_prop "$name" "$file"); then
        printf 'Cannot find actual %s in %s\n' "$name" "$file" >&2
        return 1
    fi
    [ -n "$value" ] || { echo "Missing actual $name in $file"; exit 1; }
    printf '%s=%s\n' "$name" "$(getprop "$name")" >> "$R/properties-before.txt"
    "$S/resetprop" -n "$name" "$value"
}
# Only use actual installed version values. Do not synthesize patch levels.
set_from_file ro.build.version.release /mnt/skyward-ro/system/system/build.prop
set_from_file ro.build.version.security_patch /mnt/skyward-ro/system/system/build.prop
set_from_file ro.vendor.build.security_patch /mnt/skyward-ro/vendor/build.prop
setprop hwservicemanager.disabled false
setprop vendor.gatekeeper.disable_spu true
start_role() {
    role="$1"
    /system/bin/toybox nohup "$S/km41-launch" "$role" < /dev/null > "$R/$role.log" 2>&1 &
    echo "$!" > "$R/$role.pid"
}
# KM41_READINESS_BEGIN
# Poll a real producer-owned property, not a fixed sleep or a fabricated
# "ready=true". Match the venus TWRP QSEE -> Keymaster dependency.
wait_owned_property() {
    (
        wait_role=$1; wait_property=$2; wait_expected=$3; wait_limit=$4
        case "$wait_limit" in ''|*[!0-9]*) exit 2;; esac
        [ "$wait_limit" -gt 0 ] && [ "$wait_limit" -le 60 ] || exit 2
        wait_n=0
        wait_polls=$((wait_limit*10))
        while [ "$wait_n" -lt "$wait_polls" ]; do
            role_alive "$wait_role" || { echo "HAL exited before ready: $wait_role" >&2; exit 1; }
            if [ "$(getprop "$wait_property")" = "$wait_expected" ]; then
                echo "KEYMASTER_DEPENDENCY_READY role=$wait_role property=$wait_property"
                exit 0
            fi
            wait_n=$((wait_n+1))
            [ "$wait_n" -ge "$wait_polls" ] || sleep 0.1
        done
        echo "Timed out waiting for real HAL readiness: $wait_role/$wait_property" >&2
        exit 1
    )
}
# KM41_READINESS_END
# No foreign HALs are running (checked above). Invalidate stale readiness
# from a previously stopped owned process; only the real daemon sets true.
setprop hwservicemanager.ready false
setprop vendor.sys.listeners.registered false
km41_mark SECURITY_HAL_START_BEGIN
start_role hwmanager
wait_owned_property hwmanager hwservicemanager.ready true 20
start_role qseecomd
wait_owned_property qseecomd vendor.sys.listeners.registered true 20
start_role keymaster
read -r keymaster_now keymaster_idle </proc/uptime
keymaster_deadline=$((${keymaster_now%%.*}+60))
while :; do
    if timeout 5 "$S/skyward" keymaster41 > "$R/hardware.txt" 2>&1; then
        cat "$R/hardware.txt"
        echo 'KEYMASTER41_RUNTIME_READY; metadata/DE tests require explicit commands'
        exit 0
    fi
    read -r keymaster_now keymaster_idle </proc/uptime
    [ "${keymaster_now%%.*}" -lt "$keymaster_deadline" ] || break
    sleep 0.1
done
cat "$R/hardware.txt"
echo 'HAL setup did not register successfully; logs retained; data has not been opened'
exit 1
