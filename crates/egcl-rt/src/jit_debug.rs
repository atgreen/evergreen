// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Shared DWARF for installed native code and debugger consumers.
//!
//! Images describe function identities, code ranges and proven original argument
//! homes. Native unwind/caller context remains a separate responsibility.
//! Reading an image only allocates Rust storage, never Lisp objects.

use gimli::write::{Address, AttributeValue, DwarfUnit, EndianVec, Expression, Sections};
use object::{Object, ObjectSection};
use std::sync::Arc;

/// Install-time proof supplied by the compiler. A slot is present only when it
/// retains the original actual argument for the whole managed activation.
/// Missing locations remain optimized out; these are not current local values.
#[derive(Clone, Debug)]
pub struct NativeArguments {
    pub parameters: Vec<(String, Option<u16>)>,
}

/// DWARF register number of the first argument in the native C entry ABI.
/// Alternate register-only entries must not use the managed-home description.
pub fn managed_entry_register() -> Option<u16> {
    if cfg!(all(target_arch = "x86_64", windows)) {
        Some(2) // RCX
    } else if cfg!(target_arch = "x86_64") {
        Some(5) // RDI
    } else if cfg!(target_arch = "aarch64") {
        Some(0)
    } else if cfg!(target_arch = "s390x") {
        Some(2)
    } else if cfg!(target_arch = "powerpc64") {
        Some(3)
    } else {
        None
    }
}

/// Immutable debug bytes. Readers may retain these after executable code retires;
/// only the owning JitBuffer controls the external debugger registration.
pub struct DwarfImage {
    bytes: Vec<u8>,
    entry: u64,
}

impl DwarfImage {
    #[cfg(test)]
    pub(crate) fn new(name: &str, entry: u64, code: &[u8]) -> Result<Self, String> {
        Self::with_arguments(name, entry, code, None)
    }

    pub(crate) fn with_arguments(
        name: &str,
        entry: u64,
        code: &[u8],
        arguments: Option<&NativeArguments>,
    ) -> Result<Self, String> {
        // Lisp names may contain NUL, but DWARF strings are NUL-terminated.
        // Escape backslashes too, so a literal "\\0" cannot alias a NUL name.
        let name = name.replace('\\', "\\\\").replace('\0', "\\0");
        entry
            .checked_add(code.len() as u64)
            .ok_or("JIT range overflow")?;
        let endian = if cfg!(target_endian = "little") {
            gimli::RunTimeEndian::Little
        } else {
            gimli::RunTimeEndian::Big
        };
        let mut dwarf = DwarfUnit::new(gimli::Encoding {
            format: gimli::Format::Dwarf32,
            version: 5,
            address_size: std::mem::size_of::<usize>() as u8,
        });
        let root = dwarf.unit.root();
        dwarf.unit.get_mut(root).set(
            gimli::DW_AT_name,
            AttributeValue::String(b"evergreen-jit".to_vec()),
        );
        let function = dwarf.unit.add(root, gimli::DW_TAG_subprogram);
        dwarf.unit.get_mut(function).set(
            gimli::DW_AT_name,
            AttributeValue::String(name.as_bytes().to_vec()),
        );
        dwarf
            .unit
            .get_mut(function)
            .set(gimli::DW_AT_external, AttributeValue::Flag(true));
        for id in [root, function] {
            dwarf.unit.get_mut(id).set(
                gimli::DW_AT_low_pc,
                AttributeValue::Address(Address::Constant(entry)),
            );
            dwarf.unit.get_mut(id).set(
                gimli::DW_AT_high_pc,
                AttributeValue::Udata(code.len() as u64),
            );
        }
        if let Some(arguments) = arguments {
            let register = managed_entry_register().ok_or("unsupported native argument ABI")?;
            // The managed slot address was the C entry's first argument. Using
            // its entry value remains correct after scratch registers change,
            // including prologues/epilogues. GDB needs caller entry-value context;
            // the Lisp adapter already owns this exact managed activation.
            let mut entry_register = Expression::new();
            entry_register.op_reg(gimli::Register(register));
            let mut frame_base = Expression::new();
            frame_base.op_entry_value(entry_register);
            dwarf
                .unit
                .get_mut(function)
                .set(gimli::DW_AT_frame_base, AttributeValue::Exprloc(frame_base));
            let value_type = dwarf.unit.add(root, gimli::DW_TAG_base_type);
            dwarf.unit.get_mut(value_type).set(
                gimli::DW_AT_name,
                AttributeValue::String(b"EgclVal".to_vec()),
            );
            dwarf
                .unit
                .get_mut(value_type)
                .set(gimli::DW_AT_byte_size, AttributeValue::Udata(8));
            dwarf.unit.get_mut(value_type).set(
                gimli::DW_AT_encoding,
                AttributeValue::Encoding(gimli::DW_ATE_unsigned),
            );
            for (name, slot) in &arguments.parameters {
                let parameter = dwarf.unit.add(function, gimli::DW_TAG_formal_parameter);
                let name = name.replace('\\', "\\\\").replace('\0', "\\0");
                dwarf
                    .unit
                    .get_mut(parameter)
                    .set(gimli::DW_AT_name, AttributeValue::String(name.into_bytes()));
                dwarf
                    .unit
                    .get_mut(parameter)
                    .set(gimli::DW_AT_type, AttributeValue::UnitRef(value_type));
                if let Some(slot) = slot {
                    let mut location = Expression::new();
                    location.op_fbreg(i64::from(*slot) * 8);
                    dwarf
                        .unit
                        .get_mut(parameter)
                        .set(gimli::DW_AT_location, AttributeValue::Exprloc(location));
                }
            }
        }
        let mut sections = Sections::new(EndianVec::new(endian));
        dwarf.write(&mut sections).map_err(|e| e.to_string())?;
        let mut data = vec![(".text", code.to_vec())];
        sections
            .for_each(|id, section| {
                if !section.slice().is_empty() {
                    data.push((id.name(), section.slice().to_vec()));
                }
                Ok::<_, gimli::write::Error>(())
            })
            .map_err(|e| e.to_string())?;
        let bytes = make_elf(entry, &data)?;
        Ok(Self { bytes, entry })
    }

