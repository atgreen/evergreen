//! R2.13/R2.14: native struct-by-value adapters checked against C compilation.
#![cfg(all(target_arch = "x86_64", any(target_os = "linux", windows)))]
use egcl_rt::ffi::{AlienType, ffi_call_buffered};

fn integer(bits: u8) -> AlienType {
    AlienType::Int {
        signed: false,
        bits,
    }
}
fn structure(fields: Vec<AlienType>) -> AlienType {
    AlienType::Struct {
        fields,
        packed: false,
    }
}
fn pair(a: AlienType, b: AlienType) -> AlienType {
    structure(vec![a, b])
}
fn address<T>(value: &T) -> *const u8 {
    (value as *const T).cast()
}

#[test]
fn small_aggregates_and_unions_use_their_target_return_convention() {
    unsafe {
        let one = structure(vec![integer(8)]);
        assert_eq!(
            invoke::<u8>(
                "aggregate_small1",
                &one,
                std::slice::from_ref(&one),
                &[address(&211u8)],
                None
            ),
            214
        );
        let two = structure(vec![integer(16)]);
        assert_eq!(
            invoke::<u16>(
                "aggregate_small2",
                &two,
                std::slice::from_ref(&two),
                &[address(&1234u16)],
                None
            ),
            2234
        );
        let four = structure(vec![AlienType::Float]);
        assert_eq!(
            invoke::<f32>(
                "aggregate_float1",
                &four,
                std::slice::from_ref(&four),
                &[address(&1.25f32)],
                None
            ),
            2.5
        );
        let eight = structure(vec![AlienType::Float; 2]);
        assert_eq!(
            invoke::<[f32; 2]>(
                "aggregate_float2",
                &eight,
                std::slice::from_ref(&eight),
                &[address(&[1.25f32, 2.5])],
                None
            ),
            [3.5, 3.25]
        );
        let union = AlienType::Union {
            variants: vec![integer(64), AlienType::Double],
        };
        assert_eq!(
            invoke::<u64>(
                "aggregate_union8",
                &union,
                std::slice::from_ref(&union),
                &[address(&u64::MAX)],
                None
            ),
            u64::MAX ^ 0x123456789abcdef0
        );
    }
}

#[cfg(windows)]
#[test]
fn indirect_aggregate_arguments_are_aligned_private_copies() {
    let big = structure(vec![integer(64); 3]);
    // Deliberately give the bridge an unaligned original object.
    let mut source = [0xa5u8; 40];
    let start = if source.as_ptr() as usize % 16 == 0 {
        1
    } else {
        0
    };
    let original = source;
    let alignment: u64 = unsafe {
        invoke(
            "aggregate_copy_alignment",
            &integer(64),
            &[big],
            &[source.as_mut_ptr().add(start)],
            None,
        )
    };
    assert_eq!(alignment, 0);
    assert_eq!(
        source, original,
        "C may only modify the by-value staging copy"
    );
}

#[test]
fn hidden_return_pointer_shifts_mixed_argument_positions() {
    let big = structure(vec![integer(64); 3]);
    let result: [u64; 3] = unsafe {
        invoke(
            "aggregate_sret_mixed",
            &big,
            &[
                AlienType::Double,
                integer(64),
                AlienType::Float,
                big.clone(),
                AlienType::Double,
            ],
            &[
                address(&2.0f64),
                address(&3u64),
                address(&4.0f32),
                address(&[10u64, 20, 30]),
                address(&5.0f64),
            ],
            None,
        )
    };
    assert_eq!(result, [12, 27, 35]);
}

