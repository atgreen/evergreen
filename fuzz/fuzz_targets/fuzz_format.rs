use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let control = String::from_utf8_lossy(data);
    let _ = bliss_stdlib::format::format_to_string(control.as_ref(), &[]);
});
