# Venus display experiment — Rust runtime

All Skyward-owned code in this standalone display experiment is Rust.
Original Android 11 platform Bionic/libbinder/libc++ and Xiaomi/Qualcomm
closed-source vendor libraries remain external runtime dependencies, not
source code compiled into Skyward. This is a probe, not the production display
owner and it never submits the generated color blobs to DRM.

## Build

```sh
bash porting/venus/display/build-launch.sh target/venus-display-rust/display-probe-launch
python3 porting/venus/display/build.py target/venus-display-rust/display-probe
python3 porting/venus/display/ipc/build.py target/venus-display-rust/ipc
cargo test --locked -p skyward-venus-runtime --lib
cargo clippy --locked -p skyward-venus-runtime --all-targets -- -D warnings
```

`display-probe-launch`, `display-ipc-launch` and `display-ipc-qservice`
are ARM64 statically linked Rust role-dispatch programs; `display-probe` and
`display-ipc-client` are ARM64 dynamically linked Rust programs using
`/system/bin/linker64`. The dynamic programs link no NDK C++ runtime; vendor
STC symbols are loaded only from the matched original libraries after the
pinned 35-file SHA-256 check. `display-ipc-client` and `display-ipc-qservice`
use Rust Binder transactions and the original, private Android 11 vendor
servicemanager. Kernel and vendor libraries are never reimplemented as
pretend services.

`stc.rs` implements the vendor factory, C++ virtual table, default platform
`std::__1` string/vector layout and Gamut conversion FFI directly in Rust.
These offsets were previously evidenced against matched Android 11 vendor
ELFs (see `SOURCES.md`). `platform_binder.rs` initializes matching VNDK30
ProcessState using the AArch64 nontrivial-aggregate return ABI (x8).
`guard.rs` applies a strict seccomp barrier before vendor plugin loading.

**Verification boundary:** host tests check layouts, object bounds,
color-blob packing, strict Binder and packet rejection. Android ELF/ABI
build checks verify architecture, static/dynamic linker, and dependencies.
The actual vendor virtual calls, display mode list, QService publish and
calibration correctness require execution on the Xiaomi Mi 11 in isolated
Recovery. No such device verification is implied by a successful build.
Keep the outer process timeout, inspect stderr and validate every service
response; don't flash experimental display programs as a full recovery.
