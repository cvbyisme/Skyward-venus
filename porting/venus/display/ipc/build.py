#!/usr/bin/env python3
"""Build Rust private Binder display experiment: two static and one dynamic role."""
import os
from pathlib import Path
import subprocess
import sys
ROOT=Path(__file__).resolve().parents[4]
OUT=Path(sys.argv[1]).resolve() if len(sys.argv)==2 else None
if OUT is None: raise SystemExit('usage: build.py OUTPUT_DIRECTORY')
OUT.mkdir(parents=True,exist_ok=True)
env=os.environ.copy()
env['PATH']=str(Path.home()/'.cargo/bin')+':'+env.get('PATH','')
for role in ('display-ipc-launch','display-ipc-qservice'):
    subprocess.run(['bash',str(ROOT/'porting/venus/display/build-launch.sh'),str(OUT/role)],
                   check=True,cwd=ROOT,env=env)
subprocess.run(['python3',str(ROOT/'porting/venus/display/build.py'),str(OUT/'display-ipc-client')],
               check=True,cwd=ROOT,env=env)
