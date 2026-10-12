; Annotated exact first AFTER listing from comparison-35a2185f/after-0.stdout.
; Binary revision: 35a2185ff398396afe3b630a780a4644cd5a1a62.
; Code base: decimal139888502386688 =0x7f3a547e3000; every +offset is relative to it.
; Original listing lines below are unchanged; only '; ANNOTATION:' lines are inserted.
; HOT/COLD describes source control flow, not dynamic instruction profiling.
; On a successful non-base fixnum path: two direct self CALL sites and two MV-clear CALL sites.
FIB-CODE-BASE 139888502386688
; FIB — 1 arg(s), 1 local(s), 3 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; 2806 bytes of x86-64:
; Ordinary x86-64 Lisp ABI — installed native code.
; 2806 bytes of x86-64:
; ANNOTATION: UNUSED ENTRY: UD2 prevents calling the old entry as an ordinary entry.
  +0000:  ud2
; ANNOTATION: Compatibility prologue in the mapping; neither recursive CALL below targets it.
  +0002:  push rbp
  +0003:  mov rbp,rsp
  +0006:  push rbx
  +0007:  sub rsp,8
  +000b:  sub rsp,90h
  +0012:  test byte [rsp],0
  +0016:  mov [rsp+70h],rdi
  +001b:  mov rsi,[rdi]
  +001f:  jmp 00007F3A547E3078h
; ANNOTATION: PUBLIC ORDINARY ENTRY: RCX must equal 1. Recursive edges target +003e after this check.
  +0024:  cmp rcx,1
  +002b:  je near 00007F3A547E303Eh
; ANNOTATION: COLD wrong-arity veneer, reached only if the public argument-count check fails.
  +0031:  mov r11,7F3A547F6000h
  +003b:  jmp r11
; ANNOTATION: DIRECT SELF TARGET: 0x7f3a547e303e = code base +0x3e. RBP chain begins here.
  +003e:  push rbp
  +003f:  mov rbp,rsp
; ANNOTATION: Save RBX; pad 8 bytes; reserve 0x90 (144) spill bytes. Total prologue stack use =168 bytes.
  +0042:  push rbx
  +0043:  sub rsp,8
  +0047:  sub rsp,90h
; ANNOTATION: Touch newly reserved stack storage; stack bounds were checked before admission.
  +004e:  test byte [rsp],0
; ANNOTATION: Tagged incoming n moves from RDI to RSI. R13 remains the execution context.
  +0052:  mov rsi,rdi
; ANNOTATION: Initialize ordinary frame state; no managed activation is allocated here.
  +0055:  mov rax,0
  +005f:  mov [rsp+70h],rax
; ANNOTATION: Read execution ACTIVE_NATIVE_CONTROL; save it at [rsp+0x78].
  +0064:  mov rax,[r13+20h]
  +0068:  mov [rsp+78h],rax
; ANNOTATION: Save original argument in mapped native argument home [rsp+0x80].
  +006d:  mov rax,rsi
  +0070:  mov [rsp+80h],rax
; ANNOTATION: BODY TYPE GUARD: low three bits must be zero for FIXNUM; failure -> cold +07f8.
  +0078:  test sil,7
  +007c:  jne near 00007F3A547E37F8h
  +0082:  mov r9,rsi
  +0085:  mov [rsp+48h],r9
  +008a:  mov r9,[rsp+48h]
; ANNOTATION: n < 2, with tagged constant 0x10. Base case goes directly to +0570.
  +008f:  cmp r9,10h
  +0096:  jge near 00007F3A547E30A6h
  +009c:  mov rsi,[rsp+48h]
  +00a1:  jmp 00007F3A547E3570h
  +00a6:  mov r9,[rsp+48h]
  +00ab:  mov r10,r9
; ANNOTATION: Compute tagged n-1 (subtract8); signed overflow -> precise recovery at +072f.
  +00ae:  sub r10,8
  +00b5:  jo near 00007F3A547E372Fh
  +00bb:  mov [rsp+20h],r10
; ANNOTATION: FIRST SELF EDGE: copy n-1 into writable outgoing argument home [rsp+0x68].
  +00c0:  mov rax,[rsp+20h]
  +00c5:  mov [rsp+68h],rax
