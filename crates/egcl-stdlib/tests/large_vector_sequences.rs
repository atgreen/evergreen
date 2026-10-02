// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Sequence traversal must account for the extended large-object header.
use egcl_rt::value::EgclVal;
use egcl_stdlib::sequences;

#[test]
fn sequence_copies_preserve_large_vector_lengths_and_elements() {
    let vector_type = EgclVal::from_symbol_index(egcl_compiler::reader::intern_symbol("VECTOR"));
    for n in [65_532, 65_533, 65_537, 1_000_000] {
        let values: Vec<_> = (0..n).map(|i| EgclVal::from_fixnum(i as i64)).collect();
        egcl_rt::rooted!(input = sequences::build_simple_vector(&values));
        for operation in ["subseq", "copy-seq", "concatenate", "reverse"] {
            let output = match operation {
                "subseq" => sequences::subseq(*input, 0, Some(n)),
                "copy-seq" => sequences::copy_seq(*input),
                "concatenate" => sequences::concatenate(vector_type, &[*input]),
                "reverse" => sequences::reverse(*input),
                _ => unreachable!(),
            }
            .unwrap();
            assert_eq!(sequences::length(output).unwrap(), n, "{operation}, n={n}");
            for i in 0..n {
                let expected = if operation == "reverse" { n - 1 - i } else { i };
                assert_eq!(
                    sequences::elt(output, i).unwrap(),
                    EgclVal::from_fixnum(expected as i64),
                    "{operation}, n={n}, index={i}"
                );
            }
        }
        for stable in [false, true] {
            // Descending sort must mutate the actual elements, not the extended
            // size word or the length word immediately before them.
            let sorted = if stable {
                sequences::stable_sort(*input, egcl_rt::value::NIL, None)
            } else {
                sequences::sort(*input, egcl_rt::value::NIL, None)
            }
            .unwrap();
            assert_eq!(sorted, *input);
            assert_eq!(sequences::length(sorted).unwrap(), n);
            for i in 0..n {
                assert_eq!(
                    sequences::elt(sorted, i).unwrap(),
                    EgclVal::from_fixnum((n - 1 - i) as i64),
                    "sort stable={stable}, n={n}, index={i}"
                );
            }
        }
    }
}
