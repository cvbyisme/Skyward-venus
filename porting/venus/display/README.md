# Venus color parameter experiment

**Implementation update (2026-11-19):** All Skyward-owned display experiment
runtime code and host regression logic has been ported to Rust. See
[README-RUST.md](README-RUST.md) for current build commands and verification
requirements. Older historical C++ build descriptions below are retained as
ABI research records, not current build instructions.


This directory is isolated from production Skyward, its UI, KMS owner, build and
ramdisk. `display-probe` is an ARM64 STC V2 client, **not a display service**. It
does not open DRM, create a kernel property blob, perform an atomic commit, or
claim a visible color change.

The experiment calls the original `libsnapdragoncolor-manager.so` factory
`GetScPostBlendInterface(2, 0)`, initializes the actual K2 panel name using
SDM's space-to-underscore and newline-termination contract, enumerates
the modes, then optionally invokes `ProcessOps(kScModeRenderIntent)`. The manager
loads the original QDCM and Xiaomi STC plugins according to its vendor XML.
Returned objects remain owned by the vendor `shared_ptr` control blocks until
the results have been copied; those objects are destroyed while the library is
still loaded. There is no hand-made identity LUT or flatmode substitute.

`--render` accepts the exact enumerated list index and the separately verified
QDCM XML mode ID. They are different namespaces. A render intent is also a
different number. Do not infer any one of these from another.

Supported output is limited to `PostBlendGamut`, `PostBlendIGC`, `PostBlendGC`.
Every returned asset must be understood; one unknown, duplicate or invalid
asset fails the entire render. This may require extending the adapter after
the first real output is inspected. Missing assets are not silently assigned
defaults, and no existing CRTC property is changed.

## Build and host checks (Rust)

All Skyward-owned display code is Rust. See [README-RUST.md](README-RUST.md)
for the supported build and verification workflow:

```sh
bash porting/venus/display/build-launch.sh target/venus-display-rust/display-probe-launch
python3 porting/venus/display/build.py target/venus-display-rust/display-probe
python3 porting/venus/display/ipc/build.py target/venus-display-rust/ipc
cargo test --locked -p skyward-venus-runtime --lib
cargo clippy --locked -p skyward-venus-runtime --all-targets -- -D warnings
```

The Rust test suite covers the matching kernel UAPI byte lengths,
G/B/R hardware channel order, 257th IGC sample, GC packing, owned gamut
pointers, full-frame decoding, packet truncation, vendor pin count,
ProcessState symbol identity and real seccomp enforcement in child processes.
The binary uses the original Android 11 platform C++ ABI through FFI, without
linking any NDK C++ runtime or building Skyward-owned C/C++ files. Runtime
behavior on the phone must still be validated independently.

## Controlled target experiment

Root owns device operations. Use a separate private mount/linker namespace
with the full matching vendor mounted read-only at `/vendor`, matching VNDK30
C++/Binder libraries, Android property reads, and `/system/bin/toybox`. The
current REC `/vendor` is incomplete; the actual source mount is
`/mnt/skyward-ro/vendor`. The existing network launcher role is not a display
loader role and must not be repurposed without checking its library closure.

Before loading any proprietary library, the program runs fixed-argument
`/system/bin/toybox sha256sum` and compares all 35 vendor files with the captured
manifest. There is no hash-bypass argument. A 30 s alarm is an inner limit;
the supervising root process must still impose its own kill/reap deadline.

The first uncalibrated target attempt aborted during a vendor constructor:
VNDK `ProcessState` could not open `/dev/vndbinder` under the original guard.
The revised probe first initializes the **real** VNDK `ProcessState`, after
the 35 hashes pass and before loading proprietary code. It requires the real
character device, rejects a preexisting ProcessState, checks the selected
driver, and never calls `startThreadPool` or queries a service. During this
initialization a first seccomp filter permits only `BINDER_VERSION` and
`BINDER_SET_MAX_THREADS` ioctls; `BINDER_WRITE_READ`, context-manager setup,
and all other ioctls already fail with `EPERM`.

The official Android 11 [ProcessState constructor](https://android.googlesource.com/platform/frameworks/native/+/refs/tags/android-11.0.0_r1/libs/binder/ProcessState.cpp#349)
only opens the driver, performs those two checks/settings and creates its
read-only transaction mapping. Thread-pool creation and context-manager
access are separate methods. Mapping memory does not send a transaction.

After this initialization and before `dlopen`, the experiment stacks the
original, unchanged seccomp guard that rejects **all** ioctls (including
DRM, DSI and Binder), writable opens,
process execution, mount changes and ordinary filesystem mutation. Vendor
stdout is redirected to stderr. Preserve stderr; permission or calibration
errors can mean that a plugin used a fallback despite returning success. The
guard is an experimental accident barrier, not a security sandbox for hostile
code. The initialized object remains alive until plugin objects have been
destroyed. Any actual RPC requested by a plugin must fail visibly; it is not
replaced with a fake service or successful return. Trace expectations and
the revised launcher device binding are documented in `LAUNCH.md`.

Run `display-probe --list` first. It returns the exact library-reported
gamut/gamma/intent/name/assets as JSON. After matching a returned entry with
the QDCM XML, run `display-probe --render LIST_INDEX QDCM_MODE_ID`, capturing
stdout as a binary frame and stderr separately. Accept a frame **only if the
process exits 0**. `unpack.py FRAME NEW_OUTPUT_DIR` validates and extracts the
candidate blobs. The output manifest always states `drm_applied: false`.

The real parameters are still unverified until this target experiment passes.
Additionally, `/mnt/vendor/persist/display` was not mounted during the current
capture. Its calibration must be examined through a root-controlled read-only
mount before claiming equivalence to the phone's calibrated system color.
No password or user credential belongs in any capture.

## Packing constraints

The V2 vendor produces 257-entry IGC (12-bit), 1024-entry GC (10-bit), and
117936-byte GamutConfig. The first two are packed in the original hardware
G/B/R order, with IGC's final samples at offsets 3084/3088/3092. The gamut
adapter uses the original exported pure `PPAlgorithm::ConvertGamutCoeff`
against a copied full 17-cube table and correctly allocated, owned SDE arrays.
The resulting array pointers and sizes must remain unchanged. It then packs
the 39536-byte Venus `drm_msm_3d_gamut` layout, zeroing padding.

Disabled assets carry an explicit disabled record with zero payload, separate
from an enabled blob containing zero-valued data. A future KMS integration
must preserve this distinction, keep the existing single DRM owner, account
for every affected property, and restore its previous state on failure.

Original settings use these custom render intents: increased contrast 256,
standard 257, automatic contrast 258. The XML contains multiple blend-space
variants per intent. Neither XML mode 2 (`vivid`) nor the panel flatmode bit
is a valid shortcut for a Xiaomi settings selection.
