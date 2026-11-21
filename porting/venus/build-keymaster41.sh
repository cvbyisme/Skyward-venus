#!/usr/bin/env bash
# Offline build only: no adb, fastboot, partition writes or automatic HAL start.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/29.0.14206865}"
DUMP="${VENUS_DUMP_DIR:-$HOME/SKYWARD/venus_dump}"
TOOLS="${VENUS_TOOLS_DIR:-$HOME/SKYWARD/tools}"
CACHE="${VENUS_KM41_CACHE:-$DUMP/keymaster41.sSBzw7}"
CC="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android30-clang"
CONFIG=porting/venus/ui-probe.toml
for f in "$DUMP/boot_a.img" "$DUMP/vendor_boot_a.img" "$DUMP/vbmeta_a.img" "$CC" "$CONFIG" "$TOOLS/avbtool.py" "$CACHE/runtime.img" "$CACHE/vndk30.img" "$CACHE/framework-manifest.xml"; do
    test -s "$f" || { echo "Missing input: $f" >&2; exit 1; }
done
mkdir -p target
case "${VENUS_BUILD_FLAVOR:-keymaster}" in
    keymaster) PREFIX=venus-twrp-lifecycle;;
    ce) PREFIX=venus-ce;;
    *) echo 'Unknown build flavor' >&2; exit 2;;
esac
OUT="$(mktemp -d "$ROOT/target/$PREFIX.XXXXXX")"
mkdir -p "$OUT/input" "$OUT/assets" "$OUT/normal-isolation"
echo "Output: $OUT"
sha256sum "$DUMP/boot_a.img" "$DUMP/vendor_boot_a.img" "$DUMP/vbmeta_a.img" > "$OUT/inputs.sha256"
cp -p "$DUMP/boot_a.img" "$OUT/input/boot_a.img"
cp -p "$CONFIG" "$OUT/input/device.toml"
cmp "$DUMP/boot_a.img" "$OUT/input/boot_a.img"
bash -n porting/venus/prepare-keymaster41.sh
cargo test --locked -p skyward-venus-runtime 2>&1 | tee "$OUT/runtime-tests.log"
python3 porting/venus/test-km41-result.py 2>&1 | tee "$OUT/result-tests.log"
python3 porting/venus/test-km41-host.py
python3 porting/venus/test-ce-local.py 2>&1 | tee "$OUT/ce-helper-tests.log" 2>&1 | tee "$OUT/host-wrapper-tests.log"
git diff --check
cargo test --locked -p skyward-binder -p skyward-crypto --lib 2>&1 | tee "$OUT/protocol-tests.log"
cargo test --locked -p skyward --bin skyward 2>&1 | tee "$OUT/device-policy-tests.log"
cargo test --locked -p skyward-pack 2>&1 | tee "$OUT/pack-tests.log"
cargo test --locked -p skyward-ui --lib 2>&1 | tee "$OUT/ui-tests.log"
cargo clippy --locked -p skyward-pack --all-targets -- -D warnings 2>&1 | tee "$OUT/pack-clippy.log"
cargo build --locked --release -p skyward-pack 2>&1 | tee "$OUT/pack-build.log"
cargo ndk -t arm64-v8a --platform 30 build --locked --release -p skyward 2>&1 | tee "$OUT/android-build.log"
cp -p target/aarch64-linux-android/release/skyward "$OUT/assets/skyward"
bash porting/venus/build-runtime.sh "$OUT/assets" 2>&1 | tee "$OUT/rust-runtime-build.log"
cp -p "$CACHE/runtime.img" "$CACHE/vndk30.img" "$CACHE/framework-manifest.xml" "$OUT/assets/"
printf '%s  %s\n' \
    9776b642132872ce4931218694c345231ecaac4490ead5d7c2f7c24e17481c17 "$OUT/assets/runtime.img" \
    8948745de8d27cae3fbdf23f6185ae4b6aec680b6a6febf2a2f0eaba1ab59b9e "$OUT/assets/vndk30.img" | sha256sum -c -
ARGS=(recovery --partition boot --stock "$OUT/input/boot_a.img" --binary "$OUT/assets/skyward" --config "$OUT/input/device.toml"
      --font resources/fonts/MiSansVF.ttf --mono resources/fonts/KirkMono-Regular.ttf --mono-license resources/fonts/KirkMono-OFL.txt)
# Keep the known menu/service behavior. Crypto diagnostics are explicit CLI
# commands only, not a cold-boot automatic data-unlock experiment.
target/release/skyward-pack "${ARGS[@]}" --venus-probe --venus-ui --venus-basic --out "$OUT/boot.img" 2>&1 | tee "$OUT/pack.log"
python3 porting/venus/verify-entryfix.py --ui --basic "$OUT/input/boot_a.img" "$OUT/boot.img" "$OUT/assets/skyward" "$OUT/input/device.toml" > "$OUT/validation.json"
python3 "$TOOLS/avbtool.py" info_image --image "$OUT/boot.img" > "$OUT/avb-info.txt"
python3 "$TOOLS/avbtool.py" verify_image --image "$OUT/boot.img" 2>&1 | tee "$OUT/avb-verify.txt"
target/release/skyward-pack "${ARGS[@]}" --out "$OUT/normal-isolation/boot.img" > "$OUT/normal-isolation/pack.log" 2>&1
python3 porting/venus/verify-entryfix.py --normal "$OUT/input/boot_a.img" "$OUT/normal-isolation/boot.img" "$OUT/assets/skyward" "$OUT/input/device.toml" > "$OUT/normal-isolation/validation.json"
sha256sum -c "$OUT/inputs.sha256"
(cd "$OUT" && sha256sum assets/* > ASSETS.sha256 && sha256sum boot.img input/device.toml > SHA256SUMS)
printf '%s\n' "$OUT" > target/venus-keymaster41-latest.txt
if [[ "${VENUS_BUILD_FLAVOR:-keymaster}" = ce ]]; then
    printf '%s\n' "$OUT" > target/venus-ce-latest.txt
fi
printf '\nPASS: %s/boot.img\nTemporary diagnostic image; DO NOT FLASH. No HAL or data-unlock actions were run by this build.\n' "$OUT"
