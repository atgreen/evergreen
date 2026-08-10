//! CLI entry point — argument parsing, REPL driver, and image-load entry.
//! See spec §6.1 (REPL), §7.4 (deployment modes), §2.8 (CLI args).

use bliss_compiler::reader;
use bliss_rt::error::BlissError;
use bliss_rt::object::{ConsCell, ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, EOF, NIL, T, TAG_HEAP_OBJECT};

// ── CLI arguments ──────────────────────────────────────────────────
#[derive(Clone, Debug)]
pub struct CliArgs {
    pub image: Option<String>, pub eval: Option<String>, pub load: Option<String>,
    pub no_image: bool, pub bootstrap: bool, pub workers: Option<usize>,
    pub heap_size: Option<String>, pub help: bool, pub version: bool,
    pub sandbox: bool, pub no_init: bool, pub cl_args: Vec<String>,
    pub script: Option<String>,
}

fn take_value<'a>(flag: &str, iter: &mut impl Iterator<Item = &'a String>) -> Result<String, BlissError> {
    iter.next().map(|s| s.clone()).ok_or_else(|| BlissError::Internal(format!("{} requires a value", flag)))
}

impl CliArgs {
    pub fn parse(args: &[String]) -> Result<Self, BlissError> {
        let mut r = CliArgs { image: None, eval: None, load: None, no_image: false,
            bootstrap: false, workers: None, heap_size: None, help: false, version: false,
            sandbox: false, no_init: false, cl_args: Vec::new(), script: None };
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--" => { r.cl_args = iter.cloned().collect(); break; }
                "--help" => r.help = true,
                "--version" => r.version = true,
                "--eval" | "-e" => r.eval = Some(take_value(arg, &mut iter)?),
                "--load" => r.load = Some(take_value(arg, &mut iter)?),
                "--image" => r.image = Some(take_value(arg, &mut iter)?),
                "--no-image" => r.no_image = true,
                "--bootstrap" => r.bootstrap = true,
                "--sandbox" => r.sandbox = true,
                "--no-init" => r.no_init = true,
                "--workers" => { let v = take_value(arg, &mut iter)?;
                    r.workers = Some(v.parse::<usize>().map_err(|_|
                        BlissError::Internal(format!("--workers requires a numeric value, got: {}", v)))?); }
                "--heap-size" => r.heap_size = Some(take_value(arg, &mut iter)?),
                s if s.starts_with('-') => return Err(BlissError::Internal(format!("unknown flag: {}", s))),
                _ => r.script = Some(arg.clone()),
            }
        }
        if r.image.is_some() && r.no_image { return Err(BlissError::Internal("--image and --no-image are contradictory".into())); }
        if r.sandbox && r.no_image { return Err(BlissError::Internal("--sandbox and --no-image are contradictory".into())); }
        if r.no_init && r.bootstrap { return Err(BlissError::Internal("--no-init and --bootstrap are contradictory".into())); }
        if r.eval.is_some() && r.load.is_some() { return Err(BlissError::Internal("--eval and --load are contradictory".into())); }
        Ok(r)
    }
}

