// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Shared executable-code arena (bliss-gg3i1.7.10).
//!
//! Most installed code buffers are tiny (helper veneers, call stubs, capture
//! stubs of a few dozen bytes), and giving each one its own page-rounded
//! mapping cost a full page per stub and one kernel mapping per run of them.
//! The arena instead packs buffers into large chunks, each mapped twice over
//! the same shared-memory pages: a read+write view that is never executable
//! and a read+execute view that is never writable. Code is copied in through
//! the writable view and run from the executable one, so no mapping is ever
//! writable and executable at once, and no page's protection changes while
//! other threads may be executing on it.
//!
//! A released buffer's range returns to a coalescing best-fit free list, and
//! pages that become entirely free are handed back to the kernel. Releasing a
//! range is exactly as safe as the `munmap` it replaces: `JitBuffer` is only
//! dropped once no activation, caller or published entry can reach its code.
//!
//! `fork` shares these pages between parent and child. Both sides therefore
//! retire every pre-fork chunk: neither allocates from nor recycles memory the
//! other process may still execute. Chunks created afterwards are private to
//! the process that created them.
//!
//! `EGCL_JIT_ARENA=0` disables the arena; any arena failure also falls back to
//! one mapping per buffer.

use std::cell::UnsafeCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// Bytes mapped per chunk (each mapped twice). Pages are only populated as
/// code is written, so an unfilled chunk costs address space, not memory.
const CHUNK_SIZE: usize = 8 << 20;
/// Allocation granule. Buffers previously started on page boundaries; every
/// alignment an emitter can rely on relative to its start (up to a cache line)
/// is preserved.
pub(super) const GRANULE: usize = 64;
/// Larger buffers keep their own mapping.
const MAX_ARENA_ALLOC: usize = 256 << 10;

struct Chunk {
    rw: usize,
    size: usize,
    retired: bool,
}

#[derive(Default)]
struct State {
    /// Executable-view start → chunk.
    chunks: BTreeMap<usize, Chunk>,
    /// Free executable ranges: (length, start) for best fit…
    free_by_size: BTreeSet<(usize, usize)>,
    /// …and start → length for coalescing.
    free_by_addr: BTreeMap<usize, usize>,
    disabled: bool,
    initialized: bool,
    reserved_bytes: usize,
    released_pages: u64,
}

struct Arena {
    locked: AtomicBool,
    state: UnsafeCell<State>,
}

// SAFETY: `state` is only accessed while `locked` is held.
unsafe impl Sync for Arena {}

static ARENA: Arena = Arena {
    locked: AtomicBool::new(false),
    state: UnsafeCell::new(State {
        chunks: BTreeMap::new(),
        free_by_size: BTreeSet::new(),
        free_by_addr: BTreeMap::new(),
        disabled: false,
        initialized: false,
        reserved_bytes: 0,
        released_pages: 0,
    }),
};

/// A plain spin lock rather than `Mutex`: the `fork` handlers must take it in
/// the parent and release it on both sides of the fork.
fn lock() {
    let mut spins = 0u32;
    while ARENA
        .locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        spins += 1;
        if spins < 64 {
            std::hint::spin_loop();
        } else {
            std::thread::yield_now();
        }
    }
}

fn unlock() {
    ARENA.locked.store(false, Ordering::Release);
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    lock();
    // SAFETY: the lock gives exclusive access; `f` performs no allocation that
    // could re-enter the arena (it only touches Rust collections and syscalls).
    let result = f(unsafe { &mut *ARENA.state.get() });
    unlock();
    result
}

extern "C" fn before_fork() {
    lock();
}

extern "C" fn after_fork() {
    // SAFETY: `before_fork` holds the lock on this thread.
    let state = unsafe { &mut *ARENA.state.get() };
    for chunk in state.chunks.values_mut() {
        chunk.retired = true;
    }
    state.free_by_size.clear();
    state.free_by_addr.clear();
    unlock();
}

fn page_size() -> usize {
    crate::syscall::page_size()
}

