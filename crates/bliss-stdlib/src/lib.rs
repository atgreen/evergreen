//! `bliss-stdlib` — Bliss Common Lisp standard library.
//!
//! Packages & bootstrap, CLOS, condition system, Gray streams,
//! generic sequences, hash tables, FORMAT / pretty-printer,
//! pathnames, and developer tools (REPL, debugger, profiler, SWANK).

mod ansi_symbols;

// ── Package system & bootstrap ────────────────────────────────────
pub mod packages;

// ── CLOS ──────────────────────────────────────────────────────────
pub mod characters;
pub mod numbers;
pub mod clos;

// ── Condition system ──────────────────────────────────────────────
pub mod conditions;

// ── Streams ───────────────────────────────────────────────────────
pub mod streams;

// ── Sequences & hash tables ───────────────────────────────────────
pub mod hashtable;
pub mod sequences;

// ── FORMAT & pretty-printer ───────────────────────────────────────
pub mod format;

// ── Pathnames ─────────────────────────────────────────────────────
pub mod pathnames;
pub mod offheap_image;

// ── Developer tools ───────────────────────────────────────────────
pub mod devtools;

// ── Error types ──────────────────────────────────────────────────
pub mod error;

// ── Time / date arithmetic ───────────────────────────────────────
pub mod time;

// ── Re-exports for convenience ────────────────────────────────────
pub use clos::{
    MethodCombinationType, MethodQualifier, allocate_instance, bootstrap_clos, change_class,
    class_direct_subclasses, class_direct_superclasses, class_name, class_of, class_slots,
    effective_slots,
    compute_applicable_methods, compute_class_precedence_list, compute_effective_method,
    define_class, ensure_clos_bootstrapped, find_class, generic_function_name, initialize_instance,
    is_instance, is_structure_class, make_generic_function, make_instance, reinitialize_instance,
    bind_class_name, set_find_class, set_structure_class,
    set_method_specializers, set_slot_value, shared_initialize, shared_initialize_with_list,
    slot_boundp, slot_makunbound, slot_value,
};
pub use conditions::{
    HandlerBinding, RestartSpec, acquire_preallocated_storage_condition, clear_funcall_hook,
    compute_restarts, error_condition, find_restart, handler_bind, handler_bind_fn, handler_case,
    initialize_condition_runtime_support, install_runtime_init_hook, invoke_debugger,
    invoke_restart, invoke_restart_interactively, make_simple_error, make_type_error,
    release_preallocated_storage_condition, restart_bind, restart_bind_fn, set_break_on_signals,
    set_debugger_hook, set_funcall_hook, signal_condition,
    signal_storage_condition_for_runtime_error, warn_condition,
};
pub use devtools::{
    BreakpointId, DebugFrame, ProfilerEntry, ProfilerKind, ProfilerReport, ReplState, StepMode,
    TimeResult, WatchScope, WatchTarget, break_at, break_on_entry, check_breakpoint_at_location,
    check_breakpoint_for_function, check_watchpoint, complete_symbol, describe, disassemble,
    eval_in_frame, get_last_profiler_report, has_trace_hook, inspect, instrument_function_entry,
    instrument_function_exit, invoke_debugger_ui, is_traced, list_breakpoints, record_allocation,
    record_safepoint_pc, remove_breakpoint, repl_loop, room, start_allocation_profiler,
    start_instrumentation_profiler, start_profiler, stop_allocation_profiler,
    stop_instrumentation_profiler, stop_profiler, time_execution, trace_entry, trace_exit,
    trace_function, untrace_function, unwatch, walk_stack, watch,
};
pub use error::StdlibError;
pub use format::{
    NewlineKind, TabKind, copy_pprint_dispatch, format, formatter, pprint_dispatch, pprint_indent,
    pprint_logical_block, pprint_newline, pprint_tab, register_format_function,
    set_pprint_dispatch,
};
pub use hashtable::{
    HashTest, MakeHashTableOptions, Weakness, clrhash, gethash, hash_table_count,
    hash_table_entries, hash_table_p, hash_table_rehash_size, hash_table_rehash_threshold,
    hash_table_size, hash_table_test, make_hash_table, maphash, remhash, set_gethash, sxhash,
};
pub use packages::PackageRegistry;
pub use packages::{
    InternStatus, accessible_symbols, add_nickname, add_symbol, delete_package, export,
    external_symbols_of, find_package, find_present_symbol, find_symbol, import, intern,
    intern_present, is_external_symbol, is_package, list_all_packages, make_package, package_name,
    package_name_eq,
    package_nicknames, package_shadowing_symbols, package_use_list, present_symbols,
    rename_package, seed_ansi_symbols, shadow, shadowing_import,
    unexport, unintern, unuse_package, use_package, use_package_by_name,
};
pub use pathnames::{
    PathDirComp, delete_file, directory, ensure_directories_exist, is_logical_pathname,
    is_pathname, logical_pathname_from_string,
    logical_pathname_translations, make_pathname, merge_pathnames, namestring, parse_namestring,
    pathname_device, pathname_directory, pathname_directory_components, pathname_host,
    pathname_match_p, pathname_name, pathname_type, pathname_version, pathnames_equal, probe_file,
    register_string,
    registered_string, rename_file, set_logical_pathname_translations, translate_logical_pathname,
    translate_pathname, truename, wild_pathname_p,
};
pub use sequences::{
    result_type_is_bit_vector, result_type_is_string,
    adjust_complex_vector, aref, array_total_size, build_complex_vector, build_displaced_vector,
    build_filled_simple_vector, build_md_array, build_result_sequence, build_simple_vector,
    concatenate,
    copy_seq, count,
    cvec_adjustable, cvec_capacity, cvec_char_contents, cvec_displacement, cvec_element, cvec_is_bit,
    cvec_fill_pointer,
    cvec_has_fill_pointer,
    cvec_is_string, elt,
    find, is_complex_vector, length, map, nreverse,
    position, reduce, remove, reverse, set_aref, set_elt, set_fill_pointer, sort, stable_sort,
    string_char_at, string_char_count, string_set_char, subseq, substitute, vector_pop, vector_push,
    vector_push_extend,
};
pub use streams::GrayStream;
pub use streams::{
    ExternalFormat, StreamDirection, StreamElementType, close, file_length_fn, file_position,
    get_output_stream_string, input_stream_p, interactive_stream_p, is_byte_stream,
    make_broadcast_stream, make_concatenated_stream, make_echo_stream, make_lisp_string,
    make_lisp_string_fresh, make_stderr, make_stdin, make_stdout, make_string_input_stream,
    make_string_output_stream, make_synonym_stream, make_two_way_stream, open, open_stream_p,
    output_stream_p, set_file_position, set_file_position_to_end, socket_accept,
    socket_close_listener, socket_listen,
    socket_local_port, stream_advance_to_column, stream_clear_input, stream_clear_output,
    stream_element_type, stream_external_format, stream_finish_output, stream_force_output,
    stream_fresh_line, stream_line_column, stream_line_number, stream_listen, stream_peek_char,
    stream_raw_fd, stream_read_byte, stream_read_char, stream_read_char_no_hang, stream_read_line,
    stream_read_sequence, stream_start_line_p, stream_terpri, stream_unread_char,
    stream_wait_for_input, stream_write_byte, stream_write_char, stream_write_sequence,
    stream_write_string, synonym_stream_symbol, two_way_stream_input_stream,
    two_way_stream_output_stream,
};
