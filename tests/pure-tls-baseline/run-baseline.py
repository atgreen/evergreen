#!/usr/bin/env python3
"""Run the pinned pure-tls main test entrypoint, preserving failures and timings."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
import platform
from pathlib import Path
import re
import shutil
import subprocess
import time


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    here = Path(__file__).resolve().parent
    repo = here.parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('label', help='unique output label (letters, digits, dot, dash, underscore)')
    parser.add_argument('--work-dir', type=Path, default=here,
                        help='directory containing installed ocicl/ and runtime/ (default: harness directory)')
    parser.add_argument('--egcl', default=os.environ.get('EGCL_BIN'), help='release executable; or set EGCL_BIN')
    parser.add_argument('--runtime-revision', required=True, help='exact revision used to build EGCL_BIN')
    parser.add_argument('--memory', default=os.environ.get('EGCL_MEM_MAX', '8G'))
    parser.add_argument('--timeout', type=int, default=int(os.environ.get('EGCL_TIMEOUT', '3600')))
    parser.add_argument('--closer-package-source', type=Path,
                        help='optional narrower cached-load workaround; not the default baseline')
    args = parser.parse_args()
    if not re.fullmatch(r'[A-Za-z0-9_.-]+', args.label) or args.label in ('.', '..'):
        parser.error('label must be a simple filename component')
    if not args.egcl:
        parser.error('--egcl or EGCL_BIN is required')
    binary = Path(shutil.which(args.egcl) or args.egcl).resolve()
    work = args.work_dir.resolve()
    lock = work / 'ocicl.csv'
    runtime = work / 'runtime/ocicl-runtime.lisp'
    if not binary.is_file() or not runtime.is_file() or not lock.is_file():
        parser.error('executable, ocicl.csv and runtime/ocicl-runtime.lisp must exist; see README')
    if sha256(lock) != sha256(here / 'ocicl.csv'):
        parser.error('work directory lock differs from the committed frozen baseline')
    logs = work / 'logs'
    logs.mkdir(exist_ok=True)
    prefix = logs / args.label
    if any(Path(str(prefix) + '.' + ext).exists() for ext in ('log', 'json', 'time')):
        parser.error('output label already exists; choose another label')
    cache = work / 'cache'
    tmp = work / 'tmp'
    cache.mkdir(exist_ok=True)
    tmp.mkdir(exist_ok=True)
    env = dict(os.environ, OCICL_LOCAL_ONLY='1', TMPDIR=str(tmp),
               EGCL_MEM_MAX=args.memory, EGCL_TIMEOUT=str(args.timeout),
               EGCL_BASELINE_CACHE=str(cache) + '/', EGCL_BASELINE_RUNTIME=str(runtime))
    env.pop('EGCL_BASELINE_SOURCE_ASD', None)
    env.pop('EGCL_BASELINE_CLOSER_SOURCE', None)
    if args.closer_package_source:
        env['EGCL_BASELINE_CLOSER_SOURCE'] = str(args.closer_package_source.resolve(strict=True))
    command = [str(repo / 'scripts/egcl-limited.sh'), '/usr/bin/time', '-v', '-o',
               str(Path(str(prefix) + '.time')), str(binary), '--no-init', '--load', str(here / 'run-suite.lisp')]
    inputs = {'executable_sha256': binary, 'manifest_sha256': lock,
              'harness_sha256': here / 'run-suite.lisp',
              'runner_sha256': Path(__file__), 'ocicl_runtime_sha256': runtime}
    fingerprints = {name: sha256(path) for name, path in inputs.items()}
    started_utc = datetime.now(timezone.utc).isoformat()
    started = time.monotonic()
    before = os.getloadavg()
    with Path(str(prefix) + '.log').open('w') as output:
        process = subprocess.run(command, cwd=work, env=env, stdout=output, stderr=subprocess.STDOUT)
    lines = Path(str(prefix) + '.log').read_text(errors='replace').splitlines()
    completed_suites = [line.split()[1] for line in lines
                        if line.startswith('BASELINE-SUITE-SECONDS ')]
    completed = (len(completed_suites) == 13 and len(set(completed_suites)) == 13
                 and any(line.startswith('BASELINE-TEST-SECONDS ') for line in lines)
                 and any(line in ('BASELINE-RESULT T', 'BASELINE-RESULT NIL') for line in lines))
    passed = process.returncode == 0 and completed and 'BASELINE-RESULT T' in lines
    changed = [name for name, path in inputs.items() if sha256(path) != fingerprints[name]]
    result = dict(runtime_revision=args.runtime_revision, label=args.label, returncode=process.returncode,
                  started_utc=started_utc, platform=platform.platform(), cpu_count=os.cpu_count(),
                  inputs_changed_during_run=changed,
                  entrypoint_completed=completed, completed_suites=completed_suites,
                  suite_passed=passed,
                  wall_seconds=time.monotonic() - started, load_average_before=before,
                  load_average_after=os.getloadavg(), command=command,
                  **fingerprints,
                  repository_revision=subprocess.check_output(['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip(),
                  environment_overrides={k: v for k, v in env.items() if k.startswith(('EGCL_', 'OCICL_')) or k == 'TMPDIR'},
                  limits=dict(memory=args.memory, swap='0', timeout_seconds=args.timeout),
                  markers=[line for line in lines if line.startswith('BASELINE-')],
                  fiveam_summaries=[line.strip() for line in lines if re.search(r'Did \d+ checks|Pass:|Skip:|Fail:', line)])
    Path(str(prefix) + '.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))
    if changed:
        return 1
    if process.returncode == 0 and not passed:
        return 1
    return process.returncode if process.returncode >= 0 else 128 - process.returncode


if __name__ == '__main__':
    raise SystemExit(main())
