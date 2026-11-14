#!/usr/bin/env bash
# Build and validate only. Does not flash, reboot or contact the phone.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/29.0.14206865}"
DUMP="${VENUS_DUMP_DIR:-$HOME/SKYWARD/venus_dump}"
TOOLS="${VENUS_TOOLS_DIR:-$HOME/SKYWARD/tools}"
CONFIG=porting/venus/ui-probe.toml
for f in "$DUMP/boot_a.img" "$DUMP/vendor_boot_a.img" "$DUMP/vbmeta_a.img" "$CONFIG" "$TOOLS/avbtool.py" "$ANDROID_NDK_HOME/source.properties"; do
    test -s "$f" || { echo "Missing input: $f" >&2; exit 1; }
done
OUT="$(mktemp -d "$ROOT/target/venus-basic-functions.XXXXXX")"
mkdir -p "$OUT/input" "$OUT/ui-isolation" "$OUT/normal-isolation"
echo "Output: $OUT"
sha256sum "$DUMP/boot_a.img" "$DUMP/vendor_boot_a.img" "$DUMP/vbmeta_a.img" > "$OUT/inputs.sha256"
cp -p "$DUMP/boot_a.img" "$OUT/input/boot_a.img"
cp -p "$CONFIG" "$OUT/input/device.toml"
for f in crates/skyward-pack/src/venus_probe.sh crates/skyward-pack/src/venus_probe_logs.sh crates/skyward-pack/src/venus_ui_start.sh; do sh -n "$f"; done
git diff --check
cargo test --locked -p skyward --bin skyward 2>&1 | tee "$OUT/basic-tests.log"
cargo test --locked -p skyward-pack 2>&1 | tee "$OUT/pack-tests.log"
cargo clippy --locked -p skyward-pack --all-targets -- -D warnings 2>&1 | tee "$OUT/pack-clippy.log"
cargo test --locked -p skyward-ui --lib 2>&1 | tee "$OUT/ui-tests.log"
cargo build --locked --release -p skyward-pack 2>&1 | tee "$OUT/pack-build.log"
cargo ndk -t arm64-v8a --platform 30 build --locked --release -p skyward 2>&1 | tee "$OUT/android-build.log"
cp -p target/aarch64-linux-android/release/skyward "$OUT/input/skyward"
ARGS=(recovery --partition boot --stock "$OUT/input/boot_a.img" --binary "$OUT/input/skyward" --config "$OUT/input/device.toml"
      --font resources/fonts/MiSansVF.ttf --mono resources/fonts/KirkMono-Regular.ttf --mono-license resources/fonts/KirkMono-OFL.txt)
target/release/skyward-pack "${ARGS[@]}" --venus-probe --venus-ui --venus-basic --out "$OUT/boot.img" 2>&1 | tee "$OUT/pack.log"
python3 porting/venus/verify-entryfix.py --ui --basic "$OUT/input/boot_a.img" "$OUT/boot.img" "$OUT/input/skyward" "$OUT/input/device.toml" > "$OUT/validation.json"
python3 "$TOOLS/avbtool.py" info_image --image "$OUT/boot.img" > "$OUT/avb-info.txt"
python3 "$TOOLS/avbtool.py" verify_image --image "$OUT/boot.img" 2>&1 | tee "$OUT/avb-verify.txt"
target/release/skyward-pack "${ARGS[@]}" --venus-probe --venus-ui --out "$OUT/ui-isolation/boot.img" > "$OUT/ui-isolation/pack.log" 2>&1
python3 porting/venus/verify-entryfix.py --ui "$OUT/input/boot_a.img" "$OUT/ui-isolation/boot.img" "$OUT/input/skyward" "$OUT/input/device.toml" > "$OUT/ui-isolation/validation.json"
target/release/skyward-pack "${ARGS[@]}" --out "$OUT/normal-isolation/boot.img" > "$OUT/normal-isolation/pack.log" 2>&1
python3 porting/venus/verify-entryfix.py --normal "$OUT/input/boot_a.img" "$OUT/normal-isolation/boot.img" "$OUT/input/skyward" "$OUT/input/device.toml" > "$OUT/normal-isolation/validation.json"
sha256sum -c "$OUT/inputs.sha256"
(cd "$OUT" && sha256sum boot.img input/skyward input/device.toml > SHA256SUMS)
printf '%s\n' "$OUT" > target/venus-basic-functions-latest.txt
printf '\nPASS: %s/boot.img\nTEMPORARY BASIC-FUNCTION BOOT. DO NOT FLASH.\n' "$OUT"
