; Annotated exact first AFTER T2 listing from comparison-carry-591d3ada/after-0.stdout.
; Revision:591d3ada. Mapping size:2818 bytes. Base:140600565477376 =0x7fe01ec2b000.
; Original listing lines are unchanged; only comments with prefix '; ANNOTATION:' were inserted.
; Carry protocol: CF=0 is one value with empty/inactive MV; CF=1 is zero/many with runtime tuple state.
; 'Cold MV cleanup' below means conditional CF=1 fallthrough, not an out-of-line code allocation.
; On the admitted single-result recursion path, JAE skips both MV helper CALLs.
; This is static listing/source analysis, not dynamic instruction profiling.
FIB-CODE-BASE 140600565477376
; FIB — 1 arg(s), 1 local(s), 3 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; 2818 bytes of x86-64:
; Ordinary x86-64 Lisp ABI — installed native code.
; 2818 bytes of x86-64:
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
  +001f:  jmp 00007FE01EC2B078h
; ANNOTATION: PUBLIC ORDINARY ENTRY: RCX must equal 1. Recursive edges target +003e after this check.
  +0024:  cmp rcx,1
  +002b:  je near 00007FE01EC2B03Eh
; ANNOTATION: COLD wrong-arity veneer, reached only if the public argument-count check fails.
  +0031:  mov r11,7FE01EC3E000h
  +003b:  jmp r11
; ANNOTATION: DIRECT SELF TARGET: code base +0x3e =0x7fe01ec2b03e. Both relative recursive CALLs enter here after the arity check.
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
; ANNOTATION: BODY TYPE GUARD: low three bits must be zero for FIXNUM; failure -> cold precise recovery +0a5a.
  +0078:  test sil,7
  +007c:  jne near 00007FE01EC2BA5Ah
  +0082:  mov r9,rsi
  +0085:  mov [rsp+48h],r9
  +008a:  mov r9,[rsp+48h]
; ANNOTATION: Compare tagged n with2 (0x10); base case joins single-value return at +057c.
  +008f:  cmp r9,10h
  +0096:  jge near 00007FE01EC2B0A6h
  +009c:  mov rsi,[rsp+48h]
  +00a1:  jmp 00007FE01EC2B57Ch
  +00a6:  mov r9,[rsp+48h]
  +00ab:  mov r10,r9
; ANNOTATION: Tagged n-1 subtracts8; overflow -> cold precise recovery +08b9.
  +00ae:  sub r10,8
  +00b5:  jo near 00007FE01EC2B8B9h
  +00bb:  mov [rsp+20h],r10
; ANNOTATION: FIRST SELF EDGE: copy n-1 into writable outgoing argument home [rsp+0x68].
  +00c0:  mov rax,[rsp+20h]
  +00c5:  mov [rsp+68h],rax
; ANNOTATION: HOT admission: load saved control; NULL or execution !=R13 -> cold +0234.
  +00ca:  mov rdi,[rsp+78h]
  +00cf:  cmp rdi,0
  +00d6:  je near 00007FE01EC2B234h
  +00dc:  mov rax,[rdi]
  +00df:  cmp rax,r13
  +00e2:  jne near 00007FE01EC2B234h
; ANNOTATION: HOT: GC request byte. Nonzero -> +0234.
  +00e8:  mov r11,7FE04049A0E8h
  +00f2:  movzx eax,byte [r11]
  +00f7:  cmp rax,0
  +00fe:  jne near 00007FE01EC2B234h
; ANNOTATION: HOT: SIGUSR1 pending byte. Nonzero -> +0234.
  +0104:  mov r11,7FE04049E298h
  +010e:  movzx eax,byte [r11]
  +0113:  cmp rax,0
  +011a:  jne near 00007FE01EC2B234h
; ANNOTATION: HOT: posted-signal state. Nonzero -> +0234.
  +0120:  mov r11,7FE04049CD40h
  +012a:  mov rax,[r11]
  +012d:  cmp rax,0
  +0134:  jne near 00007FE01EC2B234h
; ANNOTATION: HOT: native-error state. Nonzero -> +0234.
  +013a:  mov r11,7FE04049CBF0h
  +0144:  mov rax,[r11]
  +0147:  cmp rax,0
  +014e:  jne near 00007FE01EC2B234h
