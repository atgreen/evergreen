#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Pair bridge/direct loops from immutable CI release artifacts; retain path evidence."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import time

NAMES = ('ORDINARY-BRIDGE-LEAF', 'ORDINARY-BRIDGE-FORWARD',
         'ORDINARY-BRIDGE-DIRECT-LOOP', 'ORDINARY-BRIDGE-FORWARD-LOOP')
ORDINARY = NAMES[:2]
CHAIN_NAMES = ('ORDINARY-CHAIN-LEAF', 'ORDINARY-CHAIN-ONE', 'ORDINARY-CHAIN-TWO',
               'ORDINARY-CHAIN-ONE-LOOP', 'ORDINARY-CHAIN-TWO-LOOP')
CHAIN_ORDINARY = CHAIN_NAMES[:3]


def comparison(control):
    if control == 'leaf':
        return NAMES, ORDINARY, ('direct', 'forward'), 'ordinary-bridge'
    if control == 'forwarders':
        return CHAIN_NAMES, CHAIN_ORDINARY, ('one', 'two'), 'ordinary-chain'
    raise ValueError(f'unknown control: {control}')

WARM_TEST = ('cli::bytecode::native_transfer_tests::ordinary_t2::'
             'ordinary_register_abi_installs_and_warms_a_four_argument_call')


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def artifact(root, manifest_path):
    manifest = json.loads(manifest_path.read_text())
    if manifest['profile'] != 'release' or manifest['target'] != 'x86_64-unknown-linux-musl':
        raise ValueError('requires an x86-64 musl release CI artifact')
    name = f"target/{manifest['target']}/release/egcl"
    binary = root / name
    if digest(binary) != manifest['artifacts'].get(name):
        raise ValueError('release binary does not match immutable CI manifest')
    return {key: manifest[key] for key in ('revision', 'source_digest', 'profile', 'target', 'features')} | {
        'binary': str(binary.resolve()), 'sha256': digest(binary),
        'manifest': str(manifest_path.resolve()), 'manifest_sha256': digest(manifest_path)}


def match_configuration(before, after):
    for key in ('profile', 'target', 'features'):
        if before[key] != after[key]:
            raise ValueError(f'unmatched build configuration: {key}')


def parse_trial(stdout, log, iterations, samples, ordinary, control='leaf'):
    names, ordinary_names, sides, _ = comparison(control)
    listings = re.findall(r'BRIDGE-CODE-BEGIN (before|after) ([A-Z-]+)\s*\n(.*?)BRIDGE-CODE-END', stdout, re.S)
    code = {}
    for phase, name, listing in listings:
        if (phase, name) in code or name not in names or name not in listing or '; T2 —' not in listing:
            raise ValueError('invalid named T2 listing')
        abi = 'Ordinary x86-64 Lisp ABI' if ordinary and name in ordinary_names else 'Mapped native transfer ABI'
        if abi not in listing:
            raise ValueError(f'{name} missing expected {abi}')
        code[phase, name] = listing
    if len(code) != 2 * len(names) or any(code['before', name] != code['after', name] for name in names):
        raise ValueError('missing or changed current code listing')
    for line in log.splitlines():
        if any(name in line.upper() for name in names) and re.search(r'deopt|invalidat|recompil', line, re.I):
            raise ValueError('named code deoptimized or changed during trial')
    if ordinary and control == 'forwarders':
        for name in ('ORDINARY-CHAIN-ONE', 'ORDINARY-CHAIN-TWO'):
            if not re.search(r'^\s*\+0000:\s+ud2\s*$', code['before', name], re.M):
                raise ValueError(f'{name} must retain its ordinary-only outer entry path')
            # The initial ordinary fast CALL is followed by control reloads and
            # depth retirement. A cold helper CALL alone cannot satisfy this.
            if not re.search(r'call r11\s*\n[^\n]*mov r11,\[rsp[^\n]*\n'
                             r'[^\n]*mov r11,\[r11\+8\]\s*\n[^\n]*dec dword \[r11\]',
                             code['before', name]):
                raise ValueError(f'{name} missing non-tail ordinary CALL/depth-retirement sequence')
    rows = re.findall(r'^BRIDGE-SAMPLE (\d+) (' + '|'.join(sides) + r') (\d+) (\d+)\s*$', stdout, re.M)
    expected = iterations * (iterations - 1) // 2
    if len(rows) != 2 * samples:
        raise ValueError('incomplete paired samples')
    result = []
    for pair in range(samples):
        values = {}
        for index, side, elapsed, checksum in rows[pair * 2:pair * 2 + 2]:
            if int(index) != pair or side in values or int(checksum) != expected or int(elapsed) <= 0:
                raise ValueError('invalid pair order, elapsed time or checksum')
            values[side] = int(elapsed)
        result.append({'pair': pair, f'{sides[0]}_ns': values[sides[0]], f'{sides[1]}_ns': values[sides[1]],
                       'increment_ns': values[sides[1]] - values[sides[0]]})
    return result


