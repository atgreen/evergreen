//! CLI entry point — argument parsing, REPL driver, and image-load entry.
//! See spec §6.1 (REPL), §7.4 (deployment modes), §2.8 (CLI args).

use bliss_compiler::reader;
use bliss_rt::error::BlissError;
use bliss_rt::object::{ConsCell, ObjectHeader, RatioData, type_id};
use bliss_rt::runtime::parse_cli as parse_runtime_cli;
use bliss_rt::value::{BlissVal, EOF, NIL, T};

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

// ── CLI arguments ──────────────────────────────────────────────────
#[derive(Clone, Debug)]
pub struct CliArgs {
    pub image: Option<String>,
    pub eval: Option<String>,
    pub load: Option<String>,
    pub no_image: bool,
    pub bootstrap: bool,
    pub no_bootstrap: bool,
    pub workers: Option<usize>,
    pub heap_size: Option<String>,
    pub help: bool,
    pub version: bool,
    pub sandbox: bool,
    pub no_init: bool,
    pub cl_args: Vec<String>,
    pub script: Option<String>,
}

impl CliArgs {
    pub fn parse(args: &[String]) -> Result<Self, BlissError> {
        let mut shared_args = Vec::new();
        let mut cl_args = Vec::new();
        let mut bootstrap = false;
        let mut no_bootstrap = false;
        let mut sandbox = false;
        let mut no_init = false;
        let mut help = false;
        let mut version = false;
        let mut script = None;
        let mut saw_double_dash = false;

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if saw_double_dash {
                cl_args.push(arg.clone());
                i += 1;
                continue;
            }

            match arg.as_str() {
                "--" => {
                    saw_double_dash = true;
                    i += 1;
                }
                "--help" | "--version" => {
                    if arg == "--help" {
                        help = true;
                    } else {
                        version = true;
                    }
                    i += 1;
                }
                s if is_forwarded_runtime_flag(s) => {
                    shared_args.push(arg.clone());
                    if runtime_flag_requires_value(arg) {
                        let value = args.get(i + 1).ok_or_else(|| {
                            BlissError::Internal(format!("{} requires a value", arg))
                        })?;
                        shared_args.push(value.clone());
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                "-e" => {
                    shared_args.push("--eval".into());
                    let value = args
                        .get(i + 1)
                        .ok_or_else(|| BlissError::Internal("-e requires a value".into()))?;
                    shared_args.push(value.clone());
                    i += 2;
                }
                "--bootstrap" => {
                    // The prelude now loads by default; --bootstrap is kept as an
                    // accepted no-op for backward compatibility.
                    bootstrap = true;
                    i += 1;
                }
                "--no-bootstrap" => {
                    no_bootstrap = true;
                    i += 1;
                }
                "--sandbox" => {
                    sandbox = true;
                    i += 1;
                }
                "--no-init" => {
                    no_init = true;
                    i += 1;
                }
                s if s.starts_with('-') => {
                    return Err(BlissError::Internal(format!("unknown flag: {}", s)));
                }
                _ => {
                    if script.is_none() {
                        script = Some(arg.clone());
                    }
                    i += 1;
                }
            }
        }

        let (config, runtime_cl_args) = parse_runtime_cli(&shared_args)?;
        if !runtime_cl_args.is_empty() {
            cl_args.extend(runtime_cl_args);
        }

        let r = CliArgs {
            image: extract_flag_value(&shared_args, "--image"),
            eval: config.eval_form.clone(),
            load: config.load_file.clone(),
            no_image: shared_args.iter().any(|arg| arg == "--no-image"),
            bootstrap,
            no_bootstrap,
            workers: extract_flag_value(&shared_args, "--workers")
                .map(|value| value.parse::<usize>())
                .transpose()
                .map_err(|_| BlissError::Internal("--workers requires a numeric value".into()))?,
            heap_size: extract_size_arg(&shared_args, "--heap-size"),
            help,
            version,
            sandbox,
            no_init,
            cl_args,
            script,
        };
        if r.image.is_some() && r.no_image {
            return Err(BlissError::Internal(
                "--image and --no-image are contradictory".into(),
            ));
        }
        if r.sandbox && r.no_image {
            return Err(BlissError::Internal(
                "--sandbox and --no-image are contradictory".into(),
            ));
        }
        if r.eval.is_some() && r.load.is_some() {
            return Err(BlissError::Internal(
                "--eval and --load are contradictory".into(),
            ));
        }
        Ok(r)
    }
}

fn is_forwarded_runtime_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--eval"
            | "--load"
            | "--image"
            | "--no-image"
            | "--workers"
            | "--heap-size"
            | "--tlab-size"
            | "--nursery-size"
            | "--stack-size"
            | "--gc-log"
            | "--jit-dump"
            | "--log-level"
    )
}

fn runtime_flag_requires_value(flag: &str) -> bool {
    matches!(
        flag,
        "--eval"
            | "--load"
            | "--image"
            | "--workers"
            | "--heap-size"
            | "--tlab-size"
            | "--nursery-size"
            | "--stack-size"
            | "--gc-log"
            | "--log-level"
    )
}

fn extract_size_arg(args: &[String], flag: &str) -> Option<String> {
    extract_flag_value(args, flag)
}

fn extract_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find_map(|pair| {
        if pair[0] == flag {
            Some(pair[1].clone())
        } else {
            None
        }
    })
}

// ── Arena allocator (replaces Box::leak) ─────────────────────────
// A bump allocator that collects all allocations and frees them on drop.
// Uses a generational scheme: blocks can be promoted to a "permanent" set
// that survives arena compaction, while temporary blocks are freed.
struct Arena {
    blocks: Vec<(*mut u8, std::alloc::Layout)>,
    /// Permanent blocks that survive compaction (referenced by the environment)
    permanent: Vec<(*mut u8, std::alloc::Layout)>,
    /// Threshold: when blocks exceed this count, a compaction is suggested
    compaction_threshold: usize,
}

impl Arena {
    fn new() -> Self {
        Arena {
            blocks: Vec::new(),
            permanent: Vec::new(),
            compaction_threshold: 10000,
        }
    }

    /// Promote all current blocks to permanent status.
    /// Call this after loading an image or defining persistent functions
    /// so those allocations survive future compaction.
    fn promote_all(&mut self) {
        self.permanent.append(&mut self.blocks);
    }

    /// Free all non-permanent blocks. This reclaims memory from temporary
    /// allocations (e.g., intermediate eval results in the REPL).
    /// SAFETY: Caller must ensure no references to non-permanent blocks exist.
    fn compact(&mut self) {
        for &(ptr, layout) in &self.blocks {
            unsafe {
                std::alloc::dealloc(ptr, layout);
            }
        }
        self.blocks.clear();
    }

    /// Returns true if the number of temporary blocks exceeds the threshold
    fn should_compact(&self) -> bool {
        self.blocks.len() > self.compaction_threshold
    }

    fn alloc_cons(&mut self, car: BlissVal, cdr: BlissVal) -> BlissVal {
        let layout = std::alloc::Layout::new::<ConsCell>();
        unsafe {
            let ptr = std::alloc::alloc_zeroed(layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            let cell = ptr as *mut ConsCell;
            (*cell).car = car;
            (*cell).cdr = cdr;
            self.blocks.push((ptr, layout));
            BlissVal::from_cons_ptr(ptr)
        }
    }

    fn alloc_str(&mut self, s: &str) -> BlissVal {
        let b = s.as_bytes();
        let sz = 8 + 8 + b.len();
        let layout = std::alloc::Layout::from_size_align(sz, 8).unwrap();
        unsafe {
            let p = std::alloc::alloc_zeroed(layout);
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            *(p as *mut ObjectHeader) =
                ObjectHeader::new(type_id::SIMPLE_BASE_STRING, sz.div_ceil(8) as u16);
            *(p.add(8) as *mut u64) = b.len() as u64;
            std::ptr::copy_nonoverlapping(b.as_ptr(), p.add(16), b.len());
            self.blocks.push((p, layout));
            BlissVal::from_heap_ptr(p)
        }
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        for &(ptr, layout) in &self.blocks {
            unsafe {
                std::alloc::dealloc(ptr, layout);
            }
        }
        for &(ptr, layout) in &self.permanent {
            unsafe {
                std::alloc::dealloc(ptr, layout);
            }
        }
    }
}

// Thread-local arena so alloc functions can be called from anywhere in the evaluator.
thread_local! {
    static ARENA: RefCell<Arena> = RefCell::new(Arena::new());
}

fn arena_cons(car: BlissVal, cdr: BlissVal) -> BlissVal {
    ARENA.with(|a| a.borrow_mut().alloc_cons(car, cdr))
}

fn arena_str(s: &str) -> BlissVal {
    ARENA.with(|a| a.borrow_mut().alloc_str(s))
}

// ── Stream tracking for file I/O ─────────────────────────────────
#[derive(Clone, Debug)]
enum StreamDirection {
    Input,
    Output,
}

#[derive(Clone, Debug)]
struct StreamState {
    content: String,
    position: usize,
    direction: StreamDirection,
    path: String,
    buffer: String,
}

thread_local! {
    static STREAMS: RefCell<HashMap<u64, StreamState>> = RefCell::new(HashMap::new());
    static NEXT_STREAM_ID: RefCell<u64> = const { RefCell::new(1) };
}

fn open_stream(path: &str, direction: StreamDirection) -> Result<BlissVal, BlissError> {
    let state = match direction {
        StreamDirection::Input => {
            let content = std::fs::read_to_string(path)
                .map_err(|e| BlissError::FileError(format!("cannot open {}: {}", path, e)))?;
            StreamState {
                content,
                position: 0,
                direction,
                path: path.to_string(),
                buffer: String::new(),
            }
        }
        StreamDirection::Output => StreamState {
            content: String::new(),
            position: 0,
            direction,
            path: path.to_string(),
            buffer: String::new(),
        },
    };
    let id = NEXT_STREAM_ID.with(|c| {
        let v = *c.borrow();
        *c.borrow_mut() = v + 1;
        v
    });
    STREAMS.with(|s| s.borrow_mut().insert(id, state));
    // Represent stream as a tagged fixnum with high bit set to distinguish from regular fixnums
    Ok(BlissVal::from_fixnum(-(id as i64)))
}

fn close_stream(stream_val: BlissVal) -> Result<(), BlissError> {
    if !stream_val.is_fixnum() {
        return Ok(());
    }
    let id = (-stream_val.as_fixnum()) as u64;
    STREAMS.with(|s| {
        if let Some(state) = s.borrow_mut().remove(&id) {
            if matches!(state.direction, StreamDirection::Output) {
                std::fs::write(&state.path, &state.buffer).map_err(|e| {
                    BlissError::FileError(format!("cannot write {}: {}", state.path, e))
                })?;
            }
            Ok(())
        } else {
            Ok(())
        }
    })
}

fn is_stream(val: BlissVal) -> bool {
    val.is_fixnum() && val.as_fixnum() < 0
}

fn stream_read_line(stream_val: BlissVal) -> Result<(String, bool), BlissError> {
    if !is_stream(stream_val) {
        return Err(BlissError::StreamError("not a stream".into()));
    }
    let id = (-stream_val.as_fixnum()) as u64;
    STREAMS.with(|s| {
        let mut streams = s.borrow_mut();
        if let Some(state) = streams.get_mut(&id) {
            if state.position >= state.content.len() {
                return Ok(("".to_string(), true)); // EOF
            }
            let remaining = &state.content[state.position..];
            if let Some(newline_pos) = remaining.find('\n') {
                let line = remaining[..newline_pos].to_string();
                state.position += newline_pos + 1;
                Ok((line, false))
            } else {
                let line = remaining.to_string();
                state.position = state.content.len();
                Ok((line, true))
            }
        } else {
            Err(BlissError::StreamError("stream not open".into()))
        }
    })
}

fn stream_write_string(stream_val: BlissVal, s: &str) -> Result<(), BlissError> {
    if !is_stream(stream_val) {
        return Err(BlissError::StreamError("not a stream".into()));
    }
    let id = (-stream_val.as_fixnum()) as u64;
    STREAMS.with(|streams| {
        let mut streams = streams.borrow_mut();
        if let Some(state) = streams.get_mut(&id) {
            state.buffer.push_str(s);
            Ok(())
        } else {
            Err(BlissError::StreamError("stream not open".into()))
        }
    })
}

// ── Closure representation ───────────────────────────────────────
#[derive(Clone)]
struct Closure {
    /// Raw lambda list, for full &optional/&rest/&key binding.
    params_form: BlissVal,
    body: BlissVal,
    captured_frame: Rc<RefCell<EnvFrame>>,
}

#[derive(Clone, Copy)]
enum EvalContext {
    Repl,
    Eval,
    Load,
}

// ── Environment for variable/function bindings ───────────────────
// Uses Rc for global definitions (funs, macros, classes, methods, packages)
// so that child() only clones the local vars HashMap, not the entire env.
#[derive(Clone)]
struct Env {
    frame: Rc<RefCell<EnvFrame>>,
    funs: Rc<HashMap<String, FunDef>>,
    macros: Rc<HashMap<String, MacroDef>>,
    classes: Rc<HashMap<String, ClassDef>>,
    methods: Rc<HashMap<String, Vec<MethodDef>>>,
    packages: Rc<HashMap<String, PackageDef>>,
    current_package: String,
    sandbox: bool,
    restarts: Vec<RestartEntry>,
    handlers: Vec<HandlerEntry>,
    /// Multiple values from last (values ...) or (floor ...) call
    mv: Vec<BlissVal>,
    mv_active: bool,
    /// Closures stored by name or lambda id
    closures: Rc<RefCell<HashMap<u64, Closure>>>,
    block_stack: Vec<(String, String)>,
    catch_stack: Vec<(String, String)>,
    /// Tags visible for GO: (tag-name, tagbody-token)
    tag_stack: Vec<(String, String)>,
    eval_context: EvalContext,
}

#[derive(Clone, Default)]
struct EnvFrame {
    vars: HashMap<String, BlissVal>,
    parent: Option<Rc<RefCell<EnvFrame>>>,
}

#[derive(Clone)]
struct FunDef {
    params: Vec<String>,
    /// Raw lambda list, for full &optional/&rest/&key binding.
    params_form: BlissVal,
    body: BlissVal,
}

#[derive(Clone)]
struct MacroDef {
    params: Vec<String>,
    rest_param: Option<String>,
    body: BlissVal,
}

#[allow(dead_code)]
#[derive(Clone)]
struct ClassDef {
    name: String,
    supers: Vec<String>,
    slots: Vec<SlotDef>,
}

#[derive(Clone)]
struct SlotDef {
    name: String,
    initarg: Option<String>,
    accessor: Option<String>,
}

#[derive(Clone)]
struct MethodDef {
    specializer: String,
    params: Vec<String>,
    body: BlissVal,
}

#[allow(dead_code)]
#[derive(Clone)]
struct PackageDef {
    name: String,
    exports: Vec<String>,
    uses: Vec<String>,
    symbols: HashMap<String, BlissVal>,
}

#[derive(Clone)]
struct RestartEntry {
    name: String,
    body: Option<BlissVal>, // form to evaluate when restart is invoked
}

/// Sentinel error used for non-local control flow when invoke-restart is called.
#[derive(Debug)]
#[allow(dead_code)]
struct RestartInvoked {
    name: String,
    result: BlissVal,
}

#[derive(Clone)]
struct HandlerEntry {
    type_name: String,
    handler: BlissVal, // lambda form
}

thread_local! {
    static CONTROL_VALUES: RefCell<HashMap<String, BlissVal>> = RefCell::new(HashMap::new());
    static CONTROL_COUNTER: RefCell<u64> = const { RefCell::new(0) };
}

fn next_control_token(prefix: &str) -> String {
    let id = CONTROL_COUNTER.with(|counter| {
        let id = *counter.borrow();
        *counter.borrow_mut() = id + 1;
        id
    });
    format!("{prefix}:{id}")
}

fn store_control_value(token: &str, value: BlissVal) {
    CONTROL_VALUES.with(|values| {
        values.borrow_mut().insert(token.to_string(), value);
    });
}

fn take_control_value(token: &str) -> BlissVal {
    CONTROL_VALUES.with(|values| values.borrow_mut().remove(token).unwrap_or(NIL))
}

impl Env {
    fn new(sandbox: bool) -> Self {
        let _ = bliss_stdlib::bootstrap_clos();
        let mut packages = HashMap::new();
        seed_standard_packages(&mut packages);
        let mut env = Env {
            frame: Rc::new(RefCell::new(EnvFrame::default())),
            funs: Rc::new(HashMap::new()),
            macros: Rc::new(HashMap::new()),
            classes: Rc::new(HashMap::new()),
            methods: Rc::new(HashMap::new()),
            packages: Rc::new(packages),
            current_package: "COMMON-LISP-USER".to_string(),
            sandbox,
            restarts: Vec::new(),
            handlers: Vec::new(),
            mv: Vec::new(),
            mv_active: false,
            closures: Rc::new(RefCell::new(HashMap::new())),
            block_stack: Vec::new(),
            catch_stack: Vec::new(),
            tag_stack: Vec::new(),
            eval_context: EvalContext::Repl,
        };
        env.define_local("*MODULE-PROVIDER-FUNCTIONS*", NIL);
        env.define_local("*LOAD-HOOKS*", NIL);
        env.define_local(
            "*FEATURES*",
            vec_to_list(&[resolve_sym(":BLISS").unwrap_or(NIL)]),
        );
        env.define_local("*PACKAGE*", arena_str("COMMON-LISP-USER"));
        env.define_local("*TYPE-DEFINITIONS*", NIL);
        env.define_local("*CONDITION-TYPES*", NIL);
        env.define_local("*CONDITION-DEFINITIONS*", NIL);
        env
    }

    /// Create a child environment that shares global definitions (funs, macros,
    /// classes, methods, packages) via Rc and only clones local vars.
    fn child(&self) -> Self {
        Env {
            frame: Rc::new(RefCell::new(EnvFrame {
                vars: HashMap::new(),
                parent: Some(Rc::clone(&self.frame)),
            })),
            funs: Rc::clone(&self.funs),
            macros: Rc::clone(&self.macros),
            classes: Rc::clone(&self.classes),
            methods: Rc::clone(&self.methods),
            packages: Rc::clone(&self.packages),
            current_package: self.current_package.clone(),
            sandbox: self.sandbox,
            restarts: self.restarts.clone(),
            handlers: self.handlers.clone(),
            mv: self.mv.clone(),
            mv_active: self.mv_active,
            closures: Rc::clone(&self.closures),
            block_stack: self.block_stack.clone(),
            catch_stack: self.catch_stack.clone(),
            tag_stack: self.tag_stack.clone(),
            eval_context: self.eval_context,
        }
    }

    fn child_with_parent(&self, parent: Rc<RefCell<EnvFrame>>) -> Self {
        Env {
            frame: Rc::new(RefCell::new(EnvFrame {
                vars: HashMap::new(),
                parent: Some(parent),
            })),
            funs: Rc::clone(&self.funs),
            macros: Rc::clone(&self.macros),
            classes: Rc::clone(&self.classes),
            methods: Rc::clone(&self.methods),
            packages: Rc::clone(&self.packages),
            current_package: self.current_package.clone(),
            sandbox: self.sandbox,
            restarts: self.restarts.clone(),
            handlers: self.handlers.clone(),
            mv: self.mv.clone(),
            mv_active: self.mv_active,
            closures: Rc::clone(&self.closures),
            block_stack: self.block_stack.clone(),
            catch_stack: self.catch_stack.clone(),
            tag_stack: self.tag_stack.clone(),
            eval_context: self.eval_context,
        }
    }

    fn lookup_var(&self, name: &str) -> Option<BlissVal> {
        let frame = self.frame.borrow();
        if let Some(val) = frame.vars.get(name) {
            return Some(*val);
        }
        let parent = frame.parent.clone();
        drop(frame);
        parent.and_then(|parent| Self::lookup_frame(&parent, name))
    }

    fn set_var(&mut self, name: &str, val: BlissVal) {
        if Self::set_frame_var(&self.frame, name, val) {
            return;
        }
        self.define_local(name, val);
    }

    fn define_local(&mut self, name: &str, val: BlissVal) {
        self.frame.borrow_mut().vars.insert(name.to_string(), val);
    }

    fn lookup_frame(frame: &Rc<RefCell<EnvFrame>>, name: &str) -> Option<BlissVal> {
        let borrowed = frame.borrow();
        if let Some(val) = borrowed.vars.get(name) {
            return Some(*val);
        }
        let parent = borrowed.parent.clone();
        drop(borrowed);
        parent.and_then(|parent| Self::lookup_frame(&parent, name))
    }

    fn set_frame_var(frame: &Rc<RefCell<EnvFrame>>, name: &str, val: BlissVal) -> bool {
        {
            let mut borrowed = frame.borrow_mut();
            if borrowed.vars.contains_key(name) {
                borrowed.vars.insert(name.to_string(), val);
                return true;
            }
            let parent = borrowed.parent.clone();
            drop(borrowed);
            if let Some(parent) = parent {
                return Self::set_frame_var(&parent, name, val);
            }
        }
        false
    }

    fn clear_mv(&mut self) {
        self.mv.clear();
        self.mv_active = false;
    }

    fn set_mv(&mut self, values: Vec<BlissVal>) {
        self.mv = values;
        self.mv_active = true;
    }
}

fn with_eval_context<T>(
    env: &mut Env,
    context: EvalContext,
    f: impl FnOnce(&mut Env) -> Result<T, BlissError>,
) -> Result<T, BlissError> {
    let previous = env.eval_context;
    env.eval_context = context;
    let result = f(env);
    env.eval_context = previous;
    result
}

fn seed_standard_packages(packages: &mut HashMap<String, PackageDef>) {
    for (name, uses) in [
        ("COMMON-LISP", Vec::<String>::new()),
        ("COMMON-LISP-USER", vec!["COMMON-LISP".to_string()]),
        ("KEYWORD", Vec::<String>::new()),
        ("BLISS-INTERNAL", Vec::<String>::new()),
        ("BLISS-EXT", vec!["COMMON-LISP".to_string()]),
    ] {
        packages.insert(
            name.to_string(),
            PackageDef {
                name: name.to_string(),
                exports: Vec::new(),
                uses,
                symbols: HashMap::new(),
            },
        );
        reader::register_package(name);
    }
    reader::register_package("CL-USER");
}

// ── BlissVal printer ──────────────────────────────────────────────
fn print_val(val: BlissVal, out: &mut String) {
    if val.is_nil() {
        out.push_str("NIL");
    } else if val == T {
        out.push('T');
    } else if val == EOF {
        out.push_str("#<EOF>");
    } else if val.is_fixnum() {
        out.push_str(&val.as_fixnum().to_string());
    } else if val.is_single_float() {
        let s = format!("{}", val.as_single_float());
        out.push_str(&s);
        if !s.contains('.') && !s.contains('e') {
            out.push_str(".0");
        }
    } else if val.is_character() {
        out.push_str("#\\");
        match val.as_char() {
            ' ' => out.push_str("Space"),
            '\n' => out.push_str("Newline"),
            '\t' => out.push_str("Tab"),
            '\r' => out.push_str("Return"),
            c => out.push(c),
        }
    } else if val.is_symbol() {
        let name = sym_name(val);
        if let Some(bare) = name.strip_prefix("KEYWORD:") {
            out.push(':');
            out.push_str(bare);
        } else {
            out.push_str(&name);
        }
    } else if val.is_cons() {
        out.push('(');
        print_list_body(val, out);
        out.push(')');
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
                        for c in s.chars() {
                            if c == '"' || c == '\\' {
                                out.push('\\');
                            }
                            out.push(c);
                        }
                        out.push('"');
                    } else {
                        out.push_str("#<string>");
                    }
                }
                type_id::SIMPLE_VECTOR => {
                    let len = *(ptr.add(8) as *const u64) as usize;
                    out.push_str("#(");
                    for i in 0..len {
                        if i > 0 {
                            out.push(' ');
                        }
                        print_val(*(ptr.add(16 + i * 8) as *const BlissVal), out);
                    }
                    out.push(')');
                }
                type_id::RATIO => {
                    let num = *(ptr.add(8) as *const BlissVal);
                    let den = *(ptr.add(16) as *const BlissVal);
                    print_val(num, out);
                    out.push('/');
                    print_val(den, out);
                }
                type_id::BIGNUM => {
                    let sign = *(ptr.add(8) as *const i32);
                    let n = *(ptr.add(12) as *const u32) as usize;
                    let mut limbs: Vec<u64> = Vec::with_capacity(n);
                    for i in 0..n {
                        limbs.push(*(ptr.add(16 + i * 8) as *const u64));
                    }
                    out.push_str(&bignum_to_decimal(sign, &limbs));
                }
                _ => out.push_str(&format!("#<heap-object type={}>", hdr.type_id())),
            }
        }
    } else {
        out.push_str(&format!("#<unknown {:#x}>", val.0));
    }
}

