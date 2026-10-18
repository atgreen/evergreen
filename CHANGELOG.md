# Changelog

## Unreleased

### Added

- `EGCL-MOP`, the metaobject-protocol package `docs/egcl-lisp-api.md`
  specified but the bootstrap never installed. It provides
  `CLASS-DIRECT-SUPERCLASSES`, `CLASS-DIRECT-SUBCLASSES`,
  `GENERIC-FUNCTION-METHODS`, `CLASS-FINALIZED-P`, `FINALIZE-INHERITANCE`,
  `INTERN-EQL-SPECIALIZER` and `VALIDATE-SUPERCLASS`, and is the home of
  `CLASS-SLOTS`, `CLASS-DIRECT-SLOTS` and the `SLOT-DEFINITION-*` readers.
  `COMMON-LISP-USER` uses it, so those names stay reachable unqualified where a
  person is typing ([#380](https://github.com/atgreen/evergreen/pull/380);
  issue [#371](https://github.com/atgreen/evergreen/issues/371)).
- `DEFCLASS`'s `:METACLASS` takes effect: `CLASS-OF` answers the named
  metaclass instead of `STANDARD-CLASS`. The option was parsed and discarded,
  so a kernel that tells its own classes from plain ones with
  `(eql (class-of (find-class name)) (find-class 'its-metaclass))` -- Gendl's
  does -- saw every class as plain
  ([#380](https://github.com/atgreen/evergreen/pull/380);
  issue [#371](https://github.com/atgreen/evergreen/issues/371)).

- Package-local nicknames (CDR 10). `DEFPACKAGE` accepted `:LOCAL-NICKNAMES`
  and dropped it, so a library's `(:local-nicknames (:a :alexandria))` left
  `a:define-constant` unreadable: "package not found: A". The option now
  takes effect, the reader, `FIND-PACKAGE`, `INTERN` and `FIND-SYMBOL`
  resolve a prefix through the current package's local nicknames before the
  global names, `EGCL-EXT` exports `ADD-PACKAGE-LOCAL-NICKNAME`,
  `REMOVE-PACKAGE-LOCAL-NICKNAME`, `PACKAGE-LOCAL-NICKNAMES` and
  `PACKAGE-LOCALLY-NICKNAMED-BY-LIST` as the API document already listed
  them, `:PACKAGE-LOCAL-NICKNAMES` is on `*FEATURES*`, and UIOP's
  `DEFINE-PACKAGE` finds the functions in `EGCL-EXT`.
- Socket endpoint queries: `(egcl::%socket-listener-local-name listener)`,
  `(egcl::%socket-local-name stream)` and `(egcl::%socket-peer-name stream)`
  report an endpoint as an octet vector and a port together, four elements for
  IPv4 and sixteen for IPv6. A library adapter could previously learn a
  listener's port but never any address, so usocket's `get-local-address`,
  `get-peer-address`, `get-peer-port` and `get-local-name` had no primitive
  behind them and AllegroServe could not log or print a socket
  ([#352](https://github.com/atgreen/evergreen/pull/352),
  [#366](https://github.com/atgreen/evergreen/pull/366);
  issue [#350](https://github.com/atgreen/evergreen/issues/350)).

### Fixed

- `BASE-CHAR`, `STANDARD-CHAR` and `EXTENDED-CHAR` are proper subtypes of
  `CHARACTER` at more doors: `(coerce #\λ 'base-char)` and `(coerce #\a
  'extended-char)` signal `TYPE-ERROR` instead of returning the character,
  the interpreter's `(the base-char #\λ)` signals one, and an
  `EXTENDED-CHAR` array is a `CHARACTER` string. `COERCE` to a string
  signals `TYPE-ERROR` for an element that is not a character instead of
  dropping it, and `MAKE-ARRAY` rejects `:initial-contents` whose length is
  not the vector's instead of padding it
  ([#416](https://github.com/atgreen/evergreen/pull/416)).
- String literals are `CHARACTER` strings, as in SBCL, so a copy of one can
  hold any character: `(setf (char (copy-seq "abc") 0) #\λ)` works, and a
  literal is the same kind of string whether its file was compiled or not.
  `EGCL-EXT:READTABLE-BASE-CHAR-PREFERENCE` (`:SYMBOLS`, `:STRINGS`, `:BOTH`
  or `NIL`, as in SBCL) can make a readtable read compact 8-bit literals
  instead ([#411](https://github.com/atgreen/evergreen/pull/411)).
- Several threads may read one hash table at once, as in SBCL; only writes
  need exclusive access. Concurrent lookups in a table keyed by conses or
  other objects hashed by address used to crash once a garbage collection
  moved its keys ([#400](https://github.com/atgreen/evergreen/pull/400)).
- Threads running interpreted code no longer crash while another thread
  collects garbage. A `:synchronized t` hash table now takes its lock on every
  operation as documented; it never did, and the interpreter's shared
  macro-expansion cache is such a table
  ([#398](https://github.com/atgreen/evergreen/pull/398)).
- A class is an object rather than its own name symbol, so `(symbolp
  (find-class 'foo))` is false and `DEFCLASS` returns the class as CLHS
  requires. A class with a user `:METACLASS` is now an instance of it:
  `TYPEP` holds, a method specialized on the metaclass applies and is
  preferred over one on `STANDARD-CLASS`, and the class carries the
  metaclass's slots (issue
  [#384](https://github.com/atgreen/evergreen/issues/384),
  [#399](https://github.com/atgreen/evergreen/pull/399)).
- A slot added by redefining a class takes its `:INITFORM` on instances that
  already exist, as CLHS 4.3.6 requires: `(defclass c () ())`, an instance,
  then `(defclass c () ((s :initform 42)))` left `S` unbound where it now
  answers 42. `UPDATE-INSTANCE-FOR-REDEFINED-CLASS` is called for the update,
  so a method specialized on it participates and receives the added slots, the
  discarded slots and their values
  ([#412](https://github.com/atgreen/evergreen/pull/412)).
- A class that is only a forward reference -- named as a superclass and not yet
  defined -- can no longer be instantiated, as in SBCL: `MAKE-INSTANCE`,
  `ALLOCATE-INSTANCE` and `CHANGE-CLASS` each used to return an object with no
  slots, built against a layout the eventual `DEFCLASS` then contradicts.
  `EGCL-MOP:CLASS-FINALIZED-P` also stops claiming T for such a class, or for a
  class that inherits from one, and `EGCL-MOP:FINALIZE-INHERITANCE` signals for
  them instead of silently doing nothing
  ([#406](https://github.com/atgreen/evergreen/pull/406)).
- A class named as a superclass **before** it is defined, and then defined with
  a `:METACLASS` that has slots, is an instance of that metaclass like any
  other. `CLASS-OF` answered the metaclass while `TYPEP`, `SLOT-EXISTS-P`,
  `SLOT-VALUE`, the metaclass's slot accessors and method dispatch all still
  saw a plain `STANDARD-CLASS`. A kernel that appends a late-defined mixin to
  every class -- Gendl's does -- hit this on exactly the classes that mattered
  ([#404](https://github.com/atgreen/evergreen/pull/404);
  issue [#384](https://github.com/atgreen/evergreen/issues/384)).
- An inherited slot is reachable when the superclass is defined after the
  subclass: `(defclass sub (super) ...)` followed by `(defclass super ()
  ((a)))` left `A` unusable on an instance of `SUB`. Redefining a class now
  updates its subclasses' instance layouts, and no longer discards its record
  of them ([#399](https://github.com/atgreen/evergreen/pull/399)).
- `TYPE-OF` answers the class itself when the class has no proper name, rather
  than a name that no longer finds it
  ([#399](https://github.com/atgreen/evergreen/pull/399)).
- A built-in class is an object rather than an integer. `(find-class
  'integer)` was the fixnum -14, so `(eql -14 (find-class 'integer))` and
  `(integerp (find-class 'standard-class))` were both true, `(class-name -14)`
  answered `INTEGER`, a method specialized on `BUILT-IN-CLASS` applied to the
  integer -14, and a class printed as `-40`
  ([#396](https://github.com/atgreen/evergreen/pull/396)).
- `BASE-CHAR` is the element type of an 8-bit base string, consistently.
  `TYPEP` called every character a `BASE-CHAR` while `BASE-STRING` matched
  only 8-bit storage, `MAKE-STRING` ignored `:element-type`, and `MAP`,
  `COERCE` and `MAKE-SEQUENCE` built 32-bit strings for `BASE-STRING`, so
  `(make-array 5 :element-type 'base-char)` and `(map 'base-string ...)`
  answered strings `TYPEP` refused as base strings, and `TYPE-OF` called
  every string `SIMPLE-BASE-STRING`. `BASE-CHAR` is now the code points
  below 256 and `EXTENDED-CHAR` the rest, `UPGRADED-ARRAY-ELEMENT-TYPE` and
  `ARRAY-ELEMENT-TYPE` answer `BASE-CHAR` for 8-bit strings (a string
  displaced onto one included), the constructors honor the requested width
  (a wide character is a `TYPE-ERROR`), `COPY-SEQ`, `SUBSEQ`, `REVERSE` and the `REMOVE`,
  `SUBSTITUTE` and `REMOVE-DUPLICATES` families keep a string's width, and
  `TYPE-OF` answers `(SIMPLE-ARRAY CHARACTER (n))` for a 32-bit string (ansi
  sequences `MAP.SPECIALIZED-STRING.3`, `LENGTH.STRING.6`; the sequences
  chapter now passes completely and joins the gate)
  ([#403](https://github.com/atgreen/evergreen/pull/403)).
- Hash tables find every key their test calls equal. An `EQUALP` table missed
  `1d0` for `1`, `0.5` for `1/2`, an adjustable or fill-pointer vector or string
  for a simple one, a string for a vector of its characters, and equal
  structures and hash tables; `EQUAL` tables missed fill-pointer strings and lost
  an adjustable-vector key once an element changed. `EQL` (and `EQL`/`EQUAL`
  tables) no longer treat `-0d0` as `0d0`, nor `EQUAL` `-0.0` as `0.0`, and
  `SXHASH` no longer changes when the garbage collector moves an object
  ([#388](https://github.com/atgreen/evergreen/pull/388)).
- A generic function dispatched on a class object no longer answers whatever
  the first such call decided. With methods on `STANDARD-CLASS` and
  `STRUCTURE-CLASS`, whichever class was passed first fixed the answer for
  every class passed after it, and a method on `BUILT-IN-CLASS` traded
  decisions with one on `INTEGER` the same way
  ([#389](https://github.com/atgreen/evergreen/pull/389)).
- A saved image no longer lets newly compiled code redefine the local
  functions of code it saved. Loading the ocicl runtime in the `make image`
  egcl replaced one inside `PARSE-INTEGER`, after which `(parse-integer "1")`
  signalled "not an integer" and ASDF could not load babel
  ([#386](https://github.com/atgreen/evergreen/pull/386)).
- `RESTART` is consistently a standard class. Its class precedence list has
  always been `(RESTART STANDARD-OBJECT T)`, but `(class-of (find-class
  'restart))` answered `BUILT-IN-CLASS` and `(subtypep 'restart
  'standard-object)` answered false. All three agree now
  ([#387](https://github.com/atgreen/evergreen/pull/387)).
- `MAKE-PATHNAME` no longer builds a pathname with a wrong component when a
  garbage collection happens while it runs: with a `:directory` list or a
  namestring `:defaults`, a freshly made `:name` or `:type` string could be
  replaced by unrelated data, such as the directory's namestring
  ([#382](https://github.com/atgreen/evergreen/pull/382)).
- A library's own `DEFUN` of `CLASS-PRECEDENCE-LIST` no longer redefines the
  implementation's. `EGCL-MOP` owns and exports the name now, and the
  interpreter dispatches that spelling, so a bare read of it inside a package
  that does not inherit `EGCL-MOP` interns that package's own symbol instead of
  the implementation's. With `CLASS-SLOTS` in the previous release this
  completes [#370](https://github.com/atgreen/evergreen/issues/370)
  ([#383](https://github.com/atgreen/evergreen/pull/383)).
- `FORMAT`'s `~R` prints bignums in English: `(format nil "~R" (expt 10 20))`
  signalled a TYPE-ERROR saying its argument was not an integer. It also names
  every short-scale group through vigintillion, so 10^9 is "one billion", not
  "one thousand million"
  ([#381](https://github.com/atgreen/evergreen/pull/381)).
- `~D`, `~B`, `~O`, `~X` and `~R` keep a pad or comma character outside
  Latin-1: it was cut to its low byte, so `#\★` padded with a control character
  ([#381](https://github.com/atgreen/evergreen/pull/381)).
- `NTH-VALUE` and `DECODE-UNIVERSAL-TIME` signal a TYPE-ERROR for an argument
  that is not a non-negative integer instead of converting it through a float:
  a compiled `(nth-value -1 ...)` answered the first value, and
  `(decode-universal-time 1.5)` decoded a time
  ([#381](https://github.com/atgreen/evergreen/pull/381)).
- A library's own `DEFUN` of `CLASS-SLOTS` no longer redefines the
  implementation's. The non-ANSI MOP readers were reachable as a bare name from
  any package, so an internal helper of that name in a package that exports
  nothing hijacked the implementation's function, process-wide and silently.
  They live in `EGCL-MOP` now. `CLASS-PRECEDENCE-LIST` and
  `METHOD-SPECIALIZERS` are unchanged: they are name-dispatched builtins whose
  dispatch ignores the package, so moving the symbol does not stop the override
  ([#380](https://github.com/atgreen/evergreen/pull/380);
  issue [#370](https://github.com/atgreen/evergreen/issues/370)).
- A package designator that names no package now signals `PACKAGE-ERROR`
  instead of creating the package. `INTERN`, `EXPORT`, `IMPORT`, `SHADOW` and
  `USE-PACKAGE` created it; `FIND-SYMBOL`, `UNUSE-PACKAGE` and
  `PACKAGE-USE-LIST` answered as though the lookup had simply found nothing. So
  a typo in a package name, or a library loaded before the package it names,
  left a live package of that name: `FIND-PACKAGE` then answered it, a later
  `DEFPACKAGE` of the real one merged into the mistake, and the fault surfaced
  far from where it was made. `DEFPACKAGE` still resolves a forward `:use`
  reference, which is what the create-on-miss path exists for
  ([#379](https://github.com/atgreen/evergreen/pull/379);
  issue [#375](https://github.com/atgreen/evergreen/issues/375)).
- `OPEN` and `WITH-OPEN-FILE` honor `:external-format :latin-1` (also
  `:iso-8859-1`) and `:ascii`, alone or as the first element of a list such
  as `(:latin-1 :eol-style :lf)`; `:default` and `:utf-8` remain UTF-8. The
  keyword was accepted and ignored, so a Latin-1 file was decoded as UTF-8
  and the first accented letter was a `STREAM-ERROR`: cl-pdf opens its font
  metrics and cl-typesetting its French hyphenation patterns that way, and
  cl-typesetting could not load. Reading decodes one octet per character,
  writing refuses a character outside the format's range, and
  `STREAM-EXTERNAL-FORMAT` answers the format a file stream was opened with
  instead of `:default`. A format the stream layer cannot decode (`:utf-16`,
  `:utf-32`) is refused at `OPEN` rather than read as UTF-8.
- `LISTEN` on a freshly opened octet file stream answers T while bytes remain
  and NIL at the end of the file. With nothing buffered yet it asked the
  handle as if it were a socket, and a regular file always answered NIL, so a
  `(unsigned-byte 8)` file stream had to be read once before `LISTEN` saw
  it. The file-stream case joins the socket test of
  [#360](https://github.com/atgreen/evergreen/pull/360).
- A thread waiting for input on a stream no longer locks every other thread out
  of it. Input parked while holding the stream's state mutex, so for as long as
  one thread waited for a peer, any other operation on that stream blocked --
  `CLOSE`, a write on the other half of a socket, even `INPUT-STREAM-P`, which
  performs no I/O. A server could not close a worker's idle keep-alive
  connection, and on a bidirectional socket it could not write a reply while
  another thread read. Readers are now serialized on a separate lock and wait
  with the state mutex released
  ([#353](https://github.com/atgreen/evergreen/pull/353);
  issue [#350](https://github.com/atgreen/evergreen/issues/350)).
- The same fix now covers composite streams. A `TWO-WAY`, `ECHO`,
  `CONCATENATED` or `SYNONYM` stream reached its component while still holding
  its own lock, so a read parked on the component locked every other thread out
  of the composite -- and `*TERMINAL-IO*` is bidirectional
  ([#354](https://github.com/atgreen/evergreen/pull/354);
  issue [#350](https://github.com/atgreen/evergreen/issues/350)).
- Character reads and process-pipe reads use the same non-parking path as byte
  reads, so a read that waits for input holds no stream mutex. Regular file I/O
  is deliberately excluded: a readiness poll always reports a regular
  descriptor ready, so polling cannot make a disk read cooperative, which is
  also where the JVM leaves file I/O for its virtual threads
  ([#365](https://github.com/atgreen/evergreen/pull/365);
  issue [#350](https://github.com/atgreen/evergreen/issues/350)).
- `READ-SEQUENCE` on a socket no longer deadlocks other threads using that
  stream. It was the last read that still waited while holding the stream's
  state mutex, and the reachable one: socket streams are `(unsigned-byte 8)`,
  so `READ-SEQUENCE` is how a library reads a socket. Any other thread touching
  the stream blocked -- `CLOSE`, a write on the other half, even
  `STREAM-ELEMENT-TYPE`, which performs no I/O -- and because the thread that
  would supply the bytes was usually the blocked one, the program deadlocked
  rather than merely stalling. `READ-SEQUENCE` now waits with the mutex
  released and resumes where it left off, so a request split across several
  arrivals still fills completely and a short count still means end of file
  ([#367](https://github.com/atgreen/evergreen/pull/367);
  issue [#350](https://github.com/atgreen/evergreen/issues/350)).
- `COMPILE-FILE` now gives the body of a load-time-only top-level `EVAL-WHEN`
  the top-level processing CLHS 3.2.3.1 requires. Without `:COMPILE-TOPLEVEL`
  but with `:LOAD-TOPLEVEL`, the body forms are still top-level forms, so a
  nested `(eval-when (:compile-toplevel) ...)`, a `DEFMACRO` or a `DEFPACKAGE`
  among them keeps its compile-time effect. The whole form was evaluated
  instead, which answered NIL for the outer form and never reached the inner
  one: cl-store's `DEFBACKEND` expands to exactly that nest, so every later use
  in the file compiled as a function call and the fasl died loading with
  "STREAM is unbound"
  ([#355](https://github.com/atgreen/evergreen/pull/355)).
- `GET-DISPATCH-MACRO-CHARACTER` now reports a callable function for each
  standard `#` sub-character (CLHS 2.4.8) rather than NIL for all but `#\`. A
  reader that wraps the standard syntax -- zacl rewrites Allegro's
  `(version>= ...)` features and hands the rest back to the standard `#+`
  reader -- died calling NIL
  ([#356](https://github.com/atgreen/evergreen/pull/356)).
- A control transfer out of a reader macro now passes through `COMPILE-FILE`'s
  pre-read. A `HANDLER-CASE` clause around `COMPILE-FILE` taking a condition a
  reader macro signalled, or an invoked restart, was reported as a read failure
  and silently downgraded the output to a source-only artifact, so the clause
  never ran
  ([#357](https://github.com/atgreen/evergreen/pull/357)).
- The reader now accepts the character names SBCL and Clozure CL share:
  `Ff`, `Formfeed`, `Lf`, `Cr`, `Bs`, `Ht`, `Sp`, `Del` and `Altmode`.
  `CHAR-NAME` still answers the standard names, so printing round-trips are
  unchanged ([#358](https://github.com/atgreen/evergreen/pull/358)).
- The reader now closes the list when a Lisp reader-macro function returns no
  values immediately before the closing parenthesis, as it already did for
  `#|...|#`. The list previously lost its terminator
  ([#359](https://github.com/atgreen/evergreen/pull/359)).
- `LISTEN` no longer consumes the pending byte of an octet stream. It was
  `READ-CHAR-NO-HANG` followed by `UNREAD-CHAR` for every stream, which on an
  octet socket parked the byte in the character unread slot where `READ-BYTE`
  never looks: `LISTEN` answered T and the `READ-BYTE` after it hung or lost an
  octet outright. usocket's `WAIT-FOR-INPUT` calls `LISTEN` on every stream
  socket before its backend runs, so AllegroServe hung on its first read after
  every wait ([#360](https://github.com/atgreen/evergreen/pull/360)).
- `FUNDAMENTAL-STREAM` now sits under `STREAM`, so a method on `STREAM` applies
  to a Gray stream and `(typep gray-stream 'stream)` is true
  ([#362](https://github.com/atgreen/evergreen/pull/362)).
- `DEFINE-SYMBOL-MACRO` definitions now live in one process-wide table, so a
  symbol macro defined on one thread is visible on every other. Each thread
  previously kept its own, and a definition made before a thread started was
  invisible inside it
  ([#363](https://github.com/atgreen/evergreen/pull/363)).

### Performance

- Generic-function calls do less repeated work. A `DEFCLASS` accessor reads
  or writes its slot directly once dispatch selects it (user methods on the
  accessor still take over), slot writes skip the class-slot search for
  ordinary slots, and per-generic and per-class facts are cached. From
  compiled callers an accessor read takes about 3 us instead of 11, a write 8
  instead of 23, and `READ-BYTE` on a Gray stream 40 instead of about 110; a
  repeated HTTPS request through drakma and pure-tls is about twice as fast
  ([#415](https://github.com/atgreen/evergreen/pull/415)).
- A method body that loops now reaches optimized native code (T2) like the
  same body in a `DEFUN`. It used to stop at baseline native code (T1) until
  its 4096th call: a 1000-iteration loop took about 4 ms per call as a method
  and 30 us as a function, and now takes about 50 us as a method. Methods
  restored from a saved image are covered too
  ([#413](https://github.com/atgreen/evergreen/pull/413)).
- AES, GCM's GHASH multiply and SHA-256 compression run in the runtime, as
  `EGCL-EXT:CRYPTO-*` block primitives that EGCL's Ironclad fork calls in place
  of its portable Lisp. With the fork, encrypting 64 KB with AES-GCM takes
  seconds instead of 193 s, hashing it with SHA-256 takes under 0.3 s instead
  of 18 s, and a repeated HTTPS request through pure-tls is about 10x faster.
  `EGCL-EXT:SHA256` and `EGCL-EXT:CRC32` are now documented
  ([#410](https://github.com/atgreen/evergreen/pull/410)).
- Calls made from compiled (native) code into other compiled functions are
  cheaper: a call no longer looks `FUNCALL`/`APPLY` up by name, rebuilds the
  callee's per-call buffers, rescans its bytecode or registers a global root
  for it, and finding the current execution's state is quicker. A recursive
  native caller spends about 27% less per call (about 38% when it establishes a
  `HANDLER-CASE`), and a cached `(asdf:load-system :babel)` is about 5% faster
  ([#390](https://github.com/atgreen/evergreen/pull/390)).
- Compiled code finds its execution-local state with fewer checks per access,
  so a native call that establishes a `HANDLER-CASE` costs about 7% less again
  ([#397](https://github.com/atgreen/evergreen/pull/397)).

## 0.0.9 - 2026-10-15

### ANSI conformance

The Dietz ansi-test suite now runs end to end. Nine chapters pass completely:
`cons` (1882 tests), `data-and-control-flow` (1428), `misc` (740),
`conditions` (673), `strings` (509), `characters` (259), `environment` (210),
`hash-tables` (158) and `files` (87).

- `SUBTYPEP` compares numeric bounds exactly. Bounds were held as `f64`, which
  cannot tell adjacent integers apart above 2^53, so
  `(subtypep '(integer 2^64 2^64) '(integer 2^64+1 2^64+1))` answered `T, T` --
  a definite and wrong answer -- where it must answer `NIL, T`. Every bound the
  range algebra accepts (integer, ratio, single or double float) is a rational,
  so bounds are now exact rationals and every comparison is exact
  ([#337](https://github.com/atgreen/evergreen/pull/337)).
- An error or non-local exit from a user print function propagates. A
  `*PRINT-PPRINT-DISPATCH*` entry or a `PRINT-OBJECT` method that signalled was
  read as "no applicable entry", so the object printed normally and the error
  vanished: `(format nil "~S" 1)` answered `"1"` instead of signalling, and a
  `THROW` out of an entry was swallowed. The error is now re-signalled at the
  boundary of the printing operation
  ([#337](https://github.com/atgreen/evergreen/pull/337)).
- A method object reports its own qualifiers once detached from its generic
  function. `METHOD-QUALIFIERS` recovered the name of a short-form combination
  by looking up the owning generic, so a method removed with `REMOVE-METHOD`
  reported `(STANDARD)` rather than its own `(+)`. Methods now carry their
  combination, including across `SAVE-LISP-AND-DIE` and image restore
  ([#337](https://github.com/atgreen/evergreen/pull/337)).
- `ADD-METHOD` keeps a method it displaces. Replacing a method with the same
  qualifiers and specializers dropped the old one, so a program still holding
  it got a `TYPE-ERROR` from `(add-method gf displaced)` and the wrong method
  answered the call. The displaced method is now detached and can be re-added
  ([#337](https://github.com/atgreen/evergreen/pull/337)).
- `MACRO-FUNCTION` of a standard macro the evaluator implements natively returns
  an expander that actually expands. It previously handed back the form
  unchanged, so the returned function satisfied the type but did nothing
  ([#337](https://github.com/atgreen/evergreen/pull/337)).
- Run on kernels built without user shadow-stack support. On such a kernel
  (WSL2's, and many cloud VM kernels) `arch_prctl(ARCH_SHSTK_STATUS)` fails with
  `EINVAL`, which was read as "hardening state unknown" and refused, so every
  program aborted with `ordinary callable boundary is unavailable`
  ([#314](https://github.com/atgreen/evergreen/pull/314)).
- Reader: honor single escapes inside `|...|`, resolve escaped `\T` and `|NIL|`
  to the constants, read `t` and `nil` in a `#+`/`#-` feature expression as the
  keywords `:T` and `:NIL`, honor the length argument of `#n*` and `#n(`, give
  `#S` slots their initforms, and read `#nA` sequences.
  `WITH-STANDARD-IO-SYNTAX` binds `*PACKAGE*` to the package rather than its
  name ([#315](https://github.com/atgreen/evergreen/pull/315)).
- Printer and `FORMAT`: `WRITE` binds every printer-control keyword;
  `*PRINT-READABLY*` overrides `*PRINT-LENGTH*`, `*PRINT-LEVEL*` and
  `*PRINT-GENSYM*`; printing circular structure without `*PRINT-CIRCLE*` stops
  at the stack budget instead of faulting. The `~^`, `~{`, `~/name/`, `~<`,
  `~F`, `~C`, `~D` and `~R` directives follow their parameters, `FORMATTER`
  functions return the arguments they did not consume, and pprint-dispatch
  tables work ([#317](https://github.com/atgreen/evergreen/pull/317),
  [#320](https://github.com/atgreen/evergreen/pull/320),
  [#324](https://github.com/atgreen/evergreen/pull/324)).
- Streams, files and pathnames: `READ-CHAR-NO-HANG` and `LISTEN` no longer
  block on a pipe, socket or terminal with nothing buffered, and both honor a
  character pushed back with `UNREAD-CHAR`. `*TERMINAL-IO*` is bidirectional.
  Binary streams support wide `(unsigned-byte N)` element types; a broadcast
  stream reports its last component's `FILE-POSITION` and `FILE-LENGTH` and
  passes on values its components accept; and a too-large or negative
  `FILE-POSITION` signals a `TYPE-ERROR` instead of crashing the process.
  Logical pathname translations accept the ANSI list of wildcard pairs,
  `DIRECTORY` merges against the default directory, and `READ-SEQUENCE` fills a
  list ([#316](https://github.com/atgreen/evergreen/pull/316),
  [#318](https://github.com/atgreen/evergreen/pull/318),
  [#321](https://github.com/atgreen/evergreen/pull/321)).
- Symbols, macros and structures: every standard macro and function the
  evaluator implements natively reports as `FBOUNDP`, the REPL history
  variables and `*BREAK-ON-SIGNALS*` are bound, `DEFMACRO` keeps its
  documentation string, `SYMBOL-MACROLET` shadows a global symbol macro,
  `TRACE` and `UNTRACE` work, and `:TYPE`d structures are supported
  ([#319](https://github.com/atgreen/evergreen/pull/319),
  [#322](https://github.com/atgreen/evergreen/pull/322),
  [#325](https://github.com/atgreen/evergreen/pull/325)).
- CLOS: method objects are first class, with `REMOVE-METHOD`, `ADD-METHOD`,
  `COMPUTE-APPLICABLE-METHODS`, and a `FIND-METHOD` that distinguishes one
  method combination from another. `DEFCLASS` and `DEFGENERIC` syntax and
  initarg validity are checked, `SLOT-MISSING` is called, class slots shadow
  inherited instance slots, effective slots and the built-in classes follow the
  CLHS precedence order, short-form method combinations run with `:around`
  methods, and `CHANGE-CLASS` is generic
  ([#316](https://github.com/atgreen/evergreen/pull/316),
  [#323](https://github.com/atgreen/evergreen/pull/323)).
- Sequences and types: `NREVERSE` reverses a vector in place, `MAPCAR` blames
  the dotted tail in its `TYPE-ERROR`, `TYPEP` knows `GENERIC-FUNCTION` and
  `STANDARD-GENERIC-FUNCTION`, bit operations accept any bit array,
  `ADJUST-ARRAY` copies, `RATIONALIZE` round-trips through the float it came
  from, and a copied `RANDOM-STATE` replays its sequence
  ([#315](https://github.com/atgreen/evergreen/pull/315),
  [#316](https://github.com/atgreen/evergreen/pull/316),
  [#317](https://github.com/atgreen/evergreen/pull/317),
  [#325](https://github.com/atgreen/evergreen/pull/325)).
- Character names round-trip for the whitespace characters. `PRIN1` printed
  `#\No-break_space` and `#\Ideographic_space` as a literal invisible space,
  so the output could not be read back and was indistinguishable from
  `#\Space`. A character with a name now prints by name whenever it is not
  simple graphic, matching SBCL
  ([#348](https://github.com/atgreen/evergreen/pull/348)).
- A `:TYPE`d structure enforces its declared slot types. The constructor and
  the setf writers generated for a `(:type list)` or `(:type vector)` structure
  accepted any value, so a slot declared `INTEGER` silently held a keyword.
  Both now signal a `TYPE-ERROR`, including when the structure is loaded from a
  compiled file with no source present
  ([#348](https://github.com/atgreen/evergreen/pull/348)).
- A generic call and `MAKE-INSTANCE` no longer abort the process while
  validating keyword arguments. Both read their methods' lambda lists while
  holding the evaluator's method table, and naming a symbol takes the symbol
  registry, which the runtime's lock-order guard refuses in that order. A
  restored image has a cold symbol-name cache, so its first `MAKE-INSTANCE`
  tripped it; `ASDF:LOAD-SYSTEM` tripped the generic-call site on every debug
  build ([#341](https://github.com/atgreen/evergreen/pull/341)).
- `FUNCTION-KEYWORDS`, `INITIALIZE-INSTANCE`, `NO-APPLICABLE-METHOD` and
  `NO-NEXT-METHOD` are generic functions with function objects, so code that
  walks the standard generic functions -- or adds a method to one -- works
  ([#340](https://github.com/atgreen/evergreen/pull/340)).
- `RENAME-FILE` accepts an open file stream as its filespec, not only a
  pathname or namestring
  ([#339](https://github.com/atgreen/evergreen/pull/339)).
- `PARSE-INTEGER` with `:JUNK-ALLOWED` returns the index of the delimiter that
  stopped it, rather than an index past the whitespace following it
  ([#343](https://github.com/atgreen/evergreen/pull/343)).
- The Gray stream generic function `STREAM-WRITE-STRING` takes `START` and
  `END` as optional arguments, so a two-argument call from portable code finds
  an applicable method
  ([#342](https://github.com/atgreen/evergreen/pull/342)).
- The standard reader macros `GET-MACRO-CHARACTER` reports are callable
  functions. It returned a value that satisfied the type but could not be
  funcalled, so a readtable built by copying the standard one broke
  ([#346](https://github.com/atgreen/evergreen/pull/346)).
- A nested `EVAL-WHEN` inside a compile-time body is evaluated under the
  execute rule, as CLHS 3.2.3.1 requires
  ([#344](https://github.com/atgreen/evergreen/pull/344)).
- A `THROW`, `GO` or `RETURN-FROM` out of a form `COMPILE-FILE` evaluates at
  compile time passes through unchanged instead of being captured as a value
  ([#345](https://github.com/atgreen/evergreen/pull/345)).
- Loading a fasl whose package exports `T` or `NIL` no longer panics. The
  loader looked up a symbol index for the two constants, which have none
  ([#347](https://github.com/atgreen/evergreen/pull/347)).

### Performance

- Memoize tree-walking macro expansions, so an interpreted loop body expands
  its macros once instead of once per iteration. `ASSERT` expands a `LOOP`,
  which made interpreted test loops far slower than necessary; a
  2000-iteration `ASSERT` loop now runs in 14 ms. Memoized expansions are
  dropped at each `EVAL` boundary, so a form mutated between two evaluations is
  expanded again rather than reusing the first expansion
  ([#321](https://github.com/atgreen/evergreen/pull/321),
  [#322](https://github.com/atgreen/evergreen/pull/322)).

### Correctness

- Root printed and formatted values across calls back into Lisp — the
  pretty-printer dispatch hook and the `~{` iteration body function — so a
  relocating garbage collection during a user print function or `FORMATTER`
  cannot leave the caller holding a stale object
  ([#320](https://github.com/atgreen/evergreen/pull/320),
  [#324](https://github.com/atgreen/evergreen/pull/324)).
- Check supported primitive `FTYPE` primary-result declarations even at safety
  zero, preserving multiple values and the executing definition's contract.
  Retain checks in source-free BFASL files, and enforce declared argument types
  on interpreted calls, mapped recursion, and optimized self-tail calls
  ([#331](https://github.com/atgreen/evergreen/pull/331)). The matched typed
  Fibonacci comparison measured a small slowdown from 30 ms to 31 ms median;
  see the [checked-result report](tests/benchmarks/results/2026-10-14-fib-checked-results.md).
- Raise the declared `TYPE-ERROR` when a nested recursive argument contract
  fails, instead of returning a value through a replacement definition
  ([#334](https://github.com/atgreen/evergreen/pull/334)).
- Resume a validated chain of self-inlined native frames through ordinary guard
  recovery, transferring the outer activation state exactly once and preserving
  the executing definition across redefinition and garbage collection
  ([#333](https://github.com/atgreen/evergreen/pull/333),
  [#334](https://github.com/atgreen/evergreen/pull/334)).

### Known limitations

- `--shake` retains every candidate function instead of pruning the
  unreachable ones, so a shaken executable is correct but no smaller than the
  image it came from. The conformance work in this release took the saved image
  from 3 source closures to 30, and the reader entry points quoted inside them
  reach the analyzer's "can invoke arbitrary code" rule, which then keeps
  everything. The shaker is being conservative rather than wrong -- nothing is
  removed that a program needs -- and `dynamic = preserve` behaves as before.
  Tracked in `bliss-mihqk`; the six `shake_cli` pruning tests are marked
  `#[ignore]` against it.

## 0.0.8 - 2026-10-14

### Native compilation and performance

- Compile eligible recursive self-calls directly through the x86-64 Lisp ABI,
  preserving `NOTINLINE`, lexical function bindings, and the executing definition
  across redefinition and garbage collection. Retain this behavior in source-free
  BFASL files ([#307](https://github.com/atgreen/evergreen/pull/307)).
- Reduce ordinary T2 call overhead with cheaper multiple-value handling,
  smaller frames, execution-local polling, and fewer redundant checks. Let
  proven noncollecting entry paths, including simple recursive base cases,
  return before constructing a native frame
  ([#308](https://github.com/atgreen/evergreen/pull/308),
  [#309](https://github.com/atgreen/evergreen/pull/309),
  [#310](https://github.com/atgreen/evergreen/pull/310),
  [#326](https://github.com/atgreen/evergreen/pull/326)).
- Preserve bounded integer parameter ranges in T2. Use proven numeric bounds
  to remove redundant recursive argument checks and add/subtract overflow
  checks, while retaining checks where safety cannot be proved. Optimize
  eligible pure return paths with bounded partial evaluation
  ([#326](https://github.com/atgreen/evergreen/pull/326)).
- Eliminate redundant native frame-slot reloads while retaining the stores
  needed for garbage collection and recovery
  ([#326](https://github.com/atgreen/evergreen/pull/326), incorporating the reload
  optimization from [#311](https://github.com/atgreen/evergreen/pull/311)).

Benchmark details and caveats are recorded in the
[checked-range recursion report](tests/benchmarks/results/2026-10-14-fib-range-proofs.md).
Its measured release revision ran warmed, bounded-integer `fib(35)` in a median
30 ms on the benchmark host, versus 33 ms for an implementation-specifically
tuned SBCL version. This is a workload-specific comparison, not a general
performance claim or a matched v0.0.7-to-v0.0.8 comparison.

### Fiber scheduling and cancellation

- Schedule fibers cooperatively without automatic CPU time slices or per-fiber
  CPU budgets. Add cooperative cancellation with explicit checkpoints and
  interruptible public sleep, park, and join; retain opt-in sandbox CPU limits
  ([#326](https://github.com/atgreen/evergreen/pull/326), incorporating
  [#312](https://github.com/atgreen/evergreen/pull/312)).

### Correctness and compatibility

- Accept global `INLINE` and `NOTINLINE` declarations for SETF function names
  without confusing their policies with the corresponding getters, fixing
  ASDF loading ([#326](https://github.com/atgreen/evergreen/pull/326)).
- Enforce declared argument bounds on direct recursive calls, including calls
  replaced by pure return shortcuts. Correct constant-left subtraction when
  the result reuses the right operand's register
  ([#326](https://github.com/atgreen/evergreen/pull/326)).
- Retain global `FTYPE` declaration metadata during compilation and source-free
  BFASL loading ([#326](https://github.com/atgreen/evergreen/pull/326)).
- Reject oversized typed allocations before their body lengths or aligned
  footprints overflow GC size metadata
  ([#302](https://github.com/atgreen/evergreen/pull/302)).
- Convert huge rational values to floating point without intermediate infinity,
  preserving subnormal values and correct rounding
  ([#304](https://github.com/atgreen/evergreen/pull/304)).
- Preserve large-object identities, payload references, and true header pinning
  when restoring and resaving heap images
  ([#305](https://github.com/atgreen/evergreen/pull/305)).

### Tooling

- Label native profiler maps with the actual T1, T2, or OSR tier
  ([#303](https://github.com/atgreen/evergreen/pull/303)).

### Scope limits

Retaining `FTYPE` metadata does not yet enable result-type enforcement or
optimization based on declared result types. General recursive inlining with
nested deoptimization and a private unboxed local-call ABI remain separate work.

## 0.0.7 - 2026-10-12

- Use the new x86-64 Linux Lisp register ABI for supported T1/T2 functions,
  including recursive and cross-function calls, branches, global operations,
  and stack arguments. Preserve multiple values, moving-GC roots, and function
  redefinition across native calls and interpreter adapters; avoid false stack
  overflow errors on worker threads and foreign callbacks
  ([#298](https://github.com/atgreen/evergreen/pull/298),
  [#299](https://github.com/atgreen/evergreen/pull/299),
  [#301](https://github.com/atgreen/evergreen/pull/301)).
- Add eligible native tail calls and resume failed optimization guards without
  repeating completed calls
  ([#286](https://github.com/atgreen/evergreen/pull/286),
  [#292](https://github.com/atgreen/evergreen/pull/292)).
- Support packed `DOUBLE-FLOAT` arrays, including adjustable and displaced
  arrays. Compile eligible array arithmetic to unboxed native operations and
  reduce the instructions needed for double addition and multiplication
  ([#278](https://github.com/atgreen/evergreen/pull/278),
  [#290](https://github.com/atgreen/evergreen/pull/290),
  [#295](https://github.com/atgreen/evergreen/pull/295)).
- Reduce idle T2 loop-poll overhead on Linux x86-64; poll every selected T2
  backedge on AArch64, POWER, RISC-V, and System Z
  ([#272](https://github.com/atgreen/evergreen/pull/272),
  [#279](https://github.com/atgreen/evergreen/pull/279)).
- Scan overlapping native GC root slots once while preserving distinct copies
  of live values ([#274](https://github.com/atgreen/evergreen/pull/274)).

The new ABI migration remains incomplete: protected control scopes, raw numeric
bodies, and some multiple-value consumers still use compatible execution paths.

## 0.0.6 - 2026-10-11

This release also includes the [0.0.5 changes](https://github.com/atgreen/evergreen/blob/v0.0.6/CHANGELOG.md#005---2026-10-10).

- Speed up T2 native recursion on Linux x86-64 with machine-stack self-calls,
  cheaper returns, fewer multiple-value resets, and correct fiber stack limits.
  A release-build `fib(32)` comparison improved from 1.04 s to 0.21–0.23 s
  ([#248](https://github.com/atgreen/evergreen/pull/248),
  [#259](https://github.com/atgreen/evergreen/pull/259),
  [#260](https://github.com/atgreen/evergreen/pull/260),
  [#262](https://github.com/atgreen/evergreen/pull/262)).
- Remove invocation counting from ordinary T2 execution.
  `FUNCTION-INVOKE-COUNT` now reports tiering heat; use `PROFILE` for exact counts
  ([#261](https://github.com/atgreen/evergreen/pull/261)).
- Correct `DEFTYPE` defaults, lexical and dynamic bindings, named returns, and
  documentation; preserve `SPECIAL` parameters in compiled closures. Evaluate
  optional `TYPEP`/`SUBTYPEP` arguments and enforce `TYPEP` errors through `FUNCALL`
  ([#255](https://github.com/atgreen/evergreen/pull/255),
  [#256](https://github.com/atgreen/evergreen/pull/256),
  [#257](https://github.com/atgreen/evergreen/pull/257)).
- Accept repeated `DEFPACKAGE` option names without losing conflict checks;
  fix `APROPOS` handling of `NIL`, `T`, and invalid argument counts
  ([#250](https://github.com/atgreen/evergreen/pull/250),
  [#253](https://github.com/atgreen/evergreen/pull/253)).
- Accept open and closed file streams as pathname designators, retaining their
  opening paths when defaults change
  ([#252](https://github.com/atgreen/evergreen/pull/252)).
- Preserve saved builtin function identity and printing across replacement and
  restoration; protect large rational numerators during moving GC
  ([#249](https://github.com/atgreen/evergreen/pull/249),
  [#251](https://github.com/atgreen/evergreen/pull/251),
  [#263](https://github.com/atgreen/evergreen/pull/263)).

## 0.0.5 - 2026-10-10

### Performance and diagnostics

- Improve native execution performance on Linux x86-64: restore efficient
  direct recursive calls in eligible T2 code, and reduce thread-lookup and
  idle-safepoint overhead, including redundant polls in empty jump blocks.
  Remove per-operation function-binding checks for standard CL primitives.
  Recursive calls now count toward automatic T2
  promotion; cold or wrong-arity interpreter fallbacks count only once.
  Keep loop counters attached to the running function during redefinition or
  unbinding, avoiding invalid function accesses
  ([#227](https://github.com/atgreen/evergreen/pull/227),
  [#228](https://github.com/atgreen/evergreen/pull/228),
  [#230](https://github.com/atgreen/evergreen/pull/230),
  [#231](https://github.com/atgreen/evergreen/pull/231),
  [#232](https://github.com/atgreen/evergreen/pull/232),
  [#233](https://github.com/atgreen/evergreen/pull/233),
  [#234](https://github.com/atgreen/evergreen/pull/234),
  [#235](https://github.com/atgreen/evergreen/pull/235),
  [#237](https://github.com/atgreen/evergreen/pull/237)).

- Correct T2 diagnostics to report the profile choices actually used and the
  code actually installed; distinguish code emission from publication and
  clarify baseline arithmetic optimization. Make native compilation refusals
  searchable by function name, explain unsupported instructions, and report
  the multiple-value resets remaining in emitted code
  ([#236](https://github.com/atgreen/evergreen/pull/236),
  [#247](https://github.com/atgreen/evergreen/pull/247)).

### Compatibility

- **Compatibility:** Standard `COMMON-LISP` functions now have fixed semantics,
  following the Common Lisp standard and SBCL policy. Redefining or unbinding
  them is unsupported; this withdraws the earlier builtin-rebinding extension.
  User-defined functions, including distinct symbols created with `SHADOW`,
  remain redefinable
  ([#237](https://github.com/atgreen/evergreen/pull/237)).

### Runtime and control-flow correctness

- Let eligible running native loops on Linux x86-64 switch to optimized T2
  code without restarting, preserving completed effects, moving GC roots,
  multiple values and inherited handlers. Mapped T1/T2 now supports local
  multiple-value bindings, including those generated by `SETF`, and eligible
  child loops can optimize while their native parent is suspended. Local
  `CATCH` retains its original tag registration through optimization and guard
  recovery. `UNWIND-PROTECT` loops can optimize in protected bodies or running
  cleanups, preserving saved values and pending transfers without repeating
  cleanup. Local `HANDLER-BIND`, `HANDLER-CASE` and `RESTART-CASE` scopes
  retain their original registrations through optimization and guard recovery.
  Special-variable bindings retain their original values across these tier
  changes and restore correctly before outer cleanups, including nested
  bindings of the same variable.
  Direct recursive activations and other unsupported local scopes remain
  outside this OSR route
  ([#218](https://github.com/atgreen/evergreen/pull/218),
  [#219](https://github.com/atgreen/evergreen/pull/219),
  [#220](https://github.com/atgreen/evergreen/pull/220),
  [#223](https://github.com/atgreen/evergreen/pull/223),
  [#224](https://github.com/atgreen/evergreen/pull/224),
  [#225](https://github.com/atgreen/evergreen/pull/225),
  [#226](https://github.com/atgreen/evergreen/pull/226)).

- Preserve required type guards when optimized loops are entered through OSR
  (including nested loop entries) and reject entries with missing live values,
  preventing invalid accesses
  caused by skipped definitions
  ([#215](https://github.com/atgreen/evergreen/pull/215),
  [#216](https://github.com/atgreen/evergreen/pull/216),
  [#217](https://github.com/atgreen/evergreen/pull/217)).

- Preserve original arguments after native deoptimization and retain the callee
  in keyword-binding error backtraces. Restore supported deep recursion on
  Linux x86-64 with a larger managed-stack default, preserving stack-overflow
  conditions and explicit overrides
  ([#245](https://github.com/atgreen/evergreen/pull/245),
  [#246](https://github.com/atgreen/evergreen/pull/246)).

- Fix character counts and indexing in runtime-created Unicode strings, and
  preserve fresh-string mutability for wider characters
  ([#244](https://github.com/atgreen/evergreen/pull/244)).

### Packages

- Add an x86-64 Fedora package for building standalone RISC-V Linux
  executables with the static RV64GC musl runtime and QEMU launcher
  ([#211](https://github.com/atgreen/evergreen/pull/211)).

## 0.0.4 - 2026-10-10

### Performance and native compilation

- Reduce native call overhead on Linux x86-64: eligible T1 and T2 functions
  use the new native ABI by default, removing exceptional-status checks after
  successful returns while preserving moving GC roots, multiple values,
  speculative arithmetic, recursion and guard recovery. Unsupported bodies
  use an interpreter adapter; existing native T1 code remains when T2 declines.
  **Temporary limitation:** current-frame OSR is disabled on Linux x86-64,
  so unsupported bodies and long loops in cold functions may run slower
  ([#204](https://github.com/atgreen/evergreen/pull/204),
  [#207](https://github.com/atgreen/evergreen/pull/207),
  [#210](https://github.com/atgreen/evergreen/pull/210)).
- Optimize named calls, builtin calls, `GETHASH`, nested `FUNCALL` callbacks,
  captured-variable access, simple-string access and missed hash-table lookups
  on supported compiled paths.
  Retain replacement checks, multiple values, GC safety and error recovery
  ([#142](https://github.com/atgreen/evergreen/pull/142),
  [#145](https://github.com/atgreen/evergreen/pull/145),
  [#149](https://github.com/atgreen/evergreen/pull/149),
  [#150](https://github.com/atgreen/evergreen/pull/150),
  [#151](https://github.com/atgreen/evergreen/pull/151),
  [#152](https://github.com/atgreen/evergreen/pull/152),
  [#153](https://github.com/atgreen/evergreen/pull/153),
  [#155](https://github.com/atgreen/evergreen/pull/155)).
- Extend compiler support for captured lexical reads and writes, shared
  compilation across closures, nested callbacks and retained-body leaf
  inlining, preserving lexical `RETURN-FROM` and captured state during
  deoptimization. Some of these paths still require the legacy compiler;
  Linux x86-64 uses interpreter fallback where mapped native support is absent
  ([#146](https://github.com/atgreen/evergreen/pull/146),
  [#147](https://github.com/atgreen/evergreen/pull/147),
  [#148](https://github.com/atgreen/evergreen/pull/148),
  [#175](https://github.com/atgreen/evergreen/pull/175)).
- Improve numeric guards and recovery on supported compiled paths, including
  nullable values and bignums. Remove redundant multiple-value resets and loop
  tag checks, retain only live deoptimization locals, and preserve pending
  recursive operands. Repair x86-64 OSR recovery metadata for future use;
  Linux x86-64 OSR remains disabled in this release. Decline unsupported
  checked RISC-V OSR entries safely ([#100](https://github.com/atgreen/evergreen/pull/100),
  [#103](https://github.com/atgreen/evergreen/pull/103),
  [#106](https://github.com/atgreen/evergreen/pull/106),
  [#131](https://github.com/atgreen/evergreen/pull/131),
  [#135](https://github.com/atgreen/evergreen/pull/135),
  [#144](https://github.com/atgreen/evergreen/pull/144)).

### Compatibility

- Saved bytecode now uses version 2.0 and core images use format 9. Recompile
  older FASLs and rebuild older images. Saved functions retain their definitions
  and lexical captures across redefinition, aliases, unbinding and restoration
  ([#174](https://github.com/atgreen/evergreen/pull/174),
  [#202](https://github.com/atgreen/evergreen/pull/202),
  [#204](https://github.com/atgreen/evergreen/pull/204)).

### Calling and control-flow correctness

- Keep each function's selected definition, captures and compiled code together
  across concurrent redefinition and tier promotion, including callable-instance
  replacements and saved source-backed functions. Preserve closure function and
  control scopes through installation and image restoration, and resolve symbol
  `FUNCALL`/`APPLY` targets in the global function namespace
  ([#148](https://github.com/atgreen/evergreen/pull/148),
  [#192](https://github.com/atgreen/evergreen/pull/192),
  [#193](https://github.com/atgreen/evergreen/pull/193),
  [#204](https://github.com/atgreen/evergreen/pull/204)).
- Honor replacement builtins, readers and memory fences in interpreted, compiled
  and saved code, including lexical `VALUES` and `BYTE`, `LDB`, `TYPEP`,
  `FIRST-CHAR` and returning `ERROR` replacements. Preserve replacement functions'
  multiple values and nonlocal exits
  ([#194](https://github.com/atgreen/evergreen/pull/194),
  [#195](https://github.com/atgreen/evergreen/pull/195),
  [#196](https://github.com/atgreen/evergreen/pull/196),
  [#197](https://github.com/atgreen/evergreen/pull/197),
  [#202](https://github.com/atgreen/evergreen/pull/202)).
- Prevent stale secondary values from leaking through optimized primitives,
  predicates, accessors and numeric operations. Preserve single-float T2
  comparisons and correct spilled memory-barrier results
  ([#132](https://github.com/atgreen/evergreen/pull/132),
  [#198](https://github.com/atgreen/evergreen/pull/198),
  [#199](https://github.com/atgreen/evergreen/pull/199)).
- Preserve the exact restart selected by `CERROR`, even when an outer restart
  is also named `CONTINUE`; restore special bindings before cleanup and
  destination clauses, including transfers that replace an earlier unwind
  ([#201](https://github.com/atgreen/evergreen/pull/201)).
- Fix `EQUALP` hash-table operations across integer and signed single-float zero,
  and signal `TYPE-ERROR` for improper property lists passed to `GETF`
  ([#89](https://github.com/atgreen/evergreen/pull/89),
  [#154](https://github.com/atgreen/evergreen/pull/154)).

### Native exceptional transfers

These transfers are enabled by default on x86-64 Linux. Full compiled-shape
coverage and the rollout on other platforms remain incomplete.

- Keep eligible recursive calls, calls between T2 definitions, `FUNCALL`, `APPLY`
  and protected functions in native segments. Preserve live linkage, callable
  identity, cleanup, restarts, multiple values and moving arguments across
  legacy calls, Rust reentry and fallback
  ([#157](https://github.com/atgreen/evergreen/pull/157),
  [#164](https://github.com/atgreen/evergreen/pull/164),
  [#165](https://github.com/atgreen/evergreen/pull/165),
  [#166](https://github.com/atgreen/evergreen/pull/166),
  [#171](https://github.com/atgreen/evergreen/pull/171),
  [#172](https://github.com/atgreen/evergreen/pull/172)).
- Compile fixnum arithmetic and recover speculative guards without replaying
  effects or losing callers. Reduce root bookkeeping and unnecessary fallback;
  support rooted heap literals and debugger arguments. Keep rejection caches
  and deoptimization feedback tied to the correct definition
  ([#158](https://github.com/atgreen/evergreen/pull/158),
  [#162](https://github.com/atgreen/evergreen/pull/162),
  [#168](https://github.com/atgreen/evergreen/pull/168),
  [#187](https://github.com/atgreen/evergreen/pull/187),
  [#189](https://github.com/atgreen/evergreen/pull/189),
  [#194](https://github.com/atgreen/evergreen/pull/194)).
- Preserve continuations and roots across fiber suspension and migration,
  validate destination-worker compatibility, and release retained execution
  state when it stops. Preserve all ordinary and interactive restart return
  values. `DISASSEMBLE` distinguishes cached transfer code and the actual ABI
  of installed code ([#156](https://github.com/atgreen/evergreen/pull/156),
  [#159](https://github.com/atgreen/evergreen/pull/159),
  [#160](https://github.com/atgreen/evergreen/pull/160),
  [#168](https://github.com/atgreen/evergreen/pull/168)).

### Applications and deployment

- Embed read-only build-host files into saved images with `egcl-ext:embed-file`
  and ASDF components from `egcl-embed-asdf`, so applications can read them
  after deployment without the original files
  ([#173](https://github.com/atgreen/evergreen/pull/173)).

- Rename the tree-shaking command to `--shake`, its manifest to
  `egcl-shake-manifest`, and its ASDF extension to `egcl-shake-asdf`
  ([#163](https://github.com/atgreen/evergreen/pull/163)).

- Describe shaken executables in ASDF system definitions with
  `egcl-shake-asdf`; `asdf:make` writes the specification, saves the core
  and runs the tree shaker to produce the executable
  ([#161](https://github.com/atgreen/evergreen/pull/161)).

### Platform support

- Restore RISC-V loop promotion into T2 by checking imported fixnum values
  when entering optimized loops, with recovery for failed checks
  ([#140](https://github.com/atgreen/evergreen/pull/140)).
- Add opt-in published native callable entries for Rust-to-Lisp calls on
  AArch64 Unix with `EGCL_NATIVE_TRANSFER=1`
  ([#206](https://github.com/atgreen/evergreen/pull/206)).
- Decode RISC-V native instructions in process for `DISASSEMBLE`, including
  compressed instructions, ABI register names and branch targets
  ([#136](https://github.com/atgreen/evergreen/pull/136)).
- Decode POWER native instructions in process for `DISASSEMBLE`, with
  branch targets, OSR entries, runtime helper names and tagged constants
  ([#139](https://github.com/atgreen/evergreen/pull/139)).
- **Windows x86-64:** add opt-in native callable entry boundaries with rooted
  arguments and Windows unwind metadata. Enable with `EGCL_NATIVE_TRANSFER=1`;
  unsupported process mitigation policies produce an explicit error. Compiled
  bodies retain their existing ABI behind the boundary
  ([#205](https://github.com/atgreen/evergreen/pull/205)).
- **s390x:** support ELF-ABI foreign calls and Lisp callbacks, including floating
  point, narrow integers, large argument lists and supported variadic calls
  ([#119](https://github.com/atgreen/evergreen/pull/119),
  [#133](https://github.com/atgreen/evergreen/pull/133),
  [#141](https://github.com/atgreen/evergreen/pull/141)).
- **s390x:** reduce T1/T2 direct-call, recursive-call and builtin-call overhead;
  improve fixnum guards, constants and register saving. Direct recursive
  deoptimization preserves arguments and backtraces; wrong-arity T2 self-calls
  signal `PROGRAM-ERROR` on every target
  ([#77](https://github.com/atgreen/evergreen/pull/77),
  [#80](https://github.com/atgreen/evergreen/pull/80),
  [#88](https://github.com/atgreen/evergreen/pull/88),
  [#94](https://github.com/atgreen/evergreen/pull/94),
  [#97](https://github.com/atgreen/evergreen/pull/97),
  [#107](https://github.com/atgreen/evergreen/pull/107),
  [#108](https://github.com/atgreen/evergreen/pull/108),
  [#111](https://github.com/atgreen/evergreen/pull/111),
  [#113](https://github.com/atgreen/evergreen/pull/113),
  [#115](https://github.com/atgreen/evergreen/pull/115),
  [#122](https://github.com/atgreen/evergreen/pull/122)).
- **s390x:** compile more predicates, simple-string operations, closure factories,
  fixnum multiplication and memory barriers, including saved bytecode. Improve
  compilation diagnostics and increase the default CL stack to 4 MiB
  (512 KiB elsewhere)
  ([#91](https://github.com/atgreen/evergreen/pull/91),
  [#99](https://github.com/atgreen/evergreen/pull/99),
  [#101](https://github.com/atgreen/evergreen/pull/101),
  [#102](https://github.com/atgreen/evergreen/pull/102),
  [#118](https://github.com/atgreen/evergreen/pull/118),
  [#126](https://github.com/atgreen/evergreen/pull/126),
  [#127](https://github.com/atgreen/evergreen/pull/127)).
- **ppc64le:** correct T2 single-float arithmetic; inline more baseline numeric
  operations and optimize helper calls. Enable T2 loops, more predicates and
  bit operations, timely loop polling, and live T1-to-T2 loop replacement
  ([#95](https://github.com/atgreen/evergreen/pull/95),
  [#96](https://github.com/atgreen/evergreen/pull/96),
  [#98](https://github.com/atgreen/evergreen/pull/98),
  [#104](https://github.com/atgreen/evergreen/pull/104),
  [#105](https://github.com/atgreen/evergreen/pull/105),
  [#110](https://github.com/atgreen/evergreen/pull/110),
  [#114](https://github.com/atgreen/evergreen/pull/114)).
- **riscv64:** add baseline and optimizing native compilers with live OSR and
  deoptimization, native fiber switching, scalar LP64D foreign calls including
  variadics, recoverable null/stack-guard faults, and an opt-in native segment
  boundary that preserves integer and floating-point registers
  ([#93](https://github.com/atgreen/evergreen/pull/93),
  [#112](https://github.com/atgreen/evergreen/pull/112),
  [#120](https://github.com/atgreen/evergreen/pull/120),
  [#121](https://github.com/atgreen/evergreen/pull/121),
  [#123](https://github.com/atgreen/evergreen/pull/123),
  [#128](https://github.com/atgreen/evergreen/pull/128)).

### Quicklisp and library compatibility

- Add an x86-64 Linux Quicklisp client port with pinned setup instructions,
  native networking and filesystem adapters, fresh installation and offline
  reload. Honor custom `*MACROEXPAND-HOOK*` functions, accept file streams in
  `PROBE-FILE`, and fix wildcard-directory traversal for distribution discovery
  ([#86](https://github.com/atgreen/evergreen/pull/86),
  [#87](https://github.com/atgreen/evergreen/pull/87),
  [#90](https://github.com/atgreen/evergreen/pull/90),
  [#92](https://github.com/atgreen/evergreen/pull/92)).

## 0.0.3 - 2026-10-07

### Common Lisp support

- `EGCL-GRAY-STREAMS` now exports `STREAM-READ-SEQUENCE` and
  `STREAM-WRITE-SEQUENCE`, and `READ-SEQUENCE` / `WRITE-SEQUENCE` on a Gray
  stream dispatch through them with one bulk call, falling back to the scalar
  generics by default. Portable libraries that specialize the
  trivial-gray-streams sequence methods are now reached from the standard
  entry points ([#81](https://github.com/atgreen/evergreen/pull/81)).

- Add `PPRINT-LOGICAL-BLOCK`, its lexical list-traversal helpers, conditional
  line breaks and indentation, and `PPRINT-FILL` / `PPRINT-LINEAR`. Nested
  blocks honor prefixes, suffixes, print limits, and circular-list labels,
  allowing Quicklisp's setup code to compile past its missing-macro failure
  ([#55](https://github.com/atgreen/evergreen/pull/55)).

### Performance

- `MULTIPLE-VALUE-PROG1` now compiles to bytecode and baseline native code,
  allowing Ironclad's Keccak implementation to tier instead of remaining
  interpreted, including after compiled-file loading
  ([#56](https://github.com/atgreen/evergreen/pull/56)).

- Functions using `CAR`, `CDR`, bitwise operations, shifts, or power-of-two
  `MOD` now reach T2 native code on s390x instead of stopping at T1
  ([#62](https://github.com/atgreen/evergreen/pull/62)).

- `EQ`, `NULL`, and `NOT` now reach T2 native code on s390x
  ([#65](https://github.com/atgreen/evergreen/pull/65)).

- `SEARCH` now advances list candidates without restarting each traversal and
  searches vectors without copying the entire target, avoiding long stalls
  while libraries scan large text such as TLS certificate bundles
  ([#67](https://github.com/atgreen/evergreen/pull/67)).

- Constant-kind memory barriers now use dedicated bytecode and x86 native
  fence instructions, including after compiled-file loading
  ([#30](https://github.com/atgreen/evergreen/pull/30)).

### Platform support

- Restore Windows builds and stack-limit checks by selecting the Windows OS
  backend correctly ([#85](https://github.com/atgreen/evergreen/pull/85)).

- Evergreen now builds and runs natively on riscv64 Linux (RV64GC). The
  interpreter and bytecode tiers, compiled-file loading, and heap images
  work; saved images carry a distinct `RISCV64` tag, and `*features*`
  includes `:riscv` and `:riscv64`. Native compilation and the foreign-call
  ABI are not yet ported ([#82](https://github.com/atgreen/evergreen/pull/82)).

- Recoverable null-pointer and stack-guard faults on s390x now resume at the
  runtime recovery handler instead of terminating the process
  ([#64](https://github.com/atgreen/evergreen/pull/64)).

- `DISASSEMBLE` now decodes s390x native code in-process, with mnemonics,
  operands and branch targets in binutils syntax, instead of listing raw
  bytes ([#74](https://github.com/atgreen/evergreen/pull/74)).

- The EGCL CLI now builds and runs natively on Apple Silicon macOS from source
  ([#51](https://github.com/atgreen/evergreen/pull/51)).

### Correctness and reliability

- `OPEN :DIRECTION :PROBE` now returns a closed stream and honors
  `:IF-DOES-NOT-EXIST :CREATE`, allowing Quicklisp to enable installed
  distributions. Input streams also honor explicit file creation without
  truncating existing content ([#83](https://github.com/atgreen/evergreen/pull/83)).

- Unbound slot reads now call user-defined `SLOT-UNBOUND` methods through both
  `SLOT-VALUE` and generated accessors, enabling lazy slot initialization such
  as Quicklisp's distribution directories. The default method signals
  `UNBOUND-SLOT` with the offending slot and instance
  ([#78](https://github.com/atgreen/evergreen/pull/78)).

- `ENSURE-DIRECTORIES-EXIST` now creates the final directory component of a
  directory-only pathname, allowing fresh Quicklisp distribution installations
  to create their metadata directories
  ([#79](https://github.com/atgreen/evergreen/pull/79)).

- Floating-point `FORMAT` directives now accept ratios and bignums, allowing
  Quicklisp download progress to display sizes and rates. Large ratio
  components are scaled safely, out-of-range rational arguments signal an
  arithmetic error, and negative-zero signs are preserved
  ([#76](https://github.com/atgreen/evergreen/pull/76)).

- `DIRECTORY` now returns `NIL` for wildcard searches under missing
  directories, allowing fresh Quicklisp setup without a pre-created
  `local-init/` directory ([#75](https://github.com/atgreen/evergreen/pull/75)).

- `WRITE` now uses the dynamic `*PRINT-ESCAPE*` value when `:ESCAPE` is omitted
  ([#55](https://github.com/atgreen/evergreen/pull/55)).

- `TYPE-OF` now preserves the defining package of structure, class, and
  condition names, fixing cross-package digest copying used by TLS key
  derivation ([#57](https://github.com/atgreen/evergreen/pull/57)).

- Extension macros, including `EGCL-EXT:CAS` and atomic arithmetic, now expand
  consistently in interpreted and compiled code. Local function shadowing and
  definition-time macro snapshots are preserved
  ([#58](https://github.com/atgreen/evergreen/pull/58)).

- The EGCL fork installer now includes the Atomics CAS adapter, supporting
  CAS-dependent initialization in libraries such as `cl-cancel`
  ([#59](https://github.com/atgreen/evergreen/pull/59)).

- Native TCP listeners can now be shared across threads and fibers, honor
  backlog and address-reuse options, and use GC-safe cooperative accept and
  readiness waits on Unix; closing a listener wakes pending waits
  ([#60](https://github.com/atgreen/evergreen/pull/60)).

- Native threads now share their creator's package registry, preserving library
  symbol lookup in workers and threads created by fibers. This fixes Ironclad
  reporting SHA256 as unsupported in TLS server threads
  ([#68](https://github.com/atgreen/evergreen/pull/68)).

- `ENCODE-UNIVERSAL-TIME` now rejects invalid fields and preserves exact
  fractional time-zone offsets and large years
  ([#69](https://github.com/atgreen/evergreen/pull/69)).

- Compiled `HANDLER-BIND` now preserves lexical handler functions and initializer
  behavior, including native execution and garbage collection
  ([#70](https://github.com/atgreen/evergreen/pull/70)).

- `CHANGE-CLASS` now honors initialization arguments and class-change hooks,
  preserving retained slot values and initializing newly added slots
  ([#71](https://github.com/atgreen/evergreen/pull/71)).

- Native socket byte-read timeouts now signal `EGCL-EXT:IO-TIMEOUT`, retaining
  the affected stream so callers can distinguish expiry from other I/O errors
  ([#72](https://github.com/atgreen/evergreen/pull/72)).

- The USOCKET fork now supports binary TCP listeners, cross-thread accept and
  close, listener port queries, explicit address-reuse options, and inherited
  or independently configured read timeouts with `USOCKET:TIMEOUT-ERROR` mapping
  ([#61](https://github.com/atgreen/evergreen/pull/61)).

- `EQ` comparisons between constant operands now compile at T2, including
  comparisons used as values or branch conditions
  ([#50](https://github.com/atgreen/evergreen/pull/50)).

- Macro-generated closures now retain their enclosing `TAGBODY` exits, while
  loops without captured exits can reach T2 compilation
  ([#49](https://github.com/atgreen/evergreen/pull/49)).

- Native code now preserves live immediate values across inserted loop and
  straight-line polls, preventing runtime calls from clobbering those values
  ([#40](https://github.com/atgreen/evergreen/pull/40)).

- Native-enabled startup no longer hangs while compiling declaration-scanning
  loops; call results now retain conservative type facts at control-flow merges
  ([#38](https://github.com/atgreen/evergreen/pull/38)).

- `SETQ` and variable `SETF` now reject constants defined after the assigning
  function was compiled, and saved core images retain constant declarations
  ([#44](https://github.com/atgreen/evergreen/pull/44)).

- Repeated `POSITION`, `POSITION-IF`, and `POSITION-IF-NOT` searches now avoid
  excessive memory growth from compiling a new predicate helper on every call
  ([#42](https://github.com/atgreen/evergreen/pull/42)).

- Bit-vector results from `COPY-SEQ`, `SUBSEQ`, and `REVERSE` now preserve
  zero bits when garbage collection recycles storage
  ([#41](https://github.com/atgreen/evergreen/pull/41)).

- Generated `DEFSTRUCT` setters now enforce slot types before mutation,
  including inherited constraints and list/vector-backed structures
  ([#37](https://github.com/atgreen/evergreen/pull/37)).

- `COMPILE-FILE` no longer runs ordinary `DEFVAR` initializers early, and
  explicit compile-time initialization follows source order while existing
  bindings remain unchanged
  ([#36](https://github.com/atgreen/evergreen/pull/36)).

- Compile-time traversal now preserves remaining definitions when garbage
  collection relocates the forms being processed
  ([#35](https://github.com/atgreen/evergreen/pull/35)).

- `COMPILE-FILE` now leaves ordinary `DEFPARAMETER` initialization until load
  time, preserving existing values and avoiding duplicate initializer effects
  while retaining compile-time special declarations
  ([#34](https://github.com/atgreen/evergreen/pull/34)).

- `DEFSTRUCT` constructors now enforce declared slot types, including defaults,
  inherited constraints, BOA constructors, and list/vector-backed structures
  ([#31](https://github.com/atgreen/evergreen/pull/31)).

- Circular reader labels now preserve identity in vectors, mixed object graphs,
  arrays, and structure literals, including source-free compiled files
  ([#33](https://github.com/atgreen/evergreen/pull/33)).

- Functions returned by `MACRO-FUNCTION` now retain their macro definition
  after redefinition, and copied macro expanders survive saved-image restoration
  ([#32](https://github.com/atgreen/evergreen/pull/32)).

- Compiled `TYPEP` now returns exactly one value at every execution tier,
  without leaking secondary values from an earlier form or its argument
  ([#29](https://github.com/atgreen/evergreen/pull/29)).

## 0.0.2 - 2026-10-05

### Documentation

- Document atomic-update semantics, supported places, memory ordering, and
  current Gray-stream availability
  ([#23](https://github.com/atgreen/evergreen/pull/23)).

### Common Lisp extensions

- `EGCL-EXT:CAS`, `ATOMIC-INCF`, and `ATOMIC-DECF` now provide lock-free
  updates for supported places
  ([#20](https://github.com/atgreen/evergreen/pull/20)).

### Correctness and reliability

- Drakma plain HTTP GET and POST now work through the native TCP backend.
  The fork installer selects USOCKET's EGCL branch, and Gray-stream predicates,
  EOF handling, binary sequence reads, and array element-type aliases behave
  correctly for its stream wrappers
  ([#22](https://github.com/atgreen/evergreen/pull/22)).

- Retained generic-function values now continue dispatching through their
  generic methods after the function name is replaced by an advice closure
  ([#19](https://github.com/atgreen/evergreen/pull/19)).

- `REMF`, `PSETF`, `SHIFTF`, `ROTATEF`, and `SETF` of `VALUES` now honor
  custom place storing forms and preserve multiple store values
  ([#18](https://github.com/atgreen/evergreen/pull/18)).

- Mutexes restored from saved images now retain their names, recursive behavior,
  and shared identity while restarting unlocked with fresh native state
  ([#17](https://github.com/atgreen/evergreen/pull/17)).

- `UNINTERN` now removes the requested symbol by identity without folding its
  name, checks shadowing conflicts before changing the package, and reports
  invalid symbols and missing packages
  ([#16](https://github.com/atgreen/evergreen/pull/16)).

- Symbol lookup now preserves literal colons in imported and keyword names,
  including lookup through exports, inherited symbols, and the reader
  ([#15](https://github.com/atgreen/evergreen/pull/15)).

- `SHADOWING-IMPORT` now accepts fresh and previously uninterned symbols,
  preserving their names and identities, and propagates failures instead of
  silently reporting success
  ([#14](https://github.com/atgreen/evergreen/pull/14)).

- Package export and import operations now preserve symbol-name case, and
  export conflict checks distinguish differently cased and shadowed names
  ([#13](https://github.com/atgreen/evergreen/pull/13)).

- Reader symbol lookup now preserves escaped names and readtable case modes
  even when an uppercase symbol with the same spelling already exists
  ([#12](https://github.com/atgreen/evergreen/pull/12)).

- Compiled direct recursion now preserves errors and nonlocal exits raised
  by its stack-limit fallback instead of continuing through native callers
  ([#11](https://github.com/atgreen/evergreen/pull/11)).

- Compiled self-calls that use runtime dispatch now preserve pending errors
  and nonlocal exits instead of continuing execution after the call
  ([#10](https://github.com/atgreen/evergreen/pull/10)).

- Image-saving options now remain valid when pathname or option evaluation
  triggers a moving garbage collection, preventing corrupted option traversal
  in `SAVE-LISP-AND-DIE` and `%SAVE-CORE`
  ([#8](https://github.com/atgreen/evergreen/pull/8)).

### Debugging and observability

- Interactive debugger backtraces now preserve the failing Lisp call chain and
  available original arguments after activations unwind. Historical frames are
  labeled explicitly, and frame evaluation and locals are reported unavailable
  instead of being applied to unrelated live frames
  ([#3](https://github.com/atgreen/evergreen/pull/3)).

## 0.0.1 - 2026-10-04

This release establishes the first versioned baseline of Evergreen Common Lisp
(EGCL). EGCL is an experimental Common Lisp implementation built from scratch
in Rust. It is intended to explore a practical Lisp runtime with adaptive
native compilation, a moving collector, lightweight concurrency, and
application delivery. It targets ANSI Common Lisp, but 0.0.1 does not claim
complete conformance, compatibility stability, or production readiness.

### Execution model

- The reader, macro expander, and compiler lower supported Lisp forms to T0
  bytecode. A tree-walking evaluator remains available for bootstrap and
  compatibility paths.
- Repeatedly called functions can advance from T0 to baseline native T1 and
  optimized native T2 code. Unsupported target or operation shapes remain at a
  lower tier.
- Hot loops can enter native code during the current invocation through
  on-stack replacement (OSR).
- T2 can specialize dynamically typed operations under guarded assumptions.
  Failed guards deoptimize to a less specialized execution point while
  preserving live values and effects already performed.
- Functions remain redefinable while the program runs. Installed native code,
  OSR code, and older active definitions retain separate metadata and
  lifetimes.

### Common Lisp and runtime

- The implemented language includes the reader, packages, macros, lexical and
  dynamic binding, closures, multiple values, non-local control transfer,
  CLOS, conditions and restarts, streams, sequences, hash tables, pathnames,
  printing, and `FORMAT`. Coverage remains incomplete and is tracked by the
  specification and conformance suites.
- ASDF is included in saved distribution images. `COMPILE-FILE` produces EGCL's
  portable BFASL format for supported code, and ASDF systems can compile and
  load through the normal Lisp interfaces.
- A precise, moving generational garbage collector traces interpreted,
  bytecode, native, foreign-call, thread, and fiber roots. Stress and poison
  modes are part of runtime validation.
- Native threads provide joins, mutexes, condition variables, thread-local
  bindings, safepoints, and GC coordination.
- Stackful fibers multiplex Lisp executions over native carrier threads and
  can suspend and migrate on supported targets. Established TCP stream reads,
  writes, and readiness waits can park an unpinned fiber; connect, accept, and
  DNS operations are not yet cooperative.

### Interoperability and delivery

- The foreign function interface loads shared libraries, calls C functions,
  supports callbacks, and manages foreign memory. ABI coverage differs by
  architecture; x86-64 has the broadest call-shape support.
- The native x86-64 glibc distribution includes `egcl-jvm`, with JVM startup,
  Java calls, named bindings, Lisp implementations of Java interfaces,
  reference scopes, and shutdown. Builds configured with Python support expose
  the Lisp-facing `PY` CPython API.
- A running Lisp environment can be saved as a core image or appended to an
  executable. Images record runtime compatibility requirements and are tied to
  their architecture and runtime capabilities.
- Delivery can remove unreachable Lisp objects, functions, macros, methods,
  compiler tiers, builtin implementations, disassembly support, and the tree
  walker. A specialized delivered runtime can retain T0 only, T0+T1, or all
  three tiers.
- The Android tooling creates Makefile-based NativeActivity projects for ARM64
  devices and x86-64 emulators, including minimal and EGL/OpenGL ES templates.

### Debugging and observability

- Interactive errors enter a debugger with restarts, backtrace display, frame
  selection, frame evaluation where metadata permits, and stepping commands.
  Batch errors exit without waiting for terminal input.
- Lisp backtraces cover tree-walked, T0, native T1/T2/OSR, fiber, and outbound
  foreign-call boundaries. Historical error snapshots retain frames before
  unwinding; original arguments are reported when they have a proven stable
  location.
- Installed native code carries in-memory ELF/DWARF metadata for generated
  function names, machine-code ranges, and recoverable fixed-arity argument
  homes. Supported Linux builds register generated code through GDB's JIT
  interface.
- The runtime exposes disassembly, tier observations, event profiling,
  allocation profiling, GC statistics, tracing, and breakpoint/watchpoint
  primitives.

### Platforms and packages

- Runtime targets include Linux x86-64, AArch64, ppc64le, and s390x; Windows
  x86-64; and Android ARM64, with Android x86-64 application libraries. Native
  compiler, FFI, and validation coverage varies by target.
- The Fedora 44 x86-64 package set contains the native glibc runtime, a static
  musl runtime, glibc and static image-dumping tools for AArch64, ppc64le, and
  s390x, a Windows image-dumping tool, and Android CLI/application tooling.
- The same source RPM can build native glibc and static packages on ppc64le.
  Those POWER-native RPMs are not published as 0.0.1 assets because the release
  service does not yet provide the native OpenJDK required by that builder.
  Cross runtimes are executed under QEMU or Wine during package validation;
  packaged payloads are extracted and checked again for architecture, ASDF,
  dump/restart, and GC-stress behavior.
- Saved applications and target tools are documented in the
  [manual](docs/manual/index.md). The current support matrix is in
  [Platform support](docs/manual/user/reference/platforms.md).

### Known boundaries

- EGCL remains under active development. ANSI coverage, diagnostics,
  performance, and behavior across tiers and targets are not yet uniform.
- Source lines, inline scopes, complete local-variable recovery, DWARF unwind
  rules, full native C-stack unwinding, and capture of a concurrently running
  fiber are not complete. Optimized, overwritten, variadic, and register-only
  arguments may be unavailable in a backtrace.
- Foreign aggregate arguments are not portable across every target, and some
  non-x86 native backends support fewer optimized operations than x86-64.
- Heap images are architecture-specific. Dynamic target executables require a
  compatible system ABI, while static runtimes cannot provide every dynamic
  integration feature.
- The project specification is staged. Version 0.0.1 establishes the S5
  HotSpot-engine baseline: promoted code, OSR, and deoptimization must preserve
  results. Later specification stages remain future work.
