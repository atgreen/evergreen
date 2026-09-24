use bliss_rt::bignum::alloc_ratio_cli;
use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, NIL};
use bliss_stdlib::numbers::{ash, minusp, plusp, zerop};

fn check(value: BlissVal, expected: (bool, bool, bool)) {
    assert_eq!(
        (
            zerop(value).unwrap(),
            plusp(value).unwrap(),
            minusp(value).unwrap()
        ),
        expected
    );
}

#[test]
fn zero_sign_kernels_do_not_allocate_lisp_values() {
    let before = bliss_rt::heap_stats().bytes_allocated;
    for n in -1000..1000 {
        check(BlissVal::from_fixnum(n), (n == 0, n > 0, n < 0));
    }
    for n in [
        0.0,
        -0.0,
        1.0,
        -1.0,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
    ] {
        check(BlissVal::from_single_float(n), (n == 0.0, n > 0.0, n < 0.0));
    }
    assert_eq!(bliss_rt::heap_stats().bytes_allocated, before);
}

#[test]
fn exact_ratio_signs_do_not_round_to_float_zero() {
    bliss_rt::rooted!(den = ash(BlissVal::from_fixnum(1), BlissVal::from_fixnum(2000)).unwrap());
    check(*den, (false, true, false));
    for num in [-1, 0, 1] {
        let ratio = alloc_ratio_cli(BlissVal::from_fixnum(num), *den);
        check(ratio, (num == 0, num > 0, num < 0));
    }
    let negative_denominator = alloc_ratio_cli(BlissVal::from_fixnum(1), BlissVal::from_fixnum(-2));
    check(negative_denominator, (false, false, true));
}

#[test]
fn numeric_predicate_type_errors_name_their_domains() {
    assert!(
        matches!(zerop(NIL), Err(BlissError::TypeError { expected, .. }) if expected == "number")
    );
    assert!(
        matches!(plusp(NIL), Err(BlissError::TypeError { expected, .. }) if expected == "real")
    );
    assert!(
        matches!(minusp(NIL), Err(BlissError::TypeError { expected, .. }) if expected == "real")
    );
}
