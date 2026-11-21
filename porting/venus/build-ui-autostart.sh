#!/usr/bin/env bash
# Build/validate only. No phone commands, flash, reboot or slot changes.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
DUMP="${VENUS_DUMP_DIR:-$HOME/SKYWARD/venus_dump}"
TOOLS="${VENUS_TOOLS_DIR:-$HOME/SKYWARD/tools}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/29.0.14206865}"
CONFIG=porting/venus/ui-probe.toml
for f in "$DUMP/boot_a.img" "$DUMP/vendor_boot_a.img" "$DUMP/vbmeta_a.img" "$CONFIG" "$TOOLS/avbtool.py" "$ANDROID_NDK_HOME/source.properties" \
    resources/fonts/MiSansVF.ttf resources/fonts/KirkMono-Regular.ttf resources/fonts/KirkMono-OFL.txt; do
    test -s "$f" || { echo "Missing input: $f" >&2; exit 1; }
done
mkdir -p target
OUT="$(mktemp -d "$ROOT/target/venus-ui-autostart.XXXXXX")"
mkdir -p "$OUT/input" "$OUT/plain-probe" "$OUT/normal-isolation"
printf 'Output: %s\n' "$OUT"
sha256sum "$DUMP/boot_a.img" "$DUMP/vendor_boot_a.img" "$DUMP/vbmeta_a.img" > "$OUT/inputs.sha256"
cp -p "$DUMP/boot_a.img" "$OUT/input/boot_a.img"
cp -p "$CONFIG" "$OUT/input/device.toml"
cmp "$DUMP/boot_a.img" "$OUT/input/boot_a.img"
git diff --check
cargo test --locked -p skyward-pack 2>&1 | tee "$OUT/pack-tests.log"
cargo clippy --locked -p skyward-pack --all-targets -- -D warnings 2>&1 | tee "$OUT/pack-clippy.log"
cargo test --locked -p skyward-ui --lib 2>&1 | tee "$OUT/ui-tests.log"
cargo build --locked --release -p skyward-pack 2>&1 | tee "$OUT/pack-build.log"
cargo ndk -t arm64-v8a --platform 30 build --locked --release -p skyward 2>&1 | tee "$OUT/android-build.log"
cp -p target/aarch64-linux-android/release/skyward "$OUT/input/skyward"
file "$OUT/input/skyward" | tee "$OUT/binary-info.txt"
readelf -d "$OUT/input/skyward" > "$OUT/binary-dynamic.txt"
ARGS=(recovery --partition boot --stock "$OUT/input/boot_a.img" --binary "$OUT/input/skyward" --config "$OUT/input/device.toml"
      --font resources/fonts/MiSansVF.ttf --mono resources/fonts/KirkMono-Regular.ttf --mono-license resources/fonts/KirkMono-OFL.txt)
PACK=target/release/skyward-pack
VERIFY=porting/venus/verify-entryfix.py
"$PACK" "${ARGS[@]}" --venus-probe --venus-ui --out "$OUT/boot.img" 2>&1 | tee "$OUT/pack.log"
python3 "$VERIFY" --ui "$OUT/input/boot_a.img" "$OUT/boot.img" "$OUT/input/skyward" "$OUT/input/device.toml" | tee "$OUT/validation.json"
python3 "$TOOLS/avbtool.py" info_image --image "$OUT/boot.img" > "$OUT/avb-info.txt"
python3 "$TOOLS/avbtool.py" verify_image --image "$OUT/boot.img" 2>&1 | tee "$OUT/avb-verify.txt"
# Isolation checks: neither the old CLI probe nor normal packaging starts UI.
"$PACK" "${ARGS[@]}" --venus-probe --out "$OUT/plain-probe/boot.img" > "$OUT/plain-probe-pack.log"
python3 "$VERIFY" "$OUT/input/boot_a.img" "$OUT/plain-probe/boot.img" "$OUT/input/skyward" "$OUT/input/device.toml" > "$OUT/plain-probe-validation.json"
"$PACK" "${ARGS[@]}" --out "$OUT/normal-isolation/boot.img" > "$OUT/normal-pack.log"
python3 "$VERIFY" --normal "$OUT/input/boot_a.img" "$OUT/normal-isolation/boot.img" "$OUT/input/skyward" "$OUT/input/device.toml" > "$OUT/normal-validation.json"
sha256sum -c "$OUT/inputs.sha256"
(cd "$OUT" && sha256sum boot.img input/skyward input/device.toml > SHA256SUMS)
printf '%s\n' "$OUT" > target/venus-ui-autostart-latest.txt
printf '\nPASS: %s/boot.img\nTEMPORARY UI-ONLY BOOT: DO NOT FLASH. AUTO START STILL NEEDS A COLD-BOOT TEST.\n' "$OUT"
