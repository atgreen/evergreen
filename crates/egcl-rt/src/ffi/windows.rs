// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Windows DLL ownership. Handles exposed to Lisp are registry tokens, never
//! raw HMODULEs, so a stale token cannot close a newly loaded library.
use crate::error::EgclError;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::{Foundation::FreeLibrary, System::LibraryLoader::*};

struct Library(usize);
impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.0 as _);
        }
    }
}
static LIBRARIES: Mutex<Vec<Option<Arc<Library>>>> = Mutex::new(Vec::new());

pub fn load_foreign_library(name: &str) -> Result<*mut (), EgclError> {
    if name.contains('\0') {
        return Err(EgclError::FfiError(
            "library name contains null byte".into(),
        ));
    }
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
    if handle.is_null() {
        return Err(loader_error(name));
    }
    let mut libraries = LIBRARIES.lock().unwrap();
    libraries.push(Some(Arc::new(Library(handle as usize))));
    Ok(libraries.len() as *mut ())
}
fn loader_error(name: &str) -> EgclError {
    EgclError::FfiError(format!("{name}: {}", std::io::Error::last_os_error()))
}
fn library(token: *mut ()) -> Result<Arc<Library>, EgclError> {
    (token as usize)
        .checked_sub(1)
        .and_then(|i| LIBRARIES.lock().unwrap().get(i).and_then(Clone::clone))
        .ok_or_else(|| EgclError::FfiError("invalid or closed library handle".into()))
}
/// # Safety
/// The library must remain loaded while the returned address is used.
pub unsafe fn foreign_symbol(token: *mut (), name: &str) -> Result<*const (), EgclError> {
    let lib = library(token)?;
    let name_c = std::ffi::CString::new(name)
        .map_err(|_| EgclError::FfiError("symbol name contains null byte".into()))?;
    let symbol = unsafe { GetProcAddress(lib.0 as _, name_c.as_ptr().cast()) }
        .map(|f| f as *const ())
        .ok_or_else(|| loader_error(name))?;
    super::remember_foreign_symbol(symbol, name);
    Ok(symbol)
}
/// # Safety
/// No caller may continue using symbols from the closed library.
pub unsafe fn close_foreign_library(token: *mut ()) -> Result<(), EgclError> {
    let lib = (token as usize)
        .checked_sub(1)
        .and_then(|i| LIBRARIES.lock().unwrap().get_mut(i).and_then(Option::take))
        .ok_or_else(|| EgclError::FfiError("invalid or closed library handle".into()))?;
    drop(lib);
    Ok(())
}
/// # Safety
/// Windows has no RTLD_DEFAULT namespace; callers must select a DLL explicitly.
pub unsafe fn foreign_symbol_global(_name: &str) -> Result<*const (), EgclError> {
    Err(EgclError::FfiError(
        "Windows symbol lookup requires an explicit DLL".into(),
    ))
}
