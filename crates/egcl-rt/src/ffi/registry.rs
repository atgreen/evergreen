// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The token table behind every loader backend, and its life across a saved
//! image (GitHub #440).
//!
//! A Lisp `FOREIGN-LIBRARY` holds a 1-based token into this table, never a raw
//! loader handle, so a stale token cannot close a newer library. Tokens are
//! never reused: a closed slot stays empty.
//!
//! An image records each slot's library name in load order. Restoring it reopens
//! every recorded library into the SAME slot, so the tokens saved in the heap
//! name the same libraries again, as SBCL reopens its shared objects in place.
//! A library that cannot be reopened leaves its slot dead: looking a symbol up
//! through it names the library and the reason, and closing it succeeds without
//! doing anything, which is what CFFI's close-then-load reload path needs.
use crate::error::EgclError;
use std::sync::Mutex;

enum Slot<L> {
    /// Explicitly closed, or never recorded. Not saved.
    Closed,
    Open {
        name: String,
        library: L,
    },
    /// Recorded in a restored image but not reopened. Saved again (so a later
    /// restore retries) until it is closed.
    Dead {
        name: String,
        reason: String,
        closed: bool,
    },
}

pub(crate) struct LibraryRegistry<L> {
    slots: Mutex<Vec<Slot<L>>>,
}

fn closed_handle() -> EgclError {
    EgclError::FfiError("invalid or closed library handle".into())
}

fn dead_library(name: &str, reason: &str) -> EgclError {
    EgclError::FfiError(format!(
        "foreign library '{name}' from the saved image could not be reopened: {reason}"
    ))
}

impl<L: Clone> LibraryRegistry<L> {
    pub(crate) const fn new() -> Self {
        LibraryRegistry {
            slots: Mutex::new(Vec::new()),
        }
    }

    /// Record a newly opened library under `name` and return its token.
    pub(crate) fn insert(&self, name: &str, library: L) -> *mut () {
        let mut slots = self.slots.lock().unwrap();
        slots.push(Slot::Open {
            name: name.to_owned(),
            library,
        });
        slots.len() as *mut ()
    }

    /// The open library a token names.
    pub(crate) fn get(&self, token: *mut ()) -> Result<L, EgclError> {
        if token.is_null() {
            return Err(EgclError::FfiError("null library handle".into()));
        }
        match self.slots.lock().unwrap().get(token as usize - 1) {
            Some(Slot::Open { library, .. }) => Ok(library.clone()),
            Some(Slot::Dead { name, reason, .. }) => Err(dead_library(name, reason)),
            Some(Slot::Closed) | None => Err(closed_handle()),
        }
    }

    /// Invalidate a token and hand back its library for the caller to release
    /// outside the lock (destructors can reenter the loader). A dead token
    /// closes as a no-op, however many times.
    pub(crate) fn take(&self, token: *mut ()) -> Result<Option<L>, EgclError> {
        let mut slots = self.slots.lock().unwrap();
        let slot = (token as usize)
            .checked_sub(1)
            .and_then(|i| slots.get_mut(i))
            .ok_or_else(closed_handle)?;
        match slot {
            Slot::Open { .. } => match std::mem::replace(slot, Slot::Closed) {
                Slot::Open { library, .. } => Ok(Some(library)),
                _ => unreachable!(),
            },
            Slot::Dead { closed, .. } => {
                *closed = true;
                Ok(None)
            }
            Slot::Closed => Err(closed_handle()),
        }
    }

    /// Every open library, in load order. Only the static ELF loader searches
    /// its own libraries; dlopen has a global scope for that.
    #[allow(dead_code)]
    pub(crate) fn open_libraries(&self) -> Vec<L> {
        self.slots
            .lock()
            .unwrap()
            .iter()
            .filter_map(|slot| match slot {
                Slot::Open { library, .. } => Some(library.clone()),
                _ => None,
            })
            .collect()
    }