    /// The same in-memory ELF image supplied to the external debugger.
    pub fn elf_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn entry(&self) -> u64 {
        self.entry
    }

    /// Resolve a native PC by reading the shared DWARF, without a parallel name
    /// table. No mapped code is dereferenced, including after code retirement.
    pub fn function_name(&self, pc: u64) -> Result<Option<String>, String> {
        read_function_name(&self.bytes, pc)
    }

    /// Decode the exact managed-home recipe from the shared DWARF. Missing or
    /// unsupported locations make the original argument list unavailable.
    /// Returned indices still require bounds checks against the live frame.
    pub fn argument_slots(&self) -> Result<Option<Vec<u16>>, String> {
        read_argument_slots(&self.bytes)
    }
}

fn read_function_name(bytes: &[u8], pc: u64) -> Result<Option<String>, String> {
    let object = object::File::parse(bytes).map_err(|e| e.to_string())?;
    let endian = if object.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };
    let dwarf = gimli::Dwarf::load(|id| {
        let data = object
            .section_by_name(id.name())
            .map(|section| section.data())
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or(&[]);
        Ok::<_, String>(gimli::EndianSlice::new(data, endian))
    })?;
    let mut units = dwarf.units();
    while let Some(header) = units.next().map_err(|e| e.to_string())? {
        let unit = dwarf.unit(header).map_err(|e| e.to_string())?;
        let mut entries = unit.entries();
        while let Some((_, entry)) = entries.next_dfs().map_err(|e| e.to_string())? {
            if entry.tag() != gimli::DW_TAG_subprogram {
                continue;
            }
            let mut ranges = dwarf.die_ranges(&unit, entry).map_err(|e| e.to_string())?;
            let mut contains = false;
            while let Some(range) = ranges.next().map_err(|e| e.to_string())? {
                contains |= range.begin <= pc && pc < range.end;
            }
            if contains {
                if let Some(name) = entry
                    .attr_value(gimli::DW_AT_name)
                    .map_err(|e| e.to_string())?
                {
                    let name = dwarf.attr_string(&unit, name).map_err(|e| e.to_string())?;
                    return Ok(Some(name.to_string_lossy().into_owned()));
                }
            }
        }
    }
    Ok(None)
}