#[test]
fn variadic_hidden_return_preserves_named_float_and_promotes_trailing_float() {
    let big = structure(vec![integer(64); 3]);
    let pair = structure(vec![integer(64); 2]);
    let result: [u64; 3] = unsafe {
        invoke(
            "aggregate_variadic_sret",
            &big,
            &[
                AlienType::Float,
                integer(32),
                AlienType::Float,
                pair.clone(),
                AlienType::Double,
                pair,
            ],
            &[
                address(&3.5f32),
                address(&2i32),
                address(&5.5f32),
                address(&[10u64, 20]),
                address(&7.5f64),
                address(&[30u64, 40]),
            ],
            Some(2),
        )
    };
    assert_eq!(result, [3, 12, 100]);
}
fn symbol(name: &str) -> *const () {
    static LIBRARY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let library = *LIBRARY.get_or_init(|| {
        #[cfg(windows)]
        {
            let path = std::env::var("EGCL_FFI_AGGREGATES_DLL").expect("prebuilt MinGW C fixture");
            egcl_rt::ffi::load_foreign_library(&path).unwrap() as usize
        }
        #[cfg(unix)]
        {
            let directory =
                std::env::temp_dir().join(format!("egcl-ffi-aggregate-{}", std::process::id()));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("aggregate.so");
            let result = std::process::Command::new("cc")
                .args(["-shared", "-fPIC", "-O2"])
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/ffi_aggregates.c"
                ))
                .arg("-o")
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            egcl_rt::ffi::load_foreign_library(path.to_str().unwrap()).unwrap() as usize
        }
    });
    unsafe { egcl_rt::ffi::foreign_symbol(library as *mut (), name) }.unwrap()
}

unsafe fn invoke<T>(
    name: &str,
    result: &AlienType,
    types: &[AlienType],
    values: &[*const u8],
    fixed: Option<usize>,
) -> T {
    let mut output = std::mem::MaybeUninit::<T>::uninit();
    unsafe {
        ffi_call_buffered(
            symbol(name),
            result,
            types,
            values,
            output.as_mut_ptr().cast(),
            fixed,
        )
        .unwrap();
        output.assume_init()
    }
}

#[test]
fn aggregates_use_independent_integer_and_sse_return_banks() {
    let ii = pair(integer(64), integer(64));
    let x = [11u64, 22];
    let y = [33u64, 44];
    let result: [u64; 2] = unsafe {
        invoke(
            "aggregate_ii",
            &ii,
            &[ii.clone(), ii.clone()],
            &[address(&x), address(&y)],
            None,
        )
    };
    assert_eq!(result, [55, 22 ^ 33]);
    let ss = pair(AlienType::Double, AlienType::Double);
    let result: [f64; 2] = unsafe {
        invoke(
            "aggregate_ss",
            &ss,
            std::slice::from_ref(&ss),
            &[address(&[3.0f64, 4.0])],
            None,
        )
    };
    assert_eq!(result, [5.25, 0.5]);
    #[repr(C)]
    struct IS {
        a: u64,
        b: f64,
    }
    let is = pair(integer(64), AlienType::Double);
    let result: IS = unsafe {
        invoke(
            "aggregate_is",
            &is,
            std::slice::from_ref(&is),
            &[address(&IS { a: 9, b: 1.25 })],
            None,
        )
    };
    assert_eq!((result.a, result.b), (20, 2.5));
    #[repr(C)]
    struct SI {
        a: f64,
        b: u64,
    }
    let si = pair(AlienType::Double, integer(64));
    let result: SI = unsafe {
        invoke(
            "aggregate_si",
            &si,
            std::slice::from_ref(&si),
            &[address(&SI { a: 1.25, b: 9 })],
            None,
        )
    };
    assert_eq!((result.a, result.b), (3.75, 26));
}

#[test]
fn memory_class_returns_use_hidden_storage_and_stack_arguments() {
    let big = structure(vec![integer(64); 3]);
    let x = [1u64, 2, 3];
    let n = 7u64;
    let result: [u64; 3] = unsafe {
        invoke(
            "aggregate_big",
            &big,
            &[big.clone(), integer(64)],
            &[address(&x), address(&n)],
            None,
        )
    };
    assert_eq!(result, [8, 16, 24]);
    let packed = AlienType::Struct {
        fields: vec![integer(8), AlienType::Double],
        packed: true,
    };
    let mut bytes = [0u8; 9];
    bytes[0] = 12;
    bytes[1..].copy_from_slice(&1.25f64.to_ne_bytes());
    let result: [u8; 9] = unsafe {
        invoke(
            "aggregate_packed",
            &packed,
            std::slice::from_ref(&packed),
            &[address(&bytes)],
            None,
        )
    };
    assert_eq!(result[0], 13);
    assert_eq!(f64::from_ne_bytes(result[1..].try_into().unwrap()), 2.5);
}

