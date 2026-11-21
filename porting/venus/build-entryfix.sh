#!/usr/bin/env bash
# Build and validate only. Never invokes adb or fastboot.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
DUMP="${VENUS_DUMP_DIR:-$HOME/SKYWARD/venus_dump}"
TOOLS="${VENUS_TOOLS_DIR:-$HOME/SKYWARD/tools}"
CONFIG=porting/venus/pack-test.toml
BINARY=target/aarch64-linux-android/release/skyward
for f in "$DUMP/boot_a.img" "$DUMP/vendor_boot_a.img" "$DUMP/vbmeta_a.img" "$CONFIG" "$BINARY" "$TOOLS/avbtool.py"; do
    test -s "$f" || { echo "Missing input: $f" >&2; exit 1; }
done
mkdir -p target
OUT="$(mktemp -d "$ROOT/target/venus-entryfix.XXXXXX")"
mkdir -p "$OUT/input" "$OUT/normal-isolation"
printf 'Output: %s\n' "$OUT"
sha256sum "$DUMP/boot_a.img" "$DUMP/vendor_boot_a.img" "$DUMP/vbmeta_a.img" > "$OUT/inputs.sha256"
cp -p "$DUMP/boot_a.img" "$OUT/input/boot_a.img"
cmp "$DUMP/boot_a.img" "$OUT/input/boot_a.img"
git diff --check
cargo test --locked -p skyward-pack 2>&1 | tee "$OUT/tests.log"
cargo clippy --locked -p skyward-pack --all-targets -- -D warnings 2>&1 | tee "$OUT/clippy.log"
cargo build --locked --release -p skyward-pack 2>&1 | tee "$OUT/build.log"
ARGS=(recovery --partition boot --stock "$OUT/input/boot_a.img" --binary "$BINARY" --config "$CONFIG"
      --font resources/fonts/MiSansVF.ttf --mono resources/fonts/KirkMono-Regular.ttf --mono-license resources/fonts/KirkMono-OFL.txt)
target/release/skyward-pack "${ARGS[@]}" --venus-probe --out "$OUT/boot.img" 2>&1 | tee "$OUT/pack.log"
python3 porting/venus/verify-entryfix.py "$OUT/input/boot_a.img" "$OUT/boot.img" "$BINARY" "$CONFIG" | tee "$OUT/validation.json"
python3 "$TOOLS/avbtool.py" info_image --image "$OUT/boot.img" > "$OUT/avb-info.txt"
python3 "$TOOLS/avbtool.py" verify_image --image "$OUT/boot.img" 2>&1 | tee "$OUT/avb-verify.txt"
# Prove that the unflagged path does not modify the first-stage binary or rc.
target/release/skyward-pack "${ARGS[@]}" --out "$OUT/normal-isolation/boot.img" > "$OUT/normal-pack.log"
python3 porting/venus/verify-entryfix.py --normal "$OUT/input/boot_a.img" "$OUT/normal-isolation/boot.img" "$BINARY" "$CONFIG" > "$OUT/normal-validation.json"
sha256sum -c "$OUT/inputs.sha256"
(cd "$OUT" && sha256sum boot.img > SHA256SUMS)
printf '%s\n' "$OUT" > target/venus-entryfix-latest.txt
printf '\nPASS: %s/boot.img\nTEMPORARY PROBE ONLY. NOT DEVICE TESTED. DO NOT FLASH.\n' "$OUT"