    /// The image record: one entry per slot, the name of each library still
    /// owed a reopen.
    pub(crate) fn saved_names(&self) -> Vec<Option<String>> {
        self.slots
            .lock()
            .unwrap()
            .iter()
            .map(|slot| match slot {
                Slot::Open { name, .. }
                | Slot::Dead {
                    name,
                    closed: false,
                    ..
                } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Replace the table with an image's record, reopening each library in
    /// load order into its saved slot.
    pub(crate) fn restore(
        &self,
        names: Vec<Option<String>>,
        open: impl Fn(&str) -> Result<L, EgclError>,
    ) {
        let mut restored = Vec::with_capacity(names.len());
        for name in names {
            restored.push(match name {
                None => Slot::Closed,
                Some(name) => match open(&name) {
                    Ok(library) => Slot::Open { name, library },
                    Err(error) => Slot::Dead {
                        name,
                        reason: match error {
                            EgclError::FfiError(message) => message,
                            other => other.to_string(),
                        },
                        closed: false,
                    },
                },
            });
        }
        let previous = std::mem::replace(&mut *self.slots.lock().unwrap(), restored);
        // Release anything opened before the restore outside the lock.
        drop(previous);
    }
}

/// Encode an image record: a u32 slot count, then per slot a presence byte
/// and, when present, a u32 length and the UTF-8 name. Little-endian.
pub(crate) fn encode_names(names: &[Option<String>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(names.len() as u32).to_le_bytes());
    for name in names {
        match name {
            None => out.push(0),
            Some(name) => {
                out.push(1);
                out.extend_from_slice(&(name.len() as u32).to_le_bytes());
                out.extend_from_slice(name.as_bytes());
            }
        }
    }
    out
}

pub(crate) fn decode_names(bytes: &[u8]) -> Result<Vec<Option<String>>, EgclError> {
    let bad = || EgclError::InvalidImage("malformed foreign library section".into());
    let mut at = 0usize;
    let mut take = |n: usize| -> Result<&[u8], EgclError> {
        let end = at
            .checked_add(n)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(bad)?;
        let field = &bytes[at..end];
        at = end;
        Ok(field)
    };
    let u32_at = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap()) as usize;
    let count = u32_at(take(4)?);
    let mut names = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        names.push(match take(1)?[0] {
            0 => None,
            1 => {
                let len = u32_at(take(4)?);
                Some(String::from_utf8(take(len)?.to_vec()).map_err(|_| bad())?)
            }
            _ => return Err(bad()),
        });
    }
    if at != bytes.len() {
        return Err(bad());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_slots_keep_their_tokens() {
        let names = vec![
            Some("libm.so.6".to_owned()),
            None,
            Some("/x/é.so".to_owned()),
        ];
        assert_eq!(decode_names(&encode_names(&names)).unwrap(), names);
        assert!(decode_names(&[1, 0, 0]).is_err());

        let registry = LibraryRegistry::<u32>::new();
        registry.restore(names, |name| {
            if name == "libm.so.6" {
                Ok(7)
            } else {
                Err(EgclError::FfiError("gone".into()))
            }
        });
        assert_eq!(registry.get(1 as *mut ()).unwrap(), 7);
        assert!(registry.get(2 as *mut ()).is_err());
        let dead = registry.get(3 as *mut ()).unwrap_err().to_string();
        assert!(dead.contains("/x/é.so") && dead.contains("gone"), "{dead}");
        // A dead token closes as a no-op, repeatedly, and is then not saved.
        assert!(registry.take(3 as *mut ()).unwrap().is_none());
        assert!(registry.take(3 as *mut ()).unwrap().is_none());
        assert_eq!(
            registry.saved_names(),
            vec![Some("libm.so.6".to_owned()), None, None]
        );
        // New loads never reuse a restored token.
        assert_eq!(registry.insert("new", 9) as usize, 4);
        assert_eq!(registry.take(1 as *mut ()).unwrap(), Some(7));
        assert!(registry.take(1 as *mut ()).is_err());
    }
}
