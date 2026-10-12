FIB-CODE-BASE 139940884844544
; FIB — 1 arg(s), 1 local(s), 3 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; 3534 bytes of x86-64:
; Ordinary x86-64 Lisp ABI — installed native code.
; 3534 bytes of x86-64:
  +0000:  ud2
  +0002:  push rbp
  +0003:  mov rbp,rsp
  +0006:  push rbx
  +0007:  sub rsp,8
  +000b:  sub rsp,90h
  +0012:  test byte [rsp],0
  +0016:  mov [rsp+70h],rdi
  +001b:  mov rsi,[rdi]
  +001f:  jmp 00007F4686BB1078h
  +0024:  cmp rcx,1
  +002b:  je near 00007F4686BB103Eh
  +0031:  mov r11,7F4686C33000h
  +003b:  jmp r11
  +003e:  push rbp
  +003f:  mov rbp,rsp
  +0042:  push rbx
  +0043:  sub rsp,8
  +0047:  sub rsp,90h
  +004e:  test byte [rsp],0
  +0052:  mov rsi,rdi
  +0055:  mov rax,0
  +005f:  mov [rsp+70h],rax
  +0064:  mov rax,[r13+20h]
  +0068:  mov [rsp+78h],rax
  +006d:  mov rax,rsi
  +0070:  mov [rsp+80h],rax
  +0078:  test sil,7
  +007c:  jne near 00007F4686BB1857h
  +0082:  mov r9,rsi
  +0085:  mov [rsp+48h],r9
  +008a:  mov r9,[rsp+48h]
  +008f:  cmp r9,10h
  +0096:  jge near 00007F4686BB10A6h
  +009c:  mov rsi,[rsp+48h]
  +00a1:  jmp 00007F4686BB1848h
  +00a6:  mov r9,[rsp+48h]
  +00ab:  mov r10,r9
  +00ae:  sub r10,8
  +00b5:  jo near 00007F4686BB19D7h
  +00bb:  mov [rsp+20h],r10
  +00c0:  mov rax,[rsp+20h]
  +00c5:  mov [rsp+68h],rax
  +00ca:  mov rdi,[rsp+78h]
  +00cf:  cmp rdi,0
  +00d6:  je near 00007F4686BB13A0h
  +00dc:  mov rax,[rdi]
  +00df:  cmp rax,r13
  +00e2:  jne near 00007F4686BB13A0h
  +00e8:  mov r11,7F46887B50E8h
  +00f2:  movzx eax,byte [r11]
  +00f7:  cmp rax,0
  +00fe:  jne near 00007F4686BB13A0h
  +0104:  mov r11,7F46887B9298h
  +010e:  movzx eax,byte [r11]
  +0113:  cmp rax,0
  +011a:  jne near 00007F4686BB13A0h
  +0120:  mov r11,7F46887B7D40h
  +012a:  mov rax,[r11]
  +012d:  cmp rax,0
  +0134:  jne near 00007F4686BB13A0h
  +013a:  mov r11,7F46887B7BF0h
  +0144:  mov rax,[r11]
  +0147:  cmp rax,0
  +014e:  jne near 00007F4686BB13A0h
  +0154:  mov r11,7F46887B8970h
  +015e:  mov rax,[r11]
  +0161:  cmp rax,[rdi+30h]
  +0165:  jne near 00007F4686BB13A0h
  +016b:  mov rax,[r13+10h]
  +016f:  cmp rax,[rdi+28h]
  +0173:  jne near 00007F4686BB13A0h
  +0179:  mov rax,[r13+18h]
  +017d:  cmp rax,0
  +0184:  jne near 00007F4686BB13A0h
  +018a:  mov r11,rdi
  +018d:  add r11,40h
  +0194:  movzx eax,byte [r11]
  +0199:  cmp rax,0
  +01a0:  jne near 00007F4686BB13A0h
  +01a6:  mov r11,[rdi+18h]
  +01aa:  movzx eax,byte [r11]
  +01af:  cmp rax,0
  +01b6:  jne near 00007F4686BB13A0h
  +01bc:  mov r11,[rdi+8]
  +01c0:  mov eax,[r11]
  +01c3:  mov edx,[rdi+38h]
  +01c6:  cmp rax,rdx
  +01c9:  jge near 00007F4686BB13A0h
  +01cf:  mov r11,7F468752A738h
  +01d9:  mov r11,[r11]
  +01dc:  cmp r11,0
  +01e3:  je near 00007F4686BB13A0h
  +01e9:  mov rax,[r11]
  +01ec:  mov rdx,5836344C495350h
  +01f6:  cmp rax,rdx
  +01f9:  jne near 00007F4686BB13A0h
  +01ff:  mov rax,[r11+8]
  +0203:  mov rdx,1
  +020d:  cmp rax,rdx
  +0210:  jne near 00007F4686BB13A0h
  +0216:  mov rax,[r11+30h]
  +021a:  mov rdx,1
  +0224:  cmp rax,rdx
  +0227:  je near 00007F4686BB123Ah
  +022d:  cmp rax,0FFFFFFFFFFFFFFFFh
  +0234:  jne near 00007F4686BB13A0h
  +023a:  mov rax,[r11+38h]
  +023e:  mov rdx,0
  +0248:  cmp rax,rdx
  +024b:  jne near 00007F4686BB13A0h
  +0251:  mov rdx,7F468752A778h
  +025b:  mov rax,[rdx]
  +025e:  cmp rax,[r11+10h]
  +0262:  jne near 00007F4686BB13A0h
  +0268:  mov rax,[r11+40h]
  +026c:  cmp rax,0
  +0273:  je near 00007F4686BB13A0h
  +0279:  mov rax,[r11+20h]
  +027d:  cmp rax,0
  +0284:  je near 00007F4686BB13A0h
  +028a:  mov rdx,[r11+28h]
  +028e:  add rax,rdx
  +0291:  jb near 00007F4686BB13A0h
  +0297:  mov rdx,[r11+18h]
  +029b:  cmp rax,rdx
  +029e:  jbe near 00007F4686BB13A0h
  +02a4:  mov rax,[r11+20h]
  +02a8:  cmp rdx,rax
  +02ab:  jb near 00007F4686BB13A0h
  +02b1:  mov rdx,[r11+48h]
  +02b5:  cmp rdx,8
  +02bc:  jl near 00007F4686BB13A0h
  +02c2:  mov rax,rsp
  +02c5:  sub rax,8
  +02cc:  jb near 00007F4686BB13A0h
  +02d2:  sub rax,rdx
  +02d5:  jb near 00007F4686BB13A0h
  +02db:  mov rdx,[rdi+58h]
  +02df:  cmp rax,rdx
  +02e2:  jbe near 00007F4686BB13A0h
  +02e8:  mov r10,7F468752A740h
  +02f2:  mov r10,[r10]
  +02f5:  cmp r10,0
  +02fc:  je near 00007F4686BB1369h
  +0302:  mov rax,[r10]
  +0305:  cmp rax,r11
  +0308:  jne near 00007F4686BB1369h
  +030e:  mov edx,[r10+18h]
  +0312:  cmp rdx,0
  +0319:  je near 00007F4686BB1335h
  +031f:  sub rdx,1
  +0326:  mov rax,[r10+8]
  +032a:  mov eax,[rax]
  +032c:  cmp rdx,rax
  +032f:  jbe near 00007F4686BB13A0h
  +0335:  mov edx,[r10+1Ch]
  +0339:  cmp rdx,0
  +0340:  je near 00007F4686BB1355h
  +0346:  mov rax,[r10+10h]
  +034a:  mov eax,[rax]
  +034c:  cmp rdx,rax
  +034f:  jbe near 00007F4686BB13A0h
  +0355:  mov rax,[r10+8]
  +0359:  cmp rax,0
  +0360:  je near 00007F4686BB1369h
  +0366:  lock inc dword [rax]
  +0369:  mov rdx,[rdi+8]
  +036d:  inc dword [rdx]
  +036f:  mov r11,[r11+18h]
  +0373:  mov rdi,[rsp+68h]
  +0378:  mov rcx,1
  +0382:  mov rax,7
  +038c:  call r11
  +038f:  mov r11,[rsp+78h]
  +0394:  mov r11,[r11+8]
  +0398:  dec dword [r11]
  +039b:  jmp 00007F4686BB1434h
  +03a0:  sub rsp,40h
  +03a7:  mov rax,267Eh
  +03b1:  mov [rsp],rax
  +03b5:  mov rax,1
  +03bf:  mov [rsp+8],rax
  +03c4:  mov rax,7
  +03ce:  mov [rsp+18h],rax
  +03d3:  mov rax,7F468752A738h
  +03dd:  mov [rsp+28h],rax
  +03e2:  mov rax,7F468752A778h
  +03ec:  mov [rsp+30h],rax
  +03f1:  mov rax,0
  +03fb:  mov [rsp+38h],rax
  +0400:  mov rax,rsp
  +0403:  add rax,0A8h
  +040a:  mov [rsp+10h],rax
  +040f:  mov rax,[rsp+0B8h]
  +0417:  mov [rsp+20h],rax
  +041c:  mov rdi,rsp
  +041f:  mov r11,7F4686BB3000h
  +0429:  call r11
  +042c:  lea rsp,[rsp+40h]
  +0434:  mov [rsp+28h],rax
  +0439:  mov r9,[rsp+48h]
  +043e:  mov r10,r9
  +0441:  sub r10,10h
  +0448:  jo near 00007F4686BB18FFh
  +044e:  mov [rsp+38h],r10
  +0453:  mov r9,[rsp+28h]
  +0458:  test r9b,7
  +045c:  jne near 00007F4686BB1C2Dh
  +0462:  mov r10,r9
  +0465:  mov [rsp+50h],r10
  +046a:  mov edi,0
  +046f:  mov esi,0
  +0474:  mov edx,0
  +0479:  mov rax,7F46881C7DE0h
  +0483:  call rax
  +0485:  mov rax,[rsp+38h]
  +048a:  mov [rsp+68h],rax
  +048f:  mov rdi,[rsp+78h]
  +0494:  cmp rdi,0
  +049b:  je near 00007F4686BB1765h
  +04a1:  mov rax,[rdi]
  +04a4:  cmp rax,r13
  +04a7:  jne near 00007F4686BB1765h
  +04ad:  mov r11,7F46887B50E8h
  +04b7:  movzx eax,byte [r11]
  +04bc:  cmp rax,0
  +04c3:  jne near 00007F4686BB1765h
  +04c9:  mov r11,7F46887B9298h
  +04d3:  movzx eax,byte [r11]
  +04d8:  cmp rax,0
  +04df:  jne near 00007F4686BB1765h
  +04e5:  mov r11,7F46887B7D40h
  +04ef:  mov rax,[r11]
  +04f2:  cmp rax,0
  +04f9:  jne near 00007F4686BB1765h
  +04ff:  mov r11,7F46887B7BF0h
  +0509:  mov rax,[r11]
  +050c:  cmp rax,0
  +0513:  jne near 00007F4686BB1765h
  +0519:  mov r11,7F46887B8970h
  +0523:  mov rax,[r11]
  +0526:  cmp rax,[rdi+30h]
  +052a:  jne near 00007F4686BB1765h
  +0530:  mov rax,[r13+10h]
  +0534:  cmp rax,[rdi+28h]
  +0538:  jne near 00007F4686BB1765h
  +053e:  mov rax,[r13+18h]
  +0542:  cmp rax,0
  +0549:  jne near 00007F4686BB1765h
  +054f:  mov r11,rdi
  +0552:  add r11,40h
  +0559:  movzx eax,byte [r11]
  +055e:  cmp rax,0
  +0565:  jne near 00007F4686BB1765h
  +056b:  mov r11,[rdi+18h]
  +056f:  movzx eax,byte [r11]
  +0574:  cmp rax,0
  +057b:  jne near 00007F4686BB1765h
  +0581:  mov r11,[rdi+8]
  +0585:  mov eax,[r11]
  +0588:  mov edx,[rdi+38h]
  +058b:  cmp rax,rdx
  +058e:  jge near 00007F4686BB1765h
  +0594:  mov r11,7F468752A738h
  +059e:  mov r11,[r11]
  +05a1:  cmp r11,0
  +05a8:  je near 00007F4686BB1765h
  +05ae:  mov rax,[r11]
  +05b1:  mov rdx,5836344C495350h
  +05bb:  cmp rax,rdx
  +05be:  jne near 00007F4686BB1765h
  +05c4:  mov rax,[r11+8]
  +05c8:  mov rdx,1
  +05d2:  cmp rax,rdx
  +05d5:  jne near 00007F4686BB1765h
  +05db:  mov rax,[r11+30h]
  +05df:  mov rdx,1
  +05e9:  cmp rax,rdx
  +05ec:  je near 00007F4686BB15FFh
  +05f2:  cmp rax,0FFFFFFFFFFFFFFFFh
  +05f9:  jne near 00007F4686BB1765h
  +05ff:  mov rax,[r11+38h]
  +0603:  mov rdx,0
  +060d:  cmp rax,rdx
  +0610:  jne near 00007F4686BB1765h
  +0616:  mov rdx,7F468752A778h
  +0620:  mov rax,[rdx]
  +0623:  cmp rax,[r11+10h]
  +0627:  jne near 00007F4686BB1765h
  +062d:  mov rax,[r11+40h]
  +0631:  cmp rax,0
  +0638:  je near 00007F4686BB1765h
  +063e:  mov rax,[r11+20h]
  +0642:  cmp rax,0
  +0649:  je near 00007F4686BB1765h
  +064f:  mov rdx,[r11+28h]
  +0653:  add rax,rdx
  +0656:  jb near 00007F4686BB1765h
  +065c:  mov rdx,[r11+18h]
  +0660:  cmp rax,rdx
  +0663:  jbe near 00007F4686BB1765h
  +0669:  mov rax,[r11+20h]
  +066d:  cmp rdx,rax
  +0670:  jb near 00007F4686BB1765h
  +0676:  mov rdx,[r11+48h]
  +067a:  cmp rdx,8
  +0681:  jl near 00007F4686BB1765h
  +0687:  mov rax,rsp
  +068a:  sub rax,8
  +0691:  jb near 00007F4686BB1765h
  +0697:  sub rax,rdx
  +069a:  jb near 00007F4686BB1765h
  +06a0:  mov rdx,[rdi+58h]
  +06a4:  cmp rax,rdx
  +06a7:  jbe near 00007F4686BB1765h
  +06ad:  mov r10,7F468752A740h
  +06b7:  mov r10,[r10]
  +06ba:  cmp r10,0
  +06c1:  je near 00007F4686BB172Eh
  +06c7:  mov rax,[r10]
  +06ca:  cmp rax,r11
  +06cd:  jne near 00007F4686BB172Eh
  +06d3:  mov edx,[r10+18h]
  +06d7:  cmp rdx,0
  +06de:  je near 00007F4686BB16FAh
  +06e4:  sub rdx,1
  +06eb:  mov rax,[r10+8]
  +06ef:  mov eax,[rax]
  +06f1:  cmp rdx,rax
  +06f4:  jbe near 00007F4686BB1765h
  +06fa:  mov edx,[r10+1Ch]
  +06fe:  cmp rdx,0
  +0705:  je near 00007F4686BB171Ah
  +070b:  mov rax,[r10+10h]
  +070f:  mov eax,[rax]
  +0711:  cmp rdx,rax
  +0714:  jbe near 00007F4686BB1765h
  +071a:  mov rax,[r10+8]
  +071e:  cmp rax,0
  +0725:  je near 00007F4686BB172Eh
  +072b:  lock inc dword [rax]
  +072e:  mov rdx,[rdi+8]
  +0732:  inc dword [rdx]
  +0734:  mov r11,[r11+18h]
  +0738:  mov rdi,[rsp+68h]
  +073d:  mov rcx,1
  +0747:  mov rax,7
  +0751:  call r11
  +0754:  mov r11,[rsp+78h]
  +0759:  mov r11,[r11+8]
  +075d:  dec dword [r11]
  +0760:  jmp 00007F4686BB17F9h
  +0765:  sub rsp,40h
  +076c:  mov rax,267Eh
  +0776:  mov [rsp],rax
  +077a:  mov rax,1
  +0784:  mov [rsp+8],rax
  +0789:  mov rax,7
  +0793:  mov [rsp+18h],rax
  +0798:  mov rax,7F468752A738h
  +07a2:  mov [rsp+28h],rax
  +07a7:  mov rax,7F468752A778h
  +07b1:  mov [rsp+30h],rax
  +07b6:  mov rax,0
  +07c0:  mov [rsp+38h],rax
  +07c5:  mov rax,rsp
  +07c8:  add rax,0A8h
  +07cf:  mov [rsp+10h],rax
  +07d4:  mov rax,[rsp+0B8h]
  +07dc:  mov [rsp+20h],rax
  +07e1:  mov rdi,rsp
  +07e4:  mov r11,7F4686BB3000h
  +07ee:  call r11
  +07f1:  lea rsp,[rsp+40h]
  +07f9:  mov rbx,rax
  +07fc:  test bl,7
  +07ff:  jne near 00007F4686BB1AA0h
  +0805:  mov r9,rbx
  +0808:  mov [rsp+60h],r9
  +080d:  mov r9,[rsp+50h]
  +0812:  mov r10,[rsp+60h]
  +0817:  mov r11,r9
  +081a:  add r11,r10
  +081d:  jo near 00007F4686BB1B64h
  +0823:  mov [rsp+40h],r11
  +0828:  mov edi,0
  +082d:  mov esi,0
  +0832:  mov edx,0
  +0837:  mov rax,7F46881C7DE0h
  +0841:  call rax
  +0843:  mov rsi,[rsp+40h]
  +0848:  mov rax,rsi
  +084b:  lea rsp,[rsp+98h]
  +0853:  pop rbx
  +0854:  pop rbp
  +0855:  clc
  +0856:  ret
  +0857:  sub rsp,50h
  +085e:  test byte [rsp],0
  +0862:  mov rax,267Eh
  +086c:  mov [rsp],rax
  +0870:  mov rax,0
  +087a:  mov [rsp+8],rax
  +087f:  mov rax,1
  +0889:  mov [rsp+10h],rax
  +088e:  mov rax,0
  +0898:  mov [rsp+18h],rax
  +089d:  mov rax,rsi
  +08a0:  mov [rsp+20h],rax
  +08a5:  mov rax,1
  +08af:  mov [rsp+28h],rax
  +08b4:  mov rax,5
  +08be:  mov [rsp+30h],rax
  +08c3:  mov rax,rsp
  +08c6:  mov [rsp+38h],rax
  +08cb:  mov rax,[rsp+0C8h]
  +08d3:  mov [rsp+40h],rax
  +08d8:  lea rdi,[rsp+28h]
  +08e0:  mov rax,7F4686BAE000h    ; native precise deopt veneer
  +08ea:  call rax
  +08ec:  lea rsp,[rsp+50h]
  +08f4:  lea rsp,[rsp+98h]
  +08fc:  pop rbx
  +08fd:  pop rbp
  +08fe:  ret
  +08ff:  sub rsp,60h
  +0906:  test byte [rsp],0
  +090a:  mov rax,267Eh
  +0914:  mov [rsp],rax
  +0918:  mov rax,0Ch
  +0922:  mov [rsp+8],rax
  +0927:  mov rax,1
  +0931:  mov [rsp+10h],rax
  +0936:  mov rax,3
  +0940:  mov [rsp+18h],rax
  +0945:  mov rax,17h
  +094f:  mov [rsp+20h],rax
  +0954:  mov rax,[rsp+88h]
  +095c:  mov [rsp+28h],rax
  +0961:  mov rax,[rsp+0A8h]
  +0969:  mov [rsp+30h],rax
  +096e:  mov rax,10h
  +0978:  mov [rsp+38h],rax
  +097d:  mov rax,1
  +0987:  mov [rsp+40h],rax
  +098c:  mov rax,8
  +0996:  mov [rsp+48h],rax
  +099b:  mov rax,rsp
  +099e:  mov [rsp+50h],rax
  +09a3:  mov rax,[rsp+0D8h]
  +09ab:  mov [rsp+58h],rax
  +09b0:  lea rdi,[rsp+40h]
  +09b8:  mov rax,7F4686BAE000h    ; native precise deopt veneer
  +09c2:  call rax
  +09c4:  lea rsp,[rsp+60h]
  +09cc:  lea rsp,[rsp+98h]
  +09d4:  pop rbx
  +09d5:  pop rbp
  +09d6:  ret
  +09d7:  sub rsp,60h
  +09de:  test byte [rsp],0
  +09e2:  mov rax,267Eh
  +09ec:  mov [rsp],rax
  +09f0:  mov rax,8
  +09fa:  mov [rsp+8],rax
  +09ff:  mov rax,1
  +0a09:  mov [rsp+10h],rax
  +0a0e:  mov rax,2
  +0a18:  mov [rsp+18h],rax
  +0a1d:  mov rax,[rsp+0A8h]
  +0a25:  mov [rsp+20h],rax
  +0a2a:  mov rax,[rsp+0A8h]
  +0a32:  mov [rsp+28h],rax
  +0a37:  mov rax,8
  +0a41:  mov [rsp+30h],rax
  +0a46:  mov rax,1
  +0a50:  mov [rsp+38h],rax
  +0a55:  mov rax,7
  +0a5f:  mov [rsp+40h],rax
  +0a64:  mov rax,rsp
  +0a67:  mov [rsp+48h],rax
  +0a6c:  mov rax,[rsp+0D8h]
  +0a74:  mov [rsp+50h],rax
  +0a79:  lea rdi,[rsp+38h]
  +0a81:  mov rax,7F4686BAE000h    ; native precise deopt veneer
  +0a8b:  call rax
  +0a8d:  lea rsp,[rsp+60h]
  +0a95:  lea rsp,[rsp+98h]
  +0a9d:  pop rbx
  +0a9e:  pop rbp
  +0a9f:  ret
  +0aa0:  sub rsp,60h
  +0aa7:  test byte [rsp],0
  +0aab:  mov rax,267Eh
  +0ab5:  mov [rsp],rax
  +0ab9:  mov rax,0Eh
  +0ac3:  mov [rsp+8],rax
  +0ac8:  mov rax,1
  +0ad2:  mov [rsp+10h],rax
  +0ad7:  mov rax,2
  +0ae1:  mov [rsp+18h],rax
  +0ae6:  mov rax,17h
  +0af0:  mov [rsp+20h],rax
  +0af5:  mov rax,[rsp+0B0h]
  +0afd:  mov [rsp+28h],rax
  +0b02:  mov rax,rbx
  +0b05:  mov [rsp+30h],rax
  +0b0a:  mov rax,1
  +0b14:  mov [rsp+38h],rax
  +0b19:  mov rax,7
  +0b23:  mov [rsp+40h],rax
  +0b28:  mov rax,rsp
  +0b2b:  mov [rsp+48h],rax
  +0b30:  mov rax,[rsp+0D8h]
  +0b38:  mov [rsp+50h],rax
  +0b3d:  lea rdi,[rsp+38h]
  +0b45:  mov rax,7F4686BAE000h    ; native precise deopt veneer
  +0b4f:  call rax
  +0b51:  lea rsp,[rsp+60h]
  +0b59:  lea rsp,[rsp+98h]
  +0b61:  pop rbx
  +0b62:  pop rbp
  +0b63:  ret
  +0b64:  sub rsp,60h
  +0b6b:  test byte [rsp],0
  +0b6f:  mov rax,267Eh
  +0b79:  mov [rsp],rax
  +0b7d:  mov rax,0Eh
  +0b87:  mov [rsp+8],rax
  +0b8c:  mov rax,1
  +0b96:  mov [rsp+10h],rax
  +0b9b:  mov rax,2
  +0ba5:  mov [rsp+18h],rax
  +0baa:  mov rax,17h
  +0bb4:  mov [rsp+20h],rax
  +0bb9:  mov rax,[rsp+0B0h]
  +0bc1:  mov [rsp+28h],rax
  +0bc6:  mov rax,[rsp+0C0h]
  +0bce:  mov [rsp+30h],rax
  +0bd3:  mov rax,1
  +0bdd:  mov [rsp+38h],rax
  +0be2:  mov rax,7
  +0bec:  mov [rsp+40h],rax
  +0bf1:  mov rax,rsp
  +0bf4:  mov [rsp+48h],rax
  +0bf9:  mov rax,[rsp+0D8h]
  +0c01:  mov [rsp+50h],rax
  +0c06:  lea rdi,[rsp+38h]
  +0c0e:  mov rax,7F4686BAE000h    ; native precise deopt veneer
  +0c18:  call rax
  +0c1a:  lea rsp,[rsp+60h]
  +0c22:  lea rsp,[rsp+98h]
  +0c2a:  pop rbx
  +0c2b:  pop rbp
  +0c2c:  ret
  +0c2d:  sub rsp,60h
  +0c34:  test byte [rsp],0
  +0c38:  mov rax,267Eh
  +0c42:  mov [rsp],rax
  +0c46:  mov rax,0Ch
  +0c50:  mov [rsp+8],rax
  +0c55:  mov rax,1
  +0c5f:  mov [rsp+10h],rax
  +0c64:  mov rax,3
  +0c6e:  mov [rsp+18h],rax
  +0c73:  mov rax,17h
  +0c7d:  mov [rsp+20h],rax
  +0c82:  mov rax,[rsp+88h]
  +0c8a:  mov [rsp+28h],rax
  +0c8f:  mov rax,[rsp+0A8h]
  +0c97:  mov [rsp+30h],rax
  +0c9c:  mov rax,10h
  +0ca6:  mov [rsp+38h],rax
  +0cab:  mov rax,1
  +0cb5:  mov [rsp+40h],rax
  +0cba:  mov rax,8
  +0cc4:  mov [rsp+48h],rax
  +0cc9:  mov rax,rsp
  +0ccc:  mov [rsp+50h],rax
  +0cd1:  mov rax,[rsp+0D8h]
  +0cd9:  mov [rsp+58h],rax
  +0cde:  lea rdi,[rsp+40h]
  +0ce6:  mov rax,7F4686BAE000h    ; native precise deopt veneer
  +0cf0:  call rax
  +0cf2:  lea rsp,[rsp+60h]
  +0cfa:  lea rsp,[rsp+98h]
  +0d02:  pop rbx
  +0d03:  pop rbp
  +0d04:  ret
  +0d05:  sub rsp,60h
  +0d0c:  test byte [rsp],0
  +0d10:  mov rax,267Eh
  +0d1a:  mov [rsp],rax
  +0d1e:  mov rax,2
  +0d28:  mov [rsp+8],rax
  +0d2d:  mov rax,1
  +0d37:  mov [rsp+10h],rax
  +0d3c:  mov rax,2
  +0d46:  mov [rsp+18h],rax
  +0d4b:  mov rax,[rsp+0A8h]
  +0d53:  mov [rsp+20h],rax
  +0d58:  mov rax,[rsp+0A8h]
  +0d60:  mov [rsp+28h],rax
  +0d65:  mov rax,10h
  +0d6f:  mov [rsp+30h],rax
  +0d74:  mov rax,1
  +0d7e:  mov [rsp+38h],rax
  +0d83:  mov rax,7
  +0d8d:  mov [rsp+40h],rax
  +0d92:  mov rax,rsp
  +0d95:  mov [rsp+48h],rax
  +0d9a:  mov rax,[rsp+0D8h]
  +0da2:  mov [rsp+50h],rax
  +0da7:  lea rdi,[rsp+38h]
  +0daf:  mov rax,7F4686BAE000h    ; native precise deopt veneer
  +0db9:  call rax
  +0dbb:  lea rsp,[rsp+60h]
  +0dc3:  lea rsp,[rsp+98h]
  +0dcb:  pop rbx
  +0dcc:  pop rbp
  +0dcd:  ret
