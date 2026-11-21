#!/usr/bin/env python3
"""Recovery runtime ownership checks; only creates/clears a test directory."""
import fcntl
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

class TmpIsolation(unittest.TestCase):
    def test_init_mount_precedes_every_runtime_producer(self):
        rc = (ROOT / 'crates/skyward-pack/src/venus_probe.rc').read_text()
        mount = rc.index('mount tmpfs tmpfs /skyward/run nosuid nodev mode=0755')
        self.assertLess(rc.index('on early-init'), mount)
        self.assertLess(mount, rc.index('on post-fs'))
        self.assertEqual(rc.count('mount tmpfs tmpfs /skyward/run '), 1)
        for child in ['locks', 'state', 'tasks', 'hal/scratch', 'crypto/tmp']:
            self.assertGreater(rc.index('mkdir /skyward/run/' + child + ' '), mount)
        launcher = (ROOT / 'crates/skyward-venus-runtime/src/launcher.rs').read_text()
        self.assertNotIn('root.bind("/tmp"', launcher)
        self.assertRegex(launcher, r'root\.bind\(&scratch,\s*"tmp",\s*false,\s*false\)')

    def test_long_lived_producers_do_not_use_installers_tmp(self):
        files = [*(ROOT / 'crates/skyward/src').glob('venus_*.rs'),
                 ROOT / 'crates/skyward-install/src/task_scope.rs',
                 *(ROOT / 'porting/venus').glob('*runtime.sh'),
                 ROOT / 'crates/skyward-venus-runtime/src/launcher.rs',
                 ROOT / 'crates/skyward-venus-runtime/src/modem_prepare.rs',
                 ROOT / 'crates/skyward-venus-runtime/src/common.rs']
        for path in files:
            text = path.read_text()
            self.assertNotRegex(text, r'/tmp/(?:skyward|venus)[-\w/]*', str(path))
        installer = (ROOT / 'crates/skyward-install/src/run.rs').read_text()
        for name in ['TMPDIR', 'TMP', 'TEMP']:
            self.assertIn(f'.env("{name}", "/tmp")', installer)
        self.assertIn('"/tmp/update-binary"', installer)

    def test_package_cleanup_preserves_runtime_and_exclusion(self):
        # Exercise the destructive operation against a fixture, never host TMP.
        with tempfile.TemporaryDirectory(prefix='skyward-tmp-isolation-') as directory:
            root = Path(directory)
            tmp, run = root / 'tmp', root / 'skyward/run'
            tmp.mkdir(); (run / 'locks').mkdir(parents=True)
            state = run / 'session'; state.write_bytes(b'non-secret identity')
            cache = run / 'cache'; cache.write_bytes(b'runtime fixture')
            lock = run / 'locks/data.lock'
            with lock.open('w') as held:
                fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
                inode = lock.stat().st_ino
                for package in range(2):
                    for phase in ['before', 'during', 'after']:
                        (tmp / 'package.tmp').write_bytes(b'disposable')
                        for child in tmp.iterdir():
                            if child.is_dir(): shutil.rmtree(child)
                            else: child.unlink()
                        self.assertEqual(state.read_bytes(), b'non-secret identity')
                        self.assertEqual(cache.read_bytes(), b'runtime fixture')
                        self.assertEqual(lock.stat().st_ino, inode)
                        result = subprocess.run(['python3', '-c',
                            'import fcntl,sys; f=open(sys.argv[1],"r+"); '
                            'fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)', str(lock)],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
                        self.assertNotEqual(result.returncode, 0, (package, phase))
                        self.assertIn(b'BlockingIOError', result.stderr)

if __name__ == '__main__': unittest.main()
