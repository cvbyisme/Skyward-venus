# venus: actual Skyward menus / input bring-up

The user confirmed that the display-only test's text, colours and orientation look normal. The next stage reuses the actual Skyward App, Fonts, KmsSurface and Inputs, but NOT the production `ui::run` effect executor.

## Added implementation

- `crates/skyward/src/main.rs`: Android `ui-probe` subcommand. Existing `ui` and `recovery` paths are unchanged.
- `crates/skyward/src/ui_probe.rs`: bounded RAM-only menu/input loop; checks the entryfix manifest and venus device config; exclusive process lock; SIGINT/SIGTERM exit and brightness restoration.
- `crates/skyward-ui/src/probe.rs`: in-memory denial policy for EVERY external UI effect, with a default-deny catch-all. No installer, BCB handling, shell, wipe, decryption, mount/unmount, merge, slot switching, reboot, networking or USB reconfiguration is dispatched. Haptics are ignored. Queries that need storage return an explicit unavailable error rather than fabricated data.
- `porting/venus/ui-probe.toml`: observed 1440x3200 display size, provisional 560 logical DPI, no touch sysfs writes or HAL services. This is not a claim about physical PPI or final optimal sizing.
- `porting/venus/start-ui-probe.sh`: deploys only to `/tmp` and starts a bounded test, without flashing or rebooting.

The status-bar clock is `--:--` on purpose: this runner does not change RTC/time or pretend that the current Recovery wall clock is correct. The visible bottom banner labels this as a UI test. Settings changes are in memory only. The brightness slider is limited to a conservative low part of the backlight's raw range. The UI is interactive, but it is not a usable full Recovery yet.

## Verified in this run

- 330 `skyward-ui` library tests passed, including four new denial-policy tests.
- Android ARM64 release compilation passed.
- Live first frame settled: 1440x3200 at logical density 560.
- Live input: fts, X=0..1439 Y=0..3199, ten slots, slot zero followed by the existing input reader.
- Actual touch events and Home/Settings/About/Specs/Advanced page transitions appeared in logs without input injection.
- ADB remained running; boot ID and mount list were identical before and after startup.
- Original boot_a.img, vendor_boot_a.img, vbmeta_a.img and the previous entryfix boot.img hashes were unchanged.
- The first rendered menu frame was pulled from RAM and inspected. Physical touch alignment, scroll feel and all UI interactions are not exhaustively validated.

Logs and first-frame dump on Ubuntu:

```
/home/abc/SKYWARD/venus_dump/ui-live.N0Yaw1
```

Source backup before this UI change:

```
/home/abc/SKYWARD/skyward-before-ui-probe.WGBct2
```

## Run again in the same temporary Recovery

```bash
cd /home/abc/SKYWARD/skyward
bash porting/venus/start-ui-probe.sh 900
```

The command refuses a duplicate active test. It reads the host Android binary already built at `target/aarch64-linux-android/release/skyward`. It does not rebuild automatically. A maximum of 1800 seconds is permitted; default is 900 seconds. When it exits, the original backlight level is restored and the screen can go black. ADB and the independent logger remain.

Device files:

```
/tmp/skyward-ui-probe-v1
/tmp/venus-ui-probe.toml
/tmp/skyward-ui-probe/ui.log
/tmp/skyward-ui-probe/pid
/tmp/skyward-ui-probe/first-frame.rgba
```

Stop only this test, with a checked PID:

```bash
adb -s 535e451a shell '
p=$(cat /tmp/skyward-ui-probe/pid)
case "$p" in ""|*[!0-9]*) exit 1;; esac
if [ -r /proc/$p/cmdline ] && grep -q skyward-ui-probe-v1 /proc/$p/cmdline; then
    kill -TERM "$p"
fi
'
```

The old Boot does not auto-start this new UI. No new Boot was generated or flashed in this step. A future automatic-UI image should be introduced explicitly after user confirmation, keeping the default-deny boundary until the production mount/crypto/install paths are adapted.
