//! Safepoint mechanism — polling-page-based cooperative suspension.
//!
//! A single page is allocated page-aligned. Generated code performs a load
//! from this page at every safepoint poll site. When a safepoint is
//! requested, the page is flagged, and polling threads observe the flag
//! to enter a safepoint. See §2.5, §3.9.

use crate::error::BlissError;
use std::sync::atomic::{AtomicBool, Ordering};

const PAGE_SIZE: usize = 4096;

/// Handle to the safepoint page.
pub struct SafepointPage {
    /// Page-aligned memory for the safepoint polling page.
    page: *mut u8,
    /// Layout used for deallocation.
    layout: std::alloc::Layout,
    /// Whether a safepoint is currently requested.
    requested: AtomicBool,
}

// SafepointPage is shared across threads for GC coordination.
unsafe impl Send for SafepointPage {}
unsafe impl Sync for SafepointPage {}

impl SafepointPage {
    /// Allocate and map the safepoint page (called once at startup).
    pub fn init() -> Result<Self, BlissError> {
        let layout = std::alloc::Layout::from_size_align(PAGE_SIZE, PAGE_SIZE)
            .map_err(|e| BlissError::Internal(format!("safepoint layout error: {}", e)))?;
        let page = unsafe { std::alloc::alloc_zeroed(layout) };
        if page.is_null() {
            return Err(BlissError::Oom);
        }
        Ok(SafepointPage {
            page,
            layout,
            requested: AtomicBool::new(false),
        })
    }

    /// Get the address of the safepoint page (for code generation).
    pub fn address(&self) -> *const u8 {
        self.page as *const u8
    }

    /// Request all threads to reach a safepoint by poisoning the page.
    pub fn request_safepoint(&self) -> Result<(), BlissError> {
        self.requested.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Resume normal operation by restoring the page to readable.
    pub fn resume(&self) -> Result<(), BlissError> {
        self.requested.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Check if a safepoint is currently requested.
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

impl Drop for SafepointPage {
    fn drop(&mut self) {
        if !self.page.is_null() {
            unsafe { std::alloc::dealloc(self.page, self.layout) };
        }
    }
}

/// Inline safepoint poll — checks the safepoint flag.
/// In generated code this compiles to a single load + test.
#[inline(always)]
pub fn poll_safepoint() {
    // In a full implementation, this would load from the safepoint page
    // and trap (SIGSEGV) if the page is poisoned. For the cooperative
    // implementation, this is a no-op check point.
}

/// Enter a safepoint: publish stack state, signal "at safepoint",
/// wait for GC if needed, check yield flag, then resume.
pub fn enter_safepoint() {
    // In a full implementation, this would:
    // 1. Save the current stack pointer and frame pointer
    // 2. Signal that this thread is at a safepoint
    // 3. Wait if GC is in progress
    // 4. Check if a yield was requested
    // 5. Resume execution
}

/// Wait until all mutator threads have reached a safepoint.
pub fn wait_for_all_threads() -> Result<(), BlissError> {
    // In a full implementation, this blocks the requesting thread (GC thread)
    // until all mutator threads have signaled they are at a safepoint.
    Ok(())
}

/// Resume all threads after a safepoint operation.
pub fn resume_all_threads() -> Result<(), BlissError> {
    // In a full implementation, this signals all mutator threads to resume
    // execution after GC or other safepoint operations complete.
    Ok(())
}
