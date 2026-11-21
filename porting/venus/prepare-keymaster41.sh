#!/usr/bin/env bash
# Explicit diagnostics only. Never reboot, flash, guess credentials or repair data.
set -euo pipefail
umask 077
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
SERIAL="${ANDROID_SERIAL:-535e451a}"
MODE="${1:---hardware}"
case "$MODE" in
    --hardware|--metadata|--data-ro-keep|--de-keep) ;;
    --data-ro|--de)
        echo '旧入口会自动卸载 Data，已停用。保留挂载使用 --data-ro-keep；不要重复卸载测试。' >&2
        exit 2;;
    *) echo 'Use --hardware, --metadata, --data-ro-keep or --de-keep' >&2; exit 2;;
esac
OUT="$(cat target/venus-keymaster41-latest.txt)"
LOG=''
KLOG_PID=''
stop_capture() {
    if [[ -n "$KLOG_PID" ]]; then
        kill -TERM "$KLOG_PID" 2>/dev/null || true
        wait "$KLOG_PID" 2>/dev/null || true
        KLOG_PID=''
    fi
}
on_exit() {
    rc=$?
    trap - EXIT
    stop_capture
    if (( rc != 0 )); then
        printf '诊断未完整通过（退出码 %s），不要继续 DE/CE。\n' "$rc" >&2
        [[ -z "$LOG" ]] || printf '本轮日志：%s\n' "$LOG" >&2
    fi
    exit "$rc"
}
trap on_exit EXIT
(cd "$OUT" && sha256sum -c ASSETS.sha256)
for tool in adb timeout python3; do command -v "$tool" >/dev/null || { echo "Missing host tool: $tool" >&2; exit 1; }; done
# Refuse normal Android before staging anything. Never unmount a live /data.
if ! timeout 10 adb -s "$SERIAL" shell 'test "$(id -u)" = 0 && grep -q "^venus-entryfix-v1$" /skyward/probe-entryfix.txt && ! grep -q " /data " /proc/mounts'; then
    echo '当前不是可测试的 venus Recovery，或 /data 已在使用；未部署、未执行解密。' >&2
    exit 1
fi
test -s "$OUT/assets/km41-kmsg" || { echo 'Build the updated Keymaster assets first' >&2; exit 1; }
BOOT_ID="$(timeout 5 adb -s "$SERIAL" shell cat /proc/sys/kernel/random/boot_id | tr -d '\r')"
LOG="$(mktemp -d "$HOME/SKYWARD/venus_dump/keymaster41-retry.XXXXXX")"
echo "本轮日志：$LOG"
printf '%s\n' "$BOOT_ID" > "$LOG/boot-id-before.txt"
adb -s "$SERIAL" shell 'mkdir -p /skyward/run/hal/stage-km41; chmod 0700 /skyward/run/hal/stage-km41'
for name in skyward binderfs-setup km41-launch km41-kmsg km41-rpmb runtime.img vndk30.img framework-manifest.xml km41-runtime; do
    adb -s "$SERIAL" push "$OUT/assets/$name" "/skyward/run/hal/stage-km41/$name.new" >/dev/null
    adb -s "$SERIAL" shell "mv /skyward/run/hal/stage-km41/$name.new /skyward/run/hal/stage-km41/$name"
done
adb -s "$SERIAL" shell 'chmod 0700 /skyward/run/hal/stage-km41/skyward /skyward/run/hal/stage-km41/binderfs-setup /skyward/run/hal/stage-km41/km41-launch /skyward/run/hal/stage-km41/km41-kmsg /skyward/run/hal/stage-km41/km41-rpmb /skyward/run/hal/stage-km41/km41-runtime'
# Stream directly to Ubuntu before any operation. No dependence on a later
# successful Android boot or the kernel's existing Metadata ring snapshots.
# This child is bounded to 300 seconds and stopped by the EXIT trap.
adb -s "$SERIAL" exec-out /skyward/run/hal/stage-km41/km41-kmsg > "$LOG/kernel-live.txt" 2> "$LOG/kernel-capture-error.txt" &
KLOG_PID=$!
ready=0
for ((i=0; i<60; i++)); do
    if grep -q '^KM41_KMSG_CAPTURE_READY ' "$LOG/kernel-live.txt"; then ready=1; break; fi
    kill -0 "$KLOG_PID" 2>/dev/null || break
    sleep 0.1
done
[[ "$ready" = 1 ]] || { echo '内核实时日志未就绪，未运行解密测试。' >&2; exit 1; }
adb -s "$SERIAL" shell '/skyward/run/hal/stage-km41/km41-runtime start' 2>&1 | tee "$LOG/runtime.txt"
kill -0 "$KLOG_PID" 2>/dev/null || { echo 'Kernel capture ended before the test; refusing data operation' >&2; exit 1; }
ARGS='keymaster41'
case "$MODE" in
    --metadata) ARGS+=' --metadata';;
    --data-ro-keep) ARGS+=' --metadata --data-ro --keep-data-ro';;
    --de-keep) ARGS+=' --metadata --data-ro --keep-data-ro --de';;
esac
# Record the remote exit explicitly: some USB failures do not propagate as
# an informative adb exit status. A timeout does NOT prove remote cleanup.
REMOTE="/skyward/run/hal/stage-km41/skyward $ARGS; rc=\$?; printf '\\nKEYMASTER_REMOTE_EXIT=%s\\n' \"\$rc\"; exit \"\$rc\""
set +e
timeout 180 adb -s "$SERIAL" shell "$REMOTE" 2>&1 | tee "$LOG/result.txt"
statuses=("${PIPESTATUS[@]}")
set -e
ADB_RC=${statuses[0]}
if [[ ${statuses[1]} != 0 ]]; then ADB_RC=${statuses[1]}; fi
printf '%s\n' "$ADB_RC" > "$LOG/adb-exit.txt"
# Read-only state collection even after a failed command. No automatic retry,
# remount, service reset, reboot or superuser request in normal Android.
AFTER=$(timeout 5 adb -s "$SERIAL" shell cat /proc/sys/kernel/random/boot_id 2> "$LOG/boot-id-after.error" | tr -d '\r') || AFTER=''
printf '%s\n' "$AFTER" > "$LOG/boot-id-after.txt"
timeout 5 adb -s "$SERIAL" shell 'echo ID; id; echo BOOT; cat /proc/sys/kernel/random/boot_id; getprop sys.boot_completed; echo SERVICES; getprop init.svc.adbd; getprop init.svc.venus_ui; echo PHASE; if [ "$(id -u)" = 0 ]; then cat /metadata/skyward-probe/keymaster41-last-phase.txt; fi; echo MOUNTS; grep -E " /data |skyward-data-ro" /proc/mounts || true' > "$LOG/state-after.txt" 2>&1 || true
stop_capture
python3 porting/venus/verify-km41-result.py --mode="$MODE" --log "$LOG" --adb-rc "$ADB_RC"
