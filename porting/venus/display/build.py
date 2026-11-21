#!/usr/bin/env python3
"""Build an all-Rust Android 11 STC diagnostic from the pinned Rust sources.

No .c/.cpp/.hpp source is compiled and no device/partition is touched.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT=Path(__file__).resolve().parents[3]
OUTPUT=Path(sys.argv[1]).resolve() if len(sys.argv)==2 else None
if OUTPUT is None or OUTPUT.name not in ('display-probe','display-ipc-client'):
    raise SystemExit('usage: build.py OUTPUT_PATH (display-probe|display-ipc-client)')
ndk=Path(os.environ.get('ANDROID_NDK_HOME',str(Path.home()/'Android/Sdk/ndk/29.0.14206865')))
compiler=ndk/'toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-readelf'
env=os.environ.copy()
env['PATH']=str(Path.home()/'.cargo/bin')+':'+env.get('PATH','')
env['ANDROID_NDK_HOME']=str(ndk)
env['RUSTFLAGS']=' '.join((env.get('RUSTFLAGS',''),'-C target-feature=-crt-static')).strip()
subprocess.run(['cargo','ndk','-t','arm64-v8a','--platform','30','build','--locked','--release',
                '-p','skyward-venus-runtime','--bin','venus-runtime','--target-dir','target/venus-display-dynamic'],
               cwd=ROOT,env=env,check=True)
source=ROOT/'target/venus-display-dynamic/aarch64-linux-android/release/venus-runtime'
elf=subprocess.check_output([compiler,'-l',str(source)],text=True)
if 'INTERP' not in elf or '/system/bin/linker64' not in elf:
    raise RuntimeError('STC vendor ABI helper must use the Android dynamic linker')
dyn=subprocess.check_output([compiler,'-d',str(source)],text=True)
if 'libc++_shared' in dyn or 'libstdc++' in dyn:
    raise RuntimeError('Rust helper may not link the NDK C++ runtime')
OUTPUT.parent.mkdir(parents=True,exist_ok=True)
shutil.copy2(source,OUTPUT)
paths=[ROOT/'crates/skyward-venus-runtime/src/display'/name for name in (
    'stc.rs','probe.rs','client.rs','platform_binder.rs','blobs.rs','guard.rs','ipc.rs','qservice.rs','root.rs')]
paths += [ROOT/'porting/venus/display/vendor-pins.json',ROOT/'porting/venus/boot-ld.config.txt']
record={'implementation':'rust','role':OUTPUT.name,'sha256':hashlib.sha256(OUTPUT.read_bytes()).hexdigest(),
        'sources':{str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in paths},
        'target':'aarch64-linux-android','dynamic':dyn,'device_tested':False}
OUTPUT.with_name(OUTPUT.name+'.build.json').write_text(json.dumps(record,indent=2)+'\n')
print('PASS Rust platform-ABI probe',OUTPUT,record['sha256'])
