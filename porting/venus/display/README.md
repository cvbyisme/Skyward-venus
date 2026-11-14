# Venus color parameter experiment

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

## Build and host checks

`bash porting/venus/display/build.sh OUTPUT_ELF` uses the already verified
official VNDK30 header cache from the suspend bridge. It does not mutate that
cache. The Binder headers use the same official commit and are verified
against `binder-headers.json` (downloaded into a separate display cache if
needed). `libbinder`, `libutils` and `libc++` are freshly extracted from the
matching `vndk30.img` into that separate cache; their hashes are recorded next
to the output ELF. The binary links platform `libc++.so` / `std::__1`, never NDK
`libc++_shared.so` / `std::__ndk1`. All private layout sizes and offsets are
compile-time assertions.

The host tests require the read-only Venus kernel UAPI include directory:

```sh
g++ -std=c++17 -O1 -g -Wall -Wextra -Werror -fsanitize=address,undefined \
  -I/home/abc/android_kernel_xiaomi_sm8350_venus/techpack/display/include/uapi/display/drm \
  porting/venus/display/test_blobs.cpp -o /tmp/test-display-blobs
/tmp/test-display-blobs
g++ -std=c++17 -O2 -Wall -Wextra -Werror \
  porting/venus/display/test_io_guard.cpp -o /tmp/test-display-io-guard
/tmp/test-display-io-guard
cd porting/venus/display
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest test_unpack.py
```

The tests cover actual kernel UAPI sizes, G/B/R hardware channel ordering,
IGC's 257th sample, GC pair packing, rejection of wrong sizes/values, gamut
buffer ownership and tail padding, all truncated frames, duplicate/unknown
properties, and the live seccomp guard in a host child process.

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
