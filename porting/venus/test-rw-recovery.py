#!/usr/bin/env python3
"""Run the integrated runtime orchestration against private host fixtures.
No adb, real hardware services, device nodes or credentials are used.
"""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[1]

class Runtime(unittest.TestCase):
    def setup_fixture(self, root, *, old=False, mounted=False, checksum_fail=False, foreign=False, boot_fail=False, ready_version=None):
        b=root/'bin'; b.mkdir(); assets=root/'assets';assets.mkdir()
        log=root/'calls'; log.write_text('')
        def command(name,text):
            p=b/name;p.write_text('#!/bin/sh\n'+text+'\n');p.chmod(0o700)
        command('id', 'echo 0')
        command('sha256sum', f'echo checksum >> "{log}"; exit {1 if checksum_fail else 0}')
        (root/'marker').write_text('venus-recovery-rw-v1\n')
        (root/'mounts').write_text('/dev/block/dm-0 /data f2fs rw 0 0\n' if mounted else '')
        if ready_version is not None:
            (root/'recovery').mkdir()
            (root/'recovery/runtime-ready').write_text(f'venus-runtime-ready-v{ready_version}\n')
        if old:(root/'old-session').write_text('existing verified old session\n')
        names=['binderfs-setup','km41-launch','km41-rpmb','km41-kmsg','runtime.img','vndk30.img','framework-manifest.xml','km41-runtime.sh','ce-runtime.sh','boot-runtime.sh']
        for n in names:(assets/n).write_text('fixture\n')
        (assets/'SHA256SUMS').write_text('fixture hash list\n')
        (assets/'km41-runtime.sh').write_text(f'#!/bin/sh\necho keymaster-start >> "{log}"\n')
        (assets/'ce-runtime.sh').write_text(f'#!/bin/sh\n[ "$1" = full-recovery ] || exit 42\necho gatekeeper-start >> "{log}"\n')
        (assets/'boot-runtime.sh').write_text(f'#!/bin/sh\necho boot-control-start >> "{log}"\nexit {42 if boot_fail else 0}\n')
        if foreign:
            p=root/'km-stage';p.mkdir();(p/'runtime.img').write_text('not our file')
        script=(HERE/'rw-runtime.sh').read_text()
        replacements={
          '/tmp/skyward-km41-stage':str(root/'km-stage'),
          '/tmp/skyward-ce-stage':str(root/'ce-stage'),
          '/tmp/skyward-km41-data-session.txt':str(root/'old-session'),
          '/tmp/skyward-km41':str(root/'km-root'),
          '/tmp/skyward-recovery':str(root/'recovery'),
          '/skyward/venus-recovery.txt':str(root/'marker'),
          '/skyward/runtime':str(assets),
          '/proc/mounts':str(root/'mounts'),
        }
        for a,c in replacements.items():script=script.replace(a,c)
        script=script.replace('export PATH=/system/bin:/system/xbin',f'export PATH={b}:/usr/bin:/bin')
        p=root/'runtime.sh';p.write_text(script)
        return p,log

    def run_fixture(self,**kw):
        with tempfile.TemporaryDirectory(prefix='skyward-runtime-fixture-') as d:
            root=Path(d);script,log=self.setup_fixture(root,**kw)
            result=subprocess.run(['/bin/sh',str(script)],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=5)
            return result,log.read_text(),(root/'recovery/runtime-ready').exists(),(root/'recovery/setup.lock').exists()

    def test_fresh_boot_starts_services_then_marks_ready(self):
        r,c,ready,locked=self.run_fixture()
        self.assertEqual(r.returncode,0,r.stdout.decode());self.assertEqual(c.splitlines(),['checksum','keymaster-start','gatekeeper-start','boot-control-start'])
        self.assertTrue(ready);self.assertFalse(locked)
    def test_old_ro_session_is_never_hot_converted(self):
        r,c,ready,locked=self.run_fixture(old=True)
        self.assertNotEqual(r.returncode,0);self.assertIn(b'Existing RO',r.stdout);self.assertEqual(c,'');self.assertFalse(ready)
    def test_old_runtime_with_live_data_is_refused_without_restart(self):
        r,c,ready,locked=self.run_fixture(ready_version=1,mounted=True)
        self.assertNotEqual(r.returncode,0);self.assertIn(b'older runtime',r.stdout)
        self.assertEqual(c,'');self.assertTrue(ready);self.assertFalse(locked)
    def test_control_startup_failure_never_marks_full_runtime_ready(self):
        r,c,ready,locked=self.run_fixture(boot_fail=True)
        self.assertNotEqual(r.returncode,0);self.assertIn('boot-control-start',c);self.assertFalse(ready);self.assertFalse(locked)
    def test_existing_data_mount_is_not_adopted(self):
        r,c,ready,locked=self.run_fixture(mounted=True)
        self.assertNotEqual(r.returncode,0);self.assertEqual(c,'');self.assertFalse(ready)
    def test_bad_checksum_stops_before_any_hal_launch(self):
        r,c,ready,locked=self.run_fixture(checksum_fail=True)
        self.assertNotEqual(r.returncode,0);self.assertEqual(c.splitlines(),['checksum']);self.assertFalse(ready);self.assertFalse(locked)
    def test_foreign_stage_is_not_overwritten(self):
        r,c,ready,locked=self.run_fixture(foreign=True)
        self.assertNotEqual(r.returncode,0);self.assertNotIn('keymaster-start',c);self.assertFalse(ready);self.assertFalse(locked)
    def test_no_legacy_unmount_or_host_credential_step_in_startup(self):
        text=(HERE/'rw-runtime.sh').read_text()
        for token in ['umount ', 'mkfs.', 'fastboot ', 'adb ', ' --unlock', ' --credential']:
            self.assertNotIn(token,text)
        self.assertIn('ce-runtime.sh full-recovery',text)
    def test_full_ui_never_formats_effect_payloads(self):
        text=(ROOT/'crates/skyward/src/ui.rs').read_text()
        self.assertNotIn('eprintln!("effect {fx:?}")',text)
        self.assertIn('std::mem::discriminant(&fx)',text)
    def test_diagnostic_credential_backend_stays_separate(self):
        text=(ROOT/'crates/skyward/src/decrypt.rs').read_text()
        self.assertIn('if crate::venus_rw::enabled()',text)
        self.assertIn('with_key_service(data::open_without_credential)',text)
        self.assertIn('Zeroizing::new(credential)',text)

if __name__=='__main__':unittest.main()
