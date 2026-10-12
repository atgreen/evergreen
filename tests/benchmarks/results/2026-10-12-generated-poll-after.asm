POLL-DISASSEMBLY-BEGIN
; POLL-GUARD-BENCH — 1 arg(s), 5 local(s), 2 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; OSR loop-header entry bcps: [10]
; 1954 bytes of x86-64:
; Mapped native transfer ABI — installed native code.
; 1954 bytes of x86-64:
  +0000:  push rbx
  +0001:  sub rsp,80h
  +0008:  mov [rsp+70h],rdi
  +000d:  mov rax,7F2C5FB780A0h
  +0017:  call rax
  +0019:  mov [rsp+78h],rax
  +001e:  mov rdi,[rsp+70h]
  +0023:  mov rcx,[rdi]
  +0027:  test cl,7
  +002a:  jne near 00007F2C3E9E12B6h
  +0030:  mov r9,rcx
  +0033:  mov [rsp+50h],r9
  +0038:  mov rax,0
  +0042:  mov [rsp+68h],rax
  +0047:  mov rbx,0
  +0051:  mov rdx,7F2C601A9060h
  +005b:  movzx eax,byte [rdx]
  +0060:  cmp rax,0
  +0067:  jne near 00007F2C3E9E1129h
  +006d:  mov rdx,7F2C601AD218h
  +0077:  movzx eax,byte [rdx]
  +007c:  cmp rax,0
  +0083:  jne near 00007F2C3E9E1129h
  +0089:  mov rdx,7F2C601ABCC0h
  +0093:  mov rax,[rdx]
  +0096:  cmp rax,0
  +009d:  jne near 00007F2C3E9E1129h
  +00a3:  mov rdx,7F2C601ABB70h
  +00ad:  mov rax,[rdx]
  +00b0:  cmp rax,0
  +00b7:  jne near 00007F2C3E9E1129h
  +00bd:  mov rdi,[rsp+78h]
  +00c2:  cmp rdi,0
  +00c9:  je near 00007F2C3E9E1129h
  +00cf:  mov rdx,7F2C601AC8F0h
  +00d9:  mov rax,[rdx]
  +00dc:  cmp rax,[rdi+10h]
  +00e0:  jne near 00007F2C3E9E1129h
  +00e6:  mov rdx,[rdi]
  +00e9:  mov rax,[rdx+10h]
  +00ed:  cmp rax,[rdi+8]
  +00f1:  jne near 00007F2C3E9E1129h
  +00f7:  mov rax,[rdx+18h]
  +00fb:  cmp rax,0
  +0102:  jne near 00007F2C3E9E1129h
  +0108:  mov rdx,rdi
  +010b:  add rdx,18h
  +0112:  movzx eax,byte [rdx]
  +0117:  cmp rax,0
  +011e:  jne near 00007F2C3E9E1129h
  +0124:  jmp 00007F2C3E9E113Ah
  +0129:  mov rdi,[rsp+70h]
  +012e:  mov rax,7F2C3E9E4000h    ; native poll veneer
  +0138:  call rax
  +013a:  mov r9,[rsp+68h]
  +013f:  mov r10,[rsp+50h]
  +0144:  cmp r9,r10
  +0147:  jl near 00007F2C3E9E1152h
  +014d:  jmp 00007F2C3E9E1271h
  +0152:  mov rdx,7F2C601A9060h
  +015c:  movzx eax,byte [rdx]
  +0161:  cmp rax,0
  +0168:  jne near 00007F2C3E9E122Ah
  +016e:  mov rdx,7F2C601AD218h
  +0178:  movzx eax,byte [rdx]
  +017d:  cmp rax,0
  +0184:  jne near 00007F2C3E9E122Ah
  +018a:  mov rdx,7F2C601ABCC0h
  +0194:  mov rax,[rdx]
  +0197:  cmp rax,0
  +019e:  jne near 00007F2C3E9E122Ah
  +01a4:  mov rdx,7F2C601ABB70h
  +01ae:  mov rax,[rdx]
  +01b1:  cmp rax,0
  +01b8:  jne near 00007F2C3E9E122Ah
  +01be:  mov rdi,[rsp+78h]
  +01c3:  cmp rdi,0
  +01ca:  je near 00007F2C3E9E122Ah
  +01d0:  mov rdx,7F2C601AC8F0h
  +01da:  mov rax,[rdx]
  +01dd:  cmp rax,[rdi+10h]
  +01e1:  jne near 00007F2C3E9E122Ah
  +01e7:  mov rdx,[rdi]
  +01ea:  mov rax,[rdx+10h]
  +01ee:  cmp rax,[rdi+8]
  +01f2:  jne near 00007F2C3E9E122Ah
  +01f8:  mov rax,[rdx+18h]
  +01fc:  cmp rax,0
  +0203:  jne near 00007F2C3E9E122Ah
  +0209:  mov rdx,rdi
  +020c:  add rdx,18h
  +0213:  movzx eax,byte [rdx]
  +0218:  cmp rax,0
  +021f:  jne near 00007F2C3E9E122Ah
  +0225:  jmp 00007F2C3E9E123Bh
  +022a:  mov rdi,[rsp+70h]
  +022f:  mov rax,7F2C3E9E4000h    ; native poll veneer
  +0239:  call rax
  +023b:  mov r9,[rsp+68h]
  +0240:  mov r8,rbx
  +0243:  add r8,r9
  +0246:  jo near 00007F2C3E9E14FDh
  +024c:  mov r9,[rsp+68h]
  +0251:  mov rcx,r9
  +0254:  add rcx,8
  +025b:  jo near 00007F2C3E9E13D4h
  +0261:  mov rax,rcx
  +0264:  mov [rsp+68h],rax
  +0269:  mov rbx,r8
  +026c:  jmp 00007F2C3E9E1051h
  +0271:  mov edi,0
  +0276:  mov esi,0
  +027b:  mov edx,0
  +0280:  mov rax,7F2C5FC1CB80h
  +028a:  call rax
  +028c:  mov rax,rbx
  +028f:  add rsp,80h
  +0296:  pop rbx
  +0297:  clc
  +0298:  ret
