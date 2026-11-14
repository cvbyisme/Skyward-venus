#!/system/bin/sh
# Prepare the installed radio's volatile RFS workspace and platform ABI.
# Persistent calibration is read-only. The radio's own rmt_storage is the
# sole NV writer; no backup is ever copied over live NV.
set -eu
export PATH=/system/bin:/system/xbin
umask 077
grep -qx venus-recovery-rw-v1 /skyward/venus-recovery.txt
[ "$(id -u)" = 0 ]
[ "$(stat -f -c %t /tmp)" = 1021994 ]
C=/tmp/skyward-km41/cache
A=/skyward/runtime
[ -d "$C/vendor/rfs" ]
# This platform APEX was extracted from this installed ROM's exact CAPEX.
# It must not silently be paired with another ROM's libandroid_net ABI.
echo "327c191797ac3cbfb4562df5423ae0129a68a4f02f4d91b7171a526e861b1a35  $C/system/system/apex/com.android.tethering.capex" | sha256sum -c -
echo "2113920f4b3c255773f69940ff74beef8d84681aef7bafbb6bd05d797da45714  $A/tethering.img" | sha256sum -c -
boot=$(cat /proc/sys/kernel/random/boot_id)
R=/tmp/venus-network-persist
S=/tmp/venus-network-persist-source.$$
N=/tmp/venus-network-persist.new.$$
T=/tmp/venus-tethering
RECEIPT=/tmp/venus-tethering.skyward-boot
source_id=
source_mount_id=
stage_id=
source_mounted=0
tethering_mounted=0
tethering_dir_id=
same_directory() {
    [ -d "$1" ] && [ ! -L "$1" ] && [ "$(stat -c '%d:%i' "$1")" = "$2" ]
}
mount_at() {
    found=0
    while read -r device target filesystem options rest; do
        if [ "$target" = "$1" ]; then
            [ "$found" = 0 ] || return 1
            mount_device=$device mount_filesystem=$filesystem mount_options=$options
            found=1
        fi
    done </proc/mounts
    [ "$found" = 1 ]
}
tethering_source() {
    mount_at "$T" && [ "$mount_filesystem" = ext4 ] || return 1
    block=$(readlink -f "$mount_device")
    block=${block##*/}
    case "$block" in loop[0-9]*) ;; *) return 1;; esac
    backing=$(cat "/sys/class/block/$block/loop/backing_file")
    [ "$backing" = "$A/tethering.img" ]
}
cleanup() {
    result=$?
    trap - EXIT HUP INT TERM
    if [ "$source_mounted" = 1 ]; then
        same_directory "$S" "$source_mount_id" && mount_at "$S" && [ "$mount_filesystem" = ext4 ] &&
            [ "$(readlink -f "$mount_device")" = "$(readlink -f /dev/block/by-name/persist)" ] &&
            umount "$S" || { echo 'Cannot safely release owned persist source; staging retained' >&2; exit 1; }
    fi
    if [ -n "$source_id" ]; then
        same_directory "$S" "$source_id" && rmdir "$S" || { echo 'Persist source directory identity changed' >&2; exit 1; }
    fi
    if [ -n "$stage_id" ]; then
        same_directory "$N" "$stage_id" || { echo 'RFS staging identity changed; retained' >&2; exit 1; }
        # This root-private directory is created by this invocation. Never
        # recursively remove a path that acquired an unrelated child mount.
        while read -r device target rest; do
            case "$target" in "$N"|"$N"/*) echo 'RFS staging has a mount; retained' >&2; exit 1;; esac
        done </proc/mounts
        rm -rf "$N" || exit 1
    fi
    if [ "$tethering_mounted" = 1 ]; then
        tethering_source && umount "$T" || { echo 'Cannot safely release owned tethering mount; retained' >&2; exit 1; }
    fi
    if [ -n "$tethering_dir_id" ]; then
        same_directory "$T" "$tethering_dir_id" && rmdir "$T" || exit 1
    fi
    exit "$result"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
if [ -e "$R" ] || [ -L "$R" ]; then
    [ -d "$R" ] && [ ! -L "$R" ]
    [ -f "$R/.skyward-boot" ] && [ ! -L "$R/.skyward-boot" ]
    [ "$(stat -c '%u:%g:%a' "$R/.skyward-boot")" = 0:0:600 ]
    [ "$(cat "$R/.skyward-boot")" = "$boot" ]
    [ "$(stat -c '%u:%g:%a' "$R")" = 0:2903:750 ]
else
    [ ! -e "$N" ] && [ ! -L "$N" ]
    [ ! -e "$S" ] && [ ! -L "$S" ]
    mkdir "$N"
    stage_id=$(stat -c '%d:%i' "$N")
    mkdir "$S"
    source_id=$(stat -c '%d:%i' "$S")
    mount -t ext4 -o ro,noload,nosuid,nodev,noexec /dev/block/by-name/persist "$S"
    source_mounted=1
    source_mount_id=$(stat -c '%d:%i' "$S")
    for part in rfs hlos_rfs; do
        [ -d "$S/$part" ] && [ ! -L "$S/$part" ]
        cp -a "$S/$part" "$N/"
    done
    (cd "$S" && find rfs hlos_rfs -type f -exec sha256sum {} +) >"$N/.source.sha256"
    (cd "$N" && sha256sum -c .source.sha256) >"$N/.verified"
    umount "$S"
    source_mounted=0
    same_directory "$S" "$source_id" && rmdir "$S"
    source_id=
    printf '%s\n' "$boot" >"$N/.skyward-boot"
    chgrp 2903 "$N"
    chmod 0750 "$N"
    mv "$N" "$R"
    stage_id=
fi
# The image is on the ramdisk, so this readonly loop cannot pin system.
if mount_at "$T"; then
    [ -f "$RECEIPT" ] && [ ! -L "$RECEIPT" ]
    [ "$(stat -c '%u:%g:%a' "$RECEIPT")" = 0:0:600 ]
    [ "$(cat "$RECEIPT")" = "$boot" ]
else
    [ ! -e "$T" ] && [ ! -L "$T" ] && [ ! -e "$RECEIPT" ] && [ ! -L "$RECEIPT" ]
    mkdir "$T"
    tethering_dir_id=$(stat -c '%d:%i' "$T")
    # This measured APEX image has an ext filesystem superblock/extents,
    # not EROFS. noload forbids journal replay even on a read-only mount.
    mount -t ext4 -o ro,noload,loop,nosuid,nodev "$A/tethering.img" "$T"
    tethering_mounted=1
fi
tethering_source
case ",$mount_options," in *,ro,*) ;; *) exit 1;; esac
case ",$mount_options," in *,nosuid,*) ;; *) exit 1;; esac
case ",$mount_options," in *,nodev,*) ;; *) exit 1;; esac
[ -f "$T/lib64/libcom.android.tethering.connectivity_native.so" ]
if [ "$tethering_mounted" = 1 ]; then
    # noclobber refuses an unexpected receipt instead of overwriting it.
    (set -C; printf '%s\n' "$boot" >"$RECEIPT")
    tethering_mounted=0
    tethering_dir_id=
fi
echo 'VENUS_MODEM_PREPARED persist_source_unmounted=true platform_abi=verified'