// ── BlissVal printer ──────────────────────────────────────────────
fn print_val(val: BlissVal, out: &mut String) {
    if val.is_nil() { out.push_str("NIL"); }
    else if val == T { out.push('T'); }
    else if val == EOF { out.push_str("#<EOF>"); }
    else if val.is_fixnum() { out.push_str(&val.as_fixnum().to_string()); }
    else if val.is_single_float() {
        let s = format!("{}", val.as_single_float());
        out.push_str(&s);
        if !s.contains('.') && !s.contains('e') { out.push_str(".0"); }
    } else if val.is_character() {
        out.push_str("#\\");
        match val.as_char() { ' '=>out.push_str("Space"), '\n'=>out.push_str("Newline"),
            '\t'=>out.push_str("Tab"), '\r'=>out.push_str("Return"), c=>out.push(c) }
    } else if val.is_symbol() {
        out.push_str(&sym_name(val));
    } else if val.is_cons() {
        out.push('('); print_list_body(val, out); out.push(')');
    } else if val.is_heap_object() {
        unsafe {
            let ptr = val.as_ptr();
            let hdr = *(ptr as *const ObjectHeader);
            match hdr.type_id() {
                type_id::SIMPLE_BASE_STRING => {
                    let len = *(ptr.add(8) as *const u64) as usize;
                    let data = std::slice::from_raw_parts(ptr.add(16), len);
                    if let Ok(s) = std::str::from_utf8(data) {
                        out.push('"');
                        for c in s.chars() { if c=='"'||c=='\\' { out.push('\\'); } out.push(c); }
                        out.push('"');
                    } else { out.push_str("#<string>"); }
                }
                type_id::SIMPLE_VECTOR => {
                    let len = *(ptr.add(8) as *const u64) as usize;
                    out.push_str("#(");
                    for i in 0..len { if i>0 { out.push(' '); }
                        print_val(*(ptr.add(16+i*8) as *const BlissVal), out); }
                    out.push(')');
                }
                _ => out.push_str(&format!("#<heap-object type={}>", hdr.type_id())),
            }
        }
    } else { out.push_str(&format!("#<unknown {:#x}>", val.0)); }
}

fn print_list_body(val: BlissVal, out: &mut String) {
    let mut cur = val; let mut first = true;
    while cur.is_cons() {
        if !first { out.push(' '); } first = false;
        unsafe { let c = cur.as_ptr() as *const ConsCell;
            print_val((*c).car, out); cur = (*c).cdr; }
    }
    if !cur.is_nil() { out.push_str(" . "); print_val(cur, out); }
}

fn format_val(val: BlissVal) -> String { let mut s = String::new(); print_val(val, &mut s); s }

fn princ_val(val: BlissVal, out: &mut String) {
    if val.is_heap_object() { unsafe {
        let ptr = val.as_ptr(); let hdr = *(ptr as *const ObjectHeader);
        if hdr.type_id() == type_id::SIMPLE_BASE_STRING {
            let len = *(ptr.add(8) as *const u64) as usize;
            let data = std::slice::from_raw_parts(ptr.add(16), len);
            if let Ok(s) = std::str::from_utf8(data) { out.push_str(s); return; }
        }
    }}
    if val.is_character() { out.push(val.as_char()); return; }
    print_val(val, out);
}

// ── Symbol name lookup ────────────────────────────────────────────
fn sym_name(val: BlissVal) -> String {
    if val.is_nil() { return "NIL".into(); }
    if val == T { return "T".into(); }
    if !val.is_symbol() { return String::new(); }
    let idx = val.as_symbol_index();
    static KNOWN: &[&str] = &["QUOTE","IF","PROGN","LET","LAMBDA","PRINT","FORMAT",
        "+","-","*","/","CONS","LIST","CAR","CDR","VALUES","ERROR",
        "DEFUN","DEFMACRO","DEFCLASS","SETQ","FUNCTION",
        "BLISS::QUASIQUOTE","BLISS::UNQUOTE"];
    for &name in KNOWN {
        if let Ok((sym, _)) = reader::read_from_string(name) {
            if sym.is_symbol() && sym.as_symbol_index() == idx { return name.to_string(); }
        }
    }
    format!("SYM#{}", idx)
}

// ── Minimal bootstrap evaluator ───────────────────────────────────
fn read_eval_all(source: &str) -> Result<BlissVal, BlissError> {
    let chars: Vec<char> = source.chars().collect();
    let mut pos = 0; let mut last = NIL;
    loop {
        while pos < chars.len() && chars[pos].is_ascii_whitespace() { pos += 1; }
        if pos >= chars.len() { break; }
        let remaining: String = chars[pos..].iter().collect();
        let (val, consumed) = reader::read_from_string(&remaining)?;
        if val == EOF { break; }
        last = eval_form(val)?;
        pos += consumed;
    }
    Ok(last)
}

