use bliss_rt::bignum::{BigInt, FIXNUM_MIN};
use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;
use bliss_stdlib::numbers::ash;

#[test]
fn direct_shift_kernel_boundaries_and_allocation() {
    let before = bliss_rt::heap_stats().bytes_allocated;
    for n in -1000..1000 {
        for count in -65..10 {
            let actual = ash(BlissVal::from_fixnum(n), BlissVal::from_fixnum(count)).unwrap();
            let expected = if count < 0 {
                n >> (-count).min(63)
            } else {
                n << count
            };
            assert_eq!(actual.as_fixnum(), expected);
        }
    }
    assert_eq!(
        bliss_rt::heap_stats().bytes_allocated,
        before,
        "fixnum shifts must not allocate Lisp temporaries"
    );
    assert_eq!(
        ash(
            BlissVal::from_fixnum(FIXNUM_MIN),
            BlissVal::from_fixnum(FIXNUM_MIN)
        )
        .unwrap()
        .as_fixnum(),
        -1
    );

    // Neither gigantic positive counts nor operand validation can be skipped
    // accidentally. Reject an unrepresentable nonzero result without trying to
    // reserve an astronomical vector; zero remains exactly zero.
    let huge = BigInt::from_mag(1, vec![0, 1]).to_val();
    bliss_rt::rooted!(huge = huge);
    assert!(matches!(
        ash(BlissVal::from_fixnum(1), *huge),
        Err(BlissError::Oom)
    ));
    assert_eq!(ash(BlissVal::from_fixnum(0), *huge).unwrap().as_fixnum(), 0);
    assert!(matches!(
        ash(BlissVal::from_fixnum(0), BlissVal::from_char('x')),
        Err(BlissError::TypeError { .. })
    ));
}
