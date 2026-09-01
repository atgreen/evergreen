//! Spec-derived tests for FORMAT/printer APIs (§5.9) and pathnames (§5.8).
//! Coverage umbrella: R5.35, R5.36, R5.37, R5.38, R5.39, R5.40, R5.41,
//! R5.42, R5.43, R5.181, R5.183, R5.188, R5.195, R5.201.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use bliss_rt::error::BlissError;
use bliss_rt::object::ConsCell;
use bliss_rt::object::{ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, NIL, T};
use bliss_stdlib::{
    NewlineKind, TabKind, directory, ensure_directories_exist, format, get_output_stream_string,
    logical_pathname_translations, make_lisp_string, make_pathname, make_string_output_stream,
    merge_pathnames, namestring, parse_namestring, pathname_directory, pathname_host,
    pathname_match_p, pathname_name, pathname_type, pathname_version, pprint_logical_block,
    pprint_newline, pprint_tab, probe_file, register_string, set_logical_pathname_translations,
    translate_logical_pathname, truename, wild_pathname_p,
};

fn pseudo_string(s: &str) -> BlissVal {
    let val = make_lisp_string(s);
    register_string(val, s);
    val
}

fn keyword(name: &str) -> BlissVal {
    let mut h: u64 = 0x517cc1b727220a95;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    BlissVal::from_raw((h & !0b111) | 0b101)
}

fn list(vals: &[BlissVal]) -> BlissVal {
    let mut out = NIL;
    for &val in vals.iter().rev() {
        let cell = Box::leak(Box::new(ConsCell { car: val, cdr: out }));
        out = unsafe { BlissVal::from_cons_ptr(cell as *mut ConsCell as *mut u8) };
    }
    out
}

fn fixture_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::current_dir()
        .unwrap()
        .join("target")
        .join("spec-fixtures")
        .join(format!("{}_{}_{}", label, std::process::id(), nonce))
}

fn assert_reparse_same(pathname: BlissVal) {
    let rendered = namestring(pathname).expect("namestring");
    let (reparsed, pos) = parse_namestring(rendered, None, None).expect("reparse namestring");
    let rendered_again = namestring(reparsed).expect("re-render namestring");
    let (_, pos_again) = parse_namestring(rendered_again, None, None).expect("reparse again");
    assert_eq!(pathname_host(reparsed), pathname_host(pathname));
    assert_eq!(pathname_directory(reparsed), pathname_directory(pathname));
    assert_eq!(pathname_name(reparsed), pathname_name(pathname));
    assert_eq!(pathname_type(reparsed), pathname_type(pathname));
    assert_eq!(pathname_version(reparsed), pathname_version(pathname));
    assert!(pos > 0 || pathname == reparsed);
    assert!(pos_again > 0 || reparsed == rendered_again);
}

fn heap_string(val: BlissVal) -> String {
    assert!(val.is_heap_object(), "expected heap string");
    unsafe {
        let ptr = val.as_ptr();
        let tid = (*(ptr as *const ObjectHeader)).type_id();
        // FORMAT produces wide SIMPLE_CHARACTER_STRINGs; decode with the production
        // reader rather than reading the payload as UTF-8 bytes (bliss-cizc).
        assert!(
            tid == type_id::SIMPLE_BASE_STRING || tid == type_id::SIMPLE_CHARACTER_STRING,
            "expected a simple string heap object, got type_id {tid:#x}"
        );
        bliss_rt::object::read_simple_string(ptr)
    }
}

#[test]
fn format_accepts_nil_and_stream_destinations_and_rejects_other_values() {
    // Per R5.156, FORMAT accepts NIL and output streams as destinations.
    let rendered = format(NIL, "Hello, ~A", &[make_lisp_string("world")]).expect("format nil");
    assert_eq!(rendered, make_lisp_string("Hello, world"));

    let stream = make_string_output_stream(NIL).expect("string output stream");
    let stream_result =
        format(stream, "~D bottles", &[BlissVal::from_fixnum(3)]).expect("stream format");
    assert_eq!(stream_result, NIL);
    assert_eq!(
        get_output_stream_string(stream).expect("stream contents"),
        make_lisp_string("3 bottles")
    );

    let err = format(BlissVal::from_fixnum(7), "~A", &[]).expect_err("invalid destination");
    assert!(matches!(err, BlissError::TypeError { .. }));
}

