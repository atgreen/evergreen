# Why CLOS-heavy loads tree-walk, and what it costs

## The question this answers

Profiling `(asdf:load-system :babel)` kept showing the same inclusive chain at
65–77% of the run:

    invoke_generic_function -> invoke_method -> eval_progn -> eval_form -> eval_list

The obvious reading — "ASDF's functions are being tree-walked" — sat unresolved
for a long time because it seemed impossible: the T0→T1 threshold is only 10
invocations, and ASDF's hot generic functions are called hundreds of times per
load. Something called 375 times should be native.

The answer is that **method bodies are not on the function tier ladder at all.**

## How a method body chooses a tier

`invoke_method` (cli.rs) has exactly two paths:

```rust
if let Some(callable) = METHOD_COMPILED.with(...) {
    return apply_function(callable, args, env);   // tiered: bytecode -> T1 -> T2
}
... eval_progn(method.body, env)                  // tree-walked, forever
```

`METHOD_COMPILED` is populated **once, at `DEFMETHOD` time**, and only for
methods that pass every eligibility test. A method that fails any of them takes
the `eval_progn` branch on every single invocation for the life of the image.
There is no invocation counter on that branch and no promotion out of it —
being hot cannot rescue it.

## Measured: the exclusions land on the hot path

Instrumenting both branches over a babel load (cold + 10 no-op reloads):

    invocations=118153  compiled=19552 (16.5%)  tree_walked=98601 (83.5%)

    tree-walked invocations by reason
      58737  (59.6%)  &key-in-lambda-list
      25225  (25.6%)  call-next-method
      13139  (13.3%)  compiler-bailed
       1500  ( 1.5%)  nested-in-binding

Statically, only 28% of ASDF's 243 `defmethod`s are disqualified. Dynamically
they absorb **83.5% of invocations**. The disqualifiers are not a random
sample — they are a precise description of the ASDF protocol, which is built on
`&key` and `:around` + `call-next-method`. `FIND-COMPONENT` alone is ~56,000
invocations, 57% of all tree-walking, blocked purely by `&key`.

This is the whole of the profile chain above. It is not a slow tree-walker; it
is code that never left the tree-walker.

## `&key` was conservatism, not a limit (bliss-5rcy, fixed)

An ordinary `DEFUN` with `&key` compiles and promotes to T1 native. The genuine
obstacle was CLHS 7.6.5: keyword validation for a generic call is against the
union of **all applicable methods'** keywords, so a method must tolerate
keywords it does not itself name. The tree-walked binder gets this by passing
`allow_other_keys = true` (`bind_method_params`, bliss-lb6.14); the ordinary
function binder used by compiled bodies has no such flag, so simply deleting
the bail fails at once with `unexpected keyword argument: VERBOSE`.

`method_lambda_list_for_compile` writes the permission into the lambda list
instead, appending `&ALLOW-OTHER-KEYS`. Result:

                cold     reload x10   user
      before    7.75s      19.49s     29.27s
      after     7.55s      17.30s     26.89s
                -2.6%      -11.3%      -8.2%

    tier split  16.5% -> 51.5% compiled   (-> 61.9% after bliss-ljmj)

## The ladder is inverted for methods (bliss-d9ak)

Eager compilation at `DEFMETHOD` is itself a measured cost. With it disabled
(`TORCL_NO_METHOD_COMPILE=1`), cold load was **0.87s (11%) faster** and reload
was 0.7% *better*, 3 reps out of 3 — torcl pays to compile ~243 method bodies
whether or not they are ever invoked, and before this fix the ones that ran hot
were excluded anyway.

Functions get a hotness gate (`TORCL_T0_T1_THRESHOLD`, default 10); methods get
compiled unconditionally at definition and then, if ineligible, never promoted.
Both halves are backwards. HotSpot never compiles a method it has not seen
execute; SBCL sidesteps the question by compiling AOT at file-compile time
rather than at load time.

## Remaining, in measured order

- ~~**bliss-ccso**~~ (done) — `call-next-method` / `next-method-p` bodies could
  not tier up at all. Both are special forms reading `env.method_context`, so
  there was no callable spelling to lower to; a compiled body already runs
  against that same `env`, so it only needed evaluated-argument entry points
  (`TORCL::%CALL-NEXT-METHOD`, `TORCL::%NEXT-METHOD-P`), the context published
  around the compiled path, and `mv_operator_preserves` taught the new spelling
  so the next method's secondary values survive. Tier split 61.9% -> 74.8%
  compiled, tree-walked invocations 27,550 -> 18,172.

  **The babel load did not move** (-0.5% instructions, within noise). The
  offenders here are `:around` methods that merely delegate, so their bodies are
  almost entirely the `call-next-method` itself. The payoff is for methods with
  real bodies, which previously could never leave the tree-walker: 300k calls of
  one went 41.17s -> 3.11s (**13.2x**). Worth recording as the general lesson —
  a tier-up cliff can be 63% of invocations and still be worth ~nothing on a
  given workload, because what matters is how much work sits *behind* the
  cliff.
- ~~**bliss-ljmj**~~ (done) — `SETF` through a *function-named* accessor place
  (a struct accessor or a CLOS `:accessor`) bailed the lowerer with `form:SETF`,
  keeping the whole enclosing function tree-walked; `gethash`, `slot-value`,
  `aref`, `car`, specials and locals all compiled, only the accessor call did
  not. It was also the cause of **bliss-o4cp** — `ASDF/PLAN::ACTION-STATUS`
  (10,046 invocations) contains `(incf (total-action-count *asdf-session*))`.
  Fixed by lowering such a place to a `TORCL::SET-ACCESSOR-SLOT` primitive that
  resolves accessor -> slot at RUN time, as the tree-walker does; the lowerer
  uses only the mapping's existence as a gate, so a function compiled before its
  class exists still bails rather than baking in a stale slot. `form:SETF` bails
  on a babel load went 9 -> 0, the tier split 51.5% -> 61.9% compiled, and
  tree-walked invocations 39,889 -> 27,550 (-12,339, i.e. essentially the whole
  `compiler-bailed` population). Worth **-1.67%** of retired instructions; the
  wall-clock effect was below this machine's noise floor (see
  `measuring-performance.md` §6c).
- **bliss-d9ak** (P2) — make method compilation tier-driven.
- **bliss-ok3f** (P3) — pre-existing: a generic call accepts a keyword no
  applicable method declares.

## How to re-measure

Add two counters to the branches in `invoke_method`, key a `HashMap<u64, u64>`
by `method.method_id.0` on the tree-walked side, and record
`(name, reason)` per method at the `DEFMETHOD` site where `nested_in_binding`,
`body_uses_next_method` and the lambda list are all in scope. Dump from
`main.rs` after `result` is produced — not from the `QUIT` builtin, which
`--no-init` never defines.

Note the trap in `measuring-performance.md` §6b: a bytecode-instruction counter
**cannot** distinguish "ran native" from "was tree-walked" — both execute zero
bytecode. Counting the two dispatch branches is what works.