; ANNOTATION: HOT admission: load saved control; NULL or execution !=R13 -> cold +0234.
  +00ca:  mov rdi,[rsp+78h]
  +00cf:  cmp rdi,0
  +00d6:  je near 00007F3A547E3234h
  +00dc:  mov rax,[rdi]
  +00df:  cmp rax,r13
  +00e2:  jne near 00007F3A547E3234h
; ANNOTATION: HOT: GC request byte. Nonzero -> +0234.
  +00e8:  mov r11,7F3A760500E8h
  +00f2:  movzx eax,byte [r11]
  +00f7:  cmp rax,0
  +00fe:  jne near 00007F3A547E3234h
; ANNOTATION: HOT: SIGUSR1 pending byte. Nonzero -> +0234.
  +0104:  mov r11,7F3A76054298h
  +010e:  movzx eax,byte [r11]
  +0113:  cmp rax,0
  +011a:  jne near 00007F3A547E3234h
; ANNOTATION: HOT: posted-signal state. Nonzero -> +0234.
  +0120:  mov r11,7F3A76052D40h
  +012a:  mov rax,[r11]
  +012d:  cmp rax,0
  +0134:  jne near 00007F3A547E3234h
; ANNOTATION: HOT: native-error state. Nonzero -> +0234.
  +013a:  mov r11,7F3A76052BF0h
  +0144:  mov rax,[r11]
  +0147:  cmp rax,0
  +014e:  jne near 00007F3A547E3234h
; ANNOTATION: HOT: current signal epoch must equal cached control.seen_signal_epoch.
  +0154:  mov r11,7F3A76053970h
  +015e:  mov rax,[r11]
  +0161:  cmp rax,[rdi+30h]
  +0165:  jne near 00007F3A547E3234h
; ANNOTATION: HOT: execution carrier must equal control.seen_carrier (migration awareness).
  +016b:  mov rax,[r13+10h]
  +016f:  cmp rax,[rdi+28h]
  +0173:  jne near 00007F3A547E3234h
; ANNOTATION: HOT: execution yield request must be clear.
  +0179:  mov rax,[r13+18h]
  +017d:  cmp rax,0
  +0184:  jne near 00007F3A547E3234h
; ANNOTATION: HOT: control.deadline_armed at +0x40 must be clear; no clock read here.
  +018a:  mov r11,rdi
  +018d:  add r11,40h
  +0194:  movzx eax,byte [r11]
  +0199:  cmp rax,0
  +01a0:  jne near 00007F3A547E3234h
; ANNOTATION: HOT: indirect Env multiple-value-active byte must be clear for ordinary entry.
  +01a6:  mov r11,[rdi+18h]
  +01aa:  movzx eax,byte [r11]
  +01af:  cmp rax,0
  +01b6:  jne near 00007F3A547E3234h
; ANNOTATION: HOT: current depth must be below control.depth_limit (+0x38).
  +01bc:  mov r11,[rdi+8]
  +01c0:  mov eax,[r11]
  +01c3:  mov edx,[rdi+38h]
  +01c6:  cmp rax,rdx
  +01c9:  jge near 00007F3A547E3234h
; ANNOTATION: HOT stack admission: final callee bound =0x110 (272) bytes; reserve return word too.
  +01cf:  mov rdx,110h
  +01d9:  mov rax,rsp
  +01dc:  sub rax,8
; ANNOTATION: Borrow/underflow -> cold; then reject candidate bottom <= control.stack_limit (+0x58).
  +01e3:  jb near 00007F3A547E3234h
  +01e9:  sub rax,rdx
  +01ec:  jb near 00007F3A547E3234h
  +01f2:  mov rdx,[rdi+58h]
  +01f6:  cmp rax,rdx
  +01f9:  jbe near 00007F3A547E3234h
; ANNOTATION: HOT admitted edge: increment native depth. No descriptor/revision/heat lookup for this self call.
  +01ff:  mov rdx,[rdi+8]
  +0203:  inc dword [rdx]
; ANNOTATION: ORDINARY ARGUMENTS: RDI=n-1, RCX=1, RAX=7 (tagged NIL closure), R13 unchanged.
  +0205:  mov rdi,[rsp+68h]
  +020a:  mov rcx,1
  +0214:  mov rax,7