#[test]
fn partial_eightbytes_do_not_overwrite_caller_storage() {
    let tiny = structure(vec![integer(8); 3]);
    let input = [11u8, 22, 33];
    let mut output = [0xa5u8; 5];
    unsafe {
        ffi_call_buffered(
            symbol("aggregate_tiny"),
            &tiny,
            std::slice::from_ref(&tiny),
            &[address(&input)],
            output.as_mut_ptr().add(1),
            None,
        )
        .unwrap();
    }
    assert_eq!(output, [0xa5, 33, 11, 22, 0xa5]);
}

#[test]
fn nested_structs_and_union_overlap_merge_their_eightbyte_classes() {
    #[repr(C)]
    struct Value {
        a: f32,
        b: f32,
        c: u64,
    }
    let nested = structure(vec![pair(AlienType::Float, AlienType::Float), integer(64)]);
    let result: Value = unsafe {
        invoke(
            "aggregate_nested",
            &nested,
            std::slice::from_ref(&nested),
            &[address(&Value {
                a: 1.25,
                b: 2.5,
                c: 7,
            })],
            None,
        )
    };
    assert_eq!((result.a, result.b, result.c), (2.25, 4.5, 10));
    #[repr(C)]
    struct UnionValue {
        u: u64,
        a: f32,
        b: f32,
    }
    let union = structure(vec![
        AlienType::Union {
            variants: vec![AlienType::Double, integer(64)],
        },
        AlienType::Float,
        AlienType::Float,
    ]);
    let result: UnionValue = unsafe {
        invoke(
            "aggregate_union",
            &union,
            std::slice::from_ref(&union),
            &[address(&UnionValue {
                u: 11,
                a: 1.25,
                b: 2.5,
            })],
            None,
        )
    };
    assert_eq!((result.u, result.a, result.b), (12, 3.25, 5.5));
}