; ANNOTATION: HOT: current signal epoch must equal cached control.seen_signal_epoch.
  +0154:  mov r11,7FE04049D970h
  +015e:  mov rax,[r11]
  +0161:  cmp rax,[rdi+30h]
  +0165:  jne near 00007FE01EC2B234h
; ANNOTATION: HOT: execution carrier must equal control.seen_carrier (migration awareness).
  +016b:  mov rax,[r13+10h]
  +016f:  cmp rax,[rdi+28h]
  +0173:  jne near 00007FE01EC2B234h
; ANNOTATION: HOT: execution yield request must be clear.
  +0179:  mov rax,[r13+18h]
  +017d:  cmp rax,0
  +0184:  jne near 00007FE01EC2B234h
; ANNOTATION: HOT: control.deadline_armed at +0x40 must be clear; no clock read here.
  +018a:  mov r11,rdi
  +018d:  add r11,40h
  +0194:  movzx eax,byte [r11]
  +0199:  cmp rax,0
  +01a0:  jne near 00007FE01EC2B234h
; ANNOTATION: HOT: indirect Env multiple-value-active byte must be clear for ordinary entry.
  +01a6:  mov r11,[rdi+18h]
  +01aa:  movzx eax,byte [r11]
  +01af:  cmp rax,0
  +01b6:  jne near 00007FE01EC2B234h
; ANNOTATION: HOT: current depth must be below control.depth_limit (+0x38).
  +01bc:  mov r11,[rdi+8]
  +01c0:  mov eax,[r11]
  +01c3:  mov edx,[rdi+38h]
  +01c6:  cmp rax,rdx
  +01c9:  jge near 00007FE01EC2B234h
; ANNOTATION: HOT stack admission: final callee bound =0x110 (272) bytes; reserve return word too.
  +01cf:  mov rdx,110h
  +01d9:  mov rax,rsp
  +01dc:  sub rax,8
; ANNOTATION: Borrow/underflow -> cold; then reject candidate bottom <= control.stack_limit (+0x58).
  +01e3:  jb near 00007FE01EC2B234h
  +01e9:  sub rax,rdx
  +01ec:  jb near 00007FE01EC2B234h
  +01f2:  mov rdx,[rdi+58h]
  +01f6:  cmp rax,rdx
  +01f9:  jbe near 00007FE01EC2B234h
; ANNOTATION: HOT admitted edge: increment native depth. No descriptor/revision/heat lookup for this self call.
  +01ff:  mov rdx,[rdi+8]
  +0203:  inc dword [rdx]
; ANNOTATION: ORDINARY ARGUMENTS: RDI=n-1, RCX=1, RAX=7 (tagged NIL closure), R13 unchanged.
  +0205:  mov rdi,[rsp+68h]
  +020a:  mov rcx,1
  +0214:  mov rax,7
; ANNOTATION: HOT direct relative self CALL #1 -> +003e, skipping the public arity check.
  +021e:  call 00007FE01EC2B03Eh
; ANNOTATION: CF LIVE: MOV/MOV/DEC/JMP all preserve carry from the returned result. Depth is balanced before the join.
  +0223:  mov r11,[rsp+78h]
  +0228:  mov r11,[r11+8]
  +022c:  dec dword [r11]
  +022f:  jmp 00007FE01EC2B2C8h
; ANNOTATION: COLD SELF #1: reached by failed admission checks, not by fallthrough from successful CALL.
  +0234:  sub rsp,40h
  +023b:  mov rax,8E7h
  +0245:  mov [rsp],rax
  +0249:  mov rax,1
  +0253:  mov [rsp+8],rax
  +0258:  mov rax,7
  +0262:  mov [rsp+18h],rax
  +0267:  mov rax,7FE03F635E38h
  +0271:  mov [rsp+28h],rax
  +0276:  mov rax,7FE03F635E78h
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
  +02b3:  mov r11,7FE01EC2D000h
; ANNOTATION: COLD helper CALL: can poll/collect/dispatch retained callable; joins normal result at +02c8.
  +02bd:  call r11
