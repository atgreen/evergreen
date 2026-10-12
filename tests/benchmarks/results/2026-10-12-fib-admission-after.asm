; Actual installed T2 disassembly from after-0.stdout; addresses are run-specific.
; Annotation-only additions. SBCL listing is preserved in the evidence archive.

FIB-CODE-BASE 140244935524352
; FIB — 1 arg(s), 1 local(s), 3 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; 2657 bytes of x86-64:
; Ordinary x86-64 Lisp ABI — installed native code.
; 2657 bytes of x86-64:
  +0000:  ud2
  +0002:  push rbp
  +0003:  mov rbp,rsp
  +0006:  push rbx
  +0007:  sub rsp,8
  +000b:  sub rsp,70h
  +0012:  test byte [rsp],0
  +0016:  mov [rsp+50h],rdi
  +001b:  mov rsi,[rdi]
  +001f:  jmp 00007F8D51904075h
; Public entry checks arity. Direct self calls skip only this check.
  +0024:  cmp rcx,1
  +002b:  je near 00007F8D5190403Eh
  +0031:  mov r11,7F8D51917000h
  +003b:  jmp r11
; Direct-self entry: every invocation, including base cases, constructs this frame.
; 136 bytes below entry RSP: saved RBP/RBX, 8-byte padding, 112-byte local area.
  +003e:  push rbp
  +003f:  mov rbp,rsp
  +0042:  push rbx
  +0043:  sub rsp,8
  +0047:  sub rsp,70h
  +004e:  test byte [rsp],0
  +0052:  mov rsi,rdi
  +0055:  mov rax,0
  +005f:  mov [rsp+50h],rax
  +0064:  mov rax,[r13+20h]
  +0068:  mov [rsp+58h],rax
  +006d:  mov rax,rsi
  +0070:  mov [rsp+60h],rax
; Guard the untyped argument as a fixnum; guard failure uses framed deoptimization.
  +0075:  test sil,7
  +0079:  jne near 00007F8D51904752h
  +007f:  mov r9,rsi
  +0082:  mov [rsp+28h],r9
  +0087:  mov r9,[rsp+28h]
  +008c:  cmp r9,10h
  +0093:  jge near 00007F8D519040A3h
  +0099:  mov rsi,[rsp+28h]
  +009e:  jmp 00007F8D519044E1h
  +00a3:  mov r9,[rsp+28h]
  +00a8:  mov r10,r9
  +00ab:  sub r10,8
  +00b2:  jo near 00007F8D519044F0h
  +00b8:  mov [rsp],r10
  +00bc:  mov rax,[rsp]
  +00c0:  mov [rsp+48h],rax
; First recursive-call admission. Existing safety observations remain.
; No MV-active load here: dataflow proves an empty tuple before this call.
  +00c5:  mov rdi,[rsp+58h]
  +00ca:  cmp rdi,0
  +00d1:  je near 00007F8D519041E4h
  +00d7:  cmp r13,[rdi]
  +00db:  jne near 00007F8D519041E4h
  +00e1:  mov r11,7F8D731750E8h
; Observe GC pending directly in byte memory; no MOVZX scratch-register round trip.
  +00eb:  cmp byte [r11],0
  +00f0:  jne near 00007F8D519041E4h
  +00f6:  mov r11,7F8D73179298h
  +0100:  cmp byte [r11],0
  +0105:  jne near 00007F8D519041E4h
  +010b:  mov r11,7F8D73177D40h
; Posted signals remain a full-width qword observation.
  +0115:  cmp qword [r11],0
  +011a:  jne near 00007F8D519041E4h
  +0120:  mov r11,7F8D73177BF0h
  +012a:  cmp qword [r11],0
  +012f:  jne near 00007F8D519041E4h
  +0135:  mov r11,7F8D73178970h
  +013f:  mov rax,[r11]
  +0142:  cmp rax,[rdi+30h]
  +0146:  jne near 00007F8D519041E4h
  +014c:  mov rax,[r13+10h]
  +0150:  cmp rax,[rdi+28h]
  +0154:  jne near 00007F8D519041E4h
; Yield remains a full-width observation; deadline uses one byte.
  +015a:  cmp qword [r13+18h],0
  +015f:  jne near 00007F8D519041E4h
  +0165:  cmp byte [rdi+40h],0
  +0169:  jne near 00007F8D519041E4h
