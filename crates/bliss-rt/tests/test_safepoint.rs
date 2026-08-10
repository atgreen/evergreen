use bliss_rt::safepoint::*;

#[test]
fn safepoint_page_init_succeeds() {
    assert!(SafepointPage::init().is_ok());
}

#[test]
fn safepoint_page_address_non_null_and_aligned() {
    let page = SafepointPage::init().unwrap();
    let addr = page.address();
    assert!(!addr.is_null());
    assert_eq!(addr as usize % 4096, 0, "must be page-aligned");
}

#[test]
fn safepoint_initially_not_requested() {
    let page = SafepointPage::init().unwrap();
    assert!(!page.is_requested());
}

#[test]
fn request_then_resume_cycle() {
    let page = SafepointPage::init().unwrap();
    for _ in 0..3 {
        page.request_safepoint().unwrap();
        assert!(page.is_requested());
        page.resume().unwrap();
        assert!(!page.is_requested());
    }
}

#[test]
fn resume_without_request_is_harmless() {
    let page = SafepointPage::init().unwrap();
    assert!(page.resume().is_ok());
}

#[test]
fn wait_and_resume_all_threads() {
    assert!(wait_for_all_threads().is_ok());
    assert!(resume_all_threads().is_ok());
}

#[test]
fn enter_safepoint_does_not_panic() {
    enter_safepoint();
}
