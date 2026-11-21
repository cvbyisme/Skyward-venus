#!/usr/bin/env python3
"""Add the missing Keystore 2.0 policy vocabulary to the original Venus policy.

No permissive domains, wildcard permissions or changes to existing decisions.
Recompile at the input policy version, retain named attributes, and decompile
the result to verify every original effective statement and the exact additions.
"""
from __future__ import annotations
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import struct
import subprocess

HERE = Path(__file__).resolve().parent


def statements(text: str) -> set[str]:
    return {line.strip() for line in text.splitlines()
            if line.strip() and not line.lstrip().startswith(';')}


def verify(original: str, patched: str, additions: str) -> dict:
    old, new, added = map(statements, (original, patched, additions))
    orders = lambda lines: [line for line in lines if line.startswith('(classorder ')]
    assert len(orders(old)) == len(orders(new)) == len(orders(added)) == 1
    old_order = orders(old)[0]
    assert orders(new)[0] == old_order[:-2] + ' keystore2 keystore2_key))', 'existing class IDs changed'
    old.remove(old_order)
    new.remove(orders(new)[0])
    added.remove(orders(added)[0])
    attrs = {m[1] for line in old if (m := re.fullmatch(r'\(typeattribute (\S+)\)', line))}
    nonempty = {line.split()[1] for line in old if line.startswith('(typeattributeset ')}
    empty = attrs - nonempty
    # libsepol drops rules with an empty source/target set. Those grant no
    # permission; all named attributes and all effective rules must survive.
    removed = old - new
    for line in removed:
        fields = line.split()
        assert fields[0] in {'(allow', '(auditallow', '(dontaudit'} and (
            fields[1] in empty or fields[2] in empty), 'original policy statement lost: ' + line
    assert new - old == added, 'unexpected policy additions: ' + repr((new - old) ^ added)
    return {'original_statements': len(old), 'empty_rules_removed': len(removed),
            'added_statements': sorted(added), 'existing_effective_rules_preserved': True,
            'existing_class_ids_preserved': True}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('boot', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--checkpolicy', required=True)
    parser.add_argument('--secilc', required=True)
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location('bootcheck', HERE / 'verify-entryfix.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    _, _, entries, _, _ = module.boot(args.boot)
    original = entries['sepolicy'][1]
    assert struct.unpack_from('<I', original)[0] == 0xf97cff8c, 'not an SELinux policy'
    name_size = struct.unpack_from('<I', original, 4)[0]
    version = struct.unpack_from('<I', original, 8 + name_size)[0]
    args.output.mkdir(parents=True, exist_ok=False)
    source = args.output / 'source.policy'
    source.write_bytes(original)
    source_cil = args.output / 'source.cil'
    subprocess.run([args.checkpolicy, '-b', '-M', '-C', str(source), '-o', str(source_cil)], check=True)
    text = source_cil.read_text()
    extra = (HERE / 'crypto-policy.cil').read_text()
    assert '(class keystore2 ' not in text and '(class keystore2_key ' not in text, 'base already has Keystore 2.0; review its existing rules instead'
    keep = ''.join(f'(expandtypeattribute {name} false)\n'
                   for name in re.findall(r'^\(typeattribute ([^ )]+)\)$', text, re.M))
    combined = args.output / 'combined.cil'
    combined.write_text(text + keep + extra)
    result = args.output / 'recovery-sepolicy'
    subprocess.run([args.secilc, '-M', 'true', '-c', str(version), str(combined),
                    '-o', str(result), '-f', '/dev/null'], check=True)
    checked = args.output / 'result.cil'
    subprocess.run([args.checkpolicy, '-b', '-M', '-C', str(result), '-o', str(checked)], check=True)
    report = verify(text, checked.read_text(), extra)
    source_hash = hashlib.sha256(original).hexdigest()
    (args.output / 'recovery-sepolicy.source.sha256').write_text(source_hash + '\n')
    report.update(status='PASS', policy_version=version, source_sha256=source_hash,
                  policy_sha256=hashlib.sha256(result.read_bytes()).hexdigest())
    (args.output / 'validation.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))


if __name__ == '__main__':
    main()
