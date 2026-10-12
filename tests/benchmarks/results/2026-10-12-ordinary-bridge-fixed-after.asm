; Fixed artifact representative trial0 listings.
; Loop prefixes stop before+0562; later deopt/OSR stubs omitted.

; BEGIN ORDINARY-BRIDGE-LEAF
; ORDINARY-BRIDGE-LEAF — 4 arg(s), 4 local(s), 1 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; 140 bytes of x86-64:
; Ordinary x86-64 Lisp ABI — installed native code.
; 140 bytes of x86-64:
  +0000:  push rbp
  +0001:  mov rbp,rsp
  +0004:  push rbx
  +0005:  sub rsp,8
  +0009:  sub rsp,10h
  +0010:  mov rbx,[rdi]
  +0014:  mov r8,[rdi+8]
  +0018:  mov rsi,[rdi+10h]
  +001c:  mov rcx,[rdi+18h]
  +0020:  jmp 00007FF80FE2307Dh
  +0025:  push rbp
  +0026:  mov rbp,rsp
  +0029:  push rbx
  +002a:  sub rsp,8
  +002e:  sub rsp,10h
  +0035:  mov rbx,rcx
  +0038:  mov rcx,r10
  +003b:  mov rsi,r9
  +003e:  jmp 00007FF80FE2307Dh
  +0043:  cmp rcx,4
  +004a:  je near 00007FF80FE2305Dh
  +0050:  mov r11,7FF80FE24000h
  +005a:  jmp r11
  +005d:  push rbp
  +005e:  mov rbp,rsp
  +0061:  push rbx
  +0062:  sub rsp,8
  +0066:  sub rsp,10h
  +006d:  mov r11,[rbp+10h]
  +0071:  mov rbx,rdi
  +0074:  mov rcx,r11
  +0077:  mov r8,rsi
  +007a:  mov rsi,rdx
  +007d:  mov rax,rcx
  +0080:  lea rsp,[rsp+18h]
  +0088:  pop rbx
  +0089:  pop rbp
  +008a:  clc
  +008b:  ret
; END ORDINARY-BRIDGE-LEAF

