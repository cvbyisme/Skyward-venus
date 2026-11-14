#!/bin/sh
# Extract the exact dependency wait implementation; mock all device access.
set -eu
[ "$#" = 1 ] || exit 2
T=$(mktemp -d /tmp/km41-ready-test.XXXXXX)
trap 'rm -rf "$T"' EXIT
sed -n '/^# KM41_READINESS_BEGIN$/,/^# KM41_READINESS_END$/p' "$1" > "$T/wait.sh"
[ -s "$T/wait.sh" ]
. "$T/wait.sh"
role_alive() { [ "$SCENARIO" != dead ]; }
getprop() {
    printf '%s\n' probe >> "$T/probes"
    case "$SCENARIO" in
        ready) echo true;;
        delayed) [ ! -e "$T/tick" ] || echo true;;
        never|dead) echo false;;
    esac
}
sleep() { [ "$1" = 0.1 ] || return 9; : > "$T/tick"; }
N=0
check() {
    SCENARIO=$1; expected=$2; limit=$3
    rm -f "$T/tick" "$T/probes"
    rc=0
    wait_owned_property qseecomd vendor.sys.listeners.registered true "$limit" > "$T/out" 2>&1 || rc=$?
    [ "$rc" = "$expected" ] || { cat "$T/out"; echo 'readiness test failed' >&2; exit 1; }
    if [ "$rc" = 0 ]; then grep -q '^KEYMASTER_DEPENDENCY_READY ' "$T/out"; fi
    if [ "$SCENARIO" = never ]; then
        [ "$(wc -l < "$T/probes")" -eq $((limit*10)) ] || exit 1
    fi
    N=$((N+1))
}
check ready 0 2
check delayed 0 3
check never 1 2
check dead 1 2
check ready 2 0
check ready 2 invalid
printf 'KM41_READINESS_TESTS_PASS count=%s; no HAL or phone was used\n' "$N"
