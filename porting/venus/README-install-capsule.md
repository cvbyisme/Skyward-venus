# Venus current-build installation capsule

The integrated build carries `skyward/install/template.img` and
`skyward/install/manifest.json`. The template contains the normal header,
kernel, exact original first-stage init, Recovery executable, config and ramdisk
metadata. It has **no capsule inside it**. Only the bytes of the three fixed RAM
runtime images and MiSans font are omitted; the manifest records their exact
paths, lengths and SHA-256 values. Repeated installations therefore do not grow
the image.

`skyward-pack recovery --venus-install-capsule` is explicit and requires the
integrated Venus assets. The temporary forced-entry carrier, the fastboot-bootable standalone REC, and
the normal-entry image contain the same capsule. The standalone image omits
only the full-partition AVB footer/padding; it still needs the capsule because
it is the running Recovery UI used to install itself. The temporary init patch
is never part of the installable template. The build runs `capsule-verify TEMP NORMAL`
and `capsule-verify NORMAL NORMAL`; each reconstructs the full partition,
including its unsigned AVB footer, and compares all bytes.

The shared codec is `skyward_bootimg::capsule`. Its decoder bounds compressed
and decompressed sizes, CPIO entry count/name/content lengths and rejects
noncanonical templates, nested capsules, wrong device/partition/size, changed
payloads, and a different running Recovery binary. This is consistency and
integrity checking for the local build, not a signature trust chain.

The device API is `skyward_install::venus_capsule`:

- `prepare(&CurrentDevice, backup_path) -> ValidatedPlan` reconstructs solely
  from RAM, checks the running executable/config identity, pins and exclusively
  locks current-slot `boot`, and exposes `summary()` for review.
- `execute(plan, &fresh_context, &mut progress) -> Outcome | Failure` rechecks
  boot ID, slot, positive unlock evidence and snapshot state, writes and fsyncs a
  complete preimage backup on ext4/f2fs, reads it back, then writes and reads back
  the complete normal image. Backup names and held inode identities are checked
  before and after writing. Existing backups are never overwritten.
- `Failure.write_started` means the boot partition may be partly written.
  Retain the backup and report the error. There is no automatic rollback or
  reboot. The backup hash is an expected value, not a guarantee that a later
  process cannot modify the file; restoration must verify it again.

The caller owns the installer lock, keeps the backup filesystem mounted, and
reconciles contradictory property/cmdline evidence before constructing the
shared `venus_avb::CurrentDevice`. Choose a unique absolute backup path with an
existing real parent directory, and refresh that context immediately before
`execute`. Neither API accepts an arbitrary destination partition. AVB changes
are a separate operation on current-slot top-level vbmeta; this installer never
changes vbmeta, vendor_boot, another slot, or boot-control state.

Host tests and byte identity are not boot or flash proof. Before persistent
installation, the next integrated image still needs a RAM-only `prepare` check
on the actual device and the authorized normal-entry boot validation. Older
standalone images built without `--venus-install-capsule` do not gain this
feature until rebuilt; the UI now reports that missing-capsule condition
explicitly instead of exposing a raw ENOENT error.

## UI and command line

The advanced page offers two separate actions: disable AVB 2.0, and install the
current Recovery. The worker prepares the target, complete hashes and backup
path before the confirmation slider appears. Preparing/writing cannot be
cancelled halfway. Returning from a prepared plan drops it without writing the
target. Success and failure show the backup result; failures explicitly report
whether a target write may have begun.

The wrapper uses `/data/media/0/SkywardRecoveryMaintenance`, a real root-owned
0700 directory on verified unlocked owner storage; backup files are exclusive
0600 files. The actual Venus Data filesystem is F2FS; the backends admit durable
F2FS or ext4 and refuse RAM/unknown filesystems. It holds the existing Data
lifecycle lock and an open storage descriptor until completion.

With the UI stopped and USB on `adb` alone:

```sh
skyward venus-maintenance disable-avb
skyward venus-maintenance disable-avb --execute --expect-plan REVIEW_TOKEN
skyward venus-maintenance install-recovery
skyward venus-maintenance install-recovery --execute --expect-plan REVIEW_TOKEN
```

The first form never writes a persistent directory/file or a partition. If the
backup directory does not exist, it reports `parent_will_be_created=true` and
checks the existing parent filesystem. `--execute` creates the private directory
and prepares again; the review token binds boot ID, active slot/unlock evidence,
the original complete partition hash and the exact candidate/AVB flag result.
A changed token refuses the first partition write. The exclusive backup filename
is newly generated on each attempt, so one attempt cannot replace another backup.
Directory creation alone does not mean a backup completed.

The Venus UI holds `/tmp/skyward-maintenance.lock` from startup to exit. Its
maintenance worker clones the same CLOEXEC file description. Standalone
maintenance and the main binary's partition/mount/reboot/service-runner CLI operations
use the same lease, so a standalone plan requires the UI to stop. A live plan also
blocks other UI effects and recovery-control requests, and stops MTP while
preserving ADB. The read-only `service NAME` readiness query takes no lease;
`services` launches a runner and does take it. Readings and networking can continue.

An existing zip's `avbctl` child may reuse the UI installer's boundary only after
checking the current boot/PID/starttime receipt, actual exclusive `/proc/locks`
owner for that exact inode, matching executable inode, and its live ancestor
chain. A separate legacy gate remains held through every `avbctl` call and every
maintenance plan. No descriptor is made generally inheritable. Installer/terminal
work must have ended before preparation. The terminal records its entire SID,
including ordinary job-control background processes in different PGIDs. A script
records its own PGID; its leader stays unreaped until group shutdown, so a reused
numeric ID cannot be signalled. Group shutdown waits at most two seconds for live
members to disappear and reaps only the direct child. Other zombies are already
dead and remain for their actual parent to reap.

Root-private records in `/tmp/skyward-task-scopes` are created before task spawn.
They survive `shell = None`, installer completion, and a UI restart. Maintenance
checks them before preparation and again before execution; a remaining live task
is reported by scope and PID. Unconfirmed shutdown, interrupted spawn, malformed
records, or reused leader identity refuse maintenance. An empty scope can be
retired; an interrupted spawn without a recorded identity requires a fresh
Recovery boot. These records do not acquire another global lock or intercept
normal runtime/service commands. Closing output pipes and sending a signal are
never used as proof that a process exited.

These are process-cooperation locks, not isolation from an arbitrary malicious
root script, a script deliberately moving descendants into another session/group,
or an unrelated external raw block writer.
