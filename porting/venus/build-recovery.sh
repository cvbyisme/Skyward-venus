#!/usr/bin/env bash
# Offline build only. Does not use adb/fastboot, mount userdata, or submit credentials.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/29.0.14206865}"
DUMP="${VENUS_DUMP_DIR:-$HOME/SKYWARD/venus_dump}"
TOOLS="${VENUS_TOOLS_DIR:-$HOME/SKYWARD/tools}"
CACHE="${VENUS_KM41_CACHE:-$DUMP/keymaster41.sSBzw7}"
TETHERING="${VENUS_TETHERING_IMAGE:-$DUMP/network-20261110/apex-ro/com.android.tethering.img}"
CC="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android30-clang"
CONFIG=devices/venus.toml
for file in "$DUMP/boot_a.img" "$DUMP/vendor_boot_a.img" "$DUMP/vbmeta_a.img" "$CC" "$CONFIG" "$TOOLS/avbtool.py" "$CACHE/runtime.img" "$CACHE/vndk30.img" "$TETHERING" porting/venus/framework-manifest.xml porting/venus/suspend/build.sh; do
    test -s "$file" || { printf 'Missing input: %s\n' "$file" >&2; exit 1; }
done
mkdir -p target
OUT="$(mktemp -d "$ROOT/target/venus-recovery.XXXXXX")"
mkdir "$OUT/input" "$OUT/assets" "$OUT/temporary" "$OUT/standalone" "$OUT/normal" "$OUT/old-mode-isolation"
printf 'Output: %s\n' "$OUT"
sha256sum "$DUMP/boot_a.img" "$DUMP/vendor_boot_a.img" "$DUMP/vbmeta_a.img" > "$OUT/inputs.sha256"
cp -p "$DUMP/boot_a.img" "$OUT/input/boot_a.img"
cp -p "$CONFIG" "$OUT/input/device.toml"
cmp "$DUMP/boot_a.img" "$OUT/input/boot_a.img"
for script in km41-runtime ce-runtime rw-runtime boot-runtime cache-runtime modem-prepare; do sh -n "porting/venus/$script.sh"; done
python3 -m py_compile porting/venus/verify-recovery.py
python3 porting/venus/test-rw-recovery.py 2>&1 | tee "$OUT/rw-runtime-tests.log"
python3 porting/venus/test-runtime-cache.py 2>&1 | tee "$OUT/cache-runtime-tests.log"
python3 porting/venus/test-modem-prepare.py 2>&1 | tee "$OUT/modem-prepare-tests.log"
python3 porting/venus/test-boot-readiness.py 2>&1 | tee "$OUT/boot-readiness-tests.log"
sh porting/venus/test-km41-properties.sh porting/venus/km41-runtime.sh 2>&1 | tee "$OUT/property-tests.log"
sh porting/venus/test-km41-readiness.sh porting/venus/km41-runtime.sh 2>&1 | tee "$OUT/readiness-tests.log"
python3 porting/venus/test-ce-local.py 2>&1 | tee "$OUT/ce-helper-tests.log"
python3 porting/venus/test-km41-host.py 2>&1 | tee "$OUT/km41-host-tests.log"
python3 porting/venus/test-network-capture.py 2>&1 | tee "$OUT/network-capture-tests.log"
python3 porting/venus/test-netmgr-ready.py 2>&1 | tee "$OUT/netmgr-ready-tests.log"
${HOST_CC:-cc} -std=c11 -O2 -Wall -Wextra -Werror porting/venus/test-netmgr-init.c -o "$OUT/test-netmgr-init"
"$OUT/test-netmgr-init" | tee "$OUT/netmgr-init-tests.log"
${HOST_CC:-cc} -std=c11 -O2 -Wall -Wextra -Werror -DKM41_RPMB_TEST porting/venus/km41-rpmb.c -o "$OUT/test-rpmb"
"$OUT/test-rpmb" | tee "$OUT/rpmb-tests.log"
git diff --check
cargo test --locked -p skyward-binder -p skyward-block -p skyward-crypto -p skyward-files -p skyward-install -p skyward-net -p skyward-sys --lib 2>&1 | tee "$OUT/library-tests.log"
cargo test --locked -p skyward --bin skyward 2>&1 | tee "$OUT/policy-tests.log"
cargo test --locked -p skyward-pack 2>&1 | tee "$OUT/pack-tests.log"
cargo test --locked -p skyward-bootimg -p skyward-systems 2>&1 | tee "$OUT/systems-tests.log"
cargo test --locked -p skyward-ui --lib 2>&1 | tee "$OUT/ui-tests.log"
cargo clippy --locked -p skyward-pack --all-targets -- -D warnings 2>&1 | tee "$OUT/pack-clippy.log"
cargo build --locked --release -p skyward-pack 2>&1 | tee "$OUT/pack-build.log"
cargo ndk -t arm64-v8a --platform 30 build --locked --release -p skyward 2>&1 | tee "$OUT/android-build.log"
cp -p target/aarch64-linux-android/release/skyward "$OUT/assets/skyward"
RUSTFLAGS='-C target-feature=+crt-static' cargo ndk -t arm64-v8a --platform 30 build --locked --release -p skyward-system-init --target-dir target/system-init 2>&1 | tee "$OUT/system-init-build.log"
cp -p target/system-init/aarch64-linux-android/release/system-init "$OUT/assets/system-init"
if readelf -l "$OUT/assets/system-init" | grep -q INTERP; then
    echo 'system-init must be statically linked' >&2
    exit 1
fi
for name in binderfs-setup km41-launch km41-rpmb km41-kmsg modem-hold; do
    "$CC" -std=c11 -Os -s -Wall -Wextra -Werror -static "porting/venus/$name.c" -o "$OUT/assets/$name"