; ANNOTATION: HOT direct relative self CALL #1 -> +003e, skipping the public arity check.
  +021e:  call 00007F3A547E303Eh
; ANNOTATION: Return: decrement depth without changing carry/result registers; skip cold block -> +02c8.
  +0223:  mov r11,[rsp+78h]
  +0228:  mov r11,[r11+8]
  +022c:  dec dword [r11]
  +022f:  jmp 00007F3A547E32C8h
; ANNOTATION: COLD SELF #1: reached by failed admission checks, not by fallthrough from successful CALL.
  +0234:  sub rsp,40h
  +023b:  mov rax,8E7h
  +0245:  mov [rsp],rax
  +0249:  mov rax,1
  +0253:  mov [rsp+8],rax
  +0258:  mov rax,7
  +0262:  mov [rsp+18h],rax
  +0267:  mov rax,7F3A7520EE38h
  +0271:  mov [rsp+28h],rax
  +0276:  mov rax,7F3A7520EE78h
  +0280:  mov [rsp+30h],rax
; ANNOTATION: Cold request +0x38 =1 means exact retained self callable, not mutable global name lookup.
  +0285:  mov rax,1
  +028f:  mov [rsp+38h],rax
  +0294:  mov rax,rsp
  +0297:  add rax,0A8h
  +029e:  mov [rsp+10h],rax
  +02a3:  mov rax,[rsp+0B8h]
  +02ab:  mov [rsp+20h],rax
  +02b0:  mov rdi,rsp
; ANNOTATION: Cold published veneer address. Caller PC/maps identify and root original callable before polling.
  +02b3:  mov r11,7F3A547E5000h
; ANNOTATION: COLD helper CALL: can poll/collect/dispatch retained callable; joins normal result at +02c8.
  +02bd:  call r11
  +02c0:  lea rsp,[rsp+40h]
; ANNOTATION: Join direct/cold result of fib(n-1); preserve it in native home.
  +02c8:  mov [rsp+28h],rax
  +02cd:  mov r9,[rsp+48h]
  +02d2:  mov r10,r9
; ANNOTATION: Compute tagged n-2; overflow -> cold precise recovery +0657.
  +02d5:  sub r10,10h
  +02dc:  jo near 00007F3A547E3657h
  +02e2:  mov [rsp+38h],r10
  +02e7:  mov r9,[rsp+28h]
; ANNOTATION: Guard first recursive result as FIXNUM; failure -> precise recovery +057f.
  +02ec:  test r9b,7
  +02f0:  jne near 00007F3A547E357Fh
  +02f6:  mov r10,r9
  +02f9:  mov [rsp+50h],r10
; ANNOTATION: HOT MV CLEAR #1: zero-count c2i_t2_mv helper; normal successful path reaches this sequence.
  +02fe:  mov edi,0
  +0303:  mov esi,0
  +0308:  mov edx,0
  +030d:  mov rax,7F3A75AA2410h
; ANNOTATION: HOT nonallocating helper CALL, not an admission fallback or managed Lisp-call adapter.
  +0317:  call rax
; ANNOTATION: SECOND SELF EDGE: home n-2. The same 15 conditional admission branches follow.
  +0319:  mov rax,[rsp+38h]
  +031e:  mov [rsp+68h],rax
; ANNOTATION: HOT control ownership checks; any failure in this edge branches to +048d.
  +0323:  mov rdi,[rsp+78h]
  +0328:  cmp rdi,0
  +032f:  je near 00007F3A547E348Dh
  +0335:  mov rax,[rdi]
  +0338:  cmp rax,r13
  +033b:  jne near 00007F3A547E348Dh
; ANNOTATION: HOT GC request check.
  +0341:  mov r11,7F3A760500E8h
  +034b:  movzx eax,byte [r11]
  +0350:  cmp rax,0
  +0357:  jne near 00007F3A547E348Dh
; ANNOTATION: HOT SIGUSR1 pending check.
  +035d:  mov r11,7F3A76054298h
  +0367:  movzx eax,byte [r11]
  +036c:  cmp rax,0
  +0373:  jne near 00007F3A547E348Dh