; ANNOTATION: CF LIVE on cold-admission return too: LEA restores stack without changing the helper return discriminator.
  +02c0:  lea rsp,[rsp+40h]
; ANNOTATION: CF LIVE: save primary RAX to its mapped home with MOV before consuming carry. No flag-setting work intervenes.
  +02c8:  mov [rsp+28h],rax
; ANNOTATION: IMMEDIATE CARRY CONSUMPTION #1: JAE is JNC, testing CF=0 (one value), NOT a new arithmetic comparison. Skip cleanup -> +02ee.
  +02cd:  jae near 00007FE01EC2B2EEh
; ANNOTATION: CF=1 ONLY: zero/many values were returned but continuation wants primary only; discard the runtime tuple.
  +02d3:  mov edi,0
  +02d8:  mov esi,0
  +02dd:  mov edx,0
; ANNOTATION: Conditional MV clear: nonallocating c2i_t2_mv zero-count helper. Primary already saved at [rsp+0x28].
  +02e2:  mov rax,7FE03FEEA470h
; ANNOTATION: COLD MV-cleanup CALL #1: skipped for the ordinary CF=0 single result; distinct from admission/deopt helpers.
  +02ec:  call rax
; ANNOTATION: JOIN normalized MV state; only now may arithmetic clobber flags. First primary remains in its native home.
  +02ee:  mov r9,[rsp+48h]
  +02f3:  mov r10,r9
; ANNOTATION: Tagged n-2 subtracts0x10; overflow -> cold precise recovery +0982.
  +02f6:  sub r10,10h
  +02fd:  jo near 00007FE01EC2B982h
  +0303:  mov [rsp+38h],r10
  +0308:  mov r9,[rsp+28h]
; ANNOTATION: Type-check first recursive primary after normalization; failure -> precise recovery +0654.
  +030d:  test r9b,7
  +0311:  jne near 00007FE01EC2B654h
  +0317:  mov r10,r9
  +031a:  mov [rsp+50h],r10
; ANNOTATION: SECOND SELF EDGE: copy n-2 into mapped outgoing home [rsp+0x68].
  +031f:  mov rax,[rsp+38h]
  +0324:  mov [rsp+68h],rax
; ANNOTATION: HOT admission: control NULL/execution mismatch -> cold +0493.
  +0329:  mov rdi,[rsp+78h]
  +032e:  cmp rdi,0
  +0335:  je near 00007FE01EC2B493h
  +033b:  mov rax,[rdi]
  +033e:  cmp rax,r13
  +0341:  jne near 00007FE01EC2B493h
; ANNOTATION: HOT GC-request check.
  +0347:  mov r11,7FE04049A0E8h
  +0351:  movzx eax,byte [r11]
  +0356:  cmp rax,0
  +035d:  jne near 00007FE01EC2B493h
; ANNOTATION: HOT SIGUSR1-pending check.
  +0363:  mov r11,7FE04049E298h
  +036d:  movzx eax,byte [r11]
  +0372:  cmp rax,0
  +0379:  jne near 00007FE01EC2B493h
; ANNOTATION: HOT posted-signal check.
  +037f:  mov r11,7FE04049CD40h
  +0389:  mov rax,[r11]
  +038c:  cmp rax,0
  +0393:  jne near 00007FE01EC2B493h
; ANNOTATION: HOT native-error check.
  +0399:  mov r11,7FE04049CBF0h
  +03a3:  mov rax,[r11]
  +03a6:  cmp rax,0
  +03ad:  jne near 00007FE01EC2B493h
; ANNOTATION: HOT signal-epoch check.
  +03b3:  mov r11,7FE04049D970h
  +03bd:  mov rax,[r11]
  +03c0:  cmp rax,[rdi+30h]
  +03c4:  jne near 00007FE01EC2B493h
; ANNOTATION: HOT carrier-identity check.
  +03ca:  mov rax,[r13+10h]
  +03ce:  cmp rax,[rdi+28h]
  +03d2:  jne near 00007FE01EC2B493h
; ANNOTATION: HOT yield-request check.
  +03d8:  mov rax,[r13+18h]
  +03dc:  cmp rax,0
  +03e3:  jne near 00007FE01EC2B493h
