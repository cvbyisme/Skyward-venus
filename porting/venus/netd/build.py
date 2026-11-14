#!/usr/bin/env python3
"""Build against the pinned official VNDK30 C++ ABI; never NDK libc++_shared."""
import hashlib
import fcntl
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import urllib.request

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
CACHE = ROOT / 'target' / 'venus-netd'
HEADER_CACHE = ROOT / 'target' / 'venus-suspend'
HEADER_CACHE.mkdir(parents=True, exist_ok=True)
CACHE.mkdir(parents=True, exist_ok=True)

def run(args):
    print('+ ' + ' '.join(map(str, args)), flush=True)
    subprocess.run(list(map(str, args)), check=True)

def headers():
    supplied = os.environ.get('VENUS_NETD_HEADERS')
    target = Path(supplied) if supplied else HEADER_CACHE / 'headers'
    spec_bytes = (HERE / 'official-headers.json').read_bytes()
    identity = hashlib.sha256(spec_bytes).hexdigest()
    receipt = target / 'manifest.sha256'
    def intact():
        for item in json.loads(spec_bytes)['archives']:
            for name, expected in item['files'].items():
                file = target / item['path'] / name
                if not file.is_file() or hashlib.sha256(file.read_bytes()).hexdigest() != expected:
                    return False
        return True
    if supplied:
        if not intact():
            raise RuntimeError('supplied headers differ from pinned official VNDK30 contents')
        return target
    if receipt.exists() and receipt.read_text().strip() == identity and intact():
        return target
    spec = json.loads(spec_bytes)
    target.mkdir(parents=True, exist_ok=True)
    for item in spec['archives']:
        data = urllib.request.urlopen(item['url'], timeout=90).read()
        folder = target / item['path']
        folder.mkdir(parents=True, exist_ok=True)
        with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
            actual = {}
            for member in archive.getmembers():
                dest = (folder / member.name).resolve()
                if not dest.is_relative_to(folder.resolve()) or member.issym() or member.islnk() or not (member.isdir() or member.isfile()):
                    raise RuntimeError('unsafe official archive member: ' + member.name)
                if member.isfile():
                    actual[member.name] = hashlib.sha256(archive.extractfile(member).read()).hexdigest()
            if actual != item['files']:
                raise RuntimeError('official header member contents changed: ' + item['path'])
            archive.extractall(folder, filter='data')
    receipt.write_text(identity + '\n')
    return target

def main():
    if len(sys.argv) not in (2, 3) or (len(sys.argv) == 3 and sys.argv[2] != '--probe'):
        raise SystemExit('usage: build.sh OUTPUT_ELF [--probe]')
    build_lock = (HEADER_CACHE / 'build.lock').open('a')
    fcntl.flock(build_lock, fcntl.LOCK_EX)
    output = Path(sys.argv[1]).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    include = headers()
    ndk = Path(os.environ.get('ANDROID_NDK_HOME', str(Path.home() / 'Android/Sdk/ndk/29.0.14206865')))
    toolchain = ndk / 'toolchains/llvm/prebuilt/linux-x86_64'
    compiler = toolchain / 'bin/aarch64-linux-android30-clang++'
    image = Path(os.environ.get('VENUS_KM41_CACHE', str(ROOT.parent / 'venus_dump/keymaster41.sSBzw7'))) / 'vndk30.img'
    libs = CACHE / 'lib64'
    libs.mkdir(exist_ok=True)
    names = ['android.system.net.netd@1.1.so', 'android.system.net.netd@1.0.so', 'libhidlbase.so', 'libutils.so', 'libc++.so', 'libbase.so', 'libcutils.so']
    # Always re-extract from the selected image, so an existing cache cannot
    # silently link an ABI from a different caller-selected carrier.
    for name in names:
        library = libs / name
        library.unlink(missing_ok=True)
        run(['debugfs', '-R', f'dump /lib64/{name} "{library}"', image])
        if not library.is_file() or library.stat().st_size == 0:
            raise RuntimeError('VNDK image lacks required library ' + name)
    source = HERE / ('probe.cpp' if len(sys.argv) == 3 else 'service.cpp')
    flags = ['-std=c++17', '-O2', '-fPIE', '-pie', '-fexceptions', '-fno-rtti', '-pthread', '-Wall', '-Wextra', '-Werror',
             '-nostdinc++', '-nostdlib++', '-D_LIBCPP_ABI_NAMESPACE=__1', '-isystem', str(include / 'external/libcxx/include')]
    for path in sorted(include.rglob('*')):
        if path.is_dir() and path.name in ('include', 'include_vndk', 'gen') and path != include / 'external/libcxx/include':
            flags += ['-isystem', str(path)]
    run([compiler, *flags, source, '-o', output, '-L', libs, '-Wl,-z,relro,-z,now', '-Wl,--no-undefined',
         '-Wl,-rpath-link,' + str(libs), '-l:android.system.net.netd@1.1.so', '-l:android.system.net.netd@1.0.so', '-lhidlbase', '-lutils', '-lc++', '-llog'])
    dynamic = subprocess.check_output([toolchain / 'bin/llvm-readelf', '-d', output], text=True)
    for name in ('android.system.net.netd@1.1.so', 'libhidlbase.so', 'libc++.so'):
        if f'[{name}]' not in dynamic:
            raise RuntimeError('missing expected VNDK dependency ' + name)
    if 'libc++_shared.so' in dynamic:
        raise RuntimeError('NDK C++ ABI must not enter the VNDK30 helper')
    undefined = subprocess.check_output([toolchain / 'bin/llvm-nm', '-u', output], text=True)
    if '__ndk1' in undefined:
        raise RuntimeError('NDK std::__ndk1 symbols must not enter the VNDK30 helper')
    output.chmod(0o755)
    record = {'source': source.name, 'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
              'local_headers': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in HERE.glob('*.h')}, 'elf_sha256': hashlib.sha256(output.read_bytes()).hexdigest(),
              'official_headers': json.loads((HERE / 'official-headers.json').read_text()),
              'vndk_image': str(image), 'libraries': {n: hashlib.sha256((libs/n).read_bytes()).hexdigest() for n in names},
              'dynamic': dynamic}
    output.with_name(output.name + '.build.json').write_text(json.dumps(record, indent=2) + '\n')
    print('PASS: VNDK30 ARM64 ELF ' + str(output), flush=True)

if __name__ == '__main__':
    main()
