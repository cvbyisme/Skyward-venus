#!/usr/bin/env python3
"""Radio preparation faults with fake mounts; never contacts a device."""
from pathlib import Path
import hashlib
import os
import re
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent


class Prepare(unittest.TestCase):
    def fixture(self, root):
        bins = root / 'bin'
        bins.mkdir()
        ram = root / 'ram'
        ram.mkdir()
        assets = root / 'assets'
        assets.mkdir()
        cache = root / 'cache'
        (cache / 'vendor/rfs').mkdir(parents=True)
        capex = cache / 'system/system/apex/com.android.tethering.capex'
        capex.parent.mkdir(parents=True)
        capex.write_bytes(b'exact installed CAPEX')
        (assets / 'tethering.img').write_bytes(b'exact extracted ext4 APEX')
        source = root / 'persist'
        for name in ['rfs', 'hlos_rfs']:
            (source / name).mkdir(parents=True)
            (source / name / 'calibration').write_bytes(b'fixture calibration')
        (root / 'marker').write_text('venus-recovery-rw-v1\n')
        (root / 'boot').write_text('48188339-5852-49ff-a6e0-642d15ed113e\n')
        (root / 'mounts').write_text('')
        (root / 'persist-device').touch()
        (root / 'loop42').touch()
        (root / 'sys/loop42/loop').mkdir(parents=True)
        text = (HERE / 'modem-prepare.sh').read_text()
        substitutions = {
            '/tmp/skyward-km41/cache': str(cache), '/skyward/runtime': str(assets),
            '/skyward/venus-recovery.txt': str(root / 'marker'),
            '/proc/sys/kernel/random/boot_id': str(root / 'boot'),
            '/proc/mounts': str(root / 'mounts'),
            '/dev/block/by-name/persist': str(root / 'persist-device'),
            '/sys/class/block': str(root / 'sys'), '/tmp': str(ram),
            '327c191797ac3cbfb4562df5423ae0129a68a4f02f4d91b7171a526e861b1a35': hashlib.sha256(capex.read_bytes()).hexdigest(),
            '2113920f4b3c255773f69940ff74beef8d84681aef7bafbb6bd05d797da45714': hashlib.sha256((assets / 'tethering.img').read_bytes()).hexdigest(),
        }
        text = re.sub('|'.join(re.escape(key) for key in sorted(substitutions, key=len, reverse=True)),
                      lambda match: substitutions[match.group()], text)
        text = text.replace('export PATH=/system/bin:/system/xbin', f'export PATH={bins}:/usr/bin:/bin')
        script = root / 'prepare.sh'
        script.write_text(text)
        mocks = {
            'id': '#!/bin/sh\necho 0\n',
            'chgrp': '#!/bin/sh\nexit 0\n',
            'stat': '''#!/bin/sh
if [ "$1" = -f ]; then echo 1021994
elif [ "$2" = %u:%g:%a ]; then
 case "$3" in */venus-network-persist) echo 0:2903:750;; *) echo 0:0:600;; esac
else exec /usr/bin/stat "$@"; fi
''',
            'cp': '''#!/bin/sh
/usr/bin/cp "$@" || exit
if [ "${CORRUPT:-0}" = 1 ]; then echo corruption >> "$3/rfs/calibration"; fi
''',
            'mount': f'''#!/usr/bin/python3
import os,pathlib,shutil,sys
r=pathlib.Path({str(root)!r});args=sys.argv[1:];target=pathlib.Path(args[-1]);source=args[-2]
assert args[args.index('-t')+1]=='ext4'
options=args[args.index('-o')+1]
assert all(x in options.split(',') for x in ['ro','noload','nosuid','nodev'])
if source==str(r/'persist-device'):
 for p in (r/'persist').iterdir():shutil.copytree(p,target/p.name)
 device=source
else:
 if os.environ.get('APEX_FAIL')=='1':sys.exit(23)
 assert 'loop' in options.split(',')
 (target/'lib64').mkdir();(target/'lib64/libcom.android.tethering.connectivity_native.so').write_bytes(b'library')
 (r/'sys/loop42/loop/backing_file').write_text(source+'\\n');device=str(r/'loop42')
with (r/'mounts').open('a') as f:f.write(f'{{device}} {{target}} ext4 ro,nosuid,nodev,noload 0 0\\n')
''',
            'umount': f'''#!/usr/bin/python3
import pathlib,shutil,sys
r=pathlib.Path({str(root)!r});target=pathlib.Path(sys.argv[1]);mounts=(r/'mounts').read_text().splitlines()
assert any(l.split()[1]==str(target) for l in mounts)
for p in target.iterdir():
 if p.is_dir():shutil.rmtree(p)
 else:p.unlink()
(r/'mounts').write_text(''.join(l+'\\n' for l in mounts if l.split()[1]!=str(target)))
''',
        }
        for name, content in mocks.items():
            p = bins / name
            p.write_text(content)
            p.chmod(0o700)
        return script, ram

    def run_script(self, script, **env):
        return subprocess.run(['sh', str(script)], env={**os.environ, **env}, capture_output=True, timeout=10)

    def test_prepares_ext4_apex_and_reuses_only_matching_boot(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            script, ram = self.fixture(root)
            for _ in range(2):
                result = self.run_script(script)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual((ram / 'venus-network-persist/rfs/calibration').read_bytes(), b'fixture calibration')
            self.assertFalse(list(ram.glob('venus-network-persist-source.*')))
            self.assertFalse(list(ram.glob('venus-network-persist.new.*')))
            self.assertEqual(len((root / 'mounts').read_text().splitlines()), 1)
            (root / 'boot').write_text('a different boot\n')
            self.assertNotEqual(self.run_script(script).returncode, 0)

    def test_corrupt_copy_unmounts_source_and_never_publishes_rfs(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            script, ram = self.fixture(root)
            result = self.run_script(script, CORRUPT='1')
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((ram / 'venus-network-persist').exists())
            self.assertEqual(list(ram.iterdir()), [])
            self.assertEqual((root / 'mounts').read_text(), '')
            self.assertEqual((root / 'persist/rfs/calibration').read_bytes(), b'fixture calibration')

    def test_apex_mount_failure_removes_only_own_directory_and_can_retry(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            script, ram = self.fixture(root)
            result = self.run_script(script, APEX_FAIL='1')
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue((ram / 'venus-network-persist/.skyward-boot').is_file())
            self.assertFalse((ram / 'venus-tethering').exists())
            self.assertEqual((root / 'mounts').read_text(), '')
            result = self.run_script(script)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_foreign_apex_mount_is_neither_adopted_nor_unmounted(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            script, ram = self.fixture(root)
            target = ram / 'venus-tethering'
            target.mkdir()
            (target / 'foreign').write_text('leave intact')
            mount = f'foreign-device {target} ext4 ro 0 0\n'
            (root / 'mounts').write_text(mount)
            self.assertNotEqual(self.run_script(script).returncode, 0)
            self.assertEqual((root / 'mounts').read_text(), mount)
            self.assertEqual((target / 'foreign').read_text(), 'leave intact')


if __name__ == '__main__':
    unittest.main()