#[test]
fn exhausted_register_bank_rolls_back_the_whole_aggregate() {
    let values = [1u64, 2, 3, 4, 5, 6];
    let value = [7u64, 8];
    let mut types = vec![integer(64); 5];
    types.push(pair(integer(64), integer(64)));
    types.push(integer(64));
    let mut args: Vec<_> = values[..5].iter().map(address).collect();
    args.push(address(&value));
    args.push(address(&values[5]));
    let result: u64 =
        unsafe { invoke("aggregate_gpr_rollback", &integer(64), &types, &args, None) };
    assert_eq!(result, 1 + 4 + 9 + 16 + 25 + 42 + 56 + 48);
    let result: [u64; 3] = unsafe {
        invoke(
            "aggregate_sret_pressure",
            &structure(vec![integer(64); 3]),
            &types,
            &args,
            None,
        )
    };
    assert_eq!(result, [15, 15, 6]);
    let doubles = [1.0f64, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
    let value = [9.0f64, 10.0];
    let mut types = vec![AlienType::Double; 7];
    types.push(pair(AlienType::Double, AlienType::Double));
    types.push(AlienType::Double);
    let mut args: Vec<_> = doubles[..7].iter().map(address).collect();
    args.push(address(&value));
    args.push(address(&doubles[7]));
    let result: f64 = unsafe {
        invoke(
            "aggregate_sse_rollback",
            &AlienType::Double,
            &types,
            &args,
            None,
        )
    };
    assert_eq!(result, 28.0 + 18.0 + 30.0 + 32.0);
    #[repr(C)]
    struct SI {
        a: f64,
        b: u64,
    }
    let value = SI { a: 7.25, b: 8 };
    let tail = 9.5f64;
    let mut types = vec![integer(64); 6];
    types.push(pair(AlienType::Double, integer(64)));
    types.push(AlienType::Double);
    let mut args: Vec<_> = values.iter().map(address).collect();
    args.push(address(&value));
    args.push(address(&tail));
    let result: f64 = unsafe {
        invoke(
            "aggregate_mixed_rollback",
            &AlienType::Double,
            &types,
            &args,
            None,
        )
    };
    assert_eq!(result, 21.0 + 7.25 + 8.0 + 9.5);
}

#[test]
fn variadic_aggregates_and_default_promotions_match_va_arg() {
    #[repr(C)]
    struct IS {
        a: u64,
        b: f64,
    }
    let pairs: Vec<_> = (0..10)
        .map(|i| IS {
            a: i,
            b: i as f64 + 0.25,
        })
        .collect();
    let count = 10u32;
    let float = 1.5f32;
    let small = -3i8;
    let mut types = vec![integer(32)];
    let mut args = vec![address(&count)];
    for value in &pairs {
        types.extend([
            pair(integer(64), AlienType::Double),
            AlienType::Float,
            AlienType::Int {
                signed: true,
                bits: 8,
            },
        ]);
        args.extend([address(value), address(&float), address(&small)]);
    }
    let result: f64 = unsafe {
        invoke(
            "aggregate_variadic",
            &AlienType::Double,
            &types,
            &args,
            Some(1),
        )
    };
    assert_eq!(
        result,
        (0..10)
            .map(|i| 2.0 * i as f64 + 0.25 + 1.5 - 3.0)
            .sum::<f64>()
    );
}

#[test]
fn packed_member_alignment_is_classified_at_its_actual_outer_offset() {
    let packed = AlienType::Struct {
        fields: vec![integer(8), AlienType::Double],
        packed: true,
    };
    let ty = structure(vec![structure(vec![integer(8); 7]), packed]);
    let mut input = [1u8; 16];
    input[7] = 4;
    input[8..].copy_from_slice(&1.25f64.to_ne_bytes());
    let output: [u8; 16] = unsafe {
        invoke(
            "aggregate_realigned",
            &ty,
            std::slice::from_ref(&ty),
            &[address(&input)],
            None,
        )
    };
    assert_eq!(&output[..8], &[1, 2, 3, 4, 5, 6, 7, 6]);
    assert_eq!(f64::from_ne_bytes(output[8..].try_into().unwrap()), 3.75);
}

#[test]
fn malformed_signatures_fail_before_reading_native_buffers() {
    for ty in [
        AlienType::Void,
        integer(0),
        integer(24),
        structure(vec![]),
        structure(vec![AlienType::Void]),
        AlienType::Union { variants: vec![] },
    ] {
        let result = unsafe {
            ffi_call_buffered(
                symbol("aggregate_tiny"),
                &AlienType::Void,
                &[ty],
                &[std::ptr::dangling()],
                std::ptr::null_mut(),
                None,
            )
        };
        assert!(result.is_err());
    }
    let mut nested = integer(8);
    for _ in 0..66 {
        nested = structure(vec![nested]);
    }
    assert!(
        unsafe {
            ffi_call_buffered(
                symbol("aggregate_tiny"),
                &nested,
                &[],
                &[],
                std::ptr::null_mut(),
                None,
            )
        }
        .is_err()
    );
    assert!(
        unsafe {
            ffi_call_buffered(
                std::ptr::null(),
                &AlienType::Void,
                &[],
                &[],
                std::ptr::null_mut(),
                None,
            )
        }
        .is_err()
    );
    assert!(
        unsafe {
            ffi_call_buffered(
                symbol("aggregate_tiny"),
                &integer(8),
                &[],
                &[],
                std::ptr::null_mut(),
                None,
            )
        }
        .is_err()
    );
    assert!(
        unsafe {
            ffi_call_buffered(
                symbol("aggregate_tiny"),
                &AlienType::Void,
                &[integer(8)],
                &[std::ptr::null()],
                std::ptr::null_mut(),
                None,
            )
        }
        .is_err()
    );
    assert!(
        unsafe {
            ffi_call_buffered(
                symbol("aggregate_tiny"),
                &AlienType::Void,
                &[],
                &[],
                std::ptr::null_mut(),
                Some(1),
            )
        }
        .is_err()
    );
}

#[test]
fn argument_and_result_buffers_may_end_exactly_at_a_guard_page() {
    struct Guarded {
        base: *mut u8,
        page: usize,
    }
    impl Guarded {
        fn new() -> Self {
            let page = egcl_rt::syscall::page_size();
            assert!(page.is_power_of_two());
            let base = unsafe {
                egcl_rt::syscall::mmap(
                    std::ptr::null_mut(),
                    page * 2,
                    egcl_rt::syscall::PROT_READ | egcl_rt::syscall::PROT_WRITE,
                    egcl_rt::syscall::MAP_PRIVATE | egcl_rt::syscall::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            }
            .unwrap();
            assert_eq!(
                unsafe {
                    egcl_rt::syscall::mprotect(
                        base.cast::<u8>().add(page).cast(),
                        page,
                        egcl_rt::syscall::PROT_NONE,
                    )
                },
                Ok(())
            );
            Self { base, page }
        }
        fn tail(&self, size: usize) -> *mut u8 {
            unsafe { self.base.cast::<u8>().add(self.page - size) }
        }
    }
    impl Drop for Guarded {
        fn drop(&mut self) {
            unsafe {
                egcl_rt::syscall::munmap(self.base, self.page * 2).unwrap();
            }
        }
    }
    let source = Guarded::new();
    let result = Guarded::new();
    let tiny = structure(vec![integer(8); 3]);
    unsafe {
        std::ptr::copy_nonoverlapping([11u8, 22, 33].as_ptr(), source.tail(3), 3);
        ffi_call_buffered(
            symbol("aggregate_tiny"),
            &tiny,
            std::slice::from_ref(&tiny),
            &[source.tail(3)],
            result.tail(3),
            None,
        )
        .unwrap();
        assert_eq!(std::slice::from_raw_parts(result.tail(3), 3), [33, 11, 22]);
    }
    let packed = AlienType::Struct {
        fields: vec![integer(8), AlienType::Double],
        packed: true,
    };
    let mut bytes = [0u8; 9];
    bytes[0] = 12;
    bytes[1..].copy_from_slice(&1.25f64.to_ne_bytes());
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), source.tail(9), 9);
        ffi_call_buffered(
            symbol("aggregate_packed"),
            &packed,
            std::slice::from_ref(&packed),
            &[source.tail(9)],
            result.tail(9),
            None,
        )
        .unwrap();
        assert_eq!(*result.tail(9), 13);
        assert_eq!(
            std::ptr::read_unaligned(result.tail(9).add(1).cast::<f64>()),
            2.5
        );
    }
}

