# Venus Skyward Recovery — Rust migration build report (2026-11-19)

This records actual local verification on Ubuntu1, not on-device test results.
The existing working tree was preserved, and no phone partition was modified.

## Scope and implementation

- Skyward-owned C/C++ source under `crates/`, `porting/` and `devices/`: **0 files**.
- Display experiment's native startup helpers: Rust `display/root.rs` and `display/ipc.rs`.
- Private QService: Rust `display/qservice.rs` plus `skyward-binder` vendor transaction support.
- Private owner IPC: Rust `display/transport.rs` / `protocol.rs`.
- STC V2 diagnostic: Rust `display/stc.rs`, `display/probe.rs`, `display/client.rs`.
- AArch64 platform Binder initialization: Rust `display/platform_binder.rs`.
- Private seccomp and hardware gamma/gamut packing: Rust `display/guard.rs` and `display/blobs.rs`.
- Host build/test tools (Python/Shell), kernel UAPI headers in compiler caches, original Android platform libraries and Xiaomi/Qualcomm vendor ELFs remain external dependencies. The local `ce-local.py` credential transport helper is host-side and is **not** packed into Recovery.

## Reproducible host verification

```
cargo test --locked -p skyward-venus-runtime --lib
cargo clippy --locked -p skyward-venus-runtime --all-targets -- -D warnings
python3 porting/venus/display/build.py target/venus-display-rust/display-probe
python3 porting/venus/display/ipc/build.py target/venus-display-rust/ipc
bash porting/venus/build-recovery.sh
```

The Rust Venus helper host suite: 52 passed, 1 ignored.
The integrated build also passed logical partition, Binder, crypto, install,
pack, UI (358 passed), temporary mount isolation, network capture and keymaster
host tests. `git diff --check` passed.

Display experiment: `display-probe` and `display-ipc-client` use Android ARM64
`/system/bin/linker64`, while the launchers and `display-ipc-qservice` are
statically linked ARM64 Rust ELF. No Skyward C/C++ source is compiled.

## Latest output (Ubuntu1)

- Standalone (only for `fastboot boot`, not partition flashing):
  `target/venus-recovery.70HR16/standalone/skyward-rec.img`
  - 199,512,064 bytes
  - SHA-256 `e8fdd2d97709bc01af0e83da17ad404d7fae1c3f6179d2e749ae6566d3b13fe6`
- Temporary partition carrier: `target/venus-recovery.70HR16/temporary/boot.img`
  - 201,326,592 bytes, embedded AVB verified.
- Normal-entry partition candidate: `target/venus-recovery.70HR16/normal/boot.img`
  - 201,326,592 bytes, embedded AVB verified and capsule byte-identity verified.

`target/venus-recovery-latest.txt` points to this validated output directory.
All checks in its `SHA256SUMS` passed.

## Remaining device-only validation

`adb devices` listed no device during this run. The vendor C++ ABI is called
from Rust directly using documented matching VNDK30 symbols and type offsets,
but no successful vendor virtual dispatch, binder registration, MI service,
real panel mode enumeration, cold boot, touchscreen, PIN prompt or writable
Data decryption has been observed on this build. Treat these as unverified
until tested on an actual Xiaomi Mi 11 in isolated Recovery. Successful offline
compilation and AVB checks are **not** a substitute for device tests.
