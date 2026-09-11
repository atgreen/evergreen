//! Bliss Common Lisp — entry point.

use bliss::cli;

fn main() {
    // Collect OS args, skipping argv[0] (the program name).
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = cli::run(&args);
    // JFR-style event stream (bliss-3gme): dump the recording if BLISS_EVENTS_DUMP
    // is set, whichever way the run ended, before we exit the process.
    cli::events::maybe_dump_on_exit();
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            // Use describe_err so symbols render by name (e.g. "undefined
            // function: FIND-IF") rather than "Symbol(194)".
            eprintln!("bliss: {}", cli::describe_err(&e));
            std::process::exit(1);
        }
    }
}
