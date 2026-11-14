#!/usr/bin/env python3
"""Local owner CE helper. Credentials travel only through adb stdin, never argv,
environment variables or logs. --inspect submits no credential. --unlock asks
locally, makes one attempt, and never retries, mounts, unmounts or reboots.
"""
from __future__ import annotations
import argparse
import getpass
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import warnings

ROOT = Path(__file__).resolve().parents[2]
REMOTE = "/tmp/skyward-ce-stage/skyward"
UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")
PREFLIGHT = re.compile(r"^VENUS_CE_PREFLIGHT_OK user=0 credential=(pin|password|pattern|none) input_supported=(true|false) storage_unlocked=(true|false) source_files_unchanged=true$", re.M)


def remote_command(unlock: bool) -> str:
    flags = " --unlock-stdin --accept-credential-check" if unlock else ""
    return (f"{REMOTE} venus-ce{flags}; rc=$?; "
            "printf '\\nVENUS_CE_REMOTE_EXIT=%s\\n' \"$rc\"; exit \"$rc\"")


def framed(credential: bytearray) -> bytearray:
    if len(credential) > 4096:
        raise ValueError("Credential exceeds the input bound")
    return bytearray(struct.pack("<I", len(credential))) + credential


def validate_local(kind: str, data: bytearray) -> None:
    if kind == "pin":
        valid = 4 <= len(data) <= 64 and all(48 <= b <= 57 for b in data)
    elif kind == "password":
        try:
            data.decode("utf-8")
            valid = 0 < len(data) <= 4096 and 0 not in data
        except UnicodeError:
            valid = False
    elif kind == "none":
        valid = len(data) == 0
    else:
        valid = False
    if not valid:
        raise ValueError("Input does not match the verified credential type; no credential submitted")


def check_result(text: str, rc: int, before: str, after: str, unlock: bool) -> list[str]:
    errors = []
    lines = text.replace("\r", "").splitlines()
    if rc:
        errors.append(f"ADB/remote exit status {rc}")
    if not UUID.fullmatch(before) or not UUID.fullmatch(after) or before != after:
        errors.append("Boot identity changed or could not be verified")
    required = ["VENUS_CE_REMOTE_EXIT=0"]
    if unlock:
        required += ["VENUS_CE_SOURCE_FILES_UNCHANGED", "KEYMASTER_PHASE CE_UNLOCK_COMPLETE",
                     "VENUS_CE_UNLOCK_OK user=0 shared_storage_unlocked=true readonly=true retained=true teardown=not_attempted"]
    else:
        required += ["VENUS_CE_INSPECTION_COMPLETE; credential_submitted=false; teardown=not_attempted"]
    for marker in required:
        if marker not in lines:
            errors.append("Missing completion evidence: " + marker)
    if not PREFLIGHT.search(text.replace("\r", "")):
        errors.append("No valid CE preflight record")
    return errors


