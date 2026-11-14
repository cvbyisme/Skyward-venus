#!/usr/bin/env python3
"""Build the isolated experiment using the verified platform libc++ ABI."""
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
CACHE = ROOT / 'target/venus-display'

def binder_headers():
    spec = json.loads((HERE/'binder-headers.json').read_text())
    folder = CACHE/'binder-headers'
    if all((folder/name).is_file() and hashlib.sha256((folder/name).read_bytes()).hexdigest() == expected
           for name, expected in spec['files'].items()):
        return folder
    data = urllib.request.urlopen(spec['url'], timeout=45).read()
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        actual = {}
        for member in archive.getmembers():
            dest = (folder/member.name).resolve()
            if not dest.is_relative_to(folder.resolve()) or member.issym() or member.islnk() or not (member.isdir() or member.isfile()):
                raise RuntimeError('unsafe official Binder header member: ' + member.name)
            if member.isfile():
                actual[member.name] = hashlib.sha256(archive.extractfile(member).read()).hexdigest()
        if actual != spec['files']:
            raise RuntimeError('official Binder header contents differ from pinned manifest')
        folder.mkdir(parents=True, exist_ok=True)
        archive.extractall(folder, filter='data')
    return folder

def main():
    if len(sys.argv) != 2:
        raise SystemExit('usage: build.sh OUTPUT_ELF')
    output = Path(sys.argv[1]).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    CACHE.mkdir(parents=True, exist_ok=True)
    build_lock = (CACHE/'build.lock').open('a')
    fcntl.flock(build_lock, fcntl.LOCK_EX)
    shared_lock = (ROOT/'target/venus-suspend/build.lock').open('a')
    fcntl.flock(shared_lock, fcntl.LOCK_SH)
    include = ROOT / 'target/venus-suspend/headers'
    specification = ROOT / 'porting/venus/suspend/official-headers.json'
    spec = json.loads(specification.read_text())
    for archive in spec['archives']:
        for name, expected in archive['files'].items():
            path = include / archive['path'] / name
            if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
                raise RuntimeError('missing/changed pinned platform header: ' + str(path))
    ndk = Path(os.environ.get('ANDROID_NDK_HOME', str(Path.home()/'Android/Sdk/ndk/29.0.14206865')))
    tool = ndk/'toolchains/llvm/prebuilt/linux-x86_64/bin'
    binder_include = binder_headers()
    # Keep the display link inputs independent of the helpers' mutable cache.
    # Extract the matching platform ABI from the original VNDK30 carrier.
    image = Path(os.environ.get('VENUS_KM41_CACHE', str(ROOT.parent/'venus_dump/keymaster41.sSBzw7')))/'vndk30.img'
    libs = CACHE/'lib64'
    libs.mkdir(exist_ok=True)
    names = ['libbinder.so', 'libutils.so', 'libc++.so']
    for name in names:
        library = libs/name
        library.unlink(missing_ok=True)
        subprocess.run(['debugfs','-R',f'dump /lib64/{name} "{library}"',str(image)],check=True)
        if not library.is_file() or not library.stat().st_size:
            raise RuntimeError('VNDK30 image lacks ' + name)
    flags = ['-std=c++17','-O2','-fPIE','-pie','-fexceptions','-fno-rtti','-Wall','-Wextra','-Werror',
             '-nostdinc++','-nostdlib++','-D_LIBCPP_ABI_NAMESPACE=__1','-isystem',str(include/'external/libcxx/include')]
    flags += ['-isystem',str(binder_include)]
    for path in sorted(include.rglob('*')):
        if path.is_dir() and path.name in ('include','include_vndk','gen') and path != include/'external/libcxx/include':
            flags += ['-isystem',str(path)]
    args = [str(tool/'aarch64-linux-android30-clang++'),*flags,str(HERE/'probe.cpp'),'-o',str(output),
            '-L',str(libs),'-Wl,-z,relro,-z,now','-Wl,--no-undefined','-lbinder','-lutils','-lc++','-ldl','-llog']
    subprocess.run(args, check=True)
    dyn = subprocess.check_output([tool/'llvm-readelf','-d',output],text=True)
    symbols = subprocess.check_output([tool/'llvm-nm','-u',output],text=True)
    if '[libc++.so]' not in dyn or 'libc++_shared' in dyn or '__ndk1' in symbols:
        raise RuntimeError('wrong C++ ABI')
    for name in names:
        if f'[{name}]' not in dyn:
            raise RuntimeError('missing expected VNDK dependency ' + name)
    # The experimental binary does not link DRM, HWC, or DisplayFeature.
    for bad in ('libdrm.so','libsdmcore.so','displayfeature.default.so'):
        if f'[{bad}]' in dyn: raise RuntimeError('unexpected display-owner dependency ' + bad)
    output.chmod(0o755)
    record = {'sha256':hashlib.sha256(output.read_bytes()).hexdigest(),
              'sources':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in HERE.iterdir() if p.is_file()},
              'platform_libcxx_sha256':hashlib.sha256((libs/'libc++.so').read_bytes()).hexdigest(),
              'vndk_image':str(image),
              'libraries':{name:hashlib.sha256((libs/name).read_bytes()).hexdigest() for name in names},
              'header_manifest_sha256':hashlib.sha256(specification.read_bytes()).hexdigest(),
              'binder_header_manifest_sha256':hashlib.sha256((HERE/'binder-headers.json').read_bytes()).hexdigest(),
              'dynamic':dyn, 'device_tested':False}
    output.with_name(output.name+'.build.json').write_text(json.dumps(record,indent=2)+'\n')
    print('PASS ARM64 platform ABI:',output,record['sha256'])

if __name__ == '__main__': main()