#[test]
fn format_supports_modifiers_v_and_hash_parameters_and_is_thread_safe() {
    // Per R5.157/R5.158, directive modifiers and V/# params are supported.
    let rendered = format(
        NIL,
        "~:A ~@D ~:D ~vA ~#D",
        &[
            NIL,
            BlissVal::from_fixnum(9),
            BlissVal::from_fixnum(1200),
            BlissVal::from_fixnum(5),
            BlissVal::from_fixnum(7),
            BlissVal::from_fixnum(1),
            BlissVal::from_fixnum(2),
        ],
    )
    .expect("format features");
    assert_eq!(heap_string(rendered), "() +9 1,200 7      1");

    // Per R5.180, concurrent FORMAT NIL calls must not corrupt shared state.
    let threads: Vec<_> = (0..8)
        .map(|i| {
            thread::spawn(move || {
                format(NIL, "[~D]", &[BlissVal::from_fixnum(i)]).expect("concurrent format")
            })
        })
        .collect();
    let mut outputs = threads
        .into_iter()
        .map(|h| h.join().expect("thread join"))
        .collect::<Vec<_>>();
    outputs.sort_by_key(|v| format!("{:?}", v));
    for i in 0..8 {
        assert!(outputs.contains(&make_lisp_string(&format!("[{}]", i))));
    }
}

#[test]
fn format_exercises_recursive_conditional_iteration_and_plural_directives() {
    // Per R5.157/R5.159/R5.160/R5.161, recursive, iteration, conditional,
    // and plural directives produce observable output through FORMAT.
    let recursive_args = list(&[BlissVal::from_fixnum(4), BlissVal::from_fixnum(5)]);
    assert_eq!(
        format(
            NIL,
            "~? / ~@?",
            &[
                make_lisp_string("~D+~D"),
                recursive_args,
                make_lisp_string("~D"),
                BlissVal::from_fixnum(6)
            ]
        )
        .expect("recursive directives"),
        make_lisp_string("4+5 / 6")
    );

    assert_eq!(
        format(
            NIL,
            "~[zero~;one~;two~] ~:[no~;yes~] ~@[kept=~D~] ~{~A~^, ~} ~D item~:P",
            &[
                BlissVal::from_fixnum(2),
                T,
                BlissVal::from_fixnum(9),
                list(&[
                    make_lisp_string("a"),
                    make_lisp_string("b"),
                    make_lisp_string("c")
                ]),
                BlissVal::from_fixnum(3)
            ]
        )
        .expect("conditionals and iteration"),
        make_lisp_string("two yes kept=9 a, b, c 3 items")
    );
}

#[test]
fn format_reports_errors_for_bad_directives_and_argument_types() {
    // Per R5.157, malformed control strings and mismatched argument types must fail.
    let unmatched =
        format(NIL, "~{~A", &[list(&[BlissVal::from_fixnum(1)])]).expect_err("unmatched");
    assert!(matches!(unmatched, BlissError::Internal(_)));

    let wrong_type = format(NIL, "~D", &[T]).expect_err("wrong type");
    assert!(matches!(wrong_type, BlissError::TypeError { .. }));
}

#[test]
fn printer_facing_apis_write_observable_output() {
    // Per R5.166/R5.179, printer-facing entrypoints must drive real stream output.
    let stream = make_string_output_stream(NIL).expect("string output stream");
    let items = list(&[BlissVal::from_fixnum(1), BlissVal::from_fixnum(2)]);
    pprint_logical_block(
        stream,
        items,
        Some("("),
        None,
        Some(")"),
        make_lisp_string("~A ~A"),
    )
    .expect("logical block");
    pprint_tab(TabKind::Line, 6, 1, stream).expect("tab");
    pprint_newline(NewlineKind::Mandatory, stream).expect("newline");
    let printed = get_output_stream_string(stream).expect("printed output");
    assert_eq!(heap_string(printed), "(1 2)      \n");
}

