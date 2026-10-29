// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

struct ReaderFixture {
    previous_env: *mut Env,
    previous_boot: bool,
    indices: [u32; 2],
    old_cells: Vec<EgclVal>,
}
impl egcl_rt::gc::TraceHostRoots for ReaderFixture {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for value in &mut self.old_cells {
            visit(value);
        }
    }
}
impl Drop for ReaderFixture {
    fn drop(&mut self) {
        READ_EVAL_ENV.with(|cell| cell.set(self.previous_env));
        BOOT_COMPLETE.with(|cell| cell.set(self.previous_boot));
        for (position, index) in self.indices.iter().copied().enumerate() {
            egcl_rt::symbols::set_symbol_function(index, self.old_cells[position * 2]);
            egcl_rt::symbols::set_symbol_package(index, self.old_cells[position * 2 + 1]);
        }
    }
}

#[test]
fn reader_keeps_defined_ordinary_helpers_but_isolates_private_bootstrap_names() {
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    ensure_package_registry();
    egcl_rt::rooted!(
        foreign =
            egcl_stdlib::make_package("PRIVATE-READER-FOREIGN", &[], &["COMMON-LISP"],).unwrap()
    );
    egcl_rt::rooted!(
        imports =
            egcl_stdlib::make_package("PRIVATE-READER-IMPORTS", &[], &["COMMON-LISP"],).unwrap()
    );
    let indices = [
        reader::intern_symbol("CALCULATE-C-EVEN"),
        reader::intern_symbol("%PRIVATE-READER-FIXTURE"),
    ];
    let old_cells = indices
        .iter()
        .flat_map(|index| {
            [
                egcl_rt::symbols::symbol_function(*index).unwrap_or(egcl_rt::value::UNBOUND),
                egcl_rt::symbols::symbol_package(*index).unwrap_or(NIL),
            ]
        })
        .collect();
    // Root saved cells and retain RAII restoration even if an assertion fails.
    // The published environment itself is rooted independently above.
    let previous_env = READ_EVAL_ENV.with(|cell| cell.replace(&mut *env));
    let previous_boot = BOOT_COMPLETE.with(|cell| cell.replace(true));
    egcl_rt::rooted!(
        _fixture = ReaderFixture {
            previous_env,
            previous_boot,
            indices,
            old_cells,
        }
    );
    egcl_rt::rooted!(body = arena_cons(EgclVal::from_fixnum(42), NIL));
    egcl_rt::rooted!(function = egcl_rt::function::alloc_interpreted(NIL, *body, NIL, NIL));
    for index in indices {
        egcl_rt::symbols::set_symbol_package(index, NIL);
        egcl_rt::symbols::set_symbol_function(index, *function);
    }
    // This exercises the actual resolver and real function cells, not just the
    // guard predicate. Ordinary homeless definitions must keep library #. use.
    for package in ["PRIVATE-READER-FOREIGN", "COMMON-LISP-USER"] {
        assert_eq!(
            reader_symbol_resolver(Some(package), "CALCULATE-C-EVEN"),
            Some(indices[0])
        );
    }
    for package in ["COMMON-LISP", "EGCL-INTERNAL", "EGCL-EXT"] {
        assert_eq!(
            reader_symbol_resolver(Some(package), "%PRIVATE-READER-FIXTURE"),
            Some(indices[1])
        );
    }
    for package in ["PRIVATE-READER-FOREIGN", "COMMON-LISP-USER"] {
        let local = reader_symbol_resolver(Some(package), "%PRIVATE-READER-FIXTURE")
            .expect("private user name must intern normally");
        assert_ne!(local, indices[1]);
        let home = egcl_stdlib::find_package(package).unwrap();
        assert_eq!(egcl_rt::symbols::symbol_package(local), Some(home));
        assert_eq!(
            egcl_stdlib::intern("%PRIVATE-READER-FIXTURE", home)
                .unwrap()
                .0,
            EgclVal::from_symbol_index(local)
        );
    }
    // The documented image-control alias is an explicit compatibility case,
    // separate from sharing ordinary private implementation helpers.
    let save_core = reader::intern_symbol("%SAVE-CORE");
    assert_eq!(
        reader_symbol_resolver(Some("COMMON-LISP-USER"), "%SAVE-CORE"),
        Some(save_core)
    );
    let foreign_save_core = reader_symbol_resolver(Some("PRIVATE-READER-FOREIGN"), "%SAVE-CORE")
        .expect("foreign image-control spelling must intern locally");
    assert_ne!(foreign_save_core, save_core);
    assert_eq!(
        egcl_rt::symbols::symbol_package(foreign_save_core),
        Some(*foreign)
    );
    // Import the properly owned user symbol created above. The deliberately
    // homeless synthetic bootstrap identity is not a package-table entry.
    // An intentional import must win before the private compatibility rule.
    egcl_rt::rooted!(
        imported = egcl_stdlib::intern("%PRIVATE-READER-FIXTURE", *foreign)
            .unwrap()
            .0
    );
    assert_ne!(*imported, EgclVal::from_symbol_index(indices[1]));
    egcl_stdlib::import(&[*imported], *imports).unwrap();
    assert_eq!(
        reader_symbol_resolver(Some("PRIVATE-READER-IMPORTS"), "%PRIVATE-READER-FIXTURE"),
        imported.symbol_index()
    );
    assert_eq!(
        egcl_rt::symbols::symbol_package(imported.as_symbol_index()),
        Some(*foreign)
    );
}
