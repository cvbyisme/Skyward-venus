#!/usr/bin/env python3
"""RAM-copy fidelity and refusal tests; no real mounts, HALs or device writes."""
from pathlib import Path
import hashlib
import os
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent

class Cache(unittest.TestCase):
    def fixture(self, base, *, memory=8*1024*1024, corrupt=False, prior=False):
        ro=base/'ro'; state=base/'state'; state.mkdir(); bins=base/'bin'; bins.mkdir()
        files=['system/system/lib64/library.so','system/system/etc/build.prop',
               'system/system/apex/com.android.tethering.capex','system/system/bin/servicemanager','system/system/bin/iptables','system/system/bin/ip','system/system/bin/netutils-wrapper-1.0','system/system/bin/bootstrap/linker64','system/system/build.prop',
               'system_ext/lib64/helper.so','system_ext/etc/build.prop','system_ext/bin/hwservicemanager',
               'vendor/lib64/backend.so','vendor/etc/vintf/manifest.xml','vendor/bin/qseecomd',
               'vendor/firmware/wave.bin','vendor/rfs/fixture','vendor/build.prop','vendor/ueventd.rc','odm/etc/vintf/manifest.xml',
               'product/etc/build.prop']
        for name in files:
            p=ro/name; p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes((name+'\n').encode());p.chmod(0o640)
        (ro/'system/system/bin/linker64').symlink_to('/apex/com.android.runtime/bin/linker64')
        (ro/'system/system/bin/ip6tables').symlink_to('iptables')
        (ro/'system/system/bin/ip-wrapper-1.0').symlink_to('netutils-wrapper-1.0')
        (ro/'system/system/lib64/alias.so').symlink_to('library.so')
        fw=base/'firmware';fw.mkdir();(fw/'trustlet.mdt').write_bytes(b'firmware fixture\x00\xff')
        (base/'marker').write_text('venus-recovery-rw-v1\n');(base/'mounts').write_text('')
        (base/'boot-id').write_text('48188339-5852-49ff-a6e0-642d15ed113e\n')
        (base/'meminfo').write_text(f'MemAvailable: {memory} kB\n')
        mock=bins/'stat';mock.write_text('#!/bin/sh\nif [ "$1" = -f ]; then\n if [ "$3" = %t ]; then echo 1021994; else echo "3000000 4096"; fi\nelse exec /usr/bin/stat "$@"; fi\n');mock.chmod(0o700)
        mock=bins/'id';mock.write_text('#!/bin/sh\necho 0\n');mock.chmod(0o700)
        if corrupt:
            mock=bins/'sha256sum'
            mock.write_text(f'#!/bin/sh\nif [ "$1" = -c ]; then echo damaged > "{state}/cache.new/vendor/lib64/backend.so"; fi\nexec /usr/bin/sha256sum "$@"\n');mock.chmod(0o700)
        if prior:
            (state/'cache').mkdir();(state/'cache/ready').write_text('previous receipt\n')
        text=(HERE/'cache-runtime.sh').read_text()
        for old,new in {
            '/tmp/skyward-km41':str(state),'/mnt/skyward-ro':str(ro),'/vendor/firmware_mnt':str(fw),
            '/skyward/venus-recovery.txt':str(base/'marker'),'/proc/mounts':str(base/'mounts'),
            '/proc/meminfo':str(base/'meminfo'),'/proc/sys/kernel/random/boot_id':str(base/'boot-id'),
        }.items():text=text.replace(old,new)
        text=text.replace('export PATH=/system/bin:/system/xbin',f'export PATH={bins}:/usr/bin:/bin')
        script=base/'cache.sh';script.write_text(text)
        return script,ro,state

    def test_copy_preserves_bytes_permissions_and_symlinks_before_ready(self):
        with tempfile.TemporaryDirectory() as d:
            base=Path(d);script,ro,state=self.fixture(base)
            before={str(p.relative_to(ro)):hashlib.sha256(p.read_bytes()).hexdigest() for p in ro.rglob('*') if p.is_file()}
            r=subprocess.run(['sh',str(script)],capture_output=True,timeout=10)
            self.assertEqual(r.returncode,0,r.stdout+r.stderr)
            cache=state/'cache';self.assertTrue((cache/'ready').read_text().startswith('venus-runtime-cache-v1\nboot='))
            self.assertEqual((cache.stat().st_mode&0o777),0o700)
            self.assertEqual((cache/'system/system/lib64/library.so').stat().st_mode&0o777,0o640)
            self.assertEqual(os.readlink(cache/'system/system/lib64/alias.so'),'library.so')
            self.assertEqual(os.readlink(cache/'system/system/bin/linker64'),'/apex/com.android.runtime/bin/linker64')
            self.assertEqual(os.readlink(cache/'system/system/bin/ip-wrapper-1.0'),'netutils-wrapper-1.0')
            for name,digest in before.items():
                self.assertEqual(hashlib.sha256((cache/name).read_bytes()).hexdigest(),digest)
                self.assertEqual(hashlib.sha256((ro/name).read_bytes()).hexdigest(),digest)
            self.assertEqual((cache/'firmware/trustlet.mdt').read_bytes(),b'firmware fixture\x00\xff')
            libraries=(cache/'vendor-libraries.sha256').read_text().splitlines()
            other=(cache/'vendor-other.sha256').read_text().splitlines()
            self.assertTrue(libraries and other)
            self.assertTrue(all(line.split('  ',1)[1].startswith('lib64/') for line in libraries))
            self.assertFalse(any(line.split('  ',1)[1].startswith('lib64/') for line in other))
            self.assertEqual((cache/'vendor.sha256').read_text().splitlines(),libraries+other)
    def test_low_memory_stops_before_copying_runtime_payload(self):
        with tempfile.TemporaryDirectory() as d:
            script,_,state=self.fixture(Path(d),memory=1)
            r=subprocess.run(['sh',str(script)],capture_output=True,timeout=10)
            self.assertNotEqual(r.returncode,0);self.assertIn(b'Insufficient RAM',r.stdout)
            self.assertFalse((state/'cache').exists());self.assertFalse((state/'cache.new/vendor').exists())
    def test_copy_corruption_never_publishes_ready_cache(self):
        with tempfile.TemporaryDirectory() as d:
            script,ro,state=self.fixture(Path(d),corrupt=True)
            original=(ro/'vendor/lib64/backend.so').read_bytes()
            r=subprocess.run(['sh',str(script)],capture_output=True,timeout=10)
            self.assertNotEqual(r.returncode,0);self.assertFalse((state/'cache').exists())
            self.assertFalse((state/'cache.new/ready').exists());self.assertEqual((ro/'vendor/lib64/backend.so').read_bytes(),original)
    def test_prior_cache_is_not_adopted_or_overwritten(self):
        with tempfile.TemporaryDirectory() as d:
            script,_,state=self.fixture(Path(d),prior=True)
            r=subprocess.run(['sh',str(script)],capture_output=True,timeout=10)
            self.assertNotEqual(r.returncode,0);self.assertEqual((state/'cache/ready').read_text(),'previous receipt\n')

    def test_failed_worker_joins_other_workers_before_returning_without_ready(self):
        with tempfile.TemporaryDirectory() as d:
            base=Path(d);script,_,state=self.fixture(base)
            # One tree fails while another is still copying. A failed job
            # must not leave background writes after the setup owner exits.
            mock=base/'bin/cp'
            mock.write_text(f'''#!/bin/sh
case "$2" in
  */vendor/lib64) exit 47;;
  */firmware/.) sleep 0.3; /usr/bin/cp "$@" || exit; touch '{base}/worker-finished'; exit 0;;
esac
exec /usr/bin/cp "$@"
''')
            mock.chmod(0o700)
            # Use files, not pipes: inherited stdout must not itself make
            # subprocess.run wait for an accidentally orphaned worker.
            with (base/'stdout').open('wb') as out, (base/'stderr').open('wb') as err:
                r=subprocess.run(['sh',str(script)],stdout=out,stderr=err,timeout=10)
            self.assertNotEqual(r.returncode,0)
            self.assertTrue((base/'worker-finished').is_file())
            self.assertFalse((state/'cache').exists())
            self.assertFalse((state/'cache.new/ready').exists())
            self.assertIn('cache was not published',(base/'stdout').read_text())

if __name__=='__main__':unittest.main()
