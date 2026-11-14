#!/usr/bin/env bash
# Build a temporary-boot-only standalone Skyward REC from local source and BOOT.
# Cargo is forced offline; this script never talks to adb/fastboot or the network.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/29.0.14206865}"
BOOT="${SKYWARD_BOOT:-$ROOT/../boot.img}"
CONFIG=devices/venus.toml
for f in "$BOOT" "$CONFIG" "$ANDROID_NDK_HOME/source.properties" \
    resources/fonts/MiSansVF.ttf resources/fonts/KirkMono-Regular.ttf resources/fonts/KirkMono-OFL.txt; do
    test -s "$f" || { echo "Missing local build input: $f" >&2; exit 1; }
done

OUT="$(mktemp -d "$ROOT/target/venus-rec.XXXXXX")"
mkdir -p "$OUT/input"
cp -p "$BOOT" "$OUT/input/boot.img"
sha256sum "$OUT/input/boot.img" "$CONFIG" > "$OUT/inputs.sha256"
printf 'Output: %s\n' "$OUT"

cargo build --locked --offline --release -p skyward-pack 2>&1 | tee "$OUT/host-build.log"
cargo ndk -t arm64-v8a --platform 30 build --locked --offline --release -p skyward 2>&1 | tee "$OUT/android-build.log"
RUSTFLAGS="-C target-feature=+crt-static" cargo ndk -t arm64-v8a --platform 30 build \
    --locked --offline --release -p skyward-system-init --target-dir target/system-init \
    2>&1 | tee "$OUT/system-init-build.log"

ARGS=(recovery --partition boot --stock "$OUT/input/boot.img" \
    --binary target/aarch64-linux-android/release/skyward \
    --system-init target/system-init/aarch64-linux-android/release/system-init \
    --config "$CONFIG" --font resources/fonts/MiSansVF.ttf \
    --mono resources/fonts/KirkMono-Regular.ttf \
    --mono-license resources/fonts/KirkMono-OFL.txt)
target/release/skyward-pack "${ARGS[@]}" --standalone --venus-force-recovery \
    --out "$OUT/skyward-rec.img" 2>&1 | tee "$OUT/pack.log"
python3 porting/venus/verify-entryfix.py --temporary-rec \
    "$OUT/input/boot.img" "$OUT/skyward-rec.img" \
    target/aarch64-linux-android/release/skyward "$CONFIG" \
    > "$OUT/validation.json"
sha256sum -c "$OUT/inputs.sha256"
(cd "$OUT" && sha256sum skyward-rec.img > SHA256SUMS)
printf '%s\n' "$OUT" > target/venus-rec-latest.txt
cat "$OUT/validation.json"
cat "$OUT/SHA256SUMS"
printf '\nPASS: %s/skyward-rec.img\nTEMPORARY FASTBOOT BOOT ONLY. DO NOT FLASH.\n' "$OUT"
