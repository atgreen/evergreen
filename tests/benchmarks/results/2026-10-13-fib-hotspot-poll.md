# Fibonacci after execution-local polling

This is freshly generated installed T2 output from release commit `674838c7`,
compared with `54d1f6c1`. See the [annotated assembly](2026-10-13-fib-hotspot-poll-annotated.asm),
[before assembly](2026-10-13-fib-hotspot-poll-before.asm), and
[executable hashes and samples](2026-10-13-fib-hotspot-poll.json).

| Measurement | Before | After |
|---|---:|---:|
| Median warmed `fib(35)` | 190.5 ms | 107 ms |
| Installed code | 2685 bytes | 2311 bytes |
| Physical frame | 136 bytes | 136 bytes |
| Admission instructions, each recursive call | 41 | 12 |
| Admission conditional branches | 14 | 4 |
| Admission bytes | 234 | 65 |

The eleven administrative checks become one local notification check. Three
physical-stack checks remain: two arithmetic-underflow branches and a stack-limit
comparison. The existing proof of empty multiple-value state already omitted an
MV admission guard in this function. Type, arithmetic-overflow, and public arity
checks remain; base cases still return without constructing a frame.

| Old administrative check | New handling for these trusted self-calls |
|---|---|
| 1. Native control exists | Established at native ingress; omit repeated check. |
| 2. Control belongs to this execution | Established at boundaries; omit repeated check. Named calls retain conservative validation. |
| 3. GC requested | Producer arms local notification; published runtime boundary services GC. |
| 4. SIGUSR1 pending | Signal-safe broadcast arms execution slots. |
| 5. Posted signal pending | Delivery arms the target execution and affected carrier association. |
| 6. Pending native error | Recording the error arms its execution; runtime return transfers before native continuation. |
| 7. Process signal epoch changed | Broadcast arms slots; the runtime still claims epoch-based signal state. |
| 8. Carrier changed | Mounting arms the execution; scheduler resume validates active native segments. |
| 9. Yield requested | Explicit requests arm the execution. There is no automatic time-slice timer. |
| 10. CPU deadline armed | Only explicit sandbox deadlines remain; they keep the local poll armed while active. |
| 11. Recursion depth below artificial cap | Removed for ordinary machine frames; exact physical-stack bounds remain. Legacy Rust/native reentry still has its separate cap. |

In the captured code, the first administrative poll is exactly:

```asm
+00e6: mov r11,[r13+28h]   ; owning execution's stable poll slot
+00ea: cmp qword [r11],0   ; any runtime work pending?
+00ef: jne near 00007FA60D725145h ; rooted cold helper
```

The physical-stack checks follow at `+00f5` through `+011c`. The actual relative
self-calls are at `+013b` and `+0294`, both targeting accepted entry `+003e`.
Notifications are cleared before draining authoritative runtime state, then
persistent work is rearmed. A notification racing the drain remains pending.
Helper return values are rooted while a return poll can collect or yield.

The timing comparison used CPU 0, two fresh processes per executable, seven
samples per process, and process order before/after/after/before. Both binaries
used the default musl release profile (thin LTO, one codegen unit, rustc 1.94.1).
Each process loaded `tests/benchmarks/fib_self.lisp`, warmed with 6000 calls to
`fib(8)`, waited for installed T2, and timed only `fib(35)`. The benchmark parser
verified the checksum `9227465`, two real relative self-calls, and unchanged
installed code before and after timing. The measured speedup is about 1.78x on
this host; timings use the runtime's millisecond clock.

Fiber cancellation is cooperative: explicit checks and public sleep/park/join
observe a sticky request. It does not forcibly stop CPU-bound Lisp. See the
[fiber API](../../../docs/manual/fibers.md) for boundaries and limitations.