def workload(iterations, samples, reverse, control='leaf'):
    names, _, sides, prefix = comparison(control)
    definitions = Path(__file__).with_name(f'{prefix}-workload.lisp').read_text()
    def listings(phase):
        return '\n'.join(f'(format t "BRIDGE-CODE-BEGIN {phase} {name}~%")\n'
                         f"(disassemble '{name})\n(format t \"BRIDGE-CODE-END~%\")" for name in names)
    def measure(side):
        return f'''(let* ((start (egcl-ext:real-time-nanoseconds))
                  (checksum ({prefix}-{side}-loop {iterations}))
                  (elapsed (- (egcl-ext:real-time-nanoseconds) start)))
             (check-ordinary-bridge-result checksum {iterations})
             (format t "BRIDGE-SAMPLE ~D {side} ~D ~D~%" sample elapsed checksum))'''
    direct, forward = measure(sides[0]), measure(sides[1])
    first, second = (forward, direct) if reverse else (direct, forward)
    return definitions + f'''
(dotimes (warm 3)
  (check-ordinary-bridge-result ({prefix}-{sides[0]}-loop {iterations}) {iterations})
  (check-ordinary-bridge-result ({prefix}-{sides[1]}-loop {iterations}) {iterations}))
{listings('before')}
(dotimes (sample {samples})
  (if (evenp sample) (progn {first} {second}) (progn {second} {first})))
{listings('after')}
'''


