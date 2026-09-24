//! Proof that JitBuffer can hold and execute machine code.
use torcl_rt::jit::JitBuffer;

#[cfg(target_arch = "x86_64")]
#[test]
fn jit_executes_constant_return() {
    // mov eax, 42 ; ret   →  B8 2A 00 00 00 C3
    let code = [0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3];
    let buf = JitBuffer::new(&code).expect("mmap executable");
    let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    assert_eq!(f(), 42);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn jit_executes_add_two_args() {
    // SysV: arg0=rdi, arg1=rsi, ret=rax.
    // mov rax, rdi ; add rax, rsi ; ret
    //   48 89 F8      48 01 F0      C3
    let code = [0x48, 0x89, 0xF8, 0x48, 0x01, 0xF0, 0xC3];
    let buf = JitBuffer::new(&code).expect("mmap executable");
    let f: extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    assert_eq!(f(20, 22), 42);
    assert_eq!(f(100, 1), 101);
}
