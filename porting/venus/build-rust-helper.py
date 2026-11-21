#!/usr/bin/env python3
"""Build a standalone Venus Rust service or its explicit diagnostic client."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]

def main(role):
    if len(sys.argv) not in (2, 3) or (len(sys.argv) == 3 and sys.argv[2] != '--probe'):
        raise SystemExit('usage: build.sh OUTPUT_ELF [--probe]')
    if len(sys.argv) == 3:
        role += '-probe'
    output = Path(sys.argv[1]).resolve()
    # The executable chooses its role from argv[0], so arbitrary output names
    # receive a dedicated small Rust entrypoint from the same package.
    env = os.environ.copy()
    env.setdefault('ANDROID_NDK_HOME', str(Path.home() / 'Android/Sdk/ndk/29.0.14206865'))
    env['RUSTFLAGS'] = env.get('RUSTFLAGS', '') + ' -C target-feature=+crt-static'
    subprocess.run(['cargo', 'ndk', '-t', 'arm64-v8a', '--platform', '30', 'build', '--locked', '--release',
                    '-p', 'skyward-venus-runtime', '--bin', role, '--target-dir', 'target/venus-runtime-static'], cwd=ROOT, env=env, check=True)
    binary = ROOT / 'target/venus-runtime-static/aarch64-linux-android/release' / role
    headers = subprocess.check_output(['readelf', '-h', '-l', binary], text=True)
    if 'AArch64' not in headers or 'INTERP' in headers:
        raise RuntimeError('expected a static ARM64 Rust helper')
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(binary.read_bytes())
    output.chmod(0o755)
    sources = [ROOT/'Cargo.lock', ROOT/'Cargo.toml']
    for crate in ('skyward-venus-runtime', 'skyward-binder'):
        sources += sorted((ROOT/'crates'/crate).rglob('*.rs')) + [ROOT/'crates'/crate/'Cargo.toml']
    record = {'language': 'Rust', 'role': role, 'target': 'aarch64-linux-android', 'static': True,
              'elf_sha256': hashlib.sha256(output.read_bytes()).hexdigest(),
              'sources': {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest() for p in sources}}
    output.with_name(output.name + '.build.json').write_text(json.dumps(record, indent=2) + '\n')
    print('PASS: static Rust ARM64 ELF ' + str(output))

if __name__ == '__main__':
    raise SystemExit('invoke through suspend/build.sh or netd/build.sh')
