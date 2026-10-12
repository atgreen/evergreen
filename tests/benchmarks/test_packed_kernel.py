#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
import unittest
import packed_kernel as bench


class EvidenceTests(unittest.TestCase):
    def output(self):
        code = '; PUBLIC-PACKED-BENCH\n; T2 — code\n; 32 bytes of x86-64:\n; Mapped native transfer ABI\n  +0000: mov r9,[rdx+10h]\n  +0004: mulsd xmm0,xmm1\n  +0008: addsd xmm0,xmm1\n  +000c: mov [rdx+10h],r9\n'
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
               '[T2] PUBLIC-PACKED-BENCH: optimized worker result installed with mapped T2 ABI; bytes=32, entry=0x1234000, activation_slots=4, osr_entries=0\n')
        bench.parse(self.output(), log, 32, 2, 'egcl')
        with self.assertRaises(ValueError):
            bench.parse(self.output().replace('addsd', 'nop'), log, 32, 2, 'egcl')
        for bad in ('', log.replace('raw_f64_values=4', 'raw_f64_values=0'),
                    log.replace('osr_entries=0', 'osr_entries=1'), log + 'PUBLIC-PACKED-BENCH deoptimized\n',
                    log + '[T2] PUBLIC-PACKED-BENCH: native guard deopt #1 => resuming interpreted\n'):
            with self.subTest(log=bad), self.assertRaises(ValueError):
                bench.parse(self.output(), bad, 32, 2, 'egcl')

    def test_typed_reference_is_explicit_and_never_changes_egcl_source(self):
        typed = bench.workload('sbcl', 4096, 1, 16, sbcl_mode='typed')
        generic = bench.workload('sbcl', 4096, 1, 16, sbcl_mode='same-source')
        egcl = bench.workload('egcl', 4096, 1, 16, sbcl_mode='typed')
        self.assertIn('(type (simple-array double-float (*)) src dst)', typed)
        self.assertIn('(optimize (speed 3) (safety 1) (debug 0))', typed)
        self.assertIn('(type double-float scale)', typed)
        self.assertIn('(type fixnum i)', typed)
        self.assertNotIn('(optimize', generic)
        self.assertNotIn('(optimize', egcl)
        for source in (typed, generic, egcl):
            self.assertIn('(setf (aref dst i) (+ (* x scale) x))', source)

    def test_typed_reference_rejects_generic_machine_code(self):
        with self.assertRaises(ValueError):
            bench.parse(self.output(), '', 32, 2, 'sbcl', sbcl_mode='typed')
        packed = self.output().replace('+0004: mulsd', '+0003: movsd xmm0,[rax]\n  +0004: mulsd')
        bench.parse(packed, '', 32, 2, 'sbcl', sbcl_mode='typed')

    def test_rejects_missing_counter_wrong_checksum_or_changed_code(self):
        for bad in (self.output().replace('1200000 1584', '1200000 0'),
                    self.output().replace('96 bytes consed', 'counter unavailable'),
                    self.output().replace('mulsd xmm0,xmm1', 'nop', 1)):
            with self.subTest(output=bad), self.assertRaises(ValueError):
                bench.parse(bad, '', 32, 2, 'sbcl')

    def test_rejects_ambiguous_emission_installation_and_missing_memory_ops(self):
        emitted = '[T2] PUBLIC-PACKED-BENCH: mapped T2 emitted; raw_f64_values=4\n'
        installed = '[T2] PUBLIC-PACKED-BENCH: optimized worker result installed with mapped T2 ABI; bytes=32, entry=0x1234000, activation_slots=4, osr_entries=0\n'
        for log in (emitted * 2 + installed, emitted + installed * 2,
                    installed + emitted, emitted + installed.replace('bytes=32', 'bytes=64')):
            with self.subTest(log=log), self.assertRaises(ValueError):
                bench.parse(self.output(), log, 32, 2, 'egcl')
        for instruction in ('mov r9,[rdx+10h]', 'mov [rdx+10h],r9'):
            with self.subTest(instruction=instruction), self.assertRaises(ValueError):
                bench.parse(self.output().replace(instruction, 'nop'), emitted + installed, 32, 2, 'egcl')


if __name__ == '__main__':
    unittest.main()
