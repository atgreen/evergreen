#!/usr/bin/env python3
"""Run identical Lisp workloads, retain evidence, and render a standalone report."""
import argparse
from datetime import datetime, timezone
import hashlib
import html
import json
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent


def parse_result(output, expected):
    lines = [line for line in output.splitlines() if line.startswith('BENCH ')]
    if len(lines) != 1:
        raise ValueError('Expected exactly one BENCH result')
    match = re.fullmatch(r'BENCH (\d+) (\d+) (-?\d+)', lines[0])
    if not match:
        raise ValueError('Malformed benchmark result')
    ticks, units, value = map(int, match.groups())
    if ticks <= 0 or units <= 0 or value != expected:
        raise ValueError(f'Invalid timing or checksum: {lines[0]}; expected {expected}')
    return ticks / units


def parse_tiers(output, functions):
    records = {'before': {}, 'after': {}}
    expected = {name.upper() for name in functions}
    for line in output.splitlines():
        if not line.startswith('BENCH-TIER '):
            continue
        match = re.fullmatch(r'BENCH-TIER (before|after) ([A-Z0-9-]+) (\d+) (\d+) (\d+) (\d+)', line)
        if not match:
            raise ValueError(f'Malformed tier evidence: {line}')
        phase, name, *numbers = match.groups()
        if name not in expected or name in records[phase]:
            raise ValueError(f'Unexpected or duplicate tier evidence: {line}')
        record = dict(zip(('tier', 'calls', 'back_edges', 'osr_entries'), map(int, numbers)))
        if record['tier'] != 2:
            raise ValueError(f'T2 is required for every timed function: {line}')
        records[phase][name] = record
    if any(set(records[phase]) != expected for phase in records):
        raise ValueError('Missing before/after T2 evidence')
    return records


def tier_checks(case):
    functions = case['hot_functions']
    ready = '(and ' + ' '.join(f"(eql 2 (torcl-ext:function-tier '{name}))" for name in functions) + ')'
    prints = '\n'.join(
        f'(format t "BENCH-TIER ~a {name.upper()} ~d ~d ~d ~d~%" phase '
        f"(torcl-ext:function-tier '{name}) (torcl-ext:function-invoke-count '{name}) "
        f"(torcl-ext:function-back-edge-count '{name}) (torcl-ext:function-osr-count '{name}))"
        for name in functions)
    return f"""
#+torcl
(defun bench-report-tiers (phase)
  {prints})
#+torcl
(progn
  (dotimes (attempt 1000)
    (when {ready} (return))
    (sleep 0.01))
  (unless {ready} (error "Timed functions did not reach T2"))
  (bench-report-tiers "before"))
"""


def summarize(samples):
    q1, _, q3 = statistics.quantiles(samples, method='inclusive')
    return dict(samples_seconds=samples, median_seconds=statistics.median(samples),
                iqr_seconds=q3-q1, min_seconds=min(samples), max_seconds=max(samples))


def comparison(torcl, sbcl):
    if torcl == sbcl:
        return 'Tie', 1
    return ('TorCL', sbcl / torcl) if torcl < sbcl else ('SBCL', torcl / sbcl)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def command_output(command):
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def lisp_string(value):
    return '"' + str(value).replace('\\', '\\\\').replace('"', '\\"') + '"'


