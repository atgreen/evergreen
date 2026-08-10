//! FORMAT and pretty-printer.
//!
//! See spec §5.9.

use bliss_rt::error::BlissError;
use bliss_rt::object::{ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, NIL, T};

// ── String allocation ─────────────────────────────────────────────

/// Allocate a BlissVal string using the interned string table from the
/// streams module.  This ensures that format-produced strings compare
/// pointer-equal to strings created via `make_lisp_string()`.
fn make_bliss_string(s: &str) -> BlissVal {
    crate::streams::make_lisp_string(s)
}

// ── String extraction ────────────────────────────────────────────

/// Extract Rust string from a BlissString heap object.
/// Returns None if v is not a string-typed heap object.
fn extract_bliss_string(v: BlissVal) -> Option<String> {
    if !v.is_heap_object() {
        return None;
    }
    unsafe {
        let ptr = v.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        let tid = header.type_id();
        if tid != type_id::SIMPLE_BASE_STRING
            && tid != type_id::SIMPLE_CHARACTER_STRING
        {
            return None;
        }
        let length = *(ptr.add(8) as *const u64) as usize;
        let data_ptr = ptr.add(16);
        let bytes = std::slice::from_raw_parts(data_ptr, length);
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Walk a cons-cell linked list and collect all car values into a Vec.
fn cons_list_to_vec(v: BlissVal) -> Vec<BlissVal> {
    let mut result = Vec::new();
    let mut current = v;
    while current.is_cons() {
        unsafe {
            let ptr = current.as_ptr() as *const bliss_rt::object::ConsCell;
            result.push((*ptr).car);
            current = (*ptr).cdr;
        }
    }
    result
}

// ── Helpers ───────────────────────────────────────────────────────

fn blissval_to_print_string(v: BlissVal, escapep: bool) -> String {
    if v.is_nil() {
        return if escapep { "NIL".into() } else { "NIL".into() };
    }
    if v == T {
        return "T".into();
    }
    if v.is_fixnum() {
        return format!("{}", v.as_fixnum());
    }
    if v.is_character() {
        let c = v.as_char();
        return if escapep { format!("#\\{}", c) } else { format!("{}", c) };
    }
    if v.is_single_float() {
        return format!("{}", v.as_single_float());
    }
    if v.is_heap_object() {
        // Check if it's a string and extract its content
        if let Some(s) = extract_bliss_string(v) {
            return if escapep { format!("\"{}\"", s) } else { s };
        }
        return format!("#<heap-object {:?}>", v);
    }
    format!("#<object {:?}>", v)
}

fn format_integer(n: i64, radix: u32, colon: bool, at_sign: bool, mincol: usize, padchar: char) -> String {
    let negative = n < 0;
    let abs = if n == i64::MIN { (n as u128).wrapping_neg() as u64 } else { n.unsigned_abs() };
    let digits = if abs == 0 {
        "0".to_string()
    } else {
        let mut d = String::new();
        let mut v = abs;
        while v > 0 {
            let rem = (v % radix as u64) as u32;
            d.push(char::from_digit(rem, radix).unwrap().to_ascii_uppercase());
            v /= radix as u64;
        }
        d.chars().rev().collect()
    };
    let with_commas = if colon && radix == 10 {
        insert_commas(&digits)
    } else {
        digits
    };
    let sign = if negative { "-".to_string() }
               else if at_sign { "+".to_string() }
               else { String::new() };
    let result = format!("{}{}", sign, with_commas);
    if result.len() < mincol {
        let pad: String = std::iter::repeat(padchar).take(mincol - result.len()).collect();
        format!("{}{}", pad, result)
    } else {
        result
    }
}

fn insert_commas(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 { result.push(','); }
        result.push(c);
    }
    result.chars().rev().collect()
}

fn cardinal(n: i64) -> String {
    if n == 0 { return "zero".into(); }
    let mut result = String::new();
    let mut v = n;
    if v < 0 { result.push_str("negative "); v = -v; }
    let ones = ["", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
                "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen",
                "seventeen", "eighteen", "nineteen"];
    let tens = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];
    if v >= 1_000_000 {
        result.push_str(&cardinal(v / 1_000_000));
        result.push_str(" million");
        v %= 1_000_000;
        if v > 0 { result.push(' '); }
    }
    if v >= 1000 {
        result.push_str(&cardinal(v / 1000));
        result.push_str(" thousand");
        v %= 1000;
        if v > 0 { result.push(' '); }
    }
    if v >= 100 {
        result.push_str(ones[v as usize / 100]);
        result.push_str(" hundred");
        v %= 100;
        if v > 0 { result.push(' '); }
    }
    if v >= 20 {
        result.push_str(tens[v as usize / 10]);
        v %= 10;
        if v > 0 { result.push('-'); result.push_str(ones[v as usize]); }
    } else if v > 0 {
        result.push_str(ones[v as usize]);
    }
    result
}

fn ordinal(n: i64) -> String {
    let c = cardinal(n);
    if c.ends_with("one") { format!("{}first", &c[..c.len()-3]) }
    else if c.ends_with("two") { format!("{}second", &c[..c.len()-3]) }
    else if c.ends_with("three") { format!("{}third", &c[..c.len()-5]) }
    else if c.ends_with("five") { format!("{}fifth", &c[..c.len()-4]) }
    else if c.ends_with("eight") { format!("{}eighth", &c[..c.len()-5]) }
    else if c.ends_with("nine") { format!("{}ninth", &c[..c.len()-4]) }
    else if c.ends_with("twelve") { format!("{}twelfth", &c[..c.len()-6]) }
    else if c.ends_with('y') { format!("{}ieth", &c[..c.len()-1]) }
    else { format!("{}th", c) }
}

