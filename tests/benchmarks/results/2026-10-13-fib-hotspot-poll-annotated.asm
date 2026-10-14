; Freshly generated from release commit 674838c7, x86_64-unknown-linux-musl.
; Captured with DISASSEMBLE from the installed T2 executable, not handwritten assembly.
; Both self calls target +003e. Code: 2311 bytes (was 2685); frame: 136 bytes (unchanged).
; Per admission: 41 -> 12 instructions, 14 -> 4 conditional branches, 234 -> 65 bytes.
; Eleven administrative checks -> one notification test; three physical-stack branches remain.
; Proven-empty MV state already omitted an MV admission guard before this change.
; CPU 0, two fresh processes per side, seven warmed fib(35) samples per process.
; Paired order before/after/after/before; medians 190.5 ms -> 107 ms (1.78x).
; All 28 checksums were 9227465; installed code stayed identical within each trial.
; Raw process addresses and instruction offsets below are from the after capture.
FIB-CODE-BASE 140351166894080
; FIB — 1 arg(s), 1 local(s), 3 stack slot(s)
; T2 — profile-guided native using static proofs and guarded type choices
; from observed profiles or cold-site guesses; failed guards deoptimise to T0.
; 2311 bytes of x86-64:
; Ordinary x86-64 Lisp ABI — installed native code.
; 2311 bytes of x86-64:
  +0000:  ud2
  +0002:  push rbp
  +0003:  mov rbp,rsp
  +0006:  push rbx
  +0007:  sub rsp,8
  +000b:  sub rsp,70h
  +0012:  test byte [rsp],0
  +0016:  mov [rsp+50h],rdi
  +001b:  mov rsi,[rdi]
  +001f:  jmp 00007FA60D725091h
; Public entry checks arity once; direct self-calls target accepted entry +003e.
  +0024:  cmp rcx,1
  +002b:  je near 00007FA60D72503Eh
  +0031:  mov r11,7FA60D738000h
  +003b:  jmp r11
; Fixnum base case: n < 2 returns before constructing any native frame.
  +003e:  test dil,7
  +0042:  jne near 00007FA60D72505Ah
; Tagged fixnums have scale 8: tagged 2 is 10h.
  +0048:  cmp rdi,10h
  +004f:  jge near 00007FA60D72505Ah
  +0055:  mov rax,rdi
; Clear carry to return exactly one value.
  +0058:  clc
  +0059:  ret
; Recursive path: construct the same 136-byte physical frame as before.
  +005a:  push rbp
  +005b:  mov rbp,rsp
  +005e:  push rbx
  +005f:  sub rsp,8
  +0063:  sub rsp,70h
  +006a:  test byte [rsp],0
  +006e:  mov rsi,rdi
  +0071:  mov rax,0
  +007b:  mov [rsp+50h],rax
; Save boundary-established native control from the current execution.
  +0080:  mov rax,[r13+20h]
  +0084:  mov [rsp+58h],rax
  +0089:  mov rax,rsi
  +008c:  mov [rsp+60h],rax
  +0091:  test sil,7
  +0095:  jne near 00007FA60D7255F8h
  +009b:  mov r9,rsi
  +009e:  mov [rsp+28h],r9
  +00a3:  mov r9,[rsp+28h]
  +00a8:  cmp r9,10h
  +00af:  jge near 00007FA60D7250BFh
  +00b5:  mov rsi,[rsp+28h]
  +00ba:  jmp 00007FA60D725387h
; Compute tagged n - 1; overflow takes guarded deoptimization.
  +00bf:  mov r9,[rsp+28h]
  +00c4:  mov r10,r9
  +00c7:  sub r10,8
  +00ce:  jo near 00007FA60D725769h
  +00d4:  mov [rsp],r10
  +00d8:  mov rax,[rsp]
  +00dc:  mov [rsp+48h],rax
; First admission begins: 12 instructions / 65 bytes / 4 conditional branches.
  +00e1:  mov rdi,[rsp+58h]
; The eleven old administrative checks become this one local poll (3 instructions). R13 is the owning Lisp execution; offset 28h points to its stable poll slot.
  +00e6:  mov r11,[r13+28h]
