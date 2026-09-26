//! Exact Lisp/C scalar conversion, including values outside the fixnum range.
use torcl_rt::{
    bignum::{BigInt, bigint_from_val},
    ffi::{AlienType, marshal_to_c, unmarshal_from_c},
    value::{NIL, TorclVal},
};

#[test]
fn doubles_round_trip_without_narrowing_or_changing_lisp_type() {
    for bits in [
        1.0000000000000002f64.to_bits(),
        (-0.0f64).to_bits(),
        f64::INFINITY.to_bits(),
        0x7ff8_0000_0000_0123,
        42.0f64.to_bits(),
    ] {
        torcl_rt::rooted!(value = unmarshal_from_c(bits, &AlienType::Double).unwrap());
        assert!(
            value.is_double_float(),
            "double return must remain a double"
        );
        assert_eq!(value.as_double_float().to_bits(), bits);
        assert_eq!(marshal_to_c(*value, &AlienType::Double).unwrap(), bits);
    }
}

#[test]
fn full_width_integers_round_trip_through_lisp_bignums() {
    for signed in [false, true] {
        let ty = AlienType::Int { signed, bits: 64 };
        for raw in [0, 1, (1u64 << 60) - 1, 1 << 60, 1 << 63, u64::MAX] {
            torcl_rt::rooted!(value = unmarshal_from_c(raw, &ty).unwrap());
            let expected = if signed {
                BigInt::from_i64(raw as i64)
            } else {
                BigInt::from_mag(1, vec![raw])
            };
            let actual = bigint_from_val(*value).unwrap();
            assert_eq!(actual.sign, expected.sign);
            assert_eq!(actual.mag, expected.mag);
            assert_eq!(marshal_to_c(*value, &ty).unwrap(), raw);
        }
    }
}

#[test]
fn integer_arguments_must_fit_the_declared_c_type() {
    for bits in [8, 16, 32, 64] {
        for signed in [false, true] {
            let ty = AlienType::Int { signed, bits };
            let upper = if signed {
                (1u64 << (bits - 1)) - 1
            } else if bits == 64 {
                u64::MAX
            } else {
                (1u64 << bits) - 1
            };
            torcl_rt::rooted!(max = BigInt::from_mag(1, vec![upper]).to_val());
            assert_eq!(marshal_to_c(*max, &ty).unwrap(), upper);
            let too_big = if upper == u64::MAX {
                BigInt::from_mag(1, vec![0, 1])
            } else {
                BigInt::from_mag(1, vec![upper + 1])
            };
            torcl_rt::rooted!(overflow = too_big.to_val());
            assert!(
                marshal_to_c(*overflow, &ty).is_err(),
                "overflow into {ty:?}"
            );
            if !signed {
                assert!(marshal_to_c(TorclVal::from_fixnum(-1), &ty).is_err());
            } else {
                let magnitude = 1u64 << (bits - 1);
                torcl_rt::rooted!(minimum = BigInt::from_mag(-1, vec![magnitude]).to_val());
                assert_eq!(
                    marshal_to_c(*minimum, &ty).unwrap(),
                    magnitude.wrapping_neg()
                );
                torcl_rt::rooted!(underflow = BigInt::from_mag(-1, vec![magnitude + 1]).to_val());
                assert!(marshal_to_c(*underflow, &ty).is_err());
            }
        }
    }
    assert!(
        marshal_to_c(
            NIL,
            &AlienType::Int {
                signed: true,
                bits: 32
            }
        )
        .is_err()
    );
}

#[test]
fn integer_return_uses_only_the_declared_width() {
    for bits in [8, 16, 32] {
        let ty = AlienType::Int {
            signed: false,
            bits,
        };
        let value = unmarshal_from_c(u64::MAX, &ty).unwrap();
        assert_eq!(value.as_fixnum(), ((1u64 << bits) - 1) as i64);
    }
}

#[test]
fn invalid_integer_widths_are_errors_in_both_directions() {
    for bits in [0, 7, 65, 128] {
        let ty = AlienType::Int { signed: true, bits };
        assert!(marshal_to_c(TorclVal::from_fixnum(0), &ty).is_err());
        assert!(unmarshal_from_c(0, &ty).is_err());
    }
}
