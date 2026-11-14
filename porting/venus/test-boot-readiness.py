#!/usr/bin/env python3
"""Exercise real readiness functions with inert process and Binder fixtures."""
from pathlib import Path
import subprocess
import tempfile
import unittest

HERE=Path(__file__).resolve().parent

class Readiness(unittest.TestCase):
    def run_case(self, mode):
        with tempfile.TemporaryDirectory(prefix='venus-control-ready-') as d:
            root=Path(d);(root/'servicemanager.pid').write_text('101\n');(root/'boot.pid').write_text('202\n')
            client=root/'client';client.write_text(f'#!/bin/sh\necho queried >> "{root}/queries"\necho "android.hardware.boot.IBootControl/default is described and registered"\n');client.chmod(0o700)
            source=(HERE/'boot-runtime.sh').read_text().split('# BOOT_READINESS_BEGIN\n')[1].split('# BOOT_READINESS_END')[0]
            source=source.replace('/system/bin/skyward',str(client))
            prefix=f'''set -eu
R={root}
NAME=android.hardware.boot.IBootControl/default
mode={mode}
# No real signal or process inspection occurs in these functions.
kill() {{ [ "$1" = -0 ] && {{ [ "$2" = 101 ] || {{ [ "$2" = 202 ] && [ "$mode" != dead_boot ]; }}; }}; }}
sleep() {{ :; }}
readlink() {{
    case "$1" in
        /proc/101/root) echo "$R/root-servicemanager";;
        /proc/202/root)
            if [ "$mode" = foreign_root ]; then echo /; return; fi
            if [ "$mode" = starting ]; then
                n=0; [ ! -r "$R/polls" ] || n=$(cat "$R/polls")
                n=$((n+1)); echo "$n" >"$R/polls"
                if [ "$n" -lt 3 ]; then echo /; return; fi
            fi
            echo "$R/root-boot";;
        *) return 1;;
    esac
}}
'''
            script=root/'test.sh';script.write_text(prefix+source+'\nwait_service "is described and registered$" boot\n')
            result=subprocess.run(['sh',str(script)],capture_output=True,timeout=5)
            queries=(root/'queries').read_text().splitlines() if (root/'queries').exists() else []
            polls=int((root/'polls').read_text()) if (root/'polls').exists() else 0
            return result,queries,polls
    def test_dead_boot_is_not_mistaken_for_the_live_manager(self):
        r,q,_=self.run_case('dead_boot')
        self.assertNotEqual(r.returncode,0);self.assertIn(b'exited: boot',r.stdout);self.assertEqual(q,[])
    def test_launcher_may_finish_its_chroot_before_binder_is_queried(self):
        r,q,n=self.run_case('starting')
        self.assertEqual(r.returncode,0,r.stdout+r.stderr);self.assertGreaterEqual(n,3);self.assertEqual(len(q),1)
    def test_foreign_root_never_reaches_the_binder_query(self):
        r,q,_=self.run_case('foreign_root')
        self.assertNotEqual(r.returncode,0);self.assertIn(b'not ready: boot',r.stdout);self.assertEqual(q,[])

if __name__=='__main__':unittest.main()