/// Render a bignum (little-endian base-2^64 limbs) as a decimal string.
fn bignum_to_decimal(sign: i32, limbs: &[u64]) -> String {
    if sign == 0 || limbs.iter().all(|&l| l == 0) {
        return "0".into();
    }
    const D: u128 = 1_000_000_000;
    let mut work = limbs.to_vec();
    let mut chunks: Vec<u32> = Vec::new();
    loop {
        let mut rem: u128 = 0;
        for limb in work.iter_mut().rev() {
            let cur = (rem << 64) | (*limb as u128);
            *limb = (cur / D) as u64;
            rem = cur % D;
        }
        chunks.push(rem as u32);
        while work.len() > 1 && *work.last().unwrap() == 0 {
            work.pop();
        }
        if work.len() == 1 && work[0] == 0 {
            break;
        }
    }
    let mut s = String::new();
    if sign < 0 {
        s.push('-');
    }
    for (i, chunk) in chunks.iter().rev().enumerate() {
        if i == 0 {
            s.push_str(&chunk.to_string());
        } else {
            s.push_str(&format!("{:09}", chunk));
        }
    }
    s
}

fn print_list_body(val: BlissVal, out: &mut String) {
    let mut cur = val;
    let mut first = true;
    while cur.is_cons() {
        if !first {
            out.push(' ');
        }
        first = false;
        unsafe {
            let c = cur.as_ptr() as *const ConsCell;
            print_val((*c).car, out);
            cur = (*c).cdr;
        }
    }
    if !cur.is_nil() {
        out.push_str(" . ");
        print_val(cur, out);
    }
}

fn format_val(val: BlissVal) -> String {
    let mut s = String::new();
    print_val(val, &mut s);
    s
}

fn simple_format_message(control: &str, args: &[BlissVal]) -> String {
    let mut rendered = String::new();
    let mut chars = control.chars().peekable();
    let mut arg_index = 0usize;
    while let Some(ch) = chars.next() {
        if ch == '~' {
            if let Some(directive) = chars.next() {
                match directive {
                    'A' | 'a' => {
                        if let Some(arg) = args.get(arg_index) {
                            let mut out = String::new();
                            princ_val(*arg, &mut out);
                            rendered.push_str(&out);
                            arg_index += 1;
                            continue;
                        }
                    }
                    'S' | 's' => {
                        if let Some(arg) = args.get(arg_index) {
                            rendered.push_str(&format_val(*arg));
                            arg_index += 1;
                            continue;
                        }
                    }
                    '~' => {
                        rendered.push('~');
                        continue;
                    }
                    _ => {
                        rendered.push('~');
                        rendered.push(directive);
                        continue;
                    }
                }
            }
        }
        rendered.push(ch);
    }
    rendered
}

fn princ_val(val: BlissVal, out: &mut String) {
    if val.is_heap_object() {
        unsafe {
            let ptr = val.as_ptr();
            let hdr = *(ptr as *const ObjectHeader);
            if hdr.type_id() == type_id::SIMPLE_BASE_STRING {
                let len = *(ptr.add(8) as *const u64) as usize;
                let data = std::slice::from_raw_parts(ptr.add(16), len);
                if let Ok(s) = std::str::from_utf8(data) {
                    out.push_str(s);
                    return;
                }
            }
        }
    }
    if val.is_character() {
        out.push(val.as_char());
        return;
    }
    if val.is_symbol() {
        let name = sym_name(val);
        // For ~A, print symbol name without package prefix
        let bare = name.trim_start_matches("KEYWORD:");
        out.push_str(bare);
        return;
    }
    print_val(val, out);
}

// ── Symbol name lookup ────────────────────────────────────────────
fn sym_name(val: BlissVal) -> String {
    if val.is_nil() {
        return "NIL".into();
    }
    if val == T {
        return "T".into();
    }
    if !val.is_symbol() {
        return String::new();
    }
    let idx = val.as_symbol_index();
    // Use the reader's reverse symbol table for O(1) lookup
    if let Some(name) = reader::symbol_name(idx) {
        return name;
    }
    format!("SYM#{}", idx)
}

fn resolve_sym(name: &str) -> Option<BlissVal> {
    match reader::read_from_string(name) {
        Ok((sym, _)) if sym.is_symbol() => Some(sym),
        _ => None,
    }
}

// ── Collect a list into a Vec of elements ─────────────────────────
fn list_to_vec(val: BlissVal) -> Vec<BlissVal> {
    let mut result = Vec::new();
    let mut c = val;
    while c.is_cons() {
        let (car, cdr) = cp(c);
        result.push(car);
        c = cdr;
    }
    result
}

fn vec_to_list(elems: &[BlissVal]) -> BlissVal {
    let mut result = NIL;
    for e in elems.iter().rev() {
        result = arena_cons(*e, result);
    }
    result
}

fn format_body_forms(forms: BlissVal) -> String {
    let parts = list_to_vec(forms)
        .into_iter()
        .map(format_val)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "NIL".to_string()
    } else {
        parts.join(" ")
    }
}

// ── Quasiquote expansion ─────────────────────────────────────────
/// Expand a quasiquote template, substituting BLISS::UNQUOTE forms with
/// their evaluated values and splicing BLISS::UNQUOTE-SPLICING forms.
fn eval_quasiquote(template: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    // If template is not a cons, return it as-is (like quote)
    if !template.is_cons() {
        return Ok(template);
    }
    let (car, cdr) = cp(template);

    // Check if template is (BLISS::UNQUOTE expr)
    if car.is_symbol() && sym_name(car) == "BLISS::UNQUOTE" {
        let (expr, _) = cp(cdr);
        return eval_form(expr, env);
    }

    // Check if template is (BLISS::UNQUOTE-SPLICING expr) at top level — error
    if car.is_symbol() && sym_name(car) == "BLISS::UNQUOTE-SPLICING" {
        return Err(BlissError::Internal(",@ not inside a list".into()));
    }

    // Recursively process each element of the list, handling splicing
    let mut result_elems: Vec<BlissVal> = Vec::new();
    let mut cur = template;
    while cur.is_cons() {
        let (elem, rest) = cp(cur);
        // Check if elem is (BLISS::UNQUOTE-SPLICING expr)
        if elem.is_cons() {
            let (ecar, ecdr) = cp(elem);
            if ecar.is_symbol() && sym_name(ecar) == "BLISS::UNQUOTE-SPLICING" {
                let (splice_expr, _) = cp(ecdr);
                let splice_val = eval_form(splice_expr, env)?;
                // Splice the list into the result
                let spliced = list_to_vec(splice_val);
                result_elems.extend(spliced);
                cur = rest;
                continue;
            }
        }
        // Regular element — recursively expand
        let expanded = eval_quasiquote(elem, env)?;
        result_elems.push(expanded);
        cur = rest;
    }
    // Handle dotted pair tail
    if !cur.is_nil() {
        let expanded_tail = eval_quasiquote(cur, env)?;
        // Build from the back with the non-nil tail
        let mut result = expanded_tail;
        for e in result_elems.iter().rev() {
            result = arena_cons(*e, result);
        }
        return Ok(result);
    }
    Ok(vec_to_list(&result_elems))
}

// ── Closure ID generation ────────────────────────────────────────
thread_local! {
    static NEXT_CLOSURE_ID: RefCell<u64> = const { RefCell::new(1) };
    static NEXT_STDLIB_CLASS_ID: RefCell<i64> = const { RefCell::new(300_000) };
}

fn next_closure_id() -> u64 {
    NEXT_CLOSURE_ID.with(|c| {
        let v = *c.borrow();
        *c.borrow_mut() = v + 1;
        v
    })
}

fn next_stdlib_class_id() -> BlissVal {
    NEXT_STDLIB_CLASS_ID.with(|c| {
        let v = *c.borrow();
        *c.borrow_mut() = v + 1;
        BlissVal::from_fixnum(v)
    })
}

fn register_declared_packages(source: &str) {
    let chars: Vec<char> = source.chars().collect();
    let mut pos = 0;
    while pos < chars.len() {
        match chars[pos] {
            ';' => {
                while pos < chars.len() && chars[pos] != '\n' {
                    pos += 1;
                }
            }
            '"' => {
                pos += 1;
                while pos < chars.len() {
                    if chars[pos] == '\\' {
                        pos += 2;
                    } else if chars[pos] == '"' {
                        pos += 1;
                        break;
                    } else {
                        pos += 1;
                    }
                }
            }
            '(' => {
                pos += 1;
                let op = read_scan_token(&chars, &mut pos);
                if op.eq_ignore_ascii_case("DEFPACKAGE") {
                    let pkg = read_scan_token(&chars, &mut pos);
                    let pkg = pkg
                        .trim_start_matches(':')
                        .trim_start_matches("KEYWORD:")
                        .trim();
                    if !pkg.is_empty() {
                        reader::register_package(&pkg.to_uppercase());
                    }
                }
            }
            _ => pos += 1,
        }
    }
}

fn read_scan_token(chars: &[char], pos: &mut usize) -> String {
    while *pos < chars.len() && chars[*pos].is_ascii_whitespace() {
        *pos += 1;
    }
    if *pos >= chars.len() {
        return String::new();
    }
    let start = *pos;
    while *pos < chars.len() {
        let ch = chars[*pos];
        if ch.is_ascii_whitespace() || matches!(ch, '(' | ')' | '"' | ';') {
            break;
        }
        *pos += 1;
    }
    chars[start..*pos].iter().collect()
}

// ── Minimal bootstrap evaluator ───────────────────────────────────
fn read_eval_all_env(source: &str, env: &mut Env) -> Result<BlissVal, BlissError> {
    register_declared_packages(source);
    let chars: Vec<char> = source.chars().collect();
    let mut pos = 0;
    let mut last = NIL;
    loop {
        while pos < chars.len() && chars[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= chars.len() {
            break;
        }
        let remaining: String = chars[pos..].iter().collect();
        let (val, consumed) = reader::read_from_string(&remaining)?;
        if val == EOF {
            break;
        }
        last = eval_form(val, env)?;
        pos += consumed;
    }
    Ok(last)
}

fn bundled_asdf_path() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../lib/asdf.lisp")
        .to_string_lossy()
        .into_owned()
}

/// The bootstrap prelude, embedded at compile time so it is always available
/// regardless of where the binary runs from.
const EMBEDDED_BOOT_LISP: &str = include_str!("../../../lib/boot.lisp");

/// Source of the bootstrap prelude: the file named by BLISS_BOOT_FILE if set
/// (for testing alternate preludes), otherwise the embedded copy.
fn boot_prelude_source() -> Result<String, BlissError> {
    if let Ok(p) = std::env::var("BLISS_BOOT_FILE") {
        return std::fs::read_to_string(&p).map_err(|e| {
            BlissError::FileError(format!("cannot read BLISS_BOOT_FILE {}: {}", p, e))
        });
    }
    Ok(EMBEDDED_BOOT_LISP.to_string())
}

// Reserved for the stage-6 ASDF output-translations wiring; retained so the
// default cache location stays defined in one place until `require`/ASDF is live.
#[allow(dead_code)]
fn default_asdf_output_translations() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    format!("{home}/.cache/bliss/asdf/")
}

fn normalize_package_name(name: &str) -> String {
    name.trim_start_matches("KEYWORD:")
        .trim_start_matches(':')
        .to_uppercase()
}

fn symbol_bare_name(name: &str) -> String {
    let without_keyword = name.trim_start_matches("KEYWORD:");
    let base = without_keyword
        .rsplit_once("::")
        .map(|(_, tail)| tail)
        .or_else(|| without_keyword.rsplit_once(':').map(|(_, tail)| tail))
        .unwrap_or(without_keyword);
    base.to_uppercase()
}

fn package_status_symbol(status: &str) -> BlissVal {
    resolve_sym(&format!(":{}", status)).unwrap_or(NIL)
}

fn symbol_for_package(pkg_name: &str, bare_name: &str) -> Option<BlissVal> {
    let pkg_name = normalize_package_name(pkg_name);
    let bare_name = bare_name.to_uppercase();
    let candidates = if pkg_name == "COMMON-LISP" || pkg_name == "COMMON-LISP-USER" {
        vec![bare_name.clone()]
    } else if pkg_name == "KEYWORD" {
        vec![format!(":{}", bare_name)]
    } else {
        vec![
            format!("{pkg_name}:{bare_name}"),
            format!("{pkg_name}::{bare_name}"),
            bare_name.clone(),
        ]
    };
    candidates
        .into_iter()
        .find_map(|candidate| resolve_sym(&candidate))
}

fn intern_into_package(env: &mut Env, pkg_name: &str, bare_name: &str) -> BlissVal {
    let pkg_name = normalize_package_name(pkg_name);
    let bare_name = bare_name.to_uppercase();
    let sym = symbol_for_package(&pkg_name, &bare_name)
        .or_else(|| resolve_sym(&bare_name))
        .unwrap_or_else(|| arena_str(&bare_name));
    ensure_package_available(env, &pkg_name, &[]);
    Rc::make_mut(&mut env.packages)
        .get_mut(&pkg_name)
        .expect("package exists")
        .symbols
        .insert(bare_name, sym);
    sym
}

fn find_symbol_in_package(
    env: &Env,
    pkg_name: &str,
    bare_name: &str,
) -> Option<(BlissVal, &'static str)> {
    let pkg_name = normalize_package_name(pkg_name);
    let bare_name = bare_name.to_uppercase();
    if pkg_name == "COMMON-LISP" || pkg_name == "COMMON-LISP-USER" {
        if let Some(sym) = resolve_sym(&bare_name) {
            return Some((sym, "EXTERNAL"));
        }
    }
    if pkg_name == "KEYWORD" {
        if let Some(sym) = resolve_sym(&format!(":{}", bare_name)) {
            return Some((sym, "EXTERNAL"));
        }
    }
    let package = env.packages.get(&pkg_name)?;
    if let Some(sym) = package.symbols.get(&bare_name) {
        let status = if package.exports.iter().any(|name| name == &bare_name) {
            "EXTERNAL"
        } else {
            "INTERNAL"
        };
        return Some((*sym, status));
    }
    for used in &package.uses {
        if let Some((sym, _)) = find_symbol_in_package(env, used, &bare_name) {
            return Some((sym, "INHERITED"));
        }
    }
    None
}

fn ensure_package_available(env: &mut Env, name: &str, uses: &[&str]) {
    let packages = Rc::make_mut(&mut env.packages);
    packages
        .entry(name.to_string())
        .or_insert_with(|| PackageDef {
            name: name.to_string(),
            exports: Vec::new(),
            uses: uses.iter().map(|pkg| (*pkg).to_string()).collect(),
            symbols: HashMap::new(),
        });
    reader::register_package(name);
}

fn plist_get(list: BlissVal, key: &str) -> Option<BlissVal> {
    let mut cur = list;
    while cur.is_cons() {
        let (entry, rest) = cp(cur);
        if entry.is_cons() {
            let (entry_key, entry_vals) = cp(entry);
            if symbol_bare_name(&val_as_str(entry_key)) == key {
                return Some(cp(entry_vals).0);
            }
        }
        cur = rest;
    }
    None
}

fn plist_entry(list: BlissVal, key: &str) -> Option<BlissVal> {
    let mut cur = list;
    while cur.is_cons() {
        let (entry, rest) = cp(cur);
        if entry.is_cons() {
            let (entry_key, _) = cp(entry);
            if symbol_bare_name(&val_as_str(entry_key)) == key {
                return Some(entry);
            }
        }
        cur = rest;
    }
    None
}

fn resolve_type_spec(env: &Env, type_spec: BlissVal) -> BlissVal {
    if type_spec.is_symbol() {
        let name = symbol_bare_name(&sym_name(type_spec));
        if let Some(expanded) =
            plist_get(env.lookup_var("*TYPE-DEFINITIONS*").unwrap_or(NIL), &name)
        {
            return expanded;
        }
    }
    type_spec
}

fn condition_definition_entry(env: &Env, type_name: &str) -> Option<BlissVal> {
    plist_entry(
        env.lookup_var("*CONDITION-DEFINITIONS*").unwrap_or(NIL),
        &symbol_bare_name(type_name),
    )
}

/// A condition definition: its parent type names and its `(slot, initarg)` slots.
type ConditionDefinition = (Vec<String>, Vec<(String, String)>);

fn builtin_condition_definition(type_name: &str) -> Option<ConditionDefinition> {
    match symbol_bare_name(type_name).as_str() {
        "CONDITION" => Some((vec![], vec![])),
        "SERIOUS-CONDITION" => Some((vec!["CONDITION".into()], vec![])),
        "ERROR" => Some((vec!["SERIOUS-CONDITION".into()], vec![])),
        "WARNING" => Some((vec!["CONDITION".into()], vec![])),
        "SIMPLE-CONDITION" => Some((
            vec!["CONDITION".into()],
            vec![
                ("FORMAT-CONTROL".into(), "FORMAT-CONTROL".into()),
                ("FORMAT-ARGUMENTS".into(), "FORMAT-ARGUMENTS".into()),
            ],
        )),
        "SIMPLE-ERROR" => Some((vec!["ERROR".into(), "SIMPLE-CONDITION".into()], vec![])),
        "SIMPLE-WARNING" => Some((vec!["WARNING".into(), "SIMPLE-CONDITION".into()], vec![])),
        "TYPE-ERROR" => Some((
            vec!["ERROR".into()],
            vec![
                ("DATUM".into(), "DATUM".into()),
                ("EXPECTED-TYPE".into(), "EXPECTED-TYPE".into()),
            ],
        )),
        "CONTROL-ERROR" => Some((vec!["ERROR".into()], vec![])),
        _ => None,
    }
}

fn condition_slot_specs(env: &Env, type_name: &str) -> Vec<(String, String)> {
    let mut specs = Vec::new();
    let type_name = symbol_bare_name(type_name);
    if let Some((parents, own_slots)) = builtin_condition_definition(&type_name) {
        for parent in parents {
            specs.extend(condition_slot_specs(env, &parent));
        }
        specs.extend(own_slots);
        return specs;
    }

    if let Some(entry) = condition_definition_entry(env, &type_name) {
        let (_, rest) = cp(entry);
        let (parents_form, rest2) = cp(rest);
        let (slots_form, _) = cp(rest2);
        for parent in list_to_vec(parents_form) {
            specs.extend(condition_slot_specs(env, &sym_name(parent)));
        }
        for slot in list_to_vec(slots_form) {
            if slot.is_symbol() {
                let name = symbol_bare_name(&sym_name(slot));
                specs.push((name.clone(), name));
                continue;
            }
            if slot.is_cons() {
                let (slot_name_form, opts_form) = cp(slot);
                let slot_name = symbol_bare_name(&sym_name(slot_name_form));
                let mut initarg = slot_name.clone();
                let opts = list_to_vec(opts_form);
                let mut i = 0;
                while i + 1 < opts.len() {
                    let opt_name = symbol_bare_name(&sym_name(opts[i]));
                    if opt_name == "INITARG" {
                        initarg = symbol_bare_name(&sym_name(opts[i + 1]));
                    }
                    i += 2;
                }
                specs.push((slot_name, initarg));
            }
        }
    }

    specs
}

fn condition_default_initargs(env: &Env, type_name: &str) -> Vec<(String, BlissVal)> {
    let mut defaults = Vec::new();
    let type_name = symbol_bare_name(type_name);
    if let Some(entry) = condition_definition_entry(env, &type_name) {
        let (_, rest) = cp(entry);
        let (parents_form, rest2) = cp(rest);
        let (_, rest3) = cp(rest2);
        let (options_form, _) = cp(rest3);
        for parent in list_to_vec(parents_form) {
            defaults.extend(condition_default_initargs(env, &sym_name(parent)));
        }
        for option in list_to_vec(options_form) {
            if !option.is_cons() {
                continue;
            }
            let (name_form, values_form) = cp(option);
            if symbol_bare_name(&sym_name(name_form)) != "DEFAULT-INITARGS" {
                continue;
            }
            let values = list_to_vec(values_form);
            let mut i = 0;
            while i + 1 < values.len() {
                defaults.push((symbol_bare_name(&sym_name(values[i])), values[i + 1]));
                i += 2;
            }
        }
    }
    defaults
}

fn ensure_condition_class_registered(env: &Env, type_name: &str) -> Result<BlissVal, BlissError> {
    let type_sym = resolve_sym(&symbol_bare_name(type_name)).unwrap_or(NIL);
    if let Some(class) = bliss_stdlib::find_class(type_sym) {
        return Ok(class);
    }

    let mut parent_names = Vec::new();
    if let Some((builtin_parents, _)) = builtin_condition_definition(type_name) {
        parent_names.extend(builtin_parents);
    } else if let Some(entry) = condition_definition_entry(env, type_name) {
        let (_, rest) = cp(entry);
        let (parents_form, _) = cp(rest);
        for parent in list_to_vec(parents_form) {
            parent_names.push(symbol_bare_name(&sym_name(parent)));
        }
    } else {
        parent_names.push("CONDITION".into());
    }

    let mut supers = Vec::new();
    for parent in parent_names {
        supers.push(ensure_condition_class_registered(env, &parent)?);
    }

    let slot_names: Vec<BlissVal> = condition_slot_specs(env, type_name)
        .into_iter()
        .map(|(slot_name, _)| resolve_sym(&slot_name).unwrap_or(NIL))
        .collect();
    let class = next_stdlib_class_id();
    bliss_stdlib::define_class(type_sym, class, &supers, &slot_names)?;
    Ok(class)
}

fn condition_type_hierarchy_names(cond: BlissVal) -> Option<Vec<String>> {
    let class = bliss_stdlib::class_of(cond);
    let cpl = bliss_stdlib::compute_class_precedence_list(class).ok()?;
    let mut names = Vec::new();
    for class in cpl {
        let name = bliss_stdlib::class_name(class);
        if name.is_symbol() {
            names.push(symbol_bare_name(&sym_name(name)));
        }
    }
    if names.iter().any(|name| name == "CONDITION") {
        Some(names)
    } else {
        None
    }
}

fn condition_supertypes(env: &Env, type_name: &str) -> Vec<String> {
    let mut supers = Vec::new();
    let mut cur = plist_get(
        env.lookup_var("*CONDITION-TYPES*").unwrap_or(NIL),
        &symbol_bare_name(type_name),
    );
    while let Some(list) = cur {
        for sup in list_to_vec(list) {
            supers.push(symbol_bare_name(&val_as_str(sup)));
        }
        cur = None;
    }
    supers
}

fn condition_type_matches(env: &Env, signaled_type: &str, handler_type: &str) -> bool {
    let signaled = symbol_bare_name(signaled_type);
    let handler = symbol_bare_name(handler_type);
    handler == "T"
        || signaled == handler
        || condition_supertypes(env, &signaled)
            .iter()
            .any(|sup| sup == &handler)
}

fn condition_matches_handler(env: &Env, condition: BlissVal, handler_type: &str) -> bool {
    let handler = symbol_bare_name(handler_type);
    if handler == "T" {
        return true;
    }
    if let Some(hierarchy) = condition_type_hierarchy_names(condition) {
        return hierarchy.iter().any(|name| name == &handler);
    }
    let signaled_type = if condition.is_symbol() {
        sym_name(condition)
    } else {
        val_as_str(condition)
    };
    condition_type_matches(env, &signaled_type, handler_type)
}

fn is_package_value(env: &Env, value: BlissVal) -> bool {
    if !value.is_string() {
        return false;
    }
    let pkg_name = normalize_package_name(&val_as_str(value));
    env.packages.contains_key(&pkg_name)
        || matches!(
            pkg_name.as_str(),
            "COMMON-LISP" | "COMMON-LISP-USER" | "KEYWORD" | "BLISS-EXT"
        )
}

