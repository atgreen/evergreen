# Design: class metaobjects as real objects (bliss-rj5o)

Status: **proposal** · Owner: — · Related: bliss-r894g (GitHub #384),
bliss-mn4d, bliss-dfm5f, bliss-7l7cn, bliss-334, bliss-wc4t, bliss-jtc.18

## 1. Problem

A class metaobject is an *immediate* `EgclVal`. A `DEFCLASS` or `DEFSTRUCT`
class **is** its own name symbol; a built-in class is a negative fixnum; a few
are `TAG_SPECIAL` meta handles. Measured on `9fee9bde`:

```lisp
(symbolp (find-class 'foo))               => T
(find-class 'standard-class)              => -40      ; prints as a raw integer
(integerp (find-class 'standard-class))   => T
(find-class 'class)                       => -39
(format nil "~s" (find-class 'foo))       => FOO       ; not #<STANDARD-CLASS FOO>
(type-of (find-class 'foo))               => SYMBOL    ; not STANDARD-CLASS
```

So a class is not disjoint from `SYMBOL` or `INTEGER`, has no identity separate
from its name, and — the consequence that brought this to a head — **has
nowhere to store slots**. `INTEGERP` answering true for a class is not merely
cosmetic: a method specialized on `INTEGER` is applicable to a built-in class
metaobject, and arithmetic on one silently works.

### 1.1 What is already correct

Worth stating up front, because it narrows the work: the identity *relations*
are right today. `class_of_value` (`crates/egcl/src/cli.rs:14891`) answers the
recorded metaclass, and

```lisp
(eql (class-of (class-of (find-class 'foo))) (find-class 'standard-class))  => T
(eq  (class-of (find-class 'plain))          (find-class 'standard-class))  => T
(typep (find-class 'f) 'class)                                             => T
(eq 'f (class-name (find-class 'f)))                                       => T
(subtypep 'file-stream 'stream)                                            => (T T)
```

all hold. Four of the consequences the bead originally listed have since been
fixed by other work. What is missing is **object-hood**: distinct printing,
disjointness from `SYMBOL`/`INTEGER`, and slot storage.

### 1.2 The consumer that forced it: GitHub #384

Gendl defines a metaclass `gdl-class` with slots, and every per-class table its
kernel keeps — messages, documentation, caches — is a slot of `gdl-class` read
through an accessor on the class object, with every kernel operation a method
specialized on `gdl-class`. #380 made `CLASS-OF` answer the metaclass, which is
half of what it needs. The other half is that the class metaobject must *be* an
instance of that metaclass. Today (bliss-r894g):

```lisp
(defclass meta (standard-class) ((x :initform 1 :accessor meta-x)))
(defmethod egcl-mop:validate-superclass ((c meta) (s standard-class)) t)
(defclass foo () () (:metaclass meta))

(class-of (find-class 'foo))        => META          ; correct, #380
(typep (find-class 'foo) 'meta)     => NIL           ; expected T
(slot-exists-p (find-class 'foo) 'x)=> NIL           ; expected T
(meta-x (find-class 'foo))          => NIL           ; expected 1
(cc (find-class 'foo))              => :STD          ; expected :META
```

## 2. Target representation

A class metaobject becomes a **pinned GC-heap `STANDARD_OBJECT`** whose wrapper
is its metaclass — i.e. exactly what `allocate_instance_pinned_gc`
(`crates/egcl-stdlib/src/clos.rs:2201`) already produces for the immortal
`STORAGE-CONDITION` pool.

```
offset 0   ObjectHeader (8 bytes), PINNED bit set
offset 8   wrapper pointer -> the METACLASS's ClassWrapper   (raw, not traced)
offset 16  slot[0..N-1]    the metaclass's instance slots    (traced)
```

`N` is the metaclass's effective instance-slot count: 0 for a plain
`STANDARD-CLASS`, and for `(:metaclass META)` exactly `META`'s slots.

**The class's own metadata stays in `ClassMeta`**, the side table, keyed by the
(now address-stable) object. Name, supers, subs, slots and metaclass do *not*
move into Lisp-visible slots. That keeps the change to representation and
identity, and leaves the metadata plumbing — and the image format for it —
largely as it is.

### 2.1 Why this falls out so cleanly

Every one of #384's four symptoms is a consequence of the representation, so
each is fixed by the representation rather than by a check:

| symptom | why it then works |
|---|---|
| `TYPEP` against the metaclass | `is_instance` is true, so `TYPEP` routes through the class hierarchy (`cli.rs:16501`) and the metaclass is in the CPL |
| method dispatch on the metaclass | `method_specificity_vector` takes the `is_instance` branch (`cli.rs:7046`) and finds the metaclass in the CPL at the right distance |
| slots + accessors on the class | the object *has* the metaclass's slots; `egcl_stdlib::slot_value` already serves them |
| dispatch cache keyed per metaclass | `GfKey.classes` is `class_of(arg)` (`cli.rs:8113`), which is now the metaclass, so two metaclasses no longer collide |

`class_of` needs no new case either: for a `STANDARD_OBJECT` it already returns
`(*instance_wrapper(object)).class` (`clos.rs:1814`).

### 2.2 Why pinned, specifically

`alloc_pinned_typed` (`crates/egcl-rt/src/gc.rs:5158`) gives the four
properties a class needs, and the alternatives give none of them:

- **Address-stable for life.** Allocated straight into old-gen host regions,
  never evacuated (`gc.rs:2671`), never freed even at zero live bytes
  (`gc.rs:2862`), and a pinned nursery region is retained in place
  (`gc.rs:1782`). So a class value remains a valid `HashMap` key.
- **Still traced.** Word 0 is raw, words 1.. are traced
  (`gc.rs:1316`); minor GC walks every old-gen object for young references
  (`gc.rs:1707`) and relocates its fields (`gc.rs:1430`).
- **Immortal.** Only `FUNCTION_INTERPRETED` and `SYMBOL` are admitted to
  `free_pinned_slots` (`gc.rs:5214`), so a pinned `STANDARD_OBJECT` is never
  reclaimed and its address is never recycled. Correct for classes, and it
  removes any ABA hazard for address-keyed caches.
- **Bounded cost.** Pins are packed into dedicated host regions, bounding the
  unreclaimable area to `ceil(total pinned bytes / 1 MiB)` (`gc.rs:5236`).

The anti-pattern is explicit in the tree: nursery-allocate then `gc::pin()`
retains a whole 1 MiB region per object — bliss-wc4t measured ~316 instances
poisoning 316 × 1 MiB regions (`clos.rs:2238`). **Use `alloc_pinned_typed`.**

Pinning also makes a class usable as an `EQ` hash-table key for free:
`is_movable_pointer` (`crates/egcl-stdlib/src/hashtable.rs:918`) treats a
pinned heap object as stable, so such a table is never marked
`address_sensitive` and never needs `maybe_rehash_for_gc`.

## 3. What pinning buys, and the comments that must change

An earlier draft of this document listed the three items below as
*prerequisites*, on the reasoning that a heap class value must be traced and
rehashed like any other. That reasoning assumed a **movable** class. With a
pinned class it does not hold, and the distinction is the main reason to pin
rather than to allocate ordinarily. Recorded here because the wrong version is
an easy and expensive mistake to make: it would add an O(classes) map rebuild
to every minor GC, twice per collection, for no benefit.

Two facts settle all three, both checked against the source rather than assumed:

- **A pinned class never moves.** Never evacuated (`gc.rs:2671`), never freed
  (`gc.rs:2862`), and a pinned nursery region is retained in place
  (`gc.rs:1782`). So no copy of a class value anywhere can go stale, in a map
  key or outside the heap.
- **Every class is reachable through `class_registry`, whose values the scanner
  already visits** (`clos.rs:465`). All seven `class_meta.insert` sites
  (`clos.rs:954, 1355, 1370, 1411, 1472, 1611, 1726`) are paired with a
  `class_registry.insert` of the same class value, so no class is reachable
  *only* as a key.

### 3.1 No rehash, but keys still need MARKING

`visit_clos_roots` (`clos.rs:456-537`) iterates `values_mut()` everywhere and
never visits a map key; `structure_classes` (`clos.rs:383`) is not referenced at
all. Under pinning no key can go **stale**, so no rehash is needed — and that
matters, because an unconditional `mem::take`-and-rebuild of `class_meta` would
cost O(classes) twice per collection forever.

But marking is a separate question from relocation, and the pairing argument
above has a hole I missed on the first pass: it covers every
`class_meta.insert`, and **not removal**. `bind_class_name(name, NIL)` —
i.e. `(setf (find-class 'foo) nil)`, which CLHS requires — removes the entry
from `class_registry` and `class_by_name` and **leaves `class_meta` alone**
(`clos.rs:1642-1648`). After that call the class is reachable *only* as a
`class_meta` key. Obsolete pre-redefinition classes are a second route to the
same state.

In-process that is harmless, because a pinned `STANDARD_OBJECT` is immortal
(§2.2). For a **shaken image** it is not: `save_reachable_image` retains only
what `reachable_heap_objects` reaches (`gc.rs:6536`), so an orphaned class is
omitted from disk while the restored `class_meta` still names its address.

The cheap fix follows from pinning: because a pinned key never changes value,
the scanner can **visit a copy** of each key purely to mark it, and discard the
copy. O(n) marking, no rehash, no map rebuild:

```rust
// Keys are class values. Pinned, so they never move and need no rehash -- but
// a class orphaned by (setf (find-class 'x) nil) is reachable ONLY here, so
// mark through a copy to keep it in a shaken image.
for &key in state.class_meta.keys() {
    let mut marked = key;
    visit(&mut marked);
    debug_assert_eq!(marked, key, "a pinned class moved");
}
```

The `debug_assert` is the test for the load-bearing invariant: if a class ever
becomes movable, this fires rather than silently corrupting the map. The same
treatment applies to `structure_classes`. If classes ever do become movable the
rehash becomes mandatory, and the pattern is
`crates/egcl-stdlib/src/devtools.rs:2769-2785` (`std::mem::take`, visit,
reinsert), which must be idempotent because scanners run twice per collection
(`gc.rs:1685`/`1961` minor, `gc.rs:2480`/`2753` major).

### 3.2 `ClassWrapper.class` needs no tracing story, only a corrected comment

`ClassWrapper` (`clos.rs:118`) holds `class: EgclVal` in a leaked Rust `Box`
(`clos.rs:1786`) that no scanner visits — declined at `clos.rs:489` — and it is
the word `class_of` reads (`clos.rs:1814`). For a *movable* class that would be
a GC-invisible reference going stale on the first collection after a
`DEFCLASS`. For a pinned, immortal class the raw copy stays valid for the
process lifetime. The comment's stated reason ("the same immediate
meta-handles/symbols") stops being true and must be rewritten to name the pin.

This word is also the one `trace_object` treats as raw for a `STANDARD_OBJECT`
(`gc.rs:1316`), which is why **instances do not root their own class** —
the registry, not the instance, is responsible for that.

### 3.3 Redefinition leaks, by construction

A pinned `STANDARD_OBJECT` is never reclaimed: only `FUNCTION_INTERPRETED` and
`SYMBOL` are admitted to `free_pinned_slots` (`gc.rs:5214`) and
`dead_function_slots` is populated only for those (`gc.rs:2555`, `:2579`), and
a region holding any pin is never freed (`gc.rs:2862`). So **every `DEFCLASS`
redefinition leaks a class object and permanently retains its host region.**

That is acceptable for a bounded class population and unacceptable for a
program that redefines classes in a loop. The cost is bounded by packing
(`ceil(total pinned bytes / 1 MiB)`, `gc.rs:5236`), and a class object is small
(16 bytes plus the metaclass's slots), so thousands of redefinitions cost
single-digit MiB. Record it as a known limit rather than designing around it;
revisit if a workload shows up. Note the general doc comment at `gc.rs:5162`
says pinned slots *can* be reclaimed once unreachable — that is true for
interpreted functions and symbols only, and does not apply here.

### 3.4 The decision that dominates everything: which `type_id`?

This is the fork to settle before any code is written, and it was not obvious
until the call-site audit: **a class allocated as `STANDARD_OBJECT` makes
`egcl_stdlib::is_instance` answer true for every class.** That predicate is
just a header test (`clos.rs:1803`), there is no dedicated class `type_id`
today, and roughly 38 sites gate on it.

Type ids run to `PYTHON_OBJECT = 0x1E` (`crates/egcl-rt/src/object.rs:169-211`),
so `0x1F` and up are free. The two options:

**(a) `STANDARD_OBJECT`.** Semantically right — CLHS makes a class an instance
of `STANDARD-OBJECT` — and #384's four symptoms then fix themselves, because
`TYPEP` and dispatch already route instances through the class hierarchy and
`class_of` already reads the wrapper. The cost is the `is_instance` cascade:
every site that uses `is_instance` to mean *"an ordinary user instance"* now
admits classes, and several answer wrongly **with no diagnostic**. The worst,
all verified:

- `is_standard_class_metaobject` (`cli.rs:14951`) short-circuits on
  `class.is_symbol()` at `:14955`. Once a class is not a symbol a user class
  falls through to a hardcoded name whitelist that cannot contain it, so it
  answers `false` — and that one function drives `(class-of (find-class 'foo))`
  ⇒ `BUILT-IN-CLASS` (`cli.rs:14904`), `(typep c 'standard-class)` ⇒ NIL and
  `(typep c 'built-in-class)` ⇒ T (`cli.rs:16494`), the specializer table
  (`cli.rs:6986`), and DEFCLASS's own "is a system class" rejection
  (`cli.rs:35807`), which would refuse every user superclass and break
  `boot.lisp`. **Fix this first; it is four wrong answers from one line.**
- `TYPEP` ordering: the `is_instance` CPL branch at `cli.rs:16432` fires and
  returns *before* the dedicated metaclass arm at `cli.rs:16477`, making that
  arm dead code.
- `cli.rs:6611` computes `built_in = !is_instance(x) && !class_of(x).is_symbol()`,
  whose own comment (`:6607`) names the invariant being removed; it collapses to
  `!is_instance(x)`, routing conditions and structures to SLOT-MISSING.
- `coerce_condition_designator` (`cli.rs:5339`) returns an `is_instance` datum
  as the condition, so `(error (find-class 'my-error))` would signal the class
  object itself.
- `is_builtin_class` (`clos.rs:1179`) discriminates on the fixnum *sign*; the
  explicit fallback covers 10 of the 47 built-in classes, so
  `(make-instance (find-class 'integer))` stops erroring (`clos.rs:2320`).
- `conditions.rs:438` does `define_class(sym, sym, …); Ok(sym)` for every
  built-in condition type, which would build a parallel symbol-keyed class
  universe silently.
- `sym_name(class)` at `cli.rs:20511` feeds `env.classes.get(&class_name)`, so
  the whole EGCL-MOP slot-descriptor surface quietly returns an empty list.
- `SXHASH` of a class moves from name-based to address-based
  (`hashtable.rs:781` vs `:816`) — i.e. no longer image-portable.

**(b) A dedicated `CLASS` type_id (0x1F).** `is_instance` stays false, the
cascade evaporates, and the ~38 sites keep their current meaning. The cost is
that none of #384 fixes itself: `TYPEP`, dispatch, `slot-value`,
`slot-exists-p` and the accessor path each need an explicit class arm, and
`(typep c 'standard-object)` must be made true by hand.

**Recommendation: (a), with the `is_standard_class_metaobject` fix landed
first and separately.** The cascade is largely *correct* behaviour arriving by
the right route — `TYPE-OF` answering `STANDARD-CLASS`, a class being a
`STANDARD-OBJECT` — and option (b) buys a smaller diff by making the
representation less true, which is what the current design already does. But
the list above is the real cost of (a) and it should be budgeted, not
discovered. Several items are latent-today bugs whose comments already name the
assumption, so they are worth fixing on their own.

## 4. Lock ordering and the bootstrap

CLOS definitions is `(GcWorld, 8)` (`clos.rs:448`); GC heap state is
`(GcWorld, 2)` (`gc.rs:5982`). The checker admits an acquisition only when
`requested.order > held.order` at the same level (`lock_order.rs:73`), so
taking the heap lock while holding CLOS definitions **panics in a debug
build** and risks a real inversion in release, because the collector takes them
in the opposite order — root scanners run while it holds the heap lock
(`gc.rs:3211`) and the CLOS scanner then takes `(8, 8)`, a legal `2 → 8`.

**Therefore: allocate the class object before taking the CLOS lock, and
allocate nothing inside `with_state`/`with_state_mut`.** This is the existing
discipline, already stated for interning at `clos.rs:1293` and already the
shape of `allocate_instance_pinned_gc` (extract `(wrapper, slot_count)` under
the lock, return, then allocate outside). The public API cooperates:
`define_class_with_shared_slots` (`clos.rs:1679`) takes the class value as a
parameter, so the caller mints the object first.

Two further hazards:

- `alloc_pinned_typed` **polls a safepoint** (`gc.rs:5173`), so a class built
  but not yet registered — and the supers/slots vectors passed to
  `define_class` — must be `rooted!`/`rooted_ref!`.
- It **does not honour `EGCL_GC_STRESS`** (`gc.rs:5163`), so stress alone will
  not exercise that window. Plan explicit probes instead of relying on the
  fuzzer here.
- It returns `None` rather than collecting for any type but
  `FUNCTION_INTERPRETED` (`gc.rs:5244`), so `DEFCLASS` gains a real
  heap-exhaustion failure path.

### 4.1 Bootstrap order

`initialize_clos` (`clos.rs:1225`) currently mints 47 built-in classes as
negative fixnums `-1..-47` and stores them in one non-allocating
`with_state_mut`. Under the change it must allocate 47 pinned objects, and
allocating an instance of `STANDARD-CLASS` needs `STANDARD-CLASS`'s wrapper,
which needs its layout, which needs its `class_meta` entry — the classic MOP
circularity. Order:

1. Intern every name (allocates; must precede the lock, as today).
2. `alloc_pinned_typed` one zero-slot `STANDARD_OBJECT` per built-in class,
   wrapper word left null. No CLOS lock held.
3. One `with_state_mut`: register names → objects and build `class_meta`.
   Stores only already-allocated values; never allocates.
4. Finalize `STANDARD-CLASS`'s (and `BUILT-IN-CLASS`'s, `STRUCTURE-CLASS`'s)
   layout, then patch each class object's wrapper word via
   `set_instance_wrapper`. Cheap: one word per class.

After bootstrap, an ordinary `DEFCLASS` needs no special case — the class
object is `allocate_instance_pinned_gc(metaclass)`, which supplies the right
wrapper, the right slot count and `UNBOUND` slots, after which the metaclass's
`:initform`s run.

## 5. The image

The good news first: **the wire format for a class *value* needs nothing new.**
`TAG_HEAP_OBJECT` is already relocated by `remap_value`
(`crates/egcl-rt/src/image_relocation.rs:119`); `restore_clos_state` already
applies `remap` to the `class_registry` and `class_meta` keys (`clos.rs:936`,
`:948`); saved objects are force-pinned (`gc.rs:6482`) and restored regions
re-registered as pinned hosts (`gc.rs:6858`), so a restored class stays pinned
and fixed; and `STANDARD_OBJECT` slot words are already in the fixup list
(`gc.rs:1316`, `:6509`).

What must change:

1. **Bump both version constants and reject old cores.** The image
   `FORMAT_VERSION = 9` (`crates/egcl-rt/src/image.rs:154`, checked for exact
   equality at `:848` and `:1111`) and the CLOS block's own version
   (`cs_put_u32(&mut out, 3)` at `clos.rs:788`, checked `if version != 3` at
   `:928`). Old cores encode classes as symbols and fixnums, which after the
   change would be silently reinterpreted as valid-looking garbage.
2. **Serialize `ClassMeta.shared_slots`** — bliss-dfm5f. Already a bug; the
   version bump is the moment to fix it.
3. **Rework `max_metaobject_id()`** (`clos.rs:1128`), which harvests class
   values as meta-handle ids to advance the id floor (bliss-66io), called from
   `cli.rs:42496`. Classes stop participating in that id space; drop the
   class/`structure_classes`/built-in-field sweeps rather than leaving dead
   `is_meta_handle()` tests.
4. **Check the BBU constant pool.** `value_leaf`
   (`crates/egcl/src/cli/bytecode.rs:9516`) encodes a symbol at `:9568` and
   falls to `None` — "unencodable constant", silently rejecting the whole
   function's pool — at `:9614`. A class literal is encodable today *only*
   because it is a symbol. Either add a by-name class encoding (as pathnames do
   at `:9606`) or prove with a test that no compiled literal holds a class.
6. **Expect a T2 coverage regression, and decide whether to accept it.** A
   class constant is baked into native code today as an immediate: `emit_const`
   routes `TAG_SYMBOL` to `ConstSymbol` and a fixnum to `ConstFixnum`
   (`crates/egcl-compiler/src/t2/build.rs:3061-3084`), so `(find-class
   'hash-table)` is currently even *inferred as a fixnum type*. After the change
   both fall to the `ConstHeapObj`/`HeapLiteral` arm (`build.rs:3086`), which is
   GC-correct — T1 already gates on `is_heap_ref` in all five lowerers — but
   deopt and transfer metadata deliberately refuse heap-object constants
   (`crates/egcl-compiler/src/t2/deopt.rs:282`, `LowerError::MovingConst`;
   `transfer_capture.rs:234`). So **any function whose deopt frame state holds a
   class constant loses T2.** Pinned constants are exactly the case that refusal
   need not cover, so the clean fix is to admit a pinned heap constant there
   rather than to accept the regression.
7. **Two leaked, untraced caches hold class bits compared by address**, both
   sound only while classes are pinned, and both worth a comment naming that:
   `GF_DISPATCH_CACHE`'s `GfKey.classes` (`cli.rs:8113`) and the receiver type
   profile `ReceiverTypeProfileEntry { receiver_type: AtomicU64 }`
   (`cli/bytecode.rs:19170`, fed from `cli.rs:8042`, compared at `:19217`).
   Immortality is what makes them safe: an address is never recycled, so there
   is no ABA hazard.
8. **One image-ordering hazard, latent today.** `run_offheap_populate()`
   rehashes every Lisp hash table (`crates/egcl-rt/src/image.rs:1021`) *before*
   the host hook restores CLOS (`image.rs:1037`), and at that instant every
   restored instance's wrapper word still holds a save-time address, rewritten
   only later (`clos.rs:1106`). Harmless while key hashing never consults CLOS;
   any class-aware or slot-wise `EQUALP` hashing would dereference a dangling
   wrapper there.
9. **Keep raw, untagged words out of a class object's body.** The restore path
   remaps *every* 8-byte body word (`gc.rs:7009`) and treats `tag == 0` as a
   bare address (`crates/egcl-rt/src/image_relocation.rs:103`), so a stamp or a
   slot count stored in the body can be silently rewritten if its bits alias a
   saved heap address. Metadata belongs in `ClassMeta` or the wrapper, which is
   where §2 already puts it.

10. **Test the wrapper walk over class objects.** The save/restore wrapper table
   is keyed by save-time address and rewrites word 0 of every
   `STANDARD_OBJECT` found by `walk_heap` (`clos.rs:876`, `:1105`). Once a
   class *is* one, it is visited too — which is what we want, but it is the
   easiest place to go subtly wrong, because `class_meta.wrapper` and the class
   object's own word 0 then describe two different things (the class's
   instances' layout vs. the class's own metaclass).

## 6. Other address-keyed state

`GF_DISPATCH_CACHE` (`cli.rs:2886`) keys on `GfKey.classes`, raw class bits
written at `cli.rs:8113`, visited by no scanner and invalidated only by a
generation counter (`cli.rs:7861`). This is *sound under pinning* — addresses
are stable and never recycled — but it must be revisited if a class ever
becomes movable or collectible. There is already a comment anticipating exactly
this at `cli.rs:1560`, about the accessor cache: keying on the class object's
address *"would have been cheaper to obtain and quietly wrong — the class is a
heap value the moving collector can relocate, so its address is not an
identity."* Under this design the address *is* an identity, and that comment
should be updated to say why.

`STORAGE_POOL` (`crates/egcl-stdlib/src/conditions.rs:387`) is the existing
precedent for "pinned, therefore raw bits may live outside GC visibility"
(`conditions.rs:507`), and the matching hazard if a pin is ever dropped.

## 7. Staging

Each stage is independently landable and independently valuable. The branch
order matters: 1 and 2 are the prerequisites of §3, and both are worth having
on their own.

| # | Stage | Lands | Validation |
|---|---|---|---|
| 0 | Settle §3.4 (`type_id`), then fix `is_standard_class_metaobject` (`cli.rs:14951`) to stop using `is_symbol()` as "is a user class" | a latent bug today; four wrong answers avoided later | `(class-of (find-class 'foo))`, `(typep c 'standard-class)`, DEFCLASS with a user superclass, both tiers |
| 1 | Mark `class_meta` keys and `structure_classes` through a copy, with the `debug_assert` pinning invariant; visit `ClassMeta.shared_slots`; correct the `clos.rs:463` and `:489` comments | no behaviour change today | GC stress over DEFCLASS/DEFSTRUCT; a shaken core after `(setf (find-class 'x) nil)` |
| 2 | `shared_slots` serialization + CLST version 4 (bliss-dfm5f) | fixes a live image bug | save/restore round-trip over an `:allocation :class` shadowing class |
| 3 | The representation switch: pinned class objects, bootstrap order, `find_class` returning them, the `is_instance`-cascade sites of §3.4 | the whole of bliss-rj5o | §8 |
| 4 | Printing (`#<STANDARD-CLASS FOO>`), `TYPE-OF`, disjointness from `SYMBOL`/`INTEGER` | the §1 symptoms | ANSI objects + types-and-classes |
| 5 | BBU constant-pool encoding for a class, or a test proving no compiled literal holds one | BFASL caching keeps working | a BFASL round-trip over a function with a class constant |
| 6 | Retire the negative-fixnum ids and `fixnum_registrations`' diamond heuristic | removes the last immediate-class path | ANSI arrays + objects |
| 7 | #384's four symptoms as regression tests (bliss-r894g) | the external consumer | the §1.2 program, both evaluator tiers |

Stage 4 is the one that cannot be subdivided much; within it, expect separate
reviewable commits for allocation+bootstrap, registry re-keying, the
`find_class` coercion boundary, and the image.

## 8. Validation

- **Both evaluator tiers, every time.** A check in the tree-walking evaluator
  does not cover `cli/bytecode.rs`. Write each probe form directly *and* inside
  a `defun`; an `(eval '...)`-wrapped case tests only the tree-walker
  (memory `acceptance-eval-wrapper-hides-compiled-path`).
- **Probes must live in a FILE, not `--eval`.** `--eval '(progn (in-package
  :p) ...)'` reads the entire form in the old package before `IN-PACKAGE` takes
  effect, so a package-sensitive probe written that way measures nothing.
- **GC stress will not cover the pinned allocation itself**
  (`gc.rs:5163`). Target stress at the surrounding DEFCLASS path, and prove
  relocation separately where a rooting claim is made: copy the value, root the
  original, allocate, compare — if it did not move, the probe proved nothing
  (bliss-c0diw / bliss-ahnzt).
- **The anchor numbers.** ANSI chapters that should improve, recorded in
  `scripts/ansi-gate.sh`: `objects 811/52`, `arrays 1243/113`,
  `types-and-classes 613/10`. The bead records ~99 of the arrays failures as
  `:FAIL-NOT-ARRAY-CLASS` and 24 built-in-CPL failures (bliss-glr5). Measure
  before and after; do not claim a count without the paired runs.
- **A shaken core, not just a plain one** (§3.3).
- Every ad-hoc run under `scripts/egcl-limited.sh`.

## 9. Open questions

1. Does any compiled literal ever hold a class value? §5 item 4 decides
   whether the BBU pool needs a by-name encoding or just a test.
2. Should `ClassMeta` eventually move into Lisp-visible slots of the class
   object (a truer MOP), or stay a side table? This design keeps the side
   table deliberately; revisiting it is a separate change.
3. `(find-class 'bit)` does not exist today, so
   `(make-array 3 :element-type (find-class 'bit))` errors. Whether the
   built-in class set grows is independent of representation, but it is part of
   the arrays-chapter number above.
4. `allocate-instance` on a *symbol* currently succeeds (it answers `#<F>`),
   because the symbol *is* the class. Once it is not, that call should signal —
   the bead's own note at `clos.rs` about bliss-2icv warns not to "fix" it
   before this lands.
