#!/usr/bin/env python3
"""End-to-end host wrapper tests with a fake adb. Never contact real devices."""
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
FAKE = r'''#!/usr/bin/python3
import os,sys,time
from pathlib import Path
args=sys.argv[1:]
if args[:1]==['-s']:args=args[2:]
a=' '.join(args)
s=os.environ['KM41_MOCK_SCENARIO'];root=Path(os.environ['KM41_MOCK_STATE'])
with (root/'calls').open('a') as f:f.write(a+'\n')
if args and args[0]=='push':sys.exit(0)
if args and args[0]=='exec-out':
 print('KM41_KMSG_CAPTURE_READY mock-only',flush=True)
 print('6,1,1000,-;mock kernel record',flush=True)
 time.sleep(60);sys.exit(0)
if 'test "$(id -u)" = 0' in a:sys.exit(1 if s=='normal-system' else 0)
if args[1:]==['cat','/proc/sys/kernel/random/boot_id']:
 p=root/'count';n=int(p.read_text()) if p.exists() else 0;p.write_text(str(n+1))
 print('a4b32aa8-9517-4383-bd60-b9d620bc0c99' if n and s=='restarted' else '53a24e4f-b7aa-430a-9b71-894bfcd0d0dd');sys.exit(0)
if args and args[0]=='shell' and 'KEYMASTER_REMOTE_EXIT' in a:
 print('KEYMASTER_HIDL_READY version=41')
 print('KEYMASTER_METADATA_UNWRAP_OK bytes=236')
 print('KEYMASTER_EPHEMERAL_OK bytes=68')
 print('KEYMASTER_DECRYPTED_F2FS_SUPERBLOCK_OK')
 print('KEYMASTER_DATA_RO_MOUNT_OK; original /data remains untouched')
 print('KEYMASTER_PHASE DATA_RO_MOUNTED')
 if s=='truncated':sys.exit(0)
 print('KEYMASTER_DATA_SESSION_READY; readonly=true; retained=true')
 print('KEYMASTER_PHASE DATA_RO_RETAINED')
 print('KEYMASTER_DATA_RO_RETAINED; cleanup=not_attempted; CE=not_attempted')
 if '--de' in a.replace(';',' ').split():
  print('KEYMASTER_DATA_SESSION_REUSED; verified')
  print('KEYMASTER_DE_CHAIN_OK; no CE credential')
 print('KEYMASTER_ORIGINAL_KEY_FILES_UNCHANGED')
 print('KEYMASTER_PHASE KEYMASTER_DIAGNOSTIC_COMPLETE')
 print('KEYMASTER_REMOTE_EXIT=0');sys.exit(0)
if 'km41-runtime start' in a:print('KEYMASTER41_RUNTIME_READY mock-only')
sys.exit(0)
'''

class HostWrapper(unittest.TestCase):
    def execute(self, scenario, mode='--data-ro-keep'):
        with tempfile.TemporaryDirectory(prefix='km41-mock-') as tmp:
            root=Path(tmp);repo=root/'repo';tools=root/'tools'
            (repo/'porting/venus').mkdir(parents=True);(repo/'target').mkdir()
            (root/'SKYWARD/venus_dump').mkdir(parents=True);tools.mkdir()
            for name in ('prepare-keymaster41.sh','verify-km41-result.py'):
                shutil.copy2(HERE/name,repo/'porting/venus'/name)
            (tools/'adb').write_text(FAKE);(tools/'adb').chmod(0o700)
            out=repo/'out';(out/'assets').mkdir(parents=True)
            names=('skyward','binderfs-setup','km41-launch','km41-kmsg','km41-rpmb','runtime.img','vndk30.img','framework-manifest.xml','km41-runtime')
            checks=[]
            for n in names:
                p=out/'assets'/n;p.write_bytes(b'mock asset\n')
                checks.append(hashlib.sha256(p.read_bytes()).hexdigest()+'  assets/'+n)
            (out/'ASSETS.sha256').write_text('\n'.join(checks)+'\n')
            (repo/'target/venus-keymaster41-latest.txt').write_text(str(out)+'\n')
            env=dict(os.environ,HOME=str(root),PATH=str(tools)+':'+os.environ['PATH'],
                     KM41_MOCK_SCENARIO=scenario,KM41_MOCK_STATE=str(root))
            result=subprocess.run(['bash',str(repo/'porting/venus/prepare-keymaster41.sh'),mode],
                                  env=env,capture_output=True,text=True,timeout=15)
            text=result.stdout+result.stderr
            records=list((root/'SKYWARD/venus_dump').glob('*/summary.json'))
            summary=json.loads(records[0].read_text()) if records else None
            calls=(root/'calls').read_text() if (root/'calls').exists() else ''
            return result.returncode,text,summary,calls
    def test_success(self):
        rc,text,summary,_=self.execute('success')
        self.assertEqual(rc,0,text);self.assertTrue(summary['verified'])
        self.assertIn('VERIFIED --data-ro-keep',text)
        self.assertFalse(summary['cleanup_verified']);self.assertFalse(summary['ce_verified'])
    def test_truncated_zero_exit_reports_failure(self):
        rc,text,summary,_=self.execute('truncated')
        self.assertNotEqual(rc,0,text);self.assertFalse(summary['verified'])
        self.assertEqual(summary['last_host_phase'],'KEYMASTER_PHASE DATA_RO_MOUNTED')
        self.assertIn('remote command did not report',text)
    def test_changed_boot_id_reports_failure(self):
        rc,text,summary,_=self.execute('restarted')
        self.assertNotEqual(rc,0,text);self.assertFalse(summary['verified'])
        self.assertIn('BOOT_ID_CHANGED',text)
    def test_main_android_refused_before_push(self):
        rc,text,summary,calls=self.execute('normal-system')
        self.assertNotEqual(rc,0,text);self.assertIsNone(summary)
        self.assertNotIn('push ',calls);self.assertNotIn('exec-out ',calls)

    def test_legacy_teardown_refused_before_adb(self):
        for mode in ('--data-ro','--de'):
            rc,text,summary,calls=self.execute('success',mode)
            self.assertEqual(rc,2,text);self.assertEqual(calls,'');self.assertIsNone(summary)
    def test_retained_command_always_passes_keep_flag(self):
        rc,text,summary,calls=self.execute('success')
        self.assertEqual(rc,0,text)
        self.assertIn('--metadata --data-ro --keep-data-ro;',calls)
        self.assertNotIn('--allow-teardown-test',calls)
    def test_de_keep_passes_both_explicit_flags(self):
        rc,text,summary,calls=self.execute('success','--de-keep')
        self.assertEqual(rc,0,text)
        self.assertIn('--metadata --data-ro --keep-data-ro --de;',calls)
        self.assertFalse(summary['cleanup_verified'])
if __name__=='__main__':unittest.main()
