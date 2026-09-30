//! Linux `perf inject --jit` jitdump emission for installed native code.
//!
//! The test drives the real binary with `EGCL_PERF_JITDUMP` pointing at an
//! isolated file, then checks the jitdump header and first code-load record.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const WARM: &str = "\
    (defun sq (x) (* x x)) \
    (sq 2) (sq 3) (sq 4) \
    (format t \"done~%\")";

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(bytes[at..at + 4].try_into().expect("u32 field"))
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_ne_bytes(bytes[at..at + 8].try_into().expect("u64 field"))
}

#[test]
fn jitdump_writes_header_and_code_load_record() {
    let dir = std::env::temp_dir().join(format!("egcl-jitdump-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let dump_path = dir.join("jit.dump");

    let out = Command::new(BIN)
        .args(["--eval", WARM])
        .env("EGCL_T0_T1_THRESHOLD", "2")
        // Needs native code to emit a jitdump record; pin eager so `sq` compiles
        // at definition rather than deferring under the lazy default.
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_PERF_JITDUMP", &dump_path)
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let dump = std::fs::read(&dump_path).expect("jitdump file written");
    assert!(dump.len() >= 40, "jitdump header must be present");
    assert_eq!(read_u32(&dump, 0), 0x4A695444, "jitdump magic");
    assert_eq!(read_u32(&dump, 4), 1, "jitdump version");
    let header_size = read_u32(&dump, 8) as usize;
    assert!(header_size >= 40, "header size is plausible");
    assert!(
        dump.len() > header_size + 16,
        "code-load record must follow header"
    );

    let mut at = header_size;
    let mut saw_sq = false;
    while at + 56 <= dump.len() {
        assert_eq!(read_u32(&dump, at), 0, "record is JIT_CODE_LOAD");
        let record_size = read_u32(&dump, at + 4) as usize;
        assert!(
            record_size > 56,
            "record includes fixed fields, name, and code"
        );
        assert!(at + record_size <= dump.len(), "record size stays in file");
        assert!(read_u64(&dump, at + 32) > 0, "code address is non-zero");
        assert!(read_u64(&dump, at + 40) > 0, "code size is non-zero");

        let payload = &dump[at + 56..at + record_size];
        let nul = payload
            .iter()
            .position(|&b| b == 0)
            .expect("record contains a nul-terminated name");
        let name = std::str::from_utf8(&payload[..nul]).expect("utf8 name");
        assert!(
            payload.len() > nul + 1,
            "record includes code bytes after name"
        );
        saw_sq |= name.contains("SQ");
        at += record_size;
    }
    assert!(saw_sq, "SQ function name is recorded in a code-load record");

    std::fs::remove_dir_all(&dir).ok();
}