#[test]
fn parse_namestring_handles_physical_edges_and_tilde_expansion() {
    // Per R5.185/R5.186/R5.194, PARSE-NAMESTRING handles POSIX parsing and ~ expansion.
    let root = fixture_root("home");
    fs::create_dir_all(&root).expect("fixture root");
    let home = root.join("user-home");
    fs::create_dir_all(&home).expect("home dir");
    unsafe {
        std::env::set_var("HOME", &home);
    }

    let (root_pn, root_pos) = parse_namestring(pseudo_string("/"), None, None).expect("root parse");
    assert_eq!(root_pos, 1);
    assert_eq!(pathname_directory(root_pn), pseudo_string("/"));
    assert_eq!(pathname_name(root_pn), NIL);

    let (hidden, _) =
        parse_namestring(pseudo_string(".gitignore"), None, None).expect("hidden file");
    assert_eq!(pathname_name(hidden), pseudo_string(".gitignore"));
    assert_eq!(pathname_type(hidden), NIL);

    let (multi, _) =
        parse_namestring(pseudo_string("/tmp/foo.tar.gz"), None, None).expect("multi ext");
    assert_eq!(pathname_name(multi), pseudo_string("foo.tar"));
    assert_eq!(pathname_type(multi), pseudo_string("gz"));

    let tilde_path = "~/src/bliss.lisp";
    let (expanded, tilde_pos) =
        parse_namestring(pseudo_string(tilde_path), None, None).expect("tilde parse");
    assert_eq!(tilde_pos, tilde_path.len());
    assert_eq!(
        pathname_directory(expanded),
        pseudo_string(&format!("{}/src/", home.display()))
    );
    assert_eq!(pathname_name(expanded), pseudo_string("bliss"));
    assert_eq!(pathname_type(expanded), pseudo_string("lisp"));
}

#[test]
fn namestring_round_trips_for_physical_and_logical_pathnames() {
    // Per R5.182/R5.187, logical components canonicalize to uppercase and namestring round-trips.
    let (physical, _) = parse_namestring(pseudo_string("/var/tmp/.cache/archive.tar"), None, None)
        .expect("physical");
    assert_reparse_same(physical);

    let logical = make_pathname(
        pseudo_string("sys"),
        NIL,
        pseudo_string("src;modules;"),
        pseudo_string("core"),
        pseudo_string("lisp"),
        keyword("NEWEST"),
    )
    .expect("logical pathname");
    let rendered = namestring(logical).expect("logical namestring");
    let (reparsed, _) = parse_namestring(rendered, None, None).expect("logical reparse");
    assert_eq!(pathname_host(reparsed), pseudo_string("SYS"));
    assert_eq!(pathname_directory(reparsed), pseudo_string("SRC;MODULES;"));
    assert_eq!(pathname_name(reparsed), pseudo_string("CORE"));
    assert_eq!(pathname_type(reparsed), pseudo_string("LISP"));
}

#[test]
fn merge_pathnames_applies_defaulting_and_relative_directory_rules() {
    // Per R5.184/R5.191, MAKE-PATHNAME and MERGE-PATHNAMES preserve explicit parts
    // and fill NIL components from defaults, including relative-directory merging.
    let primary = make_pathname(
        NIL,
        NIL,
        pseudo_string("src/utils/"),
        pseudo_string("main"),
        NIL,
        NIL,
    )
    .expect("primary pathname");
    let defaults = make_pathname(
        pseudo_string("host"),
        pseudo_string("disk0"),
        pseudo_string("/workspace/project/"),
        pseudo_string("default"),
        pseudo_string("lisp"),
        keyword("NEWEST"),
    )
    .expect("default pathname");
    let merged = merge_pathnames(primary, defaults, keyword("NEWEST")).expect("merge");
    assert_eq!(pathname_host(merged), pseudo_string("host"));
    assert_eq!(pathname_name(merged), pseudo_string("main"));
    assert_eq!(pathname_type(merged), pseudo_string("lisp"));
    assert_ne!(pathname_directory(merged), NIL);
}

#[test]
fn wildcard_matching_and_wild_pathname_detection_cover_directory_name_type_and_version() {
    // Per R5.192/R5.202, wildcard detection and matching include directory wildcards,
    // name/type globs, and version wildcards.
    let (pathname, _) = parse_namestring(pseudo_string("/workspace/src/lib/core.lisp"), None, None)
        .expect("pathname");
    let wildcard = make_pathname(
        NIL,
        NIL,
        pseudo_string("/workspace/**/"),
        pseudo_string("c*e"),
        pseudo_string("*"),
        keyword("WILD"),
    )
    .expect("wildcard pathname");
    assert!(pathname_match_p(pathname, wildcard).expect("match"));
    assert!(wild_pathname_p(wildcard, None));
    assert!(wild_pathname_p(wildcard, Some(keyword("DIRECTORY"))));
    assert!(wild_pathname_p(wildcard, Some(keyword("NAME"))));
    assert!(wild_pathname_p(wildcard, Some(keyword("TYPE"))));
    assert!(wild_pathname_p(wildcard, Some(keyword("VERSION"))));
    assert!(!wild_pathname_p(wildcard, Some(keyword("HOST"))));
}