fn to_roman(n: i64, old: bool) -> String {
    if n <= 0 || n > 3999 { return format!("{}", n); }
    let mut result = String::new();
    let mut v = n as u32;
    let vals: &[(u32, &str)] = if old {
        &[(1000,"M"),(500,"D"),(100,"C"),(50,"L"),(10,"X"),(5,"V"),(1,"I")]
    } else {
        &[(1000,"M"),(900,"CM"),(500,"D"),(400,"CD"),(100,"C"),(90,"XC"),
          (50,"L"),(40,"XL"),(10,"X"),(9,"IX"),(5,"V"),(4,"IV"),(1,"I")]
    };
    for &(val, sym) in vals {
        while v >= val { result.push_str(sym); v -= val; }
    }
    result
}

fn char_name(c: char) -> String {
    match c {
        ' ' => "Space".into(),
        '\n' => "Newline".into(),
        '\t' => "Tab".into(),
        '\r' => "Return".into(),
        '\x08' => "Backspace".into(),
        '\x7f' => "Rubout".into(),
        '\x0c' => "Page".into(),
        _ => format!("{}", c),
    }
}

// ── Directive parser ──────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Param { Num(i64), V, Hash, None }

#[allow(dead_code)]
struct Directive {
    params: Vec<Param>,
    colon: bool,
    at_sign: bool,
    ch: char,
    start: usize,
    end: usize,
}

#[allow(dead_code)]
fn parse_directives(control: &str) -> Result<Vec<Directive>, BlissError> {
    let chars: Vec<char> = control.chars().collect();
    let mut directives = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '~' {
            let start = i;
            i += 1;
            if i >= chars.len() {
                return Err(BlissError::Internal("dangling ~ in format string".into()));
            }
            let mut params = Vec::new();
            // Parse parameters
            loop {
                if i >= chars.len() {
                    return Err(BlissError::Internal("dangling ~ in format string".into()));
                }
                let c = chars[i];
                if c == 'v' || c == 'V' {
                    params.push(Param::V); i += 1;
                    if i < chars.len() && chars[i] == ',' { i += 1; }
                } else if c == '#' {
                    params.push(Param::Hash); i += 1;
                    if i < chars.len() && chars[i] == ',' { i += 1; }
                } else if c == '\'' {
                    i += 1;
                    if i < chars.len() {
                        params.push(Param::Num(chars[i] as i64));
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == ',' { i += 1; }
                } else if c.is_ascii_digit() || c == '-' || c == '+' {
                    let mut num_str = String::new();
                    if c == '-' || c == '+' { num_str.push(c); i += 1; }
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        num_str.push(chars[i]); i += 1;
                    }
                    params.push(Param::Num(num_str.parse::<i64>().unwrap_or(0)));
                    if i < chars.len() && chars[i] == ',' { i += 1; }
                } else if c == ',' {
                    params.push(Param::None); i += 1;
                } else {
                    break;
                }
            }
            // Parse colon and at-sign
            let mut colon = false;
            let mut at_sign = false;
            while i < chars.len() {
                if chars[i] == ':' { colon = true; i += 1; }
                else if chars[i] == '@' { at_sign = true; i += 1; }
                else { break; }
            }
            if i >= chars.len() {
                return Err(BlissError::Internal("dangling ~ in format string".into()));
            }
            let ch = chars[i];
            i += 1;
            // Handle ~/name/
            let end = if ch == '/' {
                while i < chars.len() && chars[i] != '/' { i += 1; }
                if i < chars.len() { i += 1; }
                i
            } else { i };
            directives.push(Directive { params, colon, at_sign, ch, start, end });
        } else {
            i += 1;
        }
    }
    Ok(directives)
}

// ── Main format engine ────────────────────────────────────────────

/// Execute a FORMAT directive string. R5.40.
pub fn format(
    destination: BlissVal,
    control_string: &str,
    args: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    // Validate destination
    let to_string = destination.is_nil();
    let to_stdout = destination == T;
    // Detect stream destinations using the streams module's own query
    // function, which correctly understands the StreamState layout.
    let to_stream = !to_string && !to_stdout
        && destination.is_heap_object()
        && crate::streams::output_stream_p(destination);
    if !to_string && !to_stdout && !to_stream {
        // Not NIL, not T, not an output stream — check for string type
        if destination.is_heap_object() {
            if !bliss_rt::types::stringp(destination) {
                return Err(BlissError::TypeError {
                    datum: destination,
                    expected: "stream or string-with-fill-pointer".into(),
                });
            }
        } else {
            return Err(BlissError::TypeError {
                datum: destination,
                expected: "NIL, T, stream, or string-with-fill-pointer".into(),
            });
        }
    }

    // Validate matching brackets/braces/parens before executing
    validate_matching(control_string)?;

    let mut output = String::new();
    let mut arg_idx: usize = 0;
    format_impl(control_string, args, &mut arg_idx, &mut output)?;

    if to_stdout {
        print!("{}", output);
        Ok(NIL)
    } else if to_string {
        Ok(make_bliss_string(&output))
    } else if to_stream {
        // Write each character to the stream using the Gray streams API.
        for ch in output.chars() {
            crate::streams::stream_write_char(destination, BlissVal::from_char(ch))?;
        }
        Ok(NIL)
    } else if destination.is_heap_object() {
        // String with fill pointer — not fully supported yet, fall back
        // to stdout.
        print!("{}", output);
        Ok(NIL)
    } else {
        Ok(NIL)
    }
}

