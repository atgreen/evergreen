; disassembly for FIB
; Size: 138 bytes. Origin: #xB800BA2855                       ; FIB
; 55: L0:   8F4508           POP QWORD PTR [RBP+8]
; 58:       488955F0         MOV [RBP-16], RDX
; 5C:       BE04000000       MOV ESI, 4
; 61:       488BFA           MOV RDI, RDX
; 64:       E857EA45FF       CALL #xB8000012C0                ; SB-VM::GENERIC-<
; 69:       488B55F0         MOV RDX, [RBP-16]
; 6D:       7C69             JL L2
; 6F:       488955F0         MOV [RBP-16], RDX
; 73:       BE02000000       MOV ESI, 2
; 78:       488BFA           MOV RDI, RDX
; 7B:       E8E0E845FF       CALL #xB800001160                ; SB-VM::GENERIC--
; 80:       488BC5           MOV RAX, RBP
; 83:       488D4C24F0       LEA RCX, [RSP-16]
; 88:       4883EC20         SUB RSP, 32
; 8C:       488BD7           MOV RDX, RDI
; 8F:       488901           MOV [RCX], RAX
; 92:       488BE9           MOV RBP, RCX
; 95:       E8BBFFFFFF       CALL L0
; 9A:       488B55F0         MOV RDX, [RBP-16]
; 9E:       48897DF8         MOV [RBP-8], RDI
; A2:       BE04000000       MOV ESI, 4
; A7:       488BFA           MOV RDI, RDX
; AA:       E8B1E845FF       CALL #xB800001160                ; SB-VM::GENERIC--
; AF:       488BD7           MOV RDX, RDI
; B2:       488BC5           MOV RAX, RBP
; B5:       488D4C24F0       LEA RCX, [RSP-16]
; BA:       4883EC20         SUB RSP, 32
; BE:       488901           MOV [RCX], RAX
; C1:       488BE9           MOV RBP, RCX
; C4:       E88CFFFFFF       CALL L0
; C9:       488BF7           MOV RSI, RDI
; CC:       488B7DF8         MOV RDI, [RBP-8]
; D0:       E81BE845FF       CALL #xB8000010F0                ; SB-VM::GENERIC-+
; D5: L1:   C9               LEAVE
; D6:       F8               CLC
; D7:       C3               RET
; D8: L2:   488BFA           MOV RDI, RDX
; DB:       EBF8             JMP L1
; DD:       CC0F             INT3 15                          ; Invalid argument count trap
