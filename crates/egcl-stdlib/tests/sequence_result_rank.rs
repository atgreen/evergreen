// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_stdlib::sequences;

#[test]
fn array_sequence_result_validation_rejects_nonvector_ranks_before_allocation() {
    for element in ["T", "CHARACTER", "BIT", "(UNSIGNED-BYTE 8)", "NIL"] {
        for head in ["ARRAY", "SIMPLE-ARRAY"] {
            for dimensions in ["()", "(0 0)", "0", "2"] {
                let spec = format!("({head} {element} {dimensions})");
                let (kind, _) = egcl_compiler::reader::read_from_string(&spec).unwrap();
                egcl_rt::rooted!(kind = kind);
                assert!(
                    sequences::validate_sequence_result_type(*kind, 0).is_err(),
                    "{spec}"
                );
                assert!(
                    sequences::validate_sequence_array_rank(*kind).is_err(),
                    "{spec}"
                );
                match sequences::validate_sequence_array_rank(*kind).unwrap_err() {
                    egcl_rt::EgclError::TypeError { datum, expected } => {
                        assert_ne!(datum, egcl_rt::EgclVal::from_fixnum(1), "{spec}");
                        assert_eq!(expected, "(EQL 1)");
                    }
                    error => panic!("wrong rank error for {spec}: {error:?}"),
                }
                assert!(
                    sequences::build_result_sequence(*kind, &[]).is_err(),
                    "{spec}"
                );
            }
            for dimensions in ["1", "(*)", "(0)"] {
                let spec = format!("({head} {element} {dimensions})");
                let (kind, _) = egcl_compiler::reader::read_from_string(&spec).unwrap();
                egcl_rt::rooted!(kind = kind);
                assert!(
                    sequences::validate_sequence_result_type(*kind, 0).is_ok(),
                    "{spec}"
                );
            }
            let spec = format!("({head} {element} (1))");
            let (kind, _) = egcl_compiler::reader::read_from_string(&spec).unwrap();
            egcl_rt::rooted!(kind = kind);
            assert!(sequences::validate_sequence_result_type(*kind, 0).is_err());
            match sequences::validate_sequence_result_type(*kind, 0).unwrap_err() {
                egcl_rt::EgclError::TypeError { datum, expected } => {
                    assert_eq!(datum, egcl_rt::EgclVal::from_fixnum(0));
                    assert_eq!(expected, "(EQL 1)");
                }
                error => panic!("wrong size error for {spec}: {error:?}"),
            }
            assert!(sequences::validate_sequence_result_type(*kind, 1).is_ok());
        }
    }
}
