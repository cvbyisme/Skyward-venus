# Isolated parameter-probe launcher

`display-probe-launch` is a standalone **Rust** diagnostic entry implemented in
`crates/skyward-venus-runtime/src/display/root.rs`, outside the production
display service and `km41-launch`. It never starts a HAL, network role, DRM owner or Binder service.
It creates a private mount namespace and a small read-only tmpfs root, then
executes the existing guarded probe with its arguments unchanged. The probe's
35 vendor hashes and no-ioctl seccomp filter remain mandatory.

Build with:

```sh
bash porting/venus/display/build-launch.sh /tmp/display-probe-launch
```

The static ARM64 Rust executable embeds the exact current `boot-ld.config.txt`;
its adjacent build JSON records the Rust source and configuration SHA256.
Preserve the executable basename `display-probe-launch` when deploying: the
Rust dispatcher selects its role from that name. The probe
executes as `/skyward/runtime/display-probe`, selecting the vendor section.
The Rust probe verifies the pinned 35 vendor assets directly using SHA-256,
without a helper shell or subprocess. No vendor mode is accepted on mismatch. No flat `LD_LIBRARY_PATH` or preloaded C++ library is used.

Required source mounts must already exist. The launcher does not mount block
devices or replay any journal:

| Source | Private target | Requirement |
|---|---|---|
| `/mnt/skyward-ro/vendor` | `/vendor` | Full original vendor, already read-only |
| `/mnt/skyward-ro/system/system` | `/system` | Full matching installed system, already read-only; includes toybox |
| `/mnt/skyward-ro/system_ext` | `/system_ext` | Matching installed system_ext, already read-only |
| `/skyward/run/hal/apex-runtime` | `/apex/com.android.runtime` | Matching runtime; bind remounted read-only |
| `/skyward/run/hal/apex-vndk30` | `/apex/com.android.vndk.v30` | Matching VNDK30; bind remounted read-only |
| `/dev/__properties__` | same | Property reads only |
| `/dev/vndbinder` | same | Real character inode, directly or via the fixed `/dev/binderfs/vndbinder` symlink; separate read-only, nosuid, noexec bind, with nodev cleared only here |
| Explicit DF_Param file | `/data/vendor/display/DF_Param.xml` | Only this file is exposed from Data; read-only bind |
| Optional persist `display` directory | `/mnt/vendor/persist/display`, `/persist/display` | Existing ext4 mount must report `ro` and `noload` or `norecovery` |