fn typep_matches(env: &mut Env, object: BlissVal, type_spec: BlissVal) -> Result<bool, BlissError> {
    let type_spec = resolve_type_spec(env, type_spec);
    if type_spec.is_symbol() {
        let type_name = symbol_bare_name(&sym_name(type_spec));
        let matches = match type_name.as_str() {
            "T" => true,
            "NIL" | "NULL" => object.is_nil(),
            "ATOM" => !object.is_cons(),
            "LIST" => object.is_list(),
            "CONS" => object.is_cons(),
            "SYMBOL" => object.is_symbol(),
            "STRING" | "SIMPLE-STRING" | "BASE-STRING" => object.is_string(),
            "NUMBER" | "REAL" => object.is_fixnum() || object.is_single_float(),
            "INTEGER" | "FIXNUM" => object.is_fixnum(),
            "FLOAT" | "SINGLE-FLOAT" => object.is_single_float(),
            "CHARACTER" => object.is_character(),
            "BOOLEAN" => object.is_nil() || object == T,
            "FUNCTION" => object.is_symbol() || object.is_cons(),
            "PACKAGE" => is_package_value(env, object),
            "HASH-TABLE" => bliss_stdlib::hash_table_count(object).is_ok(),
            "PATHNAME" => bliss_stdlib::namestring(object).is_ok(),
            "STREAM" | "FILE-STREAM" | "SYNONYM-STREAM" => is_stream(object),
            other => {
                if let Some(hierarchy) = condition_type_hierarchy_names(object) {
                    hierarchy.iter().any(|name| name == other)
                } else {
                    other == symbol_bare_name(&val_as_str(object))
                }
            }
        };
        return Ok(matches);
    }

    if !type_spec.is_cons() {
        return Ok(object == type_spec);
    }

    let (head, args) = cp(type_spec);
    let op = symbol_bare_name(&sym_name(head));
    match op.as_str() {
        "OR" => {
            for spec in list_to_vec(args) {
                if typep_matches(env, object, spec)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "AND" => {
            for spec in list_to_vec(args) {
                if !typep_matches(env, object, spec)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "MEMBER" => Ok(list_to_vec(args)
            .into_iter()
            .any(|candidate| vals_equal(object, candidate))),
        "EQL" => {
            let (value, _) = cp(args);
            Ok(object == value)
        }
        "INTEGER" => {
            if !object.is_fixnum() {
                return Ok(false);
            }
            let bounds = list_to_vec(args);
            let value = object.as_fixnum();
            let lower_ok = bounds
                .first()
                .copied()
                .map(|bound| {
                    if bound.is_symbol() && symbol_bare_name(&sym_name(bound)) == "*" {
                        true
                    } else {
                        value >= bound.as_fixnum()
                    }
                })
                .unwrap_or(true);
            let upper_ok = bounds
                .get(1)
                .copied()
                .map(|bound| {
                    if bound.is_symbol() && symbol_bare_name(&sym_name(bound)) == "*" {
                        true
                    } else {
                        value <= bound.as_fixnum()
                    }
                })
                .unwrap_or(true);
            Ok(lower_ok && upper_ok)
        }
        "SATISFIES" => {
            let (predicate, _) = cp(args);
            let predicate_name = sym_name(predicate);
            if predicate_name == "FIND-PACKAGE" {
                let designator =
                    if object.is_character() || object.is_string() || object.is_symbol() {
                        val_as_str(object)
                    } else {
                        return Ok(false);
                    };
                let pkg_name = normalize_package_name(&designator);
                return Ok(env.packages.contains_key(&pkg_name)
                    || matches!(
                        pkg_name.as_str(),
                        "COMMON-LISP" | "COMMON-LISP-USER" | "KEYWORD" | "BLISS-EXT"
                    ));
            }
            Ok(!apply_function(predicate, &[object], env)?.is_nil())
        }
        _ => Ok(false),
    }
}

fn package_symbols(env: &Env, package_name: &str, include_inherited: bool) -> Vec<BlissVal> {
    let package_name = normalize_package_name(package_name);
    let mut seen = HashMap::<String, BlissVal>::new();
    if matches!(package_name.as_str(), "COMMON-LISP" | "COMMON-LISP-USER") {
        for idx in 0..4096u32 {
            if let Some(name) = reader::symbol_name(idx) {
                if !name.contains(':') {
                    seen.entry(name.clone())
                        .or_insert(BlissVal::from_symbol_index(idx));
                }
            }
        }
    } else if package_name == "KEYWORD" {
        for idx in 0..4096u32 {
            if let Some(name) = reader::symbol_name(idx) {
                if name.starts_with(':') {
                    seen.entry(name.clone())
                        .or_insert(BlissVal::from_symbol_index(idx));
                }
            }
        }
    }
    if let Some(pkg) = env.packages.get(&package_name) {
        for (name, sym) in &pkg.symbols {
            seen.entry(name.clone()).or_insert(*sym);
        }
        if include_inherited {
            for used in &pkg.uses {
                for sym in package_symbols(env, used, false) {
                    seen.entry(val_as_str(sym)).or_insert(sym);
                }
            }
        }
    }
    seen.into_values().collect()
}

fn load_path_into_env(path: &str, env: &mut Env) -> Result<BlissVal, BlissError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| BlissError::FileError(format!("cannot read {}: {}", path, e)))?;
    with_eval_context(env, EvalContext::Load, |env| {
        read_eval_all_env(&contents, env)
    })
}

fn require_module(module: &str, env: &mut Env) -> Result<BlissVal, BlissError> {
    let normalized = module
        .trim_start_matches("KEYWORD:")
        .trim_start_matches(':')
        .trim_matches('"')
        .to_uppercase();
    if normalized == "ASDF" {
        load_path_into_env(&bundled_asdf_path(), env)?;
        return Ok(T);
    }

    let providers = list_to_vec(env.lookup_var("*MODULE-PROVIDER-FUNCTIONS*").unwrap_or(NIL));
    for provider in providers {
        let provided = apply_function(provider, &[arena_str(&normalized)], env)?;
        if !provided.is_nil() {
            return Ok(provided);
        }
    }

    let candidate = format!("{}.lisp", normalized.to_ascii_lowercase());
    load_path_into_env(&candidate, env)
}

#[allow(dead_code)]
fn read_eval_all(source: &str) -> Result<BlissVal, BlissError> {
    let mut env = Env::new(false);
    read_eval_all_env(source, &mut env)
}

fn eval_form(form: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    if form.is_nil() || form == T {
        return Ok(form);
    }
    if form.is_fixnum() || form.is_single_float() || form.is_character() {
        return Ok(form);
    }
    if form.is_heap_object() {
        unsafe {
            let hdr = *(form.as_ptr() as *const ObjectHeader);
            if hdr.type_id() == type_id::SIMPLE_BASE_STRING {
                return Ok(form);
            }
        }
    }
    if form.is_symbol() {
        let name = sym_name(form);
        // Keyword symbols are self-evaluating
        if name.starts_with("KEYWORD:") {
            return Ok(form);
        }
        // Check variable environment
        if let Some(val) = env.lookup_var(&name) {
            return Ok(val);
        }
        return Err(BlissError::UnboundVariable(form));
    }
    if form.is_cons() {
        return eval_list(form, env);
    }
    Ok(form)
}

fn canonical_type_name(type_form: BlissVal) -> Result<String, BlissError> {
    if type_form.is_symbol() {
        return Ok(sym_name(type_form)
            .trim_start_matches("COMMON-LISP:")
            .to_string());
    }
    if type_form.is_cons() {
        let (head, rest) = cp(type_form);
        if head.is_symbol() && sym_name(head) == "QUOTE" {
            let (quoted, _) = cp(rest);
            if quoted.is_symbol() {
                return Ok(sym_name(quoted)
                    .trim_start_matches("COMMON-LISP:")
                    .to_string());
            }
        }
    }
    Err(BlissError::TypeError {
        datum: type_form,
        expected: "type specifier".into(),
    })
}

fn value_satisfies_declared_type(type_form: BlissVal, value: BlissVal) -> Result<bool, BlissError> {
    let type_name = canonical_type_name(type_form)?;
    Ok(match type_name.as_str() {
        "T" => true,
        "NIL" | "NULL" => value.is_nil(),
        "BOOLEAN" => value.is_nil() || value == T,
        "SYMBOL" => value.is_symbol(),
        "KEYWORD" => value.is_symbol() && sym_name(value).starts_with("KEYWORD:"),
        "CHARACTER" | "BASE-CHAR" | "STANDARD-CHAR" => value.is_character(),
        "STRING" | "SIMPLE-STRING" | "SIMPLE-BASE-STRING" => value.is_string(),
        "INTEGER" | "FIXNUM" => value.is_fixnum(),
        "FLOAT" | "SINGLE-FLOAT" | "REAL" => value.is_single_float() || value.is_fixnum(),
        "RATIO" => ratio_parts_val(value).is_some(),
        "NUMBER" => {
            value.is_fixnum() || value.is_single_float() || ratio_parts_val(value).is_some()
        }
        "LIST" => value.is_list(),
        "CONS" => value.is_cons(),
        "ATOM" => !value.is_cons(),
        other => {
            if value.is_heap_object() {
                let header = unsafe { *(value.as_ptr() as *const ObjectHeader) };
                match other {
                    "VECTOR" | "SIMPLE-VECTOR" => header.type_id() == type_id::SIMPLE_VECTOR,
                    "HASH-TABLE" => header.type_id() == type_id::HASH_TABLE,
                    _ => false,
                }
            } else {
                false
            }
        }
    })
}

fn eval_when_should_run(situations: BlissVal, env: &Env) -> bool {
    let has_situation = |target: &str| {
        list_to_vec(situations).into_iter().any(|situation| {
            situation.is_symbol()
                && sym_name(situation)
                    .trim_start_matches("KEYWORD:")
                    .trim_start_matches("COMMON-LISP:")
                    == target
        })
    };

    match env.eval_context {
        EvalContext::Load => has_situation("LOAD-TOPLEVEL") || has_situation("EXECUTE"),
        EvalContext::Eval | EvalContext::Repl => has_situation("EXECUTE"),
    }
}

fn eval_form_collecting_values(
    form: BlissVal,
    env: &mut Env,
) -> Result<(BlissVal, Vec<BlissVal>), BlissError> {
    env.clear_mv();
    let primary = eval_form(form, env)?;
    let values = if env.mv_active {
        env.mv.clone()
    } else {
        vec![primary]
    };
    Ok((primary, values))
}

fn cp(val: BlissVal) -> (BlissVal, BlissVal) {
    if !val.is_cons() {
        return (NIL, NIL);
    }
    unsafe {
        let c = val.as_ptr() as *const ConsCell;
        ((*c).car, (*c).cdr)
    }
}

/// A TAGBODY tag is a symbol or an integer. Return its canonical string key,
/// or None if the form is a statement (a cons or other non-tag object).
fn tag_key(form: BlissVal) -> Option<String> {
    if form.is_cons() {
        return None;
    }
    if form.is_symbol() {
        return Some(sym_name(form));
    }
    if form.is_fixnum() {
        return Some(format!("#{}", form.as_fixnum()));
    }
    None
}

fn eval_list(form: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (car, cdr) = cp(form);
    if car.is_symbol() {
        let name = sym_name(car);

        // Check for macro expansion first
        if let Some(mdef) = env.macros.get(&name).cloned().or_else(|| {
            name.rsplit(':')
                .next()
                .and_then(|bare| env.macros.get(bare).cloned())
        }) {
            let expanded = expand_macro(&mdef, cdr, env)?;
            return eval_form(expanded, env);
        }

        match name.as_str() {
            "QUOTE" => {
                let (q, _) = cp(cdr);
                return Ok(q);
            }
            "BLISS::QUASIQUOTE" => {
                let (template, _) = cp(cdr);
                return eval_quasiquote(template, env);
            }
            "IF" => {
                let (test, r) = cp(cdr);
                let tv = eval_form(test, env)?;
                let (then, er) = cp(r);
                return if !tv.is_nil() {
                    eval_form(then, env)
                } else if er.is_cons() {
                    let (ef, _) = cp(er);
                    eval_form(ef, env)
                } else {
                    Ok(NIL)
                };
            }
            "PROGN" => return eval_progn(cdr, env),
            "DECLARE" => return Ok(NIL),
            "THE" => {
                let (type_form, r) = cp(cdr);
                let (val_form, _) = cp(r);
                let value = eval_form(val_form, env)?;
                if value_satisfies_declared_type(type_form, value)? {
                    return Ok(value);
                }
                return Err(BlissError::TypeError {
                    datum: value,
                    expected: canonical_type_name(type_form)?,
                });
            }
            "LOOP" => return eval_loop(cdr, env),
            "EVAL-WHEN" => {
                let (situations, body) = cp(cdr);
                return if eval_when_should_run(situations, env) {
                    eval_progn(body, env)
                } else {
                    Ok(NIL)
                };
            }
            "BLOCK" => {
                let (name_form, body) = cp(cdr);
                let name = sym_name(name_form);
                let token = next_control_token("__RETURN_FROM__");
                env.block_stack.push((name, token.clone()));
                let result = eval_progn(body, env);
                env.block_stack.pop();
                match result {
                    Err(BlissError::Internal(msg)) if msg == token => {
                        return Ok(take_control_value(&token));
                    }
                    other => return other,
                }
            }
            "RETURN-FROM" => {
                let (name_form, rest) = cp(cdr);
                let (val_form, _) = cp(rest);
                let name = sym_name(name_form);
                let value = eval_form(val_form, env)?;
                if let Some((_, token)) = env
                    .block_stack
                    .iter()
                    .rev()
                    .find(|(block_name, _)| block_name == &name)
                {
                    store_control_value(token, value);
                    return Err(BlissError::Internal(token.clone()));
                }
                return Err(BlissError::Internal(format!(
                    "RETURN-FROM: no block named {} is currently visible",
                    name
                )));
            }
            "RETURN" => {
                let (val_form, _) = cp(cdr);
                let value = eval_form(val_form, env)?;
                if let Some((_, token)) = env
                    .block_stack
                    .iter()
                    .rev()
                    .find(|(block_name, _)| block_name == "NIL")
                {
                    store_control_value(token, value);
                    return Err(BlissError::Internal(token.clone()));
                }
                return Err(BlissError::Internal(
                    "RETURN: no block named NIL is currently visible".into(),
                ));
            }
            "CATCH" => {
                let (tag_form, body) = cp(cdr);
                let tag = val_as_str(eval_form(tag_form, env)?);
                let token = next_control_token("__THROW__");
                env.catch_stack.push((tag, token.clone()));
                let result = eval_progn(body, env);
                env.catch_stack.pop();
                match result {
                    Err(BlissError::Internal(msg)) if msg == token => {
                        return Ok(take_control_value(&token));
                    }
                    other => return other,
                }
            }
            "THROW" => {
                let (tag_form, rest) = cp(cdr);
                let (val_form, _) = cp(rest);
                let tag = val_as_str(eval_form(tag_form, env)?);
                let value = eval_form(val_form, env)?;
                if let Some((_, token)) = env
                    .catch_stack
                    .iter()
                    .rev()
                    .find(|(catch_tag, _)| catch_tag == &tag)
                {
                    store_control_value(token, value);
                    return Err(BlissError::Internal(token.clone()));
                }
                return Err(BlissError::Internal(format!("uncaught throw to {}", tag)));
            }
            "TAGBODY" => {
                // (tagbody {tag | statement}*)
                // Tags are symbols or integers; statements are forms evaluated in
                // order. GO transfers control to a tag; TAGBODY returns NIL.
                let items = list_to_vec(cdr);
                let token = next_control_token("__GO__");
                // Record tag name -> statement index (index just after the tag).
                let mut tag_index: HashMap<String, usize> = HashMap::new();
                let base = env.tag_stack.len();
                for (i, item) in items.iter().enumerate() {
                    if let Some(name) = tag_key(*item) {
                        tag_index.entry(name.clone()).or_insert(i);
                        env.tag_stack.push((name, token.clone()));
                    }
                }
                let mut pc = 0usize;
                let result: Result<(), BlissError> = loop {
                    if pc >= items.len() {
                        break Ok(());
                    }
                    let item = items[pc];
                    if tag_key(item).is_some() {
                        pc += 1;
                        continue;
                    }
                    match eval_form(item, env) {
                        Ok(_) => {
                            pc += 1;
                        }
                        Err(BlissError::Internal(msg)) if msg == token => {
                            let target = val_as_str(take_control_value(&token));
                            match tag_index.get(&target) {
                                Some(idx) => {
                                    pc = *idx;
                                }
                                None => break Err(BlissError::Internal(msg)),
                            }
                        }
                        Err(e) => break Err(e),
                    }
                };
                env.tag_stack.truncate(base);
                match result {
                    Ok(()) => return Ok(NIL),
                    Err(e) => return Err(e),
                }
            }
            "GO" => {
                // (go tag) — tag is not evaluated.
                let (tag_form, _) = cp(cdr);
                let name = match tag_key(tag_form) {
                    Some(n) => n,
                    None => {
                        return Err(BlissError::Internal(
                            "GO: tag must be a symbol or integer".into(),
                        ));
                    }
                };
                if let Some((_, token)) = env
                    .tag_stack
                    .iter()
                    .rev()
                    .find(|(tag_name, _)| tag_name == &name)
                {
                    let token = token.clone();
                    store_control_value(&token, arena_str(&name));
                    return Err(BlissError::Internal(token));
                }
                return Err(BlissError::Internal(format!("GO: no such tag {}", name)));
            }
            "UNWIND-PROTECT" => {
                // (unwind-protect protected cleanup...) — cleanup runs whether the
                // protected form returns normally or exits non-locally.
                let (protected, cleanup) = cp(cdr);
                let result = eval_form(protected, env);
                match result {
                    Ok(v) => {
                        let saved_mv = env.mv.clone();
                        let saved_mv_active = env.mv_active;
                        eval_progn(cleanup, env)?;
                        env.mv = saved_mv;
                        env.mv_active = saved_mv_active;
                        return Ok(v);
                    }
                    Err(e) => match eval_progn(cleanup, env) {
                        Ok(_) => return Err(e),
                        Err(cleanup_exit) => return Err(cleanup_exit),
                    },
                }
            }
            "PRINT" => {
                let (a, _) = cp(cdr);
                let v = eval_form(a, env)?;
                println!("\n{}", format_val(v));
                return Ok(v);
            }
            "PRINC" => {
                let (a, _) = cp(cdr);
                let v = eval_form(a, env)?;
                let mut s = String::new();
                princ_val(v, &mut s);
                print!("{}", s);
                return Ok(v);
            }
            "TERPRI" => {
                println!();
                return Ok(NIL);
            }
            "FRESH-LINE" => {
                println!();
                return Ok(T);
            }
            "+" => return eval_arith(cdr, env, 0, 0.0, |a, b| a + b, bigrat_add),
            "-" => return eval_arith_sub(cdr, env),
            "*" => return eval_arith(cdr, env, 1, 1.0, |a, b| a * b, bigrat_mul),
            "/" => return eval_arith_div(cdr, env),
            "CONS" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(arena_cons(a, b));
            }
            "LIST" => {
                let mut elems = Vec::new();
                let mut c = cdr;
                while c.is_cons() {
                    let (ef, r) = cp(c);
                    elems.push(eval_form(ef, env)?);
                    c = r;
                }
                return Ok(vec_to_list(&elems));
            }
            "CAR" | "FIRST" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                if v.is_cons() {
                    let (a, _) = cp(v);
                    return Ok(a);
                }
                return Err(BlissError::TypeError {
                    datum: v,
                    expected: "list".into(),
                });
            }
            "CDR" | "REST" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                if v.is_cons() {
                    let (_, d) = cp(v);
                    return Ok(d);
                }
                return Err(BlissError::TypeError {
                    datum: v,
                    expected: "list".into(),
                });
            }
            "SECOND" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                if v.is_cons() {
                    let (_, d) = cp(v);
                    if d.is_cons() {
                        let (a, _) = cp(d);
                        return Ok(a);
                    }
                }
                return Ok(NIL);
            }
            "THIRD" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                let elems = list_to_vec(v);
                return Ok(if elems.len() >= 3 { elems[2] } else { NIL });
            }
            "ATOM" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_cons() { NIL } else { T });
            }
            "NULL" | "NOT" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_nil() { T } else { NIL });
            }
            "CONSP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_cons() { T } else { NIL });
            }
            "LISTP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_list() { T } else { NIL });
            }
            "NUMBERP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_fixnum() || v.is_single_float() {
                    T
                } else {
                    NIL
                });
            }
            "STRINGP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_string() { T } else { NIL });
            }
            "BOUNDP" => {
                let (sf, _) = cp(cdr);
                let sym = eval_form(sf, env)?;
                let name = if sym.is_symbol() {
                    sym_name(sym)
                } else {
                    val_as_str(sym)
                };
                return Ok(if env.lookup_var(&name).is_some() {
                    T
                } else {
                    NIL
                });
            }
            "SYMBOLP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_symbol() { T } else { NIL });
            }
            "TYPEP" => {
                let (obj_form, r) = cp(cdr);
                let (type_form, _) = cp(r);
                let obj = eval_form(obj_form, env)?;
                let raw_type_spec = eval_form(type_form, env)?;
                let matches = typep_matches(env, obj, raw_type_spec)?;
                return Ok(if matches { T } else { NIL });
            }
            "EQ" | "EQL" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if a == b { T } else { NIL });
            }
            "EQUAL" | "EQUALP" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if vals_equal(a, b) { T } else { NIL });
            }
            "=" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if numeric_cmp(a, b)? == Ordering::Equal {
                    T
                } else {
                    NIL
                });
            }
            "<" => {
                return eval_cmp(cdr, env, |o| o == Ordering::Less);
            }
            ">" => {
                return eval_cmp(cdr, env, |o| o == Ordering::Greater);
            }
            "<=" => {
                return eval_cmp(cdr, env, |o| o != Ordering::Greater);
            }
            ">=" => {
                return eval_cmp(cdr, env, |o| o != Ordering::Less);
            }
            "/=" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if numeric_cmp(a, b)? != Ordering::Equal {
                    T
                } else {
                    NIL
                });
            }
            "AND" => {
                let mut result = T;
                let mut c = cdr;
                while c.is_cons() {
                    let (f, r) = cp(c);
                    result = eval_form(f, env)?;
                    if result.is_nil() {
                        return Ok(NIL);
                    }
                    c = r;
                }
                return Ok(result);
            }
            "OR" => {
                let mut c = cdr;
                while c.is_cons() {
                    let (f, r) = cp(c);
                    let v = eval_form(f, env)?;
                    if !v.is_nil() {
                        return Ok(v);
                    }
                    c = r;
                }
                return Ok(NIL);
            }
            "WHEN" => {
                let (test, body) = cp(cdr);
                let tv = eval_form(test, env)?;
                if !tv.is_nil() {
                    return eval_progn(body, env);
                }
                return Ok(NIL);
            }
            "UNLESS" => {
                let (test, body) = cp(cdr);
                let tv = eval_form(test, env)?;
                if tv.is_nil() {
                    return eval_progn(body, env);
                }
                return Ok(NIL);
            }
            "COND" => {
                let mut c = cdr;
                while c.is_cons() {
                    let (clause, rest) = cp(c);
                    let (test, body) = cp(clause);
                    let tv = eval_form(test, env)?;
                    if !tv.is_nil() {
                        if body.is_nil() {
                            return Ok(tv);
                        }
                        return eval_progn(body, env);
                    }
                    c = rest;
                }
                return Ok(NIL);
            }
            "VALUES" => {
                let mut vals = Vec::new();
                let mut c = cdr;
                while c.is_cons() {
                    let (af, r) = cp(c);
                    vals.push(eval_form(af, env)?);
                    c = r;
                }
                if vals.is_empty() {
                    env.set_mv(Vec::new());
                    return Ok(NIL);
                }
                env.set_mv(vals.clone());
                return Ok(vals[0]);
            }
            "FORMAT" => return eval_format(cdr, env),
            "ERROR" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal("ERROR".into()));
                }
                let control = eval_form(args[0], env)?;
                let mut format_args = Vec::new();
                for arg in &args[1..] {
                    format_args.push(eval_form(*arg, env)?);
                }
                let message = if control.is_string() && !format_args.is_empty() {
                    simple_format_message(&val_as_str(control), &format_args)
                } else {
                    val_as_str(control)
                };
                return Err(BlissError::Internal(format!("ERROR: {}", message)));
            }
            "LET" => return eval_let(cdr, env, false),
            "LET*" => return eval_let(cdr, env, true),
            "SETQ" => {
                let mut c = cdr;
                let mut result = NIL;
                while c.is_cons() {
                    let (sym_form, r) = cp(c);
                    let (val_form, r2) = cp(r);
                    let name = sym_name(sym_form);
                    let val = eval_form(val_form, env)?;
                    env.set_var(&name, val);
                    result = val;
                    c = r2;
                }
                return Ok(result);
            }
            "SETF" => {
                // (setf place value place value ...) — symbol places behave like
                // SETQ; a handful of common accessor places are supported by
                // mutating the target in place. Other places error clearly.
                let mut c = cdr;
                let mut result = NIL;
                while c.is_cons() {
                    let (place, r) = cp(c);
                    let (val_form, r2) = cp(r);
                    let val = eval_form(val_form, env)?;
                    if place.is_symbol() {
                        let name = sym_name(place);
                        env.set_var(&name, val);
                    } else if place.is_cons() {
                        let (accessor, aargs) = cp(place);
                        let acc = if accessor.is_symbol() {
                            sym_name(accessor)
                        } else {
                            String::new()
                        };
                        let (tgt_form, _) = cp(aargs);
                        match acc.as_str() {
                            "CAR" | "FIRST" => {
                                let tgt = eval_form(tgt_form, env)?;
                                if tgt.is_cons() {
                                    unsafe {
                                        (*(tgt.as_ptr() as *mut ConsCell)).car = val;
                                    }
                                } else {
                                    return Err(BlissError::TypeError {
                                        datum: tgt,
                                        expected: "cons".into(),
                                    });
                                }
                            }
                            "CDR" | "REST" => {
                                let tgt = eval_form(tgt_form, env)?;
                                if tgt.is_cons() {
                                    unsafe {
                                        (*(tgt.as_ptr() as *mut ConsCell)).cdr = val;
                                    }
                                } else {
                                    return Err(BlissError::TypeError {
                                        datum: tgt,
                                        expected: "cons".into(),
                                    });
                                }
                            }
                            "GETHASH" => {
                                // (setf (gethash key table) val)
                                let key = eval_form(tgt_form, env)?;
                                let (tbl_form, _) = cp(cp(aargs).1);
                                let tbl = eval_form(tbl_form, env)?;
                                bliss_stdlib::set_gethash(key, tbl, val)?;
                            }
                            other => {
                                return Err(BlissError::Internal(format!(
                                    "SETF: unsupported place ({} ...)",
                                    other
                                )));
                            }
                        }
                    }
                    result = val;
                    c = r2;
                }
                return Ok(result);
            }
            "DEFUN" => return eval_defun(cdr, env),
            "FLET" | "LABELS" => return eval_flet(cdr, env),
            "DEFMACRO" => return eval_defmacro(cdr, env),
            "DEFCLASS" => return eval_defclass(cdr, env),
            "DEFMETHOD" => return eval_defmethod(cdr, env),
            "MAKE-INSTANCE" => return eval_make_instance(cdr, env),
            "FUNCTION" => {
                let (name_form, _) = cp(cdr);
                if name_form.is_symbol() {
                    let fn_name = sym_name(name_form);
                    if env.funs.contains_key(&fn_name) {
                        return Ok(name_form); // return the symbol as a function designator
                    }
                }
                // (function (lambda (params) body...)) — create a closure
                if name_form.is_cons() {
                    let (lh, lr) = cp(name_form);
                    if lh.is_symbol() && sym_name(lh) == "LAMBDA" {
                        let (params_form, body) = cp(lr);
                        // Capture the current lexical environment
                        let closure = Closure {
                            params_form,
                            body,
                            captured_frame: Rc::clone(&env.frame),
                        };
                        let id = next_closure_id();
                        env.closures.borrow_mut().insert(id, closure);
                        // Return a tagged closure reference as (BLISS::CLOSURE . id)
                        let closure_sym = resolve_sym("BLISS::CLOSURE").unwrap_or(NIL);
                        return Ok(arena_cons(closure_sym, BlissVal::from_fixnum(id as i64)));
                    }
                }
                return Ok(name_form);
            }
            "LAMBDA" => {
                // Bare (lambda (params) body...) — create a closure
                let (params_form, body) = cp(cdr);
                let closure = Closure {
                    params_form,
                    body,
                    captured_frame: Rc::clone(&env.frame),
                };
                let id = next_closure_id();
                env.closures.borrow_mut().insert(id, closure);
                let closure_sym = resolve_sym("BLISS::CLOSURE").unwrap_or(NIL);
                return Ok(arena_cons(closure_sym, BlissVal::from_fixnum(id as i64)));
            }
            "FUNCALL" => {
                let (fn_form, args_form) = cp(cdr);
                let fn_val = eval_form(fn_form, env)?;
                let mut args = Vec::new();
                let mut c = args_form;
                while c.is_cons() {
                    let (af, r) = cp(c);
                    args.push(eval_form(af, env)?);
                    c = r;
                }
                return apply_function(fn_val, &args, env);
            }
            "APPLY" => {
                let (fn_form, rest) = cp(cdr);
                let fn_val = eval_form(fn_form, env)?;
                let rest_items = list_to_vec(rest);
                let mut args = Vec::new();
                for (i, item) in rest_items.iter().enumerate() {
                    let v = eval_form(*item, env)?;
                    if i == rest_items.len() - 1 {
                        // Last arg should be a list to spread
                        let spread = list_to_vec(v);
                        args.extend(spread);
                    } else {
                        args.push(v);
                    }
                }
                return apply_function(fn_val, &args, env);
            }
            "LENGTH" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(BlissVal::from_fixnum(0));
                }
                if v.is_cons() {
                    let elems = list_to_vec(v);
                    return Ok(BlissVal::from_fixnum(elems.len() as i64));
                }
                if v.is_string() {
                    let s = val_as_str(v);
                    return Ok(BlissVal::from_fixnum(s.len() as i64));
                }
                return Ok(BlissVal::from_fixnum(0));
            }
            "APPEND" => {
                let mut all = Vec::new();
                let items = list_to_vec(cdr);
                if items.is_empty() {
                    return Ok(NIL);
                }
                for (i, item_form) in items.iter().enumerate() {
                    let v = eval_form(*item_form, env)?;
                    if i == items.len() - 1 && v.is_nil() {
                        continue;
                    }
                    all.extend(list_to_vec(v));
                }
                return Ok(vec_to_list(&all));
            }
            "REVERSE" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let mut elems = list_to_vec(v);
                elems.reverse();
                return Ok(vec_to_list(&elems));
            }
            "NTH" => {
                let (nf, r) = cp(cdr);
                let (lf, _) = cp(r);
                let n = eval_form(nf, env)?;
                let l = eval_form(lf, env)?;
                let idx = num_val(n)? as usize;
                let elems = list_to_vec(l);
                return Ok(if idx < elems.len() { elems[idx] } else { NIL });
            }
            "MAKE-HASH-TABLE" => {
                // (make-hash-table &key test size ...) — honor :test, evaluate
                // and ignore the rest. Backed by the real stdlib hash table.
                let mut test = bliss_stdlib::HashTest::Eql;
                let mut c = cdr;
                while c.is_cons() {
                    let (kw, r) = cp(c);
                    if !r.is_cons() {
                        break;
                    }
                    let (vf, r2) = cp(r);
                    let v = eval_form(vf, env)?;
                    if kw.is_symbol() {
                        let kn = sym_name(kw);
                        if kn.strip_prefix("KEYWORD:").unwrap_or(&kn) == "TEST" {
                            let tn = sym_name(v);
                            let tb = tn.strip_prefix("KEYWORD:").unwrap_or(&tn).to_uppercase();
                            test = match tb.as_str() {
                                "EQ" => bliss_stdlib::HashTest::Eq,
                                "EQUAL" => bliss_stdlib::HashTest::Equal,
                                "EQUALP" => bliss_stdlib::HashTest::Equalp,
                                _ => bliss_stdlib::HashTest::Eql,
                            };
                        }
                    }
                    c = r2;
                }
                let opts = bliss_stdlib::MakeHashTableOptions {
                    test,
                    ..Default::default()
                };
                return bliss_stdlib::make_hash_table(&opts);
            }
            "GETHASH" => {
                // (gethash key table &optional default) -> value; sets the
                // second value to the present-p flag.
                let (key_form, r) = cp(cdr);
                let (tbl_form, r2) = cp(r);
                let key = eval_form(key_form, env)?;
                let tbl = eval_form(tbl_form, env)?;
                let default = if r2.is_cons() {
                    eval_form(cp(r2).0, env)?
                } else {
                    NIL
                };
                let (val, present) = bliss_stdlib::gethash(key, tbl, default)?;
                env.set_mv(vec![val, if present { T } else { NIL }]);
                return Ok(val);
            }
            "REMHASH" => {
                let (key_form, r) = cp(cdr);
                let (tbl_form, _) = cp(r);
                let key = eval_form(key_form, env)?;
                let tbl = eval_form(tbl_form, env)?;
                let removed = bliss_stdlib::remhash(key, tbl)?;
                return Ok(if removed { T } else { NIL });
            }
            "HASH-TABLE-COUNT" => {
                let (tbl_form, _) = cp(cdr);
                let tbl = eval_form(tbl_form, env)?;
                let n = bliss_stdlib::hash_table_count(tbl)?;
                return Ok(BlissVal::from_fixnum(n as i64));
            }
            "MAPCAR" => {
                let (fn_form, r) = cp(cdr);
                let fn_val = eval_form(fn_form, env)?;
                let (list_form, _) = cp(r);
                let list = eval_form(list_form, env)?;
                let elems = list_to_vec(list);
                let mut results = Vec::new();
                for e in &elems {
                    results.push(apply_function(fn_val, &[*e], env)?);
                }
                return Ok(vec_to_list(&results));
            }
            "MAP" => {
                let args = list_to_vec(cdr);
                if args.len() < 3 {
                    return Err(BlissError::Internal(
                        "MAP requires a result type, function, and sequence".into(),
                    ));
                }
                let result_type = eval_form(args[0], env)?;
                let fn_val = eval_form(args[1], env)?;
                let mut seqs: Vec<Vec<BlissVal>> = Vec::new();
                for seq_form in &args[2..] {
                    let seq = eval_form(*seq_form, env)?;
                    seqs.push(if seq.is_string() {
                        val_as_str(seq)
                            .chars()
                            .map(BlissVal::from_char)
                            .collect::<Vec<_>>()
                    } else {
                        list_to_vec(seq)
                    });
                }
                let len = seqs.iter().map(Vec::len).min().unwrap_or(0);
                let mut results = Vec::with_capacity(len);
                for i in 0..len {
                    let call_args = seqs.iter().map(|seq| seq[i]).collect::<Vec<_>>();
                    results.push(apply_function(fn_val, &call_args, env)?);
                }
                let result_name = if result_type.is_symbol() {
                    symbol_bare_name(&sym_name(result_type))
                } else {
                    val_as_str(result_type).to_uppercase()
                };
                return Ok(match result_name.as_str() {
                    "NIL" => NIL,
                    "LIST" => vec_to_list(&results),
                    "STRING" | "SIMPLE-STRING" | "BASE-STRING" => {
                        let s = results.iter().map(|v| v.as_char()).collect::<String>();
                        arena_str(&s)
                    }
                    _ => vec_to_list(&results),
                });
            }
            "FIND" => {
                let (item_form, r) = cp(cdr);
                let (seq_form, mut rest) = cp(r);
                let item = eval_form(item_form, env)?;
                let seq = eval_form(seq_form, env)?;
                let mut test = NIL;
                let mut key = None;
                let mut start = 0usize;
                let mut end = None;
                let mut from_end = false;
                while rest.is_cons() {
                    let (kw, r2) = cp(rest);
                    if !r2.is_cons() {
                        break;
                    }
                    let (value_form, r3) = cp(r2);
                    let value = eval_form(value_form, env)?;
                    if kw.is_symbol() {
                        let name = sym_name(kw);
                        match name.strip_prefix("KEYWORD:").unwrap_or(&name) {
                            "TEST" => test = value,
                            "KEY" => key = Some(value),
                            "START" => start = num_val(value)? as usize,
                            "END" => end = Some(num_val(value)? as usize),
                            "FROM-END" => from_end = !value.is_nil(),
                            _ => {}
                        }
                    }
                    rest = r3;
                }
                return bliss_stdlib::find(item, seq, test, key, start, end, from_end);
            }
            "MEMBER" => {
                let (item_f, r) = cp(cdr);
                let (list_f, _) = cp(r);
                let item = eval_form(item_f, env)?;
                let list = eval_form(list_f, env)?;
                let mut c = list;
                while c.is_cons() {
                    let (car, cdr_val) = cp(c);
                    if vals_equal(car, item) {
                        return Ok(c);
                    }
                    c = cdr_val;
                }
                return Ok(NIL);
            }
            "ASSOC" => {
                let (key_f, r) = cp(cdr);
                let (alist_f, _) = cp(r);
                let key = eval_form(key_f, env)?;
                let alist = eval_form(alist_f, env)?;
                let mut c = alist;
                while c.is_cons() {
                    let (pair, rest) = cp(c);
                    if pair.is_cons() {
                        let (k, _) = cp(pair);
                        if vals_equal(k, key) {
                            return Ok(pair);
                        }
                    }
                    c = rest;
                }
                return Ok(NIL);
            }
            "CONCATENATE" => {
                // (concatenate 'string str1 str2 ...)
                let (type_form, rest) = cp(cdr);
                let _type_val = eval_form(type_form, env)?;
                let mut result = String::new();
                let mut c = rest;
                while c.is_cons() {
                    let (sf, r) = cp(c);
                    let v = eval_form(sf, env)?;
                    result.push_str(&val_as_str(v));
                    c = r;
                }
                return Ok(arena_str(&result));
            }
            "STRING" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(arena_str(&val_as_str(v)));
            }
            "WRITE-TO-STRING" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(arena_str(&format_val(v)));
            }
            "ABS" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_fixnum() {
                    return Ok(BlissVal::from_fixnum(v.as_fixnum().abs()));
                }
                if v.is_single_float() {
                    return Ok(BlissVal::from_single_float(v.as_single_float().abs()));
                }
                return Err(BlissError::TypeError {
                    datum: v,
                    expected: "number".into(),
                });
            }
            "MIN" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MIN requires at least one argument".into(),
                    ));
                }
                let mut min = num_val(args[0])?;
                let mut is_f = args[0].is_single_float();
                for a in &args[1..] {
                    let v = num_val(*a)?;
                    let is_single_float = a.is_single_float();
                    if v < min {
                        min = v;
                    }
                    if is_single_float {
                        is_f = true;
                    }
                }
                return Ok(if is_f {
                    BlissVal::from_single_float(min as f32)
                } else {
                    BlissVal::from_fixnum(min as i64)
                });
            }
            "MAX" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MAX requires at least one argument".into(),
                    ));
                }
                let mut max = num_val(args[0])?;
                let mut is_f = args[0].is_single_float();
                for a in &args[1..] {
                    let v = num_val(*a)?;
                    let is_single_float = a.is_single_float();
                    if v > max {
                        max = v;
                    }
                    if is_single_float {
                        is_f = true;
                    }
                }
                return Ok(if is_f {
                    BlissVal::from_single_float(max as f32)
                } else {
                    BlissVal::from_fixnum(max as i64)
                });
            }
            "FLOOR" => return eval_floor(cdr, env),
            "MOD" | "REM" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                let av = num_val(a)? as i64;
                let bv = num_val(b)? as i64;
                if bv == 0 {
                    return Err(BlissError::ArithmeticError("division by zero".into()));
                }
                return Ok(BlissVal::from_fixnum(av % bv));
            }
            "TRUNCATE" => {
                let (af, r) = cp(cdr);
                let a = eval_form(af, env)?;
                let av = num_val(a)?;
                if r.is_cons() {
                    let (bf, _) = cp(r);
                    let b = eval_form(bf, env)?;
                    let bv = num_val(b)?;
                    if bv == 0.0 {
                        return Err(BlissError::ArithmeticError("division by zero".into()));
                    }
                    let q = (av / bv).trunc() as i64;
                    let rem = av - (q as f64) * bv;
                    env.set_mv(vec![
                        BlissVal::from_fixnum(q),
                        BlissVal::from_single_float(rem as f32),
                    ]);
                    return Ok(BlissVal::from_fixnum(q));
                }
                return Ok(BlissVal::from_fixnum(av as i64));
            }
            "CEILING" => {
                let (af, r) = cp(cdr);
                let a = eval_form(af, env)?;
                let av = num_val(a)?;
                if r.is_cons() {
                    let (bf, _) = cp(r);
                    let b = eval_form(bf, env)?;
                    let bv = num_val(b)?;
                    if bv == 0.0 {
                        return Err(BlissError::ArithmeticError("division by zero".into()));
                    }
                    let q = (av / bv).ceil() as i64;
                    let rem = av - (q as f64) * bv;
                    env.set_mv(vec![
                        BlissVal::from_fixnum(q),
                        BlissVal::from_single_float(rem as f32),
                    ]);
                    return Ok(BlissVal::from_fixnum(q));
                }
                return Ok(BlissVal::from_fixnum(av.ceil() as i64));
            }
            "ROUND" => {
                let (af, r) = cp(cdr);
                let a = eval_form(af, env)?;
                let av = num_val(a)?;
                if r.is_cons() {
                    let (bf, _) = cp(r);
                    let b = eval_form(bf, env)?;
                    let bv = num_val(b)?;
                    if bv == 0.0 {
                        return Err(BlissError::ArithmeticError("division by zero".into()));
                    }
                    let q = (av / bv).round() as i64;
                    let rem = av - (q as f64) * bv;
                    env.set_mv(vec![
                        BlissVal::from_fixnum(q),
                        BlissVal::from_single_float(rem as f32),
                    ]);
                    return Ok(BlissVal::from_fixnum(q));
                }
                return Ok(BlissVal::from_fixnum(av.round() as i64));
            }
            "EXPT" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                // Exact result when the base is rational and the exponent is an
                // integer: an exact rational/bignum, promoting past i64 range.
                if !a.is_single_float() && b.is_fixnum() {
                    if let Some(base) = as_bigrat(a) {
                        let e = b.as_fixnum();
                        if e >= 0 {
                            return Ok(bigrat_pow(&base, e as u64).to_val());
                        }
                        if base.num.is_zero() {
                            return Err(BlissError::ArithmeticError("division by zero".into()));
                        }
                        // negative exponent: reciprocal of base^|e|
                        let p = bigrat_pow(&base, e.unsigned_abs());
                        return Ok(bigrat_div(&BigRat::from_i64(1), &p).to_val());
                    }
                }
                let av = num_val(a)?;
                let bv = num_val(b)?;
                return Ok(BlissVal::from_single_float(av.powf(bv) as f32));
            }
            "SQRT" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let nv = num_val(v)?;
                return Ok(BlissVal::from_single_float(nv.sqrt() as f32));
            }
            "NTH-VALUE" => {
                let (nf, r) = cp(cdr);
                let (form, _) = cp(r);
                let n = eval_form(nf, env)?;
                let idx = num_val(n)? as usize;
                let (_, values) = eval_form_collecting_values(form, env)?;
                return Ok(values.get(idx).copied().unwrap_or(NIL));
            }
            "MULTIPLE-VALUE-BIND" => return eval_multiple_value_bind(cdr, env),
            "MULTIPLE-VALUE-CALL" => {
                // (multiple-value-call function form*) — gather all the values
                // produced by each form into a single argument list.
                let (fn_form, forms) = cp(cdr);
                let fn_val = eval_form(fn_form, env)?;
                let mut args = Vec::new();
                let mut c = forms;
                while c.is_cons() {
                    let (af, r) = cp(c);
                    let (_, values) = eval_form_collecting_values(af, env)?;
                    args.extend(values);
                    c = r;
                }
                return apply_function(fn_val, &args, env);
            }
            "MULTIPLE-VALUE-PROG1" => {
                let (first_form, rest_forms) = cp(cdr);
                let (primary, saved_values) = eval_form_collecting_values(first_form, env)?;
                let _ = eval_progn(rest_forms, env)?;
                env.set_mv(saved_values);
                return Ok(primary);
            }
            "MULTIPLE-VALUE-SETQ" => {
                let (vars_form, rest) = cp(cdr);
                let (values_form, _) = cp(rest);
                let vars = list_to_vec(vars_form);
                let (_, values) = eval_form_collecting_values(values_form, env)?;
                for (index, var_form) in vars.iter().enumerate() {
                    let name = sym_name(*var_form);
                    env.set_var(&name, values.get(index).copied().unwrap_or(NIL));
                }
                if values.is_empty() {
                    env.set_mv(Vec::new());
                    return Ok(NIL);
                }
                env.set_mv(values.clone());
                return Ok(values[0]);
            }
            "MULTIPLE-VALUE-LIST" => {
                // (multiple-value-list form) — a list of all the values of form.
                let (form, _) = cp(cdr);
                let (_, values) = eval_form_collecting_values(form, env)?;
                return Ok(vec_to_list(&values));
            }
            "VALUES-LIST" => {
                // (values-list list) — return the elements of list as values.
                let (form, _) = cp(cdr);
                let lst = eval_form(form, env)?;
                let vals = list_to_vec(lst);
                if vals.is_empty() {
                    env.set_mv(Vec::new());
                    return Ok(NIL);
                }
                env.set_mv(vals.clone());
                return Ok(vals[0]);
            }
            "HANDLER-CASE" => return eval_handler_case(cdr, env),
            "HANDLER-BIND" => return eval_handler_bind(cdr, env),
            "SIGNAL" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal("SIGNAL requires an argument".into()));
                }
                let cond = eval_form(args[0], env)?;
                for handler in env.handlers.clone().iter().rev() {
                    if condition_matches_handler(env, cond, &handler.type_name) {
                        let hfn = handler.handler;
                        let _ = apply_function(hfn, &[cond], env);
                    }
                }
                return Ok(NIL);
            }
            "MAKE-CONDITION" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MAKE-CONDITION requires a type".into(),
                    ));
                }
                let type_val = eval_form(args[0], env)?;
                let type_name = if type_val.is_symbol() {
                    sym_name(type_val)
                } else {
                    val_as_str(type_val)
                };
                let class = ensure_condition_class_registered(env, &type_name)?;
                let slot_specs = condition_slot_specs(env, &type_name);
                let mut initargs = Vec::new();
                let mut seen_initargs = Vec::new();
                let mut i = 1;
                while i + 1 < args.len() {
                    let key = eval_form(args[i], env)?;
                    let val = eval_form(args[i + 1], env)?;
                    let key_name = symbol_bare_name(&sym_name(key));
                    let slot_name = slot_specs
                        .iter()
                        .find(|(_, initarg)| initarg == &key_name)
                        .map(|(slot_name, _)| slot_name.clone())
                        .unwrap_or_else(|| key_name.clone());
                    seen_initargs.push(key_name);
                    initargs.push(resolve_sym(&slot_name).unwrap_or(NIL));
                    initargs.push(val);
                    i += 2;
                }
                for (initarg_name, default_value) in condition_default_initargs(env, &type_name) {
                    if seen_initargs.iter().any(|seen| seen == &initarg_name) {
                        continue;
                    }
                    if let Some((slot_name, _)) = slot_specs
                        .iter()
                        .find(|(_, initarg)| initarg == &initarg_name)
                    {
                        initargs.push(resolve_sym(slot_name).unwrap_or(NIL));
                        initargs.push(default_value);
                    }
                }
                return bliss_stdlib::make_instance(class, &initargs);
            }
            "CERROR" => return eval_cerror(cdr, env),
            "INVOKE-RESTART" => {
                let (name_form, _rest_args) = cp(cdr);
                let name_val = eval_form(name_form, env)?;
                let restart_name = val_as_str(name_val).to_uppercase();
                // Look up the restart by name in the restart stack
                for restart in env.restarts.iter().rev() {
                    if restart.name == restart_name {
                        // If restart has a body, evaluate it
                        if let Some(body) = restart.body {
                            let result = eval_progn(body, env)?;
                            return Ok(result);
                        }
                        // Otherwise (e.g., CONTINUE restart from cerror), just return NIL
                        // Signal the restart was invoked via a special error
                        return Err(BlissError::Internal(format!(
                            "__RESTART_INVOKED__:{}",
                            restart_name
                        )));
                    }
                }
                return Err(BlissError::Internal(format!(
                    "Restart {} not found",
                    restart_name
                )));
            }
            "WITH-OPEN-FILE" => return eval_with_open_file(cdr, env),
            "LOAD" => {
                let (path_form, _) = cp(cdr);
                let path_val = eval_form(path_form, env)?;
                return load_path_into_env(&val_as_str(path_val), env);
            }
            "REQUIRE" => {
                let (module_form, _) = cp(cdr);
                let module_val = eval_form(module_form, env)?;
                return require_module(&val_as_str(module_val), env);
            }
            "PROVIDE" => {
                let (module_form, _) = cp(cdr);
                let module_val = eval_form(module_form, env)?;
                env.define_local("*LAST-PROVIDED-MODULE*", module_val);
                return Ok(module_val);
            }
            "BLISS-EXT:GETENV" => {
                let (name_form, _) = cp(cdr);
                let name = val_as_str(eval_form(name_form, env)?);
                return Ok(std::env::var(&name)
                    .ok()
                    .map(|value| arena_str(&value))
                    .unwrap_or(NIL));
            }
            "READ-LINE" => {
                let args = list_to_vec(cdr);
                let stream = if !args.is_empty() {
                    eval_form(args[0], env)?
                } else {
                    NIL // stdin
                };
                if is_stream(stream) {
                    // Read from file stream
                    let (line, eof) = stream_read_line(stream)?;
                    if eof && line.is_empty() {
                        return Ok(NIL);
                    }
                    env.set_mv(vec![arena_str(&line), if eof { T } else { NIL }]);
                    return Ok(arena_str(&line));
                } else {
                    // Read from stdin
                    let mut line = String::new();
                    std::io::stdin()
                        .read_line(&mut line)
                        .map_err(|e| BlissError::StreamError(format!("read-line: {}", e)))?;
                    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                    return Ok(arena_str(trimmed));
                }
            }
            "WRITE-STRING" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "WRITE-STRING requires an argument".into(),
                    ));
                }
                let string = eval_form(args[0], env)?;
                let s = val_as_str(string);
                if args.len() > 1 {
                    let stream = eval_form(args[1], env)?;
                    if is_stream(stream) {
                        stream_write_string(stream, &s)?;
                        return Ok(string);
                    }
                }
                print!("{}", s);
                return Ok(string);
            }
            "SAVE-IMAGE" => {
                let (path_form, _) = cp(cdr);
                let path_val = eval_form(path_form, env)?;
                let path = val_as_str(path_val);
                // Save a minimal image: serialize the environment's function definitions
                let mut image_data = String::new();
                for (name, fdef) in env.funs.iter() {
                    let params_str = fdef.params.join(" ");
                    let body_str = format_body_forms(fdef.body);
                    image_data
                        .push_str(&format!("(defun {} ({}) {})\n", name, params_str, body_str));
                }
                std::fs::write(&path, &image_data)
                    .map_err(|e| BlissError::FileError(format!("save-image: {}", e)))?;
                return Ok(T);
            }
            "DEFPACKAGE" => return eval_defpackage(cdr, env),
            "IN-PACKAGE" => {
                let (pkg_form, _) = cp(cdr);
                let pkg_val = eval_form(pkg_form, env)?;
                let pkg_name = val_as_str(pkg_val)
                    .trim_start_matches("KEYWORD:")
                    .trim_start_matches(':')
                    .to_uppercase();
                env.current_package = pkg_name;
                env.define_local("*PACKAGE*", arena_str(&env.current_package));
                return Ok(T);
            }
            "MAKE-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MAKE-PACKAGE requires a package designator".into(),
                    ));
                }
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                ensure_package_available(env, &pkg_name, &[]);
                return Ok(arena_str(&pkg_name));
            }
            "FIND-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                return Ok(
                    if env.packages.contains_key(&pkg_name)
                        || matches!(
                            pkg_name.as_str(),
                            "COMMON-LISP" | "COMMON-LISP-USER" | "KEYWORD"
                        )
                    {
                        arena_str(&pkg_name)
                    } else {
                        NIL
                    },
                );
            }
            "PACKAGE-NAME" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg = val_as_str(eval_form(args[0], env)?);
                return Ok(if pkg.is_empty() {
                    NIL
                } else {
                    arena_str(&normalize_package_name(&pkg))
                });
            }
            "PACKAGE-NAMES" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                return Ok(vec_to_list(&[arena_str(&pkg)]));
            }
            "PACKAGE-NICKNAMES" | "PACKAGE-SHADOWING-SYMBOLS" | "PACKAGE-USED-BY-LIST" => {
                return Ok(NIL);
            }
            "PACKAGE-USE-LIST" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                let uses = env
                    .packages
                    .get(&pkg_name)
                    .map(|pkg| {
                        pkg.uses
                            .iter()
                            .map(|name| arena_str(name))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                return Ok(vec_to_list(&uses));
            }
            "LIST-ALL-PACKAGES" => {
                let packages = env
                    .packages
                    .keys()
                    .map(|name| arena_str(name))
                    .collect::<Vec<_>>();
                return Ok(vec_to_list(&packages));
            }
            "BLISS-INTERNAL::PACKAGE-SYMBOLS" | "BLISS-INTERNAL:PACKAGE-SYMBOLS" => {
                let args = list_to_vec(cdr);
                let package = if args.is_empty() {
                    env.current_package.clone()
                } else {
                    normalize_package_name(&val_as_str(eval_form(args[0], env)?))
                };
                let include_inherited = if let Some(arg) = args.get(1) {
                    !eval_form(*arg, env)?.is_nil()
                } else {
                    false
                };
                return Ok(vec_to_list(&package_symbols(
                    env,
                    &package,
                    include_inherited,
                )));
            }
            "USE-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(T);
                }
                let mut names = Vec::new();
                let pkgs_val = eval_form(args[0], env)?;
                if pkgs_val.is_cons() {
                    for pkg in list_to_vec(pkgs_val) {
                        names.push(normalize_package_name(&val_as_str(pkg)));
                    }
                } else {
                    names.push(normalize_package_name(&val_as_str(pkgs_val)));
                }
                let target = if args.len() > 1 {
                    normalize_package_name(&val_as_str(eval_form(args[1], env)?))
                } else {
                    env.current_package.clone()
                };
                ensure_package_available(env, &target, &[]);
                let package = Rc::make_mut(&mut env.packages)
                    .get_mut(&target)
                    .expect("package exists");
                for name in names {
                    if !package.uses.contains(&name) {
                        package.uses.push(name);
                    }
                }
                return Ok(T);
            }
            "UNUSE-PACKAGE" => return Ok(T),
            "RENAME-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "RENAME-PACKAGE requires package and new name".into(),
                    ));
                }
                let old_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                let new_name = normalize_package_name(&val_as_str(eval_form(args[1], env)?));
                if let Some(mut pkg) = Rc::make_mut(&mut env.packages).remove(&old_name) {
                    pkg.name = new_name.clone();
                    Rc::make_mut(&mut env.packages).insert(new_name.clone(), pkg);
                }
                reader::register_package(&new_name);
                return Ok(arena_str(&new_name));
            }
            "DELETE-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(T);
                }
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                Rc::make_mut(&mut env.packages).remove(&pkg_name);
                return Ok(T);
            }
            "INTERN" => {
                let (name_form, rest) = cp(cdr);
                let name_val = eval_form(name_form, env)?;
                let name_str = symbol_bare_name(&val_as_str(name_val));
                let pkg_name = if rest.is_cons() {
                    normalize_package_name(&val_as_str(eval_form(cp(rest).0, env)?))
                } else {
                    env.current_package.clone()
                };
                let sym = intern_into_package(env, &pkg_name, &name_str);
                env.set_mv(vec![
                    sym,
                    if sym.is_symbol() {
                        package_status_symbol("INTERNAL")
                    } else {
                        NIL
                    },
                ]);
                return Ok(sym);
            }
            "FIND-SYMBOL" => {
                let args = list_to_vec(cdr);
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "FIND-SYMBOL requires a name and package".into(),
                    ));
                }
                let name = symbol_bare_name(&val_as_str(eval_form(args[0], env)?));
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[1], env)?));
                if let Some((sym, status)) = find_symbol_in_package(env, &pkg_name, &name) {
                    env.set_mv(vec![sym, package_status_symbol(status)]);
                    return Ok(sym);
                }
                env.set_mv(vec![NIL, NIL]);
                return Ok(NIL);
            }
            "EXPORT" | "IMPORT" | "SHADOWING-IMPORT" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(T);
                }
                let symbols = eval_form(args[0], env)?;
                let pkg_name = if args.len() > 1 {
                    normalize_package_name(&val_as_str(eval_form(args[1], env)?))
                } else {
                    env.current_package.clone()
                };
                ensure_package_available(env, &pkg_name, &[]);
                let mut names = Vec::new();
                if symbols.is_cons() {
                    for sym in list_to_vec(symbols) {
                        names.push(symbol_bare_name(&val_as_str(sym)));
                    }
                } else {
                    names.push(symbol_bare_name(&val_as_str(symbols)));
                }
                let export_mode = car.is_symbol() && sym_name(car) == "EXPORT";
                let mut resolved = Vec::with_capacity(names.len());
                for name in names {
                    let sym = intern_into_package(env, &pkg_name, &name);
                    resolved.push((name, sym));
                }
                let package = Rc::make_mut(&mut env.packages)
                    .get_mut(&pkg_name)
                    .expect("package exists");
                for (name, sym) in resolved {
                    package.symbols.insert(name.clone(), sym);
                    if export_mode && !package.exports.contains(&name) {
                        package.exports.push(name);
                    }
                }
                return Ok(T);
            }
            "UNEXPORT" => return Ok(T),
            "SHADOW" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(T);
                }
                let names_val = eval_form(args[0], env)?;
                let pkg_name = if args.len() > 1 {
                    normalize_package_name(&val_as_str(eval_form(args[1], env)?))
                } else {
                    env.current_package.clone()
                };
                ensure_package_available(env, &pkg_name, &[]);
                let mut names = Vec::new();
                if names_val.is_cons() {
                    for name in list_to_vec(names_val) {
                        names.push(symbol_bare_name(&val_as_str(name)));
                    }
                } else {
                    names.push(symbol_bare_name(&val_as_str(names_val)));
                }
                for name in names {
                    intern_into_package(env, &pkg_name, &name);
                }
                return Ok(T);
            }
            "UNINTERN" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let symbol = eval_form(args[0], env)?;
                let pkg_name = if args.len() > 1 {
                    normalize_package_name(&val_as_str(eval_form(args[1], env)?))
                } else {
                    env.current_package.clone()
                };
                let name = symbol_bare_name(&val_as_str(symbol));
                let Some(package) = Rc::make_mut(&mut env.packages).get_mut(&pkg_name) else {
                    return Ok(NIL);
                };
                let removed_symbol = package.symbols.remove(&name).is_some();
                let removed_export =
                    if let Some(pos) = package.exports.iter().position(|n| n == &name) {
                        package.exports.remove(pos);
                        true
                    } else {
                        false
                    };
                return Ok(if removed_symbol || removed_export {
                    T
                } else {
                    NIL
                });
            }
            "SYMBOL-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let sym = eval_form(args[0], env)?;
                if !sym.is_symbol() {
                    return Ok(NIL);
                }
                let name = sym_name(sym);
                let package = if name.starts_with("KEYWORD:") {
                    Some("KEYWORD".to_string())
                } else if let Some((pkg, _)) = name.rsplit_once("::") {
                    Some(pkg.to_string())
                } else if let Some((pkg, _)) = name.rsplit_once(':') {
                    Some(pkg.to_string())
                } else {
                    Some("COMMON-LISP".to_string())
                };
                return Ok(package.map(|pkg| arena_str(&pkg)).unwrap_or(NIL));
            }
            "GENSYM" => {
                thread_local! { static COUNTER: RefCell<u64> = const { RefCell::new(0) }; }
                let n = COUNTER.with(|c| {
                    let v = *c.borrow();
                    *c.borrow_mut() = v + 1;
                    v
                });
                let name = format!("G{}", n);
                match resolve_sym(&name) {
                    Some(sym) => return Ok(sym),
                    None => return Ok(arena_str(&name)),
                }
            }
            "DOTIMES" => {
                // (dotimes (var count [result]) body...)
                let (binding, body) = cp(cdr);
                let (var_form, br) = cp(binding);
                let (count_form, result_rest) = cp(br);
                let var_name = sym_name(var_form);
                let count = eval_form(count_form, env)?;
                let n = num_val(count)? as i64;
                for i in 0..n {
                    env.define_local(&var_name, BlissVal::from_fixnum(i));
                    eval_progn(body, env)?;
                }
                if result_rest.is_cons() {
                    let (result_form, _) = cp(result_rest);
                    env.define_local(&var_name, BlissVal::from_fixnum(n));
                    return eval_form(result_form, env);
                }
                return Ok(NIL);
            }
            "DOLIST" => {
                // (dolist (var list [result]) body...)
                let (binding, body) = cp(cdr);
                let (var_form, br) = cp(binding);
                let (list_form, result_rest) = cp(br);
                let var_name = sym_name(var_form);
                let list = eval_form(list_form, env)?;
                let elems = list_to_vec(list);
                for e in &elems {
                    env.define_local(&var_name, *e);
                    eval_progn(body, env)?;
                }
                if result_rest.is_cons() {
                    let (result_form, _) = cp(result_rest);
                    env.define_local(&var_name, NIL);
                    return eval_form(result_form, env);
                }
                return Ok(NIL);
            }
            "STRING=" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if val_as_str(a) == val_as_str(b) {
                    T
                } else {
                    NIL
                });
            }
            "STRING<" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if val_as_str(a) < val_as_str(b) {
                    T
                } else {
                    NIL
                });
            }
            "STRING>" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if val_as_str(a) > val_as_str(b) {
                    T
                } else {
                    NIL
                });
            }
            "TYPE-OF" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let type_name = if v.is_nil() {
                    "NULL"
                } else if v == T {
                    "BOOLEAN"
                } else if v.is_fixnum() {
                    "FIXNUM"
                } else if v.is_single_float() {
                    "SINGLE-FLOAT"
                } else if v.is_character() {
                    "CHARACTER"
                } else if v.is_symbol() {
                    "SYMBOL"
                } else if v.is_cons() {
                    "CONS"
                } else if v.is_string() {
                    "SIMPLE-BASE-STRING"
                } else {
                    "T"
                };
                match resolve_sym(type_name) {
                    Some(sym) => return Ok(sym),
                    None => return Ok(arena_str(type_name)),
                }
            }
            _ => {}
        }

        // Check user-defined functions
        if let Some(fdef) = env.funs.get(&name).cloned() {
            let mut args = Vec::new();
            let mut c = cdr;
            while c.is_cons() {
                let (af, r) = cp(c);
                args.push(eval_form(af, env)?);
                c = r;
            }
            let mut child_env = env.child();
            bind_lambda_list(fdef.params_form, &args, &mut child_env)?;
            return eval_progn(fdef.body, &mut child_env);
        }

        // Check accessor functions (from DEFCLASS)
        // Collect matching accessor slot name to avoid borrow conflict
        let mut accessor_slot_name: Option<String> = None;
        for class in env.classes.values() {
            for slot in &class.slots {
                if let Some(ref acc) = slot.accessor {
                    if *acc == name {
                        accessor_slot_name = Some(slot.name.clone());
                    }
                }
            }
        }
        if let Some(slot_name) = accessor_slot_name {
            let (inst_form, _) = cp(cdr);
            let inst = eval_form(inst_form, env)?;
            let slots_alist = get_instance_slots(inst);
            for pair in &slots_alist {
                if pair.0 == slot_name {
                    return Ok(pair.1);
                }
            }
            return Ok(NIL);
        }

        // Check methods
        if let Some(methods) = env.methods.get(&name).cloned() {
            let mut args = Vec::new();
            let mut c = cdr;
            while c.is_cons() {
                let (af, r) = cp(c);
                args.push(eval_form(af, env)?);
                c = r;
            }

            // Find most specific method by checking specializer
            let mut best_method: Option<&MethodDef> = None;
            for method in methods.iter().rev() {
                if args.is_empty() {
                    best_method = Some(method);
                    break;
                }
                let arg_class = get_instance_class_name(args[0]);
                if method.specializer == arg_class
                    || is_subclass(&arg_class, &method.specializer, env)
                {
                    // Prefer more specific (exact match over superclass)
                    if best_method.is_none() || method.specializer == arg_class {
                        best_method = Some(method);
                    }
                }
            }

            if let Some(m) = best_method {
                let m = m.clone();
                let mut child_env = env.child();
                for (i, param) in m.params.iter().enumerate() {
                    child_env.define_local(param, if i < args.len() { args[i] } else { NIL });
                }
                return eval_progn(m.body, &mut child_env);
            }
            return Err(BlissError::Internal(format!(
                "No applicable method for {}",
                name
            )));
        }

        // Check for package-qualified symbols (e.g., TEST-PKG:HELLO)
        if name.contains(':') {
            let parts: Vec<&str> = name.splitn(2, ':').collect();
            if parts.len() == 2 {
                let _pkg_name = parts[0];
                let fn_name = parts[1].trim_start_matches(':');
                if let Some(fdef) = env.funs.get(fn_name).cloned() {
                    let mut args = Vec::new();
                    let mut c = cdr;
                    while c.is_cons() {
                        let (af, r) = cp(c);
                        args.push(eval_form(af, env)?);
                        c = r;
                    }
                    let mut child_env = env.child();
                    for (i, param) in fdef.params.iter().enumerate() {
                        child_env.define_local(param, if i < args.len() { args[i] } else { NIL });
                    }
                    return eval_progn(fdef.body, &mut child_env);
                }
            }
        }
    }

    // Lambda application: ((lambda (params) body) args...)
    if car.is_cons() {
        let (lh, lr) = cp(car);
        if lh.is_symbol() && sym_name(lh) == "LAMBDA" {
            let (params_form, body_rest) = cp(lr);
            let mut args = Vec::new();
            let mut c = cdr;
            while c.is_cons() {
                let (af, r) = cp(c);
                args.push(eval_form(af, env)?);
                c = r;
            }
            let mut child_env = env.child();
            bind_lambda_list(params_form, &args, &mut child_env)?;
            return eval_progn(body_rest, &mut child_env);
        }
    }

    Err(BlissError::UndefinedFunction(car))
}