impl State {
    fn insert_free(&mut self, start: usize, len: usize) {
        self.free_by_size.insert((len, start));
        self.free_by_addr.insert(start, len);
    }

    fn remove_free(&mut self, start: usize, len: usize) {
        self.free_by_size.remove(&(len, start));
        self.free_by_addr.remove(&start);
    }

    fn chunk_of(&self, rx: usize) -> Option<(usize, &Chunk)> {
        let (&start, chunk) = self.chunks.range(..=rx).next_back()?;
        (rx < start + chunk.size).then_some((start, chunk))
    }

    fn initialize(&mut self) -> bool {
        if !self.initialized {
            self.initialized = true;
            self.disabled = std::env::var("EGCL_JIT_ARENA").is_ok_and(|value| value == "0");
            if !self.disabled {
                // SAFETY: the handlers only take and release the arena lock and
                // mark chunks retired; they allocate nothing.
                if unsafe {
                    libc::pthread_atfork(Some(before_fork), Some(after_fork), Some(after_fork))
                } != 0
                {
                    self.disabled = true;
                }
            }
        }
        !self.disabled
    }

    fn add_chunk(&mut self) -> bool {
        // SAFETY: fresh shared anonymous mapping, then a second view of the
        // same pages (mremap with old_size 0 duplicates a shared mapping).
        unsafe {
            let rw = libc::mmap(
                std::ptr::null_mut(),
                CHUNK_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if rw == libc::MAP_FAILED {
                return false;
            }
            let rx = libc::mremap(rw, 0, CHUNK_SIZE, libc::MREMAP_MAYMOVE);
            if rx == libc::MAP_FAILED {
                libc::munmap(rw, CHUNK_SIZE);
                return false;
            }
            if libc::mprotect(rx, CHUNK_SIZE, libc::PROT_READ | libc::PROT_EXEC) != 0 {
                libc::munmap(rx, CHUNK_SIZE);
                libc::munmap(rw, CHUNK_SIZE);
                return false;
            }
            let rx = rx as usize;
            self.chunks.insert(
                rx,
                Chunk {
                    rw: rw as usize,
                    size: CHUNK_SIZE,
                    retired: false,
                },
            );
            self.insert_free(rx, CHUNK_SIZE);
            self.reserved_bytes += CHUNK_SIZE;
        }
        true
    }

    fn take(&mut self, len: usize) -> Option<usize> {
        let &(free_len, start) = self.free_by_size.range((len, 0)..).next()?;
        self.remove_free(start, free_len);
        if free_len > len {
            self.insert_free(start + len, free_len - len);
        }
        Some(start)
    }
}

/// Copy `code` into the arena. Returns its executable address and the bytes
/// reserved for it, or `None` when the arena is unavailable for this request.
pub(super) fn alloc(code: &[u8]) -> Option<(*mut u8, usize)> {
    if code.is_empty() || code.len() > MAX_ARENA_ALLOC {
        return None;
    }
    let len = code.len().div_ceil(GRANULE) * GRANULE;
    with_state(|state| {
        if !state.initialize() {
            return None;
        }
        let rx = match state.take(len) {
            Some(rx) => rx,
            None => {
                if !state.add_chunk() {
                    state.disabled = true;
                    return None;
                }
                state.take(len)?
            }
        };
        let (start, chunk) = state.chunk_of(rx)?;
        let rw = chunk.rw + (rx - start);
        // SAFETY: `rw..rw+len` is this allocation's private range in the
        // writable view; nothing executes it until the caller publishes it.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), rw as *mut u8, code.len()) };
        Some((rx as *mut u8, len))
    })
}

/// Return a range obtained from [`alloc`]. Its code must be unreachable.
pub(super) fn free(rx: *mut u8, len: usize) {
    with_state(|state| {
        let Some((pages, pages_len)) = state.release(rx as usize, len, page_size()) else {
            return;
        };
        // SAFETY: the pages lie wholly inside a free range of one chunk's
        // writable view; MADV_REMOVE drops their shared backing so both views
        // read zeros until the range is reused.
        let rc = unsafe { libc::madvise(pages as *mut libc::c_void, pages_len, libc::MADV_REMOVE) };
        if rc == 0 {
            state.released_pages += (pages_len / page_size()) as u64;
        }
    });
}

impl State {
    /// Return `rx..rx + len` to the free list. Yields the writable-view range
    /// of any pages that thereby became entirely free.
    fn release(&mut self, rx: usize, len: usize, page: usize) -> Option<(usize, usize)> {
        let (chunk_start, chunk) = self.chunk_of(rx)?;
        if chunk.retired {
            // Possibly still executed by the other side of a fork.
            return None;
        }
        let (chunk_rw, chunk_end) = (chunk.rw, chunk_start + chunk.size);
        let (mut start, mut end) = (rx, rx + len);
        // Chunks can be adjacent in the address space; never coalesce across
        // one, since each has its own writable view.
        if let Some((&prev, &prev_len)) = self.free_by_addr.range(..start).next_back()
            && prev + prev_len == start
            && prev >= chunk_start
        {
            self.remove_free(prev, prev_len);
            start = prev;
        }
        if end < chunk_end
            && let Some(&next_len) = self.free_by_addr.get(&end)
        {
            self.remove_free(end, next_len);
            end += next_len;
        }
        debug_assert!(chunk_start <= start && end <= chunk_end);
        self.insert_free(start, end - start);
        let first = start.next_multiple_of(page).max(rx & !(page - 1));
        let last = (end & !(page - 1)).min((rx + len).next_multiple_of(page));
        (first < last).then(|| (chunk_rw + (first - chunk_start), last - first))
    }
}

/// (chunks, executable bytes reserved, pages returned to the kernel).
pub(super) fn stats() -> (usize, usize, u64) {
    with_state(|state| {
        (
            state.chunks.len(),
            state.reserved_bytes,
            state.released_pages,
        )
    })
}

#[cfg(test)]
mod tests {
    use crate::jit::JitBuffer;

