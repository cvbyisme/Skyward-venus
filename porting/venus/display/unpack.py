#!/usr/bin/env python3
"""Validate a generated-only frame and export immutable, named blob candidates."""
import hashlib
import json
from pathlib import Path
import struct
import sys

SIZES={'SDE_DSPP_IGC_V4':3096,'SDE_DSPP_GC_V1':6152,'SDE_DSPP_GAMUT_V4':39536}
def decode(data):
    if len(data)<8 or data[:4]!=b'VCL1':raise ValueError('not a complete VCL1 frame')
    n,=struct.unpack_from('<I',data,4)
    if not 1<=n<=len(SIZES):raise ValueError('invalid record count')
    pos=8;records={}
    for _ in range(n):
        if pos+12>len(data):raise ValueError('truncated record')
        length,enabled,size=struct.unpack_from('<III',data,pos);pos+=12
        if length>64 or pos+length+size>len(data):raise ValueError('invalid bounds')
        name=data[pos:pos+length].decode('ascii');pos+=length
        if name not in SIZES or name in records:raise ValueError('unknown/duplicate property')
        if enabled not in (0,1) or size!=(SIZES[name] if enabled else 0):raise ValueError('invalid blob size')
        records[name]=data[pos:pos+size];pos+=size
    if pos!=len(data):raise ValueError('trailing data')
    return records

def main():
    if len(sys.argv)!=3:raise SystemExit('usage: unpack.py FRAME NEW_OUTPUT_DIR')
    records=decode(Path(sys.argv[1]).read_bytes());out=Path(sys.argv[2]);out.mkdir(mode=0o700)
    manifest={'drm_applied':False,'complete_for_returned_assets_only':True,'blobs':{}}
    for name,data in records.items():
        if data:(out/(name+'.bin')).write_bytes(data)
        manifest['blobs'][name]={'enabled':bool(data),'size':len(data),'sha256':hashlib.sha256(data).hexdigest()}
    (out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print(json.dumps(manifest,indent=2))
if __name__=='__main__':main()