; ANNOTATION: HOT posted-signal check.
  +0379:  mov r11,7F3A76052D40h
  +0383:  mov rax,[r11]
  +0386:  cmp rax,0
  +038d:  jne near 00007F3A547E348Dh
; ANNOTATION: HOT native-error check.
  +0393:  mov r11,7F3A76052BF0h
  +039d:  mov rax,[r11]
  +03a0:  cmp rax,0
  +03a7:  jne near 00007F3A547E348Dh
; ANNOTATION: HOT signal-epoch check.
  +03ad:  mov r11,7F3A76053970h
  +03b7:  mov rax,[r11]
  +03ba:  cmp rax,[rdi+30h]
  +03be:  jne near 00007F3A547E348Dh
; ANNOTATION: HOT carrier-identity check.
  +03c4:  mov rax,[r13+10h]
  +03c8:  cmp rax,[rdi+28h]
  +03cc:  jne near 00007F3A547E348Dh
; ANNOTATION: HOT yield-request check.
  +03d2:  mov rax,[r13+18h]
  +03d6:  cmp rax,0
  +03dd:  jne near 00007F3A547E348Dh
; ANNOTATION: HOT armed-deadline check.
  +03e3:  mov r11,rdi
  +03e6:  add r11,40h
  +03ed:  movzx eax,byte [r11]
  +03f2:  cmp rax,0
  +03f9:  jne near 00007F3A547E348Dh
; ANNOTATION: HOT active-MV check.
  +03ff:  mov r11,[rdi+18h]
  +0403:  movzx eax,byte [r11]
  +0408:  cmp rax,0
  +040f:  jne near 00007F3A547E348Dh
; ANNOTATION: HOT depth-limit check.
  +0415:  mov r11,[rdi+8]
  +0419:  mov eax,[r11]
  +041c:  mov edx,[rdi+38h]
  +041f:  cmp rax,rdx
  +0422:  jge near 00007F3A547E348Dh
; ANNOTATION: HOT physical-stack check, same 272-byte bound plus8-byte return word.
  +0428:  mov rdx,110h
  +0432:  mov rax,rsp
  +0435:  sub rax,8
  +043c:  jb near 00007F3A547E348Dh
  +0442:  sub rax,rdx
  +0445:  jb near 00007F3A547E348Dh
  +044b:  mov rdx,[rdi+58h]
  +044f:  cmp rax,rdx
  +0452:  jbe near 00007F3A547E348Dh
; ANNOTATION: Increment depth for admitted second child.
  +0458:  mov rdx,[rdi+8]
  +045c:  inc dword [rdx]
; ANNOTATION: ORDINARY ARGUMENTS: RDI=n-2, RCX=1, RAX=NIL closure.
  +045e:  mov rdi,[rsp+68h]
  +0463:  mov rcx,1
  +046d:  mov rax,7
; ANNOTATION: HOT direct relative self CALL #2 -> same +003e prologue.
  +0477:  call 00007F3A547E303Eh
; ANNOTATION: Balance depth; jump over second cold block -> +0521.
  +047c:  mov r11,[rsp+78h]
  +0481:  mov r11,[r11+8]
  +0485:  dec dword [r11]
  +0488:  jmp 00007F3A547E3521h
; ANNOTATION: COLD SELF #2: 64-byte published request, same retained-self semantics as +0234.
  +048d:  sub rsp,40h
  +0494:  mov rax,8E7h
  +049e:  mov [rsp],rax
  +04a2:  mov rax,1
  +04ac:  mov [rsp+8],rax
  +04b1:  mov rax,7
  +04bb:  mov [rsp+18h],rax
  +04c0:  mov rax,7F3A7520EE38h
  +04ca:  mov [rsp+28h],rax
  +04cf:  mov rax,7F3A7520EE78h
  +04d9:  mov [rsp+30h],rax
; ANNOTATION: Cold request identity flag =1.
  +04de:  mov rax,1
  +04e8:  mov [rsp+38h],rax
  +04ed:  mov rax,rsp
  +04f0:  add rax,0A8h
  +04f7:  mov [rsp+10h],rax
  +04fc:  mov rax,[rsp+0B8h]
  +0504:  mov [rsp+20h],rax
  +0509:  mov rdi,rsp
  +050c:  mov r11,7F3A547E5000h