done
"$CC" -std=c11 -Os -s -Wall -Wextra -Werror -fPIE -pie porting/venus/netmgr-ready.c -ldl -o "$OUT/assets/netmgr-ready"
VENUS_KM41_CACHE="$CACHE" bash porting/venus/suspend/build.sh "$OUT/assets/network-suspend" 2>&1 | tee "$OUT/network-suspend-build.log"
VENUS_KM41_CACHE="$CACHE" bash porting/venus/netd/build.sh "$OUT/assets/network-netd" 2>&1 | tee "$OUT/network-netd-build.log"
cp -p "$CACHE/runtime.img" "$CACHE/vndk30.img" "$OUT/assets/"
cp -p porting/venus/framework-manifest.xml "$OUT/assets/"
cp -p porting/venus/boot-ld.config.txt "$OUT/assets/"
cp -p porting/venus/network-ld.config.txt "$OUT/assets/"
cp -p "$TETHERING" "$OUT/assets/tethering.img"
for script in km41-runtime ce-runtime rw-runtime boot-runtime cache-runtime modem-prepare; do cp -p "porting/venus/$script.sh" "$OUT/assets/"; done
printf '%s  %s\n' \
    9776b642132872ce4931218694c345231ecaac4490ead5d7c2f7c24e17481c17 "$OUT/assets/runtime.img" \
    8948745de8d27cae3fbdf23f6185ae4b6aec680b6a6febf2a2f0eaba1ab59b9e "$OUT/assets/vndk30.img" \
    2113920f4b3c255773f69940ff74beef8d84681aef7bafbb6bd05d797da45714 "$OUT/assets/tethering.img" | sha256sum -c -
ARGS=(recovery --partition boot --stock "$OUT/input/boot_a.img" --binary "$OUT/assets/skyward" --system-init "$OUT/assets/system-init" --config "$OUT/input/device.toml"
      --font resources/fonts/MiSansVF.ttf --mono resources/fonts/KirkMono-Regular.ttf --mono-license resources/fonts/KirkMono-OFL.txt)
FULL=(--venus-probe --venus-ui --venus-basic --venus-recovery-assets "$OUT/assets" --venus-install-capsule)
RECOVERY=(--venus-probe --venus-ui --venus-basic --venus-recovery-assets "$OUT/assets")
target/release/skyward-pack "${ARGS[@]}" "${FULL[@]}" --out "$OUT/temporary/boot.img" 2>&1 | tee "$OUT/temporary/pack.log"
python3 porting/venus/verify-recovery.py --capsule "$OUT/input/boot_a.img" "$OUT/temporary/boot.img" "$OUT/assets/skyward" "$OUT/input/device.toml" "$OUT/assets" > "$OUT/temporary/validation.json"
python3 "$TOOLS/avbtool.py" verify_image --image "$OUT/temporary/boot.img" 2>&1 | tee "$OUT/temporary/avb-verify.txt"
target/release/skyward-pack "${ARGS[@]}" "${RECOVERY[@]}" --standalone --out "$OUT/standalone/skyward-rec.img" 2>&1 | tee "$OUT/standalone/pack.log"
python3 porting/venus/verify-recovery.py --standalone "$OUT/input/boot_a.img" "$OUT/standalone/skyward-rec.img" "$OUT/assets/skyward" "$OUT/input/device.toml" "$OUT/assets" > "$OUT/standalone/validation.json"
target/release/skyward-pack "${ARGS[@]}" "${FULL[@]}" --venus-normal-entry --out "$OUT/normal/boot.img" 2>&1 | tee "$OUT/normal/pack.log"
python3 porting/venus/verify-recovery.py --capsule --normal-entry "$OUT/input/boot_a.img" "$OUT/normal/boot.img" "$OUT/assets/skyward" "$OUT/input/device.toml" "$OUT/assets" > "$OUT/normal/validation.json"
python3 "$TOOLS/avbtool.py" verify_image --image "$OUT/normal/boot.img" 2>&1 | tee "$OUT/normal/avb-verify.txt"
# Both the temporary RAM boot and the persistent normal image carry the same
# finite installer. Prove reconstruction from each, including the complete AVB footer.
target/release/skyward-pack capsule-verify "$OUT/temporary/boot.img" "$OUT/normal/boot.img" | tee "$OUT/temporary/capsule-validation.txt"
target/release/skyward-pack capsule-verify "$OUT/normal/boot.img" "$OUT/normal/boot.img" | tee "$OUT/normal/capsule-validation.txt"
# No integrated assets or writable backend selection may leak into old packaging.
target/release/skyward-pack "${ARGS[@]}" --venus-probe --venus-ui --venus-basic --out "$OUT/old-mode-isolation/boot.img" > "$OUT/old-mode-isolation/pack.log" 2>&1
python3 porting/venus/verify-entryfix.py --ui --basic "$OUT/input/boot_a.img" "$OUT/old-mode-isolation/boot.img" "$OUT/assets/skyward" "$OUT/input/device.toml" > "$OUT/old-mode-isolation/validation.json"
sha256sum -c "$OUT/inputs.sha256"
(cd "$OUT" && sha256sum assets/* > ASSETS.sha256 && sha256sum temporary/boot.img standalone/skyward-rec.img normal/boot.img input/device.toml > SHA256SUMS)
# The old known-good CE and diagnostic build pointers remain untouched.
printf '%s\n' "$OUT" > target/venus-recovery-latest.txt
printf '\nPASS: %s/standalone/skyward-rec.img (fastboot boot only; no partition AVB footer/padding)\nIntegrated partition carrier: %s/temporary/boot.img\nCandidate with original init: %s/normal/boot.img\nCold-boot, on-screen PIN and writable Data require device validation. No partitions were flashed.\n' "$OUT" "$OUT" "$OUT"
