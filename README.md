# Skyward — Xiaomi 11 (venus) port

**Skyward-venus** is a device-focused development fork of [Skyward Recovery](https://github.com/kirklin/skyward), maintained for the Xiaomi Mi 11 (`venus`). This repository records the Venus device profile, recovery integration, build and packaging workflow, and device-specific installer work. It is not an official upstream release and is not a general-purpose image for other Xiaomi devices.

[简体中文](README.zh-CN.md) · [Venus build guide](porting/venus/README-recovery.md) · [Physical super flashing](porting/venus/README-super-flashing.md)

> **Development status:** experimental device port. A successful host build or image validation does not prove that every boot, decryption, mount, radio, or recovery operation is safe on a handset. Use the exact Venus build and follow the validation notes before considering any persistent installation. This repository does not treat a host-validated build as a device-validated release.

## Target device

| Item | Target |
| --- | --- |
| Device | Xiaomi Mi 11 |
| Codename | `venus` |
| SoC | Qualcomm Snapdragon 888 |
| Partition layout | A/B |
| Display profile | 1440 × 3200, 560 logical density |
| Device configuration | [`devices/venus.toml`](devices/venus.toml) |
| Vendor integration sources | `vendor_boot` and `vendor` |

The profile describes the intended hardware configuration; it is not a claim that every feature has passed independent testing on every firmware version. Firmware and partition layouts can differ between releases. Do not use an image or device dump from another codename.

## What this repository adds

- **Venus recovery packaging.** A device-specific build path combines the recovery binary, the Venus device profile, the selected boot image, runtime assets and the install capsule.
- **Device bring-up work.** The port contains Venus-specific integration for the display, touch input, vendor runtime and selected hardware services. Keymaster 4.1, Gatekeeper and fscrypt work is present, but credential types, cold boot, mount mode and teardown must be treated as separate device-validation items.
- **Physical `super` image installation.** The installer recognizes complete raw and Android sparse physical-super images, validates their logical-partition metadata and target geometry, and checks relevant snapshot and block-device state before writing.
- **Local-file fast path.** A physical super image selected from recovery's internal storage is read from its already-open file descriptor, bypassing the sideload FUSE path. The writer uses 4 MiB I/O buffers and retains source hashing plus full destination read-back SHA-256 verification. ADB sideload continues to use its existing transport path.

The physical-super installer is for a complete `super.img`, not for an individual `system.img`, `vendor.img` or other logical-partition image. It replaces the contents represented by both metadata slots; selecting the wrong image can make the installed system unbootable. Read [the super flashing notes](porting/venus/README-super-flashing.md) before using it.

## Build the Venus recovery

The maintained entry point is the offline build-and-package script:

```bash
cd /path/to/Skyward-venus
bash porting/venus/build-recovery.sh
```

The script does not connect to a phone, mount userdata or flash a partition. It runs host-side regression checks, builds the host packer and Android ARM64 recovery components, packages the three Venus image variants, then validates the images, capsule and AVB data. A successful run updates `target/venus-recovery-latest.txt`.

### Host requirements

- The Rust toolchain pinned by [`rust-toolchain.toml`](rust-toolchain.toml), with the Android target installed as required by the project.
- `cargo-ndk` and Android NDK r29.
- Python 3, a host C compiler, `readelf`, and the `avbtool.py` utility supplied in the local tools directory.
- The device-derived inputs listed below. These are local build inputs and are not stored in this repository.

### Build input locations

The script accepts environment overrides. Defaults are shown here exactly as used by `porting/venus/build-recovery.sh`:

| Variable | Default |
| --- | --- |
| `ANDROID_NDK_HOME` | `$HOME/Android/Sdk/ndk/29.0.14206865` |
| `VENUS_DUMP_DIR` | `$HOME/SKYWARD/venus_dump` |
| `VENUS_TOOLS_DIR` | `$HOME/SKYWARD/tools` |
| `VENUS_KM41_CACHE` | `$VENUS_DUMP_DIR/keymaster41.sSBzw7` |
| `VENUS_TETHERING_IMAGE` | `$VENUS_DUMP_DIR/network-20261110/apex-ro/com.android.tethering.img` |

The dump directory must contain `boot_a.img`, `vendor_boot_a.img` and `vbmeta_a.img`. The Keymaster cache must contain `runtime.img` and `vndk30.img`; the configured tethering image, `avbtool.py`, and the repository's Venus framework/suspend build inputs must also be present. The script stops if a required input is missing or fails its pinned hash check. If your local directory layout differs, set the corresponding environment variable rather than editing the script for a one-off machine path.

### Find the generated images

After a successful build:

```bash
OUT="$(cat target/venus-recovery-latest.txt)"
printf 'Standalone REC: %s\n' "$OUT/standalone/skyward-rec.img"
printf 'Temporary carrier: %s\n' "$OUT/temporary/boot.img"
printf 'Normal-entry candidate: %s\n' "$OUT/normal/boot.img"
sha256sum \
  "$OUT/standalone/skyward-rec.img" \
  "$OUT/temporary/boot.img" \
  "$OUT/normal/boot.img"
```

For a RAM-only boot test, use the standalone image:

```bash
fastboot boot "$OUT/standalone/skyward-rec.img"
```

`standalone/skyward-rec.img` is the fastboot-boot artifact. `temporary/boot.img` and `normal/boot.img` are packaging/installation candidates with different boot flows; do not substitute them for the standalone image. This repository's build script does not authorize `fastboot flash`. Do not flash a candidate to a persistent partition until its exact build has passed the required on-device checks and you have a recovery path.

## Tests and checks

The full build script runs the Venus Python and C regression tests, Rust tests for the affected workspace crates, strict Clippy checks for the installer and packer, Android ARM64 release builds, capsule checks and AVB verification.

For focused super-image development, run:

```bash
cargo test --locked -p skyward-lp -p skyward-vab
cargo test --locked -p skyward-install --lib
cargo clippy --locked -p skyward-install --all-targets -- -D warnings
```

These are host-side checks. They do not replace a controlled test on the target phone. In particular, verify boot, touchscreen, PIN handling, data mount/teardown, and super write/read-back behavior separately before relying on a build.

## Repository map

| Path | Purpose |
| --- | --- |
| `crates/skyward/` | Recovery application and Venus runtime entry points |
| `crates/skyward-install/` | Package installation and physical-super image handling |
| `crates/skyward-lp/` | Android logical-partition metadata parsing and validation |
| `crates/skyward-vab/` | Virtual A/B and logical-partition integration |
| `crates/skyward-ui/` | Recovery user interface |
| `crates/skyward-pack/` | Host-side image packer |
| `devices/venus.toml` | Xiaomi 11 device profile |
| `porting/venus/` | Venus runtime assets, build scripts, validation tools and porting notes |

## Firmware, secrets and generated files

The build uses device-derived firmware and runtime inputs from local directories. Do not commit personal device dumps, userdata, modem/NV backups, private keys, signing material, credentials, or proprietary blobs unless their redistribution is explicitly permitted. Build output is written under `target/` and is intentionally excluded from Git. Check the license and redistribution terms for every external component before sharing a binary image.

## Upstream and license

This project is based on [Kirk Lin's Skyward Recovery](https://github.com/kirklin/skyward). The upstream project provides the recovery foundation; this repository maintains a separate Xiaomi 11 (`venus`) adaptation and its device-specific changes. It is an independent development fork, not an official upstream branch or release. Please consult upstream for upstream-wide feature status and release information.

The repository is distributed under **GPL-3.0-or-later**; see [`LICENSE`](LICENSE). Existing upstream copyright and license notices remain applicable to the code they cover. Third-party firmware and binary components may have separate terms.
