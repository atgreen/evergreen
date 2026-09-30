// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Foreign storage is outside the moving Lisp heap. Owned allocations have
//! explicit lifetimes: dropping a pointer does not free memory C may retain.
//! Imported addresses are borrowed and cannot be freed by this allocator.

use super::AlienType;
use crate::object::{ObjectHeader, type_id};
use crate::{EgclError, EgclVal};
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::{Mutex, OnceLock};

/// An address plus optional allocation identity. Arithmetic preserves identity,
/// so freeing an allocation invalidates all its aliases, even after address reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignPointer {
    address: usize,
    allocation: u64,
}

struct Allocation {
    address: usize,
    size: usize,
    layout: Layout,
}

#[derive(Default)]
struct Allocations {
    last_id: u64,
    live: HashMap<u64, Allocation>,
    /// Base address -> identity, for freeing by address (see `free`). An address
    /// is only ever in this map while its allocation is live, so a reused
    /// address always resolves to its current owner.
    by_address: HashMap<usize, u64>,
}

fn allocations() -> &'static Mutex<Allocations> {
    static ALLOCATIONS: OnceLock<Mutex<Allocations>> = OnceLock::new();
    ALLOCATIONS.get_or_init(Default::default)
}

fn invalid(message: &str) -> EgclError {
    EgclError::FfiError(message.into())
}

impl ForeignPointer {
    pub fn is_pointer(value: EgclVal) -> bool {
        value.is_heap_object()
            && unsafe {
                (*(value.as_ptr() as *const ObjectHeader)).type_id() == type_id::FOREIGN_POINTER
            }
    }

    /// Copy native bits out before any Lisp allocation can move the wrapper.
    pub fn from_lisp(value: EgclVal) -> Result<Self, EgclError> {
        if !Self::is_pointer(value) {
            return Err(EgclError::TypeError {
                datum: value,
                expected: "EGCL-FFI:FOREIGN-POINTER".into(),
            });
        }
        // SAFETY: the type discriminator selects exactly these two untraced words.
        let words = unsafe { value.as_ptr().add(8) as *const u64 };
        Ok(Self {
            address: unsafe { words.read() } as usize,
            allocation: unsafe { words.add(1).read() },
        })
    }

    /// The wrapper may move; the referenced foreign storage never moves with it.
    pub fn into_lisp(self) -> Result<EgclVal, EgclError> {
        let body = crate::gc::alloc_typed(16, type_id::FOREIGN_POINTER).ok_or(EgclError::Oom)?;
        // SAFETY: fresh 16-byte body, with no Lisp allocation between allocation
        // and initialization. No Lisp heap references are stored in this object.
        unsafe {
            (body as *mut u64).write(self.address as u64);
            (body as *mut u64).add(1).write(self.allocation);
            Ok(EgclVal::from_heap_ptr(body.sub(8)))
        }
    }

    /// Import an address with no ownership or bounds claim. Merely constructing
    /// it is safe; dereference requires the caller's unsafe lifetime contract.
    pub fn from_address(address: usize) -> Self {
        Self {
            address,
            allocation: 0,
        }
    }

    pub fn address(self) -> usize {
        self.address
    }

    /// Reject stale/out-of-region tracked pointers before passing them to C.
    /// This cannot check how many bytes a foreign callee will access, or keep
    /// storage alive against an explicit concurrent free by the caller.
    pub fn call_address(self) -> Result<usize, EgclError> {
        if self.allocation == 0 {
            Ok(self.address)
        } else {
            self.with_access(0, |pointer| pointer as usize)
        }
    }