; ANNOTATION: HOT armed-deadline check.
  +03e9:  mov r11,rdi
  +03ec:  add r11,40h
  +03f3:  movzx eax,byte [r11]
  +03f8:  cmp rax,0
  +03ff:  jne near 00007FE01EC2B493h
; ANNOTATION: HOT active-MV check.
  +0405:  mov r11,[rdi+18h]
  +0409:  movzx eax,byte [r11]
  +040e:  cmp rax,0
  +0415:  jne near 00007FE01EC2B493h
; ANNOTATION: HOT native-depth limit check.
  +041b:  mov r11,[rdi+8]
  +041f:  mov eax,[r11]
  +0422:  mov edx,[rdi+38h]
  +0425:  cmp rax,rdx
  +0428:  jge near 00007FE01EC2B493h
; ANNOTATION: HOT physical-stack check: 272-byte published bound plus8-byte return word; reject underflow/bottom<=limit.
  +042e:  mov rdx,110h
  +0438:  mov rax,rsp
  +043b:  sub rax,8
  +0442:  jb near 00007FE01EC2B493h
  +0448:  sub rax,rdx
  +044b:  jb near 00007FE01EC2B493h
  +0451:  mov rdx,[rdi+58h]
  +0455:  cmp rax,rdx
  +0458:  jbe near 00007FE01EC2B493h
; ANNOTATION: Admitted child: increment native depth.
  +045e:  mov rdx,[rdi+8]
  +0462:  inc dword [rdx]
; ANNOTATION: ORDINARY ARGUMENTS: RDI=n-2, RCX=1, RAX=7 (NIL closure); R13 stays execution context.
  +0464:  mov rdi,[rsp+68h]
  +0469:  mov rcx,1
  +0473:  mov rax,7
; ANNOTATION: HOT direct relative self CALL #2 -> +003e. No named descriptor/revision/heat lookup.
  +047d:  call 00007FE01EC2B03Eh
; ANNOTATION: CF LIVE: reload control, decrement depth preserving CF, and jump over cold admission to +0527.
  +0482:  mov r11,[rsp+78h]
  +0487:  mov r11,[r11+8]
  +048b:  dec dword [r11]
  +048e:  jmp 00007FE01EC2B527h
; ANNOTATION: COLD ADMISSION #2: failed event/ownership/depth/stack check constructs64-byte rooted request.
  +0493:  sub rsp,40h
  +049a:  mov rax,8E7h
  +04a4:  mov [rsp],rax
  +04a8:  mov rax,1
  +04b2:  mov [rsp+8],rax
  +04b7:  mov rax,7
  +04c1:  mov [rsp+18h],rax
  +04c6:  mov rax,7FE03F635E38h
  +04d0:  mov [rsp+28h],rax
  +04d5:  mov rax,7FE03F635E78h
  +04df:  mov [rsp+30h],rax
; ANNOTATION: Request identity flag1 selects retained exact self callable, even if public FIB is replaced.
  +04e4:  mov rax,1
  +04ee:  mov [rsp+38h],rax
  +04f3:  mov rax,rsp
  +04f6:  add rax,0A8h
  +04fd:  mov [rsp+10h],rax
  +0502:  mov rax,[rsp+0B8h]
  +050a:  mov [rsp+20h],rax
  +050f:  mov rdi,rsp
  +0512:  mov r11,7FE01EC2D000h
; ANNOTATION: COLD published admission helper CALL: retains callable before polling, then invokes that callable.
  +051c:  call r11
; ANNOTATION: CF LIVE: LEA retires cold request without changing result carry.
  +051f:  lea rsp,[rsp+40h]
; ANNOTATION: CF LIVE: save primary in callee-saved RBX using MOV; both direct/cold admission paths meet here.
  +0527:  mov rbx,rax
; ANNOTATION: IMMEDIATE CARRY CONSUMPTION #2: JAE/JNC skips to +054b for CF=0. No intervening TEST/CMP destroys return carry.
  +052a:  jae near 00007FE01EC2B54Bh
