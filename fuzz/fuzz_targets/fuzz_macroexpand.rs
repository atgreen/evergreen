use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = torcl_compiler::reader::read_from_string(source.as_ref());
    let _ = torcl_compiler::macroexpand::macroexpand;
});
