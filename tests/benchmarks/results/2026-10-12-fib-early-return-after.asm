; Installed production Fibonacci, 1b2cd236; CPU/runtime addresses belong to this captured process.
FIB-CODE-BASE 139729446580224
; FIB — 1 arg(s), 1 local(s), 3 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; 2685 bytes of x86-64:
; Ordinary x86-64 Lisp ABI — installed native code.
; 2685 bytes of x86-64:
  +0000:  ud2
  +0002:  push rbp
  +0003:  mov rbp,rsp
  +0006:  push rbx
  +0007:  sub rsp,8
  +000b:  sub rsp,70h
  +0012:  test byte [rsp],0
  +0016:  mov [rsp+50h],rdi
  +001b:  mov rsi,[rdi]
  +001f:  jmp 00007F154C073091h
; Public entry checks arity before any early return.
  +0024:  cmp rcx,1
  +002b:  je near 00007F154C07303Eh
  +0031:  mov r11,7F154C086000h
  +003b:  jmp r11
; Public accepted entry and both direct self calls enter here. Guard original argument tag.
  +003e:  test dil,7
  +0042:  jne near 00007F154C07305Ah
; Compare tagged argument with tagged 2; non-base cases take the unchanged framed body.
  +0048:  cmp rdi,10h
  +004f:  jge near 00007F154C07305Ah
; Return original argument; no frame, root publication, helper or safepoint.
  +0055:  mov rax,rdi
; Carry clear denotes exactly one value.
  +0058:  clc
  +0059:  ret