    /// Allocate zeroed foreign bytes, aligned for the scalar types we support.
    /// Zero-size allocations have distinct non-null storage but no readable bytes.
    /// Only `free`, not Rust drop or Lisp GC, releases this allocation.
    pub fn allocate(size: usize) -> Result<Self, EgclError> {
        let layout = Layout::from_size_align(size.max(1), 16)
            .map_err(|_| invalid("foreign allocation size is too large"))?;
        let mut allocations = allocations().lock().unwrap();
        let id = allocations
            .last_id
            .checked_add(1)
            .ok_or_else(|| invalid("foreign allocation identities exhausted"))?;
        // SAFETY: layout is nonzero and valid. Allocation failure is checked;
        // the exact layout is retained until explicit deallocation.
        let address = unsafe { alloc_zeroed(layout) } as usize;
        if address == 0 {
            return Err(EgclError::Oom);
        }
        allocations.last_id = id;
        allocations.live.insert(
            id,
            Allocation {
                address,
                size,
                layout,
            },
        );
        allocations.by_address.insert(address, id);
        Ok(Self {
            address,
            allocation: id,
        })
    }

    /// Preserve provenance even outside the allocation. Bounds are checked at
    /// access time, so a one-past-end pointer can be moved back into the region.
    pub fn offset(self, bytes: isize) -> Result<Self, EgclError> {
        let address = self
            .address
            .checked_add_signed(bytes)
            .ok_or_else(|| invalid("foreign pointer arithmetic overflow"))?;
        Ok(Self { address, ..self })
    }

    /// Free an allocation made by `allocate`. Reject interior, borrowed and
    /// already-freed pointers. A borrowed null pointer is a harmless no-op.
    /// Release an allocation this allocator owns.
    ///
    /// A pointer that carries its allocation identity must name that
    /// allocation's base. A pointer without one — read back out of foreign
    /// memory with MEM-REF, or handed over by C — is freed by ADDRESS when that
    /// address is the base of a live tracked allocation: round-tripping a
    /// pointer through memory is ordinary FFI practice, and CFFI's contract for
    /// FOREIGN-FREE is about an address, not about which wrapper reached it
    /// (bliss-06l4z).
    ///
    /// An address this allocator never handed out is still refused. That is the
    /// point of the check: passing storage that C malloc'd to Rust's
    /// deallocator is undefined behaviour, so it must not be attempted.
    pub fn free(self) -> Result<(), EgclError> {
        if self.address == 0 && self.allocation == 0 {
            return Ok(());
        }
        let mut allocations = allocations().lock().unwrap();
        let identity = if self.allocation == 0 {
            *allocations
                .by_address
                .get(&self.address)
                .ok_or_else(|| invalid("cannot free a borrowed foreign pointer"))?
        } else {
            self.allocation
        };
        let allocation = allocations
            .live
            .get(&identity)
            .ok_or_else(|| invalid("foreign allocation has already been freed"))?;
        if allocation.address != self.address {
            return Err(invalid("cannot free an interior foreign pointer"));
        }
        let allocation = allocations.live.remove(&identity).unwrap();
        allocations.by_address.remove(&allocation.address);
        // SAFETY: this is the original allocation address and layout. Removing
        // its unique identity under the lock prevents double-free and excludes
        // accesses made through tracked aliases while deallocation occurs.
        unsafe { dealloc(allocation.address as *mut u8, allocation.layout) };
        Ok(())
    }

    fn with_access<T>(
        self,
        size: usize,
        access: impl FnOnce(*mut u8) -> T,
    ) -> Result<T, EgclError> {
        if self.address == 0 {
            return Err(invalid("cannot dereference a null foreign pointer"));
        }
        self.address
            .checked_add(size)
            .ok_or_else(|| invalid("foreign memory range overflows address space"))?;
        if self.allocation == 0 {
            // The caller of the unsafe read/write API supplies the validity
            // contract for borrowed memory; null/overflow checks cannot prove it.
            return Ok(access(self.address as *mut u8));
        }
        let allocations = allocations().lock().unwrap();
        let allocation = allocations
            .live
            .get(&self.allocation)
            .ok_or_else(|| invalid("foreign allocation has already been freed"))?;
        let offset = self
            .address
            .checked_sub(allocation.address)
            .ok_or_else(|| invalid("foreign memory access precedes allocation"))?;
        if offset
            .checked_add(size)
            .is_none_or(|end| end > allocation.size)
        {
            return Err(invalid("foreign memory access exceeds allocation"));
        }
        // Keep the registry lock until the byte operation finishes; free cannot
        // invalidate tracked storage during the access. Never allocate Lisp data
        // or call foreign/Lisp code from this closure.
        Ok(access(self.address as *mut u8))
    }