fn eval_form(form: BlissVal) -> Result<BlissVal, BlissError> {
    if form.is_nil() || form == T { return Ok(form); }
    if form.is_fixnum() || form.is_single_float() || form.is_character() { return Ok(form); }
    if form.is_heap_object() { unsafe {
        let hdr = *(form.as_ptr() as *const ObjectHeader);
        if hdr.type_id() == type_id::SIMPLE_BASE_STRING { return Ok(form); }
    }}
    if form.is_symbol() { return Err(BlissError::UnboundVariable(form)); }
    if form.is_cons() { return eval_list(form); }
    Ok(form)
}

fn cp(val: BlissVal) -> (BlissVal, BlissVal) {
    if !val.is_cons() { return (NIL, NIL); }
    unsafe { let c = val.as_ptr() as *const ConsCell; ((*c).car, (*c).cdr) }
}

fn eval_list(form: BlissVal) -> Result<BlissVal, BlissError> {
    let (car, cdr) = cp(form);
    if car.is_symbol() {
        let name = sym_name(car);
        match name.as_str() {
            "QUOTE" => { let (q, _) = cp(cdr); return Ok(q); }
            "IF" => {
                let (test, r) = cp(cdr); let tv = eval_form(test)?;
                let (then, er) = cp(r);
                return if !tv.is_nil() { eval_form(then) }
                       else if er.is_cons() { let (ef, _) = cp(er); eval_form(ef) }
                       else { Ok(NIL) };
            }
            "PROGN" => return eval_progn(cdr),
            "PRINT" => {
                let (a, _) = cp(cdr); let v = eval_form(a)?;
                println!("\n{}", format_val(v)); return Ok(v);
            }
            "+" => return eval_arith(cdr, 0, |a,b| a+b),
            "-" => return eval_arith_sub(cdr),
            "*" => return eval_arith(cdr, 1, |a,b| a*b),
            "CONS" => {
                let (af, r) = cp(cdr); let (bf, _) = cp(r);
                let a = eval_form(af)?; let b = eval_form(bf)?;
                let cell = Box::leak(Box::new(ConsCell { car: a, cdr: b }));
                return Ok(unsafe { BlissVal::from_cons_ptr(cell as *mut ConsCell as *mut u8) });
            }
            "LIST" => {
                let mut elems = Vec::new(); let mut c = cdr;
                while c.is_cons() { let (ef, r) = cp(c); elems.push(eval_form(ef)?); c = r; }
                let mut result = NIL;
                for e in elems.into_iter().rev() {
                    let cell = Box::leak(Box::new(ConsCell { car: e, cdr: result }));
                    result = unsafe { BlissVal::from_cons_ptr(cell as *mut ConsCell as *mut u8) };
                }
                return Ok(result);
            }
            "VALUES" => {
                let mut vals = Vec::new(); let mut c = cdr;
                while c.is_cons() { let (af, r) = cp(c); vals.push(eval_form(af)?); c = r; }
                if vals.is_empty() { return Ok(NIL); }
                for v in &vals[1..] { println!("{}", format_val(*v)); }
                return Ok(vals[0]);
            }
            "FORMAT" => return eval_format(cdr),
            "ERROR" => {
                let (mf, _) = cp(cdr); let m = eval_form(mf)?;
                return Err(BlissError::Internal(format!("ERROR: {}", val_as_str(m))));
            }
            "LET" => { let (_, body) = cp(cdr); return eval_progn(body); }
            _ => {}
        }
    }
    // Lambda application
    if car.is_cons() {
        let (lh, _) = cp(car);
        if lh.is_symbol() && sym_name(lh) == "LAMBDA" {
            let (_, lr) = cp(car); let (_, body) = cp(lr);
            return eval_progn(body);
        }
    }
    Err(BlissError::UndefinedFunction(car))
}

fn eval_progn(forms: BlissVal) -> Result<BlissVal, BlissError> {
    let mut r = NIL; let mut c = forms;
    while c.is_cons() { let (f, rest) = cp(c); r = eval_form(f)?; c = rest; }
    Ok(r)
}

