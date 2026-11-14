#!/usr/bin/env bash
# Explicit, bounded diagnostics in an already-booted integrated Recovery.
# Never boots a modem, mounts storage, changes radio/driver settings or reboots.
# --http-tls sends small public HTTP/TLS requests over an existing connection.
set -euo pipefail
umask 077
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
MODE="${1:---status}"
SERIAL="${ANDROID_SERIAL:-535e451a}"
case "$MODE" in
    --status) [[ $# -le 1 ]] || exit 2; DNS=();;
    --http-tls)
        [[ $# -ge 2 && $# -le 5 ]] || { echo 'Use --http-tls DNS1 [DNS2 ...]' >&2; exit 2; }
        shift
        mapfile -t DNS < <(python3 - "$@" <<'PY'
import ipaddress,sys
try:
    servers=[ipaddress.IPv4Address(s) for s in sys.argv[1:]]
    if any(s.is_unspecified or s.is_multicast or s.is_loopback or int(s)==0xffffffff for s in servers):
        raise ValueError('unusable carrier DNS')
except ValueError as e:
    print(str(e),file=sys.stderr);sys.exit(2)
print('\n'.join(map(str,servers)))
PY
)
        [[ ${#DNS[@]} -ge 1 ]] || exit 2;;
    *) echo 'Use --status or --http-tls DNS1 [DNS2 ...]' >&2; exit 2;;
esac
OUT="${VENUS_RECOVERY_DIR:-$(cat target/venus-recovery-latest.txt)}"
NETWORK_CHECK="${VENUS_NETWORK_CHECK:-$ROOT/target/aarch64-linux-android/release/examples/venus_network_check}"
for tool in adb timeout python3 sha256sum; do command -v "$tool" >/dev/null || exit 1; done
[[ -s "$OUT/assets/km41-kmsg" && -s "$OUT/assets/skyward" ]]
(cd "$OUT" && sha256sum -c ASSETS.sha256 >/dev/null)
if [[ "$MODE" = --http-tls ]]; then [[ -s "$NETWORK_CHECK" ]]; fi
# Main Android must be refused before any deployment or remote operation.
timeout 10 adb -s "$SERIAL" shell 'test "$(id -u)" = 0 && grep -qx venus-recovery-rw-v1 /skyward/venus-recovery.txt && grep -qx venus-entryfix-v1 /skyward/probe-entryfix.txt' || {
    echo 'Requires the already-booted Venus temporary Recovery; no deployment or network test was performed.' >&2; exit 1;
}
LOG="$(mktemp -d "${VENUS_LOG_ROOT:-$HOME/SKYWARD/venus_dump}/network-capture.XXXXXX")"
KLOG_PID=''
stop_capture() {
    if [[ -n "$KLOG_PID" ]]; then
        kill -TERM "$KLOG_PID" 2>/dev/null || true
        wait "$KLOG_PID" 2>/dev/null || true
        KLOG_PID=''
    fi
}
trap stop_capture EXIT
printf 'Network diagnostic logs: %s\n' "$LOG"
BOOT_ID="$(timeout 5 adb -s "$SERIAL" shell cat /proc/sys/kernel/random/boot_id | tr -d '\r')"
python3 - "$BOOT_ID" <<'PY'
import sys,uuid
assert str(uuid.UUID(sys.argv[1]))==sys.argv[1]
PY
printf '%s\n' "$BOOT_ID" >"$LOG/boot-id-before.txt"
# Record exact on-device programs; do not replace the integrated UI binary.
for name in km41-kmsg skyward; do
    remote=/skyward/runtime/km41-kmsg
    [[ "$name" != skyward ]] || remote=/system/bin/skyward
    read -r expected _ < <(sha256sum "$OUT/assets/$name")
    read -r actual _ < <(timeout 10 adb -s "$SERIAL" shell sha256sum "$remote")
    [[ "$actual" = "$expected" ]] || { echo "On-device $name differs from candidate; refusing test" >&2; exit 1; }
    printf '%s  %s\n' "$actual" "$remote" >>"$LOG/programs.sha256"
done
adb -s "$SERIAL" exec-out /skyward/runtime/km41-kmsg >"$LOG/kernel-live.txt" 2>"$LOG/kernel-capture-error.txt" &
KLOG_PID=$!
ready=0
for ((i=0;i<60;i++)); do
    kill -0 "$KLOG_PID" 2>/dev/null || break
    if grep -q '^KM41_KMSG_CAPTURE_READY ' "$LOG/kernel-live.txt" && grep -Eq '^[0-9]+,[0-9]+,[0-9]+,' "$LOG/kernel-live.txt"; then ready=1; break; fi
    sleep 0.1
done
[[ "$ready" = 1 ]] || { echo 'Live kernel capture is not ready; no network test was started.' >&2; exit 1; }
CURRENT="$(timeout 5 adb -s "$SERIAL" shell cat /proc/sys/kernel/random/boot_id | tr -d '\r')"
[[ "$CURRENT" = "$BOOT_ID" ]] && kill -0 "$KLOG_PID" 2>/dev/null || {
    echo 'Boot identity or kernel capture changed; no network test was started.' >&2; exit 1;
}
REMOTE=/system/bin/skyward
if [[ "$MODE" = --http-tls ]]; then
    # The mktemp basename contains no shell metacharacters. Only this new
    # RAM directory receives a probe; it cannot replace another program.
    TAG="${LOG##*/}"
    [[ "$TAG" =~ ^network-capture\.[a-zA-Z0-9]+$ ]]
    REMOTE="/tmp/$TAG/network-check"
    timeout 10 adb -s "$SERIAL" shell "mkdir -m 0700 /tmp/$TAG"
    timeout 15 adb -s "$SERIAL" push "$NETWORK_CHECK" "$REMOTE" >"$LOG/deploy.txt"
    timeout 5 adb -s "$SERIAL" shell "chmod 0700 $REMOTE"
    read -r expected _ < <(sha256sum "$NETWORK_CHECK")
    read -r actual _ < <(timeout 10 adb -s "$SERIAL" shell sha256sum "$REMOTE")
    [[ "$actual" = "$expected" ]] || { echo 'Network probe hash differs; refusing execution' >&2; exit 1; }
    printf '%s  %s\n' "$actual" "$REMOTE" >>"$LOG/programs.sha256"
fi
kill -0 "$KLOG_PID" 2>/dev/null || { echo 'Kernel capture ended; refusing execution' >&2; exit 1; }
set +e
if [[ "$MODE" = --status ]]; then
    timeout 180 adb -s "$SERIAL" exec-out "$REMOTE" cellular-status >"$LOG/result.txt" 2>"$LOG/result-error.txt"
else
    timeout 180 adb -s "$SERIAL" exec-out "$REMOTE" "${DNS[@]}" >"$LOG/result.txt" 2>"$LOG/result-error.txt"
fi
RC=$?
set -e
LIVE=0
kill -0 "$KLOG_PID" 2>/dev/null && LIVE=1
AFTER="$(timeout 5 adb -s "$SERIAL" shell cat /proc/sys/kernel/random/boot_id 2>"$LOG/boot-id-after.error" | tr -d '\r')" || AFTER=''
printf '%s\n' "$AFTER" >"$LOG/boot-id-after.txt"
stop_capture
python3 - "$LOG" "$MODE" "$RC" "$LIVE" <<'PY'
import json,pathlib,re,sys
log=pathlib.Path(sys.argv[1]);mode=sys.argv[2];rc=int(sys.argv[3]);live=sys.argv[4]=='1'
before=(log/'boot-id-before.txt').read_text().strip();after=(log/'boot-id-after.txt').read_text().strip()
text=(log/'result.txt').read_text(errors='replace');errors=[]
if rc:errors.append(f'remote probe exit {rc}; cleanup is not established by a timeout')
if not live:errors.append('live kernel capture ended before the probe completed')
if after!=before:errors.append('device disconnected or boot identity changed')
if mode=='--http-tls':
    if 'HTTP reaches the internet' not in text:errors.append('no HTTP connectivity result')
    if not re.search(r'^HTTPS status=200(?: OK)? bytes=[1-9][0-9]* sha256=[0-9a-f]{64}$',text,re.M):
        errors.append('no complete verified TLS transfer result')
else:
    try:
        status=json.loads(text)
        if status.get('read_only') is not True:errors.append('invalid read-only status response')
    except (ValueError,AttributeError):errors.append('missing or malformed cellular status')
summary={'scope':'one explicit network probe with host live-kernel capture','mode':mode,'verified':not errors,
         'internet_verified':not errors and mode=='--http-tls','boot_id_before':before,'boot_id_after':after,
         'adb_exit':rc,'capture_live_at_probe_end':live,'failures':errors,
         'radio_bringup_tested':False,'disconnect_reconnect_tested':False,'wlan_switch_tested':False}
(log/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
print(('PASS' if not errors else 'FAIL')+f' {mode}: {log}')
for error in errors:print('  '+error)
sys.exit(bool(errors))
PY
