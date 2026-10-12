#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
"""Retain public packed-double kernel timing, full-entry, and consing evidence."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import time
from packed_provenance import github_metadata, source_identity, verify_ci_receipt

NAME = 'PUBLIC-PACKED-BENCH'


def checksum(n):
    whole, remainder = divmod(n, 32)
    return 3 * (whole * 528 + remainder * (remainder + 1) // 2)


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def parse(stdout, log, n, samples, implementation, sbcl_mode='same-source'):
    listings = re.findall(r'PACKED-CODE-BEGIN (before|after)\s*\n(.*?)PACKED-CODE-END', stdout, re.S)
    if len(listings) != 2 or [phase for phase, _ in listings] != ['before', 'after']:
        raise ValueError('missing named pre/post code listings')
    code = listings[0][1]
    if NAME not in code or code != listings[1][1]:
        raise ValueError('missing or changed kernel code')
    if implementation == 'sbcl' and sbcl_mode == 'typed':
        if not all(re.search(r'\b' + opcode + r'\b', code, re.I)
                   for opcode in ('movsd', 'mulsd', 'addsd')):
            raise ValueError('typed SBCL reference lacks native double load/arithmetic/store evidence')
    if implementation == 'egcl':
        if '; T2 —' not in code or 'Mapped native transfer ABI' not in code:
            raise ValueError('kernel lacks named mapped T2 code')
        if not all(re.search(r'\b' + opcode + r'\b', code) for opcode in ('mulsd', 'addsd')):
            raise ValueError('kernel lacks native double multiply/add instructions')
        # Current PackedF64Ref/Set templates use SCRATCH=rdx and DATA_OFFSET=16.
        # This is an opcode gate, not a substitute for reviewing data flow.
        if not (re.search(r'\bmov\s+r\w+,\[rdx\+10h\]', code)
                and re.search(r'\bmov\s+\[rdx\+10h\],r\w+', code)):
            raise ValueError('kernel lacks packed element load/store template')
        named = '\n'.join(line for line in log.splitlines()
                          if re.match(r'\[T2\] ' + NAME + r':', line))
        emitted = list(re.finditer(r'mapped T2 emitted;[^\n]*raw_f64_values=(\d+)', named))
        installed = list(re.finditer(r'installed with mapped T2 ABI; bytes=(\d+), entry=(0x[0-9a-f]+),[^\n]*osr_entries=0\b', named))
        sizes = re.findall(r'; (\d+) bytes of x86-64:', code)
        if (len(emitted) != 1 or len(installed) != 1
                or int(emitted[0][1]) < 4 or emitted[0].start() >= installed[0].start()
                or not sizes or any(size != installed[0][1] for size in sizes)):
            raise ValueError('kernel requires one raw emission followed by one matching-size full-entry installation')
        if any(re.search(r'osr_entries=[1-9]|deopt|invalidat|recompil', line, re.I)
               for line in log.splitlines() if NAME in line.upper()):
            raise ValueError('kernel changed or claims unsupported raw OSR entry')
    rows = re.findall(r'^PACKED-SAMPLE (\d+) (\d+) (\d+)\s*$', stdout, re.M)
    if len(rows) != samples:
        raise ValueError('missing timing samples')
    result = []
    for index, (sample, elapsed, value) in enumerate(rows):
        if int(sample) != index or int(elapsed) <= 0 or int(value) != checksum(n):
            raise ValueError('wrong sample order, clock resolution, or checksum')
        result.append({'sample': index, 'elapsed_ns': int(elapsed), 'checksum': int(value)})
    probes = re.findall(r'PACKED-ALLOC-BEGIN (\d+)\s*\n(.*?)PACKED-ALLOC-END', stdout, re.S)
    if [int(size) for size, _ in probes] != [0, n, 2 * n]:
        raise ValueError('missing paired allocation probes')
    allocations = []
    for size, text in probes:
        counts = re.findall(r'([\d,]+) bytes consed', text)
        if len(counts) != 1:
            raise ValueError('TIME must report its real Lisp consing counter')
        allocations.append({'elements': int(size), 'bytes_cons': int(counts[0].replace(',', ''))})
    baseline = allocations[0]['bytes_cons']
    return {'samples': result, 'allocation_probes': allocations,
            'incremental_lisp_bytes': [row['bytes_cons'] - baseline for row in allocations[1:]]}


def workload(implementation, n, samples, passes, sbcl_mode='typed'):
    if sbcl_mode not in ('typed', 'same-source'):
        raise ValueError('unknown SBCL reference mode')
    definitions = Path(__file__).with_name('packed-kernel-workload.lisp').read_text()
    if implementation == 'sbcl' and sbcl_mode == 'typed':
        header = '(defun public-packed-bench (src dst n scale)\n  (dotimes (i n)\n'
        if definitions.count(header) != 1:
            raise ValueError('kernel source changed; review typed reference insertion')
        definitions = definitions.replace(header, '''(defun public-packed-bench (src dst n scale)
  (declare (optimize (speed 3) (safety 1) (debug 0))
           (type (simple-array double-float (*)) src dst)
           (type (integer 0 20000000) n)
           (type double-float scale))
  (dotimes (i n)
    (declare (type fixnum i))
''')
    if implementation == 'egcl':
        clock = '(egcl-ext:real-time-nanoseconds)'
        elapsed = '(- (egcl-ext:real-time-nanoseconds) start)'
    else:
        clock = '(get-internal-real-time)'
        elapsed = '(truncate (* (- (get-internal-real-time) start) 1000000000) internal-time-units-per-second)'
    def listing(phase):
        return f'(format t "PACKED-CODE-BEGIN {phase}~%") (disassemble \'public-packed-bench) (format t "PACKED-CODE-END~%")'
    probes = '\n'.join(f'''(format t "PACKED-ALLOC-BEGIN {size}~%")
      (time (public-packed-bench src dst {size} scale))
      (format t "PACKED-ALLOC-END~%")''' for size in [0, n, 2 * n])
    return definitions + f'''
(let* ((src (packed-bench-source {2 * n}))
       (dst (make-array {2 * n} :element-type 'double-float :initial-element 0d0))
       (scale 2d0)
       (*trace-output* *standard-output*))
  ;; Repeated full invocations warm the named entry. Do not infer raw OSR.
  (dotimes (warm 1024) (public-packed-bench src dst {min(n, 32)} scale))
  {listing('before')}
  (dotimes (sample {samples})
    (let ((start {clock}))
      (dotimes (pass {passes}) (public-packed-bench src dst {n} scale))
      (let ((elapsed {elapsed}))
        (format t "PACKED-SAMPLE ~D ~D ~D~%" sample elapsed (packed-bench-check dst {n})))))
  ;; Counter probes are separate from timing and exclude checksum validation.
  {probes}
  (assert (= (packed-bench-check dst {2 * n}) {checksum(2 * n)}))
  {listing('after')})
'''


def distribution(values):
    ordered = sorted(values)
    return {'count': len(ordered), 'min': ordered[0], 'median': statistics.median(ordered),
            'p95': ordered[(len(ordered) * 95 + 99) // 100 - 1], 'max': ordered[-1],
            'percentile_method': 'nearest rank'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--egcl-root', type=Path, help='unpacked immutable release CI artifact')
    parser.add_argument('--egcl-run', help='immutable CI workflow run ID')
    parser.add_argument('--egcl-artifact', help='immutable CI artifact ID')
    parser.add_argument('--egcl-archive', type=Path, help='original downloaded GitHub artifact ZIP')
    parser.add_argument('--sbcl', default='sbcl')
    parser.add_argument('--sbcl-mode', choices=('typed', 'same-source'), default='typed',
                        help='explicit optimized reference (default) or undeclared source control')
    parser.add_argument('--sbcl-only', action='store_true', help='portable smoke/tool validation; no EGCL claim')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cpu', type=int, required=True)
    parser.add_argument('--elements', type=int, default=262144)
    parser.add_argument('--passes', type=int, default=16)
    parser.add_argument('--samples', type=int, default=9)
    parser.add_argument('--trials', type=int, default=5)
    args = parser.parse_args()
    if min(args.elements, args.passes, args.samples, args.trials) < 1 or args.elements > 10000000:
        parser.error('positive counts and at most10000000 elements required')
    if args.cpu not in os.sched_getaffinity(0) or (not args.sbcl_only and not (args.egcl_root and args.egcl_run and args.egcl_artifact and args.egcl_archive)):
        parser.error('permitted CPU and EGCL artifact required unless --sbcl-only')
    args.output.mkdir(parents=True, exist_ok=False)
    output = args.output.resolve()
    checkout = Path(__file__).resolve().parents[2]
    limiter = checkout / 'scripts/egcl-limited.sh'
    receipt = None
    if not args.sbcl_only:
        run = github_metadata('runs', args.egcl_run)
        artifact = github_metadata('artifacts', args.egcl_artifact)
        receipt = verify_ci_receipt(root=args.egcl_root, archive=args.egcl_archive,
            run=run, artifact=artifact, run_id=args.egcl_run, artifact_id=args.egcl_artifact,
            source=source_identity(checkout, run['head_sha']))
        (output / 'egcl-ci-receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    env = {key: value for key, value in os.environ.items() if not key.startswith('EGCL_')}
    env.update(EGCL_MEM_MAX='1G', EGCL_TIMEOUT='300', EGCL_FORCE_TIER='t2')
    sbcl = Path(shutil.which(args.sbcl) or args.sbcl).resolve()
    sbcl_info = subprocess.check_output([str(limiter), str(sbcl), '--noinform', '--non-interactive', '--no-sysinit',
        '--no-userinit', '--eval', '(format t "~A~%~A~%~D~%" (lisp-implementation-version) sb-ext:*core-pathname* internal-time-units-per-second)'], env=env, text=True).splitlines()
    core = Path(sbcl_info[-2])
    binaries = {'sbcl': {'binary': str(sbcl), 'sha256': digest(sbcl), 'version': sbcl_info[-3],
                         'clock_units_per_second': int(sbcl_info[-1]),
                         'core': str(core), 'core_sha256': digest(core)}}
    if not args.sbcl_only:
        root = args.egcl_root.resolve()
        manifest_path = root / 'target/ci-tests/manifest.json'
        manifest = json.loads(manifest_path.read_text())
        name = f"target/{manifest['target']}/release/egcl"
        binary = root / name
        if (manifest['profile'] != 'release' or manifest['target'] != 'x86_64-unknown-linux-musl'
                or digest(binary) != manifest['artifacts'].get(name)):
            raise ValueError('EGCL binary must match immutable release manifest')
        binaries['egcl'] = {'binary': str(binary), 'sha256': digest(binary),
                            'verified_ci_receipt': receipt,
                            'manifest_sha256': digest(manifest_path), 'revision': manifest['revision'],
                            'run_id': args.egcl_run, 'artifact_id': args.egcl_artifact,
                            'clock_units_per_second': 1000000000}
        (output / 'egcl-ci-manifest.json').write_bytes(manifest_path.read_bytes())
    report = {'metric': 'ns per processed element including fixed invocation/harness overhead',
              'allocation_scope': 'TIME Lisp bytes consed for one full call at0,n,2n; subtract0 only to inspect size dependence, not Rust allocation or proof by subtraction alone',
              'path_scope': 'named EGCL raw-F64 full entry; no OSR claim; SBCL reference mode explicitly recorded',
              'sbcl_reference': {'mode': args.sbcl_mode,
                  'declarations': ('simple double-float arrays; n integer0..20000000; fixnum i; double-float scale'
                                   if args.sbcl_mode == 'typed' else 'none'),
                  'optimization': {'speed': 3, 'safety': 1, 'debug': 0} if args.sbcl_mode == 'typed' else 'implementation defaults'},
              'elements': args.elements, 'passes': args.passes, 'binaries': binaries, 'trials': [],
              'host': {'platform': platform.platform(), 'cpu': args.cpu,
                       'affinity': sorted(os.sched_getaffinity(0)), 'cpuinfo': Path('/proc/cpuinfo').read_text()}}
    governor = Path(f'/sys/devices/system/cpu/cpu{args.cpu}/cpufreq/scaling_governor')
    report['host']['governor'] = governor.read_text().strip() if governor.exists() else None
    for trial in range(args.trials):
        order = ['sbcl'] if args.sbcl_only else (['egcl', 'sbcl'] if trial % 2 == 0 else ['sbcl', 'egcl'])
        for implementation in order:
            stem = f'{implementation}-{trial}'
            source = output / f'{stem}.lisp'
            source.write_text(workload(implementation, args.elements, args.samples, args.passes, args.sbcl_mode))
            log = output / f'{stem}.t2.log'
            env['EGCL_T2_LOG'] = str(log)
            binary = binaries[implementation]
            if digest(binary['binary']) != binary['sha256'] or digest(core) != binaries['sbcl']['core_sha256']:
                raise ValueError('binary or SBCL core changed after validation')
            flags = ['--no-init'] if implementation == 'egcl' else ['--noinform', '--non-interactive', '--no-sysinit', '--no-userinit']
            command = [str(limiter), 'taskset', '-c', str(args.cpu), binary['binary'], *flags, '--load', str(source)]
            row = {'implementation': implementation, 'trial': trial, 'command': command,
                   'started_unix': time.time(), 'load_before': os.getloadavg(), 'source_sha256': digest(source)}
            report['trials'].append(row)
            try:
                result = subprocess.run(command, cwd=checkout, env=env, capture_output=True, text=True)
                (output / f'{stem}.stdout').write_text(result.stdout)
                (output / f'{stem}.stderr').write_text(result.stderr)
                row.update(returncode=result.returncode, load_after=os.getloadavg())
                if result.returncode:
                    raise ValueError(f'{stem} failed')
                row.update(parse(result.stdout, log.read_text() if log.exists() else '',
                                 args.elements, args.samples, implementation, args.sbcl_mode))
            finally:
                (output / 'manifest.json').write_text(json.dumps(report, indent=2) + '\n')
    report['summary_ns_per_element'] = {implementation: distribution([
        sample['elapsed_ns'] / (args.elements * args.passes)
        for row in report['trials'] if row['implementation'] == implementation for sample in row['samples']])
        for implementation in binaries}
    (output / 'manifest.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report['summary_ns_per_element'], indent=2))
    print('Allocation counter probes and named path evidence retained; no automatic speedup/allocation-free claim.')


if __name__ == '__main__':
    main()