fn eval_arith(args: BlissVal, init: i64, op: fn(i64,i64)->i64) -> Result<BlissVal, BlissError> {
    let mut acc = init; let mut is_f = false; let mut facc = init as f64;
    let mut c = args;
    while c.is_cons() {
        let (af, r) = cp(c); let v = eval_form(af)?;
        if v.is_fixnum() { if is_f { facc = op(facc as i64, v.as_fixnum()) as f64; } else { acc = op(acc, v.as_fixnum()); } }
        else if v.is_single_float() { if !is_f { is_f = true; facc = acc as f64; }
            facc = op(facc as i64, (v.as_single_float() as i64)) as f64; }
        else { return Err(BlissError::TypeError { datum: v, expected: "number".into() }); }
        c = r;
    }
    Ok(if is_f { BlissVal::from_single_float(facc as f32) } else { BlissVal::from_fixnum(acc) })
}

fn eval_arith_sub(args: BlissVal) -> Result<BlissVal, BlissError> {
    let mut vals = Vec::new(); let mut c = args;
    while c.is_cons() { let (af, r) = cp(c); vals.push(eval_form(af)?); c = r; }
    if vals.is_empty() { return Ok(BlissVal::from_fixnum(0)); }
    if vals.len() == 1 {
        return Ok(if vals[0].is_fixnum() { BlissVal::from_fixnum(-vals[0].as_fixnum()) }
                  else { BlissVal::from_single_float(-vals[0].as_single_float()) });
    }
    let first = if vals[0].is_fixnum() { vals[0].as_fixnum() } else { vals[0].as_single_float() as i64 };
    let mut acc = first; let mut is_f = vals[0].is_single_float();
    for v in &vals[1..] {
        if v.is_fixnum() { acc -= v.as_fixnum(); }
        else if v.is_single_float() { is_f = true; acc -= v.as_single_float() as i64; }
    }
    Ok(if is_f { BlissVal::from_single_float(acc as f32) } else { BlissVal::from_fixnum(acc) })
}

fn eval_format(args: BlissVal) -> Result<BlissVal, BlissError> {
    let (df, r) = cp(args); let dest = eval_form(df)?;
    let (ff, fa) = cp(r); let fv = eval_form(ff)?;
    let fs = val_as_str(fv);
    let mut av = Vec::new(); let mut c = fa;
    while c.is_cons() { let (af, r2) = cp(c); av.push(eval_form(af)?); c = r2; }
    let mut result = String::new();
    let fc: Vec<char> = fs.chars().collect();
    let (mut i, mut ai) = (0, 0);
    while i < fc.len() {
        if fc[i] == '~' && i+1 < fc.len() {
            match fc[i+1] {
                'A'|'a' => { if ai<av.len() { princ_val(av[ai], &mut result); ai+=1; } i+=2; }
                'D'|'d'|'S'|'s' => { if ai<av.len() { result.push_str(&format_val(av[ai])); ai+=1; } i+=2; }
                '~' => { result.push('~'); i+=2; }
                '%' => { result.push('\n'); i+=2; }
                _ => { result.push(fc[i]); i+=1; }
            }
        } else { result.push(fc[i]); i+=1; }
    }
    if dest == T { print!("{}", result); return Ok(NIL); }
    Ok(alloc_str_val(&result))
}

fn val_as_str(val: BlissVal) -> String {
    if val.is_heap_object() { unsafe {
        let p = val.as_ptr(); let h = *(p as *const ObjectHeader);
        if h.type_id() == type_id::SIMPLE_BASE_STRING {
            let len = *(p.add(8) as *const u64) as usize;
            let data = std::slice::from_raw_parts(p.add(16), len);
            if let Ok(s) = std::str::from_utf8(data) { return s.to_string(); }
        }
    }} format_val(val)
}

fn alloc_str_val(s: &str) -> BlissVal {
    let b = s.as_bytes(); let sz = 8+8+b.len();
    let layout = std::alloc::Layout::from_size_align(sz, 8).unwrap();
    unsafe {
        let p = std::alloc::alloc_zeroed(layout);
        *(p as *mut ObjectHeader) = ObjectHeader::new(type_id::SIMPLE_BASE_STRING, ((sz+7)/8) as u16);
        *(p.add(8) as *mut u64) = b.len() as u64;
        std::ptr::copy_nonoverlapping(b.as_ptr(), p.add(16), b.len());
        BlissVal::from_heap_ptr(p)
    }
}