fn eval_progn(forms: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let mut r = NIL;
    let mut c = forms;
    while c.is_cons() {
        let (f, rest) = cp(c);
        r = eval_form(f, env)?;
        c = rest;
    }
    Ok(r)
}

// ── LOOP (extended, bootstrap subset) ─────────────────────────────
//
// Supports the clauses ASDF's load path exercises:
//   :with var = init [:and var = init]*
//   :for pat :in list        (pat may be a destructuring pattern, e.g. (k . v))
//   :for pat :on list
//   :initially form*   :finally form*   (a (return X) form ends the loop with X)
//   :when/:if/:unless test <clause> [:and <clause>]* [:else <clause>+] [:end]
//   :collect/:append expr [:into var]
//   :do form*          :return expr
// A "simple" LOOP (no leading keyword) runs its body repeatedly until a
// top-level (return X); nested returns are not detected (not needed at load
// time) and a hard iteration cap guards against runaway loops.

fn is_loop_keyword(bare: &str) -> bool {
    matches!(
        bare,
        "WITH"
            | "AND"
            | "FOR"
            | "IN"
            | "ON"
            | "BEING"
            | "THE"
            | "OF"
            | "THEN"
            | "WHEN"
            | "IF"
            | "UNLESS"
            | "ELSE"
            | "END"
            | "COLLECT"
            | "COLLECTING"
            | "APPEND"
            | "APPENDING"
            | "NCONC"
            | "NCONCING"
            | "DO"
            | "DOING"
            | "RETURN"
            | "THEREIS"
            | "INTO"
            | "FINALLY"
            | "INITIALLY"
            | "WHILE"
            | "UNTIL"
            | "REPEAT"
    )
}