; ANNOTATION: COLD published helper CALL; normal admitted edge skips this instruction.
  +0516:  call r11
  +0519:  lea rsp,[rsp+40h]
; ANNOTATION: Join second recursive result; type guard failure -> precise recovery +0969.
  +0521:  mov rbx,rax
  +0524:  test bl,7
  +0527:  jne near 00007F3A547E3969h
  +052d:  mov r9,rbx
  +0530:  mov [rsp+60h],r9
; ANNOTATION: Add both tagged recursive results inline; overflow -> precise recovery +0a2d.
  +0535:  mov r9,[rsp+50h]
  +053a:  mov r10,[rsp+60h]
  +053f:  mov r11,r9
  +0542:  add r11,r10
  +0545:  jo near 00007F3A547E3A2Dh
  +054b:  mov [rsp+40h],r11
; ANNOTATION: HOT MV CLEAR #2: normalize runtime MV state before returning one value.
  +0550:  mov edi,0
  +0555:  mov esi,0
  +055a:  mov edx,0
  +055f:  mov rax,7F3A75AA2410h
; ANNOTATION: HOT nonallocating c2i_t2_mv CALL (same address as +0317).
  +0569:  call rax
  +056b:  mov rsi,[rsp+40h]
; ANNOTATION: NORMAL RETURN JOIN: base case or completed sum in RSI -> primary RAX.
  +0570:  mov rax,rsi
; ANNOTATION: Retire 152 bytes of spill/padding, restore RBX and RBP; CLC declares exactly one value.
  +0573:  lea rsp,[rsp+98h]
  +057b:  pop rbx
  +057c:  pop rbp
  +057d:  clc
  +057e:  ret
; ANNOTATION: COLD precise recovery block: first recursive result was not a FIXNUM (branch at +02f0).
  +057f:  sub rsp,60h
  +0586:  test byte [rsp],0
  +058a:  mov rax,8E7h
  +0594:  mov [rsp],rax
  +0598:  mov rax,0Ch
  +05a2:  mov [rsp+8],rax
  +05a7:  mov rax,1
  +05b1:  mov [rsp+10h],rax
  +05b6:  mov rax,3
  +05c0:  mov [rsp+18h],rax
  +05c5:  mov rax,17h
  +05cf:  mov [rsp+20h],rax
  +05d4:  mov rax,[rsp+88h]
  +05dc:  mov [rsp+28h],rax
  +05e1:  mov rax,[rsp+0A8h]
  +05e9:  mov [rsp+30h],rax
  +05ee:  mov rax,10h
  +05f8:  mov [rsp+38h],rax
  +05fd:  mov rax,1
  +0607:  mov [rsp+40h],rax
  +060c:  mov rax,8
  +0616:  mov [rsp+48h],rax
  +061b:  mov rax,rsp
  +061e:  mov [rsp+50h],rax
  +0623:  mov rax,[rsp+0D8h]
  +062b:  mov [rsp+58h],rax
  +0630:  lea rdi,[rsp+40h]
; ANNOTATION: Published precise deoptimization veneer; serialize continuation/root homes before this CALL.
  +0638:  mov rax,7F3A547E0000h    ; native precise deopt veneer
  +0642:  call rax
  +0644:  lea rsp,[rsp+60h]
  +064c:  lea rsp,[rsp+98h]
  +0654:  pop rbx
  +0655:  pop rbp
  +0656:  ret