    /// Forking retires every chunk, so tests that expect reuse must not
    /// overlap the fork test.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `mov eax, value; ret`
    fn returns(value: u32) -> Vec<u8> {
        let mut code = vec![0xb8];
        code.extend_from_slice(&value.to_le_bytes());
        code.push(0xc3);
        code
    }

    fn call_at(buffer: &JitBuffer, offset: usize) -> u32 {
        // SAFETY: every buffer here holds `returns(..)` at `offset`.
        let f: extern "C" fn() -> u32 = unsafe { std::mem::transmute(buffer.as_ptr().add(offset)) };
        f()
    }

    #[test]
    fn small_buffers_share_pages_and_execute() {
        let buffers: Vec<_> = (0..512u32)
            .map(|i| JitBuffer::new(&returns(i)).unwrap())
            .collect();
        assert!(buffers.iter().all(|b| b.in_arena));
        for (i, buffer) in buffers.iter().enumerate() {
            assert_eq!(call_at(buffer, 0), i as u32);
            assert_eq!(buffer.as_ptr() as usize % super::GRANULE, 0);
        }
        let page = crate::syscall::page_size();
        let mut pages: Vec<_> = buffers.iter().map(|b| b.as_ptr() as usize / page).collect();
        pages.sort_unstable();
        pages.dedup();
        assert!(pages.len() * 8 <= buffers.len(), "{} pages", pages.len());
    }

    #[test]
    fn released_ranges_are_reused_and_rewritten() {
        let _serial = serial();
        let first: Vec<_> = (0..256u32)
            .map(|i| JitBuffer::new(&returns(i)).unwrap())
            .collect();
        let released: std::collections::BTreeSet<_> =
            first.iter().map(|b| b.as_ptr() as usize).collect();
        drop(first);
        // Other tests allocate concurrently, so only some reuse is certain.
        let second: Vec<_> = (0..256u32)
            .map(|i| JitBuffer::new(&returns(1000 + i)).unwrap())
            .collect();
        let reused = second
            .iter()
            .filter(|b| released.contains(&(b.as_ptr() as usize)))
            .count();
        assert!(reused > 0);
        for (i, buffer) in second.iter().enumerate() {
            assert_eq!(call_at(buffer, 0), 1000 + i as u32);
        }
    }

