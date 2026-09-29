"""Protect the benchmark report against misleading timing and correctness claims."""
import unittest
import run


class ResultTests(unittest.TestCase):
    def test_result_requires_correct_output_and_positive_time(self):
        self.assertEqual(run.parse_result('noise\nBENCH 100 1000 832040\n', 832040), 0.1)
        for output in ['BENCH 100 1000 3', 'BENCH 0 1000 832040',
                       'BENCH -1 1000 832040', 'BENCH 1 0 832040',
                       'no result', 'BENCH 1 1000 832040\nBENCH 2 1000 832040']:
            with self.subTest(output=output), self.assertRaises(ValueError):
                run.parse_result(output, 832040)

    def test_summary_uses_median_and_preserves_samples(self):
        result = run.summarize([1, 2, 3, 4, 100])
        self.assertEqual(result['median_seconds'], 3)
        self.assertEqual(result['samples_seconds'], [1, 2, 3, 4, 100])
        self.assertGreater(result['iqr_seconds'], 0)

    def test_report_never_turns_a_loss_into_a_win(self):
        for torcl, sbcl, winner in [(1, 4, 'TorCL'), (4, 1, 'SBCL')]:
            self.assertEqual(run.comparison(torcl, sbcl), (winner, 4))

    def test_no_winner_for_a_tie(self):
        self.assertEqual(run.comparison(1, 1), ('Tie', 1))

    def test_parse_perf_instruction_counts_and_ignores_uncounted_rows(self):
        stderr = ('100,,cpu_core/instructions/u,1,2.0,,\n'
                   '<not counted>,,cpu_atom/instructions/u,0,0.0,,\n')
        self.assertEqual(run.parse_instructions(stderr), 100)

class ReportTests(unittest.TestCase):
    def test_report_escapes_metadata_and_shows_the_actual_winner(self):
        import json
        from pathlib import Path
        case = json.loads((Path(__file__).parent/'cases.json').read_text())[0]
        case['results'] = {'TorCL': run.summarize([4]*5), 'SBCL': run.summarize([1]*5)}
        data = {'benchmarks':[case], 'samples':5, 'generated_at':'today',
                'metadata':{'cpu':0, 'host':'<script>alert(1)</script>'}}
        page = run.render(data)
        self.assertIn('SBCL 4.00× faster', page)
        self.assertNotIn('TorCL 4.00× faster', page)
        self.assertNotIn('<script>', page)
        self.assertIn('&lt;script&gt;', page)
        self.assertIn('not a', page)

    def test_baseline_manifest_accepts_nested_checked_report(self):
        nested = {
            'metadata': {
                'commit': 'current',
                'baseline': {
                    'commit': 'baseline',
                    'binary_sha256': 'abc',
                    'benchmarks': {
                        'fibonacci': {'median_seconds': 0.112, 'instructions': {'median': 7}}
                    },
                },
            },
            'benchmarks': [],
        }
        result = run.baseline_manifest(nested)
        self.assertEqual(result['commit'], 'baseline')
        self.assertEqual(result['binary_sha256'], 'abc')
        self.assertIn('fibonacci', result['benchmarks'])

    def test_baseline_manifest_accepts_raw_results(self):
        raw = {
            'metadata': {
                'commit': 'baseline',
                'binaries': {'TorCL': {'sha256': 'abc'}},
            },
            'benchmarks': [{
                'id': 'fibonacci',
                'results': {'TorCL': {'median_seconds': 0.112}},
            }],
        }
        result = run.baseline_manifest(raw)
        self.assertEqual(result['commit'], 'baseline')
        self.assertEqual(result['benchmarks']['fibonacci']['median_seconds'], 0.112)


class TierTests(unittest.TestCase):
    def test_require_before_and_after_t2_for_each_hot_function(self):
        valid = '\n'.join(f'BENCH-TIER {phase} {name} 2 10000 100 0'
                          for phase in ('before', 'after')
                          for name in ('FIBONACCI', 'BENCH-WORKLOAD'))
        tiers = run.parse_tiers(valid, ['fibonacci', 'bench-workload'])
        self.assertEqual(tiers['before']['FIBONACCI']['tier'], 2)
        self.assertEqual(tiers['after']['BENCH-WORKLOAD']['calls'], 10000)
        for invalid in (valid.replace(' 2 10000', ' 1 10000', 1),
                        valid.replace('BENCH-TIER after FIBONACCI', 'OTHER'),
                        valid + '\nBENCH-TIER before FIBONACCI 2 10000 100 0'):
            with self.subTest(output=invalid), self.assertRaises(ValueError):
                run.parse_tiers(invalid, ['fibonacci', 'bench-workload'])


if __name__ == '__main__':
    unittest.main()
