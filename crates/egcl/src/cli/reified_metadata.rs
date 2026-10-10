// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Image persistence for the shared local-function scopes captured by closures.

use super::*;

type FunctionScope = Arc<SharedCell<HashMap<String, FunDef>>>;

pub(super) struct FunctionScopes {
    scopes: Vec<FunctionScope>,
    ids: HashMap<usize, u32>,
}

impl FunctionScopes {
    /// Called by the image serializer with the world stopped. Scope snapshots
    /// retain shared identity; serializing each closure's map separately would
    /// split recursive namespaces and cached local-function identities.
    pub(super) fn collect() -> Self {
        let roots: Vec<_> = closure_registry()
            .borrow()
            .values()
            .filter_map(|closure| closure.captured_funs.clone())
            .collect();
        let mut result = Self {
            scopes: Vec::new(),
            ids: HashMap::new(),
        };
        for scope in roots {
            result.add(scope);
        }
        let mut index = 0;
        while index < result.scopes.len() {
            let parents: Vec<_> = result.scopes[index]
                .borrow()
                .values()
                .filter_map(|definition| definition.def_funs.clone())
                .collect();
            for parent in parents {
                result.add(parent);
            }
            index += 1;
        }
        result
    }

    fn add(&mut self, scope: FunctionScope) {
        let key = Arc::as_ptr(&scope) as usize;
        if !self.ids.contains_key(&key) {
            self.ids.insert(key, self.scopes.len() as u32);
            self.scopes.push(scope);
        }
    }

    fn id(&self, scope: &Option<FunctionScope>) -> u32 {
        scope
            .as_ref()
            .map_or(u32::MAX, |scope| self.ids[&(Arc::as_ptr(scope) as usize)])
    }

    pub(super) fn frames(&self) -> Vec<Arc<SharedCell<EnvFrame>>> {
        self.scopes
            .iter()
            .flat_map(|scope| {
                scope
                    .borrow()
                    .values()
                    .filter_map(|definition| definition.defining_frame.clone())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub(super) fn write(&self, out: &mut Vec<u8>, frames: &HashMap<usize, u32>) {
        out.extend_from_slice(b"CFUN");
        put_u32(out, self.scopes.len() as u32);
        for scope in &self.scopes {
            let definitions = scope.borrow();
            put_u32(out, definitions.len() as u32);
            for (name, definition) in definitions.iter() {
                hr_put_str(out, name);
                put_u32(out, definition.params.len() as u32);
                for parameter in &definition.params {
                    hr_put_str(out, parameter);
                }
                out.extend_from_slice(&definition.params_form.to_raw().to_le_bytes());
                out.extend_from_slice(&definition.body.to_raw().to_le_bytes());
                put_u32(out, self.id(&definition.def_funs));
                put_u32(
                    out,
                    definition
                        .defining_frame
                        .as_ref()
                        .map_or(u32::MAX, |frame| frames[&(Arc::as_ptr(frame) as usize)]),
                );
                out.push(u8::from(definition.closure_ref.is_some()));
                if let Some(reference) = definition.closure_ref {
                    out.extend_from_slice(&reference.to_raw().to_le_bytes());
                }
                put_u32(out, definition.defining_specials.len() as u32);
                for &symbol in &definition.defining_specials {
                    put_u32(out, symbol);
                }
                out.push(u8::from(definition.defining_blocks.is_some()));
                if let Some((blocks, tags)) = &definition.defining_blocks {
                    write_exits(out, blocks);
                    write_exits(out, tags);
                }
            }
        }
        let registry = closure_registry();
        let closures = registry.borrow();
        put_u32(out, closures.len() as u32);
        for (&id, closure) in closures.iter() {
            out.extend_from_slice(&id.to_le_bytes());
            put_u32(out, self.id(&closure.captured_funs));
        }
    }
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_exits(out: &mut Vec<u8>, exits: &[(String, String)]) {
    put_u32(out, exits.len() as u32);
    for (name, token) in exits {
        hr_put_str(out, name);
        hr_put_str(out, token);
    }
}

fn invalid() -> EgclError {
    EgclError::InvalidImage("host registry: invalid closure function scopes".into())
}

struct Reader<'a> {
    data: &'a [u8],
    offset: &'a mut usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Result<u32, EgclError> {
        hr_get_u32(self.data, self.offset).ok_or_else(invalid)
    }
    fn u64(&mut self) -> Result<u64, EgclError> {
        hr_get_u64(self.data, self.offset).ok_or_else(invalid)
    }
    fn text(&mut self) -> Result<String, EgclError> {
        hr_get_str(self.data, self.offset).ok_or_else(invalid)
    }
    fn flag(&mut self) -> Result<bool, EgclError> {
        let value = *self.data.get(*self.offset).ok_or_else(invalid)?;
        *self.offset += 1;
        match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid()),
        }
    }
    fn exits(&mut self) -> Result<Vec<(String, String)>, EgclError> {
        let mut exits = Vec::new();
        for _ in 0..self.u32()? {
            exits.push((self.text()?, self.text()?));
        }
        Ok(exits)
    }
}