    #[test]
    fn whole_free_pages_return_to_the_kernel_and_refill() {
        let _serial = serial();
        let page = crate::syscall::page_size();
        let big = |value: u32| {
            let mut code = vec![0x90; 3 * page];
            code.extend(returns(value));
            code
        };
        let before = super::stats().2;
        let buffer = JitBuffer::new(&big(7)).unwrap();
        assert!(buffer.in_arena);
        assert_eq!(call_at(&buffer, 3 * page), 7);
        drop(buffer);
        assert!(super::stats().2 >= before + 2);
        // Released pages read back as zeros and accept new code.
        let again = JitBuffer::new(&big(9)).unwrap();
        assert_eq!(call_at(&again, 3 * page), 9);
    }

    #[test]
    fn fork_retires_shared_chunks_on_both_sides() {
        let _serial = serial();
        let before = JitBuffer::new(&returns(11)).unwrap();
        let address = before.as_ptr() as usize;
        // SAFETY: the child runs only arena code and libc before `_exit`.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let fresh = JitBuffer::new(&returns(12)).unwrap();
            let ok = call_at(&before, 0) == 11
                && call_at(&fresh, 0) == 12
                && super::with_state(|state| {
                    state.chunk_of(address).is_some_and(|(_, c)| c.retired)
                        && state
                            .chunk_of(fresh.as_ptr() as usize)
                            .is_some_and(|(_, c)| !c.retired)
                });
            // SAFETY: terminate the child without running parent destructors.
            unsafe { libc::_exit(if ok { 0 } else { 1 }) };
        }
        let mut status = 0;
        // SAFETY: waiting for the child created above.
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
        // The child may still run pre-fork code, so the parent never recycles
        // that memory.
        drop(before);
        assert!(super::with_state(|state| {
            state.chunk_of(address).is_some_and(|(_, c)| c.retired)
                && state
                    .free_by_addr
                    .range(..=address)
                    .next_back()
                    .is_none_or(|(&start, &len)| address >= start + len)
        }));
        let after = JitBuffer::new(&returns(13)).unwrap();
        assert_ne!(after.as_ptr() as usize, address);
        assert_eq!(call_at(&after, 0), 13);
    }

    #[test]
    fn free_ranges_never_coalesce_across_adjacent_chunks() {
        // Bookkeeping only: these chunks are never mapped or touched.
        let (a, size, page) = (0x10_0000usize, 0x4000usize, 0x1000usize);
        let mut state = super::State::default();
        for (rx, rw) in [(a, 0x90_0000), (a + size, 0x50_0000)] {
            state.chunks.insert(
                rx,
                super::Chunk {
                    rw,
                    size,
                    retired: false,
                },
            );
        }
        // Everything allocated except what the test releases.
        let tail_of_a = a + size - 64;
        let head_of_b = a + size;
        assert_eq!(state.release(tail_of_a, 64, page), None);
        assert_eq!(state.release(head_of_b, 64, page), None);
        assert_eq!(state.free_by_addr.get(&tail_of_a), Some(&64));
        assert_eq!(state.free_by_addr.get(&head_of_b), Some(&64));
        // Within a chunk, neighbours merge and a fully free page is reported
        // through that chunk's own writable view.
        assert_eq!(
            state.release(a + size - page, page - 64, page),
            Some((0x90_0000 + size - page, page))
        );
        assert_eq!(state.free_by_addr.get(&(a + size - page)), Some(&page));
        assert_eq!(state.take(page + 64), None);
    }

    #[test]
    fn large_buffers_keep_their_own_mapping() {
        let mut code = vec![0x90; super::MAX_ARENA_ALLOC + 1];
        code.extend(returns(5));
        let buffer = JitBuffer::new(&code).unwrap();
        assert!(!buffer.in_arena);
        assert_eq!(call_at(&buffer, super::MAX_ARENA_ALLOC + 1), 5);
    }
}
