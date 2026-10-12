#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
import unittest
import packed_kernel as bench


class EvidenceTests(unittest.TestCase):
    def output(self):
        code = '; PUBLIC-PACKED-BENCH\n; T2 — code\n; Mapped native transfer ABI\n  +0000: mulsd xmm0,xmm1\n  +0004: addsd xmm0,xmm1\n'
        return (f'PACKED-CODE-BEGIN before\n{code}PACKED-CODE-END\n'
                'PACKED-SAMPLE 0 1000000 1584\nPACKED-SAMPLE 1 1200000 1584\n'
                'PACKED-ALLOC-BEGIN 0\nEvaluation took:\n  96 bytes consed\nPACKED-ALLOC-END\n'
                'PACKED-ALLOC-BEGIN 32\nEvaluation took:\n  96 bytes consed\nPACKED-ALLOC-END\n'
                'PACKED-ALLOC-BEGIN 64\nEvaluation took:\n  96 bytes consed\nPACKED-ALLOC-END\n'
                f'PACKED-CODE-BEGIN after\n{code}PACKED-CODE-END\n')

    def test_checksums_cover_nonmultiple_sizes(self):
        self.assertEqual(bench.checksum(0), 0)
        self.assertEqual(bench.checksum(32), 1584)
        self.assertEqual(bench.checksum(35), 1602)

    def test_real_time_cons_counter_retains_fixed_adapter_overhead(self):
        data = bench.parse(self.output(), '', 32, 2, 'sbcl')
        self.assertEqual(data['allocation_probes'], [{'elements': 0, 'bytes_cons': 96},
            {'elements': 32, 'bytes_cons': 96}, {'elements': 64, 'bytes_cons': 96}])
        self.assertEqual(data['incremental_lisp_bytes'], [0, 0])
        self.assertNotIn('allocation_free', data)

    def test_requires_actual_raw_full_entry_installation(self):
        log = ('[T2] PUBLIC-PACKED-BENCH: mapped T2 emitted; raw_f64_values=4\n'
               '[T2] PUBLIC-PACKED-BENCH: optimized worker result installed with mapped T2 ABI; osr_entries=0\n')
        bench.parse(self.output(), log, 32, 2, 'egcl')
        with self.assertRaises(ValueError):
            bench.parse(self.output().replace('addsd', 'nop'), log, 32, 2, 'egcl')
        for bad in ('', log.replace('raw_f64_values=4', 'raw_f64_values=0'),
                    log.replace('osr_entries=0', 'osr_entries=1'), log + 'PUBLIC-PACKED-BENCH deoptimized\n'):
            with self.subTest(log=bad), self.assertRaises(ValueError):
                bench.parse(self.output(), bad, 32, 2, 'egcl')

    def test_rejects_missing_counter_wrong_checksum_or_changed_code(self):
        for bad in (self.output().replace('1200000 1584', '1200000 0'),
                    self.output().replace('96 bytes consed', 'counter unavailable'),
                    self.output().replace('mulsd xmm0,xmm1', 'nop', 1)):
            with self.subTest(output=bad), self.assertRaises(ValueError):
                bench.parse(bad, '', 32, 2, 'sbcl')


if __name__ == '__main__':
    unittest.main()