#[derive(Clone)]
enum LoopClause {
    Do(Vec<BlissVal>),
    Collect(BlissVal, Option<String>),
    Append(BlissVal, Option<String>),
    Return(BlissVal),
    ThereIs(BlissVal),
    Cond {
        test: BlissVal,
        negate: bool,
        then: Vec<LoopClause>,
        els: Vec<LoopClause>,
    },
}

struct LoopParser {
    toks: Vec<BlissVal>,
    pos: usize,
}

impl LoopParser {
    fn peek(&self) -> Option<BlissVal> {
        self.toks.get(self.pos).copied()
    }
    fn advance(&mut self) -> Option<BlissVal> {
        let v = self.peek();
        if v.is_some() {
            self.pos += 1;
        }
        v
    }
    /// Bare (package-stripped, upcased) name of the next token, but only when
    /// it is a recognised LOOP keyword — otherwise None (it's an expression).
    fn peek_kw(&self) -> Option<String> {
        let v = self.peek()?;
        if !v.is_symbol() {
            return None;
        }
        let n = sym_name(v);
        let bare = n.strip_prefix("KEYWORD:").unwrap_or(&n).to_string();
        if is_loop_keyword(&bare) {
            Some(bare)
        } else {
            None
        }
    }
    fn at_kw(&self, k: &str) -> bool {
        self.peek_kw().as_deref() == Some(k)
    }
    fn read_form(&mut self) -> Result<BlissVal, BlissError> {
        self.advance()
            .ok_or_else(|| BlissError::Internal("LOOP: unexpected end of clauses".into()))
    }
    /// Read one-or-more forms up to the next LOOP keyword (for :do/:finally).
    fn read_forms(&mut self) -> Vec<BlissVal> {
        let mut forms = Vec::new();
        while self.pos < self.toks.len() && self.peek_kw().is_none() {
            forms.push(self.advance().unwrap());
        }
        forms
    }
    fn read_into(&mut self) -> Result<Option<String>, BlissError> {
        if self.at_kw("INTO") {
            self.advance();
            Ok(Some(sym_name(self.read_form()?)))
        } else {
            Ok(None)
        }
    }
    fn parse_clause(&mut self) -> Result<LoopClause, BlissError> {
        let kw = self
            .peek_kw()
            .ok_or_else(|| BlissError::Internal("LOOP: expected a clause keyword".into()))?;
        self.advance();
        match kw.as_str() {
            "COLLECT" | "COLLECTING" => {
                let e = self.read_form()?;
                let into = self.read_into()?;
                Ok(LoopClause::Collect(e, into))
            }
            "APPEND" | "APPENDING" | "NCONC" | "NCONCING" => {
                let e = self.read_form()?;
                let into = self.read_into()?;
                Ok(LoopClause::Append(e, into))
            }
            "DO" | "DOING" => Ok(LoopClause::Do(self.read_forms())),
            "RETURN" => Ok(LoopClause::Return(self.read_form()?)),
            "THEREIS" => Ok(LoopClause::ThereIs(self.read_form()?)),
            "WHEN" | "IF" => self.parse_cond(false),
            "UNLESS" => self.parse_cond(true),
            other => Err(BlissError::Internal(format!(
                "LOOP: unsupported clause `{}`",
                other
            ))),
        }
    }
    fn parse_cond(&mut self, negate: bool) -> Result<LoopClause, BlissError> {
        let test = self.read_form()?;
        let mut then = vec![self.parse_clause()?];
        while self.at_kw("AND") {
            self.advance();
            then.push(self.parse_clause()?);
        }
        let mut els = Vec::new();
        if self.at_kw("ELSE") {
            self.advance();
            els.push(self.parse_clause()?);
            while self.at_kw("AND") {
                self.advance();
                els.push(self.parse_clause()?);
            }
        }
        if self.at_kw("END") {
            self.advance();
        }
        Ok(LoopClause::Cond {
            test,
            negate,
            then,
            els,
        })
    }
}

#[derive(Default)]
struct LoopAccs {
    map: std::collections::HashMap<Option<String>, Vec<BlissVal>>,
}

impl LoopAccs {
    fn collect(&mut self, key: Option<String>, v: BlissVal) {
        self.map.entry(key).or_default().push(v);
    }
    fn append(&mut self, key: Option<String>, v: BlissVal) {
        let items = list_to_vec(v);
        self.map.entry(key).or_default().extend(items);
    }
}

/// Bind a (possibly destructuring / dotted) pattern against a value.
fn loop_bind(pattern: BlissVal, value: BlissVal, env: &mut Env) {
    if pattern.is_symbol() {
        let name = sym_name(pattern);
        if name != "NIL" {
            env.define_local(&name, value);
        }
    } else if pattern.is_cons() {
        let (pcar, pcdr) = cp(pattern);
        let (vcar, vcdr) = if value.is_cons() {
            cp(value)
        } else {
            (NIL, NIL)
        };
        loop_bind(pcar, vcar, env);
        loop_bind(pcdr, vcdr, env);
    }
}

/// Collect every `:into` accumulator name so they can be bound to NIL up
/// front — LOOP guarantees accumulators are bound even if never accumulated,
/// and :finally clauses read them.
fn loop_collect_intos(clauses: &[LoopClause], out: &mut Vec<String>) {
    for c in clauses {
        match c {
            LoopClause::Collect(_, Some(n)) | LoopClause::Append(_, Some(n)) => {
                if !out.contains(n) {
                    out.push(n.clone());
                }
            }
            LoopClause::Cond { then, els, .. } => {
                loop_collect_intos(then, out);
                loop_collect_intos(els, out);
            }
            _ => {}
        }
    }
}

fn loop_exec_clauses(
    clauses: &[LoopClause],
    env: &mut Env,
    accs: &mut LoopAccs,
    ret: &mut Option<BlissVal>,
) -> Result<(), BlissError> {
    for c in clauses {
        if ret.is_some() {
            break;
        }
        loop_exec_clause(c, env, accs, ret)?;
    }
    Ok(())
}

fn loop_exec_clause(
    c: &LoopClause,
    env: &mut Env,
    accs: &mut LoopAccs,
    ret: &mut Option<BlissVal>,
) -> Result<(), BlissError> {
    match c {
        LoopClause::Do(forms) => {
            for f in forms {
                if ret.is_some() {
                    break;
                }
                eval_form(*f, env)?;
            }
        }
        LoopClause::Return(e) => {
            *ret = Some(eval_form(*e, env)?);
        }
        LoopClause::ThereIs(e) => {
            let v = eval_form(*e, env)?;
            if !v.is_nil() {
                *ret = Some(v);
            }
        }
        LoopClause::Collect(e, into) => {
            let v = eval_form(*e, env)?;
            accs.collect(into.clone(), v);
        }
        LoopClause::Append(e, into) => {
            let v = eval_form(*e, env)?;
            accs.append(into.clone(), v);
        }
        LoopClause::Cond {
            test,
            negate,
            then,
            els,
        } => {
            let t = eval_form(*test, env)?;
            let take = !t.is_nil() ^ *negate;
            if take {
                loop_exec_clauses(then, env, accs, ret)?;
            } else {
                loop_exec_clauses(els, env, accs, ret)?;
            }
        }
    }
    Ok(())
}

/// If `form` is (RETURN x) or (RETURN-FROM nil x), return Some(x-form).
fn loop_return_target(form: BlissVal) -> Option<BlissVal> {
    if !form.is_cons() {
        return None;
    }
    let (head, rest) = cp(form);
    if !head.is_symbol() {
        return None;
    }
    match sym_name(head).as_str() {
        "RETURN" => Some(cp(rest).0),
        "RETURN-FROM" => {
            let (_blk, r) = cp(rest);
            Some(cp(r).0)
        }
        _ => None,
    }
}

/// A parsed `:for` iteration clause.
enum ForClause {
    In {
        pat: BlissVal,
        list_form: BlissVal,
    },
    On {
        pat: BlissVal,
        list_form: BlissVal,
    },
    Eq {
        pat: BlissVal,
        init: BlissVal,
        then: Option<BlissVal>,
    },
    From {
        pat: BlissVal,
        start: BlissVal,
        step: Option<BlissVal>,
        limit: Option<(LoopForLimit, BlissVal)>,
    },
    Across {
        pat: BlissVal,
        seq_form: BlissVal,
    },
    Being {
        pat: BlissVal,
        source: LoopBeingSource,
    },
}

/// Runtime cursor for a `:for` clause.
enum ForState {
    In {
        pat: BlissVal,
        items: Vec<BlissVal>,
        idx: usize,
    },
    On {
        pat: BlissVal,
        tail: BlissVal,
    },
    Eq {
        pat: BlissVal,
        init: BlissVal,
        then: Option<BlissVal>,
    },
    From {
        pat: BlissVal,
        current: BlissVal,
        step: BlissVal,
        limit: Option<(LoopForLimit, BlissVal)>,
    },
    Across {
        pat: BlissVal,
        items: Vec<BlissVal>,
        idx: usize,
    },
    Being {
        pat: BlissVal,
        items: Vec<BlissVal>,
        idx: usize,
    },
}

#[derive(Clone, Copy)]
enum LoopForLimit {
    Below,
    To,
    Upto,
    Above,
    Downto,
}

enum LoopBeingSource {
    Symbols(BlissVal),
    HashKeys(BlissVal),
    HashValues(BlissVal),
}

