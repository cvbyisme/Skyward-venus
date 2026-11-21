#!/usr/bin/env python3
"""The policy audit must reject effective-rule loss and extra privileges."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('policy', Path(__file__).with_name('prepare-crypto-policy.py'))
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class PolicyAudit(unittest.TestCase):
    def setUp(self):
        self.before = '\n'.join([
            '(classorder (security drmservice))', '(typeattribute empty)',
            '(allow empty recovery (file (read)))',
            '(allow recovery recovery (file (read write)))',
        ])
        self.extra = Path(__file__).with_name('crypto-policy.cil').read_text()
        self.after = self.before.replace('(allow empty recovery (file (read)))\n', '').replace(
            '(classorder (security drmservice))', '(classorder (security drmservice keystore2 keystore2_key))')
        self.after += '\n' + self.extra.replace('(classorder (drmservice keystore2 keystore2_key))', '')

    def test_only_additions_and_empty_rule_elimination(self):
        result = policy.verify(self.before, self.after, self.extra)
        self.assertTrue(result['existing_effective_rules_preserved'])
        self.assertEqual(result['empty_rules_removed'], 1)

    def test_effective_permission_removal_rejected(self):
        with self.assertRaises(AssertionError):
            policy.verify(self.before, self.after.replace('(file (read write))', '(file (read))'), self.extra)

    def test_unrelated_access_rejected(self):
        with self.assertRaises(AssertionError):
            policy.verify(self.before, self.after + '\n(allow shell vold_key (keystore2_key (use)))', self.extra)

    def test_new_permissive_domain_rejected(self):
        with self.assertRaises(AssertionError):
            policy.verify(self.before, self.after + '\n(typepermissive recovery)', self.extra)

    def test_existing_class_order_preserved(self):
        with self.assertRaises(AssertionError):
            policy.verify(self.before, self.after.replace('(security drmservice ', '(drmservice security '), self.extra)


if __name__ == '__main__':
    unittest.main()
