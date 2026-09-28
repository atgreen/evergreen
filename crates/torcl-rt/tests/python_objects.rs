//! The Lisp↔Python object model: conversions, calls, and proxy lifetime
//! (bliss-dk3nr).
//!
//! Its own test binary, and ONE test inside it, for the reason `python_embedding.rs`
//! explains at length: an embedded CPython is a process singleton that cannot be
//! restarted, so two tests that each start one cannot share a process. Separate
//! files are separate processes, which is why this can exist at all.

#![cfg(feature = "python")]

use torcl_rt::python::{self, PythonScope};
use torcl_rt::value::{NIL, T, TorclVal};
use torcl_rt::{Collector, GcConfig, HeapCollector, ShadowRootScope, init_heap};

fn config() -> GcConfig {
    GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 16 * 1024 * 1024,
        nursery_size: 256 * 1024,
        tlab_size: 4 * 1024,
        region_size: 64 * 1024,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 256,
        old_occupancy_trigger: 0.5,
    }
}

/// Stand-in for the stdlib's `install_gc_hooks`, which a torcl-rt test cannot
/// reach. The real dispatch switches on `type_id` exactly like this.
fn finalizer_dispatch(_finalizer: TorclVal, object: TorclVal) {
    let body = object.to_raw() as *mut u8;
    // SAFETY: the collector passes the body address of an object it has determined
    // to be unreachable; this test registers finalizers only for proxies.
    unsafe {
        let header = &*(body.sub(8) as *const torcl_rt::object::ObjectHeader);
        if header.type_id() == torcl_rt::object::type_id::PYTHON_OBJECT {
            python::finalize_proxy(body);
        }
    }
}

