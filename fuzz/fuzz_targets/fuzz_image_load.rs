use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = "corrupt image";
    let _ = torcl_rt::image::load_image(data);
});
