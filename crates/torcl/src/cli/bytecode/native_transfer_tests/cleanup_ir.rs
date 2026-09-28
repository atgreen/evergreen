use super::*;
use torcl_compiler::t2::{
    build,
    ir::{AuxData, Opcode},
    verify,
};

#[test]
fn native_v2_normal_cleanup_has_explicit_saved_values_and_checked_continuations() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    torcl_rt::rooted_ref!(_env = &mut env);
    for source in [
        "((unwind-protect (values x (list x)) (list x)))",
        "((unwind-protect (unwind-protect (values x) (list x)) (list x)))",
        "((unwind-protect x (unwind-protect (values x) (list x))))",
        "((if x (unwind-protect (list x) (list x)) (unwind-protect (values) (list x))))",
    ] {
        torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        torcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function("NORMAL-CLEANUP", *params, *forms, &env, false, false).unwrap(),
        );
        let _body = ActiveBytecodeRoot::new(&body);
        assert!(build::build_from_bytecode(&body).is_err());
        let f = build::build_from_bytecode_for_transfers(&body).expect("normal cleanup SSA");
        verify::verify(&f).unwrap();
        let saves: Vec<_> = f
            .block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.iter().copied())
            .filter(|&i| f.inst(i).opcode == Opcode::CleanupSave)
            .collect();
        let restores: Vec<_> = f
            .block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.iter().copied())
            .filter(|&i| f.inst(i).opcode == Opcode::CleanupRestore)
            .collect();
        assert!(!saves.is_empty());
        assert_eq!(saves.len(), restores.len());
        use torcl_compiler::t2::pass::{Analyses, Pass};
        let mut optimized = f.clone();
        torcl_compiler::t2::opt_dce::Dce.run(&mut optimized, &mut Analyses::new());
        verify::verify(&optimized).unwrap();
        for &inst in saves.iter().chain(&restores) {
            assert!(
                optimized
                    .block_order()
                    .iter()
                    .any(|&b| optimized.block(b).insts.contains(&inst)),
                "cleanup value custody is effectful even when primary is unused"
            );
        }
        let mut bad = f.clone();
        bad.inst_mut(restores[0]).aux = AuxData::None;
        assert!(
            verify::verify(&bad)
                .unwrap_err()
                .iter()
                .any(|error| error.check == "V13 cleanup")
        );
        let mut bad = f.clone();
        bad.inst_mut(saves[0]).opcode = Opcode::ClearMv;
        assert!(
            verify::verify(&bad)
                .unwrap_err()
                .iter()
                .any(|error| error.check == "V13 cleanup")
        );
        if source.contains("(if") {
            let mut bad = f.clone();
            bad.inst_mut(restores[0]).opcode = Opcode::ClearMv;
            bad.inst_mut(restores[0]).aux = AuxData::None;
            assert!(
                verify::verify(&bad)
                    .unwrap_err()
                    .iter()
                    .any(|error| error.check == "V13 cleanup"
                        && error.detail.contains("join disagrees")),
                "one branch cannot arrive with an extra pending cleanup"
            );
        }
        assert!(
            torcl_compiler::t2::emit::emit_framed_transfers(&f, 1, body.num_slots()).is_err(),
            "no emission until native cleanup helpers are wired"
        );
        assert!(torcl_compiler::t2::emit::emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, None).is_err());
    }
}
