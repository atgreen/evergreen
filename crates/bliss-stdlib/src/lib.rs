//! `bliss-stdlib` — Bliss Common Lisp standard library.
//!
//! Packages & bootstrap, CLOS, condition system, Gray streams,
//! generic sequences, hash tables, FORMAT / pretty-printer,
//! pathnames, and developer tools (REPL, debugger, profiler, SWANK).

// ── Package system & bootstrap ────────────────────────────────────
pub mod packages;

// ── CLOS ──────────────────────────────────────────────────────────
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

// ── Developer tools ───────────────────────────────────────────────
pub mod devtools;

// ── Error types ──────────────────────────────────────────────────
pub mod error;

// ── Re-exports for convenience ────────────────────────────────────
pub use clos::{
    MethodCombinationType, MethodQualifier, allocate_instance, bootstrap_clos, change_class,
    class_direct_subclasses, class_direct_superclasses, class_name, class_of, class_slots,
    compute_applicable_methods, compute_class_precedence_list, compute_effective_method,
    define_class, find_class, initialize_instance, make_generic_function, make_instance,
    reinitialize_instance, set_find_class, set_method_specializers, set_slot_value,
    shared_initialize, shared_initialize_with_list, slot_boundp, slot_makunbound, slot_value,
};
pub use conditions::{
    HandlerBinding, RestartSpec, clear_funcall_hook, compute_restarts, error_condition,
    find_restart, handler_bind, handler_bind_fn, handler_case, invoke_debugger, invoke_restart,
    invoke_restart_interactively, make_simple_error, make_type_error, restart_bind,
    restart_bind_fn, set_debugger_hook, set_funcall_hook, signal_condition, warn_condition,
};
pub use devtools::{
    BreakpointId, DebugFrame, ProfilerEntry, ProfilerKind, ProfilerReport, ReplState, StepMode,
    TimeResult, WatchScope, WatchTarget, break_at, break_on_entry, check_breakpoint_at_location,
    check_breakpoint_for_function, check_watchpoint, complete_symbol, describe, disassemble,
    eval_in_frame, get_last_profiler_report, has_trace_hook, inspect, instrument_function_entry,
    instrument_function_exit, invoke_debugger_ui, is_traced, list_breakpoints, record_allocation,
    record_safepoint_pc, remove_breakpoint, repl_loop, room, start_allocation_profiler,
    start_instrumentation_profiler, start_profiler, start_swank_server, stop_allocation_profiler,
    stop_instrumentation_profiler, stop_profiler, stop_swank_server, time_execution, trace_entry,
    trace_exit, trace_function, untrace_function, unwatch, walk_stack, watch,
};
pub use error::StdlibError;
pub use format::{
    NewlineKind, TabKind, copy_pprint_dispatch, format, formatter, pprint_dispatch, pprint_indent,
    pprint_logical_block, pprint_newline, pprint_tab, register_format_function,
    set_pprint_dispatch,
};
pub use hashtable::{
    HashTest, MakeHashTableOptions, Weakness, clrhash, gethash, hash_table_count,
    hash_table_rehash_size, hash_table_rehash_threshold, hash_table_size, hash_table_test,
    make_hash_table, maphash, remhash, set_gethash, sxhash,
};
pub use packages::PackageRegistry;
pub use packages::{
    InternStatus, export, find_symbol, import, intern, shadow, shadowing_import, unexport,
    unintern, unuse_package, use_package,
};
pub use pathnames::{
    delete_file, directory, ensure_directories_exist, logical_pathname_translations, make_pathname,
    merge_pathnames, namestring, parse_namestring, pathname_device, pathname_directory,
    pathname_host, pathname_match_p, pathname_name, pathname_type, pathname_version, probe_file,
    register_string, rename_file, set_logical_pathname_translations, translate_logical_pathname,
    truename, wild_pathname_p,
};
pub use sequences::{
    concatenate, copy_seq, count, elt, find, length, map, nreverse, position, reduce, remove,
    reverse, set_elt, sort, stable_sort, subseq, substitute,
};
pub use streams::GrayStream;
pub use streams::{
    ExternalFormat, StreamDirection, StreamElementType, close, file_length_fn, file_position,
    get_output_stream_string, input_stream_p, interactive_stream_p, make_broadcast_stream,
    make_concatenated_stream, make_echo_stream, make_lisp_string, make_lisp_string_fresh,
    make_string_input_stream, make_string_output_stream, make_synonym_stream, make_two_way_stream,
    open, open_stream_p, output_stream_p, set_file_position, stream_advance_to_column,
    stream_clear_input, stream_clear_output, stream_element_type, stream_external_format,
    stream_finish_output, stream_force_output, stream_fresh_line, stream_line_column,
    stream_line_number, stream_listen, stream_peek_char, stream_read_byte, stream_read_char,
    stream_read_char_no_hang, stream_read_line, stream_read_sequence, stream_start_line_p,
    stream_terpri, stream_unread_char, stream_write_byte, stream_write_char, stream_write_sequence,
    stream_write_string,
};