#[test]
fn aggregate_calls_preserve_callback_gc_transitions_and_error_containment() {
    // This fixture controls the process-global GC participant set. Other Rust
    // harness threads are not Lisp mutators and do not execute safepoint polls.
    const CHILD: &str = "EGCL_AGGREGATE_CALLBACK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "aggregate_calls_preserve_callback_gc_transitions_and_error_containment",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use egcl_rt::ffi::managed_callback::{LispCallback, set_callback_runner};
    use egcl_rt::{EgclError, EgclVal};
    fn runner(closure: EgclVal, _arguments: &[EgclVal]) -> Result<EgclVal, EgclError> {
        assert_eq!(
            egcl_rt::thread::current_thread().state(),
            egcl_rt::thread::NativeThreadState::Running
        );
        egcl_rt::gc::full_gc()?;
        if closure == egcl_rt::value::NIL {
            Ok(egcl_rt::gc::alloc_double_float(42.0))
        } else {
            Err(EgclError::ProgramError(
                "aggregate callback fixture".into(),
            ))
        }
    }
    set_callback_runner(runner);
    let big = structure(vec![integer(64); 3]);
    let pointer = AlienType::Pointer(Box::new(AlienType::Void));
    for fails in [false, true] {
        let callback = LispCallback::new(
            if fails {
                EgclVal::from_fixnum(1)
            } else {
                egcl_rt::value::NIL
            },
            AlienType::Double,
            vec![AlienType::Double],
        )
        .unwrap();
        let entry = callback.as_fn_ptr();
        let mut returned = 0i32;
        let marker = &mut returned as *mut i32;
        let mut output = [0xa5u64; 3];
        let result = unsafe {
            ffi_call_buffered(
                symbol("aggregate_callback"),
                &big,
                &[pointer.clone(), pointer.clone()],
                &[address(&entry), address(&marker)],
                output.as_mut_ptr().cast(),
                None,
            )
        };
        assert_eq!(
            returned, 99,
            "C must return before a callback error reaches Rust"
        );
        assert_eq!(
            egcl_rt::thread::current_thread().state(),
            egcl_rt::thread::NativeThreadState::Running
        );
        if fails {
            let error = result.unwrap_err().to_string();
            assert!(error.contains("aggregate callback fixture"), "{error}");
            assert_eq!(
                output, [0xa5; 3],
                "do not publish a result from a failed call"
            );
        } else {
            result.unwrap();
            assert_eq!(output, [42, 2, 3]);
        }
    }
}