; BEGIN ORDINARY-BRIDGE-FORWARD
; ORDINARY-BRIDGE-FORWARD — 4 arg(s), 4 local(s), 4 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; 1029 bytes of x86-64:
; Ordinary x86-64 Lisp ABI — installed native code.
; 1029 bytes of x86-64:
  +0000:  ud2
  +0002:  push rbp
  +0003:  mov rbp,rsp
  +0006:  push rbx
  +0007:  sub rsp,8
  +000b:  sub rsp,90h
  +0012:  mov [rsp+60h],rdi
  +0017:  mov rax,[rdi]
  +001b:  mov [rsp+20h],rax
  +0020:  mov rax,[rdi+8]
  +0024:  mov [rsp+28h],rax
  +0029:  mov rax,[rdi+10h]
  +002d:  mov [rsp+30h],rax
  +0032:  mov rax,[rdi+18h]
  +0036:  mov [rsp+38h],rax
  +003b:  jmp 00007FF80FE030D4h
  +0040:  cmp rcx,4
  +0047:  je near 00007FF80FE0305Ah
  +004d:  mov r11,7FF80FE05000h
  +0057:  jmp r11
  +005a:  push rbp
  +005b:  mov rbp,rsp
  +005e:  push rbx
  +005f:  sub rsp,8
  +0063:  sub rsp,90h
  +006a:  mov r11,[rbp+10h]
  +006e:  mov rax,rdi
  +0071:  mov [rsp+20h],rax
  +0076:  mov rax,r11
  +0079:  mov [rsp+38h],rax
  +007e:  mov rax,rdx
  +0081:  mov [rsp+30h],rax
  +0086:  mov rax,rsi
  +0089:  mov [rsp+28h],rax
  +008e:  mov rax,0
  +0098:  mov [rsp+60h],rax
  +009d:  mov rax,[r13+20h]
  +00a1:  mov [rsp+68h],rax
  +00a6:  mov rax,[rsp+20h]
  +00ab:  mov [rsp+70h],rax
  +00b0:  mov rax,[rsp+28h]
  +00b5:  mov [rsp+78h],rax
  +00ba:  mov rax,[rsp+30h]
  +00bf:  mov [rsp+80h],rax
  +00c7:  mov rax,[rsp+38h]
  +00cc:  mov [rsp+88h],rax
  +00d4:  mov rax,[rsp+20h]
  +00d9:  mov [rsp+40h],rax
  +00de:  mov rax,[rsp+28h]
  +00e3:  mov [rsp+48h],rax
  +00e8:  mov rax,[rsp+30h]
  +00ed:  mov [rsp+50h],rax
  +00f2:  mov rax,[rsp+38h]
  +00f7:  mov [rsp+58h],rax
  +00fc:  mov rdi,[rsp+68h]
  +0101:  cmp rdi,0
  +0108:  je near 00007FF80FE03366h
  +010e:  mov rax,[rdi]
  +0111:  cmp rax,r13
  +0114:  jne near 00007FF80FE03366h
  +011a:  mov r11,7FF831AA1030h
  +0124:  movzx eax,byte [r11]
  +0129:  cmp rax,0
  +0130:  jne near 00007FF80FE03366h
  +0136:  mov r11,7FF831AA3310h
  +0140:  movzx eax,byte [r11]
  +0145:  cmp rax,0
  +014c:  jne near 00007FF80FE03366h
  +0152:  mov r11,7FF831AA1D20h
  +015c:  mov rax,[r11]
  +015f:  cmp rax,0
  +0166:  jne near 00007FF80FE03366h
  +016c:  mov r11,7FF831AA1C10h
  +0176:  mov rax,[r11]
  +0179:  cmp rax,0
  +0180:  jne near 00007FF80FE03366h
  +0186:  mov r11,7FF831AA32F8h
  +0190:  mov rax,[r11]
  +0193:  cmp rax,[rdi+30h]
  +0197:  jne near 00007FF80FE03366h
  +019d:  mov rax,[r13+10h]
  +01a1:  cmp rax,[rdi+28h]
  +01a5:  jne near 00007FF80FE03366h
  +01ab:  mov rax,[r13+18h]
  +01af:  cmp rax,0
  +01b6:  jne near 00007FF80FE03366h
  +01bc:  mov r11,rdi
  +01bf:  add r11,40h
  +01c6:  movzx eax,byte [r11]
  +01cb:  cmp rax,0
  +01d2:  jne near 00007FF80FE03366h
  +01d8:  mov r11,[rdi+18h]
  +01dc:  movzx eax,byte [r11]
  +01e1:  cmp rax,0
  +01e8:  jne near 00007FF80FE03366h
  +01ee:  mov r11,[rdi+8]
  +01f2:  mov eax,[r11]
  +01f5:  mov edx,[rdi+38h]
  +01f8:  cmp rax,rdx
  +01fb:  jge near 00007FF80FE03366h
  +0201:  mov r11,7FF81043A038h
  +020b:  mov r11,[r11]
  +020e:  cmp r11,0
  +0215:  je near 00007FF80FE03366h
  +021b:  mov rax,[r11]
  +021e:  mov rdx,5836344C495350h
  +0228:  cmp rax,rdx
  +022b:  jne near 00007FF80FE03366h
  +0231:  mov rax,[r11+8]
  +0235:  mov rdx,1
  +023f:  cmp rax,rdx
  +0242:  jne near 00007FF80FE03366h
  +0248:  mov rax,[r11+30h]
  +024c:  mov rdx,4
  +0256:  cmp rax,rdx
  +0259:  jne near 00007FF80FE03366h
  +025f:  mov rax,[r11+38h]
  +0263:  mov rdx,0
  +026d:  cmp rax,rdx
  +0270:  jne near 00007FF80FE03366h
  +0276:  mov rdx,7FF81043A070h
  +0280:  mov rax,[rdx]
  +0283:  cmp rax,[r11+10h]
  +0287:  jne near 00007FF80FE03366h
  +028d:  mov rax,[r11+40h]
  +0291:  cmp rax,0
  +0298:  je near 00007FF80FE03366h
  +029e:  mov rax,[r11+20h]
  +02a2:  cmp rax,0
  +02a9:  je near 00007FF80FE03366h
  +02af:  mov rdx,[r11+28h]
  +02b3:  add rax,rdx
  +02b6:  jb near 00007FF80FE03366h
  +02bc:  mov rdx,[r11+18h]
  +02c0:  cmp rax,rdx
  +02c3:  jbe near 00007FF80FE03366h
  +02c9:  mov rax,[r11+20h]
  +02cd:  cmp rdx,rax
  +02d0:  jb near 00007FF80FE03366h
  +02d6:  mov rdx,[r11+48h]
  +02da:  cmp rdx,8
  +02e1:  jl near 00007FF80FE03366h
  +02e7:  mov rax,rsp
  +02ea:  sub rax,18h
  +02f1:  jb near 00007FF80FE03366h
  +02f7:  sub rax,rdx
  +02fa:  jb near 00007FF80FE03366h
  +0300:  mov rdx,[rdi+58h]
  +0304:  cmp rax,rdx
  +0307:  jbe near 00007FF80FE03366h
  +030d:  mov rdx,[rdi+8]
  +0311:  inc dword [rdx]
  +0313:  mov r11,[r11+18h]
  +0317:  sub rsp,10h
  +031e:  mov rax,[rsp+68h]
  +0323:  mov [rsp],rax
  +0327:  mov rdi,[rsp+50h]
  +032c:  mov rsi,[rsp+58h]
  +0331:  mov rdx,[rsp+60h]
  +0336:  mov rcx,4
  +0340:  mov rax,7
  +034a:  call r11
  +034d:  mov r11,[rsp+78h]
  +0352:  mov r11,[r11+8]
  +0356:  dec dword [r11]
  +0359:  lea rsp,[rsp+10h]
  +0361:  jmp 00007FF80FE033FAh
  +0366:  sub rsp,40h
  +036d:  mov rax,8E0h
  +0377:  mov [rsp],rax
  +037b:  mov rax,4
  +0385:  mov [rsp+8],rax
  +038a:  mov rax,7
  +0394:  mov [rsp+18h],rax
  +0399:  mov rax,7FF81043A038h
  +03a3:  mov [rsp+28h],rax
  +03a8:  mov rax,7FF81043A070h
  +03b2:  mov [rsp+30h],rax
  +03b7:  mov rax,0
  +03c1:  mov [rsp+38h],rax
  +03c6:  mov rax,rsp
  +03c9:  add rax,80h
  +03d0:  mov [rsp+10h],rax
  +03d5:  mov rax,[rsp+0A8h]
  +03dd:  mov [rsp+20h],rax
  +03e2:  mov rdi,rsp
  +03e5:  mov r11,7FF80FE04000h
  +03ef:  call r11
  +03f2:  lea rsp,[rsp+40h]
  +03fa:  lea rsp,[rsp+98h]
  +0402:  pop rbx
  +0403:  pop rbp
  +0404:  ret
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
  +0010:  mov rax,7FF831384610h
  +001a:  call rax
  +001c:  mov [rsp+0B8h],rax
  +0024:  mov rdi,[rsp+0B0h]
  +002c:  mov rcx,[rdi]
  +0030:  test cl,7
  +0033:  jne near 00007FF80FDFE7C2h
  +0039:  mov r9,rcx
  +003c:  mov [rsp+98h],r9
  +0044:  mov rax,0
  +004e:  mov [rsp+58h],rax
  +0053:  mov rax,0
  +005d:  mov [rsp+68h],rax
  +0062:  mov rdx,7FF831AA1030h
  +006c:  movzx eax,byte [rdx]
  +0071:  cmp rax,0
  +0078:  jne near 00007FF80FDFE13Dh
  +007e:  mov rdx,7FF831AA3310h
  +0088:  movzx eax,byte [rdx]
  +008d:  cmp rax,0
  +0094:  jne near 00007FF80FDFE13Dh
  +009a:  mov rdx,7FF831AA1D20h
  +00a4:  mov rax,[rdx]
  +00a7:  cmp rax,0
  +00ae:  jne near 00007FF80FDFE13Dh
  +00b4:  mov rdx,7FF831AA1C10h
  +00be:  mov rax,[rdx]
  +00c1:  cmp rax,0
  +00c8:  jne near 00007FF80FDFE13Dh
  +00ce:  mov rdi,[rsp+0B8h]
  +00d6:  cmp rdi,0
  +00dd:  je near 00007FF80FDFE13Dh
  +00e3:  mov rdx,7FF831AA32F8h
  +00ed:  mov rax,[rdx]
  +00f0:  cmp rax,[rdi+10h]
  +00f4:  jne near 00007FF80FDFE13Dh
  +00fa:  mov rdx,[rdi]
  +00fd:  mov rax,[rdx+10h]
  +0101:  cmp rax,[rdi+8]
  +0105:  jne near 00007FF80FDFE13Dh
  +010b:  mov rax,[rdx+18h]
  +010f:  cmp rax,0
  +0116:  jne near 00007FF80FDFE13Dh
  +011c:  mov rdx,rdi
  +011f:  add rdx,18h
  +0126:  movzx eax,byte [rdx]
  +012b:  cmp rax,0
  +0132:  jne near 00007FF80FDFE13Dh
  +0138:  jmp 00007FF80FDFE177h
  +013d:  mov rdx,[rsp+0B0h]
  +0145:  mov rax,7
  +014f:  mov [rdx+50h],rax
  +0153:  mov [rdx+58h],rax
  +0157:  mov [rdx+60h],rax
  +015b:  mov [rdx+68h],rax
  +015f:  mov [rdx+70h],rax
  +0163:  mov rdi,[rsp+0B0h]
  +016b:  mov rax,7FF80FE02000h    ; native poll veneer
  +0175:  call rax
  +0177:  mov r9,[rsp+58h]
  +017c:  mov r10,[rsp+98h]
  +0184:  cmp r9,r10
  +0187:  mov rbx,0Fh
  +0191:  jge near 00007FF80FDFE1A1h
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
  +01d6:  mov rax,7FF8312577E0h
  +01e0:  call rax
  +01e2:  mov r11,rbx
  +01e5:  cmp r11,7
  +01ec:  je near 00007FF80FDFE1F7h
  +01f2:  jmp 00007FF80FDFE3C3h
  +01f7:  mov rdx,7FF831AA1030h
  +0201:  movzx eax,byte [rdx]
  +0206:  cmp rax,0
  +020d:  jne near 00007FF80FDFE2D2h
  +0213:  mov rdx,7FF831AA3310h
  +021d:  movzx eax,byte [rdx]
  +0222:  cmp rax,0
  +0229:  jne near 00007FF80FDFE2D2h
  +022f:  mov rdx,7FF831AA1D20h
  +0239:  mov rax,[rdx]
  +023c:  cmp rax,0
  +0243:  jne near 00007FF80FDFE2D2h
  +0249:  mov rdx,7FF831AA1C10h
  +0253:  mov rax,[rdx]
  +0256:  cmp rax,0
  +025d:  jne near 00007FF80FDFE2D2h
  +0263:  mov rdi,[rsp+0B8h]
  +026b:  cmp rdi,0
  +0272:  je near 00007FF80FDFE2D2h
  +0278:  mov rdx,7FF831AA32F8h
  +0282:  mov rax,[rdx]
  +0285:  cmp rax,[rdi+10h]
  +0289:  jne near 00007FF80FDFE2D2h
  +028f:  mov rdx,[rdi]
  +0292:  mov rax,[rdx+10h]
  +0296:  cmp rax,[rdi+8]
  +029a:  jne near 00007FF80FDFE2D2h
  +02a0:  mov rax,[rdx+18h]
  +02a4:  cmp rax,0
  +02ab:  jne near 00007FF80FDFE2D2h
  +02b1:  mov rdx,rdi
  +02b4:  add rdx,18h
  +02bb:  movzx eax,byte [rdx]
  +02c0:  cmp rax,0
  +02c7:  jne near 00007FF80FDFE2D2h
  +02cd:  jmp 00007FF80FDFE30Ch
  +02d2:  mov rdx,[rsp+0B0h]
  +02da:  mov rax,7
  +02e4:  mov [rdx+50h],rax
  +02e8:  mov [rdx+58h],rax
  +02ec:  mov [rdx+60h],rax
  +02f0:  mov [rdx+68h],rax
  +02f4:  mov [rdx+70h],rax
  +02f8:  mov rdi,[rsp+0B0h]
  +0300:  mov rax,7FF80FE02000h    ; native poll veneer
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
  +03a5:  mov rax,7FF80FE1D000h
  +03af:  call rax
  +03b1:  add rsp,40h
  +03b8:  mov rbx,rax
  +03bb:  mov rcx,rbx
  +03be:  jmp 00007FF80FDFE3D4h
  +03c3:  mov rax,[rsp+68h]
  +03c8:  add rsp,0C0h
  +03cf:  pop rbx
  +03d0:  clc
  +03d1:  ret
  +03d2:  ud2
  +03d4:  mov rdx,7FF831AA1030h
  +03de:  movzx eax,byte [rdx]
  +03e3:  cmp rax,0
  +03ea:  jne near 00007FF80FDFE4AFh
  +03f0:  mov rdx,7FF831AA3310h
  +03fa:  movzx eax,byte [rdx]
  +03ff:  cmp rax,0
  +0406:  jne near 00007FF80FDFE4AFh
  +040c:  mov rdx,7FF831AA1D20h
  +0416:  mov rax,[rdx]
  +0419:  cmp rax,0
  +0420:  jne near 00007FF80FDFE4AFh
  +0426:  mov rdx,7FF831AA1C10h
  +0430:  mov rax,[rdx]
  +0433:  cmp rax,0
  +043a:  jne near 00007FF80FDFE4AFh
  +0440:  mov rdi,[rsp+0B8h]
  +0448:  cmp rdi,0
  +044f:  je near 00007FF80FDFE4AFh
  +0455:  mov rdx,7FF831AA32F8h
  +045f:  mov rax,[rdx]
  +0462:  cmp rax,[rdi+10h]
  +0466:  jne near 00007FF80FDFE4AFh
  +046c:  mov rdx,[rdi]
  +046f:  mov rax,[rdx+10h]
  +0473:  cmp rax,[rdi+8]
  +0477:  jne near 00007FF80FDFE4AFh
  +047d:  mov rax,[rdx+18h]
  +0481:  cmp rax,0
  +0488:  jne near 00007FF80FDFE4AFh
  +048e:  mov rdx,rdi
  +0491:  add rdx,18h
  +0498:  movzx eax,byte [rdx]
  +049d:  cmp rax,0
  +04a4:  jne near 00007FF80FDFE4AFh
  +04aa:  jmp 00007FF80FDFE4FFh
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
  +04e4:  mov rax,7FF80FE02000h    ; native poll veneer
  +04ee:  call rax
  +04f0:  mov rdx,[rsp+0B0h]
  +04f8:  mov rax,[rdx+50h]
  +04fc:  mov rcx,rax
  +04ff:  test cl,7
  +0502:  jne near 00007FF80FDFEA19h
  +0508:  mov rsi,rcx
  +050b:  mov r9,[rsp+68h]
  +0510:  mov rcx,r9
  +0513:  add rcx,rsi
  +0516:  jo near 00007FF80FDFE563h
  +051c:  mov r9,[rsp+58h]
  +0521:  mov rsi,r9
  +0524:  add rsi,8
  +052b:  jo near 00007FF80FDFE699h
  +0531:  mov rax,rsi
  +0534:  mov [rsp+58h],rax
  +0539:  mov rax,rcx
  +053c:  mov [rsp+68h],rax
  +0541:  jmp 00007FF80FDFE062h
  +0546:  add rsp,0C0h
  +054d:  pop rbx
  +054e:  sub rsp,8
  +0552:  mov rax,0
  +055c:  call rax
  +055e:  add rsp,8
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
  +0010:  mov rax,7FF831384610h
  +001a:  call rax
  +001c:  mov [rsp+0B8h],rax
  +0024:  mov rdi,[rsp+0B0h]
  +002c:  mov rcx,[rdi]
  +0030:  test cl,7
  +0033:  jne near 00007FF80FDFBA31h
  +0039:  mov r9,rcx
  +003c:  mov [rsp+98h],r9
  +0044:  mov rax,0
  +004e:  mov [rsp+58h],rax
  +0053:  mov rax,0
  +005d:  mov [rsp+68h],rax
  +0062:  mov rdx,7FF831AA1030h
  +006c:  movzx eax,byte [rdx]
  +0071:  cmp rax,0
  +0078:  jne near 00007FF80FDFB13Dh
  +007e:  mov rdx,7FF831AA3310h
  +0088:  movzx eax,byte [rdx]
  +008d:  cmp rax,0
  +0094:  jne near 00007FF80FDFB13Dh
  +009a:  mov rdx,7FF831AA1D20h
  +00a4:  mov rax,[rdx]
  +00a7:  cmp rax,0
  +00ae:  jne near 00007FF80FDFB13Dh
  +00b4:  mov rdx,7FF831AA1C10h
  +00be:  mov rax,[rdx]
  +00c1:  cmp rax,0
  +00c8:  jne near 00007FF80FDFB13Dh
  +00ce:  mov rdi,[rsp+0B8h]
  +00d6:  cmp rdi,0
  +00dd:  je near 00007FF80FDFB13Dh
  +00e3:  mov rdx,7FF831AA32F8h
  +00ed:  mov rax,[rdx]
  +00f0:  cmp rax,[rdi+10h]
  +00f4:  jne near 00007FF80FDFB13Dh
  +00fa:  mov rdx,[rdi]
  +00fd:  mov rax,[rdx+10h]
  +0101:  cmp rax,[rdi+8]
  +0105:  jne near 00007FF80FDFB13Dh
  +010b:  mov rax,[rdx+18h]
  +010f:  cmp rax,0
  +0116:  jne near 00007FF80FDFB13Dh
  +011c:  mov rdx,rdi
  +011f:  add rdx,18h
  +0126:  movzx eax,byte [rdx]
  +012b:  cmp rax,0
  +0132:  jne near 00007FF80FDFB13Dh
  +0138:  jmp 00007FF80FDFB177h
  +013d:  mov rdx,[rsp+0B0h]
  +0145:  mov rax,7
  +014f:  mov [rdx+50h],rax
  +0153:  mov [rdx+58h],rax
  +0157:  mov [rdx+60h],rax
  +015b:  mov [rdx+68h],rax
  +015f:  mov [rdx+70h],rax
  +0163:  mov rdi,[rsp+0B0h]
  +016b:  mov rax,7FF80FF5A000h    ; native poll veneer
  +0175:  call rax
  +0177:  mov r9,[rsp+58h]
  +017c:  mov r10,[rsp+98h]
  +0184:  cmp r9,r10
  +0187:  mov rbx,0Fh
  +0191:  jge near 00007FF80FDFB1A1h
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
  +01d6:  mov rax,7FF8312577E0h
  +01e0:  call rax
  +01e2:  mov r11,rbx
  +01e5:  cmp r11,7
  +01ec:  je near 00007FF80FDFB1F7h
  +01f2:  jmp 00007FF80FDFB3C3h
  +01f7:  mov rdx,7FF831AA1030h
  +0201:  movzx eax,byte [rdx]
  +0206:  cmp rax,0
  +020d:  jne near 00007FF80FDFB2D2h
  +0213:  mov rdx,7FF831AA3310h
  +021d:  movzx eax,byte [rdx]
  +0222:  cmp rax,0
  +0229:  jne near 00007FF80FDFB2D2h
  +022f:  mov rdx,7FF831AA1D20h
  +0239:  mov rax,[rdx]
  +023c:  cmp rax,0
  +0243:  jne near 00007FF80FDFB2D2h
  +0249:  mov rdx,7FF831AA1C10h
  +0253:  mov rax,[rdx]
  +0256:  cmp rax,0
  +025d:  jne near 00007FF80FDFB2D2h
  +0263:  mov rdi,[rsp+0B8h]
  +026b:  cmp rdi,0
  +0272:  je near 00007FF80FDFB2D2h
  +0278:  mov rdx,7FF831AA32F8h
  +0282:  mov rax,[rdx]
  +0285:  cmp rax,[rdi+10h]
  +0289:  jne near 00007FF80FDFB2D2h
  +028f:  mov rdx,[rdi]
  +0292:  mov rax,[rdx+10h]
  +0296:  cmp rax,[rdi+8]
  +029a:  jne near 00007FF80FDFB2D2h
  +02a0:  mov rax,[rdx+18h]
  +02a4:  cmp rax,0
  +02ab:  jne near 00007FF80FDFB2D2h
  +02b1:  mov rdx,rdi
  +02b4:  add rdx,18h
  +02bb:  movzx eax,byte [rdx]
  +02c0:  cmp rax,0
  +02c7:  jne near 00007FF80FDFB2D2h
  +02cd:  jmp 00007FF80FDFB30Ch
  +02d2:  mov rdx,[rsp+0B0h]
  +02da:  mov rax,7
  +02e4:  mov [rdx+50h],rax
  +02e8:  mov [rdx+58h],rax
  +02ec:  mov [rdx+60h],rax
  +02f0:  mov [rdx+68h],rax
  +02f4:  mov [rdx+70h],rax
  +02f8:  mov rdi,[rsp+0B0h]
  +0300:  mov rax,7FF80FF5A000h    ; native poll veneer
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
  +03a5:  mov rax,7FF81040A000h
  +03af:  call rax
  +03b1:  add rsp,40h
  +03b8:  mov rbx,rax
  +03bb:  mov rcx,rbx
  +03be:  jmp 00007FF80FDFB3D4h
  +03c3:  mov rax,[rsp+68h]
  +03c8:  add rsp,0C0h
  +03cf:  pop rbx
  +03d0:  clc
  +03d1:  ret
  +03d2:  ud2
  +03d4:  mov rdx,7FF831AA1030h
  +03de:  movzx eax,byte [rdx]
  +03e3:  cmp rax,0
  +03ea:  jne near 00007FF80FDFB4AFh
  +03f0:  mov rdx,7FF831AA3310h
  +03fa:  movzx eax,byte [rdx]
  +03ff:  cmp rax,0
  +0406:  jne near 00007FF80FDFB4AFh
  +040c:  mov rdx,7FF831AA1D20h
  +0416:  mov rax,[rdx]
  +0419:  cmp rax,0
  +0420:  jne near 00007FF80FDFB4AFh
  +0426:  mov rdx,7FF831AA1C10h
  +0430:  mov rax,[rdx]
  +0433:  cmp rax,0
  +043a:  jne near 00007FF80FDFB4AFh
  +0440:  mov rdi,[rsp+0B8h]
  +0448:  cmp rdi,0
  +044f:  je near 00007FF80FDFB4AFh
  +0455:  mov rdx,7FF831AA32F8h
  +045f:  mov rax,[rdx]
  +0462:  cmp rax,[rdi+10h]
  +0466:  jne near 00007FF80FDFB4AFh
  +046c:  mov rdx,[rdi]
  +046f:  mov rax,[rdx+10h]
  +0473:  cmp rax,[rdi+8]
  +0477:  jne near 00007FF80FDFB4AFh
  +047d:  mov rax,[rdx+18h]
  +0481:  cmp rax,0
  +0488:  jne near 00007FF80FDFB4AFh
  +048e:  mov rdx,rdi
  +0491:  add rdx,18h
  +0498:  movzx eax,byte [rdx]
  +049d:  cmp rax,0
  +04a4:  jne near 00007FF80FDFB4AFh
  +04aa:  jmp 00007FF80FDFB4FFh
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
  +04e4:  mov rax,7FF80FF5A000h    ; native poll veneer
  +04ee:  call rax
  +04f0:  mov rdx,[rsp+0B0h]
  +04f8:  mov rax,[rdx+50h]
  +04fc:  mov rcx,rax
  +04ff:  test cl,7
  +0502:  jne near 00007FF80FDFB8FBh
  +0508:  mov rsi,rcx
  +050b:  mov r9,[rsp+68h]
  +0510:  mov rcx,r9
  +0513:  add rcx,rsi
  +0516:  jo near 00007FF80FDFB563h
  +051c:  mov r9,[rsp+58h]
  +0521:  mov rsi,r9
  +0524:  add rsi,8
  +052b:  jo near 00007FF80FDFB7D2h
  +0531:  mov rax,rsi
  +0534:  mov [rsp+58h],rax
  +0539:  mov rax,rcx
  +053c:  mov [rsp+68h],rax
  +0541:  jmp 00007FF80FDFB062h
  +0546:  add rsp,0C0h
  +054d:  pop rbx
  +054e:  sub rsp,8
  +0552:  mov rax,0
  +055c:  call rax
  +055e:  add rsp,8
; END ORDINARY-BRIDGE-FORWARD-LOOP

