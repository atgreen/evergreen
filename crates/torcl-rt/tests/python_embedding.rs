//! End-to-end tests for the embedded CPython layer (spec §2.7.8).
//!
//! ONE TEST, DELIBERATELY. An embedded CPython is a process singleton that cannot
//! be restarted after shutdown, and `#[test]` functions share a process and run in
//! parallel — so several of them each starting and stopping an interpreter is not
//! a test suite, it is a race. Splitting this up produced exactly that:
//!
//!   Fatal Python error: _PyImport_Init: global import state already initialized
//!
//! and aborted the process. The lifecycle is therefore exercised in order, in one
//! test, with the assertions it would otherwise have been split across.
//!
//! Gated on the `python` feature because embedding needs a target whose loader can
//! open `libpython`, which in practice means glibc rather than the default static
//! musl.

#![cfg(feature = "python")]

use torcl_rt::python::{self, PythonScope};

#[test]
fn the_interpreter_lifecycle() {
    assert!(!python::is_initialized(), "nothing should be running yet");

    // ── crossing requires an interpreter ─────────────────────
    // enter() deliberately does not start one: CPython binds the interpreter to
    // its initialising thread, so a lazily-started interpreter owned by whichever
    // thread crossed first can never be shut down safely.
    assert!(
        PythonScope::enter().is_err(),
        "crossing without an interpreter must fail, not start one"
    );

    // ── starting, and crossing in ────────────────────────────
    python::initialize().expect("an interpreter");
    python::initialize().expect("starting twice is not an error");
    {
        let scope = PythonScope::enter().expect("a crossing");
        assert!(python::is_initialized());
        scope.run("x = 6 * 7").expect("a statement runs");
        scope
            .run("assert x == 42, 'arithmetic survived the boundary'")
            .expect("state persists within the interpreter");
    }

    // ── the scope is not the interpreter ─────────────────────
    // Leaving Python must not tear it down: crossing back in finds the same
    // interpreter, not a fresh one.
    {
        let scope = PythonScope::enter().expect("re-entry");
        scope
            .run("assert x == 42, 'state persists across crossings'")
            .expect("the same interpreter");
    }

    // ── nesting ──────────────────────────────────────────────
    // Lisp→Python→Lisp→Python is the ordinary shape once callbacks exist, and
    // PyGILState_Ensure is reentrant, so it must work before they do.
    {
        let outer = PythonScope::enter().expect("outer");
        outer.run("depth = 1").expect("outer statement");
        {
            let inner = PythonScope::enter().expect("inner");
            inner.run("depth = depth + 1").expect("inner statement");
        }
        outer
            .run("assert depth == 2, 'both crossings reached one interpreter'")
            .expect("state is shared");
    }

    // ── failures ─────────────────────────────────────────────
    {
        let scope = PythonScope::enter().expect("an interpreter");
        // Turning this into a Lisp condition with a traceback is a separate
        // concern (bliss-wq5tw); this layer must at least not report success.
        assert!(
            scope.run("raise ValueError('boom')").is_err(),
            "a raising statement must not look like success"
        );
        scope.run("y = 1").expect("still usable after an exception");
        assert!(
            scope.run("x = 1\0hidden").is_err(),
            "source with an embedded null is refused rather than truncated"
        );
    }

    // ── the transition does not leak ─────────────────────────
    // Each crossing acquires and releases both the foreign state and the GIL. If
    // either leaked, this would deadlock or strand the thread in Native state.
    for index in 0..500 {
        let scope = PythonScope::enter().expect("a crossing");
        scope
            .run(&format!("counter = {index}"))
            .expect("a statement");
    }
    {
        let scope = PythonScope::enter().expect("a final crossing");
        scope
            .run("assert counter == 499, 'every crossing ran'")
            .expect("all of them took effect");
    }

    // ── shutting down ────────────────────────────────────────
    // ── only the owner may shut it down ──────────────────────
    // Not a nicety: CPython does not check this, it dereferences a thread state
    // that may belong to an exited thread. A spawned thread initialising and then
    // exiting was enough to segfault a later shutdown from main, which is what
    // made startup explicit in the first place.
    let refused = std::thread::spawn(|| python::finalize())
        .join()
        .expect("the thread itself must survive");
    assert!(
        refused.is_err(),
        "a non-owning thread must be refused, not allowed to crash the process"
    );
    assert!(
        python::is_initialized(),
        "a refused shutdown must leave the interpreter running"
    );

    // ── shutting down ────────────────────────────────────────
    python::finalize().expect("clean shutdown by the owner");
    assert!(!python::is_initialized(), "shutdown must be observable");
    python::finalize().expect("finalizing twice is not an error");
}
/// Many threads crossing at once must reach one interpreter, and the GIL must
/// actually change hands.
///
/// This is where the startup GIL bug showed. `Py_InitializeEx` returns with the GIL
/// HELD, and nothing later releases it: `PyGILState_Ensure` on the same thread sees
/// it as already held, so the matching Release does not drop it either. A
/// single-threaded test cannot show that — the one thread keeps re-entering a GIL
/// it already owns — so every other thread simply blocked forever on its first
/// crossing.
#[test]
#[ignore = "process-singleton: starts CPython, so it cannot share a process with \
            the lifecycle test. Run with --ignored."]
fn many_threads_share_one_interpreter() {
    python::initialize().expect("an interpreter, owned by this thread");
    let threads: Vec<_> = (0..8)
        .map(|index| {
            std::thread::spawn(move || {
                let scope = PythonScope::enter().expect("a crossing");
                scope
                    .run(&format!("shared_{index} = {index}"))
                    .expect("ran");
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("no thread deadlocked or aborted");
    }
    let scope = PythonScope::enter().expect("a final crossing");
    scope
        .run("assert shared_7 == 7, 'every thread reached the same interpreter'")
        .expect("one interpreter, eight threads");
    drop(scope);
    python::finalize().expect("clean shutdown by the owner");
}
