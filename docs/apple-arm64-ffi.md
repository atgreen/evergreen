# Apple ARM64 scalar foreign calls

The ARM64 foreign-call backend has separate generic AAPCS64 and Apple placement
policies. The supported types are signed and unsigned 8-, 16-, 32- and 64-bit
integers, pointers, single floats and doubles, with scalar or void returns.
Aggregates, vectors, HFA/HVA classification, 128-bit integers and Lisp callback
adapters are not added by this change.

Apple fixed arguments use the independent X0–X7 and S0/D0–S7/D7 register files.
The caller extends integer register arguments narrower than 32 bits according
to their signedness. Once a register file is exhausted, fixed arguments occupy
naturally aligned stack fields of their actual size. For example, following
eight integer register arguments, two bytes, a 16-bit integer, a 32-bit integer,
another byte and a 64-bit integer occupy byte offsets 0, 1, 2, 4, 8 and 16.
Two spilled single floats occupy offsets 0 and 4.

For a variadic call, named arguments retain these rules. Anonymous arguments
undergo C default promotions: single floats become doubles, and 8-/16-bit
integers become signed 32-bit C ints, preserving the original signed or
unsigned value. Promotion transforms the raw bits as well as the declared
argument type. Every anonymous argument then occupies an eight-byte-aligned
stack slot, even while argument registers remain available. A variadic
32-bit int occupies four meaningful bytes within that slot.

A fully initialized native-word buffer holds the outgoing stack data,
including padding in its final partial word. The existing static trampoline
copies only those initialized words and allocates a separately rounded,
16-byte-aligned outgoing area. It retains the existing X0, S0 and D0 return
paths and does not use Apple's reserved X18 register. Neither the placement
policy nor this trampoline allocates generated executable adapters, so the
same call path is usable by the iOS T0 profile.

Generic AAPCS64 keeps its previous independent register files, eight-byte
stack slots and native-word stores. Its variadic placement stays the same as
fixed placement. Correct default promotions are applied before either ABI's
placement policy.

## Validation status

The implementation and regression fixtures are source-only. No build,
typecheck, ABI execution, simulator or device result is claimed. Pure fixture
source covers both placement policies, mixed stack alignment, initialized
padding, caller extension, promotions and argument errors. Native fixture
source adds narrow fixed spills, single-float spills, a real `snprintf`
variadic call and Apple-specific register inspection, while retaining the
existing integer/double/pointer/return-class and guarded-source tests.

Actual Apple ARM64 execution and neighboring Linux/Android validation remain
necessary before claiming those results. This scalar implementation does not
complete the iOS application scaffold, process policy or device validation
tracked by GitHub182.

The policies follow Apple's
[Writing ARM64 code for Apple platforms](https://developer.apple.com/documentation/xcode/writing-arm64-code-for-apple-platforms)
and Arm's
[AAPCS64 parameter-passing specification](https://github.com/ARM-software/abi-aa/blob/main/aapcs64/aapcs64.rst).