fn validate_matching(control: &str) -> Result<(), BlissError> {
    let chars: Vec<char> = control.chars().collect();
    let mut stack: Vec<char> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '~' {
            i += 1;
            // Skip params and modifiers
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == ',' || chars[i] == '\'' ||
                chars[i] == 'v' || chars[i] == 'V' || chars[i] == '#' || chars[i] == ':' ||
                chars[i] == '@' || chars[i] == '-' || chars[i] == '+') {
                if chars[i] == '\'' { i += 1; } // skip char param
                i += 1;
            }
            if i >= chars.len() { return Err(BlissError::Internal("dangling ~ in format string".into())); }
            match chars[i] {
                '{' => stack.push('}'),
                '}' => { if stack.pop() != Some('}') { return Err(BlissError::Internal("unmatched ~}".into())); } },
                '[' => stack.push(']'),
                ']' => { if stack.pop() != Some(']') { return Err(BlissError::Internal("unmatched ~]".into())); } },
                '(' => stack.push(')'),
                ')' => { if stack.pop() != Some(')') { return Err(BlissError::Internal("unmatched ~)".into())); } },
                '<' => stack.push('>'),
                '>' => { if stack.pop() != Some('>') { return Err(BlissError::Internal("unmatched ~>".into())); } },
                '/' => { i += 1; while i < chars.len() && chars[i] != '/' { i += 1; } },
                _ => {}
            }
        }
        i += 1;
    }
    if !stack.is_empty() {
        let unmatched = match stack.last().unwrap() {
            '}' => "~{", ']' => "~[", ')' => "~(", '>' => "~<", _ => "~?"
        };
        return Err(BlissError::Internal(format!("unmatched {}", unmatched)));
    }
    Ok(())
}