class Device:
    def __init__(self, serial: str):
        self.base = ["adb", "-s", serial]

    def run(self, *args: str, timeout: int = 15, payload: bytes | bytearray | None = None) -> subprocess.CompletedProcess:
        # exec-out is output-only in this host adb and the legacy exec service
        # can allocate a raw PTY. Never use either path for credential input.
        if payload is not None and args[:4] != ("shell", "-T", "-e", "none"):
            raise RuntimeError("Binary input requires shell-v2 with PTY disabled; no input sent")
        # No-payload commands get EOF, never the user's interactive terminal.
        return subprocess.run(self.base + list(args), input=b"" if payload is None else payload,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              timeout=timeout, check=False)

    def raw(self, command: str, *, timeout: int = 15,
            payload: bytes | bytearray | None = None) -> subprocess.CompletedProcess:
        return self.run("shell", "-T", "-e", "none", command,
                        timeout=timeout, payload=payload)

    def verify_transport(self) -> dict:
        # Before asking for a credential, exercise the exact bidirectional
        # transport using PUBLIC fixture bytes and a hash process only.
        # This does not invoke Skyward, Binder, Keymaster or Gatekeeper.
        if "shell_v2" not in self.checked("features").splitlines():
            raise RuntimeError("ADB shell_v2 is required; no legacy fallback or credential input")
        probe = bytes(range(256)) * 17 + b"\x00\r\n~.\x04\xffEND-OF-TRANSPORT-PROBE"
        command = (
            "if [ -t 0 ]; then printf 'VENUS_CE_TRANSPORT_TTY_REJECTED\n'; exit 64; fi; "
            "printf 'VENUS_CE_TRANSPORT_RAW=1\n'; "
            "/system/bin/toybox sha256sum; rc=$?; "
            "printf 'VENUS_CE_TRANSPORT_EXIT=%s\n' \"$rc\"; exit \"$rc\""
        )
        try:
            reply = self.raw(command, timeout=10, payload=probe)
        except subprocess.TimeoutExpired:
            raise RuntimeError("ADB input/EOF self-test timed out; no credential was read or submitted") from None
        lines = reply.stdout.decode("ascii", errors="replace").splitlines()
        expected = hashlib.sha256(probe).hexdigest()
        if (reply.returncode != 0 or len(lines) != 3
                or lines[0] != "VENUS_CE_TRANSPORT_RAW=1"
                or lines[1].split() != [expected, "-"]
                or lines[2] != "VENUS_CE_TRANSPORT_EXIT=0"):
            raise RuntimeError("ADB raw input/EOF self-test failed; no credential was read or submitted")
        # shell_v2 must propagate remote status, not just the USB connection.
        status = self.raw("exit 37", timeout=10)
        if status.returncode != 37 or status.stdout:
            raise RuntimeError("ADB exit-status self-test failed; no credential was read or submitted")
        return {"protocol": "shell_v2", "pty": False, "fixture_bytes": len(probe),
                "binary_input_verified": True, "eof_verified": True,
                "exit_status_verified": True, "credential_submitted": False}

    def checked(self, *args: str, timeout: int = 15) -> str:
        out = self.run(*args, timeout=timeout)
        text = out.stdout.decode("utf-8", errors="replace").replace("\r", "")
        if out.returncode:
            raise RuntimeError("ADB preparation failed; no credential was submitted")
        return text

    def boot(self) -> str:
        value = self.checked("shell", "cat /proc/sys/kernel/random/boot_id").strip()
        if not UUID.fullmatch(value):
            raise RuntimeError("Cannot verify the running boot identity")
        return value

    def session(self) -> str:
        return self.checked("shell", "cat /tmp/skyward-km41-data-session.txt")


def verify_assets(out: Path) -> None:
    entries = {}
    for line in (out / "ASSETS.sha256").read_text().splitlines():
        digest, path = line.split(maxsplit=1)
        entries[path.lstrip("*")] = digest
    for name in ("skyward", "ce-runtime.sh", "km41-rpmb"):
        relative = "assets/" + name
        if relative not in entries:
            raise RuntimeError("CE assets have not been built")
        if hashlib.sha256((out / relative).read_bytes()).hexdigest() != entries[relative]:
            raise RuntimeError("CE asset checksum mismatch")


