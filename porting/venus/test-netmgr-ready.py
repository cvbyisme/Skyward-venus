#!/usr/bin/env python3
"""Exercise the real probe with host fixture libraries; no device or sockets."""
from pathlib import Path
import json
import os
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent


class Ping(unittest.TestCase):
    def run_probe(self, library, *args):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            shared = base / 'libnetmgr.so'
            source = (HERE / 'netmgr-ready.c').read_text()
            self.assertEqual(source.count('"/vendor/lib64/libnetmgr.so"'), 1)
            source = source.replace('"/vendor/lib64/libnetmgr.so"', json.dumps(str(shared)))
            (base / 'probe.c').write_text(source)
            cc = os.environ.get('HOST_CC', 'cc')
            if library is not None:
                (base / 'fixture.c').write_text(library)
                subprocess.run([cc, '-std=c11', '-fPIC', '-shared', str(base / 'fixture.c'), '-o', str(shared)], check=True)
            executable = base / 'probe'
            subprocess.run([cc, '-std=c11', '-Wall', '-Wextra', '-Werror', str(base / 'probe.c'), '-ldl', '-o', str(executable)], check=True)
            return subprocess.run([str(executable), *args], capture_output=True, timeout=3)

    def test_only_a_successful_ping_reports_present_and_vendor_output_is_separate(self):
        result = self.run_probe('''#include <stdio.h>
int netmgr_client_send_ping_msg(int timeout_ms) {
 puts("vendor diagnostic"); fflush(stdout);
 return timeout_ms == 500 ? 0 : -1;
}
''')
        self.assertEqual((result.returncode, result.stdout), (0, b'present\n'))
        self.assertIn(b'vendor diagnostic', result.stderr)

    def test_nonzero_ping_does_not_claim_readiness(self):
        for value in [-1, 1, 14]:
            result = self.run_probe(f'int netmgr_client_send_ping_msg(int timeout_ms) {{(void)timeout_ms; return {value};}}')
            self.assertEqual((result.returncode, result.stdout), (0, b'absent\n'))

    def test_load_symbol_and_usage_errors_are_not_absence(self):
        for library, args in [(None, []), ('int another_symbol(void) {return 0;}', []), (None, ['500'])]:
            result = self.run_probe(library, *args)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, b'')
            self.assertTrue(result.stderr)


if __name__ == '__main__':
    unittest.main()