fn eval_loop(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let toks = list_to_vec(cdr);
    let mut lenv = env.child();

    // Simple LOOP: no leading keyword -> repeat body until a top-level return.
    let starts_with_kw = toks
        .first()
        .and_then(|v| {
            if v.is_symbol() {
                let n = sym_name(*v);
                Some(is_loop_keyword(n.strip_prefix("KEYWORD:").unwrap_or(&n)))
            } else {
                None
            }
        })
        .unwrap_or(false);
    if !starts_with_kw {
        let mut guard: u64 = 0;
        loop {
            for f in &toks {
                if let Some(tgt) = loop_return_target(*f) {
                    return eval_form(tgt, &mut lenv);
                }
                eval_form(*f, &mut lenv)?;
            }
            guard += 1;
            if guard > 10_000_000 {
                return Err(BlissError::Internal(
                    "LOOP: simple loop exceeded iteration cap".into(),
                ));
            }
        }
    }

    // Extended LOOP.
    let mut p = LoopParser { toks, pos: 0 };
    let mut with_bindings: Vec<(BlissVal, BlissVal)> = Vec::new();
    let mut for_clauses: Vec<ForClause> = Vec::new();
    let mut initially: Vec<BlissVal> = Vec::new();
    let mut finally: Vec<BlissVal> = Vec::new();
    let mut body: Vec<LoopClause> = Vec::new();

    while let Some(kw) = p.peek_kw() {
        match kw.as_str() {
            "WITH" => {
                p.advance();
                loop {
                    let var = p.read_form()?;
                    let eq = p.read_form()?;
                    if !(eq.is_symbol() && sym_name(eq) == "=") {
                        return Err(BlissError::Internal("LOOP :with expects `=`".into()));
                    }
                    let init = p.read_form()?;
                    with_bindings.push((var, init));
                    if p.at_kw("AND") {
                        p.advance();
                        continue;
                    }
                    break;
                }
            }
            "FOR" => {
                p.advance();
                let pat = p.read_form()?;
                match p.peek_kw().as_deref() {
                    Some("IN") => {
                        p.advance();
                        let list_form = p.read_form()?;
                        for_clauses.push(ForClause::In { pat, list_form });
                    }
                    Some("ON") => {
                        p.advance();
                        let list_form = p.read_form()?;
                        for_clauses.push(ForClause::On { pat, list_form });
                    }
                    Some("ACROSS") => {
                        p.advance();
                        let seq_form = p.read_form()?;
                        for_clauses.push(ForClause::Across { pat, seq_form });
                    }
                    Some("BEING") => {
                        p.advance();
                        if p.at_kw("THE") {
                            p.advance();
                        }
                        let kind = p.read_form()?;
                        let kind_name = if kind.is_symbol() {
                            sym_name(kind)
                        } else {
                            String::new()
                        };
                        let kind_bare = kind_name.strip_prefix("KEYWORD:").unwrap_or(&kind_name);
                        let source = match kind_bare {
                            "SYMBOLS" => {
                                let in_kw = p.read_form()?;
                                let in_name = if in_kw.is_symbol() {
                                    sym_name(in_kw)
                                } else {
                                    String::new()
                                };
                                if in_name.strip_prefix("KEYWORD:").unwrap_or(&in_name) != "IN" {
                                    return Err(BlissError::Internal(
                                        "LOOP :for ... :being :the :symbols expects :in".into(),
                                    ));
                                }
                                LoopBeingSource::Symbols(p.read_form()?)
                            }
                            "HASH-KEYS" => {
                                let of_kw = p.read_form()?;
                                let of_name = if of_kw.is_symbol() {
                                    sym_name(of_kw)
                                } else {
                                    String::new()
                                };
                                if of_name.strip_prefix("KEYWORD:").unwrap_or(&of_name) != "OF" {
                                    return Err(BlissError::Internal(
                                        "LOOP :for ... :being :the :hash-keys expects :of".into(),
                                    ));
                                }
                                LoopBeingSource::HashKeys(p.read_form()?)
                            }
                            "HASH-VALUES" => {
                                let of_kw = p.read_form()?;
                                let of_name = if of_kw.is_symbol() {
                                    sym_name(of_kw)
                                } else {
                                    String::new()
                                };
                                if of_name.strip_prefix("KEYWORD:").unwrap_or(&of_name) != "OF" {
                                    return Err(BlissError::Internal(
                                        "LOOP :for ... :being :the :hash-values expects :of".into(),
                                    ));
                                }
                                LoopBeingSource::HashValues(p.read_form()?)
                            }
                            _ => {
                                return Err(BlissError::Internal(
                                    "LOOP :for ... :being supports :symbols / :hash-keys / :hash-values in the bootstrap"
                                        .into(),
                                ));
                            }
                        };
                        for_clauses.push(ForClause::Being { pat, source });
                    }
                    Some("FROM") => {
                        p.advance();
                        let start = p.read_form()?;
                        let mut step = None;
                        let mut limit = None;
                        loop {
                            match p.peek_kw().as_deref() {
                                Some("BY") => {
                                    p.advance();
                                    step = Some(p.read_form()?);
                                }
                                Some("BELOW") => {
                                    p.advance();
                                    limit = Some((LoopForLimit::Below, p.read_form()?));
                                }
                                Some("TO") => {
                                    p.advance();
                                    limit = Some((LoopForLimit::To, p.read_form()?));
                                }
                                Some("UPTO") => {
                                    p.advance();
                                    limit = Some((LoopForLimit::Upto, p.read_form()?));
                                }
                                Some("ABOVE") => {
                                    p.advance();
                                    limit = Some((LoopForLimit::Above, p.read_form()?));
                                }
                                Some("DOWNTO") => {
                                    p.advance();
                                    limit = Some((LoopForLimit::Downto, p.read_form()?));
                                }
                                _ => break,
                            }
                        }
                        for_clauses.push(ForClause::From {
                            pat,
                            start,
                            step,
                            limit,
                        });
                    }
                    _ => {
                        // :for var = init [:then step]
                        let eq = p.read_form()?;
                        if !(eq.is_symbol() && sym_name(eq) == "=") {
                            let found = if eq.is_symbol() {
                                sym_name(eq)
                            } else {
                                format!("{:?}", eq)
                            };
                            return Err(BlissError::Internal(format!(
                                "LOOP :for supports :in / :on / :across / :being / :from / = in the bootstrap (got {})",
                                found
                            )));
                        }
                        let init = p.read_form()?;
                        let then = if p.at_kw("THEN") {
                            p.advance();
                            Some(p.read_form()?)
                        } else {
                            None
                        };
                        for_clauses.push(ForClause::Eq { pat, init, then });
                    }
                }
            }
            "INITIALLY" => {
                p.advance();
                initially = p.read_forms();
            }
            "FINALLY" => {
                p.advance();
                finally = p.read_forms();
            }
            _ => body.push(p.parse_clause()?),
        }
    }

    // Establish :with bindings (sequential, LET*-style).
    for (var, init) in &with_bindings {
        let v = eval_form(*init, &mut lenv)?;
        loop_bind(*var, v, &mut lenv);
    }

    // Bind all :into accumulators to NIL up front.
    let mut into_names = Vec::new();
    loop_collect_intos(&body, &mut into_names);
    for n in &into_names {
        lenv.define_local(n, NIL);
    }

    let mut accs = LoopAccs::default();
    let mut ret: Option<BlissVal> = None;

    for f in &initially {
        eval_form(*f, &mut lenv)?;
    }

    if for_clauses.is_empty() {
        // No :for driver: run body once (covers when/collect-only loops).
        loop_exec_clauses(&body, &mut lenv, &mut accs, &mut ret)?;
    } else {
        // Build cursors, evaluating each list form once.
        let mut states: Vec<ForState> = Vec::with_capacity(for_clauses.len());
        let mut has_stepping_driver = false;
        for fc in &for_clauses {
            match fc {
                ForClause::In { pat, list_form } => {
                    let list = eval_form(*list_form, &mut lenv)?;
                    states.push(ForState::In {
                        pat: *pat,
                        items: list_to_vec(list),
                        idx: 0,
                    });
                    has_stepping_driver = true;
                }
                ForClause::On { pat, list_form } => {
                    let list = eval_form(*list_form, &mut lenv)?;
                    states.push(ForState::On {
                        pat: *pat,
                        tail: list,
                    });
                    has_stepping_driver = true;
                }
                ForClause::Eq { pat, init, then } => states.push(ForState::Eq {
                    pat: *pat,
                    init: *init,
                    then: *then,
                }),
                ForClause::From {
                    pat,
                    start,
                    step,
                    limit,
                } => {
                    let current = eval_form(*start, &mut lenv)?;
                    let step = match step {
                        Some(expr) => eval_form(*expr, &mut lenv)?,
                        None => BlissVal::from_fixnum(1),
                    };
                    let limit = match limit {
                        Some((kind, expr)) => Some((*kind, eval_form(*expr, &mut lenv)?)),
                        None => None,
                    };
                    states.push(ForState::From {
                        pat: *pat,
                        current,
                        step,
                        limit,
                    });
                    has_stepping_driver = true;
                }
                ForClause::Across { pat, seq_form } => {
                    let seq = eval_form(*seq_form, &mut lenv)?;
                    let items = if seq.is_cons() || seq.is_nil() {
                        list_to_vec(seq)
                    } else {
                        val_as_str(seq)
                            .chars()
                            .map(BlissVal::from_char)
                            .collect::<Vec<_>>()
                    };
                    states.push(ForState::Across {
                        pat: *pat,
                        items,
                        idx: 0,
                    });
                    has_stepping_driver = true;
                }
                ForClause::Being { pat, source } => {
                    let items = match source {
                        LoopBeingSource::Symbols(pkg_form) => {
                            let _ = eval_form(*pkg_form, &mut lenv)?;
                            Vec::new()
                        }
                        LoopBeingSource::HashKeys(table_form) => {
                            let table = eval_form(*table_form, &mut lenv)?;
                            bliss_stdlib::hash_table_entries(table)?
                                .into_iter()
                                .map(|(key, _)| key)
                                .collect()
                        }
                        LoopBeingSource::HashValues(table_form) => {
                            let table = eval_form(*table_form, &mut lenv)?;
                            bliss_stdlib::hash_table_entries(table)?
                                .into_iter()
                                .map(|(_, value)| value)
                                .collect()
                        }
                    };
                    states.push(ForState::Being {
                        pat: *pat,
                        items,
                        idx: 0,
                    });
                    has_stepping_driver = true;
                }
            }
        }
        let mut first = true;
        let mut guard: u64 = 0;
        loop {
            if ret.is_some() {
                break;
            }
            // Step every driver in order; later clauses see earlier bindings.
            let mut exhausted = false;
            for st in &mut states {
                match st {
                    ForState::In { pat, items, idx } => {
                        if *idx >= items.len() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, items[*idx], &mut lenv);
                        *idx += 1;
                    }
                    ForState::On { pat, tail } => {
                        if !tail.is_cons() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, *tail, &mut lenv);
                        *tail = cp(*tail).1;
                    }
                    ForState::Eq { pat, init, then } => {
                        let f = if first { *init } else { then.unwrap_or(*init) };
                        let v = eval_form(f, &mut lenv)?;
                        loop_bind(*pat, v, &mut lenv);
                    }
                    ForState::From {
                        pat,
                        current,
                        step,
                        limit,
                    } => {
                        if loop_from_exhausted(*current, limit.as_ref())? {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, *current, &mut lenv);
                        *current = loop_add_numbers(*current, *step)?;
                    }
                    ForState::Across { pat, items, idx } => {
                        if *idx >= items.len() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, items[*idx], &mut lenv);
                        *idx += 1;
                    }
                    ForState::Being { pat, items, idx } => {
                        if *idx >= items.len() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, items[*idx], &mut lenv);
                        *idx += 1;
                    }
                }
            }
            if exhausted {
                break;
            }
            loop_exec_clauses(&body, &mut lenv, &mut accs, &mut ret)?;
            first = false;
            if !has_stepping_driver {
                guard += 1;
                if guard > 10_000_000 {
                    return Err(BlissError::Internal(
                        "LOOP: exceeded iteration cap (no terminating driver)".into(),
                    ));
                }
            }
        }
    }

    // Publish named accumulators so :finally can read them.
    let named: Vec<(String, Vec<BlissVal>)> = accs
        .map
        .iter()
        .filter_map(|(k, v)| k.as_ref().map(|name| (name.clone(), v.clone())))
        .collect();
    for (name, items) in named {
        lenv.define_local(&name, vec_to_list(&items));
    }

    // :finally — an embedded (return X) ends the loop with X.
    for f in &finally {
        if ret.is_some() {
            break;
        }
        if let Some(tgt) = loop_return_target(*f) {
            ret = Some(eval_form(tgt, &mut lenv)?);
        } else {
            eval_form(*f, &mut lenv)?;
        }
    }

    if let Some(r) = ret {
        return Ok(r);
    }
    // Default: the anonymous accumulator's list, else NIL.
    if let Some(items) = accs.map.get(&None) {
        return Ok(vec_to_list(items));
    }
    Ok(NIL)
}

fn loop_add_numbers(lhs: BlissVal, rhs: BlissVal) -> Result<BlissVal, BlissError> {
    if lhs.is_fixnum() && rhs.is_fixnum() {
        return Ok(BlissVal::from_fixnum(lhs.as_fixnum() + rhs.as_fixnum()));
    }
    let sum = num_val(lhs)? + num_val(rhs)?;
    Ok(BlissVal::from_single_float(sum as f32))
}

fn loop_from_exhausted(
    current: BlissVal,
    limit: Option<&(LoopForLimit, BlissVal)>,
) -> Result<bool, BlissError> {
    let Some((kind, limit)) = limit else {
        return Ok(false);
    };
    let current = num_val(current)?;
    let limit = num_val(*limit)?;
    Ok(match kind {
        LoopForLimit::Below => current >= limit,
        LoopForLimit::To | LoopForLimit::Upto => current > limit,
        LoopForLimit::Above => current <= limit,
        LoopForLimit::Downto => current < limit,
    })
}

// ── Exact numeric tower: fixnum <-> bignum <-> ratio ──────────────
//
// Integer arithmetic promotes to arbitrary precision (BIGNUM, §1.8.1) on
// i64 overflow rather than silently wrapping. Exact rationals are carried as
// a pair of BigInts (denominator kept positive). Floats remain inexact and
// contaminate any operation they take part in, per CL contagion rules.

/// The 61-bit fixnum range (values outside it become BIGNUMs), matching the
/// reader's `fits_fixnum`.
const FIXNUM_MAX: i64 = (1 << 60) - 1;
const FIXNUM_MIN: i64 = -(1 << 60);

/// Allocate a RATIO heap object (numerator/denominator are integers).
fn alloc_ratio_cli(num: BlissVal, den: BlissVal) -> BlissVal {
    let data = Box::leak(Box::new(RatioData {
        header: ObjectHeader::new(type_id::RATIO, 3),
        numerator: num,
        denominator: den,
    }));
    unsafe { BlissVal::from_heap_ptr(data as *mut RatioData as *mut u8) }
}

/// Allocate a BIGNUM heap object (§1.8.1): header + sign + n_limbs + limbs.
/// Layout mirrors the reader so `print_val` renders it correctly.
fn alloc_bignum_cli(sign: i32, limbs: &[u64]) -> BlissVal {
    let n = limbs.len();
    let total_size = 16 + n * 8;
    let layout = std::alloc::Layout::from_size_align(total_size, 8).unwrap();
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout);
        *(ptr as *mut ObjectHeader) =
            ObjectHeader::new(type_id::BIGNUM, total_size.div_ceil(8) as u16);
        *(ptr.add(8) as *mut i32) = sign;
        *(ptr.add(12) as *mut u32) = n as u32;
        for (idx, &limb) in limbs.iter().enumerate() {
            *(ptr.add(16 + idx * 8) as *mut u64) = limb;
        }
        BlissVal::from_heap_ptr(ptr)
    }
}

/// Extract (numerator, denominator) BlissVals from a RATIO heap object.
fn ratio_parts_val(v: BlissVal) -> Option<(BlissVal, BlissVal)> {
    if !v.is_heap_object() {
        return None;
    }
    unsafe {
        let ptr = v.as_ptr();
        let hdr = *(ptr as *const ObjectHeader);
        if hdr.type_id() != type_id::RATIO {
            return None;
        }
        let num = *(ptr.add(8) as *const BlissVal);
        let den = *(ptr.add(16) as *const BlissVal);
        Some((num, den))
    }
}

// ── Arbitrary-precision integers (little-endian base-2^64 magnitude) ──

fn mag_trim(mut m: Vec<u64>) -> Vec<u64> {
    while !m.is_empty() && *m.last().unwrap() == 0 {
        m.pop();
    }
    m
}

fn mag_cmp(a: &[u64], b: &[u64]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

fn mag_add(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry: u128 = 0;
    for i in 0..a.len().max(b.len()) {
        let av = *a.get(i).unwrap_or(&0) as u128;
        let bv = *b.get(i).unwrap_or(&0) as u128;
        let s = av + bv + carry;
        out.push(s as u64);
        carry = s >> 64;
    }
    if carry != 0 {
        out.push(carry as u64);
    }
    mag_trim(out)
}

/// `a - b`, requiring `a >= b`.
fn mag_sub(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow: i128 = 0;
    for (i, &ai) in a.iter().enumerate() {
        let av = ai as i128;
        let bv = *b.get(i).unwrap_or(&0) as i128;
        let mut d = av - bv - borrow;
        if d < 0 {
            d += 1i128 << 64;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(d as u64);
    }
    mag_trim(out)
}

fn mag_mul(a: &[u64], b: &[u64]) -> Vec<u64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u64; a.len() + b.len()];
    for (i, &av) in a.iter().enumerate() {
        let av = av as u128;
        let mut carry: u128 = 0;
        for (j, &bv) in b.iter().enumerate() {
            let cur = out[i + j] as u128 + av * bv as u128 + carry;
            out[i + j] = cur as u64;
            carry = cur >> 64;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let cur = out[k] as u128 + carry;
            out[k] = cur as u64;
            carry = cur >> 64;
            k += 1;
        }
    }
    mag_trim(out)
}

fn mag_bit(m: &[u64], idx: usize) -> u64 {
    (m[idx / 64] >> (idx % 64)) & 1
}

fn mag_bitlen(m: &[u64]) -> usize {
    if m.is_empty() {
        return 0;
    }
    let top = m.len() - 1;
    64 * top + (64 - m[top].leading_zeros() as usize)
}

fn mag_shl1(m: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(m.len() + 1);
    let mut carry = 0u64;
    for &x in m {
        out.push((x << 1) | carry);
        carry = x >> 63;
    }
    if carry != 0 {
        out.push(carry);
    }
    mag_trim(out)
}

fn mag_set_bit(m: &mut Vec<u64>, idx: usize) {
    let w = idx / 64;
    while m.len() <= w {
        m.push(0);
    }
    m[w] |= 1u64 << (idx % 64);
}

/// Truncating division of magnitudes: returns (quotient, remainder). `b` must
/// be nonzero. Bit-by-bit long division — O(bits · limbs), fine at test sizes.
fn mag_divmod(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    if mag_cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    let n = mag_bitlen(a);
    let mut rem: Vec<u64> = Vec::new();
    let mut quot: Vec<u64> = Vec::new();
    for i in (0..n).rev() {
        rem = mag_shl1(&rem);
        if mag_bit(a, i) == 1 {
            if rem.is_empty() {
                rem.push(1);
            } else {
                rem[0] |= 1;
            }
        }
        if mag_cmp(&rem, b) != Ordering::Less {
            rem = mag_sub(&rem, b);
            mag_set_bit(&mut quot, i);
        }
    }
    (mag_trim(quot), mag_trim(rem))
}

#[derive(Clone, PartialEq, Eq)]
struct BigInt {
    sign: i8,      // -1, 0, or 1
    mag: Vec<u64>, // little-endian; no trailing zero limbs; empty iff sign == 0
}

impl BigInt {
    fn zero() -> BigInt {
        BigInt {
            sign: 0,
            mag: Vec::new(),
        }
    }

    fn one() -> BigInt {
        BigInt {
            sign: 1,
            mag: vec![1],
        }
    }

    fn from_i64(n: i64) -> BigInt {
        if n == 0 {
            return BigInt::zero();
        }
        BigInt {
            sign: if n < 0 { -1 } else { 1 },
            mag: vec![n.unsigned_abs()],
        }
    }

    fn from_mag(sign: i8, mag: Vec<u64>) -> BigInt {
        let mag = mag_trim(mag);
        if mag.is_empty() {
            BigInt::zero()
        } else {
            BigInt {
                sign: if sign < 0 { -1 } else { 1 },
                mag,
            }
        }
    }

    fn from_parts(sign: i32, limbs: &[u64]) -> BigInt {
        BigInt::from_mag(if sign < 0 { -1 } else { 1 }, limbs.to_vec())
    }

    fn is_zero(&self) -> bool {
        self.sign == 0
    }

    fn to_f64(&self) -> f64 {
        let mut f = 0.0f64;
        for &limb in self.mag.iter().rev() {
            f = f * 18446744073709551616.0 + limb as f64; // * 2^64
        }
        if self.sign < 0 { -f } else { f }
    }

    /// Canonicalize: a fixnum when it fits the 61-bit range, else a BIGNUM.
    fn to_val(&self) -> BlissVal {
        if self.is_zero() {
            return BlissVal::from_fixnum(0);
        }
        if self.mag.len() == 1 {
            let m = self.mag[0];
            if self.sign > 0 {
                if m <= FIXNUM_MAX as u64 {
                    return BlissVal::from_fixnum(m as i64);
                }
            } else if m <= FIXNUM_MIN.unsigned_abs() {
                return BlissVal::from_fixnum(-(m as i64));
            }
        }
        alloc_bignum_cli(self.sign as i32, &self.mag)
    }
}

fn big_neg(a: &BigInt) -> BigInt {
    BigInt {
        sign: -a.sign,
        mag: a.mag.clone(),
    }
}

fn big_add(a: &BigInt, b: &BigInt) -> BigInt {
    if a.is_zero() {
        return b.clone();
    }
    if b.is_zero() {
        return a.clone();
    }
    if a.sign == b.sign {
        BigInt::from_mag(a.sign, mag_add(&a.mag, &b.mag))
    } else {
        match mag_cmp(&a.mag, &b.mag) {
            Ordering::Equal => BigInt::zero(),
            Ordering::Greater => BigInt::from_mag(a.sign, mag_sub(&a.mag, &b.mag)),
            Ordering::Less => BigInt::from_mag(b.sign, mag_sub(&b.mag, &a.mag)),
        }
    }
}

fn big_sub(a: &BigInt, b: &BigInt) -> BigInt {
    big_add(a, &big_neg(b))
}

fn big_mul(a: &BigInt, b: &BigInt) -> BigInt {
    if a.is_zero() || b.is_zero() {
        return BigInt::zero();
    }
    BigInt::from_mag(a.sign * b.sign, mag_mul(&a.mag, &b.mag))
}

fn big_cmp(a: &BigInt, b: &BigInt) -> Ordering {
    if a.sign != b.sign {
        return a.sign.cmp(&b.sign);
    }
    match a.sign {
        0 => Ordering::Equal,
        1 => mag_cmp(&a.mag, &b.mag),
        _ => mag_cmp(&b.mag, &a.mag),
    }
}

/// Truncating (toward zero) division: (quotient, remainder). `b` nonzero.
fn big_divmod(a: &BigInt, b: &BigInt) -> (BigInt, BigInt) {
    let (q, r) = mag_divmod(&a.mag, &b.mag);
    (
        BigInt::from_mag(a.sign * b.sign, q),
        BigInt::from_mag(a.sign, r), // remainder takes the dividend's sign
    )
}

/// Exact division, assuming `g` divides `a` with no remainder.
fn big_divexact(a: &BigInt, g: &BigInt) -> BigInt {
    big_divmod(a, g).0
}

/// Non-negative gcd of two integers (gcd(0,0) == 0).
fn big_gcd(x: &BigInt, y: &BigInt) -> BigInt {
    let mut a = BigInt::from_mag(1, x.mag.clone());
    let mut b = BigInt::from_mag(1, y.mag.clone());
    while !b.is_zero() {
        let (_, r) = big_divmod(&a, &b);
        a = b;
        b = BigInt::from_mag(1, r.mag);
    }
    a
}

/// Read a fixnum or BIGNUM into a BigInt. `None` for any other value.
fn bigint_from_val(v: BlissVal) -> Option<BigInt> {
    if v.is_fixnum() {
        return Some(BigInt::from_i64(v.as_fixnum()));
    }
    if v.is_heap_object() {
        unsafe {
            let ptr = v.as_ptr();
            let hdr = *(ptr as *const ObjectHeader);
            if hdr.type_id() == type_id::BIGNUM {
                let sign = *(ptr.add(8) as *const i32);
                let n = *(ptr.add(12) as *const u32) as usize;
                let mut limbs = Vec::with_capacity(n);
                for i in 0..n {
                    limbs.push(*(ptr.add(16 + i * 8) as *const u64));
                }
                return Some(BigInt::from_parts(sign, &limbs));
            }
        }
    }
    None
}

/// An exact rational: denominator kept positive and reduced to lowest terms.
#[derive(Clone)]
struct BigRat {
    num: BigInt,
    den: BigInt,
}

impl BigRat {
    fn from_i64(n: i64) -> BigRat {
        BigRat {
            num: BigInt::from_i64(n),
            den: BigInt::one(),
        }
    }

    fn from_bigint(n: BigInt) -> BigRat {
        BigRat {
            num: n,
            den: BigInt::one(),
        }
    }