#[test]
fn the_object_model() {
    init_heap(&config()).expect("init_heap");
    torcl_rt::gc::set_finalizer_dispatch(finalizer_dispatch);
    python::initialize().expect("an interpreter");
    let scope = PythonScope::enter().expect("a crossing");

    // ── importing, and reaching into modules ─────────────────
    scope.import("math").expect("a module that exists");
    let missing = scope.import("definitely_not_a_module_47");
    assert!(missing.is_err(), "a missing module is an error");
    let message = format!("{}", missing.unwrap_err());
    assert!(
        message.contains("ModuleNotFoundError") || message.contains("No module"),
        "the error should say what Python said, got: {message}"
    );
    // A failed import must not leave its exception set for the next call to
    // inherit — the most confusing possible failure mode.
    scope.import("math").expect("still usable after a failure");

    // Which dotted segments are modules is not knowable from the name, so both
    // shapes have to work: a function in a module, and a submodule.
    scope.resolve("math.sqrt").expect("a function in a module");
    scope
        .resolve("os.path.join")
        .expect("a function in a submodule");
    assert!(
        scope.resolve("math.no_such_attribute").is_err(),
        "a real module with an unreal attribute"
    );

    // ── calling, and the value policy ────────────────────────
    let sqrt = scope.resolve("math.sqrt").expect("math.sqrt");
    let nine = scope.to_python(TorclVal::from_fixnum(9)).expect("9");
    let three = scope
        .from_python(scope.call(&sqrt, vec![nine]).expect("sqrt(9)"))
        .expect("a Lisp value");
    assert!(three.is_double_float(), "a Python float is a DOUBLE-FLOAT");
    assert_eq!(three.as_double_float(), 3.0);

    // Integers round-trip as integers, not floats: Python's int and float are
    // distinct and the mapping must not blur them.
    let identity = scope.resolve("builtins.int").expect("int");
    let seven = scope
        .from_python(
            scope
                .call(
                    &identity,
                    vec![scope.to_python(TorclVal::from_fixnum(7)).unwrap()],
                )
                .expect("int(7)"),
        )
        .expect("a Lisp value");
    assert!(seven.is_fixnum() && seven.as_fixnum() == 7);

    // Strings are COPIED, and the copy has to survive arbitrary code points.
    let upper = scope.resolve("str.upper").expect("str.upper");
    let greeting = torcl_rt::gc::alloc_character_string("héllo ☃");
    let shouted = scope
        .from_python(
            scope
                .call(&upper, vec![scope.to_python(greeting).expect("a str")])
                .expect("upper()"),
        )
        .expect("a Lisp value");
    assert!(shouted.is_string());
    assert_eq!(shouted.as_string(), "HÉLLO ☃");

    // NIL is None outbound; None and False are both NIL inbound. The ambiguity is
    // Lisp's one false value, and this is the only way it resolves.
    let repr = scope.resolve("builtins.repr").expect("repr");
    let of_nil = scope
        .from_python(
            scope
                .call(&repr, vec![scope.to_python(NIL).unwrap()])
                .unwrap(),
        )
        .unwrap();
    assert_eq!(of_nil.as_string(), "None", "NIL crosses as None");
    let of_t = scope
        .from_python(
            scope
                .call(&repr, vec![scope.to_python(T).unwrap()])
                .unwrap(),
        )
        .unwrap();
    assert_eq!(of_t.as_string(), "True", "T crosses as True");

    scope
        .run("true_value = True; false_value = False; none_value = None")
        .unwrap();
    for (name, expected) in [("true_value", T), ("false_value", NIL), ("none_value", NIL)] {
        let value = scope
            .from_python(scope.lookup(name).expect(name))
            .expect("a Lisp value");
        assert_eq!(value, expected, "{name} should arrive as {expected:?}");
    }
    // Booleans must be tested before integers: Python's bool IS an int subclass,
    // so an integer-first classification turns True into 1.
    assert_ne!(
        scope
            .from_python(scope.lookup("true_value").unwrap())
            .unwrap(),
        TorclVal::from_fixnum(1),
        "True must not arrive as the integer 1"
    );

    // An integer too large for a fixnum stays a Python object rather than being
    // truncated or turned into a float — visible and exact.
    scope.run("big = 2 ** 200").unwrap();
    let big = scope.from_python(scope.lookup("big").unwrap()).unwrap();
    assert!(
        python::is_proxy(big),
        "an integer beyond a fixnum stays a PYTHON-OBJECT"
    );

    // ── attributes, methods, types ───────────────────────────
    scope
        .run("class Point:\n    def __init__(self):\n        self.x = 3\n    def scale(self, k):\n        return self.x * k\np = Point()\n")
        .expect("a class with state and a method");
    let point = scope.lookup("p").expect("p");
    assert_eq!(
        scope
            .from_python(scope.getattr(&point, "x").unwrap())
            .unwrap(),
        TorclVal::from_fixnum(3)
    );
    scope
        .setattr(
            &point,
            "x",
            &scope.to_python(TorclVal::from_fixnum(10)).unwrap(),
        )
        .expect("setting an attribute");
    scope
        .run("assert p.x == 10, 'the attribute was set'")
        .unwrap();
    assert_eq!(
        scope
            .from_python(
                scope
                    .call_method(
                        &point,
                        "scale",
                        vec![scope.to_python(TorclVal::from_fixnum(4)).unwrap()]
                    )
                    .unwrap()
            )
            .unwrap(),
        TorclVal::from_fixnum(40),
        "a method call sees the object's current state"
    );
    assert!(
        scope.getattr(&point, "nope").is_err(),
        "a missing attribute is an error"
    );

    let kind = scope.type_of(&point).expect("the type");
    assert_eq!(scope.display(&kind).unwrap(), "<class '__main__.Point'>");
    assert!(scope.is_instance(&point, "__main__.Point").unwrap());
    assert!(!scope.is_instance(&point, "builtins.dict").unwrap());
    assert!(scope.represent(&point).unwrap().contains("Point"));

    // ── an error reports what Python said ────────────────────
    let raiser = scope.resolve("builtins.int").expect("int");
    let bad = scope.call(
        &raiser,
        vec![
            scope
                .to_python(torcl_rt::gc::alloc_character_string("not a number"))
                .unwrap(),
        ],
    );
    let message = format!("{}", bad.expect_err("int('not a number') raises"));
    assert!(
        message.contains("ValueError"),
        "the exception's type should be named, got: {message}"
    );
    assert!(
        message.contains("not a number"),
        "the exception's message should survive, got: {message}"
    );
    // And the failure must not poison the next call.
    scope.resolve("math.sqrt").expect("still usable");

    // ── a proxy owes CPython a reference, and pays it ─────────
    // The whole chain: a proxy becomes unreachable, the collector's destructor
    // QUEUES the release rather than performing it (it runs under the heap lock,
    // where __del__ must not), and the next drain performs it.
    scope
        .run("released = False\nclass Owned:\n    def __del__(self):\n        global released\n        released = True\nowned = Owned()\n")
        .expect("an object whose release is observable");
    let proxy = scope
        .proxy(scope.lookup("owned").expect("the object"))
        .expect("a PYTHON-OBJECT");
    assert!(python::is_proxy(proxy));

    // Root it before anything else can collect: the proxy is a Rust local, and a
    // precise collector cannot see one. Rooting is also what makes the negative
    // assertion below meaningful — letting a local go out of scope would prove
    // nothing about reachability, because it was never reachable to begin with.
    let roots = ShadowRootScope::new();
    let rooted = roots.root(proxy);

    // Sweep the proxies earlier parts of this test left unreachable (the 2**200
    // integer among them), so the count below is about THIS proxy and not about
    // whatever else was outstanding.
    HeapCollector::new()
        .full_gc()
        .expect("sweeping earlier garbage");
    python::drain_releases(&scope).expect("draining earlier garbage");
    let before = python::pending_releases();
    assert_eq!(before, 0, "the queue starts empty");

    // Python lets go; the proxy is now the only thing holding it.
    scope.run("del owned").unwrap();
    scope
        .run("assert not released, 'the proxy holds the last reference'")
        .unwrap();
    HeapCollector::new()
        .full_gc()
        .expect("full_gc while rooted");
    assert_eq!(
        python::pending_releases(),
        before,
        "a reachable proxy must not have been finalized"
    );
    assert!(
        python::is_proxy(rooted.get()),
        "and it is still a proxy at its (possibly new) address"
    );
    scope
        .run("assert not released, 'a reachable proxy still holds its reference'")
        .unwrap();

    // Now make it unreachable and collect again.
    drop(roots);
    HeapCollector::new().full_gc().expect("full_gc");
    assert_eq!(
        python::pending_releases(),
        before + 1,
        "the collector's destructor must queue the release"
    );
    scope
        .run("assert not released, '__del__ must not run inside a collection'")
        .expect("nothing ran in the GC pause");
    assert_eq!(python::drain_releases(&scope).unwrap(), before + 1);
    scope
        .run("assert released, 'the drain released the proxy reference'")
        .expect("__del__ ran at the drain");

    // ── a proxy round-trips ──────────────────────────────────
    // Going back out, a proxy is the object it proxies, not a description of it.
    scope.run("marker = object()").unwrap();
    let round = scope.proxy(scope.lookup("marker").unwrap()).unwrap();
    let same = scope.resolve("builtins.id").unwrap();
    let identity_out = scope
        .from_python(
            scope
                .call(&same, vec![scope.to_python(round).unwrap()])
                .unwrap(),
        )
        .unwrap();
    scope.run("expected = id(marker)").unwrap();
    let expected = scope
        .from_python(scope.lookup("expected").unwrap())
        .unwrap();
    assert_eq!(
        identity_out, expected,
        "a proxy crosses back as the same object"
    );

    drop(scope);
    python::finalize().expect("clean shutdown");
}