; ANNOTATION: COLD precise recovery block: n-2 overflow (branch at +02dc).
  +0657:  sub rsp,60h
  +065e:  test byte [rsp],0
  +0662:  mov rax,8E7h
  +066c:  mov [rsp],rax
  +0670:  mov rax,0Ch
  +067a:  mov [rsp+8],rax
  +067f:  mov rax,1
  +0689:  mov [rsp+10h],rax
  +068e:  mov rax,3
  +0698:  mov [rsp+18h],rax
  +069d:  mov rax,17h
  +06a7:  mov [rsp+20h],rax
  +06ac:  mov rax,[rsp+88h]
  +06b4:  mov [rsp+28h],rax
  +06b9:  mov rax,[rsp+0A8h]
  +06c1:  mov [rsp+30h],rax
  +06c6:  mov rax,10h
  +06d0:  mov [rsp+38h],rax
  +06d5:  mov rax,1
  +06df:  mov [rsp+40h],rax
  +06e4:  mov rax,8
  +06ee:  mov [rsp+48h],rax
  +06f3:  mov rax,rsp
  +06f6:  mov [rsp+50h],rax
  +06fb:  mov rax,[rsp+0D8h]
  +0703:  mov [rsp+58h],rax
  +0708:  lea rdi,[rsp+40h]
  +0710:  mov rax,7F3A547E0000h    ; native precise deopt veneer
  +071a:  call rax
  +071c:  lea rsp,[rsp+60h]
  +0724:  lea rsp,[rsp+98h]
  +072c:  pop rbx
  +072d:  pop rbp
  +072e:  ret
; ANNOTATION: COLD precise recovery block: n-1 overflow (branch at +00b5).
  +072f:  sub rsp,60h
  +0736:  test byte [rsp],0
  +073a:  mov rax,8E7h
  +0744:  mov [rsp],rax
  +0748:  mov rax,8
  +0752:  mov [rsp+8],rax
  +0757:  mov rax,1
  +0761:  mov [rsp+10h],rax
  +0766:  mov rax,2
  +0770:  mov [rsp+18h],rax
  +0775:  mov rax,[rsp+0A8h]
  +077d:  mov [rsp+20h],rax
  +0782:  mov rax,[rsp+0A8h]
  +078a:  mov [rsp+28h],rax
  +078f:  mov rax,8
  +0799:  mov [rsp+30h],rax
  +079e:  mov rax,1
  +07a8:  mov [rsp+38h],rax
  +07ad:  mov rax,7
  +07b7:  mov [rsp+40h],rax
  +07bc:  mov rax,rsp
  +07bf:  mov [rsp+48h],rax
  +07c4:  mov rax,[rsp+0D8h]
  +07cc:  mov [rsp+50h],rax
  +07d1:  lea rdi,[rsp+38h]
  +07d9:  mov rax,7F3A547E0000h    ; native precise deopt veneer
  +07e3:  call rax
  +07e5:  lea rsp,[rsp+60h]
  +07ed:  lea rsp,[rsp+98h]
  +07f5:  pop rbx
  +07f6:  pop rbp
  +07f7:  ret
; ANNOTATION: COLD precise recovery block: incoming n is not a FIXNUM (branch at +007c).
  +07f8:  sub rsp,50h
  +07ff:  test byte [rsp],0
  +0803:  mov rax,8E7h
  +080d:  mov [rsp],rax
  +0811:  mov rax,0
  +081b:  mov [rsp+8],rax
  +0820:  mov rax,1
  +082a:  mov [rsp+10h],rax
  +082f:  mov rax,0
  +0839:  mov [rsp+18h],rax
  +083e:  mov rax,rsi
  +0841:  mov [rsp+20h],rax
  +0846:  mov rax,1
  +0850:  mov [rsp+28h],rax
  +0855:  mov rax,5
  +085f:  mov [rsp+30h],rax
  +0864:  mov rax,rsp
  +0867:  mov [rsp+38h],rax
  +086c:  mov rax,[rsp+0C8h]
  +0874:  mov [rsp+40h],rax
  +0879:  lea rdi,[rsp+28h]
  +0881:  mov rax,7F3A547E0000h    ; native precise deopt veneer
  +088b:  call rax
  +088d:  lea rsp,[rsp+50h]
  +0895:  lea rsp,[rsp+98h]
  +089d:  pop rbx
  +089e:  pop rbp
  +089f:  ret