fn read_argument_slots(bytes: &[u8]) -> Result<Option<Vec<u16>>, String> {
    let object = object::File::parse(bytes).map_err(|e| e.to_string())?;
    let endian = if object.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };
    let dwarf = gimli::Dwarf::load(|id| {
        let data = object
            .section_by_name(id.name())
            .map(|s| s.data())
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or(&[]);
        Ok::<_, String>(gimli::EndianSlice::new(data, endian))
    })?;
    let mut units = dwarf.units();
    while let Some(header) = units.next().map_err(|e| e.to_string())? {
        let unit = dwarf.unit(header).map_err(|e| e.to_string())?;
        let mut entries = unit.entries();
        let mut depth = 0;
        let mut function_depth = None;
        let mut slots = Vec::new();
        while let Some((delta, entry)) = entries.next_dfs().map_err(|e| e.to_string())? {
            depth += delta;
            if function_depth.is_some_and(|function| depth <= function) {
                return Ok(Some(slots));
            }
            if entry.tag() == gimli::DW_TAG_subprogram {
                let Some(gimli::AttributeValue::Exprloc(base)) = entry
                    .attr_value(gimli::DW_AT_frame_base)
                    .map_err(|e| e.to_string())?
                else {
                    return Ok(None);
                };
                let mut ops = base.operations(unit.encoding());
                let Some(gimli::Operation::EntryValue { expression }) =
                    ops.next().map_err(|e| e.to_string())?
                else {
                    return Ok(None);
                };
                if ops.next().map_err(|e| e.to_string())?.is_some() {
                    return Ok(None);
                }
                let mut inner = gimli::Expression(expression).operations(unit.encoding());
                let Some(gimli::Operation::Register { register }) =
                    inner.next().map_err(|e| e.to_string())?
                else {
                    return Ok(None);
                };
                if Some(register.0) != managed_entry_register()
                    || inner.next().map_err(|e| e.to_string())?.is_some()
                {
                    return Ok(None);
                }
                function_depth = Some(depth);
            } else if function_depth.is_some_and(|function| depth == function + 1) {
                if entry.tag() == gimli::DW_TAG_unspecified_parameters {
                    return Ok(None);
                }
                if entry.tag() != gimli::DW_TAG_formal_parameter {
                    continue;
                }
                let Some(gimli::AttributeValue::Exprloc(location)) = entry
                    .attr_value(gimli::DW_AT_location)
                    .map_err(|e| e.to_string())?
                else {
                    return Ok(None);
                };
                let mut ops = location.operations(unit.encoding());
                let Some(gimli::Operation::FrameOffset { offset }) =
                    ops.next().map_err(|e| e.to_string())?
                else {
                    return Ok(None);
                };
                if offset < 0 || offset % 8 != 0 || ops.next().map_err(|e| e.to_string())?.is_some()
                {
                    return Ok(None);
                }
                let Ok(slot) = u16::try_from(offset / 8) else {
                    return Ok(None);
                };
                slots.push(slot);
            }
        }
        if function_depth.is_some() {
            return Ok(Some(slots));
        }
    }
    Ok(None)
}