def render(data):
    esc = html.escape
    cards = []
    for case in data['benchmarks']:
        a, b = case['results']['TorCL'], case['results']['SBCL']
        winner, ratio = comparison(a['median_seconds'], b['median_seconds'])
        maximum = max(a['median_seconds'], b['median_seconds'])
        bars = ''.join(f'<div class="bar-row"><b>{name}</b><div class="track"><div class="bar {name.lower()}" style="width:{100*s["median_seconds"]/maximum:.2f}%"></div></div><span>{1000*s["median_seconds"]:.3f} ms</span></div>' for name, s in [('TorCL', a), ('SBCL', b)])
        rows = ''.join(f'<tr><th>{name}</th><td>{1000*s["median_seconds"]:.3f}</td><td>{1000*s["iqr_seconds"]:.3f}</td><td>{esc(", ".join(f"{x*1000:.3f}" for x in s["samples_seconds"]))}</td></tr>' for name, s in [('TorCL', a), ('SBCL', b)])
        tier_html = ''
        if case.get('tier_evidence'):
            deopts = [record['deoptimizations'] for record in case['tier_evidence']]
            tier_html = f'<p class="muted">T2 verified before and after every TorCL sample: {esc(", ".join(case["hot_functions"]))}. Timed deoptimizations per sample: {esc(str(deopts))}.</p>'
        verdict = 'Equal medians' if winner == 'Tie' else f'{winner} {ratio:.2f}× faster'
        cards.append(f'<article><div class="eyebrow">{esc(case["category"])}</div><h2>{esc(case["title"])}</h2><p>{esc(case["description"])}</p><strong class="verdict">{verdict}</strong>{bars}{tier_html}<p class="muted">Lower is better · identical workload · checksum {case["expected"]:,} verified in every run</p><details><summary>Samples and spread</summary><div class="table-wrap"><table><thead><tr><th>Runtime</th><th>Median ms</th><th>IQR ms</th><th>All samples, ms</th></tr></thead><tbody>{rows}</tbody></table></div></details></article>')
    meta = data['metadata']
    return '''<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>TorCL · Performance lab</title>
<style>
:root{color-scheme:dark;--bg:#0b1220;--panel:#141f31;--muted:#adbbce;--mint:#68e0c2;--blue:#829eff}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:#edf3fa;font:16px/1.65 system-ui,sans-serif}main{max-width:1050px;margin:auto;padding:60px 24px}header{max-width:800px;margin-bottom:40px}h1{font-size:clamp(38px,6vw,66px);line-height:1.1;letter-spacing:-2px;margin:14px 0 24px}h2{font-size:26px;margin:8px 0}h3{margin-bottom:8px}.eyebrow{color:var(--mint);font-size:12px;letter-spacing:2px;text-transform:uppercase;font-weight:700}article,.method{background:var(--panel);border:1px solid #28364b;border-radius:18px;padding:28px;margin:24px 0}.verdict{display:block;font-size:27px;margin:24px 0;color:var(--mint)}.muted,footer{color:var(--muted);font-size:14px}.bar-row{display:grid;grid-template-columns:60px 1fr 115px;gap:16px;align-items:center;margin:16px 0}.bar-row span{text-align:right;font-variant-numeric:tabular-nums}.track{background:#202e44;border-radius:6px;overflow:hidden}.bar{height:25px;min-width:2px}.torcl{background:var(--mint)}.sbcl{background:var(--blue)}a{color:var(--mint)}summary{cursor:pointer}table{border-collapse:collapse;width:100%;font-size:14px}td,th{text-align:left;padding:12px;border-bottom:1px solid #33415a}.table-wrap{overflow:auto}pre{white-space:pre-wrap;overflow-wrap:anywhere;font-size:12px}code{font-size:13px}footer{margin-top:36px}@media(max-width:550px){main{padding:30px 14px}article,.method{padding:20px}.bar-row{grid-template-columns:50px 1fr 90px;gap:8px;font-size:13px}}
</style><main><header><div class="eyebrow">TorCL / Performance lab</div><h1>Common Lisp.<br>Measured in motion.</h1><p>Recursive calls and real library kernels, measured against SBCL. Same Lisp, same inputs, checked results. These are local measurements of specific workloads, not a claim of universal performance.</p></header>''' + ''.join(cards) + f'''
<section class="method"><h2>How to read these results</h2><p>{data['samples']} independent processes per runtime and workload. Each process performs correctness checks, 10,000 short training calls to the batch driver, and three untimed full-workload warmups before one timed sample. Every TorCL sample requires tier 2 for the kernel and batch driver before and after timing; missing or lower tiers fail the run. The tier gate waits up to 10 seconds for background compilation to publish. SBCL receives the same training calls and warmup workloads. TorCL and SBCL run sequentially in alternating order, pinned to CPU {meta['cpu']}. Timing uses GET-INTERNAL-REAL-TIME inside Lisp and excludes startup, source loading, compilation before execution, training, and warmup. JIT work and GC occurring during the measured region remain included.</p><p>Bars show median elapsed time; the table exposes every sample and its interquartile range. Speedup is the ratio of medians. This is an exploratory workstation report, not a controlled CI regression gate or a statistical significance claim. CPU frequency and background load are not controlled. GC pause telemetry is not collected.</p><p>TorCL uses its release build and automatic tiering with inherited TORCL_* tuning cleared. SBCL compiles the same source with SPEED 3 / SAFETY 1 (individual upstream declarations are preserved). TorCL-specific checks observe tiers outside timing; no implementation-specific benchmark fast paths are added. Tier 2 does not imply that every operation avoids runtime calls.</p><h3>Scope of the Ironclad comparison</h3><p>Extracted source from Ironclad, with its declarations preserved. These rows measure the named kernels and input sizes only; they do not measure the full Ironclad API, encryption throughput, or cryptographic security. Small-integer workloads do not represent production public-key sizes.</p><p><a href="results.json">Raw JSON</a> · <a href="../README.md">Reproduction and source provenance</a></p><details><summary>Machine, versions and provenance</summary><pre>{esc(json.dumps(meta, indent=2))}</pre></details></section><footer>Generated {esc(data['generated_at'])}. Source and binary SHA-256 hashes are retained in the JSON artifact.</footer></main></html>'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--torcl', default=str(ROOT/'target/x86_64-unknown-linux-musl/release/torcl'))
    parser.add_argument('--sbcl', default=shutil.which('sbcl'))
    parser.add_argument('--samples', type=int, default=7)
    parser.add_argument('--cpu', type=int, default=min(os.sched_getaffinity(0)))
    parser.add_argument('--output', type=Path, default=HERE/'results')
    parser.add_argument('--case', action='append', help='Select case IDs (repeatable)')
    args = parser.parse_args()
    if args.samples < 5:
        parser.error('At least five samples are required')
    if args.cpu not in os.sched_getaffinity(0):
        parser.error('CPU is outside the allowed affinity set')
    binaries = {'TorCL': Path(args.torcl).resolve(), 'SBCL': Path(args.sbcl or '').resolve()}
    for binary in binaries.values():
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f'Executable not found: {binary}')
    cases = json.loads((HERE/'cases.json').read_text())
    if args.case:
        if set(args.case) - {c['id'] for c in cases}:
            parser.error('Unknown case ID')
        cases = [c for c in cases if c['id'] in args.case]
    out = args.output.resolve()
    if out.exists() and any(out.iterdir()):
        parser.error('Output directory must be empty; preserve previous measurements')
    out.mkdir(parents=True, exist_ok=True)
    env = {k:v for k,v in os.environ.items() if not k.startswith('TORCL_')}
    env.update(TORCL_MEM_MAX=os.environ.get('TORCL_MEM_MAX', '4G'), TORCL_TIMEOUT='300')
    cpus = Path('/proc/cpuinfo').read_text()
    model = next((line.split(':', 1)[1].strip() for line in cpus.splitlines() if line.startswith('model name')), platform.processor())
    data = {'generated_at': datetime.now(timezone.utc).isoformat(), 'samples':args.samples,
            'metadata': {'commit':command_output(['git','rev-parse','HEAD']),
                         'working_tree':command_output(['git','status','--short']),
                         'platform':platform.platform(), 'cpu':args.cpu, 'cpu_model':model,
                         'load_average':os.getloadavg(), 'memory_cap':env['TORCL_MEM_MAX'],
                         'binaries':{name:{'path':str(path),'sha256':sha256(path)} for name,path in binaries.items()},
                         'sbcl_version':command_output([str(binaries['SBCL']), '--version']),
                         'sources': {p.name:sha256(p) for p in sorted(HERE.glob('*.lisp'))},
                         'runner_sha256':sha256(__file__), 'cases_sha256':sha256(HERE/'cases.json')},
            'benchmarks':[]}
    for case in cases:
        measurements = {'TorCL':[], 'SBCL':[]}
        tier_evidence = []
        source = '(declaim (optimize (speed 3) (safety 1) (debug 0)))\n' + (HERE/case['source']).read_text()
        source += f'''\n(bench-validate)