// ── CLI driver ─────────────────────────────────────────────────────
pub fn run(args: &[String]) -> Result<i32, BlissError> {
    let ca = CliArgs::parse(args)?;
    if ca.help { print_help(); return Ok(0); }
    if ca.version { print_version(); return Ok(0); }
    if let Some(ref expr) = ca.eval { return run_eval(expr); }
    if let Some(ref path) = ca.load { return run_load(path); }
    if let Some(ref script) = ca.script { return run_script(script); }
    run_repl()
}

fn run_eval(expr: &str) -> Result<i32, BlissError> {
    match read_eval_all(expr) {
        Ok(result) => { println!("{}", format_val(result)); Ok(0) }
        Err(e) => { eprintln!("ERROR: {}", e); Err(e) }
    }
}

fn run_load(path: &str) -> Result<i32, BlissError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| BlissError::FileError(format!("cannot read {}: {}", path, e)))?;
    match read_eval_all(&contents) {
        Ok(_) => Ok(0),
        Err(e) => { eprintln!("ERROR: {}", e); Err(e) }
    }
}

fn run_script(path: &str) -> Result<i32, BlissError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| BlissError::FileError(format!("cannot read {}: {}", path, e)))?;
    match read_eval_all(&contents) {
        Ok(_) => Ok(0),
        Err(e) => { eprintln!("ERROR: {}", e); Err(e) }
    }
}

pub fn print_help() {
    println!("Usage: bliss [OPTIONS] [SCRIPT] [-- CL-ARGS...]");
    println!();
    println!("Bliss Common Lisp");
    println!();
    println!("Options:");
    println!("  --help               Print this help message and exit");
    println!("  --version            Print version information and exit");
    println!("  --eval, -e EXPR      Evaluate EXPR and exit");
    println!("  --load FILE          Load FILE and exit");
    println!("  --image FILE         Path to the boot image");
    println!("  --no-image           Start without loading an image");
    println!("  --bootstrap          Bootstrap from lib/boot.lisp");
    println!("  --workers N          Number of worker threads");
    println!("  --heap-size SIZE     Heap size (e.g. 512M, 1G)");
    println!("  --sandbox            Enable sandbox mode");
    println!("  --no-init            Skip loading the init file");
    println!();
    println!("Arguments after -- are passed through to CL as *command-line-args*.");
}

pub fn print_version() { println!("bliss {}", env!("CARGO_PKG_VERSION")); }

// ── REPL driver ────────────────────────────────────────────────────
pub fn run_repl() -> Result<i32, BlissError> {
    let _config = ReplConfig::default();
    println!("Bliss Common Lisp {}", env!("CARGO_PKG_VERSION"));
    println!("Type (quit) to exit.");
    println!();
    let stdin = std::io::stdin();
    let mut input = String::new();
    let mut in_debugger = false;
    loop {
        eprint!("{}", if in_debugger { "Debug> " } else { "BLISS> " });
        input.clear();
        match stdin.read_line(&mut input) {
            Ok(0) => { println!(); return Ok(0); }
            Ok(_) => {
                let trimmed = input.trim();
                if trimmed.is_empty() { continue; }
                if trimmed == "(quit)" || trimmed == "(exit)" { return Ok(0); }
                if in_debugger {
                    if trimmed == ":abort" || trimmed == ":a" { in_debugger = false; continue; }
                    eprintln!("Unknown debugger command: {}", trimmed);
                    continue;
                }
                match read_eval_all(trimmed) {
                    Ok(result) => println!("{}", format_val(result)),
                    Err(e) => { eprintln!("ERROR: {}", e); in_debugger = true; }
                }
            }
            Err(e) => return Err(BlissError::Internal(format!("read error: {}", e))),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReplConfig {
    pub history_file: String, pub history_size: usize, pub syntax_highlighting: bool,
}
impl Default for ReplConfig {
    fn default() -> Self {
        Self { history_file: "~/.bliss/repl-history".into(), history_size: 1000, syntax_highlighting: true }
    }
}
