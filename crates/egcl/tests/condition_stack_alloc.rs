//! R5.109 gates: condition handler/restart cluster establishment is stack-backed.

use egcl::cli;

fn run_eval(program: &str) {
    let status = cli::run(&[
        "--no-bootstrap".into(),
        "--no-init".into(),
        "--eval".into(),
        program.into(),
    ])
    .expect("program should run");
    assert_eq!(status, 0);
}

#[test]
fn handler_and_restart_establishment_do_not_gc_allocate_per_iteration() {
    unsafe {
        std::env::set_var("EGCL_BACKEND", "bytecode");
        std::env::set_var("EGCL_T1_THRESHOLD", "2");
    }

    let program = concat!(
        "(let ((i 0)) ",
        "  (tagbody ",
        "   top ",
        "    (if (= i 12000) (go done)) ",
        "    (handler-case 1 (error (e) e)) ",
        "    (restart-case 1 (use-value (v) v)) ",
        "    (setq i (+ i 1)) ",
        "    (go top) ",
        "   done) ",
        "  i)",
    );

    let before = egcl_rt::heap_stats().bytes_allocated;
    run_eval(program);
    let allocated = egcl_rt::heap_stats().bytes_allocated - before;

    assert!(
        allocated < 512 * 1024,
        "12,000 handler/restart establishments allocated {allocated} bytes; cluster setup should not touch the GC heap"
    );
}
