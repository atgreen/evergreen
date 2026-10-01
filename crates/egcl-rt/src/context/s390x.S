// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

    .text
    .p2align 3
    .globl egcl_context_swap
    .type egcl_context_swap,@function
egcl_context_swap:
    # No nested calls: save r6-r14 and f8-f15 in our private 144-byte frame.
    aghi %r15,-144
    stmg %r6,%r14,0(%r15)
    std %f8,72(%r15)
    std %f9,80(%r15)
    std %f10,88(%r15)
    std %f11,96(%r15)
    std %f12,104(%r15)
    std %f13,112(%r15)
    std %f14,120(%r15)
    std %f15,128(%r15)
    stfpc 136(%r15)
    stg %r15,0(%r2)
    lgr %r15,%r3
    lfpc 136(%r15)
    ld %f8,72(%r15)
    ld %f9,80(%r15)
    ld %f10,88(%r15)
    ld %f11,96(%r15)
    ld %f12,104(%r15)
    ld %f13,112(%r15)
    ld %f14,120(%r15)
    ld %f15,128(%r15)
    lmg %r6,%r14,0(%r15)
    aghi %r15,144
    br %r14
    .size egcl_context_swap,.-egcl_context_swap

    .globl egcl_context_init
    .type egcl_context_init,@function
egcl_context_init:
    stfpc 136(%r2)
    br %r14
    .size egcl_context_init,.-egcl_context_init
    .section .note.GNU-stack,"",@progbits
