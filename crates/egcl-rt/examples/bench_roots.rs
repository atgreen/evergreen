// Microbench: StackRoot (mutex + registry) vs rooted! (intrusive, lock-free).
// Simulates the walk_cons pattern: N nested scopes, 4 roots each, LIFO drops.
use std::time::Instant;
use egcl_rt::gc::StackRoot;
use egcl_rt::value::EgclVal;

fn nested_stackroot(depth: usize) {
    if depth == 0 {
        return;
    }
    let mut a = EgclVal::from_fixnum(depth as i64);
    let mut b = EgclVal::from_fixnum(2);
    let mut c = EgclVal::from_fixnum(3);
    let mut d = EgclVal::from_fixnum(4);
    let _r1 = StackRoot::new(&mut a);
    let _r2 = StackRoot::new(&mut b);
    let _r3 = StackRoot::new(&mut c);
    let _r4 = StackRoot::new(&mut d);
    nested_stackroot(depth - 1);
}

fn nested_rooted(depth: usize) {
    if depth == 0 {
        return;
    }
    egcl_rt::rooted!(_a = EgclVal::from_fixnum(depth as i64));
    egcl_rt::rooted!(_b = EgclVal::from_fixnum(2));
    egcl_rt::rooted!(_c = EgclVal::from_fixnum(3));
    egcl_rt::rooted!(_d = EgclVal::from_fixnum(4));
    nested_rooted(depth - 1);
}

fn main() {
    const DEPTH: usize = 2000;
    const ITERS: usize = 500;
    // warmup
    nested_stackroot(DEPTH);
    nested_rooted(DEPTH);
    let t = Instant::now();
    for _ in 0..ITERS {
        nested_stackroot(DEPTH);
    }
    let sr = t.elapsed();
    let t = Instant::now();
    for _ in 0..ITERS {
        nested_rooted(DEPTH);
    }
    let ro = t.elapsed();
    let ops = (DEPTH * 4 * ITERS) as f64;
    println!(
        "StackRoot: {:?} total, {:.1} ns/root-op",
        sr,
        sr.as_nanos() as f64 / ops
    );
    println!(
        "rooted!:   {:?} total, {:.1} ns/root-op",
        ro,
        ro.as_nanos() as f64 / ops
    );
    println!(
        "speedup:   {:.1}x",
        sr.as_nanos() as f64 / ro.as_nanos() as f64
    );
}