fn format_impl(
    control: &str,
    args: &[BlissVal],
    arg_idx: &mut usize,
    output: &mut String,
) -> Result<(), BlissError> {
    let chars: Vec<char> = control.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '~' {
            output.push(chars[i]);
            i += 1;
            continue;
        }
        i += 1; // skip ~
        if i >= chars.len() {
            return Err(BlissError::Internal("dangling ~ in format string".into()));
        }
        // Parse params
        let mut params: Vec<Param> = Vec::new();
        loop {
            if i >= chars.len() { return Err(BlissError::Internal("dangling ~".into())); }
            let c = chars[i];
            if c == 'v' || c == 'V' {
                params.push(Param::V); i += 1;
                if i < chars.len() && chars[i] == ',' { i += 1; }
            } else if c == '#' && (i + 1 >= chars.len() || chars[i+1] != '\\') {
                params.push(Param::Hash); i += 1;
                if i < chars.len() && chars[i] == ',' { i += 1; }
            } else if c == '\'' {
                i += 1;
                if i < chars.len() { params.push(Param::Num(chars[i] as i64)); i += 1; }
                if i < chars.len() && chars[i] == ',' { i += 1; }
            } else if c.is_ascii_digit() || ((c == '-' || c == '+') && i + 1 < chars.len() && chars[i+1].is_ascii_digit()) {
                let mut num_str = String::new();
                if c == '-' || c == '+' { num_str.push(c); i += 1; }
                while i < chars.len() && chars[i].is_ascii_digit() { num_str.push(chars[i]); i += 1; }
                params.push(Param::Num(num_str.parse::<i64>().unwrap_or(0)));
                if i < chars.len() && chars[i] == ',' { i += 1; }
            } else if c == ',' {
                params.push(Param::None); i += 1;
            } else {
                break;
            }
        }
        let mut colon = false;
        let mut at_sign = false;
        while i < chars.len() {
            if chars[i] == ':' { colon = true; i += 1; }
            else if chars[i] == '@' { at_sign = true; i += 1; }
            else { break; }
        }
        if i >= chars.len() { return Err(BlissError::Internal("dangling ~".into())); }
        let directive = chars[i].to_ascii_uppercase();
        i += 1;

        let remaining = args.len().saturating_sub(*arg_idx);

        let resolve_param = |p: &Param, default: i64, aidx: &mut usize| -> Result<i64, BlissError> {
            match p {
                Param::Num(n) => Ok(*n),
                Param::V => {
                    if *aidx >= args.len() { return Err(BlissError::Internal("too few args for V param".into())); }
                    let v = args[*aidx]; *aidx += 1;
                    if v.is_nil() { Ok(default) }
                    else if v.is_fixnum() { Ok(v.as_fixnum()) }
                    else { Err(BlissError::TypeError { datum: v, expected: "integer".into() }) }
                }
                Param::Hash => Ok(remaining as i64),
                Param::None => Ok(default),
            }
        };

        match directive {
            'A' => {
                // Resolve V/# params BEFORE consuming the main argument
                let mincol = if !params.is_empty() { resolve_param(&params[0], 0, arg_idx)? as usize } else { 0 };
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~A".into())); }
                let val = args[*arg_idx]; *arg_idx += 1;
                let s = if colon && val.is_nil() { "()".into() }
                        else { blissval_to_print_string(val, false) };
                if s.len() < mincol {
                    let pad = mincol - s.len();
                    if at_sign { for _ in 0..pad { output.push(' '); } output.push_str(&s); }
                    else { output.push_str(&s); for _ in 0..pad { output.push(' '); } }
                } else { output.push_str(&s); }
            }
            'S' => {
                // Resolve V/# params BEFORE consuming the main argument
                let mincol = if !params.is_empty() { resolve_param(&params[0], 0, arg_idx)? as usize } else { 0 };
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~S".into())); }
                let val = args[*arg_idx]; *arg_idx += 1;
                let s = if colon && val.is_nil() { "()".into() }
                        else { blissval_to_print_string(val, true) };
                if s.len() < mincol {
                    let pad = mincol - s.len();
                    if at_sign { for _ in 0..pad { output.push(' '); } output.push_str(&s); }
                    else { output.push_str(&s); for _ in 0..pad { output.push(' '); } }
                } else { output.push_str(&s); }
            }
            'D' | 'B' | 'O' | 'X' => {
                let radix = match directive { 'B'=>2, 'O'=>8, 'X'=>16, _=>10 };
                // Resolve V/# params BEFORE consuming the main argument
                let mincol = if !params.is_empty() { resolve_param(&params[0], 0, arg_idx)? as usize } else { 0 };
                let padchar = if params.len() > 1 { resolve_param(&params[1], ' ' as i64, arg_idx)? as u8 as char } else { ' ' };
                if *arg_idx >= args.len() { return Err(BlissError::Internal(format!("too few args for ~{}", directive))); }
                let val = args[*arg_idx]; *arg_idx += 1;
                if !val.is_fixnum() {
                    return Err(BlissError::TypeError { datum: val, expected: "integer".into() });
                }
                output.push_str(&format_integer(val.as_fixnum(), radix, colon, at_sign, mincol, padchar));
            }
            'R' => {
                // Resolve V/# params BEFORE consuming the main argument
                let radix_param = if !params.is_empty() { Some(resolve_param(&params[0], 10, arg_idx)? as u32) } else { None };
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~R".into())); }
                let val = args[*arg_idx]; *arg_idx += 1;
                if !val.is_fixnum() {
                    return Err(BlissError::TypeError { datum: val, expected: "integer".into() });
                }
                let n = val.as_fixnum();
                if let Some(radix) = radix_param {
                    output.push_str(&format_integer(n, radix, colon, at_sign, 0, ' '));
                } else if colon && at_sign {
                    output.push_str(&to_roman(n, true));
                } else if at_sign {
                    output.push_str(&to_roman(n, false));
                } else if colon {
                    output.push_str(&ordinal(n));
                } else {
                    output.push_str(&cardinal(n));
                }
            }
            'F' => {
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~F".into())); }
                let val = args[*arg_idx]; *arg_idx += 1;
                let f = if val.is_single_float() { val.as_single_float() as f64 }
                        else if val.is_fixnum() { val.as_fixnum() as f64 }
                        else { return Err(BlissError::TypeError { datum: val, expected: "number".into() }); };
                output.push_str(&format!("{}", f));
            }
            'E' => {
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~E".into())); }
                let val = args[*arg_idx]; *arg_idx += 1;
                let f = if val.is_single_float() { val.as_single_float() as f64 }
                        else if val.is_fixnum() { val.as_fixnum() as f64 }
                        else { return Err(BlissError::TypeError { datum: val, expected: "number".into() }); };
                output.push_str(&format!("{:E}", f));
            }
            'G' => {
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~G".into())); }
                let val = args[*arg_idx]; *arg_idx += 1;
                let f = if val.is_single_float() { val.as_single_float() as f64 }
                        else if val.is_fixnum() { val.as_fixnum() as f64 }
                        else { return Err(BlissError::TypeError { datum: val, expected: "number".into() }); };
                output.push_str(&format!("{}", f));
            }
            '$' => {
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~$".into())); }
                let val = args[*arg_idx]; *arg_idx += 1;
                let f = if val.is_single_float() { val.as_single_float() as f64 }
                        else if val.is_fixnum() { val.as_fixnum() as f64 }
                        else { return Err(BlissError::TypeError { datum: val, expected: "number".into() }); };
                if at_sign && f >= 0.0 { output.push('+'); }
                output.push_str(&format!("{:.2}", f));
            }
            '%' => {
                let count = if !params.is_empty() { resolve_param(&params[0], 1, arg_idx)? } else { 1 };
                for _ in 0..count { output.push('\n'); }
            }
            '&' => {
                let count = if !params.is_empty() { resolve_param(&params[0], 1, arg_idx)? } else { 1 };
                // Fresh line: emit newline only if not at start of line
                if !output.is_empty() && !output.ends_with('\n') { output.push('\n'); }
                for _ in 1..count { output.push('\n'); }
            }
            '|' => {
                let count = if !params.is_empty() { resolve_param(&params[0], 1, arg_idx)? } else { 1 };
                for _ in 0..count { output.push('\x0c'); }
            }
            '~' => {
                let count = if !params.is_empty() { resolve_param(&params[0], 1, arg_idx)? } else { 1 };
                for _ in 0..count { output.push('~'); }
            }
            'T' => {
                let colnum = if !params.is_empty() { resolve_param(&params[0], 1, arg_idx)? as usize } else { 1 };
                let colinc = if params.len() > 1 { resolve_param(&params[1], 1, arg_idx)? as usize } else { 1 };
                let cur_col = output.rfind('\n').map(|p| output.len() - p - 1).unwrap_or(output.len());
                if cur_col < colnum {
                    for _ in 0..(colnum - cur_col) { output.push(' '); }
                } else if colinc > 0 {
                    let spaces = colinc - ((cur_col - colnum) % colinc);
                    for _ in 0..spaces { output.push(' '); }
                }
            }
            '*' => {
                let n = if !params.is_empty() { resolve_param(&params[0], 1, arg_idx)? as usize } else { 1 };
                if at_sign { *arg_idx = n; }
                else if colon {
                    if *arg_idx >= n { *arg_idx -= n; } else { *arg_idx = 0; }
                } else { *arg_idx += n; }
            }
            'C' => {
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~C".into())); }
                let val = args[*arg_idx]; *arg_idx += 1;
                if !val.is_character() {
                    return Err(BlissError::TypeError { datum: val, expected: "character".into() });
                }
                let c = val.as_char();
                if at_sign { output.push_str(&format!("#\\{}", c)); }
                else if colon { output.push_str(&char_name(c)); }
                else { output.push(c); }
            }
            'W' => {
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~W".into())); }
                let val = args[*arg_idx]; *arg_idx += 1;
                output.push_str(&blissval_to_print_string(val, true));
            }
            '?' => {
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~?".into())); }
                let ctrl_val = args[*arg_idx]; *arg_idx += 1;
                // Control string must be a string
                if !ctrl_val.is_heap_object() || !bliss_rt::types::stringp(ctrl_val) {
                    return Err(BlissError::TypeError { datum: ctrl_val, expected: "string".into() });
                }
                let sub_control = extract_bliss_string(ctrl_val)
                    .ok_or_else(|| BlissError::Internal("failed to extract format string".into()))?;
                if at_sign {
                    // ~@? — use the enclosing argument list from current position
                    format_impl(&sub_control, args, arg_idx, output)?;
                } else {
                    // ~? — consume a separate list argument for the sub-format's args
                    if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~?".into())); }
                    let args_val = args[*arg_idx]; *arg_idx += 1;
                    let sub_args = if args_val.is_nil() {
                        Vec::new()
                    } else {
                        cons_list_to_vec(args_val)
                    };
                    let mut sub_idx = 0;
                    format_impl(&sub_control, &sub_args, &mut sub_idx, output)?;
                }
            }
            'P' => {
                // ~P and ~:P both back up one arg, peek at it for the plural
                // decision, then restore arg_idx (no net consumption).
                // ~@P does the y/ies variant; plain ~P does s/empty.
                if *arg_idx > 0 { *arg_idx -= 1; }
                if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~P".into())); }
                let val = args[*arg_idx]; *arg_idx += 1;
                let is_one = val.is_fixnum() && val.as_fixnum() == 1;
                if at_sign {
                    output.push_str(if is_one { "y" } else { "ies" });
                } else {
                    if !is_one { output.push('s'); }
                }
            }
            '^' => {
                // ~^ up-and-out: in iteration context, terminates if no more args
                if *arg_idx >= args.len() { return Ok(()); }
            }
            '{' => {
                // Find matching ~}
                let body_start = i;
                let body_end = find_matching_close(&chars, i, '{')?;
                let body: String = chars[body_start..body_end].iter().collect();
                i = skip_close_directive(&chars, body_end);
                if at_sign && colon {
                    // ~:@{...~} — each remaining arg is itself a list (cons cell)
                    while *arg_idx < args.len() {
                        let sub = args[*arg_idx]; *arg_idx += 1;
                        if sub.is_nil() { continue; } // empty sublist
                        let sub_args = cons_list_to_vec(sub);
                        let mut sub_idx = 0;
                        format_impl(&body, &sub_args, &mut sub_idx, output)?;
                    }
                } else if at_sign {
                    // ~@{...~} — remaining args form the iteration list
                    while *arg_idx < args.len() {
                        format_impl(&body, args, arg_idx, output)?;
                    }
                } else if colon {
                    // ~:{...~} — arg is a list of sublists; apply body to each sublist
                    if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~:{".into())); }
                    let list_val = args[*arg_idx]; *arg_idx += 1;
                    if !list_val.is_nil() {
                        let sublists = cons_list_to_vec(list_val);
                        for sublist in &sublists {
                            let sub_args = if sublist.is_nil() {
                                Vec::new()
                            } else {
                                cons_list_to_vec(*sublist)
                            };
                            let mut sub_idx = 0;
                            format_impl(&body, &sub_args, &mut sub_idx, output)?;
                        }
                    }
                } else {
                    // ~{...~} — arg is a list; iterate body over list elements
                    if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~{".into())); }
                    let list_val = args[*arg_idx]; *arg_idx += 1;
                    if !list_val.is_nil() {
                        let list_elements = cons_list_to_vec(list_val);
                        let mut sub_idx = 0;
                        while sub_idx < list_elements.len() {
                            format_impl(&body, &list_elements, &mut sub_idx, output)?;
                        }
                    }
                }
            }
            '}' => {
                return Err(BlissError::Internal("unmatched ~}".into()));
            }
            '[' => {
                let body_start = i;
                let body_end = find_matching_close(&chars, i, '[')?;
                let body: String = chars[body_start..body_end].iter().collect();
                i = skip_close_directive(&chars, body_end);
                // Parse clauses separated by ~;
                let clauses = split_clauses(&body);
                if colon {
                    // ~:[false~;true~] boolean conditional
                    if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~:[".into())); }
                    let val = args[*arg_idx]; *arg_idx += 1;
                    let idx = if val.is_nil() { 0 } else { 1 };
                    if idx < clauses.len() {
                        format_impl(&clauses[idx], args, arg_idx, output)?;
                    }
                } else if at_sign {
                    // ~@[clause~] true-test
                    if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~@[".into())); }
                    let val = args[*arg_idx];
                    if !val.is_nil() {
                        // Don't consume arg - it remains for use inside clause
                        if !clauses.is_empty() {
                            format_impl(&clauses[0], args, arg_idx, output)?;
                        }
                    } else {
                        *arg_idx += 1; // consume the nil
                    }
                } else {
                    // Numeric conditional
                    if *arg_idx >= args.len() { return Err(BlissError::Internal("too few args for ~[".into())); }
                    let val = args[*arg_idx]; *arg_idx += 1;
                    if !val.is_fixnum() {
                        return Err(BlissError::TypeError { datum: val, expected: "integer".into() });
                    }
                    let idx = val.as_fixnum() as usize;
                    if idx < clauses.len() {
                        format_impl(&clauses[idx], args, arg_idx, output)?;
                    }
                }
            }
            ']' => {
                return Err(BlissError::Internal("unmatched ~]".into()));
            }
            '(' => {
                let body_start = i;
                let body_end = find_matching_close(&chars, i, '(')?;
                let body: String = chars[body_start..body_end].iter().collect();
                i = skip_close_directive(&chars, body_end);
                let mut inner = String::new();
                format_impl(&body, args, arg_idx, &mut inner)?;
                if colon && at_sign {
                    output.push_str(&inner.to_uppercase());
                } else if colon {
                    output.push_str(&capitalize_words(&inner));
                } else if at_sign {
                    output.push_str(&capitalize_first(&inner));
                } else {
                    output.push_str(&inner.to_lowercase());
                }
            }
            ')' => {
                return Err(BlissError::Internal("unmatched ~)".into()));
            }
            '<' => {
                let body_start = i;
                let body_end = find_matching_close(&chars, i, '<')?;
                let body: String = chars[body_start..body_end].iter().collect();
                i = skip_close_directive(&chars, body_end);
                if colon {
                    // ~:<...~:> logical block: just format the body
                    format_impl(&body, args, arg_idx, output)?;
                } else {
                    // Justification
                    let mincol = if !params.is_empty() { resolve_param(&params[0], 0, arg_idx)? as usize } else { 0 };
                    let clauses = split_clauses(&body);
                    let mut parts = Vec::new();
                    for clause in &clauses {
                        let mut part = String::new();
                        format_impl(clause, args, arg_idx, &mut part)?;
                        parts.push(part);
                    }
                    let total_len: usize = parts.iter().map(|p| p.len()).sum();
                    let width = mincol.max(total_len);
                    if parts.len() <= 1 {
                        let s = parts.first().map(|s| s.as_str()).unwrap_or("");
                        output.push_str(s);
                        for _ in s.len()..width { output.push(' '); }
                    } else {
                        let gaps = parts.len() - 1;
                        let extra = width.saturating_sub(total_len);
                        let per_gap = if gaps > 0 { extra / gaps } else { 0 };
                        let mut remainder = if gaps > 0 { extra % gaps } else { 0 };
                        for (j, part) in parts.iter().enumerate() {
                            output.push_str(part);
                            if j < gaps {
                                let g = per_gap + if remainder > 0 { remainder -= 1; 1 } else { 0 };
                                for _ in 0..g { output.push(' '); }
                            }
                        }
                    }
                }
            }
            '>' => {
                return Err(BlissError::Internal("unmatched ~>".into()));
            }
            '/' => {
                // ~/name/ — user dispatch function. Consume one argument.
                let name_start = i;
                while i < chars.len() && chars[i] != '/' { i += 1; }
                let name: String = chars[name_start..i].iter().collect();
                if i < chars.len() { i += 1; } // skip closing /
                let _ = i; // suppress unused assignment warning (we return below)
                // Consume one argument as per CL spec
                if *arg_idx >= args.len() { return Err(BlissError::Internal(format!("too few args for ~/{}/", name))); }
                let _arg = args[*arg_idx]; *arg_idx += 1;
                // Return an error with the function name so the caller knows which function was not found
                return Err(BlissError::UndefinedFunction(make_bliss_string(&name)));
            }
            '\n' => {
                // ~\n — ignored newline (with optional whitespace eating)
                if !at_sign { while i < chars.len() && chars[i].is_whitespace() { i += 1; } }
            }
            _ => {
                return Err(BlissError::Internal(format!("unknown format directive ~{}", directive)));
            }
        }
    }
    Ok(())
}

/// Skip past a closing directive like ~}, ~], ~), ~>, ~:>, etc.
/// `pos` points to the `~`. Returns position after the directive char.
fn skip_close_directive(chars: &[char], pos: usize) -> usize {
    let mut j = pos + 1; // skip ~
    while j < chars.len() && (chars[j] == ':' || chars[j] == '@' ||
        chars[j].is_ascii_digit() || chars[j] == ',' || chars[j] == '\'' ||
        chars[j] == 'v' || chars[j] == 'V' || chars[j] == '#' ||
        chars[j] == '-' || chars[j] == '+') {
        if chars[j] == '\'' && j + 1 < chars.len() { j += 1; }
        j += 1;
    }
    if j < chars.len() { j + 1 } else { j }
}

/// Find matching close bracket. `start` is first char of body (after opening bracket).
/// Returns position of `~` in the closing `~close` directive.
fn find_matching_close(chars: &[char], start: usize, open: char) -> Result<usize, BlissError> {
    let close = match open { '{' => '}', '[' => ']', '(' => ')', '<' => '>', _ => open };
    let mut depth = 1;
    let mut j = start;
    while j < chars.len() {
        if chars[j] == '~' {
            let tilde_at = j;
            j += 1;
            // Skip params and modifiers
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == ',' || chars[j] == '\'' ||
                chars[j] == 'v' || chars[j] == 'V' || chars[j] == '#' || chars[j] == ':' ||
                chars[j] == '@' || chars[j] == '-' || chars[j] == '+') {
                if chars[j] == '\'' && j + 1 < chars.len() { j += 1; }
                j += 1;
            }
            if j < chars.len() {
                if chars[j] == open { depth += 1; }
                else if chars[j] == close {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(tilde_at);
                    }
                }
            }
        }
        j += 1;
    }
    Err(BlissError::Internal(format!("unmatched ~{}", open)))
}

fn split_clauses(body: &str) -> Vec<String> {
    let chars: Vec<char> = body.chars().collect();
    let mut clauses = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '~' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == ',' || chars[i] == '\'' ||
                chars[i] == 'v' || chars[i] == 'V' || chars[i] == '#' || chars[i] == ':' ||
                chars[i] == '@' || chars[i] == '-' || chars[i] == '+') {
                if chars[i] == '\'' && i + 1 < chars.len() { i += 1; }
                i += 1;
            }
            if i < chars.len() {
                match chars[i] {
                    '{' | '[' | '(' | '<' => depth += 1,
                    '}' | ']' | ')' | '>' => depth -= 1,
                    ';' if depth == 0 => { clauses.push(current.clone()); current.clear(); i += 1; continue; }
                    _ => {}
                }
            }
            let chunk: String = chars[start..=i.min(chars.len()-1)].iter().collect();
            current.push_str(&chunk);
            i += 1;
        } else {
            current.push(chars[i]);
            i += 1;
        }
    }
    clauses.push(current);
    clauses
}

