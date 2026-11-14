#!/usr/bin/env python3
import struct
import unittest
from unpack import decode, SIZES

def entry(name, enabled, data):
    name=name.encode('ascii')
    return struct.pack('<III',len(name),enabled,len(data))+name+data
def frame(*records):return b'VCL1'+struct.pack('<I',len(records))+b''.join(records)

class Protocol(unittest.TestCase):
    def test_enabled_and_disable(self):
        data=frame(entry('SDE_DSPP_IGC_V4',1,b'\0'*3096),entry('SDE_DSPP_GC_V1',0,b''))
        self.assertEqual(len(decode(data)['SDE_DSPP_IGC_V4']),3096)
        self.assertEqual(decode(data)['SDE_DSPP_GC_V1'],b'')
    def test_reject_every_truncation(self):
        data=frame(entry('SDE_DSPP_GAMUT_V4',1,b'\0'*39536))
        for n in range(len(data)):
            with self.assertRaises(ValueError):decode(data[:n])
    def test_no_unknown_duplicate_partial_or_trailing(self):
        known=entry('SDE_DSPP_GC_V1',0,b'')
        for data in [frame(known,known),frame(entry('fake',0,b'')),frame(known)+b'x',
                     frame(entry('SDE_DSPP_GC_V1',1,b'x')),frame(entry('SDE_DSPP_GC_V1',2,b'')),frame()]:
            with self.assertRaises(ValueError):decode(data)

if __name__=='__main__':unittest.main()
