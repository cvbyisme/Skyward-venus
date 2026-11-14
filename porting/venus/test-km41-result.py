#!/usr/bin/env python3
"""Offline regression tests: no device, adb, mounts or credentials."""
from pathlib import Path
import runpy
import unittest
ns=runpy.run_path(str(Path(__file__).with_name('verify-km41-result.py')))
check=ns['failures']
BOOT='53a24e4f-b7aa-430a-9b71-894bfcd0d0dd'
HW='KEYMASTER_HIDL_READY version=41\n'
META=HW+'''KEYMASTER_METADATA_UNWRAP_OK bytes=236
KEYMASTER_EPHEMERAL_OK bytes=68
KEYMASTER_ORIGINAL_KEY_FILES_UNCHANGED
KEYMASTER_PHASE KEYMASTER_DIAGNOSTIC_COMPLETE
'''
DATA=META+'''KEYMASTER_DECRYPTED_F2FS_SUPERBLOCK_OK
KEYMASTER_DATA_RO_MOUNT_OK; original /data remains untouched
KEYMASTER_PHASE DATA_RO_UMOUNT_OK
KEYMASTER_PHASE DATA_RO_DM_REMOVE_OK
KEYMASTER_DATA_RO_CLEANUP_OK
'''
EXIT='KEYMASTER_REMOTE_EXIT=0\n'
class Results(unittest.TestCase):
    def test_complete_data(self):
        self.assertEqual(check('--data-ro', DATA+EXIT,BOOT,BOOT,0,True),[])
    def test_mount_only_is_not_success_even_when_adb_exits_zero(self):
        partial=HW+'KEYMASTER_METADATA_UNWRAP_OK bytes=236\nKEYMASTER_EPHEMERAL_OK bytes=68\nKEYMASTER_DECRYPTED_F2FS_SUPERBLOCK_OK\nKEYMASTER_DATA_RO_MOUNT_OK; original /data remains untouched\n'
        self.assertTrue(check('--data-ro',partial,BOOT,BOOT,0,True))
    def test_restart_is_failure_even_with_complete_markers(self):
        self.assertTrue(check('--data-ro',DATA+EXIT,BOOT,'a4b32aa8-9517-4383-bd60-b9d620bc0c99',0,True))
    def test_adb_disconnect_and_missing_boot_id(self):
        self.assertTrue(check('--data-ro',DATA+EXIT,BOOT,'',1,True))
    def test_capture_not_ready(self):
        self.assertTrue(check('--metadata',META+EXIT,BOOT,BOOT,0,False))
    def test_metadata_does_not_require_data(self):
        self.assertEqual(check('--metadata',META+EXIT,BOOT,BOOT,0,True),[])
    def test_hardware_mode(self):
        self.assertEqual(check('--hardware',HW+EXIT,BOOT,BOOT,0,True),[])
    def test_de_requires_de_completion(self):
        self.assertTrue(check('--de',DATA+EXIT,BOOT,BOOT,0,True))
        self.assertEqual(check('--de',DATA+'KEYMASTER_DE_CHAIN_OK; no CE credential was submitted\n'+EXIT,BOOT,BOOT,0,True),[])
    def test_nonzero_remote_exit(self):
        self.assertTrue(check('--data-ro',DATA+'KEYMASTER_REMOTE_EXIT=139\n',BOOT,BOOT,0,True))
    def test_incomplete_phase_does_not_match_exact_marker(self):
        self.assertTrue(check('--data-ro',DATA.replace('DATA_RO_DM_REMOVE_OK','DATA_RO_DM_REMOVE_OK_INCOMPLETE')+EXIT,BOOT,BOOT,0,True))
KEEP=META+'''KEYMASTER_DECRYPTED_F2FS_SUPERBLOCK_OK
KEYMASTER_DATA_RO_MOUNT_OK; original /data remains untouched
KEYMASTER_DATA_SESSION_READY; readonly=true; retained=true
KEYMASTER_PHASE DATA_RO_RETAINED
KEYMASTER_DATA_RO_RETAINED; cleanup=not_attempted; CE=not_attempted
'''
class RetainedResults(unittest.TestCase):
    def test_retained_is_not_cleanup_success(self):
        self.assertEqual(check('--data-ro-keep',KEEP+EXIT,BOOT,BOOT,0,True),[])
        self.assertTrue(check('--data-ro',KEEP+EXIT,BOOT,BOOT,0,True))
    def test_missing_receipt_is_failure(self):
        self.assertTrue(check('--data-ro-keep',KEEP.replace('KEYMASTER_DATA_SESSION_READY;','KEYMASTER_DATA_SESSION_MISSING;')+EXIT,BOOT,BOOT,0,True))
    def test_unexpected_teardown_is_failure(self):
        for marker in ['KEYMASTER_PHASE DATA_RO_UMOUNT_BEGIN','KEYMASTER_PHASE DATA_RO_DM_REMOVE_OK','KEYMASTER_DATA_RO_CLEANUP_OK']:
            self.assertTrue(check('--data-ro-keep',KEEP+marker+'\n'+EXIT,BOOT,BOOT,0,True))
    def test_de_keep_requires_reuse_and_de_evidence(self):
        self.assertTrue(check('--de-keep',KEEP+EXIT,BOOT,BOOT,0,True))
        extra='KEYMASTER_DATA_SESSION_REUSED; verified\nKEYMASTER_DE_CHAIN_OK; no credential\n'
        self.assertEqual(check('--de-keep',KEEP+extra+EXIT,BOOT,BOOT,0,True),[])
    def test_restart_is_not_a_retained_success(self):
        self.assertTrue(check('--data-ro-keep',KEEP+EXIT,BOOT,'a4b32aa8-9517-4383-bd60-b9d620bc0c99',0,True))
if __name__=='__main__':unittest.main()