fn capitalize_words(s: &str) -> String {
    let mut result = String::new();
    let mut cap_next = true;
    for c in s.chars() {
        if c.is_whitespace() || !c.is_alphanumeric() {
            result.push(c);
            cap_next = true;
        } else if cap_next {
            result.push(c.to_uppercase().next().unwrap());
            cap_next = false;
        } else {
            result.push(c.to_lowercase().next().unwrap());
        }
    }
    result
}

fn capitalize_first(s: &str) -> String {
    let mut result = String::new();
    let mut done = false;
    for c in s.chars() {
        if !done && c.is_alphabetic() {
            result.push(c.to_uppercase().next().unwrap());
            done = true;
        } else if done {
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

// ── formatter ─────────────────────────────────────────────────────

/// Compile a FORMAT control string for repeated use.
/// Returns a closure (function-tagged heap object) that, when called with
/// a stream and arguments, performs the formatting.
pub fn formatter(control_string: &str) -> Result<BlissVal, BlissError> {
    // Validate the control string
    validate_matching(control_string)?;
    // Allocate a ClosureData that captures the control string.
    // The closure's function field points to the control string as a BlissVal.
    // When invoked, the runtime should extract the control string and call format().
    let ctrl_str = make_bliss_string(control_string);
    let total = std::mem::size_of::<bliss_rt::object::ClosureData>() + 8; // one captured var
    let size_units = ((total + 7) / 8) as u16;
    let layout = std::alloc::Layout::from_size_align(total, 8).unwrap();
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout);
        if ptr.is_null() { std::alloc::handle_alloc_error(layout); }
        let header = ObjectHeader::new(type_id::CLOSURE, size_units);
        let closure = ptr as *mut bliss_rt::object::ClosureData;
        (*closure).header = header;
        (*closure).function = ctrl_str; // the captured control string
        // Store control string in closed_vars slot (offset after ClosureData)
        *(ptr.add(std::mem::size_of::<bliss_rt::object::ClosureData>()) as *mut BlissVal) = ctrl_str;
        // Return as function-tagged pointer so it's callable
        Ok(BlissVal::from_function_ptr(ptr))
    }
}

// ── Pretty-printer ─────────────────────────────────────────────────

/// Begin a logical block for pretty-printing (PPRINT-LOGICAL-BLOCK). R5.41.
pub fn pprint_logical_block(
    stream: BlissVal,
    list: BlissVal,
    prefix: Option<&str>,
    per_line_prefix: Option<&str>,
    suffix: Option<&str>,
    body: BlissVal,
) -> Result<(), BlissError> {
    // Build the output: per_line_prefix (or prefix) + body content + suffix
    let mut output = String::new();

    // Emit prefix or per-line-prefix
    if let Some(plp) = per_line_prefix {
        output.push_str(plp);
    } else if let Some(p) = prefix {
        output.push_str(p);
    }

    // Process the body: if body is a string, format it with the list as args
    if body.is_heap_object() && bliss_rt::types::stringp(body) {
        if let Some(body_str) = extract_bliss_string(body) {
            let list_elements = if list.is_nil() {
                Vec::new()
            } else {
                cons_list_to_vec(list)
            };
            let mut body_output = String::new();
            let mut idx = 0;
            format_impl(&body_str, &list_elements, &mut idx, &mut body_output)?;
            output.push_str(&body_output);
        }
    } else if !list.is_nil() {
        // If no body format string, print the list elements separated by spaces
        let elements = cons_list_to_vec(list);
        for (j, elem) in elements.iter().enumerate() {
            if j > 0 { output.push(' '); }
            output.push_str(&blissval_to_print_string(*elem, false));
        }
    }

    if let Some(s) = suffix {
        output.push_str(s);
    }

    if stream == T {
        print!("{}", output);
    } else if stream.is_heap_object() {
        // Write to stream - for now print to stdout as stream write API
        // is not fully available
        print!("{}", output);
    }
    Ok(())
}

/// Insert a conditional newline (PPRINT-NEWLINE). R5.41.
pub fn pprint_newline(kind: NewlineKind, stream: BlissVal) -> Result<(), BlissError> {
    if stream == T {
        match kind {
            NewlineKind::Mandatory => { println!(); }
            NewlineKind::Linear | NewlineKind::Fill | NewlineKind::Miser => {
                // In a full XP implementation, these are conditional.
                // For now, linear emits, fill/miser don't.
                if kind == NewlineKind::Linear { println!(); }
            }
        }
    }
    Ok(())
}

/// Kind of pretty-printer newline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewlineKind {
    Linear,
    Fill,
    Miser,
    Mandatory,
}