; ANNOTATION: Additional emitted precise recovery record (bytecode position2); no direct branch here in this listing.
  +08a0:  sub rsp,60h
  +08a7:  test byte [rsp],0
  +08ab:  mov rax,8E7h
  +08b5:  mov [rsp],rax
  +08b9:  mov rax,2
  +08c3:  mov [rsp+8],rax
  +08c8:  mov rax,1
  +08d2:  mov [rsp+10h],rax
  +08d7:  mov rax,2
  +08e1:  mov [rsp+18h],rax
  +08e6:  mov rax,[rsp+0A8h]
  +08ee:  mov [rsp+20h],rax
  +08f3:  mov rax,[rsp+0A8h]
  +08fb:  mov [rsp+28h],rax
  +0900:  mov rax,10h
  +090a:  mov [rsp+30h],rax
  +090f:  mov rax,1
  +0919:  mov [rsp+38h],rax
  +091e:  mov rax,7
  +0928:  mov [rsp+40h],rax
  +092d:  mov rax,rsp
  +0930:  mov [rsp+48h],rax
  +0935:  mov rax,[rsp+0D8h]
  +093d:  mov [rsp+50h],rax
  +0942:  lea rdi,[rsp+38h]
  +094a:  mov rax,7F3A547E0000h    ; native precise deopt veneer
  +0954:  call rax
  +0956:  lea rsp,[rsp+60h]
  +095e:  lea rsp,[rsp+98h]
  +0966:  pop rbx
  +0967:  pop rbp
  +0968:  ret
; ANNOTATION: COLD precise recovery block: second recursive result is not a FIXNUM (branch at +0527).
  +0969:  sub rsp,60h
  +0970:  test byte [rsp],0
  +0974:  mov rax,8E7h
  +097e:  mov [rsp],rax
  +0982:  mov rax,0Eh
  +098c:  mov [rsp+8],rax
  +0991:  mov rax,1
  +099b:  mov [rsp+10h],rax
  +09a0:  mov rax,2
  +09aa:  mov [rsp+18h],rax
  +09af:  mov rax,17h
  +09b9:  mov [rsp+20h],rax
  +09be:  mov rax,[rsp+0B0h]
  +09c6:  mov [rsp+28h],rax
  +09cb:  mov rax,rbx
  +09ce:  mov [rsp+30h],rax
  +09d3:  mov rax,1
  +09dd:  mov [rsp+38h],rax
  +09e2:  mov rax,7
  +09ec:  mov [rsp+40h],rax
  +09f1:  mov rax,rsp
  +09f4:  mov [rsp+48h],rax
  +09f9:  mov rax,[rsp+0D8h]
  +0a01:  mov [rsp+50h],rax
  +0a06:  lea rdi,[rsp+38h]
  +0a0e:  mov rax,7F3A547E0000h    ; native precise deopt veneer
  +0a18:  call rax
  +0a1a:  lea rsp,[rsp+60h]
  +0a22:  lea rsp,[rsp+98h]
  +0a2a:  pop rbx
  +0a2b:  pop rbp
  +0a2c:  ret
; ANNOTATION: COLD precise recovery block: sum overflow (branch at +0545).
  +0a2d:  sub rsp,60h
  +0a34:  test byte [rsp],0
  +0a38:  mov rax,8E7h
  +0a42:  mov [rsp],rax
  +0a46:  mov rax,0Eh
  +0a50:  mov [rsp+8],rax
  +0a55:  mov rax,1
  +0a5f:  mov [rsp+10h],rax
  +0a64:  mov rax,2
  +0a6e:  mov [rsp+18h],rax
  +0a73:  mov rax,17h
  +0a7d:  mov [rsp+20h],rax
  +0a82:  mov rax,[rsp+0B0h]
  +0a8a:  mov [rsp+28h],rax
  +0a8f:  mov rax,[rsp+0C0h]
  +0a97:  mov [rsp+30h],rax
  +0a9c:  mov rax,1
  +0aa6:  mov [rsp+38h],rax
  +0aab:  mov rax,7
  +0ab5:  mov [rsp+40h],rax
  +0aba:  mov rax,rsp
  +0abd:  mov [rsp+48h],rax
  +0ac2:  mov rax,[rsp+0D8h]
  +0aca:  mov [rsp+50h],rax
  +0acf:  lea rdi,[rsp+38h]
  +0ad7:  mov rax,7F3A547E0000h    ; native precise deopt veneer
  +0ae1:  call rax
  +0ae3:  lea rsp,[rsp+60h]
  +0aeb:  lea rsp,[rsp+98h]
  +0af3:  pop rbx
  +0af4:  pop rbp
  +0af5:  ret
