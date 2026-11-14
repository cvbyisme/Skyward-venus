#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
output=${1:?usage: build-launch.sh OUTPUT_ELF}
python3 - "$root" "$output" <<'PY'
from pathlib import Path
import hashlib,json,os,subprocess,sys,tempfile
root=Path(sys.argv[1]);out=Path(sys.argv[2]).resolve();out.parent.mkdir(parents=True,exist_ok=True)
source=root/'porting/venus/display/probe-launch.c'
config=root/'porting/venus/boot-ld.config.txt'
ndk=Path(os.environ.get('ANDROID_NDK_HOME',str(Path.home()/'Android/Sdk/ndk/29.0.14206865')))
tools=ndk/'toolchains/llvm/prebuilt/linux-x86_64/bin'
with tempfile.TemporaryDirectory(prefix='display-launch-build-') as tmp:
 header=Path(tmp)/'config.h'
 header.write_text('#define DISPLAY_LINKER_CONFIG '+json.dumps(config.read_text())+'\n')
 subprocess.run([tools/'aarch64-linux-android30-clang','-std=c11','-O2','-static','-Wall','-Wextra','-Werror',
                 '-include',header,source,'-o',out],check=True)
 subprocess.run([tools/'llvm-strip',out],check=True)
record={str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest() for p in (source,config)}
record.update(sha256=hashlib.sha256(out.read_bytes()).hexdigest(),device_tested=False)
out.with_name(out.name+'.build.json').write_text(json.dumps(record,indent=2)+'\n')
print(out,record['sha256'])
PY
