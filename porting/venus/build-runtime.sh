#!/usr/bin/env bash
# Build Venus helpers from Rust. No adb/fastboot or device-side operations.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/29.0.14206865}"
OUT="${1:?usage: build-runtime.sh OUTPUT_DIRECTORY}"
mkdir -p "$OUT"
cargo test --locked -p skyward-venus-runtime
cargo clippy --locked -p skyward-venus-runtime --all-targets -- -D warnings
# Launcher and early Binder helpers must run before the private Android
# runtime exists. netmgr-ready needs dynamic Bionic for the vendor dlopen.
RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" cargo ndk -t arm64-v8a --platform 30 build --locked --release -p skyward-venus-runtime --bin venus-runtime --target-dir target/venus-runtime-static
STATIC=target/venus-runtime-static/aarch64-linux-android/release/venus-runtime
if readelf -l "$STATIC" | grep -q INTERP; then
    echo 'Venus early helpers must be statically linked' >&2
    exit 1
fi
for name in venus-runtime binderfs-setup km41-launch km41-rpmb km41-kmsg modem-hold crypto-launch km41-runtime ce-runtime boot-runtime cache-runtime crypto-start rw-runtime modem-prepare network-suspend network-netd; do
    cp -p "$STATIC" "$OUT/$name"
done
RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=-crt-static" cargo ndk -t arm64-v8a --platform 30 build --locked --release -p skyward-venus-runtime --bin venus-runtime --target-dir target/venus-runtime-dynamic
DYNAMIC=target/venus-runtime-dynamic/aarch64-linux-android/release/venus-runtime
readelf -l "$DYNAMIC" | grep -q '/system/bin/linker64'
cp -p "$DYNAMIC" "$OUT/netmgr-ready"
