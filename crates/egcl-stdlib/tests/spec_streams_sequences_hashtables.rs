use std::collections::HashSet;
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

// Coverage umbrella: R5.23, R5.24, R5.25, R5.26, R5.27, R5.28, R5.29,
// R5.30, R5.31, R5.32, R5.33, R5.34, R5.46, R5.47, R5.111, R5.112,
// R5.116, R5.117, R5.121, R5.125, R5.127, R5.129, R5.130, R5.132,
// R5.134, R5.135, R5.136, R5.137, R5.138, R5.139, R5.140, R5.141,
// R5.142, R5.143, R5.144, R5.156, R5.157.

use egcl_rt::error::EgclError;
use egcl_rt::object::{ConsCell, ObjectHeader, type_id};
use egcl_rt::value::{EOF, NIL, T, EgclVal};
use egcl_stdlib::hashtable::{
    HashTest, MakeHashTableOptions, Weakness, clrhash, gethash, hash_table_count, hash_table_size,
    make_hash_table, remhash, set_gethash, sxhash,
};
use egcl_stdlib::pathnames::register_string;
use egcl_stdlib::sequences;
use egcl_stdlib::streams::{
    ExternalFormat, IF_EXISTS_SUPERSEDE_VAL, StreamDirection, close, file_length_fn, file_position,
    get_output_stream_string, make_broadcast_stream, make_concatenated_stream, make_echo_stream,
    make_lisp_string, make_lisp_string_fresh, make_string_input_stream, make_string_output_stream,
    make_synonym_stream, make_two_way_stream, open, open_stream_p, set_file_position,
    set_symbol_stream, stream_advance_to_column, stream_element_type, stream_external_format,
    stream_finish_output, stream_force_output, stream_fresh_line, stream_listen, stream_peek_char,
    stream_read_char, stream_read_char_no_hang, stream_read_line, stream_read_sequence,
    stream_start_line_p, stream_unread_char, stream_write_char, stream_write_sequence,
    stream_write_string,
};

fn make_list(vals: &[i64]) -> EgclVal {
    let mut list = NIL;
    for &v in vals.iter().rev() {
        let cell = Box::leak(Box::new(ConsCell {
            car: EgclVal::from_fixnum(v),
            cdr: list,
        }));
        list = unsafe { EgclVal::from_cons_ptr(cell as *mut ConsCell as *mut u8) };
    }
    list
}

fn make_vector(vals: &[i64]) -> EgclVal {
    let total_u64s = 2 + vals.len();
    let mut buf: Vec<u64> = Vec::with_capacity(total_u64s);
    buf.push(ObjectHeader::new(type_id::SIMPLE_VECTOR, total_u64s as u16).0);
    buf.push(vals.len() as u64);
    for &v in vals {
        buf.push(EgclVal::from_fixnum(v).to_raw());
    }
    let ptr = buf.as_mut_ptr() as *mut u8;
    std::mem::forget(buf);
    unsafe { EgclVal::from_heap_ptr(ptr) }
}

fn seq_to_fixnums(sequence: EgclVal) -> Vec<i64> {
    (0..sequences::length(sequence).unwrap())
        .map(|i| sequences::elt(sequence, i).unwrap().as_fixnum())
        .collect()
}

fn cons_ptrs(mut list: EgclVal) -> Vec<usize> {
    let mut out = Vec::new();
    while list.is_cons() {
        out.push(unsafe { list.as_ptr() as usize });
        let cell = unsafe { &*(list.as_ptr() as *const ConsCell) };
        list = cell.cdr;
    }
    out
}

fn lisp_string_to_string(val: EgclVal) -> String {
    // Constructed strings are 32-bit SIMPLE_CHARACTER_STRINGs (write_character_string,
    // spec §1.6.3): the payload is wide code points, not UTF-8 bytes. Decode with the
    // production reader; the old byte-slice decode read wide "olleh" as "o\0\0\0l"
    // (bliss-cizc).
    unsafe { egcl_rt::object::read_simple_string(val.as_ptr()) }
}

fn vector_symbol() -> EgclVal {
    EgclVal::from_symbol_index(egcl_compiler::reader::intern_symbol("VECTOR"))
}

fn addition_fn() -> EgclVal {
    EgclVal::from_symbol_index(4)
}

