#[test]
fn foreign_runtime_inhibition_prevents_creating_an_image() {
    let path = std::env::temp_dir().join(format!("torcl-inhibited-{}.bimg", std::process::id()));
    torcl_rt::image::inhibit_saving().unwrap();
    torcl_rt::image::inhibit_saving().unwrap();
    let error = torcl_rt::image::save_image(
        path.to_str().unwrap(),
        &torcl_rt::image::SaveImageOptions {
            executable: false,
            compression: torcl_rt::image::ImageCompression::None,
            purify: false,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("foreign runtime"), "{error}");
    assert!(!path.exists());
}