#[test]
fn logical_pathname_translations_are_setfable_and_translate_matching_sources() {
    // Per R5.189/R5.190/R5.193, logical translations are stored per host and
    // matched via wildcard transfer into the target pattern.
    let target_root = fixture_root("logical");
    fs::create_dir_all(target_root.join("src/pkg")).expect("logical target root");
    let host = "SYS";
    let translations = pseudo_string(&format!(
        "((\"SRC;*.*\" \"{}/src/*.*\"))",
        target_root.display()
    ));
    set_logical_pathname_translations(host, translations).expect("set translations");
    assert_eq!(
        logical_pathname_translations(host).expect("get translations"),
        translations
    );

    let (logical, _) =
        parse_namestring(pseudo_string("SYS:SRC;PKG.LISP"), None, None).expect("logical parse");
    let translated = translate_logical_pathname(logical).expect("logical translation");
    assert_eq!(pathname_name(translated), pseudo_string("PKG"));
    assert_eq!(pathname_type(translated), pseudo_string("LISP"));
    assert!(pathname_directory(translated) != NIL);
}

#[test]
fn filesystem_pathname_entrypoints_create_probe_and_resolve_truenames() {
    // Per R5.196/R5.197/R5.199, filesystem entrypoints exercise real OS effects.
    let root = fixture_root("fs");
    let file_path = root.join("a/b/example.lisp");
    let file_spec = pseudo_string(&file_path.to_string_lossy());

    let (ensured, created) = ensure_directories_exist(file_spec).expect("ensure dirs");
    assert_eq!(ensured, file_spec);
    assert!(created);

    fs::write(&file_path, "(print :ok)\n").expect("create file");
    let probed = probe_file(file_spec).expect("probe existing file");
    assert!(probed.is_some());
    let true_name = truename(file_spec).expect("truename");
    let rendered_true = namestring(true_name).expect("truename namestring");
    let (roundtrip_true, _) =
        parse_namestring(rendered_true, None, None).expect("reparse truename");
    assert_eq!(pathname_name(roundtrip_true), pseudo_string("example"));
    assert_eq!(pathname_type(roundtrip_true), pseudo_string("lisp"));

    let missing = root.join("missing.lisp");
    let missing_spec = pseudo_string(&missing.to_string_lossy());
    assert_eq!(probe_file(missing_spec).expect("probe missing"), None);
    let missing_err = truename(missing_spec).expect_err("truename missing");
    assert!(matches!(missing_err, BlissError::FileError(_)));
}

#[test]
fn directory_handles_wild_inferiors_and_pathnames_are_safe_to_read_concurrently() {
    // Per R5.198, DIRECTORY accepts wild pathnames including :WILD-INFERIORS.
    // Per R5.200, constructed pathname objects remain safe for concurrent reads.
    let root = fixture_root("dir");
    fs::create_dir_all(root.join("one/two")).expect("nested dirs");
    fs::write(root.join("top.lisp"), "").expect("top file");
    fs::write(root.join("one/nested.lisp"), "").expect("nested file");
    fs::write(root.join("one/two/deep.lisp"), "").expect("deep file");

    let wildcard = pseudo_string(&format!("{}/**/*.lisp", root.display()));
    let listed = directory(wildcard).expect("directory wildcard listing");
    assert_eq!(listed.len(), 3);

    let shared = listed[0];
    let readers: Vec<_> = (0..4)
        .map(|_| {
            thread::spawn(move || {
                for _ in 0..10 {
                    let _ = pathname_name(shared);
                    let _ = pathname_type(shared);
                    let _ = namestring(shared).expect("read namestring");
                }
            })
        })
        .collect();
    for reader in readers {
        reader.join().expect("reader thread");
    }
}