// Explicit section addresses are needed for already-installed code. The ELF
// writer's relocatable-object interface does not assign these virtual addresses.
fn make_elf(entry: u64, data: &[(&str, Vec<u8>)]) -> Result<Vec<u8>, String> {
    use object::write::elf::{FileHeader, SectionHeader, Writer};
    let endian = if cfg!(target_endian = "little") {
        object::Endianness::Little
    } else {
        object::Endianness::Big
    };
    let machine = if cfg!(target_arch = "x86_64") {
        object::elf::EM_X86_64
    } else if cfg!(target_arch = "aarch64") {
        object::elf::EM_AARCH64
    } else if cfg!(target_arch = "powerpc64") {
        object::elf::EM_PPC64
    } else if cfg!(target_arch = "s390x") {
        object::elf::EM_S390
    } else if cfg!(target_arch = "riscv64") {
        object::elf::EM_RISCV
    } else {
        // Runtime DWARF reading remains available on other targets. External
        // registration is separately restricted to supported Linux targets.
        object::elf::EM_NONE
    };
    let mut bytes = Vec::new();
    let mut writer = Writer::new(endian, cfg!(target_pointer_width = "64"), &mut bytes);
    writer.reserve_file_header();
    let mut headers = Vec::new();
    for (index, (name, content)) in data.iter().enumerate() {
        writer.reserve_section_index();
        let name = writer.add_section_name(name.as_bytes());
        let offset = writer.reserve(content.len(), 1);
        headers.push(SectionHeader {
            name: Some(name),
            sh_type: object::elf::SHT_PROGBITS,
            sh_flags: if index == 0 {
                (object::elf::SHF_ALLOC | object::elf::SHF_EXECINSTR) as u64
            } else {
                0
            },
            sh_addr: if index == 0 { entry } else { 0 },
            sh_offset: offset as u64,
            sh_size: content.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
    }
    writer.reserve_shstrtab_section_index();
    writer.reserve_shstrtab();
    writer.reserve_section_headers();
    writer
        .write_file_header(&FileHeader {
            os_abi: 0,
            abi_version: 0,
            e_type: object::elf::ET_EXEC,
            e_machine: machine,
            e_entry: entry,
            e_flags: 0,
        })
        .map_err(|e| e.to_string())?;
    for (_, content) in data {
        writer.write(content);
    }
    writer.write_shstrtab();
    writer.write_null_section_header();
    for header in &headers {
        writer.write_section_header(header);
    }
    writer.write_shstrtab_section_header();
    Ok(bytes)
}

/// Exclusively owned by executable storage, never by a metadata reader.
pub(crate) struct Registration {
    image: Arc<DwarfImage>,
    #[cfg(all(
        target_os = "linux",
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "powerpc64",
            target_arch = "s390x",
            target_arch = "riscv64"
        )
    ))]
    external: gdb::Registration,
}

impl Registration {
    pub(crate) fn new(image: Arc<DwarfImage>) -> Self {
        Self {
            #[cfg(all(
                target_os = "linux",
                any(
                    target_arch = "x86_64",
                    target_arch = "aarch64",
                    target_arch = "powerpc64",
                    target_arch = "s390x",
                    target_arch = "riscv64"
                )
            ))]
            external: gdb::Registration::new(&image),
            image,
        }
    }

    pub(crate) fn image(&self) -> std::sync::Weak<DwarfImage> {
        Arc::downgrade(&self.image)
    }
}

// Remove the registration before Rust drops `image`, which owns symfile bytes.
impl Drop for Registration {
    fn drop(&mut self) {
        #[cfg(all(
            target_os = "linux",
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "powerpc64",
                target_arch = "s390x",
                target_arch = "riscv64"
            )
        ))]
        self.external.unregister();
    }
}

#[cfg(all(
    target_os = "linux",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "powerpc64",
        target_arch = "s390x",
        target_arch = "riscv64"
    )
))]
mod gdb {
    use super::DwarfImage;
    use std::ptr;
    use std::sync::Mutex;

    #[repr(C)]
    struct Entry {
        next: *mut Entry,
        previous: *mut Entry,
        symfile_addr: *const u8,
        symfile_size: u64,
    }

    #[repr(C)]
    struct Descriptor {
        version: u32,
        action_flag: u32,
        relevant_entry: *mut Entry,
        first_entry: *mut Entry,
    }

    // GDB's JIT ABI requires these exact exported names and layouts. All Rust
    // access to the descriptor and list links is serialized by REGISTRY.
    static REGISTRY: Mutex<()> = Mutex::new(());
    #[unsafe(no_mangle)]
    static mut __jit_debug_descriptor: Descriptor = Descriptor {
        version: 1,
        action_flag: 0,
        relevant_entry: ptr::null_mut(),
        first_entry: ptr::null_mut(),
    };

    #[unsafe(no_mangle)]
    #[inline(never)]
    pub extern "C" fn __jit_debug_register_code() {
        // A real compiler barrier preserves the call and descriptor publication
        // even in optimized builds; the debugger breaks at this function.
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) struct Registration {
        // Raw allocation: other registrations mutate its links under REGISTRY.
        // Do not retain Box/&mut references while it is linked into the list.
        entry: *mut Entry,
    }

    // SAFETY: entry/descriptor mutations take REGISTRY, the node has a stable
    // allocation, and its immutable symfile bytes outlive registration.
    unsafe impl Send for Registration {}
    unsafe impl Sync for Registration {}

