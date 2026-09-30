# Runtime architecture

EGCL's bootstrap is a Rust workspace. The runtime crate is the foundation;
the compiler and standard library build on it, and the command-line crate joins
those pieces into a runnable Lisp system.

## Where behavior belongs

`egcl-rt` owns the mechanisms that make Lisp values and execution possible:
allocation, object representation, roots, safepoints, threads, and native code
memory. `egcl-compiler` owns reusable compilation machinery. `egcl-stdlib`
owns Common Lisp library behavior. `egcl` integrates them with parsing of
command-line options, loading, evaluation, and tier dispatch.

The large evaluator is an integration layer, not an invitation to duplicate
standard-library operations. Two independent implementations of streams or
sequences can diverge in representation and behavior even when their public
names agree. Delegating to the stdlib keeps one owner for those semantics.

## Tiering crosses crate boundaries

The user-visible [execution model](../../user/explanation/execution.md) describes
T0, T1, T2, OSR, and deoptimization. In the source, bytecode integration and
native-to-interpreter bridges live beside the evaluator, while the optimized
SSA pipeline lives in the compiler crate. Architecture emitters supply the
machine-specific part of that pipeline.

A compiled activation also participates in GC. Before a runtime call that can
allocate or a safepoint that can collect, live Lisp references must be visible
in collector-scanned locations. After collection, native execution must use the
updated references. Deoptimization has the additional job of reconstructing
logical Lisp state from optimized values and reconstruction recipes.

## Specification and implementation

The specification includes the intended self-hosting system and future stages.
The bootstrap is the implementation users can run now. A normative design
requirement describes what must eventually hold at its assigned stage; it does
not establish that an interface is currently callable.

The manual keeps task-oriented instructions grounded in the implementation.
Detailed internal design records remain in `docs/design/`, where proposals and
historical investigations can retain their context without presenting them as
current user guarantees.
