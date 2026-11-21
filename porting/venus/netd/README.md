# Venus OEM netd HIDL service

This is the narrow `android.system.net.netd@1.1::INetd/default` service needed by
the stock Venus netmgr. It does not start Android framework netd, replace DNS,
change the main/default route, configure interface addresses, or control WDS.
Its Rust implementation speaks the original Android 11 HIDL wire protocol. The parent recovery modem runtime owns
its process and must keep it alive while native netmgr uses its network handles.

## Implemented contract

* `createOemNetwork` allocates a distinct, tracked empty physical network, IDs
  1..50, handle `(id << 32) | 0xcafed00d`, and packet mark `id | 0xf0000`.
  This correctly has no routing syscall: AOSP's empty PhysicalNetwork creation
  also only allocates an object and sets its permission in memory.
* Add/remove interface installs/removes real IPv4 and IPv6 incoming packet MARK
  rules and the exact AOSP Android11 explicit-network/root-oif/permission-oif
  routing rules, using table `1000 + ifindex`. Incoming marking preserves bit20.
  It requires `/system/bin/iptables` and `/system/bin/ip6tables`, MARK and comment
  extensions. Every mutation is checked by a subsequent kernel readback.
* Add/remove route supports canonical IPv4 and IPv6 prefixes, direct routes and
  same-family gateways on an interface owned by that network. It does not adopt
  preexisting routes. Empty ifname, noncanonical prefixes and special `throw` /
  `unreachable` nexthops are rejected; these are not needed by this recovery.
* Destroy removes only recorded operations; it never flushes a table or chain.
  Failed removal retains the object and receipts for retry. Preexisting tables,
  duplicate rules, stale ifindices and foreign objects fail closed.
* Both forwarding APIs return `UNKNOWN_ERROR` with `EOPNOTSUPP` diagnostics.
  This recovery does not implement IWLAN/tethering forwarding; it must not
  report success without changing and owning the corresponding kernel state.
* Callers are restricted to root or radio. Each HIDL operation is serialized.

No global default rule set, iptables chain flush, BPF program, interface address,
DNS resolver or WLAN route is installed by this service. It does not claim to be
a complete replacement for framework netd.

## Process protocol and current integration boundary

Run with no arguments as root in the launcher's verified private RAM root, with
hwbinder and the verified Android runtime tools available. Registration emits exactly
`VENUS_NETD_READY pid=<pid>` on stdout. The owner must check this PID and real HIDL
lookup from a separate bounded probe; registration can synchronously unblock
the stock netmgr initialization thread.

SIGTERM/SIGINT blocks new mutations, cleans up owned objects and exits 0 only
after success, emitting `VENUS_NETD_STOPPED pid=<pid>`. On failure it emits
`VENUS_NETD_STOP_FAILED pid=<pid> errno=<n>` and stays alive with its receipts.
Retry SIGTERM after resolving the error. Do not KILL a cleanup-failed process.
Stop native netmgr before the service so it cannot immediately create new work.

This first independently tested service retains receipts in memory. A crash or
SIGKILL is **not** an orphan-cleanup protocol. It never adopts/flushes an old
generation's rules; such a generation must be audited before restarting. The
production owner must not declare teardown complete merely because the process
disappeared. A persistent RAM receipt/recovery protocol remains an integration
requirement before arbitrary automatic crash/restart recovery is enabled.

## Build and tests

`build.sh OUTPUT_ELF` builds the static Rust service; `build.sh OUTPUT_ELF --probe`
builds the independent Rust HIDL probe. The implementation lives in
`crates/skyward-venus-runtime/src/netd`; the transport is in `skyward-binder`.
No generated C++ header or VNDK C++ linkage is needed by these helpers.

Host transaction, codec, ownership and failure tests:

```sh
cargo test -p skyward-venus-runtime netd::
```

The opt-in kernel test checks actual IPv4/IPv6 rules and routes, duplicate
rejection, normalization, iptables MARK rules, and complete cleanup. It refuses
to mutate the caller's original network namespace. From the repository root:

```sh
export SKYWARD_TEST_INITIAL_NETNS=$(stat -Lc %i /proc/self/ns/net)
unshare -Urn sh -c '
    set -eu
    ip link add netdtest0 type dummy
    ip link set netdtest0 up
    cargo test -p skyward-venus-runtime isolated_real_kernel_rules_routes_and_mark_cleanup -- --ignored
'
```

The isolated host test uses `/usr/sbin/iptables`; production uses
`/system/bin/iptables`. These tests do not establish device registration,
vendor-netmgr compatibility, or cellular connectivity.