    impl Registration {
        pub(super) fn new(image: &DwarfImage) -> Self {
            let entry = Box::into_raw(Box::new(Entry {
                next: ptr::null_mut(),
                previous: ptr::null_mut(),
                symfile_addr: image.bytes.as_ptr(),
                symfile_size: image.bytes.len() as u64,
            }));
            let _lock = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: the mutex protects all links; the caller retains the image
            // until unregister returns and this registration owns the node.
            unsafe {
                let first = __jit_debug_descriptor.first_entry;
                (*entry).next = first;
                if !first.is_null() {
                    (*first).previous = entry;
                }
                __jit_debug_descriptor.first_entry = entry;
                notify(entry, 1);
            }
            Self { entry }
        }

        pub(super) fn unregister(&mut self) {
            if self.entry.is_null() {
                return;
            }
            let _lock = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: the mutex serializes removal and notification. Both the
            // node and its symfile bytes remain allocated through the callback.
            unsafe {
                let entry = self.entry;
                let previous = (*entry).previous;
                let next = (*entry).next;
                if previous.is_null() {
                    __jit_debug_descriptor.first_entry = next;
                } else {
                    (*previous).next = next;
                }
                if !next.is_null() {
                    (*next).previous = previous;
                }
                notify(entry, 2);
                drop(Box::from_raw(entry));
                self.entry = ptr::null_mut();
            }
        }
    }

    // Caller holds REGISTRY and keeps the entry alive until this returns.
    unsafe fn notify(entry: *mut Entry, action: u32) {
        unsafe {
            __jit_debug_descriptor.relevant_entry = entry;
            __jit_debug_descriptor.action_flag = action;
        }
        __jit_debug_register_code();
        unsafe {
            __jit_debug_descriptor.relevant_entry = ptr::null_mut();
            __jit_debug_descriptor.action_flag = 0;
        }
    }

    #[cfg(test)]
    pub(super) fn contains(image: &DwarfImage) -> bool {
        let _lock = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the mutex prevents any node from retiring during traversal.
        unsafe {
            let mut entry = __jit_debug_descriptor.first_entry;
            let mut previous = ptr::null_mut();
            let mut found = false;
            while !entry.is_null() {
                assert_eq!((*entry).previous, previous);
                found |= (*entry).symfile_addr == image.bytes.as_ptr();
                previous = entry;
                entry = (*entry).next;
            }
            found
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lisp_names_with_nul_do_not_prevent_installation_or_alias_literal_escapes() {
        let nul = DwarfImage::new("LISP::A\0B", 4096, &[0; 16]).unwrap();
        let literal = DwarfImage::new(r"LISP::A\0B", 4096, &[0; 16]).unwrap();
        assert_eq!(
            nul.function_name(4096).unwrap().as_deref(),
            Some(r"LISP::A\0B")
        );
        assert_eq!(
            literal.function_name(4096).unwrap().as_deref(),
            Some(r"LISP::A\\0B")
        );
    }

    #[test]
    fn malformed_image_is_an_error() {
        assert!(read_function_name(b"bad ELF", 1).is_err());
        let image = DwarfImage::new("TRUNCATED", 4096, &[0; 16]).unwrap();
        assert!(read_function_name(&image.bytes[..image.bytes.len() / 2], 4096).is_err());
    }

    #[cfg(all(
        target_os = "linux",
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "powerpc64",
            target_arch = "s390x",
            target_arch = "riscv64"
        )
    ))]
    #[test]
    fn registrations_retire_with_code_even_if_readers_keep_images() {
        for order in [[0, 1, 2], [1, 2, 0], [2, 1, 0]] {
            let mut buffers = Vec::new();
            let mut images = Vec::new();
            for _ in 0..3 {
                let mut buffer = crate::jit::JitBuffer::new(&[0; 16]).unwrap();
                let image = buffer
                    .install_debug_info("REDEFINED")
                    .unwrap()
                    .upgrade()
                    .unwrap();
                assert!(gdb::contains(&image));
                buffers.push(Some(buffer));
                images.push(image);
            }
            for index in order {
                drop(buffers[index].take());
                for (i, image) in images.iter().enumerate() {
                    assert_eq!(gdb::contains(image), buffers[i].is_some());
                    assert_eq!(
                        image.function_name(image.entry).unwrap().as_deref(),
                        Some("REDEFINED")
                    );
                }
            }
        }
    }
}
