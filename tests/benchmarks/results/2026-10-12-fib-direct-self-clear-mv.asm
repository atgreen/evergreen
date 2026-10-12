
/tmp/egcl-fib-parity/egcl-after-35a2185f:     file format elf64-x86-64


Disassembly of section .init:

Disassembly of section .plt:

Disassembly of section .plt.got:

Disassembly of section .text:

00000000002d5370 <egcl::cli::bytecode::c2i_clear_mv>:
  2d5370:	55                   	push   %rbp
  2d5371:	41 57                	push   %r15
  2d5373:	41 56                	push   %r14
  2d5375:	41 55                	push   %r13
  2d5377:	41 54                	push   %r12
  2d5379:	53                   	push   %rbx
  2d537a:	48 83 ec 38          	sub    $0x38,%rsp
  2d537e:	48 8d 05 fb b7 6c 00 	lea    0x6cb7fb(%rip),%rax        # 9a0b80 <egcl::cli::bytecode::NATIVE_ENV>
  2d5385:	48 89 44 24 28       	mov    %rax,0x28(%rsp)
  2d538a:	ff 15 90 88 6c 00    	call   *0x6c8890(%rip)        # 99dc20 <_GLOBAL_OFFSET_TABLE_+0x13b0>
  2d5390:	48 85 c0             	test   %rax,%rax
  2d5393:	74 0c                	je     2d53a1 <egcl::cli::bytecode::c2i_clear_mv+0x31>
  2d5395:	48 8b 58 60          	mov    0x60(%rax),%rbx
  2d5399:	41 be 01 00 00 00    	mov    $0x1,%r14d
  2d539f:	eb 03                	jmp    2d53a4 <egcl::cli::bytecode::c2i_clear_mv+0x34>
  2d53a1:	45 31 f6             	xor    %r14d,%r14d
  2d53a4:	48 8b 3d 05 b8 6c 00 	mov    0x6cb805(%rip),%rdi        # 9a0bb0 <egcl::cli::bytecode::NATIVE_ENV+0x30>
  2d53ab:	4c 89 f6             	mov    %r14,%rsi
  2d53ae:	48 89 da             	mov    %rbx,%rdx
  2d53b1:	ff 15 59 89 6c 00    	call   *0x6c8959(%rip)        # 99dd10 <_GLOBAL_OFFSET_TABLE_+0x14a0>
  2d53b7:	a8 01                	test   $0x1,%al
  2d53b9:	74 08                	je     2d53c3 <egcl::cli::bytecode::c2i_clear_mv+0x53>
  2d53bb:	49 89 d7             	mov    %rdx,%r15
  2d53be:	e9 7e 02 00 00       	jmp    2d5641 <egcl::cli::bytecode::c2i_clear_mv+0x2d1>
  2d53c3:	8b 05 ef b7 6c 00    	mov    0x6cb7ef(%rip),%eax        # 9a0bb8 <egcl::cli::bytecode::NATIVE_ENV+0x38>
  2d53c9:	85 c0                	test   %eax,%eax
  2d53cb:	0f 85 96 02 00 00    	jne    2d5667 <egcl::cli::bytecode::c2i_clear_mv+0x2f7>
  2d53d1:	4d 85 f6             	test   %r14,%r14
  2d53d4:	74 08                	je     2d53de <egcl::cli::bytecode::c2i_clear_mv+0x6e>
  2d53d6:	45 31 e4             	xor    %r12d,%r12d
  2d53d9:	49 89 df             	mov    %rbx,%r15
  2d53dc:	eb 53                	jmp    2d5431 <egcl::cli::bytecode::c2i_clear_mv+0xc1>
  2d53de:	48 c7 c0 d8 ff ff ff 	mov    $0xffffffffffffffd8,%rax
  2d53e5:	64 48 8b 00          	mov    %fs:(%rax),%rax
  2d53e9:	48 83 f8 02          	cmp    $0x2,%rax
  2d53ed:	0f 86 62 03 00 00    	jbe    2d5755 <egcl::cli::bytecode::c2i_clear_mv+0x3e5>
  2d53f3:	f0 48 ff 40 f0       	lock incq -0x10(%rax)
  2d53f8:	0f 8e 55 03 00 00    	jle    2d5753 <egcl::cli::bytecode::c2i_clear_mv+0x3e3>
  2d53fe:	48 83 c0 f0          	add    $0xfffffffffffffff0,%rax
  2d5402:	48 89 44 24 10       	mov    %rax,0x10(%rsp)
  2d5407:	4c 8b 78 10          	mov    0x10(%rax),%r15
  2d540b:	f0 48 ff 08          	lock decq (%rax)
  2d540f:	75 0b                	jne    2d541c <egcl::cli::bytecode::c2i_clear_mv+0xac>
  2d5411:	48 8d 7c 24 10       	lea    0x10(%rsp),%rdi
  2d5416:	ff 15 ec a5 6c 00    	call   *0x6ca5ec(%rip)        # 99fa08 <_GLOBAL_OFFSET_TABLE_+0x3198>
  2d541c:	41 bc 01 00 00 00    	mov    $0x1,%r12d
  2d5422:	64 80 3c 25 18 fd ff 	cmpb   $0x0,%fs:0xfffffffffffffd18
  2d5429:	ff 00 
  2d542b:	0f 85 71 02 00 00    	jne    2d56a2 <egcl::cli::bytecode::c2i_clear_mv+0x332>
  2d5431:	b9 01 00 00 00       	mov    $0x1,%ecx
  2d5436:	31 c0                	xor    %eax,%eax
  2d5438:	f0 0f b1 0d 48 b7 6c 	lock cmpxchg %ecx,0x6cb748(%rip)        # 9a0b88 <egcl::cli::bytecode::NATIVE_ENV+0x8>
  2d543f:	00 
  2d5440:	0f 85 7a 02 00 00    	jne    2d56c0 <egcl::cli::bytecode::c2i_clear_mv+0x350>
  2d5446:	48 8d 05 43 1f 6d 00 	lea    0x6d1f43(%rip),%rax        # 9a7390 <std::panicking::panic_count::GLOBAL_PANIC_COUNT>
  2d544d:	48 8b 00             	mov    (%rax),%rax
  2d5450:	48 d1 e0             	shl    $1,%rax
  2d5453:	48 85 c0             	test   %rax,%rax
  2d5456:	0f 85 76 02 00 00    	jne    2d56d2 <egcl::cli::bytecode::c2i_clear_mv+0x362>
  2d545c:	31 c0                	xor    %eax,%eax
  2d545e:	0f b6 0d 27 b7 6c 00 	movzbl 0x6cb727(%rip),%ecx        # 9a0b8c <egcl::cli::bytecode::NATIVE_ENV+0xc>
  2d5465:	84 c9                	test   %cl,%cl
  2d5467:	0f 85 7c 02 00 00    	jne    2d56e9 <egcl::cli::bytecode::c2i_clear_mv+0x379>
  2d546d:	89 44 24 0c          	mov    %eax,0xc(%rsp)
  2d5471:	4c 89 e7             	mov    %r12,%rdi
  2d5474:	4c 89 fe             	mov    %r15,%rsi
  2d5477:	e8 f4 ed e8 ff       	call   164270 <core::hash::BuildHasher::hash_one>
  2d547c:	48 89 c5             	mov    %rax,%rbp
  2d547f:	48 c1 ed 39          	shr    $0x39,%rbp
  2d5483:	48 8b 15 0e b7 6c 00 	mov    0x6cb70e(%rip),%rdx        # 9a0b98 <egcl::cli::bytecode::NATIVE_ENV+0x18>
  2d548a:	48 8b 0d ff b6 6c 00 	mov    0x6cb6ff(%rip),%rcx        # 9a0b90 <egcl::cli::bytecode::NATIVE_ENV+0x10>
  2d5491:	66 0f 6e c5          	movd   %ebp,%xmm0
  2d5495:	66 0f 60 c0          	punpcklbw %xmm0,%xmm0
  2d5499:	f2 0f 70 c0 00       	pshuflw $0x0,%xmm0,%xmm0
  2d549e:	66 0f 70 c0 44       	pshufd $0x44,%xmm0,%xmm0
  2d54a3:	31 f6                	xor    %esi,%esi
  2d54a5:	66 0f 76 c9          	pcmpeqd %xmm1,%xmm1
  2d54a9:	48 89 c7             	mov    %rax,%rdi
  2d54ac:	48 21 d7             	and    %rdx,%rdi
  2d54af:	f3 0f 6f 14 39       	movdqu (%rcx,%rdi,1),%xmm2
  2d54b4:	66 0f 6f da          	movdqa %xmm2,%xmm3
  2d54b8:	66 0f 74 d8          	pcmpeqb %xmm0,%xmm3
  2d54bc:	66 44 0f d7 c3       	pmovmskb %xmm3,%r8d
  2d54c1:	45 85 c0             	test   %r8d,%r8d
  2d54c4:	74 2d                	je     2d54f3 <egcl::cli::bytecode::c2i_clear_mv+0x183>
  2d54c6:	f3 45 0f bc c8       	tzcnt  %r8d,%r9d
  2d54cb:	49 01 f9             	add    %rdi,%r9
  2d54ce:	49 21 d1             	and    %rdx,%r9
  2d54d1:	49 f7 d9             	neg    %r9
  2d54d4:	4f 8d 0c 49          	lea    (%r9,%r9,2),%r9
  2d54d8:	46 39 64 c9 e8       	cmp    %r12d,-0x18(%rcx,%r9,8)
  2d54dd:	75 07                	jne    2d54e6 <egcl::cli::bytecode::c2i_clear_mv+0x176>
  2d54df:	4e 39 7c c9 f0       	cmp    %r15,-0x10(%rcx,%r9,8)
  2d54e4:	74 28                	je     2d550e <egcl::cli::bytecode::c2i_clear_mv+0x19e>
  2d54e6:	45 8d 48 ff          	lea    -0x1(%r8),%r9d
  2d54ea:	66 45 21 c1          	and    %r8w,%r9w
  2d54ee:	45 89 c8             	mov    %r9d,%r8d
  2d54f1:	75 d3                	jne    2d54c6 <egcl::cli::bytecode::c2i_clear_mv+0x156>
  2d54f3:	66 0f 74 d1          	pcmpeqb %xmm1,%xmm2
  2d54f7:	66 44 0f d7 c2       	pmovmskb %xmm2,%r8d
  2d54fc:	45 85 c0             	test   %r8d,%r8d
  2d54ff:	75 25                	jne    2d5526 <egcl::cli::bytecode::c2i_clear_mv+0x1b6>
  2d5501:	48 01 f7             	add    %rsi,%rdi
  2d5504:	48 83 c7 10          	add    $0x10,%rdi
  2d5508:	48 83 c6 10          	add    $0x10,%rsi
  2d550c:	eb 9e                	jmp    2d54ac <egcl::cli::bytecode::c2i_clear_mv+0x13c>
  2d550e:	4a 8d 14 c9          	lea    (%rcx,%r9,8),%rdx
  2d5512:	4c 8b 7a f8          	mov    -0x8(%rdx),%r15
  2d5516:	80 7c 24 0c 00       	cmpb   $0x0,0xc(%rsp)
  2d551b:	0f 84 dc 00 00 00    	je     2d55fd <egcl::cli::bytecode::c2i_clear_mv+0x28d>
  2d5521:	e9 f4 00 00 00       	jmp    2d561a <egcl::cli::bytecode::c2i_clear_mv+0x2aa>
  2d5526:	4c 89 7c 24 20       	mov    %r15,0x20(%rsp)
  2d552b:	49 89 c7             	mov    %rax,%r15
  2d552e:	48 83 3d 6a b6 6c 00 	cmpq   $0x0,0x6cb66a(%rip)        # 9a0ba0 <egcl::cli::bytecode::NATIVE_ENV+0x20>
  2d5535:	00 
  2d5536:	0f 84 40 02 00 00    	je     2d577c <egcl::cli::bytecode::c2i_clear_mv+0x40c>
  2d553c:	ff 15 3e b6 6c 00    	call   *0x6cb63e(%rip)        # 9a0b80 <egcl::cli::bytecode::NATIVE_ENV>
  2d5542:	49 89 c5             	mov    %rax,%r13
  2d5545:	bf 08 00 00 00       	mov    $0x8,%edi
  2d554a:	be 08 00 00 00       	mov    $0x8,%esi
  2d554f:	e8 9c 3e 1a 00       	call   4793f0 <<egcl::tcache::ThreadCached as core::alloc::global::GlobalAlloc>::alloc>
  2d5554:	48 85 c0             	test   %rax,%rax
  2d5557:	0f 84 e6 01 00 00    	je     2d5743 <egcl::cli::bytecode::c2i_clear_mv+0x3d3>
  2d555d:	4c 89 28             	mov    %r13,(%rax)
  2d5560:	48 8b 0d 29 b6 6c 00 	mov    0x6cb629(%rip),%rcx        # 9a0b90 <egcl::cli::bytecode::NATIVE_ENV+0x10>
  2d5567:	48 8b 15 2a b6 6c 00 	mov    0x6cb62a(%rip),%rdx        # 9a0b98 <egcl::cli::bytecode::NATIVE_ENV+0x18>
  2d556e:	49 21 d7             	and    %rdx,%r15
  2d5571:	f3 42 0f 6f 04 39    	movdqu (%rcx,%r15,1),%xmm0
  2d5577:	66 0f d7 f8          	pmovmskb %xmm0,%edi
  2d557b:	85 ff                	test   %edi,%edi
  2d557d:	0f 84 15 02 00 00    	je     2d5798 <egcl::cli::bytecode::c2i_clear_mv+0x428>
  2d5583:	4c 8b 4c 24 20       	mov    0x20(%rsp),%r9
  2d5588:	f3 0f bc ff          	tzcnt  %edi,%edi
  2d558c:	4c 01 ff             	add    %r15,%rdi
  2d558f:	48 21 d7             	and    %rdx,%rdi
  2d5592:	0f b6 34 39          	movzbl (%rcx,%rdi,1),%esi
  2d5596:	40 84 f6             	test   %sil,%sil
  2d5599:	0f 89 22 02 00 00    	jns    2d57c1 <egcl::cli::bytecode::c2i_clear_mv+0x451>
  2d559f:	4c 8d 47 f0          	lea    -0x10(%rdi),%r8
  2d55a3:	49 21 d0             	and    %rdx,%r8
  2d55a6:	40 88 2c 39          	mov    %bpl,(%rcx,%rdi,1)
  2d55aa:	42 88 6c 01 10       	mov    %bpl,0x10(%rcx,%r8,1)
  2d55af:	48 f7 df             	neg    %rdi
  2d55b2:	48 8d 3c 7f          	lea    (%rdi,%rdi,2),%rdi
  2d55b6:	48 8d 14 f9          	lea    (%rcx,%rdi,8),%rdx
  2d55ba:	40 80 e6 01          	and    $0x1,%sil
  2d55be:	4c 89 64 f9 e8       	mov    %r12,-0x18(%rcx,%rdi,8)
  2d55c3:	4c 89 4c f9 f0       	mov    %r9,-0x10(%rcx,%rdi,8)
  2d55c8:	48 89 44 f9 f8       	mov    %rax,-0x8(%rcx,%rdi,8)
  2d55cd:	f3 0f 6f 05 cb b5 6c 	movdqu 0x6cb5cb(%rip),%xmm0        # 9a0ba0 <egcl::cli::bytecode::NATIVE_ENV+0x20>
  2d55d4:	00 
  2d55d5:	40 0f b6 c6          	movzbl %sil,%eax
  2d55d9:	66 0f 6e c8          	movd   %eax,%xmm1
  2d55dd:	66 0f c6 0d 4a d0 50 	shufpd $0x2,0x50d04a(%rip),%xmm1        # 7e2630 <anon.d0e348410202043700075751da0d75a7.32.llvm.11594112987551283523+0x10>
  2d55e4:	00 02 
  2d55e6:	66 0f fb c1          	psubq  %xmm1,%xmm0
  2d55ea:	f3 0f 7f 05 ae b5 6c 	movdqu %xmm0,0x6cb5ae(%rip)        # 9a0ba0 <egcl::cli::bytecode::NATIVE_ENV+0x20>
  2d55f1:	00 
  2d55f2:	4c 8b 7a f8          	mov    -0x8(%rdx),%r15
  2d55f6:	80 7c 24 0c 00       	cmpb   $0x0,0xc(%rsp)
  2d55fb:	75 1d                	jne    2d561a <egcl::cli::bytecode::c2i_clear_mv+0x2aa>
  2d55fd:	48 8d 05 8c 1d 6d 00 	lea    0x6d1d8c(%rip),%rax        # 9a7390 <std::panicking::panic_count::GLOBAL_PANIC_COUNT>
  2d5604:	48 8b 00             	mov    (%rax),%rax
  2d5607:	48 b9 ff ff ff ff ff 	movabs $0x7fffffffffffffff,%rcx
  2d560e:	ff ff 7f 
  2d5611:	48 85 c8             	test   %rcx,%rax
  2d5614:	0f 85 48 01 00 00    	jne    2d5762 <egcl::cli::bytecode::c2i_clear_mv+0x3f2>
  2d561a:	31 c0                	xor    %eax,%eax
  2d561c:	87 05 66 b5 6c 00    	xchg   %eax,0x6cb566(%rip)        # 9a0b88 <egcl::cli::bytecode::NATIVE_ENV+0x8>
  2d5622:	83 f8 02             	cmp    $0x2,%eax
  2d5625:	0f 84 f5 00 00 00    	je     2d5720 <egcl::cli::bytecode::c2i_clear_mv+0x3b0>
  2d562b:	48 8b 3d 7e b5 6c 00 	mov    0x6cb57e(%rip),%rdi        # 9a0bb0 <egcl::cli::bytecode::NATIVE_ENV+0x30>
  2d5632:	4c 89 f6             	mov    %r14,%rsi
  2d5635:	48 89 da             	mov    %rbx,%rdx
  2d5638:	4c 89 f9             	mov    %r15,%rcx
  2d563b:	ff 15 ef a4 6c 00    	call   *0x6ca4ef(%rip)        # 99fb30 <_GLOBAL_OFFSET_TABLE_+0x32c0>
  2d5641:	49 8b 07             	mov    (%r15),%rax
  2d5644:	48 85 c0             	test   %rax,%rax
  2d5647:	74 0f                	je     2d5658 <egcl::cli::bytecode::c2i_clear_mv+0x2e8>
  2d5649:	48 c7 40 70 00 00 00 	movq   $0x0,0x70(%rax)
  2d5650:	00 
  2d5651:	c6 80 59 01 00 00 00 	movb   $0x0,0x159(%rax)
  2d5658:	48 83 c4 38          	add    $0x38,%rsp
  2d565c:	5b                   	pop    %rbx
  2d565d:	41 5c                	pop    %r12
  2d565f:	41 5d                	pop    %r13
  2d5661:	41 5e                	pop    %r14
  2d5663:	41 5f                	pop    %r15
  2d5665:	5d                   	pop    %rbp
  2d5666:	c3                   	ret
  2d5667:	48 8d 44 24 28       	lea    0x28(%rsp),%rax
  2d566c:	48 89 44 24 30       	mov    %rax,0x30(%rsp)
  2d5671:	48 8d 44 24 30       	lea    0x30(%rsp),%rax
  2d5676:	48 89 44 24 10       	mov    %rax,0x10(%rsp)
  2d567b:	48 8d 3d 36 b5 6c 00 	lea    0x6cb536(%rip),%rdi        # 9a0bb8 <egcl::cli::bytecode::NATIVE_ENV+0x38>
  2d5682:	48 8d 0d 87 ee 69 00 	lea    0x69ee87(%rip),%rcx        # 974510 <anon.d0e348410202043700075751da0d75a7.251.llvm.11594112987551283523+0x3790>
  2d5689:	4c 8d 05 d8 b2 6a 00 	lea    0x6ab2d8(%rip),%r8        # 980968 <anon.d0e348410202043700075751da0d75a7.251.llvm.11594112987551283523+0xfbe8>
  2d5690:	48 8d 54 24 10       	lea    0x10(%rsp),%rdx
  2d5695:	31 f6                	xor    %esi,%esi
  2d5697:	ff 15 fb a0 6c 00    	call   *0x6ca0fb(%rip)        # 99f798 <_GLOBAL_OFFSET_TABLE_+0x2f28>
  2d569d:	e9 2f fd ff ff       	jmp    2d53d1 <egcl::cli::bytecode::c2i_clear_mv+0x61>
  2d56a2:	48 8d 3d 41 d7 56 00 	lea    0x56d741(%rip),%rdi        # 842dea <core::unicode::unicode_data::white_space::WHITESPACE_MAP+0x513f5>
  2d56a9:	48 8d 15 e8 b2 6a 00 	lea    0x6ab2e8(%rip),%rdx        # 980998 <anon.d0e348410202043700075751da0d75a7.251.llvm.11594112987551283523+0xfc18>
  2d56b0:	be 5d 00 00 00       	mov    $0x5d,%esi
  2d56b5:	ff 15 7d 8d 6c 00    	call   *0x6c8d7d(%rip)        # 99e438 <_GLOBAL_OFFSET_TABLE_+0x1bc8>
  2d56bb:	e9 93 00 00 00       	jmp    2d5753 <egcl::cli::bytecode::c2i_clear_mv+0x3e3>
  2d56c0:	48 8d 3d c1 b4 6c 00 	lea    0x6cb4c1(%rip),%rdi        # 9a0b88 <egcl::cli::bytecode::NATIVE_ENV+0x8>
  2d56c7:	ff 15 03 82 6c 00    	call   *0x6c8203(%rip)        # 99d8d0 <_GLOBAL_OFFSET_TABLE_+0x1060>
  2d56cd:	e9 74 fd ff ff       	jmp    2d5446 <egcl::cli::bytecode::c2i_clear_mv+0xd6>
  2d56d2:	ff 15 e8 93 6c 00    	call   *0x6c93e8(%rip)        # 99eac0 <_GLOBAL_OFFSET_TABLE_+0x2250>
  2d56d8:	34 01                	xor    $0x1,%al
  2d56da:	0f b6 0d ab b4 6c 00 	movzbl 0x6cb4ab(%rip),%ecx        # 9a0b8c <egcl::cli::bytecode::NATIVE_ENV+0xc>
  2d56e1:	84 c9                	test   %cl,%cl
  2d56e3:	0f 84 84 fd ff ff    	je     2d546d <egcl::cli::bytecode::c2i_clear_mv+0xfd>
  2d56e9:	48 8d 0d 98 b4 6c 00 	lea    0x6cb498(%rip),%rcx        # 9a0b88 <egcl::cli::bytecode::NATIVE_ENV+0x8>
  2d56f0:	48 89 4c 24 10       	mov    %rcx,0x10(%rsp)
  2d56f5:	88 44 24 18          	mov    %al,0x18(%rsp)
  2d56f9:	48 8d 3d ea f1 51 00 	lea    0x51f1ea(%rip),%rdi        # 7f48ea <core::unicode::unicode_data::white_space::WHITESPACE_MAP+0x2ef5>
  2d5700:	48 8d 0d 29 15 6a 00 	lea    0x6a1529(%rip),%rcx        # 976c30 <anon.d0e348410202043700075751da0d75a7.251.llvm.11594112987551283523+0x5eb0>
  2d5707:	4c 8d 05 72 b2 6a 00 	lea    0x6ab272(%rip),%r8        # 980980 <anon.d0e348410202043700075751da0d75a7.251.llvm.11594112987551283523+0xfc00>
  2d570e:	48 8d 54 24 10       	lea    0x10(%rsp),%rdx
  2d5713:	be 2b 00 00 00       	mov    $0x2b,%esi
  2d5718:	ff 15 92 9b 6c 00    	call   *0x6c9b92(%rip)        # 99f2b0 <_GLOBAL_OFFSET_TABLE_+0x2a40>
  2d571e:	eb 33                	jmp    2d5753 <egcl::cli::bytecode::c2i_clear_mv+0x3e3>
  2d5720:	48 8d 35 61 b4 6c 00 	lea    0x6cb461(%rip),%rsi        # 9a0b88 <egcl::cli::bytecode::NATIVE_ENV+0x8>
  2d5727:	bf ca 00 00 00       	mov    $0xca,%edi
  2d572c:	ba 81 00 00 00       	mov    $0x81,%edx
  2d5731:	b9 01 00 00 00       	mov    $0x1,%ecx
  2d5736:	31 c0                	xor    %eax,%eax
  2d5738:	ff 15 02 9e 6c 00    	call   *0x6c9e02(%rip)        # 99f540 <_GLOBAL_OFFSET_TABLE_+0x2cd0>
  2d573e:	e9 e8 fe ff ff       	jmp    2d562b <egcl::cli::bytecode::c2i_clear_mv+0x2bb>
  2d5743:	bf 08 00 00 00       	mov    $0x8,%edi
  2d5748:	be 08 00 00 00       	mov    $0x8,%esi
  2d574d:	ff 15 a5 98 6c 00    	call   *0x6c98a5(%rip)        # 99eff8 <_GLOBAL_OFFSET_TABLE_+0x2788>
  2d5753:	0f 0b                	ud2
  2d5755:	48 89 c7             	mov    %rax,%rdi
  2d5758:	e8 53 8e dc ff       	call   9e5b0 <std::thread::current::init_current>
  2d575d:	e9 a0 fc ff ff       	jmp    2d5402 <egcl::cli::bytecode::c2i_clear_mv+0x92>
  2d5762:	ff 15 58 93 6c 00    	call   *0x6c9358(%rip)        # 99eac0 <_GLOBAL_OFFSET_TABLE_+0x2250>
  2d5768:	84 c0                	test   %al,%al
  2d576a:	0f 85 aa fe ff ff    	jne    2d561a <egcl::cli::bytecode::c2i_clear_mv+0x2aa>
  2d5770:	c6 05 15 b4 6c 00 01 	movb   $0x1,0x6cb415(%rip)        # 9a0b8c <egcl::cli::bytecode::NATIVE_ENV+0xc>
  2d5777:	e9 9e fe ff ff       	jmp    2d561a <egcl::cli::bytecode::c2i_clear_mv+0x2aa>
  2d577c:	48 8d 3d 0d b4 6c 00 	lea    0x6cb40d(%rip),%rdi        # 9a0b90 <egcl::cli::bytecode::NATIVE_ENV+0x10>
  2d5783:	be 01 00 00 00       	mov    $0x1,%esi
  2d5788:	b9 01 00 00 00       	mov    $0x1,%ecx
  2d578d:	ff 15 15 76 6c 00    	call   *0x6c7615(%rip)        # 99cda8 <_GLOBAL_OFFSET_TABLE_+0x538>
  2d5793:	e9 a4 fd ff ff       	jmp    2d553c <egcl::cli::bytecode::c2i_clear_mv+0x1cc>
  2d5798:	41 b8 10 00 00 00    	mov    $0x10,%r8d
  2d579e:	4c 8b 4c 24 20       	mov    0x20(%rsp),%r9
  2d57a3:	4d 01 c7             	add    %r8,%r15
  2d57a6:	49 21 d7             	and    %rdx,%r15
  2d57a9:	f3 42 0f 6f 04 39    	movdqu (%rcx,%r15,1),%xmm0
  2d57af:	66 0f d7 f8          	pmovmskb %xmm0,%edi
  2d57b3:	49 83 c0 10          	add    $0x10,%r8
  2d57b7:	85 ff                	test   %edi,%edi
  2d57b9:	0f 85 c9 fd ff ff    	jne    2d5588 <egcl::cli::bytecode::c2i_clear_mv+0x218>
  2d57bf:	eb e2                	jmp    2d57a3 <egcl::cli::bytecode::c2i_clear_mv+0x433>
  2d57c1:	66 0f 6f 01          	movdqa (%rcx),%xmm0
  2d57c5:	66 0f d7 f0          	pmovmskb %xmm0,%esi
  2d57c9:	f3 0f bc fe          	tzcnt  %esi,%edi
  2d57cd:	0f b6 34 39          	movzbl (%rcx,%rdi,1),%esi
  2d57d1:	e9 c9 fd ff ff       	jmp    2d559f <egcl::cli::bytecode::c2i_clear_mv+0x22f>
  2d57d6:	0f b6 74 24 0c       	movzbl 0xc(%rsp),%esi
  2d57db:	48 8d 3d a6 b3 6c 00 	lea    0x6cb3a6(%rip),%rdi        # 9a0b88 <egcl::cli::bytecode::NATIVE_ENV+0x8>
  2d57e2:	e8 29 13 e7 ff       	call   146b10 <core::ptr::drop_in_place<std::sync::poison::mutex::MutexGuard<egcl::cli::bytecode::T2CompileInput>>>
  2d57e7:	eb 18                	jmp    2d5801 <egcl::cli::bytecode::c2i_clear_mv+0x491>
  2d57e9:	ff 15 a1 8f 6c 00    	call   *0x6c8fa1(%rip)        # 99e790 <_GLOBAL_OFFSET_TABLE_+0x1f20>
  2d57ef:	48 8d 7c 24 10       	lea    0x10(%rsp),%rdi
  2d57f4:	e8 d7 52 e7 ff       	call   14aad0 <core::ptr::drop_in_place<std::sync::poison::PoisonError<std::sync::poison::mutex::MutexGuard<std::sync::mpmc::zero::Inner>>>>
  2d57f9:	eb 06                	jmp    2d5801 <egcl::cli::bytecode::c2i_clear_mv+0x491>
  2d57fb:	ff 15 8f 8f 6c 00    	call   *0x6c8f8f(%rip)        # 99e790 <_GLOBAL_OFFSET_TABLE_+0x1f20>
  2d5801:	ff 15 31 8b 6c 00    	call   *0x6c8b31(%rip)        # 99e338 <_GLOBAL_OFFSET_TABLE_+0x1ac8>

Disassembly of section .fini:
