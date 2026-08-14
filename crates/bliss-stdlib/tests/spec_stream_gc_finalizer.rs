//! bliss-jtc.7a: a file stream that becomes unreachable without an explicit
//! `close` has its file descriptor released by the GC finalizer, and composite
//! streams survive the GC trace/finalize path (their components are traced
//! through the off-heap block).
//!
//! These tests exercise `full_gc` on the process-global heap and install the
//! real stdlib GC hooks (finalizer dispatch + stream tracing), so they are
//! serialized on a file-local lock like the other GC tests in the tree.

use bliss_rt::value::{NIL, T};
use bliss_stdlib::streams::{install_gc_hooks, IF_EXISTS_SUPERSEDE_VAL};
use bliss_stdlib::{
    make_broadcast_stream, make_lisp_string, make_string_output_stream, open, register_string,
    stream_write_string, ExternalFormat, StreamDirection,
};
use std::sync::{Mutex, OnceLock};

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// Count the process's open file descriptors via /proc/self/fd.
fn open_fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|it| it.count())
        .unwrap_or(0)
}

#[test]
fn file_stream_fd_released_by_gc_finalizer() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    install_gc_hooks();
    // (The R5.121 unclosed-stream warnings print to stderr during the churn;
    // we assert on fd counts, not output.)
    let dir = std::env::temp_dir();
    let path = dir.join("bliss_7a_finalizer_probe.txt");
    let path_str = path.to_string_lossy().into_owned();

    let baseline = open_fd_count();

    // Open many output file streams and drop each WITHOUT closing. Each holds an
    // open fd inside its off-heap block until the GC finalizer drops that block.
    for i in 0..80 {
        let p = make_lisp_string(&path_str); // fresh each iteration (GC may move it)
        register_string(p, &path_str); // so extract_path_string resolves it
        let s = open(
            p,
            StreamDirection::Output,
            NIL,
            IF_EXISTS_SUPERSEDE_VAL,
            T,
            ExternalFormat::Utf8,
        )
        .expect("open output file stream");
        let _ = stream_write_string(s, make_lisp_string("x"), 0, None);
        drop(s); // unreachable, unclosed
        if i % 20 == 19 {
            bliss_rt::full_gc().expect("full_gc");
        }
    }
    // Final collections: every dead stream handle's finalizer must run and close
    // its fd. If finalization were broken, ~80 fds would remain open.
    bliss_rt::full_gc().expect("full_gc");
    bliss_rt::full_gc().expect("full_gc");

    let after = open_fd_count();
    assert!(
        after <= baseline + 8,
        "file-stream fds leaked despite GC finalization: baseline={baseline}, after={after}"
    );
}

#[test]
fn composite_stream_survives_gc_trace_and_finalize() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    install_gc_hooks();

    // Create and use broadcast streams over string-output components, then let
    // them go unreachable and collect. This drives the STREAM trace hook (which
    // visits the composite's components in the off-heap block) and the finalizer
    // over composite handles — it must neither miss references nor crash.
    for _ in 0..50 {
        let a = make_string_output_stream(NIL).unwrap();
        let b = make_string_output_stream(NIL).unwrap();
        let bc = make_broadcast_stream(&[a, b]).unwrap();
        stream_write_string(bc, make_lisp_string("hello"), 0, None).unwrap();
        bliss_rt::full_gc().expect("full_gc");
    }
    bliss_rt::full_gc().expect("full_gc");
}
