# Venus Recovery entryfix v1 — temporary probe only

## Purpose and limits

This is a candidate fix for the first-stage normal-boot diversion observed when temporarily booting the Skyward probe on Xiaomi Mi 11 (venus). It is **not a daily-use boot image, not a complete Recovery port, and has not been tested on the phone**.

Only `skyward-pack recovery --partition boot --venus-probe` enables the fix. The unflagged packaging path was separately validated to leave the stock first-stage `system/bin/init` and Recovery `init.rc` unchanged.

The probe does not automatically enter Skyward's normal recovery handler or UI, process BCB requests, switch slots, flash images, decrypt userdata, or format partitions. Existing stock hardware initialization remains. The logger may mount **metadata read-write** and write diagnostic files; the probe is not a promise of zero storage writes.

## Verified first-stage binary

- Input init length: 1,080,648 bytes.
- Input init SHA-256: `38429485d5212ec36624e8d12808c83544f3ce41fcad21f07f5f29075b1b6d9b`.
- Output init SHA-256: `047f6a7463610712ba6e63c91e7ad0f1c407a4cef7323fcac4aad109631ecabf`.
- ELF64 little-endian AArch64. In this executable segment, file offsets equal virtual addresses.

The compiler implemented the two `androidboot.force_normal_boot=1` searches using MOV/MOVK constants. That explains why searching for the complete text in this binary found no match.

| Offset | Original instruction | Probe instruction | Purpose |
|---|---|---|---|
| `0x77ab4` | `6a 09 00 54` / `b.ge 0x77be0` | `1f 20 03 d5` / `nop` | Keep Recovery module-list selection when the Recovery program exists. |
| `0x77d28` | `6a 07 00 54` / `b.ge 0x77e14` | `1f 20 03 d5` / `nop` | Skip the force-normal-boot search that leads to the root switch. |

Exactly eight bytes of `system/bin/init` change. Input length, ELF architecture, full SHA-256, both original instructions and final SHA-256 are checked. An unfamiliar or already-patched init is refused rather than patched using guessed offsets. This is intentionally specific to the inspected init, not a general solution for all venus firmware versions.

## Source changes

- `crates/skyward-pack/src/venus_first_stage.rs`: guarded two-instruction patch and five unit tests.
- `crates/skyward-pack/src/venus_probe.rs`: applies the patch only in probe mode, writes an identifying manifest and preserves the diagnostic service entry.
- `crates/skyward-pack/src/main.rs`: registers the module and requires Boot Header v3 in probe mode.
- `crates/skyward-pack/src/venus_probe.rc`: independent ADB/log startup and init kmsg stage markers.
- `crates/skyward-pack/src/venus_probe.sh`: CLI/linker, input-device and DRM/KMS diagnostics, with timeouts and kmsg markers.
- `crates/skyward-pack/src/venus_probe_logs.sh`: bounded RAM logs; optional metadata mount without fsck, format or mount_all; bounded summary persistence.
- `porting/venus/build-entryfix.sh`: build and validate only; never invokes adb or fastboot.
- `porting/venus/verify-entryfix.py`: independent GZIP/newc/Boot checks and exact entry/metadata comparison.

Earlier GZIP and `--partition boot` adaptations are retained.

## Rebuild on this computer

```bash
cd /home/abc/SKYWARD/skyward
bash porting/venus/build-entryfix.sh
```

This reuses the previously compiled Android ARM64 `target/aarch64-linux-android/release/skyward`. Only the host-side packer changed in this fix. The build helper reads the latest `venus_dump/boot_a.img`, copies it into a new output directory, builds both probe and normal-isolation images, and updates `target/venus-entryfix-latest.txt` only after successful validation. It does not flash or boot the phone.

The source input is expected to be a full 192 MiB Boot partition with its AVB footer. A newly packaged image without that layout requires separate inspection; do not pad it blindly or substitute an old Kernel.

## Confirmed output at this checkpoint

```text
/home/abc/SKYWARD/skyward/target/venus-entryfix.cacV6j/boot.img
```

Output SHA-256:

```text
3fc5fdacf9d4789875d72b43e0ee9d0589e0316ea280b3b45f60ce5df6888662
```

Kernel length: 20,605,025 bytes. The output kernel is byte-for-byte identical to the latest source DEBUG Kernel.

Kernel SHA-256:

```text
cb4030cc537f7082f6c791d567512acbb73bafa866f5b839979e60798eaea7b4
```

17 packer unit tests passed. Clippy with `-D warnings`, shell syntax checks, independent image validation, AVB self-verification, and normal-mode isolation checks passed. AVB algorithm is NONE; self-verification does not provide a Xiaomi signing key or prove bootloader acceptance.

## Temporary device test (manual, not executed by the build)

Do not flash this forced-Recovery probe to a boot slot. Do not switch slots or change vbmeta to test it.

When the phone is already in bootloader Fastboot, not fastbootd:

```bash
fastboot devices
fastboot getvar is-userspace
fastboot boot /home/abc/SKYWARD/skyward/target/venus-entryfix.cacV6j/boot.img
```

Do not follow this with another reboot command. No Skyward UI is expected; the first acceptance criteria are Recovery-root stage markers and ADB diagnostics. The patch's real startup outcome remains unverified until that test.

When ADB is online:

```bash
adb shell 'cat /skyward/probe-entryfix.txt; cat /tmp/skyward-probe/probe.log'
DEST=$(mktemp -d "$HOME/SKYWARD/venus_dump/entryfix-logs.XXXXXX")
adb pull /tmp/skyward-probe "$DEST/"
printf 'Logs: %s\n' "$DEST"
```

Expected progress markers include `VENUS_PROBE_INIT_EARLY`, `VENUS_PROBE_POST_FS`, `VENUS_PROBE_BOOT`, `VENUS_PROBE_BEGIN` and `VENUS_PROBE_END`. A rootfs/property parameter may still show `force_normal_boot=1`: the probe ignores that input at the verified binary decisions, it does not rewrite `/proc/cmdline`.

RAM logs are under `/tmp/skyward-probe` and must be pulled before reboot. If metadata mounting and permissions succeed, summaries are under `/metadata/skyward-probe/last-probe.log` and `last-nodes.txt`. Existing DEBUG Kernel export can run once metadata is mounted. A failure before mounting metadata may still prevent persistent capture.

The probe retains debug ADB properties, including unauthenticated root access where supported by the stock adbd build. Use a trusted USB connection, not this image for normal operation.

## Backup and rollback

Pre-entryfix source backup:

```text
/home/abc/SKYWARD/skyward-before-entryfix.Xf5saZ
```

The output directory also retains an input Boot copy, SHA-256 records, tests, AVB reports, validation JSON, disassembly evidence and `entryfix-changes.patch`. That patch is incremental from the pre-entryfix source backup, so reversing it keeps the earlier GZIP/Boot-partition work. It must not be reversed blindly after later edits.

To preview a source rollback (no change is applied):

```bash
cd /home/abc/SKYWARD/skyward
git apply --reverse --check target/venus-entryfix.cacV6j/entryfix-changes.patch
```

Only after that check passes and a rollback is intended, apply the same command without `--check`, then rebuild the host packer. Generated images remain separate files; reverting source does not mutate them or phone partitions.