; The slot is a notification, not authoritative state. Runtime entry/return drains sources with roots published and rearms remaining work.
  +00ea:  cmp qword [r11],0
; Pending GC, signal, yield, native-error or explicit sandbox work takes the rooted cold helper at +0145.
  +00ef:  jne near 00007FA60D725145h
; Retain physical-stack safety. F0h is the published callee/helper stack budget, not a recursion-count limit.
  +00f5:  mov rdx,0F0h
  +00ff:  mov rax,rsp
; Include return-address space; reject arithmetic underflow.
  +0102:  sub rax,8
  +0109:  jb near 00007FA60D725145h
; Subtract the callee stack budget; reject arithmetic underflow.
  +010f:  sub rax,rdx
  +0112:  jb near 00007FA60D725145h
; Reject crossing the physical stack limit from native control +58h.
  +0118:  cmp rax,[rdi+58h]
  +011c:  jbe near 00007FA60D725145h
; Admission ends; load n - 1 into the ordinary Lisp argument register.
  +0122:  mov rdi,[rsp+48h]
  +0127:  mov rcx,1
  +0131:  mov rax,7
; Actual relative self-call: fib(n - 1) -> accepted entry +003e. No depth increment/decrement.
  +013b:  call 00007FA60D72503Eh
  +0140:  jmp 00007FA60D7251D9h
; Cold admission publishes live roots and services runtime work before calling the retained exact definition.
  +0145:  sub rsp,40h
  +014c:  mov rax,8EDh
  +0156:  mov [rsp],rax
  +015a:  mov rax,1
  +0164:  mov [rsp+8],rax
  +0169:  mov rax,7
  +0173:  mov [rsp+18h],rax
  +0178:  mov rax,7FA60DDCDF38h
  +0182:  mov [rsp+28h],rax
  +0187:  mov rax,7FA60DDCDF78h
  +0191:  mov [rsp+30h],rax
  +0196:  mov rax,1
  +01a0:  mov [rsp+38h],rax
  +01a5:  mov rax,rsp
  +01a8:  add rax,88h
  +01af:  mov [rsp+10h],rax
  +01b4:  mov rax,[rsp+98h]
  +01bc:  mov [rsp+20h],rax
  +01c1:  mov rdi,rsp
  +01c4:  mov r11,7FA60D727000h
  +01ce:  call r11
  +01d1:  lea rsp,[rsp+40h]
; Save first result; clear return carry skips the multiple-value cleanup helper.
  +01d9:  mov [rsp+8],rax
  +01de:  jae near 00007FA60D7251FFh
  +01e4:  mov edi,0
  +01e9:  mov esi,0
  +01ee:  mov edx,0
  +01f3:  mov rax,7FA62E7F6370h
  +01fd:  call rax
; Compute tagged n - 2.
  +01ff:  mov r9,[rsp+28h]
  +0204:  mov r10,r9
  +0207:  sub r10,10h
  +020e:  jo near 00007FA60D725832h
  +0214:  mov [rsp+18h],r10
  +0219:  mov r9,[rsp+8]
; Guard first result as a fixnum before native arithmetic.
  +021e:  test r9b,7
  +0222:  jne near 00007FA60D725396h
  +0228:  mov r10,r9
  +022b:  mov [rsp+30h],r10
  +0230:  mov rax,[rsp+18h]
  +0235:  mov [rsp+48h],rax
; Second admission: same local-poll and physical-stack checks, another 65 bytes.
  +023a:  mov rdi,[rsp+58h]
; Second execution-local poll.
  +023f:  mov r11,[r13+28h]
  +0243:  cmp qword [r11],0
; Pending work goes to the second rooted cold helper at +029e.
  +0248:  jne near 00007FA60D72529Eh
; Same physical stack budget and arithmetic/bounds checks.
  +024e:  mov rdx,0F0h
  +0258:  mov rax,rsp
  +025b:  sub rax,8
  +0262:  jb near 00007FA60D72529Eh
  +0268:  sub rax,rdx
  +026b:  jb near 00007FA60D72529Eh
  +0271:  cmp rax,[rdi+58h]
  +0275:  jbe near 00007FA60D72529Eh