All binds are nonrecursive. The source's submounts are not imported. The whole
Data partition, metadata, `/proc`, `/sys`, `/dev/binder`, `/dev/hwbinder` and
DRM devices are absent. The sole device is `/dev/vndbinder`, for real
ProcessState initialization before loading the vendor plugins. A read-only
character-device mount does not itself prevent driver operations: the probe
first allows only the version/max-thread ioctls, then stacks its unchanged
all-ioctl-denial filter. No Binder service or thread pool is started.
Inherited descriptors above stderr are closed, and the environment is cleared.
The installed Android linker must support its documented fallback to absolute
`argv[0]` when `/proc/self/exe` is absent. [AOSP implementation](https://android.googlesource.com/platform/bionic/+/731631f300090436d7f5df80d50b6275c8c60a93/linker/linker_main.cpp#214)
does so. Vendor code that requires an unavailable device or proc entry must
fail visibly; this launcher does not substitute success.

## Root-controlled device procedure

Device operations belong to the supervising root session. Copy the static
launcher and the exact built guarded `display-probe` into a root-owned RAM
directory, compare their build hashes, and make them executable. Example
commands **inside the already selected Recovery shell**, after those checks:

```sh
mkdir -p /tmp/venus-display-test
chmod 700 /tmp/venus-display-test
# Root deploys the two verified ELF files here before the following command.
/system/bin/toybox timeout -s KILL 40 \
  /tmp/venus-display-test/display-probe-launch \
  /tmp/venus-display-test/display-probe \
  /data/vendor/display/DF_Param.xml \
  --uncalibrated --list \
  >/tmp/venus-display-test/list.json \
  2>/tmp/venus-display-test/list.stderr
status=$?
printf '%s\n' "$status" >/tmp/venus-display-test/list.status
```

Keep the outer deadline and reap the exact owned process. The probe also keeps
its 30-second alarm, and its hash subprocess keeps its 10-second alarm. Accept
output only on status 0 and inspect stderr for initialization/calibration
fallbacks. `--uncalibrated` deliberately exposes an empty persist view and logs
`ABSENT-UNCALIBRATED`; this cannot establish calibrated stock color equivalence.

For the first revised run, root should wrap the same launcher command with
the verified static `strace` already staged outside the private root, keeping
the existing outer deadline. For example, with its real path assigned to
`STRACE` in the outer Recovery shell:

```sh
/system/bin/toybox timeout -s KILL 40 "$STRACE" -f -tt -s 160 \
  -e trace=execve,openat,ioctl,mmap,munmap,prctl,clone,write,close \
  -o /tmp/venus-display-test/list.trace \
  /tmp/venus-display-test/display-probe-launch \
  /tmp/venus-display-test/display-probe \
  /data/vendor/display/DF_Param.xml --uncalibrated --list \
  >/tmp/venus-display-test/list.json 2>/tmp/venus-display-test/list.stderr
```

Inspect the actual sequence, not just the exit status:

1. Rust verifies all 35 pinned vendor hashes before
   `BINDER_INIT_BEGIN`.
2. Between `BINDER_INIT_BEGIN` and `BINDER_INIT_DONE`, the probe opens
   `/dev/vndbinder`, obtains the real protocol version, sets the maximum
   threads and maps read-only driver memory. No Binder RPC or thread-pool
   creation is expected. `BINDER_WRITE_READ` must never succeed.
3. Before any vendor `dlopen`, the second `PR_SET_SECCOMP` succeeds and the
   probe logs `DISPLAY_IO_GUARD_ACTIVE`. Every later ioctl, including
   `BINDER_WRITE_READ`, must fail with `EPERM`. The two-phase filter's host
   regression also verifies this with real syscalls.
4. If the plugin now requires an actual Binder service or transaction, stop
   at that failure and report it. Do not weaken either filter or replace the
   result with success.

For calibration, root may separately mount the **identified original persist
partition** read-only with journal replay disabled at
`/tmp/venus-display-persist-ro`. Verify its mount options first, then replace
`--uncalibrated` with:

```text
--persist-display /tmp/venus-display-persist-ro/display
```

The launcher independently checks the most specific containing mount in
`/proc/mounts` before entering its private root. A mere read-only RAM copy is
rejected as calibrated persist. It neither creates missing calibration nor
copies files from another phone.

Once an exact enumerated entry has been matched to the QDCM XML, replace
`--list` with `--render LIST_INDEX QDCM_ID`, redirect stdout to a binary frame,
keep stderr separately, and accept it only on exit 0. Use `unpack.py` as
documented in `README.md`. No DRM operation is performed by this experiment.

The launch log names a unique `/tmp/venus-display-probe-XXXXXX` root. Its mounts
vanish when its final process exits. After reaping, remove **only that exact
empty directory** with `rmdir`; never recursively delete a possibly live root.

## Verified scope

The local closure audit walked the two vendor XML plugins plus manager,
`libsdm-color`, `libsdm-colormgr-algo` and `libqdcm-mode-parser` recursively.
It resolved 22 private vendor/VNDK objects with no missing namespace links.
The six installed-system LLNDK boundaries are `liblog`, `libc`, `libm`, `libdl`,
`libcgrouprc` and `libvndksupport`; their current target symbol resolution still
requires the actual guarded device run. Notably `libbinder`, `libutils`,
`libcutils`, `libc++`, `libxml2`, `libstagefright_foundation` and `libhidlbase`
remain in VNDK30, while installed-system LLNDK dependencies retain their own
platform namespace. `libtinyxml2_1`, `libmiXmlParser`, `libqservice`,
`libdisplaydebug`, `libsdmutils` and `libhidltransport` come from full vendor.

Audit evidence: `venus_dump/network-refine-20261111/display-launch-closure/`.
Host real user/mount-namespace tests confirmed read-only source and root
writes fail, DF_Param-only visibility, absent proc/sys/other Binder, cleared
environment, rejection of tmpfs pretending to be calibrated persist, and
mount disappearance after exit. Evidence and rerunnable host harness are in
`venus_dump/network-refine-20261111/display-launch-test/` (original no-device
launcher) and `display-binder-preinit-test/` (revised sole-character-device
binding and stacked filters). The latter uses a real `/dev/null` inode solely
to exercise mount flags and type checking; it does not emulate or validate
Binder. These checks do not prove ARM64 vendor loading, successful Binder
initialization, calibration, or physical display behavior.
