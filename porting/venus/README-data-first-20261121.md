# Venus Skyward Data-first boot verification — 2026-11-21

## Scope

The temporary/standalone Recovery now launches its owned `venus_security`
service at the init `on boot` trigger. The original Android 11
`wait_for_keymaster` init service was an unconditional restart loop which
failed every five seconds because the stock binary could not find
`libbinder.so` in this isolated Recovery. Only the temporary image omits this
legacy service. The **normal Android-entry image retains the original init
service** and does not auto-start `venus_security`.

The installer capsule builder first normalizes the temporary init/legacy-wait
configuration back to the normal-entry variant, then restores the temporary
carrier. Both capsule reconstruction paths return the same verified normal
boot image; original stock boot/kernel/vendor_boot inputs remain unchanged.

The mounted Data lifecycle is unchanged and retains original metadata unwrap,
DM-default-key creation, fstab-specified fsck, F2FS mount, DE and PIN-based
CE unlock (no credential bypass). `venus_rw::open()` emits duration_ms markers
for HAL readiness, metadata unwrap, DM creation, fsck and mount. The UI no
longer mislabels all of these stages "Reading the screen lock".

## Measured device result

Device: Xiaomi Mi 11 (`venus`), serial `535e451a`, slot a. Image booted by
`fastboot boot` only; no partition flashed. Times are kernel monotonic seconds
from two separate temporary Recovery starts, not a controlled benchmark.

| Phase | Old image | New image |
|---|---:|---:|
| Runtime cache completed | 28.54 s | 26.22 s |
| Security HAL start | 28.62 s | 26.30 s |
| Metadata unwrap began | 33.72 s | 31.41 s |
| Writable F2FS mount completed | 70.91 s | 31.56 s |
| User CE storage ready | 80.29 s | 41.64 s |

New data worker (`VENUS_DATA_TIMING`) reported:

- `security_ready_ms=25557` (worker started at 5.827 s; includes waiting for
  prestarted security service, a fully verified ~2.7-GiB RAM copy and HALs)
- `metadata_unwrap_ms=13`
- `dm_create_ms=11`
- `fsck_ms=22`, `fsck_status=0`
- `mount_ms=59`
- `open_total_ms=25814`

The measured new boot's `/data` mount was writable F2FS; directories
`/data/media/0` and `/data/system_ce/0` were accessible and the kernel log
ended with `the users whose own files are still closed: []`.

The old boot had no per-stage breakdown for 33.72..70.80s. Do **not** assert
that the complete 37-second gap was definitely fsck; differences in filesystem
state, caches and credential interaction between boots could affect timings.
The time from DE readiness to CE readiness also depends on user credential
entry, which the Recovery must not silently bypass.

## Build and checks

`bash porting/venus/build-recovery.sh` exited 0. `target/venus-recovery.ZBsNXJ`
is current (`target/venus-recovery-latest.txt`). All SHA256SUMS passed.
Temporary and normal 192MiB images passed embedded AVB; capsule_normal_byte_identity
passed twice with SHA256
`048fbdff34a49e9094f66b9793a7276bb31e333c3ae4490b25f1c25ab425e9ca`.
The new standalone image is
`target/venus-recovery.ZBsNXJ/standalone/skyward-rec.img`, 199516160 bytes,
SHA256 `fb047ad4aacc323caa2118a2d38131de38c297bb87571b57179da2e1880e9519`.

Focused tests: 4 Skyward pack tests, 358 UI tests, 10 Venus Data lifecycle
tests; full bundle's additional offline checks passed. Debug logs copied to
`target/data-first-live-20261121/` on the Ubuntu host. No PIN or personal
file contents are included in this report.

## Remaining speed limit

The ~26-second verified RAM cache/HAL warmup remains on the critical path.
It is intentional: source partitions must be released safely during package
installation while HAL processes remain in read-only private mount namespaces
and their verified libraries live in RAM. Skipping this copy or fsck outright
would break the source-release safety contract or risk the filesystem.
Further performance improvements require a new, independently verified
subset/late-cache architecture and live tests for OTA super-partition flashing.
