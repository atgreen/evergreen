// Microbench: StackRoot (mutex + registry) vs rooted! (intrusive, lock-free).
// Simulates the walk_cons pattern: N nested scopes, 4 roots each, LIFO drops.
use bliss_rt::gc::StackRoot;
use bliss_rt::value::BlissVal;
use std::time::Instant;

fn nested_stackroot(depth: usize) {
    if depth == 0 {
        return;
    }
    let mut a = BlissVal::from_fixnum(depth as i64);
    let mut b = BlissVal::from_fixnum(2);
    let mut c = BlissVal::from_fixnum(3);
    let mut d = BlissVal::from_fixnum(4);
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
    bliss_rt::rooted!(_a = BlissVal::from_fixnum(depth as i64));
    bliss_rt::rooted!(_b = BlissVal::from_fixnum(2));
    bliss_rt::rooted!(_c = BlissVal::from_fixnum(3));
    bliss_rt::rooted!(_d = BlissVal::from_fixnum(4));
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
