; Representative listings from measured trial0.
; Loop excerpts end at+0562; later deopt/OSR stubs omitted.

; BEGIN ORDINARY-BRIDGE-LEAF
; ORDINARY-BRIDGE-LEAF — 4 arg(s), 4 local(s), 1 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; 71 bytes of x86-64:
; Mapped native transfer ABI — installed native code.
; 71 bytes of x86-64:
  +0000:  push rbx
  +0001:  sub rsp,10h
  +0008:  mov [rsp+8],rdi
  +000d:  mov rbx,[rdi]
  +0011:  mov r8,[rdi+8]
  +0015:  mov rsi,[rdi+10h]
  +0019:  mov rcx,[rdi+18h]
  +001d:  mov rax,rcx
  +0020:  add rsp,10h
  +0027:  pop rbx
  +0028:  clc
  +0029:  ret
  +002a:  add rsp,10h
  +0031:  pop rbx
  +0032:  sub rsp,8
  +0036:  mov rax,0
  +0040:  call rax
  +0042:  add rsp,8
  +0046:  ret
; END ORDINARY-BRIDGE-LEAF

; BEGIN ORDINARY-BRIDGE-FORWARD
; ORDINARY-BRIDGE-FORWARD — 4 arg(s), 4 local(s), 4 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; 310 bytes of x86-64:
; Mapped native transfer ABI — installed native code.
; 310 bytes of x86-64:
  +0000:  push rbx
  +0001:  sub rsp,30h
  +0008:  mov [rsp+20h],rdi
  +000d:  mov rax,[rdi]
  +0011:  mov [rsp],rax
  +0015:  mov rax,[rdi+8]
  +0019:  mov [rsp+8],rax
  +001e:  mov rax,[rdi+10h]
  +0022:  mov [rsp+10h],rax
  +0027:  mov rax,[rdi+18h]
  +002b:  mov [rsp+18h],rax
  +0030:  mov rdx,[rsp+20h]
  +0035:  mov rax,7
  +003f:  mov [rdx+40h],rax
  +0043:  mov [rdx+48h],rax
  +0047:  mov [rdx+50h],rax
  +004b:  mov [rdx+58h],rax
  +004f:  mov rdx,[rsp+20h]
  +0054:  mov rax,[rsp]
  +0058:  mov [rdx+40h],rax
  +005c:  mov rax,[rsp+8]
  +0061:  mov [rdx+48h],rax
  +0065:  mov rax,[rsp+10h]
  +006a:  mov [rdx+50h],rax
  +006e:  mov rax,[rsp+18h]
  +0073:  mov [rdx+58h],rax
  +0077:  sub rsp,40h
  +007e:  mov [rsp+18h],rdx
  +0083:  mov rax,8E0h
  +008d:  mov [rsp],rax
  +0091:  mov rax,4
  +009b:  mov [rsp+8],rax
  +00a0:  add rdx,40h
  +00a7:  mov [rsp+10h],rdx
  +00ac:  mov rdi,rsp
  +00af:  mov rax,7F80428D9000h
  +00b9:  call rax
  +00bb:  add rsp,40h
  +00c2:  mov rbx,rax
  +00c5:  jmp 00007F80428D50CCh
  +00ca:  ud2
  +00cc:  mov rax,rbx
  +00cf:  add rsp,30h
  +00d6:  pop rbx
  +00d7:  sub rsp,18h
  +00db:  mov [rsp+10h],rax
  +00e0:  mov rdi,rax
  +00e3:  mov rsi,rsp
  +00e6:  mov rdx,0FFFFFFFFFFFFFFFFh
  +00ed:  mov rax,7F8063D1F610h
  +00f7:  call rax
  +00f9:  mov rdx,[rsp]
  +00fd:  mov rcx,[rsp+8]
  +0102:  mov rax,[rsp+10h]
  +0107:  add rsp,18h
  +010b:  cmp rdx,1
  +010f:  je near 00007F80428D5117h
  +0115:  stc
  +0116:  ret
  +0117:  clc
  +0118:  ret
  +0119:  add rsp,30h
  +0120:  pop rbx
  +0121:  sub rsp,8
  +0125:  mov rax,0
  +012f:  call rax
  +0131:  add rsp,8
  +0135:  ret
