
    .text
    .p2align 2
    .globl enter_ppc
    .type enter_ppc,@function
enter_ppc:
    # Save LR/TOC in the caller's ELFv2 linkage area BEFORE claiming our frame.
    # A Rust callee uses the new r1 and is allowed to overwrite its own 16/24
    # slots; keeping these words in the old frame is what makes nested calls safe.
    mflr 0
    std 0,16(1)
    std 2,24(1)
    # Allocate an ELFv2 frame. The back chain at 0(r1) points to the caller.
    stdu 1,-512(1)

    # Save r14-r31 (ELFv2 nonvolatile GPRs).
    std 14,64(1)
    std 15,72(1)
    std 16,80(1)
    std 17,88(1)
    std 18,96(1)
    std 19,104(1)
    std 20,112(1)
    std 21,120(1)
    std 22,128(1)
    std 23,136(1)
    std 24,144(1)
    std 25,152(1)
    std 26,160(1)
    std 27,168(1)
    std 28,176(1)
    std 29,184(1)
    std 30,192(1)
    std 31,200(1)

    # Save f14-f31.  The JIT currently uses scalar FP values, so preserving the
    # ELFv2 nonvolatile set is sufficient; VSX upper halves are not live there.
    stfd 14,224(1)
    stfd 15,232(1)
    stfd 16,240(1)
    stfd 17,248(1)
    stfd 18,256(1)
    stfd 19,264(1)
    stfd 20,272(1)
    stfd 21,280(1)
    stfd 22,288(1)
    stfd 23,296(1)
    stfd 24,304(1)
    stfd 25,312(1)
    stfd 26,320(1)
    stfd 27,328(1)
    stfd 28,336(1)
    stfd 29,344(1)
    stfd 30,352(1)
    stfd 31,360(1)

    # Save the five incoming pointers and the caller-visible machine state.
    std 3,384(1)
    std 4,392(1)
    std 5,400(1)
    std 6,408(1)
    std 7,416(1)
    mffs 0
    stfd 0,448(1)
    mfcr 0
    std 0,456(1)
    mfxer 0
    std 0,464(1)

    # Publish the frame and the common landing address in the pinned anchor.
    std 1,0(6)
    bl .Lppc_getpc
.Lppc_getpc:
    mflr 12
    addi 12,12,.Lppc_landing-.Lppc_getpc
    std 12,8(6)

    # Generated entries use the normal ELFv2 three-argument convention:
    # (slots, TorclStack, anchor).
    ld 3,392(1)
    ld 4,400(1)
    ld 5,408(1)
    ld 12,384(1)
    mtctr 12
    bctrl

    # A normal generated return carries its value in r3.
    std 3,432(1)
    li 12,0
    std 12,440(1)
    b .Lppc_landing

.Lppc_landing:
    # Both normal returns and leave_native_segment arrive here with r1 pointing
    # at this frame.  Publish the explicit out-parameter before restoring state.
    ld 12,416(1)
    ld 3,432(1)
    std 3,0(12)
    ld 3,440(1)
    std 3,8(12)
    ld 12,408(1)
    li 0,0
    std 0,0(12)
    std 0,8(12)

    # Restore condition state, FP state, all nonvolatile FP/GPR registers, and
    # the caller's linkage area.  The back chain at 0(r1) is the old r1.
    ld 0,464(1)
    mtxer 0
    ld 0,456(1)
    mtcrf 255,0
    lfd 0,448(1)
    mtfsf 255,0
    lfd 14,224(1)
    lfd 15,232(1)
    lfd 16,240(1)
    lfd 17,248(1)
    lfd 18,256(1)
    lfd 19,264(1)
    lfd 20,272(1)
    lfd 21,280(1)
    lfd 22,288(1)
    lfd 23,296(1)
    lfd 24,304(1)
    lfd 25,312(1)
    lfd 26,320(1)
    lfd 27,328(1)
    lfd 28,336(1)
    lfd 29,344(1)
    lfd 30,352(1)
    lfd 31,360(1)
    ld 14,64(1)
    ld 15,72(1)
    ld 16,80(1)
    ld 17,88(1)
    ld 18,96(1)
    ld 19,104(1)
    ld 20,112(1)
    ld 21,120(1)
    ld 22,128(1)
    ld 23,136(1)
    ld 24,144(1)
    ld 25,152(1)
    ld 26,160(1)
    ld 27,168(1)
    ld 28,176(1)
    ld 29,184(1)
    ld 30,192(1)
    ld 31,200(1)
    ld 12,0(1)
    ld 0,16(12)
    mtlr 0
    ld 2,24(12)
    mr 1,12
    blr
    .size enter_ppc,.-enter_ppc

    .globl leave_ppc
    .type leave_ppc,@function
leave_ppc:
    # r3=anchor, r4=value, r5=NativeExit.  The entry frame is still live even
    # though the generated callee may have moved r1 for its own frame.
    ld 12,0(3)
    std 4,432(12)
    std 5,440(12)
    mr 1,12
    b .Lppc_landing
    .size leave_ppc,.-leave_ppc

    # Test fixtures: the generated-entry ABI is (slots, stack, anchor).
    .globl ppc_probe_normal
    .type ppc_probe_normal,@function
ppc_probe_normal:
    ld 3,0(3)
    li 14,11
    li 15,12
    li 16,13
    li 17,14
    li 18,15
    li 19,16
    li 20,17
    li 21,18
    li 22,19
    li 23,20
    li 24,21
    li 25,22
    li 26,23
    li 27,24
    li 28,25
    li 29,26
    li 30,27
    li 31,28
    blr
    .size ppc_probe_normal,.-ppc_probe_normal

    .globl ppc_probe_transfer
    .type ppc_probe_transfer,@function
ppc_probe_transfer:
    mr 3,5
    li 4,42
    li 5,1
    b leave_ppc
    .size ppc_probe_transfer,.-ppc_probe_transfer
