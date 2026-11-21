# Venus kernel wake-lock HIDL bridge

`network-suspend` provides the official
`android.system.suspend@1.0::ISystemSuspend/default` interface required by the
Venus vendor network services. It acquires and releases real kernel wake locks
through `/sys/power/wake_lock` and `/sys/power/wake_unlock`. It does not control
autosuspend or write `/sys/power/state`.

Each concurrent `IWakeLock` has its own kernel token, including when clients
use the same debug name. Explicit release is idempotent. Binder object
destruction releases the token, including when a client dies. Tokens use
`skyward-suspend-<service-pid>-<slot>`. A slot returns to a thread-safe pool only
after a successful kernel release; a previously released Binder object cannot
release a new holder of that slot. This bounds persistent kernel wake-source
names by peak concurrency within one service instance, rather than by the
number of acquisitions. The Venus kernel does not garbage-collect these names.

Backend errors terminate the service with exit code 74 and a
`VENUS_SUSPEND_FATAL` log. The Android 11 legacy power client handles Binder
`DEAD_OBJECT`, but does not reliably handle an ordinary HIDL exception or a
successful null handle. A failed acquire never returns a successful null
handle, and a failed release never discards ownership or reuses its slot.
Malformed wake-lock type values are rejected with `EX_ILLEGAL_ARGUMENT`.

The parent owner must stop its network clients before recovering from bridge
death, wait for the exact bridge process to exit, then release only tokens
with that instance's full PID prefix. Kernel wake locks survive service death.
The parent must complete that cleanup before starting a replacement or
allowing PID reuse. Normal radio-off keeps the bridge alive so existing
vendor clients do not retain a dead cached service proxy. Startup refuses
another registered service and takes an exclusive root-owned 0600 lock file.

After successful HIDL registration, stdout contains and flushes exactly:

```text
VENUS_SUSPEND_READY pid=<pid> prefix=skyward-suspend-<pid>-
```

The parent also verifies actual HIDL discovery and the child PID; this log
alone is not proof of usable service registration.

## Build

From the Skyward repository root:

```sh
bash porting/venus/suspend/build.sh target/venus-suspend/network-suspend
bash porting/venus/suspend/build.sh target/venus-suspend/network-suspend-probe --probe
```

The implementation is Rust in `crates/skyward-venus-runtime/src/suspend.rs`;
the diagnostic client is `suspend_probe.rs`. `skyward-binder` sends and receives
the Android 11 HIDL wire format directly, including single-direction release,
local Binder object ownership, nested buffers, and the frozen AOSP hash chain.
No C++ transport shim or VNDK shared-library dependency is built into the helper.

The build uses the Android NDK linker and the Rust `aarch64-linux-android`
target. `ANDROID_NDK_HOME` selects the NDK. Outputs are static ARM64 executables;
each adjacent `.build.json` records source and ELF SHA-256 values. Recovery
packaging deduplicates the service entrypoint against the Rust multicall binary.
Actual HIDL registration and vendor interoperability still need device testing.

## Validation

```sh
cargo test -p skyward-venus-runtime suspend::
```

They cover independent holders, repeated and concurrent release, destruction,
failed acquire/release, 10,000 sequential reuse cycles, concurrent reuse, and
stale released objects. These tests do not prove HIDL transport or real kernel
behavior. The separate direct-HIDL probe is used in recovery to test release,
object destruction, client exit, and supervised bridge-death cleanup without
asking the kernel to suspend. Run its isolated lifecycle checks before starting
vendor network clients; concurrent client activity prevents attributing kernel
tokens to individual opaque HIDL objects by set difference alone.

See [SOURCES.md](SOURCES.md) for official source pins and license provenance.
