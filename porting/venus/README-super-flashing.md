# Venus physical super image flashing

This change adds full physical `super` image installation to Skyward Recovery for the Xiaomi 11 (`venus`). It is distinct from flashing an individual logical partition such as `system`, `vendor`, or `product`.

## Supported images

- Raw physical super images with valid liblp geometry and both A/B metadata slots.
- Android sparse physical super images. Sparse input is exposed as a seekable expanded stream; the expanded image is never allocated in RAM.
- The expanded image size and the liblp block-device size must exactly match the phone's physical `super` partition.
- Both metadata slots must be clean, describe one physical block device named `super`, and contain their corresponding system partition. Linear extents must remain within their declared block device.

A sparse logical-partition image is not treated as a physical super image. The parser validates the sparse chunk table and any declared CRC32 checksums before offering the image.

## Internal-storage performance path

When the user selects a local file in Recovery, `run_file` inspects the already-open file descriptor. If it is a complete physical super image, the installer passes that descriptor directly to `SuperImage` and the block writer. It bypasses the `/sideload` FUSE file service, avoiding its 64 KiB block requests, per-block bookkeeping, and extra userspace copy for multi-gigabyte images.

The common block writer and read-back verifier use a 4 MiB I/O buffer (`crates/skyward-install/src/image.rs::IO_CHUNK`). Sparse checksum verification uses the same buffer size. The USB ADB sideload path remains on its existing transport/FUSE flow; this direct-file optimization applies to internal-storage/locally selected files.

The writer still hashes the source stream while writing, flushes/synchronizes the destination, reads the written physical partition back, and compares the full SHA-256 digest. The larger buffer reduces I/O overhead; it does not disable integrity verification.

## Safety gates

Before the first write, the installer:

1. Checks the battery policy and refuses installation when charge is insufficient.
2. Rejects an active OTA snapshot/merge state.
3. Validates the complete expanded image and both LP metadata copies against the physical partition size.
4. Shows an explicit full-container warning; a physical super image has no A/B slot selector because it replaces both slots.
5. Waits for the user's explicit confirmation.
6. Releases the Venus private read-only source mappings and the current-slot logical-partition mappings.
7. Rechecks snapshot state and proves the physical super partition is neither mounted nor held by a device-mapper mapping.
8. Writes the image and performs full read-back SHA-256 verification.

Do not interrupt power during a super write. Only use an image built for the exact target device and partition layout. Host tests and image parsing are not a substitute for device validation.

## Build the Venus Recovery images

The repository's reproducible offline build entrypoint is:

```bash
cd /home/abc/SKYWARD/skyward
bash porting/venus/build-recovery.sh
```

The script uses the following environment variables; the defaults match the current Ubuntu development layout:

| Variable | Default |
| --- | --- |
| `ANDROID_NDK_HOME` | `$HOME/Android/Sdk/ndk/29.0.14206865` |
| `VENUS_DUMP_DIR` | `$HOME/SKYWARD/venus_dump` |
| `VENUS_TOOLS_DIR` | `$HOME/SKYWARD/tools` |
| `VENUS_KM41_CACHE` | `$VENUS_DUMP_DIR/keymaster41.sSBzw7` |
| `VENUS_TETHERING_IMAGE` | `$VENUS_DUMP_DIR/network-20261110/apex-ro/com.android.tethering.img` |

Required host tools include the pinned Rust toolchain in `rust-toolchain.toml`, `cargo-ndk`, Android NDK r29, Python 3, a host C compiler, `readelf`, and the `avbtool.py` file in `VENUS_TOOLS_DIR`. The build also requires the Venus boot/vendor_boot/vbmeta dumps, the Keymaster runtime/VNDK cache, and the tethering image listed above. Missing inputs cause the script to stop before packaging.

The build runs the Python/C/Rust regression tests, strict Clippy checks for the installer and packer, Android ARM64 release build, static `system-init` build, image/capsule validation, and AVB verification. It does not use ADB or fastboot and does not flash any phone partition. Python bytecode is directed into the build output directory so the source tree stays clean.

On success, the script prints the output directory and updates `target/venus-recovery-latest.txt`. Resolve the exact paths with:

```bash
OUT="$(cat target/venus-recovery-latest.txt)"
printf 'Standalone REC: %s\n' "$OUT/standalone/skyward-rec.img"
printf 'Temporary carrier: %s\n' "$OUT/temporary/boot.img"
printf 'Normal-entry candidate: %s\n' "$OUT/normal/boot.img"
sha256sum "$OUT/standalone/skyward-rec.img" "$OUT/temporary/boot.img" "$OUT/normal/boot.img"
```

For RAM-only testing, use the standalone image with its full path:

```bash
fastboot boot "$OUT/standalone/skyward-rec.img"
```

Do not substitute `normal/boot.img` or `temporary/boot.img` for the standalone fastboot-boot image. Do not run `fastboot flash` unless a separate device-validation and installation procedure explicitly authorizes it.

## Test commands for the super-image code

```bash
cargo test --locked -p skyward-lp -p skyward-vab
cargo test --locked -p skyward-install --lib
cargo clippy --locked -p skyward-install --all-targets -- -D warnings
```

The full `build-recovery.sh` runs these checks as part of the complete packaging flow.
