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

## 3. Prerequisites

These are **not** follow-ups. Each is a latent correctness hole that the
representation change converts into a live one, and none is fixed by pinning.

### 3.1 The CLOS root scanner visits no map keys

`visit_clos_roots` (`clos.rs:456-537`) iterates `values_mut()` everywhere and
rests on a stated premise that the change removes — `clos.rs:463`:

> *"Registry keys are symbols or private meta-handles and never move."*

Specifically unvisited:

- **`class_meta`'s keys** (`clos.rs:471` visits only values). These *are* the
  class values. After a relocation the key keeps the old bits while every
  visited copy is updated, so `st.class_meta.get(&class)` misses — breaking
  `finalize_class_layout` (`clos.rs:1771`), `allocate_instance_pinned_gc`
  (`clos.rs:2204`, which then errors "cannot allocate an instance of a class
  with no slot layout"), `struct_slot_order` (`clos.rs:2281`) and subclass
  registration (`clos.rs:1710`).
- **`structure_classes`** (`clos.rs:383`) — not referenced by the scanner at
  all. Its elements are class values. Unrooted and unrewritten, a live struct
  class answers `false` to `is_struct_class` (`clos.rs:1585`), changing
  `EQUALP` descent and `#S(...)` printing (the bliss-i1i9 behaviour).
- **`ClassMeta.shared_slots`** (`clos.rs:69`) — unvisited. Safe today because
  slot names are pinned symbols. See also bliss-dfm5f: it is not serialized
  either, which is a live bug independent of this work.

A map keyed on a relocatable pointer cannot be fixed in place — it needs
rehashing. The repo's reference implementation is
`crates/egcl-stdlib/src/devtools.rs:2769-2785`: `std::mem::take`, `visit(&mut
key)`, reinsert. **It must be idempotent**, because scanners run twice per
collection — once for marking and once for relocation (`gc.rs:1685`/`1961`
minor, `gc.rs:2480`/`2753` major).

Pinning makes rehashing unnecessary in the steady state, but the scanner is
still the only thing that makes a class *reachable*, which §3.3 covers.

### 3.2 `ClassWrapper.class` is an untraced `EgclVal` outside the heap

`ClassWrapper` (`clos.rs:118`) holds `class: EgclVal` and is a leaked Rust
`Box` (`clos.rs:1786`). The scanner deliberately skips it — `clos.rs:489`:

> *"Wrapper class ids and slot-layout names are the same immediate
> meta-handles/symbols represented above, so they need no rewrite."*

That is the single most load-bearing assumption being removed. It is also the
word `class_of` reads (`clos.rs:1814`), and the word `trace_object` treats as
raw for a `STANDARD_OBJECT` (`gc.rs:1316`) — so **instances do not root their
own class**. A heap class stored there is a GC-invisible reference that goes
stale on the first collection after a `DEFCLASS`.

Either give `ClassWrapper`/`SlotLayout` a tracing story, or stop storing a
class *value* there (e.g. store a stable class id and resolve through the
registry). Pinning makes the stored pointer valid; it does not make it a root.

### 3.3 Reachability is still required

Pinning stops relocation, not collection *semantics*. An unreachable pinned
object is never freed and never zeroed (`gc.rs:2862`), yet its slots keep being
traced at every minor GC (`gc.rs:1707`) — a permanently-traced zombie holding
dangling slots. A class must stay reachable from a root scanner for its whole
life. This also matters for the shaken image: `save_reachable_image` retains
only what `reachable_heap_objects` reaches (`gc.rs:6536`), so a class reachable
*only* as a `class_meta` key would be omitted from disk while the restored
registry still names its address. Verify with a **shaken** core, not a plain one.

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
5. **Test the wrapper walk over class objects.** The save/restore wrapper table
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
| 1 | Root-scanner completeness: visit `class_meta` keys (idempotent rehash), `structure_classes`, `ClassMeta.shared_slots` | no behaviour change today (all immediates) | GC stress over DEFCLASS/DEFSTRUCT; a probe asserting the rehash is idempotent across both scanner passes |
| 2 | `ClassWrapper.class` tracing story | no behaviour change today | as above, plus a class-redefinition probe |
| 3 | `shared_slots` serialization + CLST version 4 (bliss-dfm5f) | fixes a live image bug | save/restore round-trip over an `:allocation :class` shadowing class |
| 4 | The representation switch: pinned class objects, bootstrap order, `find_class` returning them | the whole of bliss-rj5o | §8 |
| 5 | Printing (`#<STANDARD-CLASS FOO>`), `TYPE-OF`, disjointness from `SYMBOL`/`INTEGER` | the §1 symptoms | ANSI objects + types-and-classes |
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