; Depth and stack admission remain before the direct native call.
  +016f:  mov r11,[rdi+8]
  +0173:  mov eax,[r11]
  +0176:  mov edx,[rdi+38h]
  +0179:  cmp rax,rdx
  +017c:  jge near 00007F8D519041E4h
  +0182:  mov rdx,0F0h
  +018c:  mov rax,rsp
  +018f:  sub rax,8
  +0196:  jb near 00007F8D519041E4h
  +019c:  sub rax,rdx
  +019f:  jb near 00007F8D519041E4h
  +01a5:  cmp rax,[rdi+58h]
  +01a9:  jbe near 00007F8D519041E4h
  +01af:  mov rdx,[rdi+8]
  +01b3:  inc dword [rdx]
  +01b5:  mov rdi,[rsp+48h]
  +01ba:  mov rcx,1
  +01c4:  mov rax,7
; Direct self call to +003e; RDI argument, RAX result, CF value-count descriptor.
  +01ce:  call 00007F8D5190403Eh
; Retire depth with MOV/DEC/JMP: carry is preserved.
  +01d3:  mov r11,[rsp+58h]
  +01d8:  mov r11,[r11+8]
  +01dc:  dec dword [r11]
  +01df:  jmp 00007F8D51904278h
  +01e4:  sub rsp,40h
  +01eb:  mov rax,8E7h
  +01f5:  mov [rsp],rax
  +01f9:  mov rax,1
  +0203:  mov [rsp+8],rax
  +0208:  mov rax,7
  +0212:  mov [rsp+18h],rax
  +0217:  mov rax,7F8D72235F38h
  +0221:  mov [rsp+28h],rax
  +0226:  mov rax,7F8D72235F78h
  +0230:  mov [rsp+30h],rax
  +0235:  mov rax,1
  +023f:  mov [rsp+38h],rax
  +0244:  mov rax,rsp
  +0247:  add rax,88h
  +024e:  mov [rsp+10h],rax
  +0253:  mov rax,[rsp+98h]
  +025b:  mov [rsp+20h],rax
  +0260:  mov rdi,rsp
  +0263:  mov r11,7F8D51906000h
  +026d:  call r11
  +0270:  lea rsp,[rsp+40h]
; Save primary result; JAE is the disassembler spelling of JNC.
; CF=0 skips the MV-clear helper; CF=1 discards its secondary tuple.
  +0278:  mov [rsp+8],rax
  +027d:  jae near 00007F8D5190429Eh
  +0283:  mov edi,0
  +0288:  mov esi,0
  +028d:  mov edx,0
  +0292:  mov rax,7F8D72BC5470h
  +029c:  call rax
  +029e:  mov r9,[rsp+28h]
  +02a3:  mov r10,r9
  +02a6:  sub r10,10h
  +02ad:  jo near 00007F8D519045B9h
  +02b3:  mov [rsp+18h],r10
  +02b8:  mov r9,[rsp+8]
  +02bd:  test r9b,7
  +02c1:  jne near 00007F8D519048C3h
  +02c7:  mov r10,r9
  +02ca:  mov [rsp+30h],r10
  +02cf:  mov rax,[rsp+18h]
  +02d4:  mov [rsp+48h],rax
; Second recursive-call admission: previous normalization proves empty MV state.
  +02d9:  mov rdi,[rsp+58h]
  +02de:  cmp rdi,0
  +02e5:  je near 00007F8D519043F8h
  +02eb:  cmp r13,[rdi]
  +02ef:  jne near 00007F8D519043F8h
  +02f5:  mov r11,7F8D731750E8h
  +02ff:  cmp byte [r11],0
  +0304:  jne near 00007F8D519043F8h
  +030a:  mov r11,7F8D73179298h
  +0314:  cmp byte [r11],0
  +0319:  jne near 00007F8D519043F8h
  +031f:  mov r11,7F8D73177D40h
  +0329:  cmp qword [r11],0
  +032e:  jne near 00007F8D519043F8h
  +0334:  mov r11,7F8D73177BF0h
  +033e:  cmp qword [r11],0
  +0343:  jne near 00007F8D519043F8h
  +0349:  mov r11,7F8D73178970h
  +0353:  mov rax,[r11]
  +0356:  cmp rax,[rdi+30h]
  +035a:  jne near 00007F8D519043F8h
  +0360:  mov rax,[r13+10h]
  +0364:  cmp rax,[rdi+28h]
  +0368:  jne near 00007F8D519043F8h
  +036e:  cmp qword [r13+18h],0
  +0373:  jne near 00007F8D519043F8h
  +0379:  cmp byte [rdi+40h],0
  +037d:  jne near 00007F8D519043F8h
  +0383:  mov r11,[rdi+8]
  +0387:  mov eax,[r11]
  +038a:  mov edx,[rdi+38h]
  +038d:  cmp rax,rdx
  +0390:  jge near 00007F8D519043F8h
  +0396:  mov rdx,0F0h
  +03a0:  mov rax,rsp
  +03a3:  sub rax,8
  +03aa:  jb near 00007F8D519043F8h
  +03b0:  sub rax,rdx
  +03b3:  jb near 00007F8D519043F8h
  +03b9:  cmp rax,[rdi+58h]
  +03bd:  jbe near 00007F8D519043F8h
  +03c3:  mov rdx,[rdi+8]
  +03c7:  inc dword [rdx]
  +03c9:  mov rdi,[rsp+48h]
  +03ce:  mov rcx,1
  +03d8:  mov rax,7