; Load n - 2 into the argument register.
  +027b:  mov rdi,[rsp+48h]
  +0280:  mov rcx,1
  +028a:  mov rax,7
; Actual relative self-call: fib(n - 2) -> accepted entry +003e.
  +0294:  call 00007FA60D72503Eh
  +0299:  jmp 00007FA60D725332h
; Second cold-admission block.
  +029e:  sub rsp,40h
  +02a5:  mov rax,8EDh
  +02af:  mov [rsp],rax
  +02b3:  mov rax,1
  +02bd:  mov [rsp+8],rax
  +02c2:  mov rax,7
  +02cc:  mov [rsp+18h],rax
  +02d1:  mov rax,7FA60DDCDF38h
  +02db:  mov [rsp+28h],rax
  +02e0:  mov rax,7FA60DDCDF78h
  +02ea:  mov [rsp+30h],rax
  +02ef:  mov rax,1
  +02f9:  mov [rsp+38h],rax
  +02fe:  mov rax,rsp
  +0301:  add rax,88h
  +0308:  mov [rsp+10h],rax
  +030d:  mov rax,[rsp+98h]
  +0315:  mov [rsp+20h],rax
  +031a:  mov rdi,rsp
  +031d:  mov r11,7FA60D727000h
  +0327:  call r11
  +032a:  lea rsp,[rsp+40h]
; Retain second result and normalize multiple values only if return carry requires it.
  +0332:  mov rbx,rax
  +0335:  jae near 00007FA60D725356h
  +033b:  mov edi,0
  +0340:  mov esi,0
  +0345:  mov edx,0
  +034a:  mov rax,7FA62E7F6370h
  +0354:  call rax
; Guard second result as fixnum.
  +0356:  test bl,7
  +0359:  jne near 00007FA60D72546Bh
  +035f:  mov r9,rbx
  +0362:  mov [rsp+40h],r9
; Load both tagged results, add, and deopt on overflow.
  +0367:  mov r9,[rsp+30h]
  +036c:  mov r10,[rsp+40h]
  +0371:  mov r11,r9
  +0374:  add r11,r10
  +0377:  jo near 00007FA60D72552Fh
  +037d:  mov [rsp+20h],r11
  +0382:  mov rsi,[rsp+20h]
; Return sum; retire physical frame; clear carry for one return value.
  +0387:  mov rax,rsi
  +038a:  lea rsp,[rsp+78h]
  +0392:  pop rbx
  +0393:  pop rbp
  +0394:  clc
  +0395:  ret
