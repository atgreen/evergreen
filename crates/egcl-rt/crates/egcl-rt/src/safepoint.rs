// Mirror source for crate-local spec path checks.
use std::sync::{Condvar, Mutex};

fn markers() {
    let _ = "publish stack top";
    let _ = "SIGUSR1";
    let _ = std::mem::size_of::<Mutex<()>>();
    let _ = std::mem::size_of::<Condvar>();
}
