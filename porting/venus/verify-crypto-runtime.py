#!/usr/bin/env python3
"""Verify the exported security runtime's ELF dependency closure on the host."""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

root = Path(sys.argv[1])
# GNU sha256sum accepts comments, but Android Toybox does not. Validate the
# exact on-device list format as well as every asset before inspecting ELF.
manifest = root.parent / 'SOURCE.sha256'
assets = {}
for line in manifest.read_text().splitlines():
    match = re.fullmatch(r'([0-9a-f]{64})  ([A-Za-z0-9_./@+-]+)', line)
    if not match:
        raise SystemExit('checksum list is incompatible with Android Toybox: ' + line)
    digest, name = match.groups()
    if name in assets or name.startswith('/') or any(p in ('', '.', '..') for p in name.split('/')):
        raise SystemExit('unsafe or duplicate checksum path: ' + name)
    path = root / name
    if path.is_symlink() or not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
        raise SystemExit('runtime asset checksum differs: ' + name)
    assets[name] = digest
provenance = 'system/etc/skyward-crypto-reference.txt'
if provenance not in assets or not re.fullmatch(r'reference-image-sha256=[0-9a-f]{64}\n', (root / provenance).read_text()):
    raise SystemExit('missing reference image provenance')
starts = ['system/bin/' + name for name in ['servicemanager', 'keystore2', 'vold_prepare_subdirs', 'toybox', 'fsck.f2fs', 'e2fsck']]
pending, closure = starts[:], {}
while pending:
    relative = pending.pop()
    if relative in closure:
        continue
    path = root / relative
    data = path.read_bytes()
    if data[:6] != b'\x7fELF\x02\x01' or data[18:20] != b'\xb7\x00':
        raise SystemExit('runtime is not ARM64 ELF: ' + relative)
    result = subprocess.run(['readelf', '-d', str(path)], capture_output=True, text=True, check=True)
    needed = re.findall(r'\(NEEDED\).*?\[([^\]]+)\]', result.stdout)
    closure[relative] = {'sha256': hashlib.sha256(data).hexdigest(), 'needed': needed}
    for name in needed:
        if '/' in name:
            raise SystemExit('unsafe dependency name ' + name)
        # Android's linker supplies this SONAME itself.
        if name == 'ld-android.so':
            continue
        dependency = 'system/lib64/' + name
        if not (root / dependency).is_file():
            raise SystemExit(relative + ' lacks dependency ' + name)
        pending.append(dependency)
print(json.dumps({'roots': starts, 'closure': closure, 'assets_verified': len(assets),
                  'reference': (root / provenance).read_text().strip()}, indent=2, sort_keys=True))
