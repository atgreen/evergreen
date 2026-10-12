POLL-DISASSEMBLY-BEGIN
; POLL-GUARD-BENCH — 1 arg(s), 5 local(s), 2 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; OSR loop-header entry bcps: [10]
; 1482 bytes of x86-64:
; Mapped native transfer ABI — installed native code.
; 1482 bytes of x86-64:
  +0000:  push rbx
  +0001:  sub rsp,80h
  +0008:  mov [rsp+70h],rdi
  +000d:  mov rcx,[rdi]
  +0011:  test cl,7
  +0014:  jne near 00007EFBE1E6D353h
  +001a:  mov r9,rcx
  +001d:  mov [rsp+50h],r9
  +0022:  mov rax,0
  +002c:  mov [rsp+68h],rax
  +0031:  mov rbx,0
  +003b:  mov rdi,[rsp+70h]
  +0040:  or rdi,1
  +0044:  mov rax,7EFBE1E8B000h    ; native poll veneer
  +004e:  call rax
  +0050:  mov r9,[rsp+68h]
  +0055:  mov r10,[rsp+50h]
  +005a:  cmp r9,r10
  +005d:  jl near 00007EFBE1E6D068h
  +0063:  jmp 00007EFBE1E6D0AFh
  +0068:  mov rdi,[rsp+70h]
  +006d:  mov rax,7EFBE1E8B000h    ; native poll veneer
  +0077:  call rax
  +0079:  mov r9,[rsp+68h]
  +007e:  mov r8,rbx
  +0081:  add r8,r9
  +0084:  jo near 00007EFBE1E6D21Dh
  +008a:  mov r9,[rsp+68h]
  +008f:  mov rcx,r9
  +0092:  add rcx,8
  +0099:  jo near 00007EFBE1E6D0F4h
  +009f:  mov rax,rcx
  +00a2:  mov [rsp+68h],rax
  +00a7:  mov rbx,r8
  +00aa:  jmp 00007EFBE1E6D03Bh
  +00af:  mov edi,0
  +00b4:  mov esi,0
  +00b9:  mov edx,0
  +00be:  mov rax,7EFC030CB9D0h
  +00c8:  call rax
  +00ca:  mov rax,rbx
  +00cd:  add rsp,80h
  +00d4:  pop rbx
  +00d5:  clc
  +00d6:  ret
