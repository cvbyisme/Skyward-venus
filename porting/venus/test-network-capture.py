#!/usr/bin/env python3
"""Exercise lost-capture/reboot refusal and proof boundaries with fake adb."""
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest

HERE=Path(__file__).resolve().parent
FAKE=r'''#!/usr/bin/python3
from pathlib import Path
import json,os,sys,time
args=sys.argv[1:]
if args[:1]==['-s']:args=args[2:]
root=Path(os.environ['MOCK_STATE']);case=os.environ['MOCK_CASE'];joined=' '.join(args)
with (root/'calls').open('a') as f:f.write(joined+'\n')
if args[:1]==['shell'] and 'venus-recovery-rw-v1' in joined:sys.exit(1 if case=='main-system' else 0)
if args[:1]==['shell'] and 'cat /proc/sys/kernel/random/boot_id' in joined:
 p=root/'boot-count';n=int(p.read_text()) if p.exists() else 0;p.write_text(str(n+1))
 if case=='capture-down' and n==1:time.sleep(.2)
 print('a4b32aa8-9517-4383-bd60-b9d620bc0c99' if case=='restarted' and n>=2 else '53a24e4f-b7aa-430a-9b71-894bfcd0d0dd');sys.exit(0)
if args[:2]==['shell','sha256sum']:
 print(os.environ['MOCK_SHA']+'  '+args[-1]);sys.exit(0)
if args[:2]==['exec-out','/skyward/runtime/km41-kmsg']:
 print('KM41_KMSG_CAPTURE_READY mock-only',flush=True)
 print('6,1,1000,-;mock kernel record',flush=True)
 if case=='capture-down':time.sleep(.05);sys.exit(0)
 time.sleep(60);sys.exit(0)
if args[:1]==['exec-out']:
 if 'cellular-status' in args:print(json.dumps({'read_only':True,'cards':[]}))
 else:
  print('HTTP reaches the internet')
  print('HTTPS status=200 OK bytes=5 sha256='+'a'*64)
 sys.exit(3 if case=='probe-failed' else 0)
sys.exit(0)
'''
class Capture(unittest.TestCase):
    def run_mock(self,case,mode='--status',dns=('10.0.0.53',)):
        with tempfile.TemporaryDirectory(prefix='network-capture-test-') as tmp:
            base=Path(tmp);repo=base/'repo';scripts=repo/'porting/venus';scripts.mkdir(parents=True)
            shutil.copy2(HERE/'capture-network.sh',scripts/'capture-network.sh')
            image=base/'image';(image/'assets').mkdir(parents=True)
            payload=b'fake diagnostic asset\n';digest=hashlib.sha256(payload).hexdigest()
            for name in ('skyward','km41-kmsg'):(image/'assets'/name).write_bytes(payload)
            (image/'ASSETS.sha256').write_text(''.join(digest+'  assets/'+n+'\n' for n in ('skyward','km41-kmsg')))
            probe=base/'network-check';probe.write_bytes(payload)
            tools=base/'tools';tools.mkdir();(tools/'adb').write_text(FAKE);(tools/'adb').chmod(0o700)
            logs=base/'logs';logs.mkdir()
            env=dict(os.environ,PATH=str(tools)+':'+os.environ['PATH'],VENUS_RECOVERY_DIR=str(image),VENUS_NETWORK_CHECK=str(probe),
                     VENUS_LOG_ROOT=str(logs),MOCK_STATE=str(base),MOCK_CASE=case,MOCK_SHA=digest)
            command=['bash',str(scripts/'capture-network.sh'),mode]
            if mode=='--http-tls':command.extend(dns)
            run=subprocess.run(command,env=env,capture_output=True,text=True,timeout=15)
            calls=(base/'calls').read_text() if (base/'calls').exists() else ''
            reports=list(logs.glob('*/summary.json'))
            summary=json.loads(reports[0].read_text()) if reports else None
            return run.returncode,run.stdout+run.stderr,calls,summary
    def test_status_never_claims_internet(self):
        rc,text,_,summary=self.run_mock('ok')
        self.assertEqual(rc,0,text);self.assertTrue(summary['verified']);self.assertFalse(summary['internet_verified'])
    def test_complete_tls_and_live_capture_are_required(self):
        rc,text,calls,summary=self.run_mock('ok','--http-tls')
        self.assertEqual(rc,0,text);self.assertTrue(summary['internet_verified']);self.assertIn('push ',calls)
        self.assertFalse(summary['disconnect_reconnect_tested'])
    def test_capture_loss_before_test_refuses_probe_and_deployment(self):
        rc,text,calls,summary=self.run_mock('capture-down','--http-tls')
        self.assertNotEqual(rc,0,text);self.assertNotIn('push ',calls);self.assertNotIn(' cellular-status',calls);self.assertIsNone(summary)
    def test_main_android_refused_before_any_capture_or_deployment(self):
        rc,text,calls,_=self.run_mock('main-system','--http-tls')
        self.assertNotEqual(rc,0,text);self.assertNotIn('exec-out',calls);self.assertNotIn('push ',calls)
    def test_reboot_invalidates_success_output(self):
        rc,text,_,summary=self.run_mock('restarted','--http-tls')
        self.assertNotEqual(rc,0,text);self.assertFalse(summary['internet_verified']);self.assertTrue(any('boot identity' in e for e in summary['failures']))
    def test_nonzero_probe_invalidates_success_output(self):
        rc,text,_,summary=self.run_mock('probe-failed','--http-tls')
        self.assertNotEqual(rc,0,text);self.assertFalse(summary['internet_verified']);self.assertEqual(summary['adb_exit'],3)
    def test_bad_dns_refused_before_adb(self):
        for dns in (('10.0.0.1;reboot',),('0.0.0.0',),('127.0.0.1',)):
            rc,text,calls,_=self.run_mock('ok','--http-tls',dns)
            self.assertNotEqual(rc,0,text);self.assertEqual(calls,'')
if __name__=='__main__':unittest.main()
