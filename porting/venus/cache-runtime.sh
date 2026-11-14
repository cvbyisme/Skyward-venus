#!/system/bin/sh
# Retain the installed HAL runtime in RAM before any service starts. Never
# copies userdata, metadata, credentials or RPMB; never unmounts a partition.
set -eu
export PATH=/system/bin:/system/xbin
umask 077
R=/tmp/skyward-km41
C=$R/cache
N=$R/cache.new
[ "$(id -u)" = 0 ]
grep -q '^venus-recovery-rw-v1$' /skyward/venus-recovery.txt
[ "$(stat -f -c %t /tmp)" = 1021994 ]
if grep -q ' /data ' /proc/mounts; then echo 'Cannot cache runtime with live Data'; exit 1; fi
[ ! -e "$C" ] && [ ! -L "$C" ] && [ ! -e "$N" ] && [ ! -L "$N" ] || {
    echo 'Runtime cache already exists or was interrupted; inspect before retrying'; exit 1;
}
mkdir "$N"
chmod 0700 "$N"
cache_mark() {
    read -r cache_uptime cache_idle </proc/uptime
    printf '%s boot_seconds=%s\n' "$*" "$cache_uptime"
}
cache_mark VENUS_RUNTIME_CACHE_PREFLIGHT_BEGIN
printf '%s\n' 'system system/lib64
system system/etc
system system/bin/servicemanager
system system/bin/iptables
system system/bin/ip6tables
system system/bin/ip
system system/bin/netutils-wrapper-1.0
system system/bin/ip-wrapper-1.0
system system/bin/linker64
system system/bin/bootstrap
system system/build.prop
system system/apex/com.android.tethering.capex
system_ext lib64
system_ext etc
system_ext bin/hwservicemanager
vendor lib64
vendor etc
vendor bin
vendor firmware
vendor rfs
vendor build.prop
vendor ueventd.rc
odm etc
product etc
firmware .' >"$N/entries"
: >"$N/sizes"
while read -r part relative; do
    source=/mnt/skyward-ro/$part
    [ "$part" != firmware ] || source=/vendor/firmware_mnt
    [ -e "$source/$relative" ] || [ -L "$source/$relative" ] || {
        echo "Required runtime source missing: $part/$relative"; exit 1;
    }
    # find does not follow directory symlinks. Count uncompressed regular
    # file sizes, rather than EROFS's compressed allocated block count.
    find "$source/$relative" -type f -exec stat -c %s {} + >>"$N/sizes"
done <"$N/entries"
kib=0
while read -r size; do
    case "$size" in ''|*[!0-9]*) echo 'Invalid source size'; exit 1;; esac
    # Recovery's mksh arithmetic is signed 32-bit. Keep all totals in KiB;
    # each runtime component on this ROM is below 1 GB (checked before
    # converting its individual byte count), even though the total is not.
    [ "${#size}" -le 9 ] || { echo 'A runtime component exceeds the supported RAM-copy size'; exit 1; }
    kib=$((kib+(size+1023)/1024))
done <"$N/sizes"
available=0
while read -r key value unit; do
    if [ "$key" = MemAvailable: ]; then available=$value; break; fi
done </proc/meminfo
set -- $(stat -f -c '%a %S' /tmp)
space=$(($1*($2/1024)))
[ "$kib" -gt 0 ] && [ "$available" -ge $((kib+1048576)) ] &&
    [ "$space" -ge $((kib+131072)) ] || {
    echo "Insufficient RAM: source_kib=$kib available_kib=$available tmp_free_kib=$space"; exit 1;
}
cache_mark "VENUS_RUNTIME_CACHE_BEGIN size_kib=$kib available_kib=$available"
copy_verify_part() {
    copy_part=$1
    copy_group=${2:-$copy_part}
    cache_mark "VENUS_RUNTIME_CACHE_PART_BEGIN part=$copy_part group=$copy_group"
    while read -r part relative; do
        [ "$part" = "$copy_part" ] || continue
        case "$copy_group:$relative" in
            vendor-libraries:lib64) ;;
            vendor-libraries:*) continue;;
            vendor-other:lib64) continue;;
        esac
        source=/mnt/skyward-ro/$part
        [ "$part" != firmware ] || source=/vendor/firmware_mnt
        parent=${relative%/*}
        [ "$parent" != "$relative" ] || parent=.
        # Each worker owns disjoint entries and its own manifest. Keep
        # archive metadata/symlinks, and verify both source and RAM bytes.
        (umask 022; mkdir -p "$N/$part/$parent")
        cp -a "$source/$relative" "$N/$part/$parent/"
        (cd "$source" && find "$relative" -type f -exec sha256sum {} +) >>"$N/$copy_group.sha256"
    done <"$N/entries"
    cache_mark "VENUS_RUNTIME_CACHE_PART_COPIED part=$copy_part group=$copy_group"
    (cd "$N/$copy_part" && sha256sum -c "$N/$copy_group.sha256") >"$N/verification-$copy_group.log" 2>&1
    cache_mark "VENUS_RUNTIME_CACHE_PART_READY part=$copy_part group=$copy_group"
}
# Bound concurrency to five jobs on disjoint read-only source entries.
# Vendor libraries dominate the tree; other vendor entries can complete
# independently rather than queueing behind the entire 1 GiB lib64 tree.
# The aggregate RAM budget above is unchanged. Join every worker, including
# on failure, before a ready receipt can ever be published.
copy_verify_part system &
cache_job_system=$!
copy_verify_part vendor vendor-libraries &
cache_job_vendor_libraries=$!
copy_verify_part vendor vendor-other &
cache_job_vendor_other=$!
copy_verify_part firmware &
cache_job_firmware=$!
(copy_verify_part system_ext; copy_verify_part odm; copy_verify_part product) &
cache_job_small=$!
cache_failed=0
for cache_job in "$cache_job_system" "$cache_job_vendor_libraries" "$cache_job_vendor_other" "$cache_job_firmware" "$cache_job_small"; do
    wait "$cache_job" || cache_failed=1
done
[ "$cache_failed" = 0 ] || { echo 'Runtime copy/verification failed; cache was not published'; exit 1; }
cat "$N/vendor-libraries.sha256" "$N/vendor-other.sha256" >"$N/vendor.sha256"
for part in system system_ext vendor-libraries vendor-other odm product firmware; do
    cat "$N/verification-$part.log" >>"$N/verification.log"
done
# Retain the mount target even when a ROM has no files in it.
(umask 022; mkdir -p "$N/vendor/firmware_mnt")
boot=$(cat /proc/sys/kernel/random/boot_id)
printf 'venus-runtime-cache-v1\nboot=%s\n' "$boot" >"$N/ready"
printf '%s\n' "$kib" >"$N/size-kib"
mv "$N" "$C"
cache_mark "VENUS_RUNTIME_CACHE_READY size_kib=$kib verified=sha256 source_partitions_untouched=true"
