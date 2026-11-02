// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Heap creators select templates from the exact retained native code owner.
use super::*;

#[cfg(test)]
pub(in crate::cli::bytecode) static OBSERVE: egcl_rt::execution_local::ExecutionLocal<
    Cell<Option<unsafe fn(*mut EgclVal, usize)>>,
> = unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(None)) };

pub(super) fn make(args: &[EgclVal], env: &Env) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(args = args.to_vec());
    if args.len() < 2 || !args[0].is_fixnum() || args[0].as_fixnum() < 0 {
        return Err(invalid_capture());
    }
    #[cfg(test)]
    if let Some(observe) = OBSERVE.with(|slot| slot.replace(None)) {
        unsafe { observe(args.as_mut_ptr(), args.len()) };
    }
    let bcp = usize::try_from(args[0].as_fixnum()).map_err(|_| invalid_capture())?;
    let nested = {
        let boundary = root_publication::ACTIVE.with(Cell::get);
        let boundary = unsafe { boundary.as_ref() }.ok_or_else(invalid_capture)?;
        let owner = unsafe { boundary.owner().as_ref() }.ok_or_else(invalid_capture)?;
        let Some(Instr::MakeEnvClosure { func, nflat }) = owner.body.code.get(bcp) else {
            return Err(invalid_capture());
        };
        if args.len() != usize::from(*nflat) + 2 {
            return Err(invalid_capture());
        }
        Arc::clone(
            owner
                .body
                .nested_functions
                .get(*func as usize)
                .ok_or_else(invalid_capture)?,
        )
    };
    // Release all native-directory/body borrows before allocation. The caller
    // segment retains the template, while these writable roots own every
    // parent and flat capture throughout make_closure's allocations.
    super::super::heap_env::make_closure(&nested, args[1], &args[2..], env).ok_or_else(|| {
        EgclError::Internal(egcl_rt::gc::allocation_failure_message(
            "a closure environment",
        ))
    })
}