    /// Check a complete range without dereferencing it. This is not a lease:
    /// callers must revalidate before a later access, especially after callbacks.
    pub fn check_range(self, size: usize) -> Result<(), EgclError> {
        self.with_access(size, |_| ())
    }

    /// Copy native object bytes, preserving potentially uninitialized C padding.
    ///
    /// # Safety
    /// Borrowed memory must be readable for `size` bytes. No external writer or
    /// deallocator may race the copy; tracked frees are excluded by the registry.
    pub unsafe fn read_buffer(self, size: usize) -> Result<Vec<MaybeUninit<u8>>, EgclError> {
        self.with_access(size, |pointer| {
            let mut buffer = vec![MaybeUninit::uninit(); size];
            unsafe { std::ptr::copy_nonoverlapping(pointer.cast(), buffer.as_mut_ptr(), size) };
            buffer
        })
    }

    /// Copy native object bytes back after checking the destination's current
    /// allocation identity and bounds. Never hold the registry across C/Lisp.
    ///
    /// # Safety
    /// Borrowed memory must be writable for the buffer length and exclusively
    /// accessible during the copy. The buffer must not overlap the destination.
    pub unsafe fn write_buffer(self, buffer: &[MaybeUninit<u8>]) -> Result<(), EgclError> {
        self.with_access(buffer.len(), |pointer| unsafe {
            std::ptr::copy_nonoverlapping(buffer.as_ptr(), pointer.cast(), buffer.len());
        })
    }

    /// Read raw scalar bits in native byte order, including at unaligned addresses.
    ///
    /// # Safety
    /// Borrowed storage must be readable and initialized for the selected type.
    /// C code must not concurrently mutate or free the storage. No Rust reference
    /// to its contents may conflict with this access. Owned storage must only be
    /// released through `free`, not through a foreign allocator.
    pub unsafe fn read_scalar(self, ty: &AlienType) -> Result<u64, EgclError> {
        let size = scalar_size(ty)?;
        self.with_access(size, |pointer| {
            // SAFETY: with_access validates owned bounds and lifetime; the
            // caller supplies borrowed validity and external synchronization.
            unsafe {
                match size {
                    1 => pointer.read() as u64,
                    2 => (pointer as *const u16).read_unaligned() as u64,
                    4 => (pointer as *const u32).read_unaligned() as u64,
                    8 => (pointer as *const u64).read_unaligned(),
                    _ => unreachable!(),
                }
            }
        })
    }

    /// Store raw scalar bits in native byte order. Narrow types use the low bits;
    /// Lisp-level range checking belongs to `marshal_to_c`, before this operation.
    ///
    /// # Safety
    /// Borrowed storage must be writable for the selected type. C code must not
    /// concurrently access or free it, and no Rust reference may alias it. Owned
    /// storage must only be released through `free`, not a foreign allocator.
    pub unsafe fn write_scalar(self, ty: &AlienType, bits: u64) -> Result<(), EgclError> {
        let size = scalar_size(ty)?;
        self.with_access(size, |pointer| {
            // SAFETY: as for read_scalar, with exclusive access supplied by caller.
            unsafe {
                match size {
                    1 => pointer.write(bits as u8),
                    2 => (pointer as *mut u16).write_unaligned(bits as u16),
                    4 => (pointer as *mut u32).write_unaligned(bits as u32),
                    8 => (pointer as *mut u64).write_unaligned(bits),
                    _ => unreachable!(),
                }
            }
        })
    }
}

fn scalar_size(ty: &AlienType) -> Result<usize, EgclError> {
    match ty {
        AlienType::Int {
            bits: 8 | 16 | 32 | 64,
            ..
        }
        | AlienType::Float
        | AlienType::Double
        | AlienType::Pointer(_)
        | AlienType::FnPtr { .. } => Ok(ty.size()),
        _ => Err(invalid(
            "foreign scalar memory access requires a supported scalar type",
        )),
    }
}
