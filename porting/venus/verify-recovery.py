#!/usr/bin/env python3
"""Offline validation of integrated venus Recovery; never executes image code."""
from __future__ import annotations
import argparse
import hashlib
import importlib.util
import json
import stat
import struct
import xml.etree.ElementTree as ET
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("entrycheck", HERE / "verify-entryfix.py")
v = importlib.util.module_from_spec(spec)
spec.loader.exec_module(v)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("source", type=Path)
    p.add_argument("output", type=Path)
    p.add_argument("binary", type=Path)
    p.add_argument("config", type=Path)
    p.add_argument("assets", type=Path)
    p.add_argument("--normal-entry", action="store_true")
    p.add_argument("--capsule", action="store_true")
    p.add_argument("--standalone", action="store_true")
    a = p.parse_args()
    v.check(not (a.standalone and (a.normal_entry or a.capsule)),
            "standalone REC cannot use normal-entry or installer-capsule mode")
    bh, bk, before, _, _ = v.boot(a.source)
    ah, ak, after, compressed, raw = v.boot(a.output)
    v.check(bh == ah, "Boot header changed beyond ramdisk_size")
    v.check(bk == ak, "DEBUG kernel changed")
    if a.standalone:
        expected_size = 4096 + v.align(len(ak), 4096) + v.align(compressed, 4096)
        v.check(a.output.stat().st_size == expected_size, "standalone image has noncanonical section padding or extra partition data")
        v.check(a.output.stat().st_size < a.source.stat().st_size, "standalone image still fills the boot partition")
        with a.output.open("rb") as f:
            f.seek(-64, 2)
            v.check(f.read(4) != b"AVBf", "standalone image unexpectedly carries a full-partition AVB footer")
    else:
        v.check(a.source.stat().st_size == a.output.stat().st_size == 201326592, "wrong complete partition size")
    allowed = {"system/bin/init", "system/etc/init/hw/init.rc", "system/bin/recovery", "prop.default",
               "system/bin/7za", "system/bin/avbctl", "system/bin/resetprop", "system/bin/unzip", "system/bin/zstd"}
    changed = []
    for name, (meta, data) in before.items():
        v.check(name in after, "original entry lost: " + name)
        new_meta, new_data = after[name]
        v.check(meta == new_meta, "original permissions/type/ownership changed: " + name)
        if data != new_data:
            changed.append(name)
            v.check(name in allowed, "unplanned change: " + name)
    v.check(after["system/bin/recovery"][1] == a.binary.read_bytes(), "wrong executable")
    v.check(after["system/bin/skyward"][1] == b"recovery", "wrong CLI symlink")
    v.check(after["skyward/device.toml"][1] == a.config.read_bytes(), "wrong device config")
    meta, program = after["skyward/system-init"]
    v.check(program == (a.assets / "system-init").read_bytes() and program.startswith(b"\x7fELF"), "wrong second-system first program")
    v.check(stat.S_ISREG(meta[0]) and meta[0] & 0o777 == 0o755 and meta[1:3] == (0, 0), "wrong system-init metadata")
    expected_init = v.ORIGINAL_INIT if a.normal_entry else v.PATCHED_INIT
    v.check(v.sha(after["system/bin/init"][1]) == expected_init, "wrong first-stage init for selected boot mode")
    if a.normal_entry:
        v.check(after["system/bin/init"] == before["system/bin/init"], "normal-entry candidate must restore original init exactly")
    else:
        oi, ni = before["system/bin/init"][1], after["system/bin/init"][1]
        v.check(len(oi) == len(ni), "init length changed")
        v.check([i for i, (x,y) in enumerate(zip(oi,ni)) if x != y] == [k for x in v.SITES for k in range(x,x+4)], "temporary init patch is not precisely the known eight bytes")
    marker = after["skyward/venus-recovery.txt"][1].decode()
    v.check(marker.startswith("venus-recovery-rw-v1\n") and "data_mode=rw\n" in marker, "RW capability marker missing")
    entry = "original_init" if a.normal_entry else "temporary_forced_recovery"
    v.check(f"entry={entry}\n" in marker, "marker lies about boot mode")
    wrapper = after["skyward/ui-start.sh"][1].decode()
    v.check("exec /system/bin/skyward venus-recovery --config /skyward/device.toml" in wrapper, "full Recovery UI not selected")
    v.check("ui-probe" not in wrapper, "still starts isolated UI probe")
    v.check(after["skyward/rw-runtime.sh"][1] == (a.assets / "rw-runtime.sh").read_bytes(), "runtime entry differs")
    rc = after["system/etc/init/hw/init.rc"][1].decode()
    v.check(rc.count("service venus_ui ") == 1 and "on boot\n    start venus_ui\n" in rc, "UI autostart service missing/duplicated")
    v.check(rc.count("service venus_security ") == 1, "independent security service missing/duplicated")
    v.check("service venus_security /system/bin/sh /skyward/security-start.sh\n    disabled\n    oneshot\n" in rc, "wrong security service lifecycle")
    v.check(rc.count("service venus_modem ") == 1, "independent modem owner missing/duplicated")
    modem_service = "service venus_modem /system/bin/skyward venus-modem\n    disabled\n    oneshot\n    user root\n    group root system radio log\n    seclabel u:r:recovery:s0\n    stdio_to_kmsg\n"
    v.check(modem_service in rc and "start venus_modem" not in rc, "modem must have an explicit independent owner without boot autostart")
    security = after["skyward/security-start.sh"][1].decode()
    v.check("set -eu\n" in security and "sh /skyward/rw-runtime.sh\n" in security and "while :; do sleep 3600; done\n" in security, "HAL process group owner must survive setup")
    v.check("service recovery /system/bin/sh /skyward/probe.sh\n    disabled\n    oneshot\n" in rc, "independent diagnostic service lost")
    v.check("import /miui.factoryreset.rc" not in rc, "stock automatic factory-reset path still imported")
    v.check("reboot_on_failure" not in rc, "unexpected reboot-on-failure")
    v.check("system/etc/init/skyward-mtp.rc" in after, "MTP function setup missing")
    manifest = after["skyward/runtime/SHA256SUMS"][1].decode().splitlines()
    names = []
    for line in manifest:
        digest, name = line.split()
        v.check("/" not in name and name not in names, "unsafe runtime checksum path")
        names.append(name)
        meta, payload = after["skyward/runtime/" + name]
        v.check(stat.S_ISREG(meta[0]) and meta[1:3] == (0,0), "runtime not a root-owned file")
        expected_mode = 0o644 if name in {"boot-ld.config.txt", "network-ld.config.txt"} else (0o600 if name.endswith((".img", ".xml", ".txt")) else 0o700)
        v.check(stat.S_IMODE(meta[0]) == expected_mode, "runtime file access differs: " + name)
        v.check(hashlib.sha256(payload).hexdigest() == digest, "embedded runtime checksum mismatch: " + name)
        v.check(payload == (a.assets / name).read_bytes(), "runtime asset was replaced: " + name)
    v.check(set(names) == {"binderfs-setup", "km41-launch", "km41-rpmb", "km41-kmsg", "runtime.img", "vndk30.img", "framework-manifest.xml", "km41-runtime.sh", "ce-runtime.sh", "boot-ld.config.txt", "boot-runtime.sh", "cache-runtime.sh", "modem-hold", "modem-prepare.sh", "network-ld.config.txt", "tethering.img", "network-suspend", "netmgr-ready", "network-netd"}, "incomplete HAL/radio bundle")
    suspend = after["skyward/runtime/network-suspend"][1]
    v.check(len(suspend) >= 64 and suspend.startswith(b"\x7fELF\x02\x01") and suspend[18:20] == b"\xb7\x00", "network suspend helper is not ARM64")
    v.check("network-suspend" not in rc and "network_suspend" not in rc, "network suspend lifecycle must remain owned by the modem runtime")
    ready = after["skyward/runtime/netmgr-ready"][1]
    v.check(len(ready) >= 64 and ready.startswith(b"\x7fELF\x02\x01") and ready[18:20] == b"\xb7\x00", "netmgr readiness helper is not ARM64")
    v.check("netmgr-ready" not in rc and "netmgr_ready" not in rc, "netmgr readiness must remain an owned one-shot probe")
    netd = after["skyward/runtime/network-netd"][1]
    v.check(len(netd) >= 64 and netd.startswith(b"\x7fELF\x02\x01") and netd[18:20] == b"\xb7\x00", "network netd helper is not ARM64")
    v.check("network-netd" not in rc and "network_netd" not in rc, "network netd lifecycle must remain owned by the modem runtime")
    framework = ET.fromstring(after["skyward/runtime/framework-manifest.xml"][1])
    netd_hal = [hal for hal in framework.findall("hal") if hal.findtext("name") == "android.system.net.netd"]
    v.check(len(netd_hal) == 1 and netd_hal[0].get("format") == "hidl" and netd_hal[0].findtext("transport") == "hwbinder" and netd_hal[0].findtext("fqname") == "@1.1::INetd/default", "missing exact OEM network HIDL declaration")
    declarations = [hal for hal in framework.findall("hal") if hal.findtext("name") == "android.system.suspend"]
    v.check(len(declarations) == 1 and declarations[0].get("format") == "hidl"
            and declarations[0].findtext("transport") == "hwbinder"
            and declarations[0].findtext("fqname") == "@1.0::ISystemSuspend/default", "missing exact HIDL suspend declaration")
    linker = after["skyward/runtime/boot-ld.config.txt"][1].decode().splitlines()
    v.check("dir.vendor = /skyward/runtime/" in linker and "dir.vendor = /vendor/bin/" in linker,
            "network helpers must use the existing isolated vendor ABI without replacing the HAL mapping")
    v.check(v.sha(after["skyward/runtime/tethering.img"][1]) == "2113920f4b3c255773f69940ff74beef8d84681aef7bafbb6bd05d797da45714", "unexpected platform tethering image")
    holder = after["skyward/runtime/modem-hold"][1]
    v.check(holder.startswith(b"\x7fELF\x02\x01") and holder[18:20] == b"\xb7\x00", "modem holder is not ARM64")
    v.check(len(holder) >= 64, "truncated modem holder ELF")
    phoff, = struct.unpack_from("<Q", holder, 32)
    phsize, phnum = struct.unpack_from("<HH", holder, 54)
    v.check(phsize >= 56 and phnum > 0 and phoff + phsize * phnum <= len(holder), "invalid modem holder program headers")
    v.check(all(struct.unpack_from("<I", holder, phoff + i * phsize)[0] != 3 for i in range(phnum)), "modem holder must be statically linked")
    v.check(all(after[name] == before[name] for name in before if name.startswith("first_stage_ramdisk/")), "normal-boot first-stage files changed")
    capsule = None
    capsule_paths = {"skyward/install/template.img", "skyward/install/manifest.json"}
    if a.capsule:
        v.check(capsule_paths <= after.keys(), "finite installer capsule missing")
        for name in capsule_paths:
            meta, _ = after[name]
            v.check(stat.S_ISREG(meta[0]) and stat.S_IMODE(meta[0]) == 0o600 and meta[1:3] == (0, 0), "unsafe capsule metadata")
        template = after["skyward/install/template.img"][1]
        encoded = after["skyward/install/manifest.json"][1]
        v.check(len(template) <= 96 * 1024 * 1024 and len(encoded) <= 16384, "capsule exceeds bounds")
        capsule = json.loads(encoded)
        v.check(capsule["version"] == 1 and capsule["device"] == "venus" and capsule["carrier"] == "boot" and capsule["partition_size"] == 201326592, "capsule identity mismatch")
        v.check(capsule["template_sha256"] == v.sha(template) and capsule["kernel_sha256"] == v.sha(ak), "capsule template/kernel mismatch")
        v.check(capsule["native_init_sha256"] == v.ORIGINAL_INIT and capsule["recovery_sha256"] == v.sha(a.binary.read_bytes()) and capsule["config_sha256"] == v.sha(a.config.read_bytes()), "capsule running-build identity mismatch")
        v.check([ref["path"] for ref in capsule["payloads"]] == ["skyward/runtime/runtime.img", "skyward/runtime/vndk30.img", "skyward/runtime/tethering.img", "skyward/MiSansVF.ttf"], "unsafe capsule payload references")
        for ref in capsule["payloads"]:
            data = after[ref["path"]][1]
            v.check(ref["size"] == len(data) and ref["sha256"] == v.sha(data), "capsule RAM payload mismatch")
    else:
        v.check(not capsule_paths & after.keys(), "unexpected capsule without explicit validation")
    print(json.dumps({"status":"PASS", "scope":"standalone fastboot-bootable venus REC image" if a.standalone else "offline integrated venus Recovery image", "original_first_stage":a.normal_entry,
                      "kernel_sha256":v.sha(ak), "boot_sha256":v.file_sha(a.output), "ramdisk_compressed_bytes":compressed,
                      "ramdisk_uncompressed_bytes":raw,"image_size":a.output.stat().st_size,"runtime_assets":names,"original_entries_changed":changed,
                      "installer_capsule":capsule, "data_mode":"rw", "cold_boot_verified":False,"on_device_pin_verified":False,"userdata_write_verified":False}, indent=2))


if __name__ == "__main__":
    main()
