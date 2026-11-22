#!/usr/bin/env python3
"""Take the venus runtime assets out of an already built venus Recovery.

The integrated venus Recovery is packed with a directory of assets: the
Keymaster runtime and VNDK images of the installed phone, the tethering apex,
and a small crypto runtime that carries the recovery's own libc, linker and
keystore. Those files are not in this repository and cannot be rebuilt from
it: they come from the phone's own vendor partitions, which are not
redistributable as sources.

An image built from them already holds all of them, so this script takes them
back out of such an image, verifying the two whose SHA-256 the packaging
scripts pin. It writes everything the packer asks for except the Rust helpers
(build-runtime.sh makes those) and the crypto policy (prepare-crypto-policy.py
makes that from the stock recovery, so it matches the stock image it is laid
on).

usage: restore-upstream-assets.py IMAGE OUT_DIR
"""

import gzip
import hashlib
import lzma
import os
import struct
import subprocess
import sys
import tempfile

# The two assets whose SHA-256 porting/venus/build-*.sh pin.
PINNED = {
    'runtime.img': '9776b642132872ce4931218694c345231ecaac4490ead5d7c2f7c24e17481c17',
    'vndk30.img': '8948745de8d27cae3fbdf23f6185ae4b6aec680b6a6febf2a2f0eaba1ab59b9e',
    'tethering.img': '2113920f4b3c255773f69940ff74beef8d84681aef7bafbb6bd05d797da45714',
}
# Taken out of the ramdisk as files, at the names the packer reads.
PLAIN = ['skyward/runtime/runtime.img',
         'skyward/runtime/vndk30.img',
         'skyward/runtime/tethering.img',
         'skyward/runtime/framework-manifest.xml']
# The crypto runtime is a tree in the ramdisk and an archive in the assets.
CRYPTO_ROOT = 'skyward/crypto-runtime/'
CRYPTO_ALLOWED = ('system/lib64/', 'system/bin/', 'system/etc/')
CRYPTO_SOURCE = CRYPTO_ROOT + 'SOURCE.sha256'


def fail(message):
    print(message, file=sys.stderr)
    sys.exit(1)


def boot_ramdisk(path):
    with open(path, 'rb') as f:
        head = f.read(4096)
        if head[:8] != b'ANDROID!':
            fail(f'{path} is not a boot image')
        kernel_size, ramdisk_size = struct.unpack_from('<II', head, 8)
        off = 4096 + ((kernel_size + 4095) // 4096) * 4096
        f.seek(off)
        return f.read(ramdisk_size)


def unpack(blob, workdir):
    if blob[:2] == b'\x1f\x8b':
        return gzip.decompress(blob)
    if blob[:6] == b'\xfd7zXZ\x00':
        return lzma.decompress(blob)
    if blob[:4] == b'\x04\x22\x4d\x18':
        src = os.path.join(workdir, 'rd.lz4')
        dst = os.path.join(workdir, 'rd.raw')
        open(src, 'wb').write(blob)
        if subprocess.run(['lz4', '-d', '-f', src, dst],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
            fail('could not decompress the ramdisk (is lz4 installed?)')
        return open(dst, 'rb').read()
    return blob


def align(n, a=4):
    return (n + a - 1) // a * a


def walk(cpio):
    """Walk a newc archive, yielding (name, mode, size, offset)."""
    i = 0
    while i + 110 <= len(cpio) and cpio[i:i + 6] == b'070701':
        f = [int(cpio[i + 6 + k * 8:i + 6 + (k + 1) * 8], 16) for k in range(13)]
        mode, size, namesize = f[1], f[6], f[11]
        name_at = i + 110
        name = cpio[name_at:name_at + namesize].split(b'\0')[0].decode('utf-8', 'replace')
        data_at = i + align(110 + namesize)
        if name == 'TRAILER!!!':
            return
        yield name, mode, size, data_at
        i = data_at + align(size)


def newc(entries):
    """Build a newc archive from (name, mode, data) triples."""
    out = bytearray()
    ino = 1
    for name, mode, data in entries:
        raw = name.encode() + b'\0'
        fields = [ino, mode, 0, 0, 1, 0, len(data), 0, 0, 0, 0, len(raw), 0]
        out += b'070701' + b''.join(b'%08X' % v for v in fields)
        out += raw
        out += b'\0' * (align(110 + len(raw)) - 110 - len(raw))
        out += data
        out += b'\0' * (align(len(data)) - len(data))
        ino += 1
    trailer = b'TRAILER!!!\0'
    fields = [ino, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, len(trailer), 0]
    out += b'070701' + b''.join(b'%08X' % v for v in fields)
    out += trailer
    out += b'\0' * (align(110 + len(trailer)) - 110 - len(trailer))
    return bytes(out)


def main():
    if len(sys.argv) != 3:
        fail(__doc__)
    image, out_dir = sys.argv[1], sys.argv[2]
    os.makedirs(out_dir, exist_ok=True)
    with tempfile.TemporaryDirectory() as workdir:
        cpio = unpack(boot_ramdisk(image), workdir)
    table = {n: (m, s, a) for n, m, s, a in walk(cpio)}
    print(f'{image}: {len(table)} ramdisk entries')

    for name in PLAIN:
        if name not in table:
            fail(f'{image} holds no {name}')
        mode, size, at = table[name]
        short = os.path.basename(name)
        data = cpio[at:at + size]
        if short in PINNED:
            got = hashlib.sha256(data).hexdigest()
            if got != PINNED[short]:
                fail(f'{short} has the SHA-256 {got}, where the build pins '
                     f'{PINNED[short]}')
        with open(os.path.join(out_dir, short), 'wb') as f:
            f.write(data)
        print(f'took {short}: {size} bytes')

    if CRYPTO_SOURCE not in table:
        fail(f'{image} holds no {CRYPTO_SOURCE}')
    mode, size, at = table[CRYPTO_SOURCE]
    with open(os.path.join(out_dir, 'crypto-runtime.SOURCE.sha256'), 'wb') as f:
        f.write(cpio[at:at + size])
    print(f'took crypto-runtime.SOURCE.sha256: {size} bytes')

    entries = []
    for name, mode, size, at in walk(cpio):
        if not name.startswith(CRYPTO_ROOT) or name == CRYPTO_SOURCE:
            continue
        inner = name[len(CRYPTO_ROOT):]
        if mode & 0o170000 != 0o100000:
            continue
        if not (inner.startswith(CRYPTO_ALLOWED) or inner == 'file_contexts'):
            continue
        if any(part in ('', '.', '..') for part in inner.split('/')):
            fail(f'unsafe crypto runtime name {name}')
        entries.append((inner, mode & 0o777, cpio[at:at + size]))
    if not entries:
        fail(f'{image} holds no crypto runtime')
    archive = newc(entries)
    src = os.path.join(out_dir, 'crypto-runtime.cpio')
    dst = src + '.lz4'
    open(src, 'wb').write(archive)
    if subprocess.run(['lz4', '-1', '-f', src, dst],
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        fail('could not compress the crypto runtime (is lz4 installed?)')
    os.unlink(src)
    print(f'packed crypto-runtime.cpio.lz4: {len(entries)} files, '
          f'{os.path.getsize(dst)} bytes')


if __name__ == '__main__':
    main()