use torcl_rt::bignum::{BigInt, FIXNUM_MIN};
use torcl_rt::error::TorclError;
use torcl_rt::value::TorclVal;
use torcl_stdlib::numbers::ash;

#[test]
fn direct_shift_kernel_boundaries_and_allocation() {
    let before = torcl_rt::heap_stats().bytes_allocated;
    for n in -1000..1000 {
        for count in -65..10 {
            let actual = ash(TorclVal::from_fixnum(n), TorclVal::from_fixnum(count)).unwrap();
            let expected = if count < 0 {
                n >> (-count).min(63)
            } else {
                n << count
            };
            assert_eq!(actual.as_fixnum(), expected);
        }
    }
    assert_eq!(
        torcl_rt::heap_stats().bytes_allocated,
        before,
        "fixnum shifts must not allocate Lisp temporaries"
    );
    assert_eq!(
        ash(
            TorclVal::from_fixnum(FIXNUM_MIN),
            TorclVal::from_fixnum(FIXNUM_MIN)
        )
        .unwrap()
        .as_fixnum(),
        -1
    );

    // Neither gigantic positive counts nor operand validation can be skipped
    // accidentally. Reject an unrepresentable nonzero result without trying to
    // reserve an astronomical vector; zero remains exactly zero.
    let huge = BigInt::from_mag(1, vec![0, 1]).to_val();
    torcl_rt::rooted!(huge = huge);
    assert!(matches!(
        ash(TorclVal::from_fixnum(1), *huge),
        Err(TorclError::Oom)
    ));
    assert_eq!(ash(TorclVal::from_fixnum(0), *huge).unwrap().as_fixnum(), 0);
    assert!(matches!(
        ash(TorclVal::from_fixnum(0), TorclVal::from_char('x')),
        Err(TorclError::TypeError { .. })
    ));
}