def summary(values, iterations):
    ordered = sorted(value / iterations for value in values)
    return {'count': len(ordered), 'min_ns_per_iteration': ordered[0],
            'median_ns_per_iteration': statistics.median(ordered),
            'p95_ns_per_iteration': ordered[(len(ordered) * 95 + 99) // 100 - 1],
            'max_ns_per_iteration': ordered[-1], 'percentile_method': 'nearest rank'}


def prove_warmed_edge(root, manifest_path, output, limiter, env):
    manifest = json.loads(manifest_path.read_text())
    cases = [case for shard in manifest['shards'] for case in shard if case['test'] == WARM_TEST]
    if len(cases) != 1:
        raise ValueError('after artifact must contain exact warmed ordinary edge test')
    case = cases[0]
    executable = root / case['executable']
    if digest(executable) != manifest['artifacts'][case['executable']]:
        raise ValueError('warmed test binary hash mismatch')
    command = [str(limiter), str(executable.resolve()), WARM_TEST, '--exact', '--include-ignored',
               '--nocapture', '--test-threads=1']
    result = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True)
    (output / 'warmed-edge.stdout').write_text(result.stdout)
    (output / 'warmed-edge.stderr').write_text(result.stderr)
    if result.returncode or 'test result: ok. 1 passed; 0 failed' not in result.stdout:
        raise ValueError('matching-artifact warmed-edge integration test failed or did not run')
    return {'command': command, 'sha256': digest(executable), 'test': WARM_TEST,
            'scope': 'Equivalent four-argument bridge takes zero cold handoffs after warming. '
                     'Not a measurement of fallback count in each timed process.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for side in ('before', 'after'):
        parser.add_argument(f'--{side}-root', type=Path, required=True)
        parser.add_argument(f'--{side}-run', required=True, help='immutable CI workflow run ID')
        parser.add_argument(f'--{side}-artifact', required=True, help='immutable CI artifact ID')
    parser.add_argument('--control', choices=('leaf', 'forwarders'), default='leaf',
                        help='leaf regression control or matched one-versus-two forwarding entries')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--build-description', required=True,
                        help='matched CI toolchain/configuration and build log references')
    parser.add_argument('--cpu', type=int, required=True)
    parser.add_argument('--iterations', type=int, default=1000000)
    parser.add_argument('--samples', type=int, default=9)
    parser.add_argument('--trials', type=int, default=5)
    args = parser.parse_args()
    if min(args.iterations, args.samples, args.trials) < 1 or args.iterations > 100000000:
        parser.error('positive counts required; at most100000000 iterations for fixnum checksum')
    if args.cpu not in os.sched_getaffinity(0):
        parser.error('CPU outside permitted affinity')
    args.output.mkdir(parents=True, exist_ok=False)
    output = args.output.resolve()
    checkout = Path(__file__).resolve().parents[2]
    limiter = checkout / 'scripts/egcl-limited.sh'
    env = {key: value for key, value in os.environ.items() if not key.startswith('EGCL_')}
    env.update(EGCL_MEM_MAX='1G', EGCL_TIMEOUT='300', EGCL_FORCE_TIER='t2')
    _, _, sides, _ = comparison(args.control)
    report = {'control': args.control,
              'metric': f'{sides[1]} minus {sides[0]} loop ns/iteration; includes guards, bookkeeping and return, not a single CALL.',
              'review_required': 'Inspect retained ordinary entry guards/argument movement/CF teardown and matched outer dispatch.',
              'build_description': args.build_description,
              'host': {'platform': platform.platform(), 'cpu': args.cpu,
                       'cpuinfo': Path('/proc/cpuinfo').read_text(), 'affinity': sorted(os.sched_getaffinity(0))},
              'iterations': args.iterations, 'binaries': {}, 'trials': []}
    governor = Path(f'/sys/devices/system/cpu/cpu{args.cpu}/cpufreq/scaling_governor')
    report['host']['governor'] = governor.read_text().strip() if governor.exists() else None
    for side in ('before', 'after'):
        root = getattr(args, f'{side}_root').resolve()
        record = artifact(root, root / 'target/ci-tests/manifest.json')
        record.update(run_id=getattr(args, f'{side}_run'), artifact_id=getattr(args, f'{side}_artifact'))
        report['binaries'][side] = record
        (output / f'{side}-ci-manifest.json').write_bytes(Path(record['manifest']).read_bytes())
    match_configuration(report['binaries']['before'], report['binaries']['after'])
    (output / 'manifest.json').write_text(json.dumps(report, indent=2) + '\n')
    report['warmed_edge'] = prove_warmed_edge(args.after_root.resolve(),
        args.after_root / 'target/ci-tests/manifest.json', output, limiter, env)
    for trial in range(args.trials):
        source = output / f'workload-{trial}.lisp'
        source.write_text(workload(args.iterations, args.samples, trial % 2, args.control))
        for side in (('before', 'after') if trial % 2 == 0 else ('after', 'before')):
            stem = f'{side}-{trial}'
            log = output / f'{stem}.t2.log'
            env['EGCL_T2_LOG'] = str(log)
            binary = report['binaries'][side]
            if digest(Path(binary['binary'])) != binary['sha256']:
                raise ValueError('binary changed after validation')
            command = [str(limiter), 'taskset', '-c', str(args.cpu), binary['binary'],
                       '--no-init', '--load', str(source)]
            row = {'side': side, 'trial': trial, 'started_unix': time.time(),
                   'command': command, 'workload_sha256': digest(source), 'load_before': os.getloadavg()}
            report['trials'].append(row)
            try:
                result = subprocess.run(command, cwd=checkout, env=env, capture_output=True, text=True)
                (output / f'{stem}.stdout').write_text(result.stdout)
                (output / f'{stem}.stderr').write_text(result.stderr)
                row.update(returncode=result.returncode, load_after=os.getloadavg())
                if result.returncode:
                    raise ValueError(f'{stem} process failed')
                row['pairs'] = parse_trial(result.stdout, log.read_text(), args.iterations, args.samples, side == 'after', args.control)
            finally:
                (output / 'manifest.json').write_text(json.dumps(report, indent=2) + '\n')
    report['summary'] = {side: {metric: summary([pair[metric] for trial in report['trials']
        if trial['side'] == side for pair in trial['pairs']], args.iterations)
        for metric in (f'{sides[0]}_ns', f'{sides[1]}_ns', 'increment_ns')} for side in ('before', 'after')}
    (output / 'manifest.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report['summary'], indent=2))
    print(report['review_required'])


if __name__ == '__main__':
    main()
