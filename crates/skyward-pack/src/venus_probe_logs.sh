#!/system/bin/sh
export PATH=/system/bin:/system/xbin
umask 077
D=/tmp/skyward-probe
mkdir -p "$D"
mark() {
    printf '<6>skyward-venus-probe: %s\n' "$*" >/dev/kmsg 2>/dev/null || :
}
mark 'VENUS_PROBE_LOGGER_BEGIN entryfix-v1'

# Keep larger logs in RAM; rotation is limited to a few MiB.
logcat -b all -v threadtime -f "$D/logcat.txt" -r 1024 -n 2 >"$D/logcat-start.txt" 2>&1 &
LOGPID=$!
trap 'kill "$LOGPID" 2>/dev/null' EXIT

# Mount metadata only, without fsck/format/mount_all. This also lets the
# user's DEBUG Kernel exporter discover /metadata and save its early ring.
# Mount/log failure must not stop ADB or the diagnostic service.
if ! grep -q ' /metadata ' /proc/mounts; then
    n=0
    while [ "$n" -lt 5 ]; do
        [ -b /dev/block/by-name/metadata ] && break
        [ -b /dev/block/bootdevice/by-name/metadata ] && break
        n=$((n + 1))
        sleep 1
    done
    DEV=/dev/block/by-name/metadata
    [ -b "$DEV" ] || DEV=/dev/block/bootdevice/by-name/metadata
    if [ -b "$DEV" ]; then
        timeout 5 mount -t ext4 -o nosuid,nodev,noatime "$DEV" /metadata >"$D/metadata-mount.txt" 2>&1
        mark "METADATA_MOUNT_RC=$?"
    else
        echo 'No metadata block device; RAM logs only.' >"$D/metadata-mount.txt"
        mark 'METADATA_DEVICE_MISSING'
    fi
fi

P=
if grep -q ' /metadata ' /proc/mounts && [ ! -L /metadata/skyward-probe ]; then
    if mkdir -p /metadata/skyward-probe && chmod 0700 /metadata/skyward-probe; then
        P=/metadata/skyward-probe
        # Keep one bounded previous boot so the next temporary image does
        # not overwrite the only evidence from a boot with no working USB.
        for name in probe.log nodes.txt usb-state.txt; do
            if [ -f "$P/last-$name" ] && [ ! -L "$P/last-$name" ] &&
                [ ! -L "$P/previous-$name" ]; then
                head -c 131072 "$P/last-$name" >"$P/previous-$name"
            fi
        done
        printf 'entryfix-v1 logger started\n' >"$P/last-start.txt"
        mark 'METADATA_READY'
    fi
fi

# Do not duplicate the multi-MiB kernel log onto the small metadata partition.
# Persist only a bounded probe summary (<=128 KiB) and USB/input nodes (<=32 KiB).
# Files are fixed names so repeated probes do not add unbounded directories.
SAVED=0
BOOT_ID=$(cat /proc/sys/kernel/random/boot_id)
LAST_USB=
usb_snapshot() {
    controller=$(getprop sys.usb.controller)
    case "$controller" in
        ''|*[!a-zA-Z0-9._-]*) hardware=unknown;;
        *) hardware=$(cat "/sys/class/udc/$controller/state" 2>/dev/null);;
    esac
    bound=$(cat /config/usb_gadget/g1/UDC 2>/dev/null)
    f1=$(readlink /config/usb_gadget/g1/configs/b.1/f1 2>/dev/null)
    f2=$(readlink /config/usb_gadget/g1/configs/b.1/f2 2>/dev/null)
    # No credentials, SSIDs or subscriber identity are queried.
    printf 'controller=%s bound=%s hardware=%s f1=%s f2=%s\n' "$controller" "$bound" "$hardware" "$f1" "$f2"
    for key in sys.usb.config sys.usb.state sys.usb.configfs sys.usb.config_disable sys.usb.ffs.ready sys.usb.ffs.mtp.ready init.svc.adbd init.svc.fastbootd; do
        printf '%s=%s\n' "$key" "$(getprop "$key")"
    done
}
while :; do
    USB=$(usb_snapshot)
    if [ "$USB" != "$LAST_USB" ]; then
        read -r USB_UPTIME USB_IDLE </proc/uptime
        {
            printf 'boot=%s boot_seconds=%s\n' "$BOOT_ID" "$USB_UPTIME"
            printf '%s\n' "$USB"
        } >"$D/usb-state.txt"
        if [ -n "$P" ] && [ ! -L "$P/last-usb-state.txt" ]; then
            cp "$D/usb-state.txt" "$P/last-usb-state.tmp" &&
                mv "$P/last-usb-state.tmp" "$P/last-usb-state.txt"
        fi
        mark "VENUS_USB_STATE boot_seconds=$USB_UPTIME $USB"
        LAST_USB=$USB
    fi
    if dmesg >"$D/kernel.tmp" 2>"$D/dmesg-error.txt"; then
        mv "$D/kernel.tmp" "$D/kernel.txt"
    fi
    if [ "$SAVED" = 0 ] && [ -n "$P" ] && [ -f "$D/done" ]; then
        head -c 131072 "$D/probe.log" >"$P/last-probe.tmp" && mv "$P/last-probe.tmp" "$P/last-probe.log"
        head -c 32768 "$D/nodes.txt" >"$P/last-nodes.tmp" && mv "$P/last-nodes.tmp" "$P/last-nodes.txt"
        sync
        SAVED=1
        mark 'PROBE_SUMMARY_SAVED'
    fi
    sleep 2
done