fn negate_key() -> EgclVal {
    EgclVal::from_symbol_index(3)
}

fn unique_path(name: &str) -> EgclVal {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = format!("{}/target/spec-artifacts", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = format!("{dir}/{}_{}_{}.tmp", name, std::process::id(), nanos);
    let val = make_lisp_string_fresh(&path);
    register_string(val, &path);
    val
}

#[test]
fn stream_string_entrypoints_round_trip_and_dispatch() {
    // Per R5.113 and R5.118, standard stream entrypoints must dispatch through the
    // Gray protocol and string streams must support in-memory character I/O.
    let input = make_string_input_stream(make_lisp_string("alpha\nbeta"), 0, None).unwrap();

    assert_eq!(stream_peek_char(input).unwrap(), EgclVal::from_char('a'));
    assert_eq!(
        stream_read_char_no_hang(input).unwrap(),
        EgclVal::from_char('a')
    );
    assert_eq!(stream_read_char(input).unwrap(), EgclVal::from_char('l'));

    let (line1, eof1) = stream_read_line(input).unwrap();
    assert_eq!(lisp_string_to_string(line1), "pha");
    assert!(!eof1);

    let (line2, eof2) = stream_read_line(input).unwrap();
    assert_eq!(lisp_string_to_string(line2), "beta");
    assert!(eof2);
    assert_eq!(stream_read_char(input).unwrap(), EOF);
    assert!(!stream_listen(input).unwrap());

    let output = make_string_output_stream(NIL).unwrap();
    stream_write_string(output, make_lisp_string("ab"), 0, None).unwrap();
    stream_write_char(output, EgclVal::from_char('c')).unwrap();
    assert!(stream_fresh_line(output).unwrap());
    assert!(stream_start_line_p(output));
    assert!(stream_advance_to_column(output, 3).unwrap());
    stream_write_sequence(
        output,
        &[EgclVal::from_char('x'), EgclVal::from_char('y')],
    )
    .unwrap();

    let rendered = lisp_string_to_string(get_output_stream_string(output).unwrap());
    assert_eq!(rendered, "abc\n   xy");
}

#[test]
fn composite_streams_delegate_to_their_constituents() {
    // Per R5.119, broadcast/concatenated/two-way/echo/synonym streams must delegate
    // exactly to their underlying streams through the public entrypoints.
    let out_a = make_string_output_stream(NIL).unwrap();
    let out_b = make_string_output_stream(NIL).unwrap();
    let broadcast = make_broadcast_stream(&[out_a, out_b]).unwrap();
    stream_write_string(broadcast, make_lisp_string("fanout"), 0, None).unwrap();
    assert_eq!(
        lisp_string_to_string(get_output_stream_string(out_a).unwrap()),
        "fanout"
    );
    assert_eq!(
        lisp_string_to_string(get_output_stream_string(out_b).unwrap()),
        "fanout"
    );

    let cat = make_concatenated_stream(&[
        make_string_input_stream(make_lisp_string("ab"), 0, None).unwrap(),
        make_string_input_stream(make_lisp_string("cd"), 0, None).unwrap(),
    ])
    .unwrap();
    let chars = stream_read_sequence(cat, 4)
        .unwrap()
        .into_iter()
        .map(|v| v.as_char())
        .collect::<String>();
    assert_eq!(chars, "abcd");

    let two_way_out = make_string_output_stream(NIL).unwrap();
    let two_way = make_two_way_stream(
        make_string_input_stream(make_lisp_string("in"), 0, None).unwrap(),
        two_way_out,
    )
    .unwrap();
    stream_write_char(two_way, EgclVal::from_char('!')).unwrap();
    assert_eq!(stream_read_char(two_way).unwrap(), EgclVal::from_char('i'));
    assert_eq!(
        lisp_string_to_string(get_output_stream_string(two_way_out).unwrap()),
        "!"
    );

    let echo_out = make_string_output_stream(NIL).unwrap();
    let echo = make_echo_stream(
        make_string_input_stream(make_lisp_string("xy"), 0, None).unwrap(),
        echo_out,
    )
    .unwrap();
    assert_eq!(stream_read_char(echo).unwrap(), EgclVal::from_char('x'));
    assert_eq!(stream_read_char(echo).unwrap(), EgclVal::from_char('y'));
    assert_eq!(
        lisp_string_to_string(get_output_stream_string(echo_out).unwrap()),
        "xy"
    );

    let symbol = EgclVal::from_symbol_index(4001);
    let synonym_target = make_string_input_stream(make_lisp_string("syn"), 0, None).unwrap();
    set_symbol_stream(symbol, synonym_target);
    let synonym = make_synonym_stream(symbol).unwrap();
    assert_eq!(stream_read_char(synonym).unwrap(), EgclVal::from_char('s'));
}

#[test]
fn close_is_idempotent_and_closed_streams_signal_errors() {
    // Per R5.122 and R5.123, CLOSE must be idempotent and further I/O must signal
    // a stream-closed error.
    let stream = make_string_output_stream(NIL).unwrap();
    close(stream, false).unwrap();
    close(stream, true).unwrap();
    assert!(!open_stream_p(stream));

    let err = stream_write_char(stream, EgclVal::from_char('x')).unwrap_err();
    match err {
        EgclError::StreamError(message) => assert!(message.contains("closed")),
        other => panic!("expected StreamError, got {other:?}"),
    }
}

#[test]
fn file_streams_expose_position_length_and_external_format() {
    // Per R5.114, R5.115, R5.124, and R5.128, file streams must expose file I/O,
    // explicit external formats, and position/length queries.
    let path = unique_path("spec_stream");
    let out = open(
        path,
        StreamDirection::Output,
        NIL,
        IF_EXISTS_SUPERSEDE_VAL,
        T,
        ExternalFormat::Utf8,
    )
    .unwrap();

    assert_eq!(stream_external_format(out), ExternalFormat::Utf8);
    assert_eq!(stream_element_type(out), T);
    stream_write_string(out, make_lisp_string("hello"), 0, None).unwrap();
    stream_force_output(out).unwrap();
    stream_finish_output(out).unwrap();
    assert_eq!(file_position(out).unwrap().as_fixnum(), 5);
    close(out, false).unwrap();

    let input = open(
        path,
        StreamDirection::Input,
        NIL,
        T,
        T,
        ExternalFormat::Utf8,
    )
    .unwrap();
    assert_eq!(file_length_fn(input).unwrap().as_fixnum(), 5);
    assert_eq!(file_position(input).unwrap().as_fixnum(), 0);
    assert_eq!(
        stream_read_sequence(input, 2)
            .unwrap()
            .into_iter()
            .map(|v| v.as_char())
            .collect::<String>(),
        "he"
    );
    assert_eq!(file_position(input).unwrap().as_fixnum(), 2);
    assert_eq!(
        set_file_position(input, EgclVal::from_fixnum(1)).unwrap(),
        T
    );
    assert!(stream_unread_char(input, EgclVal::from_char('e')).is_ok());
}

#[test]
fn stream_multi_element_operations_are_atomic_across_threads() {
    // Per R5.120 and the stream lock-ordering rules referenced by R13.05/R13.06,
    // each write-sequence call must hold the per-stream mutex for the full operation.
    let stream = Arc::new(make_string_output_stream(NIL).unwrap());

    thread::scope(|scope| {
        for ch in ['A', 'B'] {
            let stream = Arc::clone(&stream);
            scope.spawn(move || {
                for _ in 0..50 {
                    stream_write_sequence(
                        *stream,
                        &[
                            EgclVal::from_char(ch),
                            EgclVal::from_char(ch),
                            EgclVal::from_char(ch),
                            EgclVal::from_char(ch),
                        ],
                    )
                    .unwrap();
                }
            });
        }
    });

    let out = lisp_string_to_string(get_output_stream_string(*stream).unwrap());
    assert_eq!(out.len(), 400);
    for chunk in out.as_bytes().chunks_exact(4) {
        assert!(chunk.iter().all(|b| *b == chunk[0]));
    }
}

#[test]
fn sequence_dispatch_and_bounds_follow_spec() {
    // Per R5.131-R5.133 and R5.149-R5.151, including the R5.150 error path,
    // sequence entrypoints must work on
    // lists and vectors and validate start/end bounds before processing.
    let list = make_list(&[10, 20, 30, 40]);
    let vector = make_vector(&[1, 2, 3, 4]);

    assert_eq!(sequences::length(list).unwrap(), 4);
    assert_eq!(sequences::length(vector).unwrap(), 4);
    assert_eq!(
        seq_to_fixnums(sequences::subseq(list, 1, None).unwrap()),
        vec![20, 30, 40]
    );
    assert_eq!(
        seq_to_fixnums(sequences::concatenate(vector_symbol(), &[list, vector]).unwrap()),
        vec![10, 20, 30, 40, 1, 2, 3, 4]
    );

    let err = sequences::subseq(vector, 3, Some(1)).unwrap_err();
    match err {
        EgclError::TypeError { .. } => {}
        other => panic!("expected TypeError, got {other:?}"),
    }
    assert!(sequences::subseq(vector, 0, Some(99)).is_err());
}

#[test]
fn sequence_search_and_reduction_honor_keywords() {
    // Per R5.133 and R5.149-R5.151, keyword-driven sequence operations must
    // honor :key/:start/:end/:from-end consistently through the top-level API.
    let list = make_list(&[1, 2, 3, 2, 1]);

    assert_eq!(
        sequences::find(
            EgclVal::from_fixnum(-2),
            list,
            NIL,
            Some(negate_key()),
            0,
            None,
            false
        )
        .unwrap(),
        EgclVal::from_fixnum(2)
    );
    assert_eq!(
        sequences::position(EgclVal::from_fixnum(2), list, NIL, None, 0, None, true).unwrap(),
        EgclVal::from_fixnum(3)
    );
    assert_eq!(
        sequences::count(EgclVal::from_fixnum(2), list, NIL, None, 1, Some(4)).unwrap(),
        EgclVal::from_fixnum(2)
    );
    assert_eq!(
        sequences::reduce(
            addition_fn(),
            list,
            Some(EgclVal::from_fixnum(10)),
            None,
            1,
            Some(4),
            false
        )
        .unwrap(),
        EgclVal::from_fixnum(17)
    );
}

#[test]
#[ignore = "stage 3: sequences/hash-tables"]
fn sort_and_stable_sort_follow_destructive_contracts() {
    // Per R5.145-R5.148, including R5.146 and R5.147 specifically,
    // SORT/STABLE-SORT are destructive contracts: callers must use the
    // returned value, vectors sort in-place, and list sorts reuse cons cells.
    let vector = make_vector(&[4, 1, 3, 2]);
    let sorted_vector = sequences::sort(vector, T, None).unwrap();
    assert_eq!(sorted_vector, vector);
    assert_eq!(seq_to_fixnums(vector), vec![1, 2, 3, 4]);

    let list = make_list(&[4, 1, 3, 2]);
    let original_cells: HashSet<_> = cons_ptrs(list).into_iter().collect();
    let sorted_list = sequences::stable_sort(list, T, None).unwrap();
    assert_eq!(seq_to_fixnums(sorted_list), vec![1, 2, 3, 4]);
    let returned_cells: HashSet<_> = cons_ptrs(sorted_list).into_iter().collect();
    assert_eq!(returned_cells, original_cells);
}

#[test]
#[ignore = "stage 3: sequences/hash-tables"]
fn hash_tables_obey_test_functions_and_sxhash_contract() {
    // Per R5.154 and R5.155, hash selection must match the equality test and
    // SXHASH must agree for EQUAL objects.
    let eq_table = make_hash_table(&MakeHashTableOptions {
        test: HashTest::Equal,
        ..MakeHashTableOptions::default()
    })
    .unwrap();
    let key_a = make_list(&[1, 2, 3]);
    let key_b = make_list(&[1, 2, 3]);
    set_gethash(key_a, eq_table, EgclVal::from_fixnum(99)).unwrap();
    assert_eq!(
        gethash(key_b, eq_table, NIL).unwrap(),
        (EgclVal::from_fixnum(99), true)
    );

    let equalp_table = make_hash_table(&MakeHashTableOptions {
        test: HashTest::Equalp,
        ..MakeHashTableOptions::default()
    })
    .unwrap();
    let hello = make_lisp_string("Hello");
    let lower = make_lisp_string("hello");
    set_gethash(hello, equalp_table, EgclVal::from_fixnum(7)).unwrap();
    assert_eq!(
        gethash(lower, equalp_table, NIL).unwrap(),
        (EgclVal::from_fixnum(7), true)
    );

    let sx_a = sxhash(key_a);
    let sx_b = sxhash(key_b);
    assert!(sx_a.is_fixnum() && sx_a.as_fixnum() >= 0);
    assert_eq!(sx_a, sx_b);
}

#[test]
fn robin_hood_growth_and_shrink_are_observable() {
    // Per R5.152 and R5.153, public operations must preserve lookup semantics
    // across Robin Hood displacement, growth, and shrink cycles.
    let table = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    let initial_size = hash_table_size(table).unwrap();

    for i in 0..20 {
        set_gethash(
            EgclVal::from_fixnum(i),
            table,
            EgclVal::from_fixnum(i * 10),
        )
        .unwrap();
    }
    assert_eq!(hash_table_size(table).unwrap(), initial_size * 2);
    for i in 0..20 {
        assert_eq!(
            gethash(EgclVal::from_fixnum(i), table, NIL).unwrap(),
            (EgclVal::from_fixnum(i * 10), true)
        );
    }

    for i in 0..17 {
        assert!(remhash(EgclVal::from_fixnum(i), table).unwrap());
    }
    assert!(hash_table_size(table).unwrap() <= initial_size * 2);
    assert!(hash_table_size(table).unwrap() >= initial_size);
}

#[test]
fn remhash_backward_shift_keeps_probe_chains_findable() {
    // Per R5.152, deletion uses backward shifting, so removing one key must not
    // strand later keys in the same probe cluster.
    let table = make_hash_table(&MakeHashTableOptions::default()).unwrap();
    for i in 0..64 {
        set_gethash(
            EgclVal::from_fixnum(i),
            table,
            EgclVal::from_fixnum(i + 100),
        )
        .unwrap();
    }
    assert!(remhash(EgclVal::from_fixnum(7), table).unwrap());
    assert_eq!(
        gethash(EgclVal::from_fixnum(7), table, NIL).unwrap(),
        (NIL, false)
    );
    assert_eq!(
        gethash(EgclVal::from_fixnum(63), table, NIL).unwrap(),
        (EgclVal::from_fixnum(163), true)
    );
}

#[test]
fn synchronized_and_weak_hash_table_modes_preserve_normal_operations() {
    // Per R9.10 and R9.11, synchronized and weak tables are created through
    // MAKE-HASH-TABLE and must still support the standard hash-table API.
    for weakness in [
        Some(Weakness::Key),
        Some(Weakness::Value),
        Some(Weakness::KeyAndValue),
    ] {
        let table = make_hash_table(&MakeHashTableOptions {
            synchronized: true,
            weakness,
            ..MakeHashTableOptions::default()
        })
        .unwrap();
        set_gethash(EgclVal::from_fixnum(1), table, EgclVal::from_fixnum(11)).unwrap();
        set_gethash(EgclVal::from_fixnum(2), table, EgclVal::from_fixnum(22)).unwrap();
        assert_eq!(hash_table_count(table).unwrap(), 2);
        assert_eq!(
            gethash(EgclVal::from_fixnum(2), table, NIL).unwrap(),
            (EgclVal::from_fixnum(22), true)
        );
        clrhash(table).unwrap();
        assert_eq!(hash_table_count(table).unwrap(), 0);
    }
}

#[test]
fn strings_are_sequences() {
    // ANSI: a string is a sequence of characters. Regression for bliss-2pt.9
    // (LENGTH on a string reproduced the ASDF "not of type sequence" error).
    let s = make_lisp_string("hello");
    assert_eq!(sequences::length(s).unwrap(), 5);
    assert_eq!(sequences::elt(s, 0).unwrap(), EgclVal::from_char('h'));
    assert_eq!(sequences::elt(s, 4).unwrap(), EgclVal::from_char('o'));
    assert!(sequences::elt(s, 5).is_err()); // out of bounds

    // REVERSE and SUBSEQ of a string are strings (same element type).
    assert_eq!(
        lisp_string_to_string(sequences::reverse(s).unwrap()),
        "olleh"
    );
    assert_eq!(
        lisp_string_to_string(sequences::subseq(s, 1, Some(3)).unwrap()),
        "el"
    );

    // COPY-SEQ preserves contents (read side goes through collect_elements).
    assert_eq!(
        sequences::length(sequences::copy_seq(s).unwrap()).unwrap(),
        5
    );
}
