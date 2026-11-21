#!/usr/bin/env python3
"""Offline validation only. Never runs code from the phone image."""
import argparse
import gzip
import hashlib
import json
import stat
import struct
from pathlib import Path

ORIGINAL_INIT = "38429485d5212ec36624e8d12808c83544f3ce41fcad21f07f5f29075b1b6d9b"
PATCHED_INIT = "047f6a7463610712ba6e63c91e7ad0f1c407a4cef7323fcac4aad109631ecabf"
SITES = {0x77ab4: bytes.fromhex("6a090054"), 0x77d28: bytes.fromhex("6a070054")}
NOP = bytes.fromhex("1f2003d5")

def check(ok, why):
    if not ok:
        raise ValueError(why)

def sha(data):
    return hashlib.sha256(data).hexdigest()

def file_sha(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for b in iter(lambda: f.read(1024 * 1024), b""):
            h.update(b)
    return h.hexdigest()

def align(n, page):
    return (n + page - 1) // page * page

def entries(raw):
    out = {}
    off = 0
    while True:
        check(raw[off:off + 6] == b"070701", "not one complete newc CPIO archive")
        fields = [int(raw[off + 6 + k * 8:off + 14 + k * 8], 16) for k in range(13)]
        size, namesize = fields[6], fields[11]
        check(namesize > 0 and off + 110 + namesize <= len(raw), "invalid CPIO name")
        namebytes = raw[off + 110:off + 110 + namesize]
        check(namebytes[-1:] == b"\0", "CPIO name is not terminated")
        name = namebytes[:-1].decode("utf-8")
        start = align(off + 110 + namesize, 4)
        end = start + size
        check(end <= len(raw), "truncated CPIO payload")
        data = raw[start:end]
        off = align(end, 4)
        if name == "TRAILER!!!":
            check(not any(raw[off:]), "extra nonzero data after CPIO trailer")
            return out
        check(name not in out, "duplicate archive name: " + name)
        # Preserve link/device metadata; inode numbers may be regenerated.
        meta = tuple(fields[1:6] + fields[7:11])
        check(not (stat.S_ISREG(fields[1]) and fields[4] > 1), "regular hardlinks need a separate validator")
        out[name] = (meta, data)

def boot(path):
    with path.open("rb") as f:
        h = bytearray(f.read(4096))
        check(h[:8] == b"ANDROID!", "not a boot image: " + str(path))
        check(struct.unpack_from("<I", h, 40)[0] == 3, "not header v3")
        check(struct.unpack_from("<I", h, 20)[0] == 1580, "unexpected header size")
        ks, rs = struct.unpack_from("<II", h, 8)
        f.seek(4096)
        kernel = f.read(ks)
        f.seek(4096 + align(ks, 4096))
        packed = f.read(rs)
        check(len(kernel) == ks and len(packed) == rs, "truncated boot section")
        check(packed[:2] == b"\x1f\x8b", "not GZIP")
    raw = gzip.decompress(packed)
    h = h[:1580]
    h[12:16] = b"\0" * 4
    return h, kernel, entries(raw), rs, len(raw)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("binary", type=Path)
    parser.add_argument("config", type=Path)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--normal", action="store_true")
    modes.add_argument("--ui", action="store_true")
    modes.add_argument("--temporary-rec", action="store_true")
    parser.add_argument("--basic", action="store_true")
    a = parser.parse_args()
    check(not a.basic or a.ui, "--basic requires --ui")
    before_h, before_k, before, _, _ = boot(a.source)
    after_h, after_k, after, rs, raw_size = boot(a.output)
    check(before_h == after_h, "header changed beyond ramdisk_size")
    check(before_k == after_k, "kernel changed")
    if a.temporary_rec:
        expected_size = 4096 + align(len(after_k), 4096) + align(rs, 4096)
        check(a.output.stat().st_size == expected_size, "standalone REC has unexpected trailing padding or truncation")
        check(a.output.stat().st_size < a.source.stat().st_size, "standalone REC was padded to the full boot partition")
        with a.output.open("rb") as f:
            f.seek(-64, 2)
            check(f.read(4) != b"AVBf", "standalone REC unexpectedly has a full-partition AVB footer")
    else:
        check(a.source.stat().st_size == a.output.stat().st_size == 201326592, "partition size changed")
    changed = []
    allowed = {"system/bin/recovery", "prop.default", "system/bin/7za", "system/bin/avbctl", "system/bin/resetprop", "system/bin/unzip", "system/bin/zstd"}
    if not a.normal:
        allowed |= {"system/bin/init", "system/etc/init/hw/init.rc"}
    for name, (meta, data) in before.items():
        check(name in after, "original entry lost: " + name)
        new_meta, new_data = after[name]
        check(meta == new_meta, "metadata changed: " + name)
        if data != new_data:
            changed.append(name)
            check(name in allowed, "unplanned content change: " + name)
    check(after["system/bin/recovery"][1] == a.binary.read_bytes(), "wrong Skyward binary")
    check(after["skyward/device.toml"][1] == a.config.read_bytes(), "wrong config")
    init_old, init_new = before["system/bin/init"][1], after["system/bin/init"][1]
    check(sha(init_old) == ORIGINAL_INIT, "unexpected source init")
    diffs = [i for i, (x, y) in enumerate(zip(init_old, init_new)) if x != y]
    if a.normal:
        check(init_new == init_old, "normal build must not patch init")
        check(before["system/etc/init/hw/init.rc"] == after["system/etc/init/hw/init.rc"], "normal build must not patch recovery init.rc")
        check("skyward/probe.sh" not in after, "probe leaked into normal build")
    elif a.temporary_rec:
        check(sha(init_new) == PATCHED_INIT, "wrong temporary REC first-stage init hash")
        check(len(init_new) == len(init_old), "first-stage init size changed")
        check(diffs == [k for start in SITES for k in range(start, start + 4)], "temporary REC changes more than the two verified instructions")
        for off, word in SITES.items():
            check(init_old[off:off + 4] == word and init_new[off:off + 4] == NOP, "first-stage instruction mismatch")
        check(before["system/etc/init/hw/init.rc"] == after["system/etc/init/hw/init.rc"], "stock recovery init.rc changed")
        check("service recovery /system/bin/recovery\n" in after["system/etc/init/hw/init.rc"][1].decode(), "stock Skyward recovery service entry is missing")
        marker = after.get("skyward/venus-temporary-rec.txt", (None, b""))[1].decode()
        check(marker.startswith("venus-temporary-rec-v1\n") and "entry=stock_recovery_service\n" in marker, "temporary REC marker is missing or invalid")
        check("skyward/probe.sh" not in after and "skyward/probe-entryfix.txt" not in after, "probe-only entry leaked into the full REC")
    else:
        check(sha(init_new) == PATCHED_INIT, "wrong patched init hash")
        check(len(init_new) == len(init_old), "init size changed")
        check(diffs == [k for start in SITES for k in range(start, start + 4)], "init changes are not exactly the two instructions")
        for off, word in SITES.items():
            check(init_old[off:off + 4] == word and init_new[off:off + 4] == NOP, "instruction mismatch")
        rc = after["system/etc/init/hw/init.rc"][1].decode()
        check("service recovery /system/bin/skyward venus-helper probe\n    disabled\n    oneshot\n" in rc, "probe entry missing")
        check("import /miui.factoryreset.rc" not in rc, "factory reset still imported")
        check("VENUS_PROBE_INIT_EARLY" in rc and "setprop sys.usb.config adb" in rc, "independent init diagnostics missing")
        check(after["system/bin/skyward"][1] == b"recovery", "wrong CLI symlink")
        check("service venus_probe_logs /system/bin/skyward venus-helper probe-logs" in rc and "skyward/probe-entryfix.txt" in after, "probe resources missing")
    rc = after["system/etc/init/hw/init.rc"][1].decode()
    ui_files = {"skyward/ui-autostart.txt"}
    source_dir = Path(__file__).resolve().parents[2] / "crates/skyward-pack/src"
    if a.ui:
        check(ui_files.issubset(after), "UI-autostart files missing")
        check(after["skyward/ui-autostart.txt"][1].startswith(b"venus-ui-autostart-v1\n"), "UI marker mismatch")
        if a.basic:
            check(after.get("skyward/basic-features.txt", (None, b""))[1] == b"venus-basic-v1\n", "basic marker missing")
        check("service venus_ui /system/bin/skyward venus-helper ui-start" in rc, "Rust UI startup missing")
        ui_rc = (source_dir / "venus_ui.rc").read_text()
        check(rc.endswith(ui_rc), "UI service stanza missing or changed")
        check(rc.count("service venus_ui ") == 1, "UI service must be unique")
        check("on boot\n    start venus_ui\n" in ui_rc, "UI is not started at boot")
        check("    oneshot\n" in ui_rc and "reboot_on_failure" not in ui_rc, "UI must not trigger reboot/restart loops")
        check("ui-probe".encode() in after["system/bin/recovery"][1], "missing isolated UI command")
        check(not any(name in after for name in ["skyward/probe.sh", "skyward/probe-logs.sh", "skyward/ui-start.sh"]), "legacy shell runtime retained")
    else:
        check(not ui_files.intersection(after), "UI files leaked into a non-UI image")
        check("service venus_ui " not in rc, "UI service leaked into a non-UI image")
    if not a.basic:
        check("skyward/basic-features.txt" not in after, "basic features leaked into non-basic image")
    report = {
        "status": "PASS", "mode": "temporary-skyward-recovery" if a.temporary_rec else ("normal-isolation" if a.normal else ("basic-functions" if a.basic else ("ui-autostart" if a.ui else "entryfix-probe"))),
        "source": str(a.source.resolve()), "output": str(a.output.resolve()),
        "source_sha256": file_sha(a.source), "output_sha256": file_sha(a.output),
        "kernel_size": len(after_k), "kernel_sha256": sha(after_k),
        "init_sha256": sha(init_new), "init_changed_offsets": [hex(x) for x in diffs],
        "partition_size": a.output.stat().st_size, "gzip_ramdisk_size": rs, "cpio_size": raw_size,
        "original_entries": len(before), "output_entries": len(after),
        "changed_original_entries": sorted(changed), "new_entries": sorted(set(after) - set(before)),
        "original_entry_metadata_preserved": True,
        "device_tested": False,
    }
    print(json.dumps(report, indent=2))

if __name__ == "__main__":
    main()