; Fallback constructs the original 136-byte frame below entry RSP.
  +005a:  push rbp
  +005b:  mov rbp,rsp
  +005e:  push rbx
  +005f:  sub rsp,8
  +0063:  sub rsp,70h
  +006a:  test byte [rsp],0
  +006e:  mov rsi,rdi
  +0071:  mov rax,0
  +007b:  mov [rsp+50h],rax
  +0080:  mov rax,[r13+20h]
  +0084:  mov [rsp+58h],rax
  +0089:  mov rax,rsi
  +008c:  mov [rsp+60h],rax
  +0091:  test sil,7
  +0095:  jne near 00007F154C0735D5h
  +009b:  mov r9,rsi
  +009e:  mov [rsp+28h],r9
  +00a3:  mov r9,[rsp+28h]
  +00a8:  cmp r9,10h
  +00af:  jge near 00007F154C0730BFh
  +00b5:  mov rsi,[rsp+28h]
  +00ba:  jmp 00007F154C0734FDh
  +00bf:  mov r9,[rsp+28h]
  +00c4:  mov r10,r9
  +00c7:  sub r10,8
  +00ce:  jo near 00007F154C073752h
  +00d4:  mov [rsp],r10
  +00d8:  mov rax,[rsp]
  +00dc:  mov [rsp+48h],rax
  +00e1:  mov rdi,[rsp+58h]
  +00e6:  cmp rdi,0
  +00ed:  je near 00007F154C073200h
  +00f3:  cmp r13,[rdi]
  +00f7:  jne near 00007F154C073200h
  +00fd:  mov r11,7F156D8E60E8h
  +0107:  cmp byte [r11],0
  +010c:  jne near 00007F154C073200h
  +0112:  mov r11,7F156D8EA298h
  +011c:  cmp byte [r11],0
  +0121:  jne near 00007F154C073200h
  +0127:  mov r11,7F156D8E8D40h
  +0131:  cmp qword [r11],0
  +0136:  jne near 00007F154C073200h
  +013c:  mov r11,7F156D8E8BF0h
  +0146:  cmp qword [r11],0
  +014b:  jne near 00007F154C073200h
  +0151:  mov r11,7F156D8E9970h
  +015b:  mov rax,[r11]
  +015e:  cmp rax,[rdi+30h]
  +0162:  jne near 00007F154C073200h
  +0168:  mov rax,[r13+10h]
  +016c:  cmp rax,[rdi+28h]
  +0170:  jne near 00007F154C073200h
  +0176:  cmp qword [r13+18h],0
  +017b:  jne near 00007F154C073200h
  +0181:  cmp byte [rdi+40h],0
  +0185:  jne near 00007F154C073200h
  +018b:  mov r11,[rdi+8]
  +018f:  mov eax,[r11]
  +0192:  mov edx,[rdi+38h]
  +0195:  cmp rax,rdx
  +0198:  jge near 00007F154C073200h
  +019e:  mov rdx,0F0h
  +01a8:  mov rax,rsp
  +01ab:  sub rax,8
  +01b2:  jb near 00007F154C073200h
  +01b8:  sub rax,rdx
  +01bb:  jb near 00007F154C073200h
  +01c1:  cmp rax,[rdi+58h]
  +01c5:  jbe near 00007F154C073200h
  +01cb:  mov rdx,[rdi+8]
  +01cf:  inc dword [rdx]
  +01d1:  mov rdi,[rsp+48h]
  +01d6:  mov rcx,1
  +01e0:  mov rax,7
  +01ea:  call 00007F154C07303Eh
  +01ef:  mov r11,[rsp+58h]
  +01f4:  mov r11,[r11+8]
  +01f8:  dec dword [r11]
  +01fb:  jmp 00007F154C073294h
  +0200:  sub rsp,40h
  +0207:  mov rax,8E7h
  +0211:  mov [rsp],rax
  +0215:  mov rax,1
  +021f:  mov [rsp+8],rax
  +0224:  mov rax,7
  +022e:  mov [rsp+18h],rax
  +0233:  mov rax,7F154C89AE38h
  +023d:  mov [rsp+28h],rax
  +0242:  mov rax,7F154C89AE78h
  +024c:  mov [rsp+30h],rax
  +0251:  mov rax,1
  +025b:  mov [rsp+38h],rax
  +0260:  mov rax,rsp
  +0263:  add rax,88h
  +026a:  mov [rsp+10h],rax
  +026f:  mov rax,[rsp+98h]
  +0277:  mov [rsp+20h],rax
  +027c:  mov rdi,rsp
  +027f:  mov r11,7F154C075000h
  +0289:  call r11
  +028c:  lea rsp,[rsp+40h]
  +0294:  mov [rsp+8],rax
  +0299:  jae near 00007F154C0732BAh
  +029f:  mov edi,0
  +02a4:  mov esi,0
  +02a9:  mov edx,0
  +02ae:  mov rax,7F156D3339D0h
  +02b8:  call rax
  +02ba:  mov r9,[rsp+28h]
  +02bf:  mov r10,r9
  +02c2:  sub r10,10h
  +02c9:  jo near 00007F154C0739A8h
  +02cf:  mov [rsp+18h],r10
  +02d4:  mov r9,[rsp+8]
  +02d9:  test r9b,7
  +02dd:  jne near 00007F154C07367Dh
  +02e3:  mov r10,r9
  +02e6:  mov [rsp+30h],r10
  +02eb:  mov rax,[rsp+18h]
  +02f0:  mov [rsp+48h],rax
  +02f5:  mov rdi,[rsp+58h]
  +02fa:  cmp rdi,0
  +0301:  je near 00007F154C073414h
  +0307:  cmp r13,[rdi]
  +030b:  jne near 00007F154C073414h
  +0311:  mov r11,7F156D8E60E8h
  +031b:  cmp byte [r11],0
  +0320:  jne near 00007F154C073414h
  +0326:  mov r11,7F156D8EA298h
  +0330:  cmp byte [r11],0
  +0335:  jne near 00007F154C073414h
  +033b:  mov r11,7F156D8E8D40h
  +0345:  cmp qword [r11],0
  +034a:  jne near 00007F154C073414h
  +0350:  mov r11,7F156D8E8BF0h
  +035a:  cmp qword [r11],0
  +035f:  jne near 00007F154C073414h
  +0365:  mov r11,7F156D8E9970h
  +036f:  mov rax,[r11]
  +0372:  cmp rax,[rdi+30h]
  +0376:  jne near 00007F154C073414h
  +037c:  mov rax,[r13+10h]
  +0380:  cmp rax,[rdi+28h]
  +0384:  jne near 00007F154C073414h
  +038a:  cmp qword [r13+18h],0
  +038f:  jne near 00007F154C073414h
  +0395:  cmp byte [rdi+40h],0
  +0399:  jne near 00007F154C073414h
  +039f:  mov r11,[rdi+8]
  +03a3:  mov eax,[r11]
  +03a6:  mov edx,[rdi+38h]
  +03a9:  cmp rax,rdx
  +03ac:  jge near 00007F154C073414h
  +03b2:  mov rdx,0F0h
  +03bc:  mov rax,rsp
  +03bf:  sub rax,8
  +03c6:  jb near 00007F154C073414h
  +03cc:  sub rax,rdx
  +03cf:  jb near 00007F154C073414h
  +03d5:  cmp rax,[rdi+58h]
  +03d9:  jbe near 00007F154C073414h
  +03df:  mov rdx,[rdi+8]
  +03e3:  inc dword [rdx]
  +03e5:  mov rdi,[rsp+48h]
  +03ea:  mov rcx,1
  +03f4:  mov rax,7
  +03fe:  call 00007F154C07303Eh
  +0403:  mov r11,[rsp+58h]
  +0408:  mov r11,[r11+8]
  +040c:  dec dword [r11]
  +040f:  jmp 00007F154C0734A8h
  +0414:  sub rsp,40h
  +041b:  mov rax,8E7h
  +0425:  mov [rsp],rax
  +0429:  mov rax,1
  +0433:  mov [rsp+8],rax
  +0438:  mov rax,7
  +0442:  mov [rsp+18h],rax
  +0447:  mov rax,7F154C89AE38h
  +0451:  mov [rsp+28h],rax
  +0456:  mov rax,7F154C89AE78h
  +0460:  mov [rsp+30h],rax
  +0465:  mov rax,1
  +046f:  mov [rsp+38h],rax
  +0474:  mov rax,rsp
  +0477:  add rax,88h
  +047e:  mov [rsp+10h],rax
  +0483:  mov rax,[rsp+98h]
  +048b:  mov [rsp+20h],rax
  +0490:  mov rdi,rsp
  +0493:  mov r11,7F154C075000h
  +049d:  call r11
  +04a0:  lea rsp,[rsp+40h]
  +04a8:  mov rbx,rax
  +04ab:  jae near 00007F154C0734CCh
  +04b1:  mov edi,0
  +04b6:  mov esi,0
  +04bb:  mov edx,0
  +04c0:  mov rax,7F156D3339D0h
  +04ca:  call rax
  +04cc:  test bl,7
  +04cf:  jne near 00007F154C0738E4h
  +04d5:  mov r9,rbx
  +04d8:  mov [rsp+40h],r9
  +04dd:  mov r9,[rsp+30h]
  +04e2:  mov r10,[rsp+40h]
  +04e7:  mov r11,r9
  +04ea:  add r11,r10
  +04ed:  jo near 00007F154C07350Ch
  +04f3:  mov [rsp+20h],r11
  +04f8:  mov rsi,[rsp+20h]
  +04fd:  mov rax,rsi
  +0500:  lea rsp,[rsp+78h]
  +0508:  pop rbx
  +0509:  pop rbp
  +050a:  clc
  +050b:  ret
  +050c:  sub rsp,60h
  +0513:  test byte [rsp],0
  +0517:  mov rax,8E7h
  +0521:  mov [rsp],rax
  +0525:  mov rax,0Eh
  +052f:  mov [rsp+8],rax
  +0534:  mov rax,1
  +053e:  mov [rsp+10h],rax
  +0543:  mov rax,2
  +054d:  mov [rsp+18h],rax
  +0552:  mov rax,17h
  +055c:  mov [rsp+20h],rax
  +0561:  mov rax,[rsp+90h]
  +0569:  mov [rsp+28h],rax
  +056e:  mov rax,[rsp+0A0h]
  +0576:  mov [rsp+30h],rax
  +057b:  mov rax,1
  +0585:  mov [rsp+38h],rax
  +058a:  mov rax,7
  +0594:  mov [rsp+40h],rax
  +0599:  mov rax,rsp
  +059c:  mov [rsp+48h],rax
  +05a1:  mov rax,[rsp+0B8h]
  +05a9:  mov [rsp+50h],rax
  +05ae:  lea rdi,[rsp+38h]
  +05b6:  mov rax,7F154C060000h    ; native precise deopt veneer
  +05c0:  call rax
  +05c2:  lea rsp,[rsp+60h]
  +05ca:  lea rsp,[rsp+78h]
  +05d2:  pop rbx
  +05d3:  pop rbp
  +05d4:  ret
  +05d5:  sub rsp,50h
  +05dc:  test byte [rsp],0
  +05e0:  mov rax,8E7h
  +05ea:  mov [rsp],rax
  +05ee:  mov rax,0
  +05f8:  mov [rsp+8],rax
  +05fd:  mov rax,1
  +0607:  mov [rsp+10h],rax
  +060c:  mov rax,0
  +0616:  mov [rsp+18h],rax
  +061b:  mov rax,rsi
  +061e:  mov [rsp+20h],rax
  +0623:  mov rax,1
  +062d:  mov [rsp+28h],rax
  +0632:  mov rax,5
  +063c:  mov [rsp+30h],rax
  +0641:  mov rax,rsp
  +0644:  mov [rsp+38h],rax
  +0649:  mov rax,[rsp+0A8h]
  +0651:  mov [rsp+40h],rax
  +0656:  lea rdi,[rsp+28h]
  +065e:  mov rax,7F154C060000h    ; native precise deopt veneer
  +0668:  call rax
  +066a:  lea rsp,[rsp+50h]
  +0672:  lea rsp,[rsp+78h]
  +067a:  pop rbx
  +067b:  pop rbp
  +067c:  ret
  +067d:  sub rsp,60h
  +0684:  test byte [rsp],0
  +0688:  mov rax,8E7h
  +0692:  mov [rsp],rax
  +0696:  mov rax,0Ch
  +06a0:  mov [rsp+8],rax
  +06a5:  mov rax,1
  +06af:  mov [rsp+10h],rax
  +06b4:  mov rax,3
  +06be:  mov [rsp+18h],rax
  +06c3:  mov rax,17h
  +06cd:  mov [rsp+20h],rax
  +06d2:  mov rax,[rsp+68h]
  +06d7:  mov [rsp+28h],rax
  +06dc:  mov rax,[rsp+88h]
  +06e4:  mov [rsp+30h],rax
  +06e9:  mov rax,10h
  +06f3:  mov [rsp+38h],rax
  +06f8:  mov rax,1
  +0702:  mov [rsp+40h],rax
  +0707:  mov rax,8
  +0711:  mov [rsp+48h],rax
  +0716:  mov rax,rsp
  +0719:  mov [rsp+50h],rax
  +071e:  mov rax,[rsp+0B8h]
  +0726:  mov [rsp+58h],rax
  +072b:  lea rdi,[rsp+40h]
  +0733:  mov rax,7F154C060000h    ; native precise deopt veneer
  +073d:  call rax
  +073f:  lea rsp,[rsp+60h]
  +0747:  lea rsp,[rsp+78h]
  +074f:  pop rbx
  +0750:  pop rbp
  +0751:  ret
  +0752:  sub rsp,60h
  +0759:  test byte [rsp],0
  +075d:  mov rax,8E7h
  +0767:  mov [rsp],rax
  +076b:  mov rax,8
  +0775:  mov [rsp+8],rax
  +077a:  mov rax,1
  +0784:  mov [rsp+10h],rax
  +0789:  mov rax,2
  +0793:  mov [rsp+18h],rax
  +0798:  mov rax,[rsp+88h]
  +07a0:  mov [rsp+20h],rax
  +07a5:  mov rax,[rsp+88h]
  +07ad:  mov [rsp+28h],rax
  +07b2:  mov rax,8
  +07bc:  mov [rsp+30h],rax
  +07c1:  mov rax,1
  +07cb:  mov [rsp+38h],rax
  +07d0:  mov rax,7
  +07da:  mov [rsp+40h],rax
  +07df:  mov rax,rsp
  +07e2:  mov [rsp+48h],rax
  +07e7:  mov rax,[rsp+0B8h]
  +07ef:  mov [rsp+50h],rax
  +07f4:  lea rdi,[rsp+38h]
  +07fc:  mov rax,7F154C060000h    ; native precise deopt veneer
  +0806:  call rax
  +0808:  lea rsp,[rsp+60h]
  +0810:  lea rsp,[rsp+78h]
  +0818:  pop rbx
  +0819:  pop rbp
  +081a:  ret
  +081b:  sub rsp,60h
  +0822:  test byte [rsp],0
  +0826:  mov rax,8E7h
  +0830:  mov [rsp],rax
  +0834:  mov rax,2
  +083e:  mov [rsp+8],rax
  +0843:  mov rax,1
  +084d:  mov [rsp+10h],rax
  +0852:  mov rax,2
  +085c:  mov [rsp+18h],rax
  +0861:  mov rax,[rsp+88h]
  +0869:  mov [rsp+20h],rax
  +086e:  mov rax,[rsp+88h]
  +0876:  mov [rsp+28h],rax
  +087b:  mov rax,10h
  +0885:  mov [rsp+30h],rax
  +088a:  mov rax,1
  +0894:  mov [rsp+38h],rax
  +0899:  mov rax,7
  +08a3:  mov [rsp+40h],rax
  +08a8:  mov rax,rsp
  +08ab:  mov [rsp+48h],rax
  +08b0:  mov rax,[rsp+0B8h]
  +08b8:  mov [rsp+50h],rax
  +08bd:  lea rdi,[rsp+38h]
  +08c5:  mov rax,7F154C060000h    ; native precise deopt veneer
  +08cf:  call rax
  +08d1:  lea rsp,[rsp+60h]
  +08d9:  lea rsp,[rsp+78h]
  +08e1:  pop rbx
  +08e2:  pop rbp
  +08e3:  ret
  +08e4:  sub rsp,60h
  +08eb:  test byte [rsp],0
  +08ef:  mov rax,8E7h
  +08f9:  mov [rsp],rax
  +08fd:  mov rax,0Eh
  +0907:  mov [rsp+8],rax
  +090c:  mov rax,1
  +0916:  mov [rsp+10h],rax
  +091b:  mov rax,2
  +0925:  mov [rsp+18h],rax
  +092a:  mov rax,17h
  +0934:  mov [rsp+20h],rax
  +0939:  mov rax,[rsp+90h]
  +0941:  mov [rsp+28h],rax
  +0946:  mov rax,rbx
  +0949:  mov [rsp+30h],rax
  +094e:  mov rax,1
  +0958:  mov [rsp+38h],rax
  +095d:  mov rax,7
  +0967:  mov [rsp+40h],rax
  +096c:  mov rax,rsp
  +096f:  mov [rsp+48h],rax
  +0974:  mov rax,[rsp+0B8h]
  +097c:  mov [rsp+50h],rax
  +0981:  lea rdi,[rsp+38h]
  +0989:  mov rax,7F154C060000h    ; native precise deopt veneer
  +0993:  call rax
  +0995:  lea rsp,[rsp+60h]
  +099d:  lea rsp,[rsp+78h]
  +09a5:  pop rbx
  +09a6:  pop rbp
  +09a7:  ret
  +09a8:  sub rsp,60h
  +09af:  test byte [rsp],0
  +09b3:  mov rax,8E7h
  +09bd:  mov [rsp],rax
  +09c1:  mov rax,0Ch
  +09cb:  mov [rsp+8],rax
  +09d0:  mov rax,1
  +09da:  mov [rsp+10h],rax
  +09df:  mov rax,3
  +09e9:  mov [rsp+18h],rax
  +09ee:  mov rax,17h
  +09f8:  mov [rsp+20h],rax
  +09fd:  mov rax,[rsp+68h]
  +0a02:  mov [rsp+28h],rax
  +0a07:  mov rax,[rsp+88h]
  +0a0f:  mov [rsp+30h],rax
  +0a14:  mov rax,10h
  +0a1e:  mov [rsp+38h],rax
  +0a23:  mov rax,1
  +0a2d:  mov [rsp+40h],rax
  +0a32:  mov rax,8
  +0a3c:  mov [rsp+48h],rax
  +0a41:  mov rax,rsp
  +0a44:  mov [rsp+50h],rax
  +0a49:  mov rax,[rsp+0B8h]
  +0a51:  mov [rsp+58h],rax
  +0a56:  lea rdi,[rsp+40h]
  +0a5e:  mov rax,7F154C060000h    ; native precise deopt veneer
  +0a68:  call rax
  +0a6a:  lea rsp,[rsp+60h]
  +0a72:  lea rsp,[rsp+78h]
  +0a7a:  pop rbx
  +0a7b:  pop rbp
  +0a7c:  ret
