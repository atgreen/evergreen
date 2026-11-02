# Execution-owned ordinary call linkage

`EGCL_R13_CALL_LINKAGE=1` selects an experimental x86-64 Linux lookup for
ordinary call descriptors, revisions and invocation heat. The default remains
the existing absolute-slot emission. Set the control before starting EGCL.

Generated code resolves a process-wide call ordinal through the execution
context in R13. It then applies the existing descriptor, arity, environment,
revision, stack and heat checks. A missing view, out-of-range ordinal or empty
slot takes the existing cold dispatcher with rooted actual arguments. Cold
requests derive their slot addresses from that same execution, with zero
addresses when unresolved.

## Ownership and publication

Each `LispExecutionContext` owns one `ExecutionCallLinkage`. A single atomic
pointer publishes an immutable view containing both the backing pointer and
its capacity. Backing entries are atomic cell pointers. Publication uses
release ordering; the x86-64 selector's ordinary loads provide acquire reads.
Old views and backing arrays remain alive until the context retires after its
last native frame. Geometric growth bounds retained backing space.

The directory retains each stable `Arc<CallCell>`. The execution's existing
`SLOTS` and `State` scanner retain callable values, adapters and code. Those
owners outlive every native frame. A published ordinal never changes cell
identity; an old view may miss a newly populated slot and safely go cold.

Directory installation uses an ordered execution-object lock (level 10),
with a stable instance suborder. It performs Rust storage allocation only:
no Lisp allocation, poll, evaluation or nested runtime lock occurs while the
guard is held. Generated selection takes no lock and cannot collect or yield.
The selected cell remains in R10 across descriptor/revision/stack checks;
the cold path resolves independently.

## Validation and limits

Focused checks exercise one generated selector against distinct execution
contexts, empty and growing directories, and a fiber forced onto another
carrier. An emitted-address inventory checks both hot admission and cold
requests for embedded descriptor, revision and heat-slot addresses. Runtime
validation also covers allocating adapters, actual relocation, suspension,
redefinition, arity and multiple values.

The predeclared release comparison uses five fresh-process samples per mode
with balanced ordering, twenty million warmed calls for fast call cohorts and
two million for an escaping private-array allocation cohort. Reporting gates are
3% CPU overhead for lean named calls, 10% for descriptor/private-primitive
calls and 15% for instructions where measurement is available. A missed gate
does not justify changing the default or weakening correctness checks.

This does not make a compiled artifact shareable. Installation-owned adapters,
FUNCALL probes, other installation metadata and code owners remain
outside this slice. Their lifetime and binding work is tracked by
`bliss-gg3i1.7.15.10.6`; this lookup prerequisite is
`bliss-gg3i1.7.15.10.6.1`.