; END ORDINARY-BRIDGE-FORWARD

; BEGIN ORDINARY-BRIDGE-DIRECT-LOOP
; ORDINARY-BRIDGE-DIRECT-LOOP — 1 arg(s), 5 local(s), 5 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; OSR loop-header entry bcps: [10]
; 3289 bytes of x86-64:
; Mapped native transfer ABI — installed native code.
; 3289 bytes of x86-64:
  +0000:  push rbx
  +0001:  sub rsp,0C0h
  +0008:  mov [rsp+0B0h],rdi
  +0010:  mov rax,7F8063E90870h
  +001a:  call rax
  +001c:  mov [rsp+0B8h],rax
  +0024:  mov rdi,[rsp+0B0h]
  +002c:  mov rcx,[rdi]
  +0030:  test cl,7
  +0033:  jne near 00007F80428D07D2h
  +0039:  mov r9,rcx
  +003c:  mov [rsp+98h],r9
  +0044:  mov rax,0
  +004e:  mov [rsp+58h],rax
  +0053:  mov rax,0
  +005d:  mov [rsp+68h],rax
  +0062:  mov rdx,7F8064561030h
  +006c:  movzx eax,byte [rdx]
  +0071:  cmp rax,0
  +0078:  jne near 00007F80428D013Dh
  +007e:  mov rdx,7F8064563310h
  +0088:  movzx eax,byte [rdx]
  +008d:  cmp rax,0
  +0094:  jne near 00007F80428D013Dh
  +009a:  mov rdx,7F8064561D20h
  +00a4:  mov rax,[rdx]
  +00a7:  cmp rax,0
  +00ae:  jne near 00007F80428D013Dh
  +00b4:  mov rdx,7F8064561C18h
  +00be:  mov rax,[rdx]
  +00c1:  cmp rax,0
  +00c8:  jne near 00007F80428D013Dh
  +00ce:  mov rdi,[rsp+0B8h]
  +00d6:  cmp rdi,0
  +00dd:  je near 00007F80428D013Dh
  +00e3:  mov rdx,7F80645632F8h
  +00ed:  mov rax,[rdx]
  +00f0:  cmp rax,[rdi+10h]
  +00f4:  jne near 00007F80428D013Dh
  +00fa:  mov rdx,[rdi]
  +00fd:  mov rax,[rdx+10h]
  +0101:  cmp rax,[rdi+8]
  +0105:  jne near 00007F80428D013Dh
  +010b:  mov rax,[rdx+18h]
  +010f:  cmp rax,0
  +0116:  jne near 00007F80428D013Dh
  +011c:  mov rdx,rdi
  +011f:  add rdx,18h
  +0126:  movzx eax,byte [rdx]
  +012b:  cmp rax,0
  +0132:  jne near 00007F80428D013Dh
  +0138:  jmp 00007F80428D0177h
  +013d:  mov rdx,[rsp+0B0h]
  +0145:  mov rax,7
  +014f:  mov [rdx+50h],rax
  +0153:  mov [rdx+58h],rax
  +0157:  mov [rdx+60h],rax
  +015b:  mov [rdx+68h],rax
  +015f:  mov [rdx+70h],rax
  +0163:  mov rdi,[rsp+0B0h]
  +016b:  mov rax,7F80428D4000h    ; native poll veneer
  +0175:  call rax
  +0177:  mov r9,[rsp+58h]
  +017c:  mov r10,[rsp+98h]
  +0184:  cmp r9,r10
  +0187:  mov rbx,0Fh
  +0191:  jge near 00007F80428D01A1h
  +0197:  mov rbx,7
  +01a1:  mov rdx,[rsp+0B0h]
  +01a9:  mov rax,7
  +01b3:  mov [rdx+50h],rax
  +01b7:  mov [rdx+58h],rax
  +01bb:  mov [rdx+60h],rax
  +01bf:  mov [rdx+68h],rax
  +01c3:  mov [rdx+70h],rax
  +01c7:  mov edi,0
  +01cc:  mov esi,0
  +01d1:  mov edx,0
  +01d6:  mov rax,7F8063D1F610h
  +01e0:  call rax
  +01e2:  mov r11,rbx
  +01e5:  cmp r11,7
  +01ec:  je near 00007F80428D01F7h
  +01f2:  jmp 00007F80428D03C3h
  +01f7:  mov rdx,7F8064561030h
  +0201:  movzx eax,byte [rdx]
  +0206:  cmp rax,0
  +020d:  jne near 00007F80428D02D2h
  +0213:  mov rdx,7F8064563310h
  +021d:  movzx eax,byte [rdx]
  +0222:  cmp rax,0
  +0229:  jne near 00007F80428D02D2h
  +022f:  mov rdx,7F8064561D20h
  +0239:  mov rax,[rdx]
  +023c:  cmp rax,0
  +0243:  jne near 00007F80428D02D2h
  +0249:  mov rdx,7F8064561C18h
  +0253:  mov rax,[rdx]
  +0256:  cmp rax,0
  +025d:  jne near 00007F80428D02D2h
  +0263:  mov rdi,[rsp+0B8h]
  +026b:  cmp rdi,0
  +0272:  je near 00007F80428D02D2h
  +0278:  mov rdx,7F80645632F8h
  +0282:  mov rax,[rdx]
  +0285:  cmp rax,[rdi+10h]
  +0289:  jne near 00007F80428D02D2h
  +028f:  mov rdx,[rdi]
  +0292:  mov rax,[rdx+10h]
  +0296:  cmp rax,[rdi+8]
  +029a:  jne near 00007F80428D02D2h
  +02a0:  mov rax,[rdx+18h]
  +02a4:  cmp rax,0
  +02ab:  jne near 00007F80428D02D2h
  +02b1:  mov rdx,rdi
  +02b4:  add rdx,18h
  +02bb:  movzx eax,byte [rdx]
  +02c0:  cmp rax,0
  +02c7:  jne near 00007F80428D02D2h
  +02cd:  jmp 00007F80428D030Ch
  +02d2:  mov rdx,[rsp+0B0h]
  +02da:  mov rax,7
  +02e4:  mov [rdx+50h],rax
  +02e8:  mov [rdx+58h],rax
  +02ec:  mov [rdx+60h],rax
  +02f0:  mov [rdx+68h],rax
  +02f4:  mov [rdx+70h],rax
  +02f8:  mov rdi,[rsp+0B0h]
  +0300:  mov rax,7F80428D4000h    ; native poll veneer
  +030a:  call rax
  +030c:  mov rdx,[rsp+0B0h]
  +0314:  mov rax,7
  +031e:  mov [rdx+50h],rax
  +0322:  mov [rdx+58h],rax
  +0326:  mov [rdx+60h],rax
  +032a:  mov [rdx+68h],rax
  +032e:  mov [rdx+70h],rax
  +0332:  mov rdx,[rsp+0B0h]
  +033a:  mov rax,0
  +0344:  mov [rdx+58h],rax
  +0348:  mov rax,8
  +0352:  mov [rdx+60h],rax
  +0356:  mov rax,10h
  +0360:  mov [rdx+68h],rax
  +0364:  mov rax,[rsp+58h]
  +0369:  mov [rdx+70h],rax
  +036d:  sub rsp,40h
  +0374:  mov [rsp+18h],rdx
  +0379:  mov rax,8E0h
  +0383:  mov [rsp],rax
  +0387:  mov rax,4
  +0391:  mov [rsp+8],rax
  +0396:  add rdx,58h
  +039d:  mov [rsp+10h],rdx
  +03a2:  mov rdi,rsp
  +03a5:  mov rax,7F80428E5000h
  +03af:  call rax
  +03b1:  add rsp,40h
  +03b8:  mov rbx,rax
  +03bb:  mov rcx,rbx
  +03be:  jmp 00007F80428D03D4h
  +03c3:  mov rax,[rsp+68h]
  +03c8:  add rsp,0C0h
  +03cf:  pop rbx
  +03d0:  clc
  +03d1:  ret
  +03d2:  ud2
  +03d4:  mov rdx,7F8064561030h
  +03de:  movzx eax,byte [rdx]
  +03e3:  cmp rax,0
  +03ea:  jne near 00007F80428D04AFh
  +03f0:  mov rdx,7F8064563310h
  +03fa:  movzx eax,byte [rdx]
  +03ff:  cmp rax,0
  +0406:  jne near 00007F80428D04AFh
  +040c:  mov rdx,7F8064561D20h
  +0416:  mov rax,[rdx]
  +0419:  cmp rax,0
  +0420:  jne near 00007F80428D04AFh
  +0426:  mov rdx,7F8064561C18h
  +0430:  mov rax,[rdx]
  +0433:  cmp rax,0
  +043a:  jne near 00007F80428D04AFh
  +0440:  mov rdi,[rsp+0B8h]
  +0448:  cmp rdi,0
  +044f:  je near 00007F80428D04AFh
  +0455:  mov rdx,7F80645632F8h
  +045f:  mov rax,[rdx]
  +0462:  cmp rax,[rdi+10h]
  +0466:  jne near 00007F80428D04AFh
  +046c:  mov rdx,[rdi]
  +046f:  mov rax,[rdx+10h]
  +0473:  cmp rax,[rdi+8]
  +0477:  jne near 00007F80428D04AFh
  +047d:  mov rax,[rdx+18h]
  +0481:  cmp rax,0
  +0488:  jne near 00007F80428D04AFh
  +048e:  mov rdx,rdi
  +0491:  add rdx,18h
  +0498:  movzx eax,byte [rdx]
  +049d:  cmp rax,0
  +04a4:  jne near 00007F80428D04AFh
  +04aa:  jmp 00007F80428D04FFh
  +04af:  mov rdx,[rsp+0B0h]
  +04b7:  mov rax,7
  +04c1:  mov [rdx+50h],rax
  +04c5:  mov [rdx+58h],rax
  +04c9:  mov [rdx+60h],rax
  +04cd:  mov [rdx+68h],rax
  +04d1:  mov [rdx+70h],rax
  +04d5:  mov rax,rcx
  +04d8:  mov [rdx+50h],rax
  +04dc:  mov rdi,[rsp+0B0h]
  +04e4:  mov rax,7F80428D4000h    ; native poll veneer
  +04ee:  call rax
  +04f0:  mov rdx,[rsp+0B0h]
  +04f8:  mov rax,[rdx+50h]
  +04fc:  mov rcx,rax
  +04ff:  test cl,7
  +0502:  jne near 00007F80428D0A19h
  +0508:  mov rsi,rcx
  +050b:  mov r9,[rsp+68h]
  +0510:  mov rcx,r9
  +0513:  add rcx,rsi
  +0516:  jo near 00007F80428D069Ch
  +051c:  mov r9,[rsp+58h]
  +0521:  mov rsi,r9
  +0524:  add rsi,8
  +052b:  jo near 00007F80428D08F0h
  +0531:  mov rax,rsi
  +0534:  mov [rsp+58h],rax
  +0539:  mov rax,rcx
  +053c:  mov [rsp+68h],rax
  +0541:  jmp 00007F80428D0062h
  +0546:  add rsp,0C0h
  +054d:  pop rbx
  +054e:  sub rsp,8
  +0552:  mov rax,0
  +055c:  call rax
  +055e:  add rsp,8
  +0562:  ret
