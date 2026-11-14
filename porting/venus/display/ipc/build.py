#!/usr/bin/env python3
"""Separate IPC experiment artifacts; does not rebuild the frozen guarded probe."""
from pathlib import Path
import fcntl, hashlib, importlib.util, json, os, subprocess, sys, tempfile
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[3]
OUT=Path(sys.argv[1]).resolve() if len(sys.argv)==2 else None
if OUT is None: raise SystemExit('usage: python3 porting/venus/display/ipc/build.py OUTPUT_DIRECTORY')
OUT.mkdir(parents=True,exist_ok=True)
CACHE=ROOT/'target/venus-display'
lock=(CACHE/'build.lock').open('a');fcntl.flock(lock,fcntl.LOCK_EX)
shared=(ROOT/'target/venus-suspend/build.lock').open('a');fcntl.flock(shared,fcntl.LOCK_SH)
specfile=ROOT/'porting/venus/suspend/official-headers.json'
spec=json.loads(specfile.read_text());include=ROOT/'target/venus-suspend/headers'
for a in spec['archives']:
 for name,sha in a['files'].items():
  p=include/a['path']/name
  if not p.is_file() or hashlib.sha256(p.read_bytes()).hexdigest()!=sha:raise RuntimeError('changed platform header: '+str(p))
module=importlib.util.spec_from_file_location('guarded_display_builder',HERE.parent/'build.py')
display=importlib.util.module_from_spec(module);module.loader.exec_module(display)
binder=display.binder_headers()
ndk=Path(os.environ.get('ANDROID_NDK_HOME',str(Path.home()/'Android/Sdk/ndk/29.0.14206865')))
tools=ndk/'toolchains/llvm/prebuilt/linux-x86_64/bin'
libs=ROOT/'target/venus-display-ipc/lib64';libs.mkdir(parents=True,exist_ok=True)
vndk=ROOT.parent/'venus_dump/keymaster41.sSBzw7/vndk30.img'
for name in ['libbinder.so','libutils.so','libc++.so']:
 p=libs/name;p.unlink(missing_ok=True)
 subprocess.run(['debugfs','-R',f'dump /lib64/{name} "{p}"',str(vndk)],check=True)
 if not p.is_file():raise RuntimeError('missing original '+name)
flags=['-std=c++17','-O2','-fPIE','-pie','-fexceptions','-fno-rtti','-Wall','-Wextra','-Werror',
 '-nostdinc++','-nostdlib++','-D_LIBCPP_ABI_NAMESPACE=__1','-isystem',str(include/'external/libcxx/include'),'-isystem',str(binder)]
for p in sorted(include.rglob('*')):
 if p.is_dir() and p.name in ('include','include_vndk','gen') and p!=include/'external/libcxx/include':flags+=['-isystem',str(p)]
for source in ['qservice.cpp','client.cpp']:
 output=OUT/('display-ipc-'+source.removesuffix('.cpp'))
 subprocess.run([tools/'aarch64-linux-android30-clang++',*flags,HERE/source,'-o',output,'-L',libs,
  '-Wl,-z,relro,-z,now','-Wl,--no-undefined','-lbinder','-lutils','-lc++','-ldl','-llog'],check=True)
 subprocess.run([tools/'llvm-strip',output],check=True)
 dynamic=subprocess.check_output([tools/'llvm-readelf','-d',output],text=True)
 syms=subprocess.check_output([tools/'llvm-nm','-D','-u',output],text=True)
 if '[libc++.so]' not in dynamic or 'libc++_shared' in dynamic or '__ndk1' in syms:raise RuntimeError('wrong platform ABI')
 output.with_suffix('.build.json').write_text(json.dumps({'sha256':hashlib.sha256(output.read_bytes()).hexdigest(),
  'sources':{str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in HERE.glob('*') if p.is_file()},
  'libraries':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in libs.glob('*.so')},'dynamic':dynamic,'device_tested':False},indent=2)+'\n')
 print(output,hashlib.sha256(output.read_bytes()).hexdigest())
if (HERE/'launch.c').exists():
 with tempfile.TemporaryDirectory(prefix='display-ipc-build-') as tmp:
  header=Path(tmp)/'config.h';header.write_text('#define DISPLAY_LINKER_CONFIG '+json.dumps((ROOT/'porting/venus/boot-ld.config.txt').read_text())+'\n')
  output=OUT/'display-ipc-launch'
  subprocess.run([tools/'aarch64-linux-android30-clang','-std=c11','-O2','-static','-Wall','-Wextra','-Werror','-include',header,HERE/'launch.c','-o',output],check=True)
  subprocess.run([tools/'llvm-strip',output],check=True)
  print(output,hashlib.sha256(output.read_bytes()).hexdigest())
