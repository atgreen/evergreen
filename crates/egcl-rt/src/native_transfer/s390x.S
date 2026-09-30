// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

    .text
    .p2align 3
    .globl enter_s390x
    .type enter_s390x,@function
enter_s390x:
    # Fixed 256-byte frame. Save the caller's return address and stack first.
    lgr     %r0,%r15
    aghi    %r15,-256
    stg     %r14,0(%r15)
    stg     %r0,8(%r15)
    stmg    %r6,%r13,16(%r15)
    std     %f8,80(%r15)
    std     %f9,88(%r15)
    std     %f10,96(%r15)
    std     %f11,104(%r15)
    std     %f12,112(%r15)
    std     %f13,120(%r15)
    std     %f14,128(%r15)
    std     %f15,136(%r15)
    stg     %r2,160(%r15)
    stg     %r3,168(%r15)
    stg     %r4,176(%r15)
    stg     %r5,184(%r15)
    stg     %r6,192(%r15)

    # Publish the frame and the common landing address in the anchor.
    stg     %r15,0(%r5)
    larl    %r1,.Llanding
    stg     %r1,8(%r5)

    # Generated entries use (slots, stack, anchor) in r2-r4.
    lg      %r2,168(%r15)
    lg      %r3,176(%r15)
    lg      %r4,184(%r15)
    lg      %r1,160(%r15)
    basr    %r14,%r1
    stg     %r2,200(%r15)
    lghi    %r1,0
    stg     %r1,208(%r15)
    j       .Llanding

.Llanding:
    lg      %r1,192(%r15)
    lg      %r2,200(%r15)
    stg     %r2,0(%r1)
    lg      %r2,208(%r15)
    stg     %r2,8(%r1)
    lg      %r1,184(%r15)
    lghi    %r0,0
    stg     %r0,0(%r1)
    stg     %r0,8(%r1)

    lmg     %r6,%r13,16(%r15)
    ld      %f8,80(%r15)
    ld      %f9,88(%r15)
    ld      %f10,96(%r15)
    ld      %f11,104(%r15)
    ld      %f12,112(%r15)
    ld      %f13,120(%r15)
    ld      %f14,128(%r15)
    ld      %f15,136(%r15)
    lg      %r0,8(%r15)
    lg      %r14,0(%r15)
    lgr     %r15,%r0
    br      %r14
    .size enter_s390x,.-enter_s390x

    .globl leave_s390x
    .type leave_s390x,@function
leave_s390x:
    # r2=anchor, r3=value, r4=NativeExit.
    lg      %r1,0(%r2)
    stg     %r3,200(%r1)
    stg     %r4,208(%r1)
    lg      %r3,8(%r2)
    lgr     %r15,%r1
    br      %r3
    .size leave_s390x,.-leave_s390x

    .globl s390x_probe_normal
    .type s390x_probe_normal,@function
s390x_probe_normal:
    lg      %r2,0(%r2)
    lghi    %r6,11
    lghi    %r7,12
    lghi    %r8,13
    lghi    %r9,14
    lghi    %r10,15
    lghi    %r11,16
    lghi    %r12,17
    lghi    %r13,18
    br      %r14
    .size s390x_probe_normal,.-s390x_probe_normal

    .globl s390x_probe_transfer
    .type s390x_probe_transfer,@function
s390x_probe_transfer:
    lgr     %r2,%r4
    lghi    %r3,42
    lghi    %r4,1
    j       leave_s390x
    .size s390x_probe_transfer,.-s390x_probe_transfer

    # A caller-side probe for the ELF64 nonvolatile GPR set. The Rust helper
    # enters s390x_probe_clobber through the segment boundary; this wrapper
    # verifies that r6-r13 survive the generated body.
    .globl s390x_probe_registers
    .type s390x_probe_registers,@function
    # The Rust callback is supplied by the integration test.  Keep this
    # assembly probe linkable in production libraries, where no test callback
    # exists; the test's strong definition overrides the weak reference.
    .weak s390x_probe_call
s390x_probe_registers:
    lgr     %r0,%r15
    aghi    %r15,-512
    stg     %r14,384(%r15)
    stg     %r0,392(%r15)
    stmg    %r6,%r13,400(%r15)
    lghi    %r6,101
    lghi    %r7,102
    lghi    %r8,103
    lghi    %r9,104
    lghi    %r10,105
    lghi    %r11,106
    lghi    %r12,107
    lghi    %r13,108
    # Through the PLT: the callback is a WEAK, UNDEFINED symbol in a production
    # library, and a bare brasl emits R_390_PC32DBL against it, which the linker
    # refuses in the PIE that Rust links by default ("cannot be used against
    # symbol s390x_probe_call; recompile with -fPIC"). @PLT is the PIC call form
    # and still lets the test's strong definition override the weak reference.
    brasl   %r14,s390x_probe_call@PLT
    lghi    %r0,1
    cghi    %r6,101
    jne     .Lregister_fail
    cghi    %r7,102
    jne     .Lregister_fail
    cghi    %r8,103
    jne     .Lregister_fail
    cghi    %r9,104
    jne     .Lregister_fail
    cghi    %r10,105
    jne     .Lregister_fail
    cghi    %r11,106
    jne     .Lregister_fail
    cghi    %r12,107
    jne     .Lregister_fail
    cghi    %r13,108
    jne     .Lregister_fail
    j       .Lregister_done
.Lregister_fail:
    lghi    %r0,0
.Lregister_done:
    lmg     %r6,%r13,400(%r15)
    lg      %r1,392(%r15)
    lg      %r14,384(%r15)
    lgr     %r15,%r1
    lgr     %r2,%r0
    br      %r14
    .size s390x_probe_registers,.-s390x_probe_registers

    .globl s390x_probe_clobber
    .type s390x_probe_clobber,@function
s390x_probe_clobber:
    lg      %r2,0(%r2)
    lghi    %r6,1
    lghi    %r7,2
    lghi    %r8,3
    lghi    %r9,4
    lghi    %r10,5
    lghi    %r11,6
    lghi    %r12,7
    lghi    %r13,8
    br      %r14
    .size s390x_probe_clobber,.-s390x_probe_clobber