    /// Build a reduced rational; `den` must be nonzero.
    fn new(mut num: BigInt, mut den: BigInt) -> BigRat {
        if den.sign < 0 {
            num = big_neg(&num);
            den = big_neg(&den);
        }
        if num.is_zero() {
            return BigRat {
                num: BigInt::zero(),
                den: BigInt::one(),
            };
        }
        let g = big_gcd(&num, &den);
        if big_cmp(&g, &BigInt::one()) != Ordering::Equal {
            num = big_divexact(&num, &g);
            den = big_divexact(&den, &g);
        }
        BigRat { num, den }
    }

    fn is_integer(&self) -> bool {
        big_cmp(&self.den, &BigInt::one()) == Ordering::Equal
    }

    fn to_f64(&self) -> f64 {
        self.num.to_f64() / self.den.to_f64()
    }

    fn to_val(&self) -> BlissVal {
        if self.is_integer() {
            self.num.to_val()
        } else {
            alloc_ratio_cli(self.num.to_val(), self.den.to_val())
        }
    }
}

fn bigrat_neg(a: &BigRat) -> BigRat {
    BigRat {
        num: big_neg(&a.num),
        den: a.den.clone(),
    }
}

fn bigrat_add(a: &BigRat, b: &BigRat) -> BigRat {
    let num = big_add(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den));
    let den = big_mul(&a.den, &b.den);
    BigRat::new(num, den)
}

fn bigrat_sub(a: &BigRat, b: &BigRat) -> BigRat {
    let num = big_sub(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den));
    let den = big_mul(&a.den, &b.den);
    BigRat::new(num, den)
}

fn bigrat_mul(a: &BigRat, b: &BigRat) -> BigRat {
    BigRat::new(big_mul(&a.num, &b.num), big_mul(&a.den, &b.den))
}

/// `a / b`; `b` must be nonzero (the caller checks for zero divisors).
fn bigrat_div(a: &BigRat, b: &BigRat) -> BigRat {
    BigRat::new(big_mul(&a.num, &b.den), big_mul(&a.den, &b.num))
}

fn bigrat_cmp(a: &BigRat, b: &BigRat) -> Ordering {
    // Denominators are positive, so comparing cross-products is order-preserving.
    big_cmp(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den))
}

/// Raise an exact rational to a non-negative integer power.
fn bigrat_pow(base: &BigRat, mut e: u64) -> BigRat {
    let mut result = BigRat::from_i64(1);
    let mut b = base.clone();
    while e > 0 {
        if e & 1 == 1 {
            result = bigrat_mul(&result, &b);
        }
        e >>= 1;
        if e > 0 {
            b = bigrat_mul(&b, &b);
        }
    }
    result
}

/// A fixnum, BIGNUM, or RATIO as an exact rational; `None` otherwise.
fn as_bigrat(v: BlissVal) -> Option<BigRat> {
    if let Some(n) = bigint_from_val(v) {
        return Some(BigRat::from_bigint(n));
    }
    if let Some((nv, dv)) = ratio_parts_val(v) {
        let n = bigint_from_val(nv)?;
        let d = bigint_from_val(dv)?;
        if d.is_zero() {
            return None;
        }
        return Some(BigRat::new(n, d));
    }
    None
}

fn num_val(v: BlissVal) -> Result<f64, BlissError> {
    if v.is_single_float() {
        Ok(v.as_single_float() as f64)
    } else if let Some(r) = as_bigrat(v) {
        Ok(r.to_f64())
    } else {
        Err(BlissError::TypeError {
            datum: v,
            expected: "number".into(),
        })
    }
}

/// Exact-where-possible numeric comparison. Floats force inexact comparison
/// (CL contagion); otherwise operands compare as exact rationals.
fn numeric_cmp(a: BlissVal, b: BlissVal) -> Result<Ordering, BlissError> {
    if a.is_single_float() || b.is_single_float() {
        let av = num_val(a)?;
        let bv = num_val(b)?;
        return Ok(av.partial_cmp(&bv).unwrap_or(Ordering::Equal));
    }
    match (as_bigrat(a), as_bigrat(b)) {
        (Some(ra), Some(rb)) => Ok(bigrat_cmp(&ra, &rb)),
        (None, _) => Err(BlissError::TypeError {
            datum: a,
            expected: "number".into(),
        }),
        (_, None) => Err(BlissError::TypeError {
            datum: b,
            expected: "number".into(),
        }),
    }
}

fn eval_arith(
    args: BlissVal,
    env: &mut Env,
    init_i: i64,
    init_f: f64,
    op_f: fn(f64, f64) -> f64,
    op_r: fn(&BigRat, &BigRat) -> BigRat,
) -> Result<BlissVal, BlissError> {
    let mut acc = BigRat::from_i64(init_i);
    let mut acc_f = init_f;
    let mut is_float = false;
    let mut c = args;
    while c.is_cons() {
        let (af, r) = cp(c);
        let v = eval_form(af, env)?;
        if v.is_single_float() {
            if !is_float {
                is_float = true;
                acc_f = acc.to_f64();
            }
            acc_f = op_f(acc_f, v.as_single_float() as f64);
        } else if let Some(rv) = as_bigrat(v) {
            if is_float {
                acc_f = op_f(acc_f, rv.to_f64());
            } else {
                acc = op_r(&acc, &rv);
            }
        } else {
            return Err(BlissError::TypeError {
                datum: v,
                expected: "number".into(),
            });
        }
        c = r;
    }
    Ok(if is_float {
        BlissVal::from_single_float(acc_f as f32)
    } else {
        acc.to_val()
    })
}

fn eval_arith_sub(args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let mut vals = Vec::new();
    let mut c = args;
    while c.is_cons() {
        let (af, r) = cp(c);
        vals.push(eval_form(af, env)?);
        c = r;
    }
    if vals.is_empty() {
        return Ok(BlissVal::from_fixnum(0));
    }
    if vals.len() == 1 {
        if vals[0].is_single_float() {
            return Ok(BlissVal::from_single_float(-vals[0].as_single_float()));
        }
        if let Some(r) = as_bigrat(vals[0]) {
            return Ok(bigrat_neg(&r).to_val());
        }
        return Err(BlissError::TypeError {
            datum: vals[0],
            expected: "number".into(),
        });
    }
    let mut is_float = vals[0].is_single_float();
    let mut acc_f = num_val(vals[0])?;
    let mut acc = as_bigrat(vals[0]).unwrap_or_else(|| BigRat::from_i64(0));
    for v in &vals[1..] {
        if v.is_single_float() {
            if !is_float {
                is_float = true;
                acc_f = acc.to_f64();
            }
            acc_f -= v.as_single_float() as f64;
        } else if let Some(rv) = as_bigrat(*v) {
            if is_float {
                acc_f -= rv.to_f64();
            } else {
                acc = bigrat_sub(&acc, &rv);
            }
        } else {
            return Err(BlissError::TypeError {
                datum: *v,
                expected: "number".into(),
            });
        }
    }
    Ok(if is_float {
        BlissVal::from_single_float(acc_f as f32)
    } else {
        acc.to_val()
    })
}

