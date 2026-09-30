# How Lisp runs

EGCL balances startup cost against the value of optimizing frequently executed
code. It does not require every function to pass through an optimizing compiler
before the program can run.

## Interpretation and compilation

The reader turns text into Lisp forms. Macro expansion and lowering turn
supported forms into bytecode. Some forms use the tree-walking evaluator as a
compatibility or bootstrap path. Bytecode executes in T0 while the runtime
collects information about calls and loop back edges.

| Tier | Role |
| --- | --- |
| T0 | Bytecode execution with low compilation cost |
| T1 | Baseline native code for supported bytecode shapes |
| T2 | Optimized native code using the SSA compiler pipeline |

A function that cannot be compiled by a target backend can continue at a lower
tier. Native support is target-specific; see the
[platform table](../reference/platforms.md). A function's semantics should not
depend on whether it is interpreted or compiled.

## A hot function is different from a hot loop

A function called repeatedly may have its entry point replaced by native code.
A long-running loop may instead enter compiled code during the current call.
That transition is **on-stack replacement**, or OSR. EGCL transfers live values
to the compiled loop's expected locations and continues the same computation.

This distinction matters when observing performance. A function can still be
reported as T0 by `function-tier` or `disassemble` while its active loop runs
native code through OSR. An installed function tier is not a trace of every
instruction executed during a call.

## Why optimized code can exit

T2 may optimize under assumptions, such as an operand being a fixnum. Guards
check those assumptions. When a guard fails, deoptimization reconstructs the
state needed to resume less-specialized execution. It must preserve effects
already performed, values still live, and the correct resumption position.

The same boundary interacts with the garbage collector: objects may move, so
native registers and stack locations must agree with the collector's root
metadata. The [runtime architecture](../../contributing/explanation/architecture.md)
explains where these responsibilities live in the source tree.
