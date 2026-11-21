#!/usr/bin/env bash
# Standalone Rust diagnostic launcher; do not build or execute legacy C code.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
OUTPUT="${1:?usage: build-launch.sh OUTPUT_ELF}"
ROLE="$(basename -- "$OUTPUT")"
case "$ROLE" in display-probe-launch|display-ipc-launch|display-ipc-qservice) ;; *) echo "Unsupported display role: $ROLE" >&2; exit 2;; esac
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/29.0.14206865}"
RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" cargo ndk -t arm64-v8a --platform 30 build --locked --release -p skyward-venus-runtime --bin venus-runtime --target-dir target/venus-runtime-static
BINARY=target/venus-runtime-static/aarch64-linux-android/release/venus-runtime
if readelf -l "$BINARY" | grep -q INTERP; then
    echo 'Display launcher requires a static early-execution binary' >&2
    exit 1
fi
mkdir -p "$(dirname -- "$OUTPUT")"
cp -p "$BINARY" "$OUTPUT"
python3 - "$OUTPUT" "$ROOT" "$ROLE" <<'PY'
import hashlib, json, sys
from pathlib import Path
output=Path(sys.argv[1]).resolve(); root=Path(sys.argv[2])
paths=[root/'crates/skyward-venus-runtime/src/display/root.rs',
       root/'crates/skyward-venus-runtime/src/display/ipc.rs',
       root/'crates/skyward-venus-runtime/src/display/qservice.rs',
       root/'crates/skyward-venus-runtime/src/display/stc.rs',
       root/'crates/skyward-venus-runtime/src/display/platform_binder.rs',
       root/'crates/skyward-venus-runtime/src/display/protocol.rs',
       root/'crates/skyward-venus-runtime/src/display/transport.rs',
       root/'crates/skyward-venus-runtime/src/display/mod.rs',
       root/'crates/skyward-venus-runtime/src/display/guard.rs',
       root/'crates/skyward-venus-runtime/src/lib.rs',
       root/'crates/skyward-venus-runtime/src/common.rs',
       root/'crates/skyward-venus-runtime/Cargo.toml',
       root/'porting/venus/boot-ld.config.txt']
record={'sha256':hashlib.sha256(output.read_bytes()).hexdigest(),'implementation':'rust','role':sys.argv[3],'sources':{str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest() for p in paths},'device_tested':False}
output.with_name(output.name+'.build.json').write_text(json.dumps(record,indent=2)+'\n')
print('PASS Rust display launcher',output,record['sha256'])
PY