; Second direct self call; same carry-preserving retirement and normalization.
  +03e2:  call 00007F8D5190403Eh
  +03e7:  mov r11,[rsp+58h]
  +03ec:  mov r11,[r11+8]
  +03f0:  dec dword [r11]
  +03f3:  jmp 00007F8D5190448Ch
  +03f8:  sub rsp,40h
  +03ff:  mov rax,8E7h
  +0409:  mov [rsp],rax
  +040d:  mov rax,1
  +0417:  mov [rsp+8],rax
  +041c:  mov rax,7
  +0426:  mov [rsp+18h],rax
  +042b:  mov rax,7F8D72235F38h
  +0435:  mov [rsp+28h],rax
  +043a:  mov rax,7F8D72235F78h
  +0444:  mov [rsp+30h],rax
  +0449:  mov rax,1
  +0453:  mov [rsp+38h],rax
  +0458:  mov rax,rsp
  +045b:  add rax,88h
  +0462:  mov [rsp+10h],rax
  +0467:  mov rax,[rsp+98h]
  +046f:  mov [rsp+20h],rax
  +0474:  mov rdi,rsp
  +0477:  mov r11,7F8D51906000h
  +0481:  call r11
  +0484:  lea rsp,[rsp+40h]
  +048c:  mov rbx,rax
  +048f:  jae near 00007F8D519044B0h
  +0495:  mov edi,0
  +049a:  mov esi,0
  +049f:  mov edx,0
  +04a4:  mov rax,7F8D72BC5470h
  +04ae:  call rax
  +04b0:  test bl,7
  +04b3:  jne near 00007F8D5190468Eh
  +04b9:  mov r9,rbx
  +04bc:  mov [rsp+40h],r9
  +04c1:  mov r9,[rsp+30h]
  +04c6:  mov r10,[rsp+40h]
  +04cb:  mov r11,r9
  +04ce:  add r11,r10
  +04d1:  jo near 00007F8D51904998h
  +04d7:  mov [rsp+20h],r11
  +04dc:  mov rsi,[rsp+20h]