; END ORDINARY-BRIDGE-DIRECT-LOOP

; BEGIN ORDINARY-BRIDGE-FORWARD-LOOP
; ORDINARY-BRIDGE-FORWARD-LOOP — 1 arg(s), 5 local(s), 5 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; OSR loop-header entry bcps: [10]
; 3289 bytes of x86-64:
; Mapped native transfer ABI — installed native code.
; 3289 bytes of x86-64:
  +0000:  push rbx
  +0001:  sub rsp,0C0h
  +0008:  mov [rsp+0B0h],rdi
  +0010:  mov rax,7F8063E90870h
  +001a:  call rax
  +001c:  mov [rsp+0B8h],rax
  +0024:  mov rdi,[rsp+0B0h]
  +002c:  mov rcx,[rdi]
  +0030:  test cl,7
  +0033:  jne near 00007F80428CD8F8h
  +0039:  mov r9,rcx
  +003c:  mov [rsp+98h],r9
  +0044:  mov rax,0
  +004e:  mov [rsp+58h],rax
  +0053:  mov rax,0
  +005d:  mov [rsp+68h],rax
  +0062:  mov rdx,7F8064561030h
  +006c:  movzx eax,byte [rdx]
  +0071:  cmp rax,0
  +0078:  jne near 00007F80428CD13Dh
  +007e:  mov rdx,7F8064563310h
  +0088:  movzx eax,byte [rdx]
  +008d:  cmp rax,0
  +0094:  jne near 00007F80428CD13Dh
  +009a:  mov rdx,7F8064561D20h
  +00a4:  mov rax,[rdx]
  +00a7:  cmp rax,0
  +00ae:  jne near 00007F80428CD13Dh
  +00b4:  mov rdx,7F8064561C18h
  +00be:  mov rax,[rdx]
  +00c1:  cmp rax,0
  +00c8:  jne near 00007F80428CD13Dh
  +00ce:  mov rdi,[rsp+0B8h]
  +00d6:  cmp rdi,0
  +00dd:  je near 00007F80428CD13Dh
  +00e3:  mov rdx,7F80645632F8h
  +00ed:  mov rax,[rdx]
  +00f0:  cmp rax,[rdi+10h]
  +00f4:  jne near 00007F80428CD13Dh
  +00fa:  mov rdx,[rdi]
  +00fd:  mov rax,[rdx+10h]
  +0101:  cmp rax,[rdi+8]
  +0105:  jne near 00007F80428CD13Dh
  +010b:  mov rax,[rdx+18h]
  +010f:  cmp rax,0
  +0116:  jne near 00007F80428CD13Dh
  +011c:  mov rdx,rdi
  +011f:  add rdx,18h
  +0126:  movzx eax,byte [rdx]
  +012b:  cmp rax,0
  +0132:  jne near 00007F80428CD13Dh
  +0138:  jmp 00007F80428CD177h
  +013d:  mov rdx,[rsp+0B0h]
  +0145:  mov rax,7
  +014f:  mov [rdx+50h],rax
  +0153:  mov [rdx+58h],rax
  +0157:  mov [rdx+60h],rax
  +015b:  mov [rdx+68h],rax
  +015f:  mov [rdx+70h],rax
  +0163:  mov rdi,[rsp+0B0h]
  +016b:  mov rax,7F80428F5000h    ; native poll veneer
  +0175:  call rax
  +0177:  mov r9,[rsp+58h]
  +017c:  mov r10,[rsp+98h]
  +0184:  cmp r9,r10
  +0187:  mov rbx,0Fh
  +0191:  jge near 00007F80428CD1A1h
  +0197:  mov rbx,7
  +01a1:  mov rdx,[rsp+0B0h]
  +01a9:  mov rax,7
  +01b3:  mov [rdx+50h],rax
  +01b7:  mov [rdx+58h],rax
  +01bb:  mov [rdx+60h],rax
  +01bf:  mov [rdx+68h],rax
  +01c3:  mov [rdx+70h],rax
  +01c7:  mov edi,0
  +01cc:  mov esi,0
  +01d1:  mov edx,0
  +01d6:  mov rax,7F8063D1F610h
  +01e0:  call rax
  +01e2:  mov r11,rbx
  +01e5:  cmp r11,7
  +01ec:  je near 00007F80428CD1F7h
  +01f2:  jmp 00007F80428CD3C3h
  +01f7:  mov rdx,7F8064561030h
  +0201:  movzx eax,byte [rdx]
  +0206:  cmp rax,0
  +020d:  jne near 00007F80428CD2D2h
  +0213:  mov rdx,7F8064563310h
  +021d:  movzx eax,byte [rdx]
  +0222:  cmp rax,0
  +0229:  jne near 00007F80428CD2D2h
  +022f:  mov rdx,7F8064561D20h
  +0239:  mov rax,[rdx]
  +023c:  cmp rax,0
  +0243:  jne near 00007F80428CD2D2h
  +0249:  mov rdx,7F8064561C18h
  +0253:  mov rax,[rdx]
  +0256:  cmp rax,0
  +025d:  jne near 00007F80428CD2D2h
  +0263:  mov rdi,[rsp+0B8h]
  +026b:  cmp rdi,0
  +0272:  je near 00007F80428CD2D2h
  +0278:  mov rdx,7F80645632F8h
  +0282:  mov rax,[rdx]
  +0285:  cmp rax,[rdi+10h]
  +0289:  jne near 00007F80428CD2D2h
  +028f:  mov rdx,[rdi]
  +0292:  mov rax,[rdx+10h]
  +0296:  cmp rax,[rdi+8]
  +029a:  jne near 00007F80428CD2D2h
  +02a0:  mov rax,[rdx+18h]
  +02a4:  cmp rax,0
  +02ab:  jne near 00007F80428CD2D2h
  +02b1:  mov rdx,rdi
  +02b4:  add rdx,18h
  +02bb:  movzx eax,byte [rdx]
  +02c0:  cmp rax,0
  +02c7:  jne near 00007F80428CD2D2h
  +02cd:  jmp 00007F80428CD30Ch
  +02d2:  mov rdx,[rsp+0B0h]
  +02da:  mov rax,7
  +02e4:  mov [rdx+50h],rax
  +02e8:  mov [rdx+58h],rax
  +02ec:  mov [rdx+60h],rax
  +02f0:  mov [rdx+68h],rax
  +02f4:  mov [rdx+70h],rax
  +02f8:  mov rdi,[rsp+0B0h]
  +0300:  mov rax,7F80428F5000h    ; native poll veneer
  +030a:  call rax
  +030c:  mov rdx,[rsp+0B0h]
  +0314:  mov rax,7
  +031e:  mov [rdx+50h],rax
  +0322:  mov [rdx+58h],rax
  +0326:  mov [rdx+60h],rax
  +032a:  mov [rdx+68h],rax
  +032e:  mov [rdx+70h],rax
  +0332:  mov rdx,[rsp+0B0h]
  +033a:  mov rax,0
  +0344:  mov [rdx+58h],rax
  +0348:  mov rax,8
  +0352:  mov [rdx+60h],rax
  +0356:  mov rax,10h
  +0360:  mov [rdx+68h],rax
  +0364:  mov rax,[rsp+58h]
  +0369:  mov [rdx+70h],rax
  +036d:  sub rsp,40h
  +0374:  mov [rsp+18h],rdx
  +0379:  mov rax,8E1h
  +0383:  mov [rsp],rax
  +0387:  mov rax,4
  +0391:  mov [rsp+8],rax
  +0396:  add rdx,58h
  +039d:  mov [rsp+10h],rdx
  +03a2:  mov rdi,rsp
  +03a5:  mov rax,7F8042957000h
  +03af:  call rax
  +03b1:  add rsp,40h
  +03b8:  mov rbx,rax
  +03bb:  mov rcx,rbx
  +03be:  jmp 00007F80428CD3D4h
  +03c3:  mov rax,[rsp+68h]
  +03c8:  add rsp,0C0h
  +03cf:  pop rbx
  +03d0:  clc
  +03d1:  ret
  +03d2:  ud2
  +03d4:  mov rdx,7F8064561030h
  +03de:  movzx eax,byte [rdx]
  +03e3:  cmp rax,0
  +03ea:  jne near 00007F80428CD4AFh
  +03f0:  mov rdx,7F8064563310h
  +03fa:  movzx eax,byte [rdx]
  +03ff:  cmp rax,0
  +0406:  jne near 00007F80428CD4AFh
  +040c:  mov rdx,7F8064561D20h
  +0416:  mov rax,[rdx]
  +0419:  cmp rax,0
  +0420:  jne near 00007F80428CD4AFh
  +0426:  mov rdx,7F8064561C18h
  +0430:  mov rax,[rdx]
  +0433:  cmp rax,0
  +043a:  jne near 00007F80428CD4AFh
  +0440:  mov rdi,[rsp+0B8h]
  +0448:  cmp rdi,0
  +044f:  je near 00007F80428CD4AFh
  +0455:  mov rdx,7F80645632F8h
  +045f:  mov rax,[rdx]
  +0462:  cmp rax,[rdi+10h]
  +0466:  jne near 00007F80428CD4AFh
  +046c:  mov rdx,[rdi]
  +046f:  mov rax,[rdx+10h]
  +0473:  cmp rax,[rdi+8]
  +0477:  jne near 00007F80428CD4AFh
  +047d:  mov rax,[rdx+18h]
  +0481:  cmp rax,0
  +0488:  jne near 00007F80428CD4AFh
  +048e:  mov rdx,rdi
  +0491:  add rdx,18h
  +0498:  movzx eax,byte [rdx]
  +049d:  cmp rax,0
  +04a4:  jne near 00007F80428CD4AFh
  +04aa:  jmp 00007F80428CD4FFh
  +04af:  mov rdx,[rsp+0B0h]
  +04b7:  mov rax,7
  +04c1:  mov [rdx+50h],rax
  +04c5:  mov [rdx+58h],rax
  +04c9:  mov [rdx+60h],rax
  +04cd:  mov [rdx+68h],rax
  +04d1:  mov [rdx+70h],rax
  +04d5:  mov rax,rcx
  +04d8:  mov [rdx+50h],rax
  +04dc:  mov rdi,[rsp+0B0h]
  +04e4:  mov rax,7F80428F5000h    ; native poll veneer
  +04ee:  call rax
  +04f0:  mov rdx,[rsp+0B0h]
  +04f8:  mov rax,[rdx+50h]
  +04fc:  mov rcx,rax
  +04ff:  test cl,7
  +0502:  jne near 00007F80428CD7C2h
  +0508:  mov rsi,rcx
  +050b:  mov r9,[rsp+68h]
  +0510:  mov rcx,r9
  +0513:  add rcx,rsi
  +0516:  jo near 00007F80428CD68Ch
  +051c:  mov r9,[rsp+58h]
  +0521:  mov rsi,r9
  +0524:  add rsi,8
  +052b:  jo near 00007F80428CD563h
  +0531:  mov rax,rsi
  +0534:  mov [rsp+58h],rax
  +0539:  mov rax,rcx
  +053c:  mov [rsp+68h],rax
  +0541:  jmp 00007F80428CD062h
  +0546:  add rsp,0C0h
  +054d:  pop rbx
  +054e:  sub rsp,8
  +0552:  mov rax,0
  +055c:  call rax
  +055e:  add rsp,8
  +0562:  ret
; END ORDINARY-BRIDGE-FORWARD-LOOP