; ANNOTATION: CF=1 ONLY: prepare zero-count MV-clear operation; RBX preserves the primary across helper call.
  +0530:  mov edi,0
  +0535:  mov esi,0
  +053a:  mov edx,0
  +053f:  mov rax,7FE03FEEA470h
; ANNOTATION: COLD MV-cleanup CALL #2: skipped for single-value results.
  +0549:  call rax
; ANNOTATION: JOIN normalized MV state; type-check second primary only after carry has been consumed. Failure -> +072c.
  +054b:  test bl,7
  +054e:  jne near 00007FE01EC2B72Ch
  +0554:  mov r9,rbx
  +0557:  mov [rsp+60h],r9
; ANNOTATION: Inline tagged addition of both recursive primaries; overflow -> precise recovery +07f0.
  +055c:  mov r9,[rsp+50h]
  +0561:  mov r10,[rsp+60h]
  +0566:  mov r11,r9
  +0569:  add r11,r10
  +056c:  jo near 00007FE01EC2B7F0h
; ANNOTATION: Store/load final sum. No unconditional MV-clear helper remains between addition and return.
  +0572:  mov [rsp+40h],r11
  +0577:  mov rsi,[rsp+40h]
; ANNOTATION: NORMAL RETURN JOIN for base case and sum: move primary to RAX.
  +057c:  mov rax,rsi
; ANNOTATION: Retire152-byte spills/padding with LEA, restore RBX/RBP; total frame retirement168 bytes.
  +057f:  lea rsp,[rsp+98h]
  +0587:  pop rbx
  +0588:  pop rbp
; ANNOTATION: CLC; RET publishes one value. The caller may rely on CF=0 meaning MV buffer already empty/inactive.
  +0589:  clc
  +058a:  ret
; ANNOTATION: Additional precise recovery block for bytecode position2; no direct branch here in this listing.
  +058b:  sub rsp,60h
  +0592:  test byte [rsp],0
  +0596:  mov rax,8E7h
  +05a0:  mov [rsp],rax
  +05a4:  mov rax,2
  +05ae:  mov [rsp+8],rax
  +05b3:  mov rax,1
  +05bd:  mov [rsp+10h],rax
  +05c2:  mov rax,2
  +05cc:  mov [rsp+18h],rax
  +05d1:  mov rax,[rsp+0A8h]
  +05d9:  mov [rsp+20h],rax
  +05de:  mov rax,[rsp+0A8h]
  +05e6:  mov [rsp+28h],rax
  +05eb:  mov rax,10h
  +05f5:  mov [rsp+30h],rax
  +05fa:  mov rax,1
  +0604:  mov [rsp+38h],rax
  +0609:  mov rax,7
  +0613:  mov [rsp+40h],rax
  +0618:  mov rax,rsp
  +061b:  mov [rsp+48h],rax
  +0620:  mov rax,[rsp+0D8h]
  +0628:  mov [rsp+50h],rax
  +062d:  lea rdi,[rsp+38h]
; ANNOTATION: Published precise deopt veneer: recovery payload/maps are built before invocation.
  +0635:  mov rax,7FE01EC28000h    ; native precise deopt veneer
  +063f:  call rax
  +0641:  lea rsp,[rsp+60h]
  +0649:  lea rsp,[rsp+98h]
  +0651:  pop rbx
  +0652:  pop rbp
  +0653:  ret
; ANNOTATION: COLD GUARD: first recursive primary was not FIXNUM (branch +0311).
  +0654:  sub rsp,60h
  +065b:  test byte [rsp],0
  +065f:  mov rax,8E7h
  +0669:  mov [rsp],rax
  +066d:  mov rax,0Ch
  +0677:  mov [rsp+8],rax
  +067c:  mov rax,1
  +0686:  mov [rsp+10h],rax
  +068b:  mov rax,3
  +0695:  mov [rsp+18h],rax
  +069a:  mov rax,17h
  +06a4:  mov [rsp+20h],rax
  +06a9:  mov rax,[rsp+88h]
  +06b1:  mov [rsp+28h],rax
  +06b6:  mov rax,[rsp+0A8h]
  +06be:  mov [rsp+30h],rax
  +06c3:  mov rax,10h
  +06cd:  mov [rsp+38h],rax
  +06d2:  mov rax,1
  +06dc:  mov [rsp+40h],rax
  +06e1:  mov rax,8
  +06eb:  mov [rsp+48h],rax
  +06f0:  mov rax,rsp
  +06f3:  mov [rsp+50h],rax
  +06f8:  mov rax,[rsp+0D8h]
  +0700:  mov [rsp+58h],rax
  +0705:  lea rdi,[rsp+40h]
  +070d:  mov rax,7FE01EC28000h    ; native precise deopt veneer
  +0717:  call rax
  +0719:  lea rsp,[rsp+60h]
  +0721:  lea rsp,[rsp+98h]
  +0729:  pop rbx
  +072a:  pop rbp
  +072b:  ret