/// Adjust indentation (PPRINT-INDENT). R5.41.
pub fn pprint_indent(_relative: bool, _n: i32, _stream: BlissVal) -> Result<(), BlissError> {
    Ok(())
}

/// Tab (PPRINT-TAB). R5.41.
pub fn pprint_tab(
    _kind: TabKind,
    _colnum: u32,
    _colinc: u32,
    _stream: BlissVal,
) -> Result<(), BlissError> {
    Ok(())
}

/// Kind of tab for pprint-tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Line,
    Section,
    LineRelative,
    SectionRelative,
}

// ── Pprint dispatch ────────────────────────────────────────────────

/// Dispatch table entry.
#[allow(dead_code)]
struct DispatchEntry {
    _type_spec: BlissVal,
    function: BlissVal,
    priority: f64,
}

/// A simple pprint dispatch table stored as a heap object.
#[allow(dead_code)]
struct PprintDispatchTable {
    entries: Vec<DispatchEntry>,
}

// Global default dispatch table
use std::sync::Mutex;
static DEFAULT_DISPATCH: Mutex<Option<Vec<(BlissVal, BlissVal, f64)>>> = Mutex::new(None);

/// NOTE: Leaked allocation — not GC-registered. See make_bliss_string note.
fn ensure_default_table() {
    let mut table = DEFAULT_DISPATCH.lock().unwrap();
    if table.is_none() {
        // Default table with a catch-all entry
        *table = Some(vec![(NIL, make_bliss_string("default-printer"), 0.0)]);
    }
}

