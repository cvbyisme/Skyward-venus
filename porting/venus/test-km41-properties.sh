#!/bin/sh
# Exercise the exact runtime property reader without starting HALs or mounts.
# Runs under host /bin/sh and the phone's /system/bin/sh. No awk is needed.
set -eu
[ "$#" -eq 1 ] || { echo 'Usage: test-km41-properties.sh km41-runtime.sh' >&2; exit 2; }
SOURCE=$1
BASE=${TMPDIR:-/tmp}
# Recovery inherits /data/local/tmp on some builds even though Data is closed.
if [ ! -d "$BASE" ] || [ ! -w "$BASE" ]; then BASE=/tmp; fi
T=$(mktemp -d "$BASE/km41-properties.XXXXXX")
trap 'rm -rf "$T"' EXIT
sed -n '/^# KM41_PROPERTY_READER_BEGIN$/,/^# KM41_PROPERTY_READER_END$/p' "$SOURCE" > "$T/reader.sh"
[ -s "$T/reader.sh" ] || { echo 'Property reader test markers missing' >&2; exit 1; }
. "$T/reader.sh"
# mksh uses the existing toybox printf executable; dash/bash use a builtin.
# Expose only printf, so neither awk nor another text processor can be used.
mkdir "$T/tools"
printf_tool=$(command -v printf)
case "$printf_tool" in /*) ln -s "$printf_tool" "$T/tools/printf";; esac
N=0
check() {
    # Only printf is available as an external command.
    actual=$(PATH="$T/tools"; export PATH; read_prop "$1" "$T/input")
    [ "$actual" = "$2" ] || { printf 'FAIL property reader test %s\n' "$3" >&2; exit 1; }
    N=$((N + 1))
}
refusal() {
    status=0
    (PATH="$T/tools"; export PATH; read_prop "$1" "$2") > "$T/result" 2> "$T/error" || status=$?
    [ "$status" -eq "$3" ] || { echo 'FAIL unexpected property-reader exit status' >&2; exit 1; }
    N=$((N + 1))
}
printf '# comment\n#ro.build.version.release=99\nroXbuildXversionXrelease=88\nro.build.version.release.extra=77\nro.build.version.release=17\nro.build.version.security_patch=2026-10-01\n' > "$T/input"
check ro.build.version.release 17 exact-key
check ro.build.version.security_patch 2026-10-01 patch-level
printf 'ro.test=value-without-final-newline' > "$T/input"
check ro.test value-without-final-newline no-final-newline
printf 'ro.test=CRLF-value\r\n' > "$T/input"
check ro.test CRLF-value crlf
printf 'ro.test=first\nro.test=second\n' > "$T/input"
check ro.test first first-match
printf '%s\n' 'ro.test=  left=right\tail  ' > "$T/input"
check ro.test '  left=right\tail  ' preserve-value
printf '%s\n' 'ro.test=$(exit 96);`exit 97`' > "$T/input"
check ro.test '$(exit 96);`exit 97`' no-evaluation
printf 'ro.test=\n' > "$T/input"
check ro.test '' empty-value
refusal ro.missing "$T/input" 1
refusal ro.test "$T/nonexistent" 2
refusal '' "$T/input" 2
printf 'KM41_PROPERTY_READER_TESTS_PASS count=%s; restricted PATH excludes awk\n' "$N"