; ANNOTATION: COLD GUARD: second recursive primary was not FIXNUM (branch +054e).
  +072c:  sub rsp,60h
  +0733:  test byte [rsp],0
  +0737:  mov rax,8E7h
  +0741:  mov [rsp],rax
  +0745:  mov rax,0Eh
  +074f:  mov [rsp+8],rax
  +0754:  mov rax,1
  +075e:  mov [rsp+10h],rax
  +0763:  mov rax,2
  +076d:  mov [rsp+18h],rax
  +0772:  mov rax,17h
  +077c:  mov [rsp+20h],rax
  +0781:  mov rax,[rsp+0B0h]
  +0789:  mov [rsp+28h],rax
  +078e:  mov rax,rbx
  +0791:  mov [rsp+30h],rax
  +0796:  mov rax,1
  +07a0:  mov [rsp+38h],rax
  +07a5:  mov rax,7
  +07af:  mov [rsp+40h],rax
  +07b4:  mov rax,rsp
  +07b7:  mov [rsp+48h],rax
  +07bc:  mov rax,[rsp+0D8h]
  +07c4:  mov [rsp+50h],rax
  +07c9:  lea rdi,[rsp+38h]
  +07d1:  mov rax,7FE01EC28000h    ; native precise deopt veneer
  +07db:  call rax
  +07dd:  lea rsp,[rsp+60h]
  +07e5:  lea rsp,[rsp+98h]
  +07ed:  pop rbx
  +07ee:  pop rbp
  +07ef:  ret
; ANNOTATION: COLD GUARD: tagged sum overflow (branch +056c).
  +07f0:  sub rsp,60h
  +07f7:  test byte [rsp],0
  +07fb:  mov rax,8E7h
  +0805:  mov [rsp],rax
  +0809:  mov rax,0Eh
  +0813:  mov [rsp+8],rax
  +0818:  mov rax,1
  +0822:  mov [rsp+10h],rax
  +0827:  mov rax,2
  +0831:  mov [rsp+18h],rax
  +0836:  mov rax,17h
  +0840:  mov [rsp+20h],rax
  +0845:  mov rax,[rsp+0B0h]
  +084d:  mov [rsp+28h],rax
  +0852:  mov rax,[rsp+0C0h]
  +085a:  mov [rsp+30h],rax
  +085f:  mov rax,1
  +0869:  mov [rsp+38h],rax
  +086e:  mov rax,7
  +0878:  mov [rsp+40h],rax
  +087d:  mov rax,rsp
  +0880:  mov [rsp+48h],rax
  +0885:  mov rax,[rsp+0D8h]
  +088d:  mov [rsp+50h],rax
  +0892:  lea rdi,[rsp+38h]
  +089a:  mov rax,7FE01EC28000h    ; native precise deopt veneer
  +08a4:  call rax
  +08a6:  lea rsp,[rsp+60h]
  +08ae:  lea rsp,[rsp+98h]
  +08b6:  pop rbx
  +08b7:  pop rbp
  +08b8:  ret
