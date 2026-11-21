# Rust implementation and interface provenance

The service and diagnostic sources are now implemented in
`crates/skyward-venus-runtime`; Binder transport is in `crates/skyward-binder`.
The production implementation does not compile C/C++ or load VNDK transport
libraries. The historical pinned header manifests below are retained as
interface reference data. No vendor implementation is copied into Rust.

Frozen interface names, method ordering, transaction semantics and hash chains
were checked against the Android 11 contracts:

- [System interfaces and frozen hashes](https://android.googlesource.com/platform/system/hardware/interfaces/+/refs/tags/android-11.0.0_r48/)
- [HIDL IBase and frozen hashes](https://android.googlesource.com/platform/system/libhidl/+/refs/tags/android-11.0.0_r48/transport/)

The rest of this document records the prior build input provenance for audit.

# Source and license provenance

The bridge and host tests are new code in this directory, identified with
`SPDX-License-Identifier: Apache-2.0`. They implement the AOSP HIDL contract;
they do not copy a vendor service or substitute a fake success backend.

| Build input | Official source | License provenance |
| --- | --- | --- |
| HIDL interfaces, generated headers, transport headers, platform headers | [AOSP VNDK v30, commit 5f9884aa352825291757dfd6694b874ad8c1805e](https://android.googlesource.com/platform/prebuilts/vndk/v30/+/5f9884aa352825291757dfd6694b874ad8c1805e/arm64/include/) | Per-file AOSP notices, primarily Apache-2.0; retain all original notices. |
| Platform libc++ headers | `external/libcxx/include` in the same pinned VNDK archive | Headers identify the MIT and University of Illinois open-source licenses; their original notices remain in the downloaded cache. |
| Bionic C headers | [AOSP bionic, commit 673bb02eec857ef6b004e562ab41d6b3a0e21b8a](https://android.googlesource.com/platform/bionic/+/673bb02eec857ef6b004e562ab41d6b3a0e21b8a/libc/include/) (`android-11.0.0_r48`) | Per-file BSD-style and other upstream notices, retained unchanged. |
| VNDK30 shared libraries | Existing Venus `vndk30.img` carrier selected by `VENUS_KM41_CACHE` | Existing Android/VNDK distribution; `.build.json` records each exact library hash. No library implementation is copied into this source directory. |
| Compiler, API-30 linker stubs | Installed official Android NDK 29.0.14206865 | Existing NDK licenses and notices apply. |

`official-headers.json` records each immutable official download URL and
every regular file's SHA-256. Downloaded headers are build-cache inputs, not
vendored source files. Their individual licenses remain authoritative;
this table does not relicense them or the surrounding GPLv3 Skyward project.

Contract references:

- [AOSP ISystemSuspend.hal](https://android.googlesource.com/platform/system/hardware/interfaces/+/refs/tags/android-11.0.0_r48/suspend/1.0/ISystemSuspend.hal)
- [AOSP IWakeLock.hal](https://android.googlesource.com/platform/system/hardware/interfaces/+/refs/tags/android-11.0.0_r48/suspend/1.0/IWakeLock.hal)
- [AOSP legacy power client](https://android.googlesource.com/platform/hardware/libhardware_legacy/+/refs/tags/android-11.0.0_r48/power.cpp)

The implementation intentionally leaves autosuspend policy to the surrounding
recovery runtime. The HIDL API's wake-lock ownership and release semantics are
preserved through real kernel operations and Binder object lifetime.
