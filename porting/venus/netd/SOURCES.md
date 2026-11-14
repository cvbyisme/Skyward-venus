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
