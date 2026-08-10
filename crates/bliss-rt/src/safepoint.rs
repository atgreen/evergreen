//! Safepoint mechanism — polling-page-based cooperative suspension.
//!
//! A single page is mmap'd read-only. Generated code performs a load
//! from this page at every safepoint poll site. When a safepoint is
//! requested, mprotect makes the page PROT_NONE, causing SIGSEGV which
//! the signal handler converts to a safepoint trap. See §2.5, §3.9.

use crate::error::BlissError;

/// Handle to the safepoint page.
pub struct SafepointPage {
    _private: (),
}

impl SafepointPage {
    /// Allocate and map the safepoint page (called once at startup).
    pub fn init() -> Result<Self, BlissError> {
        unimplemented!("SafepointPage::init")
    }

    /// Get the address of the safepoint page (for code generation).
    pub fn address(&self) -> *const u8 {
        unimplemented!("SafepointPage::address")
    }

    /// Request all threads to reach a safepoint by poisoning the page.
    pub fn request_safepoint(&self) -> Result<(), BlissError> {
        unimplemented!("SafepointPage::request_safepoint")
    }

    /// Resume normal operation by restoring the page to readable.
    pub fn resume(&self) -> Result<(), BlissError> {
        unimplemented!("SafepointPage::resume")
    }

    /// Check if a safepoint is currently requested.
    pub fn is_requested(&self) -> bool {
        unimplemented!("SafepointPage::is_requested")
    }
}

/// Inline safepoint poll — generates a load from the polling page.
/// Compiles to a single `test` instruction on x86-64.
#[inline(always)]
pub fn poll_safepoint() {
    unimplemented!("poll_safepoint")
}

/// Enter a safepoint: publish stack state, signal "at safepoint",
/// wait for GC if needed, check yield flag, then resume.
pub fn enter_safepoint() {
    unimplemented!("enter_safepoint")
}

/// Wait until all mutator threads have reached a safepoint.
pub fn wait_for_all_threads() -> Result<(), BlissError> {
    unimplemented!("wait_for_all_threads")
}

/// Resume all threads after a safepoint operation.
pub fn resume_all_threads() -> Result<(), BlissError> {
    unimplemented!("resume_all_threads")
}