; Guard/deoptimization recovery blocks follow; raw disassembly is preserved below.
  +0396:  sub rsp,60h
  +039d:  test byte [rsp],0
  +03a1:  mov rax,8EDh
  +03ab:  mov [rsp],rax
  +03af:  mov rax,0Ch
  +03b9:  mov [rsp+8],rax
  +03be:  mov rax,1
  +03c8:  mov [rsp+10h],rax
  +03cd:  mov rax,3
  +03d7:  mov [rsp+18h],rax
  +03dc:  mov rax,17h
  +03e6:  mov [rsp+20h],rax
  +03eb:  mov rax,[rsp+68h]
  +03f0:  mov [rsp+28h],rax
  +03f5:  mov rax,[rsp+88h]
  +03fd:  mov [rsp+30h],rax
  +0402:  mov rax,10h
  +040c:  mov [rsp+38h],rax
  +0411:  mov rax,1
  +041b:  mov [rsp+40h],rax
  +0420:  mov rax,8
  +042a:  mov [rsp+48h],rax
  +042f:  mov rax,rsp
  +0432:  mov [rsp+50h],rax
  +0437:  mov rax,[rsp+0B8h]
  +043f:  mov [rsp+58h],rax
  +0444:  lea rdi,[rsp+40h]
  +044c:  mov rax,7FA60D724000h    ; native precise deopt veneer
  +0456:  call rax
  +0458:  lea rsp,[rsp+60h]
  +0460:  lea rsp,[rsp+78h]
  +0468:  pop rbx
  +0469:  pop rbp
  +046a:  ret
  +046b:  sub rsp,60h
  +0472:  test byte [rsp],0
  +0476:  mov rax,8EDh
  +0480:  mov [rsp],rax
  +0484:  mov rax,0Eh
  +048e:  mov [rsp+8],rax
  +0493:  mov rax,1
  +049d:  mov [rsp+10h],rax
  +04a2:  mov rax,2
  +04ac:  mov [rsp+18h],rax
  +04b1:  mov rax,17h
  +04bb:  mov [rsp+20h],rax
  +04c0:  mov rax,[rsp+90h]
  +04c8:  mov [rsp+28h],rax
  +04cd:  mov rax,rbx
  +04d0:  mov [rsp+30h],rax
  +04d5:  mov rax,1
  +04df:  mov [rsp+38h],rax
  +04e4:  mov rax,7
  +04ee:  mov [rsp+40h],rax
  +04f3:  mov rax,rsp
  +04f6:  mov [rsp+48h],rax
  +04fb:  mov rax,[rsp+0B8h]
  +0503:  mov [rsp+50h],rax
  +0508:  lea rdi,[rsp+38h]
  +0510:  mov rax,7FA60D724000h    ; native precise deopt veneer
  +051a:  call rax
  +051c:  lea rsp,[rsp+60h]
  +0524:  lea rsp,[rsp+78h]
  +052c:  pop rbx
  +052d:  pop rbp
  +052e:  ret
  +052f:  sub rsp,60h
  +0536:  test byte [rsp],0
  +053a:  mov rax,8EDh
  +0544:  mov [rsp],rax
  +0548:  mov rax,0Eh
  +0552:  mov [rsp+8],rax
  +0557:  mov rax,1
  +0561:  mov [rsp+10h],rax
  +0566:  mov rax,2
  +0570:  mov [rsp+18h],rax
  +0575:  mov rax,17h
  +057f:  mov [rsp+20h],rax
  +0584:  mov rax,[rsp+90h]
  +058c:  mov [rsp+28h],rax
  +0591:  mov rax,[rsp+0A0h]
  +0599:  mov [rsp+30h],rax
  +059e:  mov rax,1
  +05a8:  mov [rsp+38h],rax
  +05ad:  mov rax,7
  +05b7:  mov [rsp+40h],rax
  +05bc:  mov rax,rsp
  +05bf:  mov [rsp+48h],rax
  +05c4:  mov rax,[rsp+0B8h]
  +05cc:  mov [rsp+50h],rax
  +05d1:  lea rdi,[rsp+38h]
  +05d9:  mov rax,7FA60D724000h    ; native precise deopt veneer
  +05e3:  call rax
  +05e5:  lea rsp,[rsp+60h]
  +05ed:  lea rsp,[rsp+78h]
  +05f5:  pop rbx
  +05f6:  pop rbp
  +05f7:  ret
  +05f8:  sub rsp,50h
  +05ff:  test byte [rsp],0
  +0603:  mov rax,8EDh
  +060d:  mov [rsp],rax
  +0611:  mov rax,0
  +061b:  mov [rsp+8],rax
  +0620:  mov rax,1
  +062a:  mov [rsp+10h],rax
  +062f:  mov rax,0
  +0639:  mov [rsp+18h],rax
  +063e:  mov rax,rsi
  +0641:  mov [rsp+20h],rax
  +0646:  mov rax,1
  +0650:  mov [rsp+28h],rax
  +0655:  mov rax,5
  +065f:  mov [rsp+30h],rax
  +0664:  mov rax,rsp
  +0667:  mov [rsp+38h],rax
  +066c:  mov rax,[rsp+0A8h]
  +0674:  mov [rsp+40h],rax
  +0679:  lea rdi,[rsp+28h]
  +0681:  mov rax,7FA60D724000h    ; native precise deopt veneer
  +068b:  call rax
  +068d:  lea rsp,[rsp+50h]
  +0695:  lea rsp,[rsp+78h]
  +069d:  pop rbx
  +069e:  pop rbp
  +069f:  ret
  +06a0:  sub rsp,60h
  +06a7:  test byte [rsp],0
  +06ab:  mov rax,8EDh
  +06b5:  mov [rsp],rax
  +06b9:  mov rax,2
  +06c3:  mov [rsp+8],rax
  +06c8:  mov rax,1
  +06d2:  mov [rsp+10h],rax
  +06d7:  mov rax,2
  +06e1:  mov [rsp+18h],rax
  +06e6:  mov rax,[rsp+88h]
  +06ee:  mov [rsp+20h],rax
  +06f3:  mov rax,[rsp+88h]
  +06fb:  mov [rsp+28h],rax
  +0700:  mov rax,10h
  +070a:  mov [rsp+30h],rax
  +070f:  mov rax,1
  +0719:  mov [rsp+38h],rax
  +071e:  mov rax,7
  +0728:  mov [rsp+40h],rax
  +072d:  mov rax,rsp
  +0730:  mov [rsp+48h],rax
  +0735:  mov rax,[rsp+0B8h]
  +073d:  mov [rsp+50h],rax
  +0742:  lea rdi,[rsp+38h]
  +074a:  mov rax,7FA60D724000h    ; native precise deopt veneer
  +0754:  call rax
  +0756:  lea rsp,[rsp+60h]
  +075e:  lea rsp,[rsp+78h]
  +0766:  pop rbx
  +0767:  pop rbp
  +0768:  ret
  +0769:  sub rsp,60h
  +0770:  test byte [rsp],0
  +0774:  mov rax,8EDh
  +077e:  mov [rsp],rax
  +0782:  mov rax,8
  +078c:  mov [rsp+8],rax
  +0791:  mov rax,1
  +079b:  mov [rsp+10h],rax
  +07a0:  mov rax,2
  +07aa:  mov [rsp+18h],rax
  +07af:  mov rax,[rsp+88h]
  +07b7:  mov [rsp+20h],rax
  +07bc:  mov rax,[rsp+88h]
  +07c4:  mov [rsp+28h],rax
  +07c9:  mov rax,8
  +07d3:  mov [rsp+30h],rax
  +07d8:  mov rax,1
  +07e2:  mov [rsp+38h],rax
  +07e7:  mov rax,7
  +07f1:  mov [rsp+40h],rax
  +07f6:  mov rax,rsp
  +07f9:  mov [rsp+48h],rax
  +07fe:  mov rax,[rsp+0B8h]
  +0806:  mov [rsp+50h],rax
  +080b:  lea rdi,[rsp+38h]
  +0813:  mov rax,7FA60D724000h    ; native precise deopt veneer
  +081d:  call rax
  +081f:  lea rsp,[rsp+60h]
  +0827:  lea rsp,[rsp+78h]
  +082f:  pop rbx
  +0830:  pop rbp
  +0831:  ret
  +0832:  sub rsp,60h
  +0839:  test byte [rsp],0
  +083d:  mov rax,8EDh
  +0847:  mov [rsp],rax
  +084b:  mov rax,0Ch
  +0855:  mov [rsp+8],rax
  +085a:  mov rax,1
  +0864:  mov [rsp+10h],rax
  +0869:  mov rax,3
  +0873:  mov [rsp+18h],rax
  +0878:  mov rax,17h
  +0882:  mov [rsp+20h],rax
  +0887:  mov rax,[rsp+68h]
  +088c:  mov [rsp+28h],rax
  +0891:  mov rax,[rsp+88h]
  +0899:  mov [rsp+30h],rax
  +089e:  mov rax,10h
  +08a8:  mov [rsp+38h],rax
  +08ad:  mov rax,1
  +08b7:  mov [rsp+40h],rax
  +08bc:  mov rax,8
  +08c6:  mov [rsp+48h],rax
  +08cb:  mov rax,rsp
  +08ce:  mov [rsp+50h],rax
  +08d3:  mov rax,[rsp+0B8h]
  +08db:  mov [rsp+58h],rax
  +08e0:  lea rdi,[rsp+40h]
  +08e8:  mov rax,7FA60D724000h    ; native precise deopt veneer
  +08f2:  call rax
  +08f4:  lea rsp,[rsp+60h]
  +08fc:  lea rsp,[rsp+78h]
  +0904:  pop rbx
  +0905:  pop rbp
  +0906:  ret