fn reference<T>(items: &[Arc<T>], index: u32) -> Result<Option<Arc<T>>, EgclError> {
    if index == u32::MAX {
        Ok(None)
    } else {
        items
            .get(index as usize)
            .cloned()
            .map(Some)
            .ok_or_else(invalid)
    }
}

/// Restore placeholders before their contents so nested scopes preserve graph
/// identity. Only Rust allocation occurs; every tagged field is remapped before
/// the restored graph is published to the closure registry's root scanner.
pub(super) fn restore(
    data: &[u8],
    offset: &mut usize,
    remap: &impl Fn(u64) -> u64,
    frames: &[Arc<SharedCell<EnvFrame>>],
    restored_closure_ids: &HashSet<u64>,
) -> Result<(), EgclError> {
    let mut reader = Reader { data, offset };
    let count = reader.u32()? as usize;
    if count > data.len().saturating_sub(*reader.offset) / 4 {
        return Err(invalid());
    }
    let scopes: Vec<FunctionScope> = (0..count)
        .map(|_| Arc::new(SharedCell::new(HashMap::new())))
        .collect();
    for scope in &scopes {
        for _ in 0..reader.u32()? {
            let name = reader.text()?;
            let mut params = Vec::new();
            for _ in 0..reader.u32()? {
                params.push(reader.text()?);
            }
            let params_form = EgclVal::from_raw(remap(reader.u64()?));
            let body = EgclVal::from_raw(remap(reader.u64()?));
            let def_funs = reference(&scopes, reader.u32()?)?;
            let defining_frame = reference(frames, reader.u32()?)?;
            let closure_ref = if reader.flag()? {
                Some(EgclVal::from_raw(remap(reader.u64()?)))
            } else {
                None
            };
            let mut defining_specials = Vec::new();
            for _ in 0..reader.u32()? {
                defining_specials.push(reader.u32()?);
            }
            let defining_blocks = if reader.flag()? {
                Some((reader.exits()?, reader.exits()?))
            } else {
                None
            };
            if scope
                .borrow_mut()
                .insert(
                    name,
                    FunDef {
                        params,
                        params_form,
                        body,
                        def_funs,
                        closure_ref,
                        defining_blocks,
                        defining_frame,
                        defining_specials,
                    },
                )
                .is_some()
            {
                return Err(invalid());
            }
        }
    }
    let mut attachments = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..reader.u32()? {
        let id = reader.u64()?;
        if !seen.insert(id) {
            return Err(invalid());
        }
        attachments.push((id, reference(&scopes, reader.u32()?)?));
    }
    let registry = closure_registry();
    let mut closures = registry.borrow_mut();
    if attachments.len() != restored_closure_ids.len()
        || attachments
            .iter()
            .any(|(id, _)| !restored_closure_ids.contains(id) || !closures.contains_key(id))
    {
        return Err(invalid());
    }
    for (id, scope) in attachments {
        closures
            .get_mut(&id)
            .expect("validated closure identity")
            .captured_funs = scope;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parser_rejects_missing_or_truncated_function_scopes() {
        let _lock = heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let bytes = host_serialize_registries();
        let marker = bytes.windows(4).position(|word| word == b"CFUN").unwrap();
        for truncated in [&bytes[..marker], &bytes[..marker + 4], &bytes[..marker + 7]] {
            assert!(matches!(
                host_restore_registries(truncated),
                Err(EgclError::InvalidImage(_))
            ));
        }
    }

    #[test]
    fn decoder_rejects_missing_scope_and_frame_references() {
        for (scope, frame) in [(1, u32::MAX), (u32::MAX, 0)] {
            let mut bytes = Vec::new();
            put_u32(&mut bytes, 1); // one scope
            put_u32(&mut bytes, 1); // one definition
            hr_put_str(&mut bytes, "LOCAL");
            put_u32(&mut bytes, 0); // no parameter names
            bytes.extend_from_slice(&NIL.to_raw().to_le_bytes());
            bytes.extend_from_slice(&NIL.to_raw().to_le_bytes());
            put_u32(&mut bytes, scope);
            put_u32(&mut bytes, frame);
            bytes.push(0); // no cached closure identity
            put_u32(&mut bytes, 0); // no special declarations
            bytes.push(0); // no control targets
            put_u32(&mut bytes, 0); // no closure attachments
            assert!(restore(&bytes, &mut 0, &|v| v, &[], &HashSet::new()).is_err());
        }
    }

    #[test]
    fn decoder_rejects_duplicate_closure_attachments() {
        let mut bytes = Vec::new();
        put_u32(&mut bytes, 0); // no scopes
        put_u32(&mut bytes, 2); // duplicate attachments
        for _ in 0..2 {
            bytes.extend_from_slice(&7u64.to_le_bytes());
            put_u32(&mut bytes, u32::MAX);
        }
        assert!(restore(&bytes, &mut 0, &|v| v, &[], &HashSet::from([7])).is_err());
    }
}
