#!/usr/bin/env python3
"""Offline tests; fake credentials only, never contacts a phone."""
from pathlib import Path
import importlib.util
import contextlib
import hashlib
import tempfile
import io
import json
import os
import pty
import select
import struct
import subprocess
import sys
import time
import unittest
from unittest.mock import patch

HERE=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('ce_local',HERE/'ce-local.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
BOOT='4a23f017-b8b0-4d24-a124-506701f455d8'
PRE='VENUS_CE_PREFLIGHT_OK user=0 credential=pin input_supported=true storage_unlocked=false source_files_unchanged=true\n'
INSPECT=PRE+'VENUS_CE_INSPECTION_COMPLETE; credential_submitted=false; teardown=not_attempted\nVENUS_CE_REMOTE_EXIT=0\n'
UNLOCK=PRE+'''VENUS_CE_SOURCE_FILES_UNCHANGED
KEYMASTER_PHASE CE_UNLOCK_COMPLETE
VENUS_CE_UNLOCK_OK user=0 shared_storage_unlocked=true readonly=true retained=true teardown=not_attempted
VENUS_CE_REMOTE_EXIT=0
'''

class Local(unittest.TestCase):
    def test_exact_length_framing(self):
        data=bytearray(b'unit-test-only')
        self.assertEqual(m.framed(data),struct.pack('<I',len(data))+data)
        with self.assertRaises(ValueError):m.framed(bytearray(4097))
    def test_pin_not_trimmed_or_reinterpreted(self):
        m.validate_local('pin',bytearray(b'1234'))
        for value in [b'',b'123',b' 1234',b'1234\n',b'abcd']:
            with self.assertRaises(ValueError):m.validate_local('pin',bytearray(value))
        with self.assertRaises(ValueError):m.validate_local('pattern',bytearray(b'12369'))
    def test_remote_command_has_no_credential_parameter(self):
        text=m.remote_command(True)
        self.assertIn('--unlock-stdin --accept-credential-check',text)
        for bad in ['--credential ', '1234','tee ','reboot','umount','format']:
            self.assertNotIn(bad,text)
    def test_transport_secret_only_goes_to_stdin(self):
        packet=m.framed(bytearray(b'fixture-secret'))
        with patch.object(m.subprocess,'run',return_value=subprocess.CompletedProcess([],0,b'')) as run:
            m.Device('fixture-device').raw(m.remote_command(True),payload=packet)
        args,kwargs=run.call_args
        self.assertNotIn('fixture-secret',str(args))
        self.assertEqual(args[0][:6], ['adb','-s','fixture-device','shell','-T','-e'])
        self.assertEqual(args[0][6], 'none')
        self.assertEqual(kwargs['input'],packet)
        self.assertNotIn('env',kwargs)
    def test_success_scopes_are_separate(self):
        self.assertFalse(m.check_result(INSPECT,0,BOOT,BOOT,False))
        self.assertFalse(m.check_result(UNLOCK,0,BOOT,BOOT,True))
        self.assertTrue(m.check_result(INSPECT,0,BOOT,BOOT,True))
    def test_truncated_response_even_with_zero_adb_exit_fails(self):
        self.assertTrue(m.check_result(PRE,0,BOOT,BOOT,True))
        self.assertTrue(m.check_result(UNLOCK.replace('VENUS_CE_REMOTE_EXIT=0',''),0,BOOT,BOOT,True))
    def test_reboot_disconnect_and_nonzero_exit_fail(self):
        for after,rc in [('',0),('5'+BOOT[1:],0),(BOOT,1)]:
            self.assertTrue(m.check_result(UNLOCK,rc,BOOT,after,True))
    def test_noninteractive_input_refused_before_getpass(self):
        with patch.object(m.sys,'stdin',io.StringIO('1234\n')),patch.object(m.getpass,'getpass') as read:
            with self.assertRaises(RuntimeError):m.local_credential('pin')
            read.assert_not_called()
    @unittest.skipUnless(sys.platform.startswith('linux'),'requires a Unix PTY')
    def test_local_terminal_hides_pin(self):
        # Run only the input function in a throwaway controlling terminal.
        # It prints a boolean result, never the fake PIN itself.
        pid,fd=pty.fork()
        if pid==0:
            code=("import importlib.util; s=importlib.util.spec_from_file_location('ce',"+repr(str(HERE/'ce-local.py'))+"); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); "
                  "d=m.local_credential('pin'); print('FIXTURE_OK',d==bytearray([56,49,55,50,57,51])); d[:]=b'\\0'*len(d)")
            os.execv(sys.executable,[sys.executable,'-c',code])
        captured=b'';confirmed=False;sent=False;end=time.monotonic()+10
        try:
            while time.monotonic()<end:
                if select.select([fd],[],[],0.2)[0]:
                    try:chunk=os.read(fd,4096)
                    except OSError:break
                    if not chunk:break
                    captured+=chunk
                    if not confirmed and b'UNLOCK' in captured:
                        os.write(fd,b'UNLOCK\n');confirmed=True
                    if confirmed and not sent and '当前锁屏 PIN：'.encode() in captured:
                        os.write(fd,b'817293\n');sent=True
                    if b'FIXTURE_OK True' in captured:break
            self.assertTrue(sent,captured.decode(errors='replace'))
            self.assertIn(b'FIXTURE_OK True',captured)
            self.assertNotIn(b'817293',captured,'PIN was echoed by terminal')
        finally:
            try:os.kill(pid,15)
            except ProcessLookupError:pass
            os.close(fd);os.waitpid(pid,0)


class Transport(unittest.TestCase):
    def setUp(self):
        self.device = m.Device('offline-fixture')
        self.fixture = bytes(range(256))*17 + bytes([0,13,10,126,46,4,255]) + b'END-OF-TRANSPORT-PROBE'
        self.answer = ('VENUS_CE_TRANSPORT_RAW=1\n' + hashlib.sha256(self.fixture).hexdigest()
                       + '  -\nVENUS_CE_TRANSPORT_EXIT=0\n').encode()

    def probe(self, answer=None, rc=0, status=37):
        if answer is None: answer=self.answer
        with patch.object(self.device,'checked',return_value='shell_v2\n'), patch.object(
                self.device,'raw',side_effect=[subprocess.CompletedProcess([],rc,answer),
                                              subprocess.CompletedProcess([],status,b'')]) as raw:
            result=self.device.verify_transport()
            self.assertEqual(raw.call_args_list[0].kwargs['payload'],self.fixture)
            return result

    def test_legacy_exec_and_pty_payloads_refused_before_spawn(self):
        for args in [('exec-out','anything'),('exec-in','anything'),('shell','anything'),
                     ('shell','-tt','anything'),('shell','-T','-x','anything')]:
            with self.subTest(args=args),patch.object(m.subprocess,'run') as run:
                with self.assertRaises(RuntimeError): self.device.run(*args,payload=b'public-fixture')
                run.assert_not_called()

    def test_noninput_commands_get_eof_not_host_terminal(self):
        with patch.object(m.subprocess,'run',return_value=subprocess.CompletedProcess([],0,b'')) as run:
            self.device.run('features')
        self.assertEqual(run.call_args.kwargs['input'],b'')

    def test_missing_shell_v2_refused_without_raw_fallback(self):
        with patch.object(self.device,'checked',return_value='cmd\n'),patch.object(self.device,'raw') as raw:
            with self.assertRaisesRegex(RuntimeError,'shell_v2'):self.device.verify_transport()
            raw.assert_not_called()

    def test_transport_checks_binary_eof_and_exit_status(self):
        result=self.probe()
        self.assertFalse(result['pty'])
        self.assertFalse(result['credential_submitted'])
        self.assertTrue(result['eof_verified'])
        self.assertTrue(result['exit_status_verified'])
        self.assertEqual(result['fixture_bytes'],len(self.fixture))

    def test_tty_or_mangled_or_partial_reply_is_not_accepted(self):
        for answer in [b'VENUS_CE_TRANSPORT_TTY_REJECTED\n',
                       self.answer.replace(b'RAW=1',b'RAW=0'),
                       self.answer.replace(hashlib.sha256(self.fixture).hexdigest().encode(),b'0'*64),
                       self.answer.replace(b'VENUS_CE_TRANSPORT_EXIT=0\n',b''),
                       b'echoed-fixture\n'+self.answer]:
            with self.subTest(answer=answer[:30]):
                with self.assertRaises(RuntimeError):self.probe(answer)

    def test_remote_exit_code_must_not_be_swallowed(self):
        with self.assertRaises(RuntimeError):self.probe(status=0)
        with self.assertRaises(RuntimeError):self.probe(rc=1)

    def test_eof_timeout_refuses_before_asking_for_credentials(self):
        with patch.object(self.device,'checked',return_value='shell_v2\n'),patch.object(
                self.device,'raw',side_effect=subprocess.TimeoutExpired('offline',10)):
            with self.assertRaisesRegex(RuntimeError,'no credential was read'):self.device.verify_transport()

    def test_selftest_runs_before_local_prompt(self):
        with tempfile.TemporaryDirectory(prefix='ce-helper-order-') as temp:
            old_umask=os.umask(0o077)
            os.umask(old_umask)
            root=Path(temp)/'repo'; (root/'target').mkdir(parents=True)
            (Path(temp)/'venus_dump').mkdir()
            out=root/'target'/'fake-build';out.mkdir()
            (root/'target/venus-ce-latest.txt').write_text(str(out)+'\n')
            with patch.object(m,'ROOT',root),patch.object(m.shutil,'which',return_value='/fake/adb'), \
                 patch.object(m,'verify_assets'),patch.object(m,'Device',return_value=self.device), \
                 patch.object(self.device,'checked',return_value=''),patch.object(self.device,'boot',return_value=BOOT), \
                 patch.object(self.device,'session',return_value='identity'), \
                 patch.object(self.device,'verify_transport',side_effect=RuntimeError('transport-rejected')), \
                 patch.object(m,'local_credential') as prompt,patch.object(m.sys,'argv',['ce-local.py','--unlock']), \
                 patch.object(m.sys.stdin,'isatty',return_value=True),contextlib.redirect_stdout(io.StringIO()):
                try:
                    with self.assertRaisesRegex(RuntimeError,'transport-rejected'):m.main()
                    prompt.assert_not_called()
                finally:
                    # main intentionally sets a private umask; restore its previous value.
                    os.umask(old_umask)

    def test_actual_subprocess_pipe_passes_framed_binary_and_eof(self):
        # A real subprocess and pipe, but a fake adb: never contacts a device
        # or an authentication service. It checks options and tty itself.
        with tempfile.TemporaryDirectory(prefix='ce-raw-pipe-') as temp:
            fake=Path(temp)/'fake_adb.py'
            fake.write_text("import sys,os\n"
                "if sys.argv[1:] != ['shell','-T','-e','none','fixture-echo']:sys.exit(64)\n"
                "if os.isatty(0):sys.exit(65)\n"
                "sys.stdout.buffer.write(sys.stdin.buffer.read())\n")
            self.device.base=[sys.executable,str(fake)]
            packet=m.framed(bytearray(bytes(range(256))*15))
            result=self.device.raw('fixture-echo',payload=packet,timeout=5)
            self.assertEqual(result.returncode,0)
            self.assertEqual(result.stdout,packet)

if __name__=='__main__':unittest.main()