fn eval_arith_div(args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let mut vals = Vec::new();
    let mut c = args;
    while c.is_cons() {
        let (af, r) = cp(c);
        vals.push(eval_form(af, env)?);
        c = r;
    }
    if vals.is_empty() {
        return Err(BlissError::ArithmeticError(
            "/ requires at least one argument".into(),
        ));
    }
    if vals.len() == 1 {
        if vals[0].is_single_float() {
            let v = vals[0].as_single_float();
            if v == 0.0 {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            return Ok(BlissVal::from_single_float(1.0 / v));
        }
        if let Some(r) = as_bigrat(vals[0]) {
            if r.num.is_zero() {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            // reciprocal: den/num
            return Ok(bigrat_div(&BigRat::from_i64(1), &r).to_val());
        }
        return Err(BlissError::TypeError {
            datum: vals[0],
            expected: "number".into(),
        });
    }
    let mut is_float = vals[0].is_single_float();
    let mut acc_f = num_val(vals[0])?;
    let mut acc = as_bigrat(vals[0]).unwrap_or_else(|| BigRat::from_i64(0));
    for v in &vals[1..] {
        if v.is_single_float() {
            let dv = v.as_single_float() as f64;
            if dv == 0.0 {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            if !is_float {
                is_float = true;
                acc_f = acc.to_f64();
            }
            acc_f /= dv;
        } else if let Some(rv) = as_bigrat(*v) {
            if rv.num.is_zero() {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            if is_float {
                acc_f /= rv.to_f64();
            } else {
                acc = bigrat_div(&acc, &rv);
            }
        } else {
            return Err(BlissError::TypeError {
                datum: *v,
                expected: "number".into(),
            });
        }
    }
    if is_float {
        Ok(BlissVal::from_single_float(acc_f as f32))
    } else {
        Ok(acc.to_val())
    }
}

fn eval_cmp(
    args: BlissVal,
    env: &mut Env,
    pred: fn(Ordering) -> bool,
) -> Result<BlissVal, BlissError> {
    let (af, r) = cp(args);
    let (bf, _) = cp(r);
    let a = eval_form(af, env)?;
    let b = eval_form(bf, env)?;
    Ok(if pred(numeric_cmp(a, b)?) { T } else { NIL })
}

fn eval_args(args: BlissVal, env: &mut Env) -> Result<Vec<BlissVal>, BlissError> {
    let mut result = Vec::new();
    let mut c = args;
    while c.is_cons() {
        let (af, r) = cp(c);
        result.push(eval_form(af, env)?);
        c = r;
    }
    Ok(result)
}

// ── LET binding (issue #2 fix) ───────────────────────────────────
fn eval_let(cdr: BlissVal, env: &mut Env, sequential: bool) -> Result<BlissVal, BlissError> {
    let (bindings_form, body) = cp(cdr);
    if sequential {
        let mut child_env = env.child();
        let mut c = bindings_form;
        while c.is_cons() {
            let (binding, rest) = cp(c);
            if binding.is_cons() {
                let (var_form, val_rest) = cp(binding);
                let (val_form, _) = cp(val_rest);
                let var_name = sym_name(var_form);
                let val = eval_form(val_form, &mut child_env)?;
                child_env.define_local(&var_name, val);
            } else if binding.is_symbol() {
                let var_name = sym_name(binding);
                child_env.define_local(&var_name, NIL);
            }
            c = rest;
        }
        return eval_progn(body, &mut child_env);
    }

    let mut evaluated = Vec::new();
    let mut c = bindings_form;
    while c.is_cons() {
        let (binding, rest) = cp(c);
        if binding.is_cons() {
            let (var_form, val_rest) = cp(binding);
            let (val_form, _) = cp(val_rest);
            evaluated.push((sym_name(var_form), eval_form(val_form, env)?));
        } else if binding.is_symbol() {
            evaluated.push((sym_name(binding), NIL));
        }
        c = rest;
    }

    let mut child_env = env.child();
    for (name, val) in evaluated {
        child_env.define_local(&name, val);
    }
    eval_progn(body, &mut child_env)
}

// ── DEFUN ────────────────────────────────────────────────────────
fn eval_defun(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (params_form, body) = cp(rest);
    let name = sym_name(name_form);
    let params = extract_params(params_form);
    Rc::make_mut(&mut env.funs).insert(
        name.clone(),
        FunDef {
            params,
            params_form,
            body,
        },
    );
    Ok(name_form)
}

// ── FLET / LABELS local function binding ────────────────────────
// (flet ((name (lambda-list) body...) ...) body...)
// (labels ...) has the same shape but the local functions are mutually
// recursive. Both bind the named functions in a fresh child environment and
// evaluate the body there; because named functions in this evaluator resolve
// their names through the current environment's function table at call time,
// LABELS-style mutual recursion works naturally, and FLET-bound functions are
// visible only within the FLET/LABELS body.
fn eval_flet(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (defs_form, body) = cp(cdr);
    let mut child_env = env.child();
    let mut c = defs_form;
    while c.is_cons() {
        let (def, rest) = cp(c);
        if def.is_cons() {
            let (name_form, def_rest) = cp(def);
            let (params_form, fbody) = cp(def_rest);
            let name = sym_name(name_form);
            let params = extract_params(params_form);
            Rc::make_mut(&mut child_env.funs).insert(
                name,
                FunDef {
                    params,
                    params_form,
                    body: fbody,
                },
            );
        }
        c = rest;
    }
    eval_progn(body, &mut child_env)
}

// ── Extract parameter names from a lambda list ──────────────────
fn extract_params(params_form: BlissVal) -> Vec<String> {
    let mut params = Vec::new();
    let mut c = params_form;
    while c.is_cons() {
        let (p, rest) = cp(c);
        if p.is_symbol() {
            let pname = sym_name(p);
            // Skip lambda-list keywords
            if !pname.starts_with('&') {
                params.push(pname);
            }
        }
        c = rest;
    }
    params
}

// ── Ordinary lambda-list binding ─────────────────────────────────
// Binds required, &optional (with defaults + supplied-p), &rest/&body,
// &key (with defaults, supplied-p, and ((:kw var) ...) form),
// &allow-other-keys (accepted, not enforced), and &aux. Defaults are
// evaluated left-to-right in `env` so they can see earlier parameters.

/// Bare (KEYWORD:-stripped) name of a symbol.
fn key_bare(sym: BlissVal) -> String {
    let n = sym_name(sym);
    n.strip_prefix("KEYWORD:").unwrap_or(&n).to_string()
}

/// Parse an &optional/&aux element: `var` | `(var [default [supplied-p]])`.
fn parse_var_spec(elem: BlissVal) -> (String, BlissVal, Option<String>) {
    if elem.is_symbol() {
        return (sym_name(elem), NIL, None);
    }
    if elem.is_cons() {
        let (var, r) = cp(elem);
        let (default, r2) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
        let supp = if r2.is_cons() {
            Some(sym_name(cp(r2).0))
        } else {
            None
        };
        return (sym_name(var), default, supp);
    }
    (String::new(), NIL, None)
}

/// Parse a &key element: `var` | `(var [default [supp]])` | `((:kw var) [default [supp]])`.
/// Returns (keyword-bare-name, var-name, default-form, supplied-p-var).
fn parse_key_spec(elem: BlissVal) -> (String, String, BlissVal, Option<String>) {
    if elem.is_symbol() {
        let var = sym_name(elem);
        return (var.clone(), var, NIL, None);
    }
    if elem.is_cons() {
        let (head, r) = cp(elem);
        let (default, r2) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
        let supp = if r2.is_cons() {
            Some(sym_name(cp(r2).0))
        } else {
            None
        };
        if head.is_symbol() {
            let var = sym_name(head);
            return (var.clone(), var, default, supp);
        }
        if head.is_cons() {
            let (kw_sym, r3) = cp(head);
            let var = if r3.is_cons() {
                sym_name(cp(r3).0)
            } else {
                String::new()
            };
            return (key_bare(kw_sym), var, default, supp);
        }
    }
    (String::new(), String::new(), NIL, None)
}

/// Look up a keyword's value in a `key value key value ...` argument tail.
fn find_key_arg(plist: &[BlissVal], kw_bare: &str) -> Option<BlissVal> {
    let mut i = 0;
    while i + 1 < plist.len() {
        if plist[i].is_symbol() && key_bare(plist[i]) == kw_bare {
            return Some(plist[i + 1]);
        }
        i += 2;
    }
    None
}

fn bind_lambda_list(
    params_form: BlissVal,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<(), BlissError> {
    #[derive(PartialEq)]
    enum Mode {
        Req,
        Opt,
        Rest,
        Key,
        Aux,
    }
    let mut mode = Mode::Req;
    let mut arg_i = 0usize;
    let mut key_start: Option<usize> = None;
    let mut rest_bound = false;
    let mut saw_key = false;
    let mut allow_other_keys = false;
    let mut key_specs: Vec<(String, String, BlissVal, Option<String>)> = Vec::new();

    let mut c = params_form;
    while c.is_cons() {
        let (elem, rest) = cp(c);
        c = rest;
        if elem.is_symbol() {
            match sym_name(elem).as_str() {
                "&OPTIONAL" => {
                    mode = Mode::Opt;
                    continue;
                }
                "&REST" | "&BODY" => {
                    mode = Mode::Rest;
                    continue;
                }
                "&KEY" => {
                    mode = Mode::Key;
                    saw_key = true;
                    key_start.get_or_insert(arg_i);
                    continue;
                }
                "&AUX" => {
                    mode = Mode::Aux;
                    continue;
                }
                "&ALLOW-OTHER-KEYS" => {
                    allow_other_keys = true;
                    continue;
                }
                _ => {}
            }
        }
        match mode {
            Mode::Req => {
                let v = args.get(arg_i).copied().ok_or_else(|| {
                    BlissError::Internal(format!(
                        "too few arguments for lambda list: missing value for {}",
                        sym_name(elem)
                    ))
                })?;
                arg_i += 1;
                env.define_local(&sym_name(elem), v);
            }
            Mode::Opt => {
                let (var, default_form, supp) = parse_var_spec(elem);
                if arg_i < args.len() {
                    env.define_local(&var, args[arg_i]);
                    arg_i += 1;
                    if let Some(sp) = supp {
                        env.define_local(&sp, T);
                    }
                } else {
                    let dv = if default_form == NIL {
                        NIL
                    } else {
                        eval_form(default_form, env)?
                    };
                    env.define_local(&var, dv);
                    if let Some(sp) = supp {
                        env.define_local(&sp, NIL);
                    }
                }
            }
            Mode::Rest => {
                if rest_bound {
                    return Err(BlissError::Internal(
                        "malformed lambda list: multiple &rest/&body variables".into(),
                    ));
                }
                let remaining = args.get(arg_i..).unwrap_or(&[]);
                env.define_local(&sym_name(elem), vec_to_list(remaining));
                key_start.get_or_insert(arg_i);
                rest_bound = true;
            }
            Mode::Key => {
                let (kw_bare, var, default_form, supp) = parse_key_spec(elem);
                key_specs.push((kw_bare, var, default_form, supp));
            }
            Mode::Aux => {
                let (var, default_form, _) = parse_var_spec(elem);
                let dv = if default_form == NIL {
                    NIL
                } else {
                    eval_form(default_form, env)?
                };
                env.define_local(&var, dv);
            }
        }
    }

    if saw_key {
        let start = key_start.unwrap_or(arg_i);
        let tail = args.get(start..).unwrap_or(&[]);
        if tail.len() % 2 != 0 {
            return Err(BlissError::Internal(
                "keyword arguments must appear in key/value pairs".into(),
            ));
        }

        let mut call_allows_other_keys = false;
        for pair in tail.chunks(2) {
            let key = pair[0];
            if !key.is_symbol() {
                return Err(BlissError::TypeError {
                    datum: key,
                    expected: "keyword".into(),
                });
            }
            let bare = key_bare(key);
            if bare == "ALLOW-OTHER-KEYS" && !pair[1].is_nil() {
                call_allows_other_keys = true;
            }
        }

        for (kw_bare, var, default_form, supp) in &key_specs {
            if let Some(v) = find_key_arg(tail, kw_bare) {
                env.define_local(var, v);
                if let Some(sp) = supp {
                    env.define_local(sp, T);
                }
            } else {
                let dv = if *default_form == NIL {
                    NIL
                } else {
                    eval_form(*default_form, env)?
                };
                env.define_local(var, dv);
                if let Some(sp) = supp {
                    env.define_local(sp, NIL);
                }
            }
        }

        if !(allow_other_keys || call_allows_other_keys) {
            for pair in tail.chunks(2) {
                let bare = key_bare(pair[0]);
                if bare == "ALLOW-OTHER-KEYS" {
                    continue;
                }
                if !key_specs.iter().any(|(kw, _, _, _)| kw == &bare) {
                    return Err(BlissError::Internal(format!(
                        "unexpected keyword argument: {}",
                        bare
                    )));
                }
            }
        }
    } else if !rest_bound && arg_i < args.len() {
        return Err(BlissError::Internal(format!(
            "too many arguments for lambda list: expected {}, got {}",
            arg_i,
            args.len()
        )));
    }

    Ok(())
}

// ── DEFMACRO ─────────────────────────────────────────────────────
fn eval_defmacro(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (params_form, body) = cp(rest);
    let name = sym_name(name_form);

    let mut params = Vec::new();
    let mut rest_param = None;
    let mut c = params_form;
    while c.is_cons() {
        let (p, rest_p) = cp(c);
        if p.is_symbol() {
            let pname = sym_name(p);
            if pname == "&BODY" || pname == "&REST" {
                if rest_p.is_cons() {
                    let (rest_name, rest_after_name) = cp(rest_p);
                    rest_param = Some(sym_name(rest_name));
                    c = rest_after_name;
                    continue;
                }
            } else if !pname.starts_with('&') {
                params.push(pname);
            }
        }
        c = rest_p;
    }

    Rc::make_mut(&mut env.macros).insert(
        name.clone(),
        MacroDef {
            params,
            rest_param,
            body,
        },
    );
    Ok(name_form)
}

fn expand_macro(mdef: &MacroDef, args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let mut child_env = env.child();
    let arg_list = list_to_vec(args);

    let mut arg_idx = 0;
    for param in &mdef.params {
        if arg_idx < arg_list.len() {
            child_env.define_local(param, arg_list[arg_idx]);
            arg_idx += 1;
        } else {
            child_env.define_local(param, NIL);
        }
    }

    if let Some(rest_param) = &mdef.rest_param {
        let body_args = if arg_idx < arg_list.len() {
            vec_to_list(&arg_list[arg_idx..])
        } else {
            NIL
        };
        child_env.define_local(rest_param, body_args);
    }

    // Evaluate the macro body to get the expansion (it should be a quasiquote form)
    eval_progn(mdef.body, &mut child_env)
}

// ── DEFCLASS ─────────────────────────────────────────────────────
fn eval_defclass(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (supers_form, rest2) = cp(rest);
    let (slots_form, _) = cp(rest2);

    let name = sym_name(name_form);

    // Parse superclasses
    let mut supers = Vec::new();
    let super_list = list_to_vec(supers_form);
    for s in &super_list {
        supers.push(sym_name(*s));
    }

    // Parse slots
    let mut slots = Vec::new();
    let slot_list = list_to_vec(slots_form);
    for slot_form in &slot_list {
        if slot_form.is_cons() {
            let (slot_name_form, slot_opts) = cp(*slot_form);
            let slot_name = sym_name(slot_name_form);
            let mut initarg = None;
            let mut accessor = None;

            // Parse slot options
            let opts = list_to_vec(slot_opts);
            let mut i = 0;
            while i < opts.len() {
                let opt_name = sym_name(opts[i]);
                let opt_bare = opt_name
                    .trim_start_matches("KEYWORD:")
                    .trim_start_matches(':');
                if opt_bare == "INITARG" {
                    if i + 1 < opts.len() {
                        let ia = sym_name(opts[i + 1]);
                        initarg = Some(
                            ia.trim_start_matches("KEYWORD:")
                                .trim_start_matches(':')
                                .to_string(),
                        );
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "ACCESSOR" {
                    if i + 1 < opts.len() {
                        accessor = Some(sym_name(opts[i + 1]));
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "INITFORM"
                    || opt_bare == "READER"
                    || opt_bare == "WRITER"
                    || opt_bare == "ALLOCATION"
                    || opt_bare == "TYPE"
                    || opt_bare == "DOCUMENTATION"
                {
                    // Skip known slot option with value
                    i += 2;
                } else {
                    i += 1;
                }
            }

            slots.push(SlotDef {
                name: slot_name,
                initarg,
                accessor,
            });
        } else if slot_form.is_symbol() {
            slots.push(SlotDef {
                name: sym_name(*slot_form),
                initarg: None,
                accessor: None,
            });
        }
    }

    Rc::make_mut(&mut env.classes).insert(
        name.clone(),
        ClassDef {
            name: name.clone(),
            supers,
            slots,
        },
    );
    Ok(name_form)
}

// ── DEFMETHOD ────────────────────────────────────────────────────
fn eval_defmethod(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (spec_params_form, body) = cp(rest);
    let name = sym_name(name_form);

    // Parse specialized params: ((var class) ...)
    let params_list = list_to_vec(spec_params_form);
    let mut params = Vec::new();
    let mut specializer = "T".to_string();

    for p in &params_list {
        if p.is_cons() {
            let (var_form, rest_p) = cp(*p);
            let (class_form, _) = cp(rest_p);
            params.push(sym_name(var_form));
            specializer = sym_name(class_form);
        } else if p.is_symbol() {
            params.push(sym_name(*p));
        }
    }

    Rc::make_mut(&mut env.methods)
        .entry(name.clone())
        .or_default()
        .push(MethodDef {
            specializer,
            params,
            body,
        });
    Ok(name_form)
}

// ── MAKE-INSTANCE ────────────────────────────────────────────────
fn eval_make_instance(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (class_form, init_args) = cp(cdr);
    let class_val = eval_form(class_form, env)?;
    let class_name = sym_name(class_val);

    let class_def = env
        .classes
        .get(&class_name)
        .cloned()
        .ok_or_else(|| BlissError::Internal(format!("Unknown class: {}", class_name)))?;

    // Parse keyword init args
    let args_vec = list_to_vec(init_args);
    let mut init_map: HashMap<String, BlissVal> = HashMap::new();
    let mut i = 0;
    while i + 1 < args_vec.len() {
        let key_val = eval_form(args_vec[i], env)?;
        let key_name = sym_name(key_val)
            .trim_start_matches("KEYWORD:")
            .trim_start_matches(':')
            .to_string();
        let val = eval_form(args_vec[i + 1], env)?;
        init_map.insert(key_name, val);
        i += 2;
    }

    // Build instance as a list: (CLASS-NAME (slot1 . val1) (slot2 . val2) ...)
    let mut slot_pairs = Vec::new();
    for slot in &class_def.slots {
        let val = if let Some(ref ia) = slot.initarg {
            init_map.get(ia).copied().unwrap_or(NIL)
        } else {
            init_map.get(&slot.name).copied().unwrap_or(NIL)
        };
        let name_val = arena_str(&slot.name);
        slot_pairs.push(arena_cons(name_val, val));
    }

    let class_name_val = arena_str(&class_name);
    let mut result = vec_to_list(&slot_pairs);
    result = arena_cons(class_name_val, result);
    Ok(result)
}

fn get_instance_class_name(val: BlissVal) -> String {
    if val.is_cons() {
        let (car, _) = cp(val);
        return val_as_str(car);
    }
    "T".to_string()
}

fn get_instance_slots(val: BlissVal) -> Vec<(String, BlissVal)> {
    let mut result = Vec::new();
    if val.is_cons() {
        let (_, slots) = cp(val); // skip class name
        let mut c = slots;
        while c.is_cons() {
            let (pair, rest) = cp(c);
            if pair.is_cons() {
                let (name_val, slot_val) = cp(pair);
                result.push((val_as_str(name_val), slot_val));
            }
            c = rest;
        }
    }
    result
}

fn is_subclass(child: &str, parent: &str, env: &Env) -> bool {
    if child == parent {
        return true;
    }
    if let Some(class_def) = env.classes.get(child) {
        for super_name in &class_def.supers {
            if is_subclass(super_name, parent, env) {
                return true;
            }
        }
    }
    false
}

// ── Apply function (lambda or named) ─────────────────────────────
fn apply_function(
    fn_val: BlissVal,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    // Function could be a lambda form, a symbol naming a function, or a closure
    if fn_val.is_symbol() {
        let name = sym_name(fn_val);
        if let Some(fdef) = env.funs.get(&name).cloned() {
            let mut child_env = env.child();
            bind_lambda_list(fdef.params_form, args, &mut child_env)?;
            return eval_progn(fdef.body, &mut child_env);
        }
        // Builtin: synthesize `(name 'arg1 'arg2 ...)` and evaluate it so the
        // full operator-position builtin set (not just apply_builtin's subset)
        // is reachable through funcall/apply/mapcar.
        let quote_sym = resolve_sym("QUOTE").unwrap_or(NIL);
        let mut items = Vec::with_capacity(args.len() + 1);
        items.push(fn_val);
        for a in args {
            items.push(arena_cons(quote_sym, arena_cons(*a, NIL)));
        }
        let form = vec_to_list(&items);
        return eval_form(form, env);
    }
    if fn_val.is_cons() {
        let (lh, lr) = cp(fn_val);
        // Check for closure: (BLISS::CLOSURE . id)
        if lh.is_symbol() && sym_name(lh) == "BLISS::CLOSURE" && lr.is_fixnum() {
            let id = lr.as_fixnum() as u64;
            let closure = { env.closures.borrow().get(&id).cloned() };
            if let Some(closure) = closure {
                let mut child_env = env.child_with_parent(Rc::clone(&closure.captured_frame));
                // Bind parameters
                bind_lambda_list(closure.params_form, args, &mut child_env)?;
                return eval_progn(closure.body, &mut child_env);
            }
        }
        if lh.is_symbol() && sym_name(lh) == "LAMBDA" {
            let (params_form, body) = cp(lr);
            let mut child_env = env.child();
            bind_lambda_list(params_form, args, &mut child_env)?;
            return eval_progn(body, &mut child_env);
        }
    }
    Err(BlissError::Internal(format!("Cannot apply: {:?}", fn_val)))
}

#[expect(
    dead_code,
    reason = "legacy builtin dispatch is retained during evaluator consolidation"
)]
fn apply_builtin(name: &str, args: &[BlissVal], _env: &mut Env) -> Result<BlissVal, BlissError> {
    match name {
        "+" => {
            let mut sum: f64 = 0.0;
            let mut is_f = false;
            for a in args {
                let v = num_val(*a)?;
                sum += v;
                if a.is_single_float() {
                    is_f = true;
                }
            }
            Ok(if is_f {
                BlissVal::from_single_float(sum as f32)
            } else {
                BlissVal::from_fixnum(sum as i64)
            })
        }
        "-" => {
            if args.is_empty() {
                return Ok(BlissVal::from_fixnum(0));
            }
            if args.len() == 1 {
                let v = num_val(args[0])?;
                return Ok(BlissVal::from_fixnum((-v) as i64));
            }
            let mut acc = num_val(args[0])?;
            let mut is_f = args[0].is_single_float();
            for a in &args[1..] {
                acc -= num_val(*a)?;
                if a.is_single_float() {
                    is_f = true;
                }
            }
            Ok(if is_f {
                BlissVal::from_single_float(acc as f32)
            } else {
                BlissVal::from_fixnum(acc as i64)
            })
        }
        "*" => {
            let mut prod: f64 = 1.0;
            let mut is_f = false;
            for a in args {
                let v = num_val(*a)?;
                prod *= v;
                if a.is_single_float() {
                    is_f = true;
                }
            }
            Ok(if is_f {
                BlissVal::from_single_float(prod as f32)
            } else {
                BlissVal::from_fixnum(prod as i64)
            })
        }
        "CONS" => {
            if args.len() >= 2 {
                Ok(arena_cons(args[0], args[1]))
            } else {
                Err(BlissError::Internal("CONS requires 2 arguments".into()))
            }
        }
        "CAR" | "FIRST" => {
            if args.is_empty() {
                return Ok(NIL);
            }
            let v = args[0];
            if v.is_nil() {
                return Ok(NIL);
            }
            if v.is_cons() {
                let (a, _) = cp(v);
                return Ok(a);
            }
            Err(BlissError::TypeError {
                datum: v,
                expected: "list".into(),
            })
        }
        "CDR" | "REST" => {
            if args.is_empty() {
                return Ok(NIL);
            }
            let v = args[0];
            if v.is_nil() {
                return Ok(NIL);
            }
            if v.is_cons() {
                let (_, d) = cp(v);
                return Ok(d);
            }
            Err(BlissError::TypeError {
                datum: v,
                expected: "list".into(),
            })
        }
        _ => Err(BlissError::UndefinedFunction(
            resolve_sym(name).unwrap_or(NIL),
        )),
    }
}

// ── FLOOR ────────────────────────────────────────────────────────
fn eval_floor(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (af, r) = cp(cdr);
    let a = eval_form(af, env)?;
    let av = num_val(a)?;
    if r.is_cons() {
        let (bf, _) = cp(r);
        let b = eval_form(bf, env)?;
        let bv = num_val(b)?;
        if bv == 0.0 {
            return Err(BlissError::ArithmeticError("division by zero".into()));
        }
        let q = (av / bv).floor() as i64;
        let rem = av - (q as f64) * bv;
        env.set_mv(vec![
            BlissVal::from_fixnum(q),
            BlissVal::from_fixnum(rem as i64),
        ]);
        return Ok(BlissVal::from_fixnum(q));
    }
    let q = av.floor() as i64;
    let rem = av - q as f64;
    env.set_mv(vec![
        BlissVal::from_fixnum(q),
        BlissVal::from_single_float(rem as f32),
    ]);
    Ok(BlissVal::from_fixnum(q))
}

// ── MULTIPLE-VALUE-BIND ─────────────────────────────────────────
fn eval_multiple_value_bind(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (vars_form, rest) = cp(cdr);
    let (values_form, body) = cp(rest);

    // Evaluate the values form
    env.clear_mv();
    let primary = eval_form(values_form, env)?;
    let mv = env.mv.clone();

    // Bind variables
    let var_names: Vec<String> = list_to_vec(vars_form)
        .iter()
        .map(|v| sym_name(*v))
        .collect();
    let mut child_env = env.child();

    for (i, var_name) in var_names.iter().enumerate() {
        let val = if i == 0 {
            primary
        } else if i < mv.len() {
            mv[i]
        } else {
            NIL
        };
        child_env.define_local(var_name, val);
    }

    eval_progn(body, &mut child_env)
}

// ── HANDLER-CASE ────────────────────────────────────────────────
fn eval_handler_case(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (protected_form, clauses) = cp(cdr);

    match eval_form(protected_form, env) {
        Ok(val) => Ok(val),
        Err(e) => {
            // Try to find a matching handler clause
            let mut c = clauses;
            while c.is_cons() {
                let (clause, rest) = cp(c);
                let (type_form, clause_rest) = cp(clause);
                let type_name = sym_name(type_form);

                // Check if this handler matches
                if type_name == "ERROR" || type_name == "CONDITION" || type_name == "T" {
                    let (bind_list, handler_body) = cp(clause_rest);
                    let mut child_env = env.child();

                    // Bind the condition variable
                    if bind_list.is_cons() {
                        let (var_form, _) = cp(bind_list);
                        let var_name = sym_name(var_form);
                        let err_msg = format!("{}", e);
                        child_env.define_local(&var_name, arena_str(&err_msg));
                    }

                    return eval_progn(handler_body, &mut child_env);
                }
                c = rest;
            }
            Err(e) // No matching handler
        }
    }
}

// ── HANDLER-BIND ────────────────────────────────────────────────
fn eval_handler_bind(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (bindings_form, body) = cp(cdr);

    // Parse handler bindings and install them
    let mut c = bindings_form;
    while c.is_cons() {
        let (binding, rest) = cp(c);
        let (type_form, handler_rest) = cp(binding);
        let (handler_form, _) = cp(handler_rest);
        let type_name = sym_name(type_form);
        env.handlers.push(HandlerEntry {
            type_name,
            handler: handler_form,
        });
        c = rest;
    }

    let result = eval_progn(body, env);

    // Clean up handlers (pop what we added)
    let binding_count = list_to_vec(bindings_form).len();
    for _ in 0..binding_count {
        env.handlers.pop();
    }

    result
}

// ── CERROR ───────────────────────────────────────────────────────
fn eval_cerror(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (_continue_form, rest) = cp(cdr);
    let (msg_form, _) = cp(rest);
    let msg = eval_form(msg_form, env)?;

    // Install a CONTINUE restart
    env.restarts.push(RestartEntry {
        name: "CONTINUE".to_string(),
        body: None,
    });

    // Check if there's a handler that will invoke the restart
    for handler in env.handlers.clone().iter().rev() {
        if handler.type_name == "ERROR"
            || handler.type_name == "CONDITION"
            || handler.type_name == "T"
        {
            let hfn = handler.handler;
            let cond = arena_str(&val_as_str(msg));
            match apply_function(hfn, &[cond], env) {
                Ok(_) => {
                    // Handler returned normally
                    env.restarts.pop();
                    return Ok(NIL);
                }
                Err(e) => {
                    // Check if it's a restart invocation
                    let err_str = format!("{}", e);
                    if err_str.contains("__RESTART_INVOKED__:CONTINUE") {
                        env.restarts.pop();
                        return Ok(NIL);
                    }
                    env.restarts.pop();
                    return Err(e);
                }
            }
        }
    }

    env.restarts.pop();
    // If no handler, signal the error
    Err(BlissError::Internal(format!("ERROR: {}", val_as_str(msg))))
}

// ── WITH-OPEN-FILE ──────────────────────────────────────────────
fn eval_with_open_file(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (binding, body) = cp(cdr);
    let (var_form, rest) = cp(binding);
    let (path_form, opts_rest) = cp(rest);

    let var_name = sym_name(var_form);
    let path_val = eval_form(path_form, env)?;
    let path = val_as_str(path_val);

    // Check sandbox mode
    if env.sandbox {
        return Err(BlissError::SandboxViolation(format!(
            "File access denied in sandbox mode: {}",
            path
        )));
    }

    // Parse options
    let opts = list_to_vec(opts_rest);
    let mut direction = StreamDirection::Input;
    let mut i = 0;
    while i < opts.len() {
        let opt_name = sym_name(opts[i]);
        let opt_bare = opt_name
            .trim_start_matches("KEYWORD:")
            .trim_start_matches(':');
        if opt_bare == "DIRECTION" {
            if i + 1 < opts.len() {
                let dir = sym_name(opts[i + 1]);
                let dir_bare = dir.trim_start_matches("KEYWORD:").trim_start_matches(':');
                if dir_bare == "OUTPUT" {
                    direction = StreamDirection::Output;
                }
                i += 2;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    // Open the file and create a stream
    let stream_val = open_stream(&path, direction)?;

    let mut child_env = env.child();
    child_env.define_local(&var_name, stream_val);

    // Evaluate body, then close the stream (unwind-protect style)
    let result = eval_progn(body, &mut child_env);
    close_stream(stream_val)?;
    result
}

// ── DEFPACKAGE ──────────────────────────────────────────────────
fn eval_defpackage(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, opts) = cp(cdr);
    let name_val = eval_form(name_form, env)?;
    let pkg_name = val_as_str(name_val)
        .trim_start_matches("KEYWORD:")
        .trim_start_matches(':')
        .to_uppercase();

    let mut exports = Vec::new();
    let mut uses = Vec::new();

    let mut c = opts;
    while c.is_cons() {
        let (opt, rest) = cp(c);
        if opt.is_cons() {
            let (key, val_list) = cp(opt);
            let key_name = sym_name(key);
            let key_bare = key_name
                .trim_start_matches("KEYWORD:")
                .trim_start_matches(':');
            if key_bare == "USE" {
                let mut vc = val_list;
                while vc.is_cons() {
                    let (v, vr) = cp(vc);
                    uses.push(
                        sym_name(v)
                            .trim_start_matches("KEYWORD:")
                            .trim_start_matches(':')
                            .to_uppercase(),
                    );
                    vc = vr;
                }
            } else if key_bare == "EXPORT" {
                let mut vc = val_list;
                while vc.is_cons() {
                    let (v, vr) = cp(vc);
                    exports.push(
                        sym_name(v)
                            .trim_start_matches("KEYWORD:")
                            .trim_start_matches(':')
                            .to_uppercase(),
                    );
                    vc = vr;
                }
            }
        }
        c = rest;
    }

    Rc::make_mut(&mut env.packages).insert(
        pkg_name.clone(),
        PackageDef {
            name: pkg_name.clone(),
            exports,
            uses,
            symbols: HashMap::new(),
        },
    );
    reader::register_package(&pkg_name);

    Ok(T)
}

// ── FORMAT ───────────────────────────────────────────────────────
fn eval_format(args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (df, r) = cp(args);
    let dest = eval_form(df, env)?;
    let (ff, fa) = cp(r);
    let fv = eval_form(ff, env)?;
    let fs = val_as_str(fv);
    let mut av = Vec::new();
    let mut c = fa;
    while c.is_cons() {
        let (af, r2) = cp(c);
        av.push(eval_form(af, env)?);
        c = r2;
    }
    let mut result = String::new();
    let fc: Vec<char> = fs.chars().collect();
    let (mut i, mut ai) = (0, 0);
    while i < fc.len() {
        if fc[i] == '~' && i + 1 < fc.len() {
            match fc[i + 1] {
                'A' | 'a' => {
                    if ai < av.len() {
                        princ_val(av[ai], &mut result);
                        ai += 1;
                    }
                    i += 2;
                }
                'D' | 'd' | 'S' | 's' => {
                    if ai < av.len() {
                        result.push_str(&format_val(av[ai]));
                        ai += 1;
                    }
                    i += 2;
                }
                '~' => {
                    result.push('~');
                    i += 2;
                }
                '%' => {
                    result.push('\n');
                    i += 2;
                }
                _ => {
                    result.push(fc[i]);
                    i += 1;
                }
            }
        } else {
            result.push(fc[i]);
            i += 1;
        }
    }
    if dest == T {
        print!("{}", result);
        return Ok(NIL);
    }
    Ok(arena_str(&result))
}

fn val_as_str(val: BlissVal) -> String {
    if val.is_heap_object() {
        unsafe {
            let p = val.as_ptr();
            let h = *(p as *const ObjectHeader);
            if h.type_id() == type_id::SIMPLE_BASE_STRING {
                let len = *(p.add(8) as *const u64) as usize;
                let data = std::slice::from_raw_parts(p.add(16), len);
                if let Ok(s) = std::str::from_utf8(data) {
                    return s.to_string();
                }
            }
        }
    }
    if val.is_symbol() {
        let name = sym_name(val);
        // For symbols used as keyword args, strip package prefix
        return name;
    }
    format_val(val)
}

fn vals_equal(a: BlissVal, b: BlissVal) -> bool {
    if a == b {
        return true;
    }
    if a.is_fixnum() && b.is_fixnum() {
        return a.as_fixnum() == b.as_fixnum();
    }
    if a.is_single_float() && b.is_single_float() {
        return a.as_single_float() == b.as_single_float();
    }
    if a.is_string() && b.is_string() {
        return val_as_str(a) == val_as_str(b);
    }
    if a.is_cons() && b.is_cons() {
        let (a_car, a_cdr) = cp(a);
        let (b_car, b_cdr) = cp(b);
        return vals_equal(a_car, b_car) && vals_equal(a_cdr, b_cdr);
    }
    false
}

// ── CLI driver ─────────────────────────────────────────────────────
pub fn run(args: &[String]) -> Result<i32, BlissError> {
    let ca = CliArgs::parse(args)?;
    if ca.help {
        print_help();
        return Ok(0);
    }
    if ca.version {
        print_version();
        return Ok(0);
    }

    let mut env = Env::new(ca.sandbox);

    // Set *command-line-args* (issue #4)
    if !ca.cl_args.is_empty() {
        let args_list: Vec<BlissVal> = ca.cl_args.iter().map(|s| arena_str(s)).collect();
        let args_val = vec_to_list(&args_list);
        env.define_local("*COMMAND-LINE-ARGS*", args_val);
    } else {
        env.define_local("*COMMAND-LINE-ARGS*", NIL);
    }

    // Load the Lisp bootstrap prelude. It defines standard-CL forms (defvar,
    // push, incf, ...) that are part of the language, not an optional feature,
    // so it loads unconditionally — before any init file, --eval, --load, or
    // script — for every mode. `--no-bootstrap` opts out (raw evaluator);
    // `--bootstrap` is now the default and kept only for compatibility. A
    // failure here is fatal: the prelude is core.
    if !ca.no_bootstrap {
        let contents = boot_prelude_source()?;
        read_eval_all_env(&contents, &mut env)?;
    }

    // Load init file unless --no-init (issue #8)
    if !ca.no_init && ca.eval.is_none() && ca.load.is_none() && ca.script.is_none() {
        // Try to load init file
        if let Ok(init_path) = std::env::var("BLISS_INIT_FILE") {
            if let Ok(contents) = std::fs::read_to_string(&init_path) {
                let _ = read_eval_all_env(&contents, &mut env);
            }
        } else {
            // Try ~/.blissrc
            if let Ok(home) = std::env::var("HOME") {
                let init_path = format!("{}/.blissrc", home);
                if let Ok(contents) = std::fs::read_to_string(&init_path) {
                    let _ = read_eval_all_env(&contents, &mut env);
                }
            }
        }
    }

    // Load image if specified (issue #8)
    if let Some(ref image_path) = ca.image {
        if let Ok(contents) = std::fs::read_to_string(image_path) {
            read_eval_all_env(&contents, &mut env)?;
        }
    }

    if let Some(ref expr) = ca.eval {
        return run_eval_env(expr, &mut env);
    }
    if let Some(ref path) = ca.load {
        return run_load_env(path, &mut env);
    }
    if let Some(ref script) = ca.script {
        return run_script_env(script, &mut env);
    }
    run_repl_env(&mut env)
}

fn run_eval_env(expr: &str, env: &mut Env) -> Result<i32, BlissError> {
    match with_eval_context(env, EvalContext::Eval, |env| read_eval_all_env(expr, env)) {
        Ok(result) => {
            println!("{}", format_val(result));
            // When the top-level form yielded multiple values, echo the
            // secondary values too (one per line). env.mv holds the full value
            // list only when the last form actually produced multiple values;
            // guard on mv[0] == result so a stale channel from a nested form
            // does not leak extra output.
            let mv = env.mv.clone();
            if mv.len() > 1 && mv[0] == result {
                for v in &mv[1..] {
                    println!("{}", format_val(*v));
                }
            }
            Ok(0)
        }
        Err(e) => {
            eprintln!("ERROR: {}", describe_err(&e));
            Err(e)
        }
    }
}

/// Render an error for display, resolving symbol indices to their names so
/// messages read `undefined function: FOO` instead of `... Symbol(147)`.
fn describe_err(e: &BlissError) -> String {
    match e {
        BlissError::UndefinedFunction(s) => format!("undefined function: {}", sym_name(*s)),
        BlissError::UnboundVariable(s) => format!("unbound variable: {}", sym_name(*s)),
        _ => format!("{}", e),
    }
}

fn run_load_env(path: &str, env: &mut Env) -> Result<i32, BlissError> {
    match load_path_into_env(path, env) {
        Ok(_) => Ok(0),
        Err(e) => {
            eprintln!("ERROR: {}", describe_err(&e));
            Err(e)
        }
    }
}

fn run_script_env(path: &str, env: &mut Env) -> Result<i32, BlissError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| BlissError::FileError(format!("cannot read {}: {}", path, e)))?;
    match read_eval_all_env(&contents, env) {
        Ok(_) => Ok(0),
        Err(e) => {
            eprintln!("ERROR: {}", describe_err(&e));
            Err(e)
        }
    }
}

// Keep standalone versions for backward compatibility
#[allow(dead_code)]
fn run_eval(expr: &str) -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_eval_env(expr, &mut env)
}

#[allow(dead_code)]
fn run_load(path: &str) -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_load_env(path, &mut env)
}

#[allow(dead_code)]
fn run_script(path: &str) -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_script_env(path, &mut env)
}

pub fn help_text() -> &'static str {
    concat!(
        "Usage: bliss [OPTIONS] [SCRIPT] [-- CL-ARGS...]\n",
        "\n",
        "Bliss Common Lisp\n",
        "\n",
        "Options:\n",
        "  --help               Print this help message and exit\n",
        "  --version            Print version information and exit\n",
        "  --eval, -e EXPR      Evaluate EXPR and exit\n",
        "  --load FILE          Load FILE and exit\n",
        "  --image FILE         Path to the boot image\n",
        "  --no-image           Start without loading an image\n",
        "  --bootstrap          Deprecated; the prelude now loads by default\n",
        "  --no-bootstrap       Skip the bootstrap prelude (raw evaluator)\n",
        "  --workers N          Number of worker threads\n",
        "  --heap-size SIZE     Heap size (e.g. 512M, 1G)\n",
        "  --tlab-size SIZE     Per-thread TLAB size\n",
        "  --nursery-size SIZE  Nursery size\n",
        "  --stack-size SIZE    CL stack size per green thread\n",
        "  --gc-log FILE        Write GC logs to FILE\n",
        "  --jit-dump           Emit jitdump metadata\n",
        "  --log-level LEVEL    Set log level (error|warn|info|debug|trace)\n",
        "  --sandbox            Enable sandbox mode\n",
        "  --no-init            Skip loading the init file\n",
        "\n",
        "Arguments after -- are passed through to CL as *command-line-args*.\n",
    )
}

pub fn print_help() {
    print!("{}", help_text());
}

pub fn print_version() {
    println!("bliss {}", env!("CARGO_PKG_VERSION"));
}

// ── REPL driver ────────────────────────────────────────────────────
pub fn run_repl() -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_repl_env(&mut env)
}

fn run_repl_env(env: &mut Env) -> Result<i32, BlissError> {
    let _config = ReplConfig::default();
    println!("Bliss Common Lisp {}", env!("CARGO_PKG_VERSION"));
    println!("Type (quit) to exit.");
    println!();
    // Promote initial allocations (env setup, image load) to permanent
    ARENA.with(|a| a.borrow_mut().promote_all());
    let stdin = std::io::stdin();
    let mut input = String::new();
    let mut in_debugger = false;
    loop {
        eprint!("{}", if in_debugger { "Debug> " } else { "BLISS> " });
        input.clear();
        match stdin.read_line(&mut input) {
            Ok(0) => {
                println!();
                return Ok(0);
            }
            Ok(_) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed == "(quit)" || trimmed == "(exit)" {
                    return Ok(0);
                }
                if in_debugger {
                    if trimmed == ":abort" || trimmed == ":a" {
                        in_debugger = false;
                        continue;
                    }
                    eprintln!("Unknown debugger command: {}", trimmed);
                    continue;
                }
                match read_eval_all_env(trimmed, env) {
                    Ok(result) => {
                        println!("{}", format_val(result));
                        // Promote allocations from this eval (they may be stored in env)
                        ARENA.with(|a| a.borrow_mut().promote_all());
                    }
                    Err(e) => {
                        eprintln!("ERROR: {}", describe_err(&e));
                        in_debugger = true;
                        // On error, temporary allocations can be freed
                        ARENA.with(|a| {
                            let mut arena = a.borrow_mut();
                            if arena.should_compact() {
                                arena.compact();
                            }
                        });
                    }
                }
            }
            Err(e) => return Err(BlissError::Internal(format!("read error: {}", e))),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReplConfig {
    pub history_file: String,
    pub history_size: usize,
    pub syntax_highlighting: bool,
}
impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            history_file: "~/.bliss/repl-history".into(),
            history_size: 1000,
            syntax_highlighting: true,
        }
    }
}