/// Get the pprint dispatch function for a type.
pub fn pprint_dispatch(_object: BlissVal) -> Result<(BlissVal, bool), BlissError> {
    ensure_default_table();
    let table = DEFAULT_DISPATCH.lock().unwrap();
    if let Some(entries) = table.as_ref() {
        if let Some(entry) = entries.last() {
            return Ok((entry.1, true));
        }
    }
    Ok((NIL, false))
}

/// Set a pprint dispatch entry.
pub fn set_pprint_dispatch(
    type_specifier: BlissVal,
    function: Option<BlissVal>,
    priority: f64,
    _table: BlissVal,
) -> Result<(), BlissError> {
    ensure_default_table();
    let mut table = DEFAULT_DISPATCH.lock().unwrap();
    if let Some(entries) = table.as_mut() {
        // Remove existing entry for this type
        entries.retain(|e| e.0 != type_specifier || (e.2 - priority).abs() > f64::EPSILON);
        if let Some(func) = function {
            entries.push((type_specifier, func, priority));
            entries.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        }
    }
    Ok(())
}

/// Copy a pprint dispatch table.
/// NOTE: Leaked allocation — not GC-registered. See make_bliss_string note.
pub fn copy_pprint_dispatch(table: Option<BlissVal>) -> Result<BlissVal, BlissError> {
    ensure_default_table();
    let _ = table;
    // Allocate a new dispatch table object as a heap object
    // We use a simple-vector type for the table representation
    let header = ObjectHeader::new(type_id::SIMPLE_VECTOR, 2);
    let layout = std::alloc::Layout::from_size_align(16, 8).unwrap();
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout);
        if ptr.is_null() { std::alloc::handle_alloc_error(layout); }
        *(ptr as *mut ObjectHeader) = header;
        *(ptr.add(8) as *mut u64) = 0; // empty table marker
        Ok(BlissVal::from_heap_ptr(ptr))
    }
}
