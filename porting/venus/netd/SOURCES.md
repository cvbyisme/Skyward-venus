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

# Source and ABI basis

The implementation follows the Android11 OEM HIDL ABI used by Venus VNDK30.
The relevant official sources are:

* [NetworkController.cpp](https://android.googlesource.com/platform/system/netd/+/refs/tags/android-11.0.0_r48/server/NetworkController.cpp):
  `createPhysicalOemNetwork` allocates IDs 1..50; `createPhysicalNetworkLocked`
  creates an empty `PhysicalNetwork` and stores it in `mNetworks`.
* [PhysicalNetwork.cpp](https://android.googlesource.com/platform/system/netd/+/refs/tags/android-11.0.0_r48/server/PhysicalNetwork.cpp):
  `setPermission` with an empty interface set updates only `mPermission`;
  actual interface addition invokes `RouteController`.
* [NetworkController.h](https://android.googlesource.com/platform/system/netd/+/refs/tags/android-11.0.0_r48/server/NetworkController.h):
  net-handle magic `0xcafed00d` and OEM ID bounds.
* [NetdHwService.cpp](https://android.googlesource.com/platform/system/netd/+/refs/tags/android-11.0.0_r48/server/NetdHwService.cpp):
  HIDL argument routing, `PERMISSION_SYSTEM`, packet mark and errno-to-HAL status.
* [RouteController.cpp](https://android.googlesource.com/platform/system/netd/+/refs/tags/android-11.0.0_r48/server/RouteController.cpp):
  `modifyIncomingPacketMark`, `modifyExplicitNetworkRule`,
  `modifyOutputInterfaceRules`, `modifyPhysicalNetwork` and `modifyIpRoute`.
* [Fwmark.h](https://android.googlesource.com/platform/system/netd/+/refs/tags/android-11.0.0_r48/include/Fwmark.h)
  and [Permission.h](https://android.googlesource.com/platform/system/netd/+/refs/tags/android-11.0.0_r48/include/Permission.h):
  mark bit positions, permission value 3, preserved billing bit20.

Downloaded source hashes are retained in the evidence directory
`venus_dump/network-refine-20261111/netd-reference/android11/manifest.json`.
The independent generated C++ headers are pinned in `official-headers.json`.

Intentional recovery restrictions are documented in README.md. In particular,
the service uses an ownership comment on each exact incoming MARK rule, applies
transactional rollback/readback, and deletes only tracked routes/rules instead
of AOSP's broad per-interface table flush. Unsupported forwarding returns an
error; it is never represented by a successful stub.
