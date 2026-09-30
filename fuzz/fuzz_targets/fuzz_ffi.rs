use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = data.first().copied().unwrap_or_default();
    let _ = "type-tag + value bytes";
    let _ = egcl_rt::ffi::call_foreign as usize;
});
