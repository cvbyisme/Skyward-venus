#!/usr/bin/env python3
"""Validate recorded diagnostics; never contacts or modifies a device."""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re

MODES = ("--hardware", "--metadata", "--data-ro", "--de", "--data-ro-keep", "--de-keep")
UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")

def failures(mode: str, text: str, before: str, after: str, adb_rc: int,
             kernel_ready: bool) -> list[str]:
    if mode not in MODES:
        raise ValueError("unsupported mode")
    lines = text.replace("\r", "").splitlines()
    errors: list[str] = []
    if adb_rc:
        errors.append(f"ADB/test pipeline exited with {adb_rc}")
    if "KEYMASTER_REMOTE_EXIT=0" not in lines:
        errors.append("remote command did not report a successful exit")
    if not kernel_ready:
        errors.append("kernel capture did not acknowledge readiness")
    if not UUID.fullmatch(before.strip()) or not UUID.fullmatch(after.strip()):
        errors.append("could not verify both boot IDs")
    elif before.strip() != after.strip():
        errors.append("BOOT_ID_CHANGED: phone restarted during the diagnostic")
    required = ["KEYMASTER_HIDL_READY "]
    if mode != "--hardware":
        required += ["KEYMASTER_METADATA_UNWRAP_OK ", "KEYMASTER_EPHEMERAL_OK ",
                     "KEYMASTER_ORIGINAL_KEY_FILES_UNCHANGED",
                     "KEYMASTER_PHASE KEYMASTER_DIAGNOSTIC_COMPLETE"]
    if mode in ("--data-ro", "--de"):
        required += ["KEYMASTER_DECRYPTED_F2FS_SUPERBLOCK_OK",
                     "KEYMASTER_DATA_RO_MOUNT_OK;",
                     "KEYMASTER_PHASE DATA_RO_UMOUNT_OK",
                     "KEYMASTER_PHASE DATA_RO_DM_REMOVE_OK",
                     "KEYMASTER_DATA_RO_CLEANUP_OK"]
    if mode in ("--data-ro-keep", "--de-keep"):
        required += ["KEYMASTER_DECRYPTED_F2FS_SUPERBLOCK_OK",
                     "KEYMASTER_DATA_RO_MOUNT_OK;",
                     "KEYMASTER_DATA_SESSION_READY;",
                     "KEYMASTER_PHASE DATA_RO_RETAINED",
                     "KEYMASTER_DATA_RO_RETAINED;"]
        if any(line.startswith("KEYMASTER_PHASE DATA_RO_UMOUNT_")
               or line.startswith("KEYMASTER_PHASE DATA_RO_DM_REMOVE_")
               or line == "KEYMASTER_DATA_RO_CLEANUP_OK" for line in lines):
            errors.append("retained session unexpectedly entered teardown")
    if mode == "--de-keep":
        required += ["KEYMASTER_DATA_SESSION_REUSED;"]
    if mode in ("--de", "--de-keep"):
        required += ["KEYMASTER_DE_CHAIN_OK;"]
    for marker in required:
        if not any(line.startswith(marker) if marker.endswith((" ", ";"))
                   else line == marker for line in lines):
            errors.append("missing evidence: " + marker)
    return errors

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", required=True, choices=MODES)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--adb-rc", type=int, required=True)
    args = parser.parse_args()
    def read(name: str) -> str:
        path = args.log / name
        return path.read_text(errors="replace") if path.is_file() else ""
    text = read("result.txt")
    before, after = read("boot-id-before.txt"), read("boot-id-after.txt")
    ready = "KM41_KMSG_CAPTURE_READY " in read("kernel-live.txt")
    errors = failures(args.mode, text, before, after, args.adb_rc, ready)
    phases = [line for line in text.splitlines() if line.startswith("KEYMASTER_PHASE ")]
    summary = {"mode": args.mode, "verified": not errors, "adb_exit": args.adb_rc,
               "boot_id_before": before.strip(), "boot_id_after": after.strip(),
               "last_host_phase": phases[-1] if phases else None,
               "lifecycle": "retained" if args.mode.endswith("-keep") else "diagnostic",
               "cleanup_verified": not errors and args.mode in ("--data-ro", "--de"),
               "ce_verified": False, "failures": errors}
    (args.log / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    if errors:
        print(f"FAILED {args.mode}; logs={args.log}")
        for error in errors:
            print("  " + error)
        if phases:
            print("Last phase seen by host: " + phases[-1])
        print("Do not repeat the data test or advance to DE/CE before inspecting the failure.")
        return 1
    print(f"VERIFIED {args.mode}; logs={args.log}")
    if args.mode.endswith("-keep"):
        print("Read-only Data remains mounted; teardown was NOT tested; CE remains locked/unverified.")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