; ANNOTATION: COLD GUARD: n-1 overflow (branch +00b5).
  +08b9:  sub rsp,60h
  +08c0:  test byte [rsp],0
  +08c4:  mov rax,8E7h
  +08ce:  mov [rsp],rax
  +08d2:  mov rax,8
  +08dc:  mov [rsp+8],rax
  +08e1:  mov rax,1
  +08eb:  mov [rsp+10h],rax
  +08f0:  mov rax,2
  +08fa:  mov [rsp+18h],rax
  +08ff:  mov rax,[rsp+0A8h]
  +0907:  mov [rsp+20h],rax
  +090c:  mov rax,[rsp+0A8h]
  +0914:  mov [rsp+28h],rax
  +0919:  mov rax,8
  +0923:  mov [rsp+30h],rax
  +0928:  mov rax,1
  +0932:  mov [rsp+38h],rax
  +0937:  mov rax,7
  +0941:  mov [rsp+40h],rax
  +0946:  mov rax,rsp
  +0949:  mov [rsp+48h],rax
  +094e:  mov rax,[rsp+0D8h]
  +0956:  mov [rsp+50h],rax
  +095b:  lea rdi,[rsp+38h]
  +0963:  mov rax,7FE01EC28000h    ; native precise deopt veneer
  +096d:  call rax
  +096f:  lea rsp,[rsp+60h]
  +0977:  lea rsp,[rsp+98h]
  +097f:  pop rbx
  +0980:  pop rbp
  +0981:  ret
; ANNOTATION: COLD GUARD: n-2 overflow (branch +02fd).
  +0982:  sub rsp,60h
  +0989:  test byte [rsp],0
  +098d:  mov rax,8E7h
  +0997:  mov [rsp],rax
  +099b:  mov rax,0Ch
  +09a5:  mov [rsp+8],rax
  +09aa:  mov rax,1
  +09b4:  mov [rsp+10h],rax
  +09b9:  mov rax,3
  +09c3:  mov [rsp+18h],rax
  +09c8:  mov rax,17h
  +09d2:  mov [rsp+20h],rax
  +09d7:  mov rax,[rsp+88h]
  +09df:  mov [rsp+28h],rax
  +09e4:  mov rax,[rsp+0A8h]
  +09ec:  mov [rsp+30h],rax
  +09f1:  mov rax,10h
  +09fb:  mov [rsp+38h],rax
  +0a00:  mov rax,1
  +0a0a:  mov [rsp+40h],rax
  +0a0f:  mov rax,8
  +0a19:  mov [rsp+48h],rax
  +0a1e:  mov rax,rsp
  +0a21:  mov [rsp+50h],rax
  +0a26:  mov rax,[rsp+0D8h]
  +0a2e:  mov [rsp+58h],rax
  +0a33:  lea rdi,[rsp+40h]
  +0a3b:  mov rax,7FE01EC28000h    ; native precise deopt veneer
  +0a45:  call rax
  +0a47:  lea rsp,[rsp+60h]
  +0a4f:  lea rsp,[rsp+98h]
  +0a57:  pop rbx
  +0a58:  pop rbp
  +0a59:  ret
; ANNOTATION: COLD GUARD: incoming n not FIXNUM (branch +007c).
  +0a5a:  sub rsp,50h
  +0a61:  test byte [rsp],0
  +0a65:  mov rax,8E7h
  +0a6f:  mov [rsp],rax
  +0a73:  mov rax,0
  +0a7d:  mov [rsp+8],rax
  +0a82:  mov rax,1
  +0a8c:  mov [rsp+10h],rax
  +0a91:  mov rax,0
  +0a9b:  mov [rsp+18h],rax
  +0aa0:  mov rax,rsi
  +0aa3:  mov [rsp+20h],rax
  +0aa8:  mov rax,1
  +0ab2:  mov [rsp+28h],rax
  +0ab7:  mov rax,5
  +0ac1:  mov [rsp+30h],rax
  +0ac6:  mov rax,rsp
  +0ac9:  mov [rsp+38h],rax
  +0ace:  mov rax,[rsp+0C8h]
  +0ad6:  mov [rsp+40h],rax
  +0adb:  lea rdi,[rsp+28h]
  +0ae3:  mov rax,7FE01EC28000h    ; native precise deopt veneer
  +0aed:  call rax
  +0aef:  lea rsp,[rsp+50h]
  +0af7:  lea rsp,[rsp+98h]
  +0aff:  pop rbx
  +0b00:  pop rbp
  +0b01:  ret