; Shared return, including base case: put primary in RAX, restore frame, clear CF.
  +04e1:  mov rax,rsi
  +04e4:  lea rsp,[rsp+78h]
  +04ec:  pop rbx
  +04ed:  pop rbp
  +04ee:  clc
  +04ef:  ret
  +04f0:  sub rsp,60h
  +04f7:  test byte [rsp],0
  +04fb:  mov rax,8E7h
  +0505:  mov [rsp],rax
  +0509:  mov rax,8
  +0513:  mov [rsp+8],rax
  +0518:  mov rax,1
  +0522:  mov [rsp+10h],rax
  +0527:  mov rax,2
  +0531:  mov [rsp+18h],rax
  +0536:  mov rax,[rsp+88h]
  +053e:  mov [rsp+20h],rax
  +0543:  mov rax,[rsp+88h]
  +054b:  mov [rsp+28h],rax
  +0550:  mov rax,8
  +055a:  mov [rsp+30h],rax
  +055f:  mov rax,1
  +0569:  mov [rsp+38h],rax
  +056e:  mov rax,7
  +0578:  mov [rsp+40h],rax
  +057d:  mov rax,rsp
  +0580:  mov [rsp+48h],rax
  +0585:  mov rax,[rsp+0B8h]
  +058d:  mov [rsp+50h],rax
  +0592:  lea rdi,[rsp+38h]
  +059a:  mov rax,7F8D51903000h    ; native precise deopt veneer
  +05a4:  call rax
  +05a6:  lea rsp,[rsp+60h]
  +05ae:  lea rsp,[rsp+78h]
  +05b6:  pop rbx
  +05b7:  pop rbp
  +05b8:  ret
  +05b9:  sub rsp,60h
  +05c0:  test byte [rsp],0
  +05c4:  mov rax,8E7h
  +05ce:  mov [rsp],rax
  +05d2:  mov rax,0Ch
  +05dc:  mov [rsp+8],rax
  +05e1:  mov rax,1
  +05eb:  mov [rsp+10h],rax
  +05f0:  mov rax,3
  +05fa:  mov [rsp+18h],rax
  +05ff:  mov rax,17h
  +0609:  mov [rsp+20h],rax
  +060e:  mov rax,[rsp+68h]
  +0613:  mov [rsp+28h],rax
  +0618:  mov rax,[rsp+88h]
  +0620:  mov [rsp+30h],rax
  +0625:  mov rax,10h
  +062f:  mov [rsp+38h],rax
  +0634:  mov rax,1
  +063e:  mov [rsp+40h],rax
  +0643:  mov rax,8
  +064d:  mov [rsp+48h],rax
  +0652:  mov rax,rsp
  +0655:  mov [rsp+50h],rax
  +065a:  mov rax,[rsp+0B8h]
  +0662:  mov [rsp+58h],rax
  +0667:  lea rdi,[rsp+40h]
  +066f:  mov rax,7F8D51903000h    ; native precise deopt veneer
  +0679:  call rax
  +067b:  lea rsp,[rsp+60h]
  +0683:  lea rsp,[rsp+78h]
  +068b:  pop rbx
  +068c:  pop rbp
  +068d:  ret
  +068e:  sub rsp,60h
  +0695:  test byte [rsp],0
  +0699:  mov rax,8E7h
  +06a3:  mov [rsp],rax
  +06a7:  mov rax,0Eh
  +06b1:  mov [rsp+8],rax
  +06b6:  mov rax,1
  +06c0:  mov [rsp+10h],rax
  +06c5:  mov rax,2
  +06cf:  mov [rsp+18h],rax
  +06d4:  mov rax,17h
  +06de:  mov [rsp+20h],rax
  +06e3:  mov rax,[rsp+90h]
  +06eb:  mov [rsp+28h],rax
  +06f0:  mov rax,rbx
  +06f3:  mov [rsp+30h],rax
  +06f8:  mov rax,1
  +0702:  mov [rsp+38h],rax
  +0707:  mov rax,7
  +0711:  mov [rsp+40h],rax
  +0716:  mov rax,rsp
  +0719:  mov [rsp+48h],rax
  +071e:  mov rax,[rsp+0B8h]
  +0726:  mov [rsp+50h],rax
  +072b:  lea rdi,[rsp+38h]
  +0733:  mov rax,7F8D51903000h    ; native precise deopt veneer
  +073d:  call rax
  +073f:  lea rsp,[rsp+60h]
  +0747:  lea rsp,[rsp+78h]
  +074f:  pop rbx
  +0750:  pop rbp
  +0751:  ret
  +0752:  sub rsp,50h
  +0759:  test byte [rsp],0
  +075d:  mov rax,8E7h
  +0767:  mov [rsp],rax
  +076b:  mov rax,0
  +0775:  mov [rsp+8],rax
  +077a:  mov rax,1
  +0784:  mov [rsp+10h],rax
  +0789:  mov rax,0
  +0793:  mov [rsp+18h],rax
  +0798:  mov rax,rsi
  +079b:  mov [rsp+20h],rax
  +07a0:  mov rax,1
  +07aa:  mov [rsp+28h],rax
  +07af:  mov rax,5
  +07b9:  mov [rsp+30h],rax
  +07be:  mov rax,rsp
  +07c1:  mov [rsp+38h],rax
  +07c6:  mov rax,[rsp+0A8h]
  +07ce:  mov [rsp+40h],rax
  +07d3:  lea rdi,[rsp+28h]
  +07db:  mov rax,7F8D51903000h    ; native precise deopt veneer
  +07e5:  call rax
  +07e7:  lea rsp,[rsp+50h]
  +07ef:  lea rsp,[rsp+78h]
  +07f7:  pop rbx
  +07f8:  pop rbp
  +07f9:  ret
  +07fa:  sub rsp,60h
  +0801:  test byte [rsp],0
  +0805:  mov rax,8E7h
  +080f:  mov [rsp],rax
  +0813:  mov rax,2
  +081d:  mov [rsp+8],rax
  +0822:  mov rax,1
  +082c:  mov [rsp+10h],rax
  +0831:  mov rax,2
  +083b:  mov [rsp+18h],rax
  +0840:  mov rax,[rsp+88h]
  +0848:  mov [rsp+20h],rax
  +084d:  mov rax,[rsp+88h]
  +0855:  mov [rsp+28h],rax
  +085a:  mov rax,10h
  +0864:  mov [rsp+30h],rax
  +0869:  mov rax,1
  +0873:  mov [rsp+38h],rax
  +0878:  mov rax,7
  +0882:  mov [rsp+40h],rax
  +0887:  mov rax,rsp
  +088a:  mov [rsp+48h],rax
  +088f:  mov rax,[rsp+0B8h]
  +0897:  mov [rsp+50h],rax
  +089c:  lea rdi,[rsp+38h]
  +08a4:  mov rax,7F8D51903000h    ; native precise deopt veneer
  +08ae:  call rax
  +08b0:  lea rsp,[rsp+60h]
  +08b8:  lea rsp,[rsp+78h]
  +08c0:  pop rbx
  +08c1:  pop rbp
  +08c2:  ret
  +08c3:  sub rsp,60h
  +08ca:  test byte [rsp],0
  +08ce:  mov rax,8E7h
  +08d8:  mov [rsp],rax
  +08dc:  mov rax,0Ch
  +08e6:  mov [rsp+8],rax
  +08eb:  mov rax,1
  +08f5:  mov [rsp+10h],rax
  +08fa:  mov rax,3
  +0904:  mov [rsp+18h],rax
  +0909:  mov rax,17h
  +0913:  mov [rsp+20h],rax
  +0918:  mov rax,[rsp+68h]
  +091d:  mov [rsp+28h],rax
  +0922:  mov rax,[rsp+88h]
  +092a:  mov [rsp+30h],rax
  +092f:  mov rax,10h
  +0939:  mov [rsp+38h],rax
  +093e:  mov rax,1
  +0948:  mov [rsp+40h],rax
  +094d:  mov rax,8
  +0957:  mov [rsp+48h],rax
  +095c:  mov rax,rsp
  +095f:  mov [rsp+50h],rax
  +0964:  mov rax,[rsp+0B8h]
  +096c:  mov [rsp+58h],rax
  +0971:  lea rdi,[rsp+40h]
  +0979:  mov rax,7F8D51903000h    ; native precise deopt veneer
  +0983:  call rax
  +0985:  lea rsp,[rsp+60h]
  +098d:  lea rsp,[rsp+78h]
  +0995:  pop rbx
  +0996:  pop rbp
  +0997:  ret
  +0998:  sub rsp,60h
  +099f:  test byte [rsp],0
  +09a3:  mov rax,8E7h
  +09ad:  mov [rsp],rax
  +09b1:  mov rax,0Eh
  +09bb:  mov [rsp+8],rax
  +09c0:  mov rax,1
  +09ca:  mov [rsp+10h],rax
  +09cf:  mov rax,2
  +09d9:  mov [rsp+18h],rax
  +09de:  mov rax,17h
  +09e8:  mov [rsp+20h],rax
  +09ed:  mov rax,[rsp+90h]
  +09f5:  mov [rsp+28h],rax
  +09fa:  mov rax,[rsp+0A0h]
  +0a02:  mov [rsp+30h],rax
  +0a07:  mov rax,1
  +0a11:  mov [rsp+38h],rax
  +0a16:  mov rax,7
  +0a20:  mov [rsp+40h],rax
  +0a25:  mov rax,rsp
  +0a28:  mov [rsp+48h],rax
  +0a2d:  mov rax,[rsp+0B8h]
  +0a35:  mov [rsp+50h],rax
  +0a3a:  lea rdi,[rsp+38h]
  +0a42:  mov rax,7F8D51903000h    ; native precise deopt veneer
  +0a4c:  call rax
  +0a4e:  lea rsp,[rsp+60h]
  +0a56:  lea rsp,[rsp+78h]
  +0a5e:  pop rbx
  +0a5f:  pop rbp
  +0a60:  ret
