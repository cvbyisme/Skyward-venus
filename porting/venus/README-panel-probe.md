# Venus: entryfix live validation and bounded display probe

## Live results from the current Recovery boot

Logs: `/home/abc/SKYWARD/venus_dump/entryfix-live.GSJKdW`

- ADB serial `535e451a` reports `recovery`; shell is UID 0.
- The expected `/skyward/probe-entryfix.txt` manifest is present.
- Kernel log: `First stage mount skipped (recovery mode)`.
- All init/probe markers are present through `VENUS_PROBE_END`.
- Skyward version/help and `kms --card /dev/dri/card0` all returned 0.
- DSI connector 54 reports 1440x3200 and 120/144/90/60 Hz modes.
- Touchscreen `fts` is `/dev/input/event4`, X=0..1439, Y=0..3199. This verifies discovery/axes only, not a user's touch gesture.
- Metadata logging succeeded; userdata is not mounted in the captured probe mount list.
- The original probe intentionally does NOT start the full Recovery UI. Black screen by itself is not evidence of display failure.

## Independent display-only example

Source: `crates/skyward-ui/examples/venus_panel_probe.rs`

This example uses Skyward's existing Canvas, Fonts and KmsSurface. It shows text and red/green/blue/white bars for a bounded interval, accepts no input, and never invokes recovery(), UI effects, installer, BCB processing, decryption or block-device operations. It temporarily changes the panel backlight and restores the saved value on normal completion/error. DRM mode/scanout changes are real hardware operations; no flash/reboot occurs.

Host rendering tests: 2 passed. Android ARM64 release build passed.

Live run result:

```text
PANEL_OPEN: DSI connector 54 mode "1440x3200x144x217622cmd" 1440x3200 144 Hz, crtc 132, planes [63, 83] DrmFourcc(XB24)
BACKLIGHT: 200 -> 400
PANEL_FRAME_SETTLED: 1440x3200; hold 15s; no input accepted
PANEL_PROBE_END: brightness restored; visual confirmation still required
PANEL_TEST_RC=0
```

After the run: ADB still reports Recovery, boot ID is unchanged and brightness is 200 again. Software submission/flip completion is verified; physical visibility, color accuracy and touch interaction are NOT yet visually confirmed. A separate capture attempt did not obtain an image (`the panel has no encoder`); do not treat it as a successful screenshot.

The example does not keep a UI running after it exits; returning to black is possible and expected.

## Re-run while this same temporary Recovery is still booted

The executable is already at `/tmp/venus-panel-probe`. No new Boot is required.

```bash
adb -s 535e451a shell '/tmp/venus-panel-probe 20'
```

Accepted duration: 1..60 seconds. Use a trusted USB connection. Do not run `/system/bin/recovery --help`: that program name enters the full recovery handler.

## Rebuild (does not change Boot or phone partitions)

```bash
cd /home/abc/SKYWARD/skyward
export PATH="$HOME/.cargo/bin:$PATH"
export ANDROID_NDK_HOME="$HOME/Android/Sdk/ndk/29.0.14206865"
cargo test --locked -p skyward-ui --example venus_panel_probe
cargo ndk -t arm64-v8a --platform 30 build --locked --release -p skyward-ui --example venus_panel_probe
adb -s 535e451a push target/aarch64-linux-android/release/examples/venus_panel_probe /tmp/venus-panel-probe
adb -s 535e451a shell 'chmod 0755 /tmp/venus-panel-probe'
```

Next work is to configure the actual UI and introduce a controlled bring-up path. The current `pack-test.toml` intentionally lacks display density, and the full UI runtime includes storage/USB/background work. Do not assume that `skyward ui` is an equivalent display-only test.