(bench-train)
(dotimes (warmup 3) (unless (= (bench-workload) {case['expected']}) (error "Warmup checksum failed")))
{tier_checks(case)}
(let* ((deopts-before #+torcl (torcl-ext:deopt-count) #-torcl 0)
       (start (get-internal-real-time)) (value (bench-workload)) (end (get-internal-real-time))
       (deopts-after #+torcl (torcl-ext:deopt-count) #-torcl 0))
  (format t "BENCH ~d ~d ~d~%" (- end start) internal-time-units-per-second value)
  (format t "BENCH-DEOPTS ~d~%" (- deopts-after deopts-before)))
#+torcl (bench-report-tiers "after")
'''
        script = out/(case['id']+'.lisp')
        script.write_text(source)
        for sample in range(args.samples):
            order = ['TorCL','SBCL'] if sample % 2 == 0 else ['SBCL','TorCL']
            for name in order:
                with tempfile.TemporaryDirectory(prefix='torcl-bench-') as temp:
                    if name == 'TorCL':
                        invocation = [str(binaries[name]), '--no-init', '--load', str(script)]
                    else:
                        fasl = Path(temp)/'case.fasl'
                        form = f'(multiple-value-bind (file warnings failure) (compile-file {lisp_string(script)} :output-file {lisp_string(fasl)}) (declare (ignore warnings)) (when failure (error "Compilation failed")) (load file))'
                        invocation = [str(binaries[name]), '--noinform','--no-sysinit','--no-userinit','--non-interactive','--eval',form]
                    command = [str(ROOT/'scripts/torcl-limited.sh'), 'taskset','-c',str(args.cpu),*invocation]
                    result = subprocess.run(command, env=env, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
                    (out/f'{case["id"]}-{name.lower()}-{sample+1}.log').write_text(result.stdout)
                    if result.returncode:
                        raise RuntimeError(f'{case["id"]} {name} exited {result.returncode}; see {out}')
                    seconds = parse_result(result.stdout, case['expected'])
                    if name == 'TorCL':
                        evidence = parse_tiers(result.stdout, case['hot_functions'])
                        deopts = re.findall(r'^BENCH-DEOPTS (\d+)$', result.stdout, re.MULTILINE)
                        if len(deopts) != 1:
                            raise ValueError('Missing deoptimization evidence')
                        evidence['deoptimizations'] = int(deopts[0])
                        tier_evidence.append(evidence)
                    measurements[name].append(seconds)
                    print(f'{case["id"]} {name} {sample+1}/{args.samples}: {seconds:.6f}s', flush=True)
        data['benchmarks'].append({**case, 'tier_evidence':tier_evidence, 'results':{name:summarize(values) for name,values in measurements.items()}})
    (out/'results.json').write_text(json.dumps(data, indent=2)+'\n')
    (out/'index.html').write_text(render(data))
    print(f'Report: {out / "index.html"}')


if __name__ == '__main__':
    main()
