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

    // ── owning a Python reference ────────────────────────────
    // The whole reason releases are queued: Py_DECREF can run `__del__`, i.e.
    // arbitrary Python, so it must not fire from wherever a Lisp proxy happened to
    // become unreachable. This checks the deferral is real rather than decorative —
    // the object stays alive with its last Python name deleted and its Rust owner
    // dropped, and dies only at the drain.
    {
        let scope = PythonScope::enter().expect("an interpreter");
        scope
            .run(
                "died = False\n\
                 class Probe:\n\
                 \x20   def __del__(self):\n\
                 \x20       global died\n\
                 \x20       died = True\n\
                 probe = Probe()\n",
            )
            .expect("a probe whose death is observable");

        let owned = scope.lookup("probe").expect("the probe object");
        assert!(scope.lookup("no_such_name").is_none(), "an unbound name");

        // Python's own reference goes away; ours is what keeps it alive.
        scope.run("del probe").expect("dropping Python's name");
        scope
            .run("assert not died, 'the Rust-side reference kept it alive'")
            .expect("still alive");

        let before = python::pending_releases();
        drop(owned);
        assert_eq!(
            python::pending_releases(),
            before + 1,
            "dropping a PyRef must queue a release, not perform one"
        );
        scope
            .run("assert not died, '__del__ must not run at drop time'")
            .expect("still alive after the drop");

        // The drain is the safe point where arbitrary Python may run.
        assert_eq!(
            python::drain_releases(&scope).expect("a drain"),
            before + 1,
            "the drain releases everything queued"
        );
        assert_eq!(python::pending_releases(), 0, "the queue is empty after");
        scope
            .run("assert died, 'the drained release destroyed the object'")
            .expect("__del__ ran at the drain");

        // A second reference to a live object, and a drain with nothing to do.
        scope.run("kept = Probe()").expect("a second probe");
        let first = scope.lookup("kept").expect("the object");
        let second = first.duplicate(&scope).expect("a second owned reference");
        assert_eq!(first.as_ptr(), second.as_ptr(), "the same object");
        scope.run("died = False; del kept").expect("Python lets go");
        drop(first);
        python::drain_releases(&scope).expect("a drain");
        scope
            .run("assert not died, 'one of two references was released'")
            .expect("the duplicate still holds it");
        drop(second);
        python::drain_releases(&scope).expect("a drain");
        scope
            .run("assert died, 'the last reference was released'")
            .expect("both references were accounted for");
        assert_eq!(python::drain_releases(&scope).expect("a drain"), 0);
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

    // ── native fault recovery is disarmed while Python runs ──
    // TorCL rewrites a faulting instruction to a native recovery epilogue, and it
    // decides to from the FAULT ADDRESS ALONE — anything below one page is "a null
    // guard", with nothing looking at where the fault happened. So a null
    // dereference inside CPython is indistinguishable from one in compiled Lisp,
    // and with recovery armed it would unwind a Lisp frame that is not on top,
    // leaving CPython mid-operation with its reference counts wrong.
    //
    // Asserted here rather than by provoking a real fault because this is the
    // precise property: it must hold for the whole crossing, and be restored after.
    {
        torcl_rt::runtime::set_sigsegv_recovery_ips(0xAAAA_0000, 0xBBBB_0000);
        {
            let scope = PythonScope::enter().expect("a crossing");
            assert_eq!(
                torcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
                0,
                "null-guard recovery must be disarmed while Python runs"
            );
            assert_eq!(
                torcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
                0,
                "stack-guard recovery must be disarmed while Python runs"
            );
            // A nested crossing must not restore it on the way out of the inner one.
            {
                let _inner = PythonScope::enter().expect("a nested crossing");
                assert_eq!(
                    torcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
                    0
                );
            }
            assert_eq!(
                torcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
                0,
                "leaving a nested crossing must not rearm recovery"
            );
            scope.run("x = 1").expect("Python still runs");
        }
        assert_eq!(
            torcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
            0xAAAA_0000,
            "recovery must be restored on the way out"
        );
        assert_eq!(
            torcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
            0xBBBB_0000
        );
        torcl_rt::runtime::set_sigsegv_recovery_ips(0, 0);
    }

    // ── embedding claims no signals ──────────────────────────
    // Two runtimes must not both believe they own SIGINT. `Py_InitializeEx(0)` is
    // the whole of the arbitration: CPython installs no handlers, so TorCL's remain
    // the process's — and this asserts it from INSIDE Python, which is where a claim
    // would be visible.
    //
    // `getsignal` returning None is the interesting value: it means a handler IS
    // installed that Python did not install. So None proves both halves at once —
    // TorCL owns the signal, and CPython did not replace it. SIG_DFL would mean
    // nobody owns it (true of SIGCHLD, which neither runtime claims), and a callable
    // would mean CPython had taken it.
    //
    // The handlers have to be installed for the question to mean anything: a bare
    // test process is not the CLI and has none, so without this every signal reads
    // SIG_DFL and the test would pass while asserting nothing.
    {
        torcl_rt::install_signal_handlers().expect("TorCL's signal handlers");
        let scope = PythonScope::enter().expect("a crossing");
        scope
            .run(
                r#"
import signal
for _signal in (signal.SIGINT, signal.SIGTERM, signal.SIGSEGV):
    assert signal.getsignal(_signal) is None, _signal.name
assert signal.getsignal(signal.SIGCHLD) is signal.SIG_DFL
"#,
            )
            .expect("TorCL still owns the signals it installed");
    }

    // ── shutting down ────────────────────────────────────────
    // ── only the owner may shut it down ──────────────────────
    // Not a nicety: CPython does not check this, it dereferences a thread state
    // that may belong to an exited thread. A spawned thread initialising and then
    // exiting was enough to segfault a later shutdown from main, which is what
    // made startup explicit in the first place.
    let refused = std::thread::spawn(python::finalize)
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