def local_credential(kind: str) -> bytearray:
    # Do not let getpass fall back to an echoing stdin or to tool input.
    if not sys.stdin.isatty():
        raise RuntimeError("Unlock must be run interactively in your own Ubuntu terminal")
    with open("/dev/tty", "r", encoding="utf-8") as tty_in, open("/dev/tty", "w", encoding="utf-8") as tty:
        tty.write("将验证当前用户0的锁屏凭据一次。输错会由手机安全硬件计次；不会自动重试。\n")
        tty.write("确认继续请输入 UNLOCK，其他输入取消：")
        tty.flush()
        if tty_in.readline().strip() != "UNLOCK":
            raise RuntimeError("Cancelled locally; no credential submitted")
        if kind == "none":
            return bytearray()
        with warnings.catch_warnings():
            warnings.simplefilter("error", getpass.GetPassWarning)
            text = getpass.getpass("当前锁屏 PIN：" if kind == "pin" else "当前锁屏密码：", stream=tty)
        try:
            data = bytearray(text.encode("utf-8"))
        except UnicodeError:
            raise RuntimeError("Credential encoding is unsupported; no credential submitted") from None
        finally:
            text = ""
        try:
            validate_local(kind, data)
            return data
        except Exception:
            data[:] = b"\0" * len(data)
            raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--inspect", action="store_true")
    mode.add_argument("--unlock", action="store_true")
    args = parser.parse_args()
    if not shutil.which("adb"):
        raise RuntimeError("Host adb is missing")
    if args.unlock and not sys.stdin.isatty():
        raise RuntimeError("Run --unlock from your own interactive terminal; no credential was read")
    os.umask(0o077)
    out = Path((ROOT / "target/venus-ce-latest.txt").read_text().strip()).resolve()
    if not out.is_relative_to((ROOT / "target").resolve()):
        raise RuntimeError("Unexpected CE build path")
    verify_assets(out)
    device = Device(os.environ.get("ANDROID_SERIAL", "535e451a"))
    device.checked("shell", 'test "$(id -u)" = 0 && grep -q "^venus-entryfix-v1$" /skyward/probe-entryfix.txt && ! grep -q " /data " /proc/mounts && test -f /tmp/skyward-km41-data-session.txt')
    before = device.boot()
    session = device.session()
    log = Path(tempfile.mkdtemp(prefix="ce-local.", dir=ROOT.parent / "venus_dump"))
    print(f"本轮日志：{log}")
    (log / "boot-before.txt").write_text(before + "\n")
    transport = device.verify_transport()
    (log / "transport.json").write_text(json.dumps(transport, indent=2) + "\n")
    print("VENUS_CE_TRANSPORT_OK shell_v2=true stdin_tty=false binary_stdin=true eof=true exit_status=true")
    device.checked("shell", "mkdir -p /tmp/skyward-ce-stage; chmod 0700 /tmp/skyward-ce-stage")
    for name in ("skyward", "ce-runtime.sh", "km41-rpmb"):
        device.checked("push", str(out / "assets" / name), f"/tmp/skyward-ce-stage/{name}.new")
        device.checked("shell", f"chmod 0700 /tmp/skyward-ce-stage/{name}.new; mv /tmp/skyward-ce-stage/{name}.new /tmp/skyward-ce-stage/{name}")
        expected = hashlib.sha256((out / "assets" / name).read_bytes()).hexdigest()
        actual = device.checked("shell", f"sha256sum /tmp/skyward-ce-stage/{name}").split()[0]
        if expected != actual:
            raise RuntimeError("Deployed CE asset checksum mismatch")
    runtime = device.checked("shell", "sh /tmp/skyward-ce-stage/ce-runtime.sh", timeout=50)
    (log / "runtime.txt").write_text(runtime)
    print(runtime, end="")
    query = device.raw(remote_command(False), timeout=45)
    text = query.stdout.decode("utf-8", errors="replace").replace("\r", "")
    (log / "inspection.txt").write_text(text)
    print(text, end="")
    errors = check_result(text, query.returncode, before, device.boot(), False)
    if device.session() != session:
        errors.append("Retained Data session identity changed")
    if errors:
        (log / "summary.json").write_text(json.dumps({"verified": False, "errors": errors}, indent=2) + "\n")
        raise RuntimeError("; ".join(errors))
    record = PREFLIGHT.search(text)
    assert record is not None
    kind, supported, unlocked = record.groups()
    if not args.unlock:
        (log / "summary.json").write_text(json.dumps({"verified": True, "scope": "CE preflight only", "credential_submitted": False, "storage_unlocked": unlocked == "true", "teardown_verified": False}, indent=2) + "\n")
        print("VERIFIED CE_PREFLIGHT; no credential was submitted; unlock not tested")
        return 0
    if unlocked == "true":
        raise RuntimeError("CE is already unlocked; refusing a redundant credential check")
    if supported != "true":
        raise RuntimeError("This credential input type is not enabled; no credential submitted")
    data = local_credential(kind)
    packet = framed(data)
    try:
        if device.boot() != before or device.session() != session:
            raise RuntimeError("Recovery session changed while waiting for local input; no credential submitted")
        # Not a shell command, env value, CLI argument, or log: only stdin bytes.
        result = device.raw(remote_command(True), timeout=90, payload=packet)
    except subprocess.TimeoutExpired:
        raise RuntimeError("Unlock response timed out; remote outcome is unknown. Do not retry or reboot; inspect the current state") from None
    finally:
        data[:] = b"\0" * len(data)
        packet[:] = b"\0" * len(packet)
    text = result.stdout.decode("utf-8", errors="replace").replace("\r", "")
    (log / "unlock-result.txt").write_text(text)
    print(text, end="")
    try:
        after = device.boot()
        same_session = device.session() == session
    except (RuntimeError, subprocess.TimeoutExpired):
        after, same_session = "", False
    errors = check_result(text, result.returncode, before, after, True)
    if not same_session:
        errors.append("Retained Data identity changed or could not be verified")
    (log / "summary.json").write_text(json.dumps({"verified": not errors, "scope": "owner0 CE on retained readonly session", "boot_before": before, "boot_after": after, "same_session": same_session, "teardown_verified": False, "errors": errors}, indent=2) + "\n")
    if errors:
        raise RuntimeError("; ".join(errors) + "; no automatic retry was attempted")
    print("VERIFIED CE_UNLOCK; read-only internal storage: /mnt/skyward-data-ro/media/0")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, ValueError, OSError, subprocess.TimeoutExpired, getpass.GetPassWarning) as exc:
        print("CE 未完整通过：" + str(exc), file=sys.stderr)
        raise SystemExit(1)
    except KeyboardInterrupt:
        print("本地操作已中断；若已经提交凭据，不要重复尝试，先检查手机状态。", file=sys.stderr)
        raise SystemExit(130)
