// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Builtin operations on evaluated arguments. Source-form evaluation belongs
//! to the caller; keeping it outside this dispatch lets delivery select native
//! implementations without retaining the tree-walking operator dispatcher.
use super::*;

type Handler = fn(&str, &[EgclVal], &mut Env) -> Result<EgclVal, EgclError>;

pub(super) fn call(
    name: &str,
    args: &[EgclVal],
    env: &mut Env,
) -> Option<Result<EgclVal, EgclError>> {
    // Match the source dispatch's normalization of extension registry keys.
    let normalized;
    let name = if let Some(rest) = name.strip_prefix("EGCL-EXT::") {
        normalized = format!("EGCL-EXT:{rest}");
        &normalized
    } else if let Some(rest) = name.strip_prefix("EGCL-INTERNAL::") {
        normalized = format!("EGCL-INTERNAL:{rest}");
        &normalized
    } else {
        name
    };
    let handler = resolve(name)?;
    // A handler can call Lisp (MAPHASH, SOME, Gray stream methods). Only an
    // actual multiple-value producer may expose those values to its caller.
    // The source evaluator used to enforce this after the synthesized call.
    let preserve = mv_operator_preserves(name)
        || mv_operator_preserves(name.rsplit(':').next().unwrap_or(name));
    env.clear_mv();
    let result = handler(name, args, env);
    if result.is_ok() && !preserve {
        env.clear_mv();
    }
    Some(result)
}

#[egcl_delivery_macros::builtin_dispatch(name)]
fn resolve(name: &str) -> Option<Handler> {
    match name {
        "PRINT" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (print object &optional stream): per CLHS, PRINT is PRIN1
            // preceded by a newline and followed by a SPACE (not a newline)
            // — "the printed representation of object is preceded by a
            // newline and followed by a space" (bliss-g5dg).

            if args.is_empty() {
                return Err(EgclError::ProgramError("PRINT requires an object".into()));
            }
            // Render first (dispatching print-object allocates), then resolve
            // the stream from the rooted args so a GC in between cannot stale
            // the object or the stream (bliss-6b2 #2).
            let rendered = format_val_env(args[0], env, true);
            let stream = if args.len() > 1 { args[1] } else { NIL };
            egcl_rt::rooted!(out = resolve_output_stream(stream, env));
            write_str_to(*out, "\n", env)?;
            write_str_to(*out, &rendered, env)?;
            write_str_to(*out, " ", env)?;
            Ok(args[0])
        }),

        "PRINC" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (princ object &optional stream)

            if args.is_empty() {
                return Err(EgclError::ProgramError("PRINC requires an object".into()));
            }
            let mut s = String::new();
            princ_val_env(args[0], env, &mut s);
            let stream = if args.len() > 1 { args[1] } else { NIL };
            let out = resolve_output_stream(stream, env);
            write_str_to(out, &s, env)?;
            Ok(args[0])
        }),

        "PRIN1" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (prin1 object &optional stream): the escaped (readable)
            // representation, no surrounding newlines. Returns the object.

            if args.is_empty() {
                return Err(EgclError::ProgramError("PRIN1 requires an object".into()));
            }
            let rendered = format_val_env(args[0], env, true);
            let stream = if args.len() > 1 { args[1] } else { NIL };
            let out = resolve_output_stream(stream, env);
            write_str_to(out, &rendered, env)?;
            Ok(args[0])
        }),

        "WRITE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (write object &key stream escape ...): render OBJECT honouring
            // :escape (default T → prin1-style; NIL → princ-style) to :stream
            // (default *standard-output*). Other keywords are accepted and
            // ignored. Returns the object.

            if args.is_empty() {
                return Err(EgclError::ProgramError("WRITE requires an object".into()));
            }
            let mut stream_idx: Option<usize> = None;
            let mut escape = true;
            let mut i = 1;
            while i + 1 < args.len() {
                let key = sym_bare_name_rc(args[i]);
                let val = args[i + 1];
                match key.as_ref() {
                    "STREAM" => stream_idx = Some(i + 1),
                    "ESCAPE" => escape = !val.is_nil(),
                    _ => {}
                }
                i += 2;
            }
            // :escape NIL is princ semantics (unquoted strings/chars);
            // otherwise prin1 (readable) semantics. Render before resolving the
            // stream from the rooted args so an allocating render cannot stale
            // it (bliss-6b2 #2).
            let rendered = if escape {
                format_val_env(args[0], env, true)
            } else {
                let mut s = String::new();
                princ_val_env(args[0], env, &mut s);
                s
            };
            let stream = stream_idx.map(|k| args[k]).unwrap_or(NIL);
            let out = resolve_output_stream(stream, env);
            write_str_to(out, &rendered, env)?;
            Ok(args[0])
        }),

        "WRITE-CHAR" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (write-char character &optional stream)

            if args.is_empty() {
                return Err(EgclError::Internal(
                    "WRITE-CHAR requires a character".into(),
                ));
            }
            let ch = args[0];
            let stream = if args.len() > 1 { args[1] } else { NIL };
            let out = resolve_output_stream(stream, env);
            check_pending_sigpipe_for_output()?;
            if is_gray_stream(out) {
                invoke_generic_function("STREAM-WRITE-CHAR", &[out, ch], env)?;
            } else {
                egcl_stdlib::stream_write_char(out, ch)?;
            }
            Ok(ch)
        }),

        "MAKE-STRING" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (make-string size &key initial-element element-type) → a FRESH
            // (non-interned) mutable string, so (setf (char s i) c) / REPLACE
            // can mutate it without aliasing a shared literal.
            //
            // Argument validation is a catchable PROGRAM-ERROR (ansi
            // MAKE-STRING.ERROR.1-6): no size, an odd number of keyword
            // arguments, a non-symbol in keyword position, or an unknown
            // keyword when :allow-other-keys is not (first-occurrence) true.
            // For a repeated keyword the LEFTMOST value wins (KEYWORDS.7).

            if args.is_empty() {
                return Err(EgclError::ProgramError(
                    "MAKE-STRING requires a size argument".into(),
                ));
            }
            let size = args[0];
            if !size.is_fixnum() || size.as_fixnum() < 0 {
                return Err(EgclError::TypeError {
                    datum: size,
                    expected: "non-negative fixnum size".into(),
                });
            }
            let kv = &args[1..];
            if kv.len() % 2 != 0 {
                return Err(EgclError::ProgramError(
                    "MAKE-STRING: keyword arguments must appear in key/value pairs".into(),
                ));
            }
            // Determine :allow-other-keys from its FIRST occurrence (ANSI).
            let mut allow_other = false;
            let mut j = 0;
            while j < kv.len() {
                if kv[j].is_symbol() && sym_bare_name_rc(kv[j]).as_ref() == "ALLOW-OTHER-KEYS" {
                    allow_other = !kv[j + 1].is_nil();
                    break;
                }
                j += 2;
            }
            let mut fill = ' ';
            let mut fill_set = false;
            let mut i = 0;
            while i < kv.len() {
                let key = kv[i];
                let val = kv[i + 1];
                if !key.is_symbol() {
                    let mut kbuf = String::new();
                    print_val(key, &mut kbuf);
                    return Err(EgclError::ProgramError(format!(
                        "MAKE-STRING: keyword argument name is not a symbol: {kbuf}"
                    )));
                }
                match sym_bare_name_rc(key).as_ref() {
                    "INITIAL-ELEMENT" => {
                        if !fill_set {
                            if !val.is_character() {
                                return Err(EgclError::TypeError {
                                    datum: val,
                                    expected: "character".into(),
                                });
                            }
                            fill = val.as_char();
                            fill_set = true;
                        }
                    }
                    "ELEMENT-TYPE" | "ALLOW-OTHER-KEYS" => {}
                    other => {
                        if !allow_other {
                            return Err(EgclError::ProgramError(format!(
                                "MAKE-STRING: unknown keyword argument :{other}"
                            )));
                        }
                    }
                }
                i += 2;
            }
            let content: String = std::iter::repeat_n(fill, size.as_fixnum() as usize).collect();
            Ok(egcl_stdlib::make_lisp_string_fresh(&content))
        }),

        "MAKE-BROADCAST-STREAM" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (make-broadcast-stream &rest streams) → output stream that fans
            // each write out to all component streams. eval_args keeps the
            // component streams rooted across the allocating constructor.

            egcl_stdlib::make_broadcast_stream(&args)
        }),

        "MAKE-CONCATENATED-STREAM" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (make-concatenated-stream &rest streams) → input stream that
            // reads successively from each component.

            egcl_stdlib::make_concatenated_stream(&args)
        }),

        "MAKE-TWO-WAY-STREAM" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (make-two-way-stream input output)

            let input = args.first().copied().unwrap_or(NIL);
            let output = args.get(1).copied().unwrap_or(NIL);
            egcl_stdlib::make_two_way_stream(input, output)
        }),

        "MAKE-ECHO-STREAM" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (make-echo-stream input output) → reads echo to output.

            let input = args.first().copied().unwrap_or(NIL);
            let output = args.get(1).copied().unwrap_or(NIL);
            egcl_stdlib::make_echo_stream(input, output)
        }),

        "MAKE-SYNONYM-STREAM" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (make-synonym-stream symbol) → stream that forwards to the stream
            // that is the current value of SYMBOL.

            let symbol = args.first().copied().unwrap_or(NIL);
            egcl_stdlib::make_synonym_stream(symbol)
        }),

        "STREAMP" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (streamp object) → T if object is a stream.

            let obj = args.first().copied().unwrap_or(NIL);
            Ok(if is_stream(obj) { T } else { NIL })
        }),

        "INPUT-STREAM-P" | "EGCL::%NATIVE-INPUT-STREAM-P" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (input-stream-p stream) → T if the stream can be read from.

            let obj = args.first().copied().unwrap_or(NIL);
            Ok(if egcl_stdlib::input_stream_p(obj) {
                T
            } else {
                NIL
            })
        }),

        "EGCL::%NATIVE-STREAM-ELEMENT-TYPE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() != 1 {
                return Err(EgclError::ProgramError(
                    "%native-stream-element-type requires one stream".into(),
                ));
            }
            Ok(egcl_stdlib::stream_element_type(args[0]))
        }),

        "OUTPUT-STREAM-P" | "EGCL::%NATIVE-OUTPUT-STREAM-P" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (output-stream-p stream) → T if the stream can be written to.

            let obj = args.first().copied().unwrap_or(NIL);
            Ok(if egcl_stdlib::output_stream_p(obj) {
                T
            } else {
                NIL
            })
        }),

        "OPEN-STREAM-P" | "EGCL::%NATIVE-OPEN-STREAM-P" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (open-stream-p stream) → T if the stream is not closed.

            let obj = args.first().copied().unwrap_or(NIL);
            Ok(if egcl_stdlib::open_stream_p(obj) {
                T
            } else {
                NIL
            })
        }),

        "CLOSE" | "EGCL::%NATIVE-CLOSE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (close stream &key abort) → T. Closing a non-stream is a no-op.

            let stream = if args.is_empty() { NIL } else { args[0] };
            let mut abort = false;
            let mut i = 1;
            while i + 1 < args.len() {
                let key = args[i];
                if key.is_symbol() && sym_bare_name_rc(key).as_ref() == "ABORT" {
                    abort = args[i + 1] != NIL;
                }
                i += 2;
            }
            if is_stream(stream) {
                egcl_stdlib::close(stream, abort)?;
            }
            Ok(T)
        }),

        "EGCL::%FN-BODY" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            let f = args.first().copied().unwrap_or(NIL);
            Ok(if egcl_rt::function::is_interpreted_function(f) {
                egcl_rt::function::body(f)
            } else {
                NIL
            })
        }),

        "SET-DISPATCH-MACRO-CHARACTER" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() < 3 || !args[0].is_character() || !args[1].is_character() {
                return Err(EgclError::Internal(
                    "SET-DISPATCH-MACRO-CHARACTER requires disp-char sub-char function".into(),
                ));
            }
            let handler = coerce_installed_function(env, args[2]);
            let rt = args
                .get(3)
                .copied()
                .filter(|v| v.is_heap_object())
                .unwrap_or_else(cli_current_readtable);
            reader::set_dispatch_macro_character(
                rt,
                args[0].as_char(),
                args[1].as_char(),
                handler,
            )?;
            Ok(T)
        }),

        "GET-DISPATCH-MACRO-CHARACTER" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() < 2 || !args[0].is_character() || !args[1].is_character() {
                return Err(EgclError::ProgramError(
                    "GET-DISPATCH-MACRO-CHARACTER requires disp-char sub-char".into(),
                ));
            }
            let rt = args
                .get(2)
                .copied()
                .filter(|v| v.is_heap_object())
                .unwrap_or_else(cli_current_readtable);
            // CLHS: disp-char must be a dispatch macro character, else error.
            if !reader::is_dispatch_macro_character(rt, args[0].as_char()) {
                return Err(EgclError::StreamError(format!(
                    "{} is not a dispatch macro character",
                    args[0].as_char()
                )));
            }
            if let Some(handler) =
                reader::get_dispatch_macro_character(rt, args[0].as_char(), args[1].as_char())?
            {
                return Ok(handler);
            }
            if args[0].as_char() == '#' && args[1].as_char() == '\\' {
                let name = resolve_sym("EGCL::%STANDARD-CHARACTER-READER").unwrap();
                return Ok(symbol_function_object(env, name).unwrap_or(NIL));
            }
            Ok(NIL)
        }),

        "EGCL::%STANDARD-CHARACTER-READER" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() != 3 {
                return Err(EgclError::ProgramError(
                    "standard character reader requires stream, sub-character and argument".into(),
                ));
            }
            let suppress = env
                .lookup_var("*READ-SUPPRESS*")
                .is_some_and(|v| !v.is_nil());
            if !args[2].is_nil() && !suppress {
                return Err(EgclError::StreamError(
                    "#\\ does not accept a numeric argument".into(),
                ));
            }
            egcl_stdlib::streams::read_character_literal(args[0], suppress)
        }),

        "MAKE-DISPATCH-MACRO-CHARACTER" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (make-dispatch-macro-character char &optional non-term-p rt):
            // no char, a non-char, or more than 3 args is a PROGRAM-ERROR
            // (make-dispatch-macro-character.error.1/.2).
            if args.is_empty() || !args[0].is_character() || args.len() > 3 {
                return Err(EgclError::ProgramError(
                    "MAKE-DISPATCH-MACRO-CHARACTER requires a character".into(),
                ));
            }
            let non_term = args.get(1).copied().unwrap_or(NIL) != NIL;
            let rt = args
                .get(2)
                .copied()
                .filter(|v| v.is_heap_object())
                .unwrap_or_else(cli_current_readtable);
            reader::make_dispatch_macro_character(rt, args[0].as_char(), non_term)?;
            Ok(T)
        }),

        "SET-SYNTAX-FROM-CHAR" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (set-syntax-from-char to-char from-char &optional to-rt from-rt)

            if args.len() < 2 || !args[0].is_character() || !args[1].is_character() {
                return Err(EgclError::ProgramError(
                    "SET-SYNTAX-FROM-CHAR requires two characters".into(),
                ));
            }
            let to_rt = args
                .get(2)
                .copied()
                .filter(|v| v.is_heap_object())
                .unwrap_or_else(cli_current_readtable);
            // from-readtable defaults to the STANDARD readtable (NIL here,
            // which the reader treats as standard syntax).
            let from_rt = args
                .get(3)
                .copied()
                .filter(|v| v.is_heap_object())
                .unwrap_or(NIL);
            reader::set_syntax_from_char(args[0].as_char(), args[1].as_char(), to_rt, from_rt);
            Ok(T)
        }),

        "SET-MACRO-CHARACTER" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() < 2 || !args[0].is_character() {
                return Err(EgclError::Internal(
                    "SET-MACRO-CHARACTER requires char function".into(),
                ));
            }
            let handler = coerce_installed_function(env, args[1]);
            let non_term = args.get(2).copied().unwrap_or(NIL) != NIL;
            let rt = args
                .get(3)
                .copied()
                .filter(|v| v.is_heap_object())
                .unwrap_or_else(cli_current_readtable);
            reader::set_macro_character(rt, args[0].as_char(), handler, non_term)?;
            Ok(T)
        }),

        "GET-MACRO-CHARACTER" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (get-macro-character char &optional readtable): 0 or >2 args is
            // a PROGRAM-ERROR (get-macro-character.error.1/.2).
            if args.is_empty() || args.len() > 2 {
                return Err(EgclError::ProgramError(
                    "GET-MACRO-CHARACTER takes one or two arguments".into(),
                ));
            }
            if !args[0].is_character() {
                return Err(EgclError::TypeError {
                    datum: args[0],
                    expected: "CHARACTER".into(),
                });
            }
            let rt = args
                .get(1)
                .copied()
                .filter(|v| v.is_heap_object())
                .unwrap_or_else(cli_current_readtable);
            let ch = args[0].as_char();
            let (func, non_term) = reader::get_macro_character(rt, ch)?;
            let (f, nt) = match func {
                Some(f) if f != T => (f, non_term),
                // No custom handler: report the built-in macro function for a
                // standard macro character as an fbound placeholder symbol
                // (get-macro-character.1/.3 only check functionp/fboundp).
                _ => match reader::standard_macro_char(ch) {
                    Some(standard_nt) => (
                        resolve_sym("EGCL::%STANDARD-READER-MACRO").unwrap_or(NIL),
                        standard_nt,
                    ),
                    None => (NIL, false),
                },
            };
            env.set_mv(vec![f, if nt { T } else { NIL }]);
            Ok(f)
        }),

        "EGCL::%COPY-READTABLE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // CLHS: from omitted → copy the CURRENT readtable; from = NIL
            // → a readtable with STANDARD syntax (no user registrations).
            let to = args.get(1).copied().filter(|v| v.is_heap_object());
            match args.first().copied() {
                Some(v) if v.is_heap_object() => Ok(reader::copy_readtable(v, to)?),
                Some(v) if v == NIL => Ok(reader::make_readtable(None)?),
                _ => Ok(reader::copy_readtable(cli_current_readtable(), to)?),
            }
        }),

        "EGCL::%READTABLE-CASE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (egcl::%readtable-case rt) => :upcase|:downcase|:preserve|:invert

            let rt = args.first().copied().unwrap_or(NIL);
            let mode = reader::readtable_case_mode(rt).unwrap_or(0);
            let kw = match mode {
                1 => ":DOWNCASE",
                2 => ":PRESERVE",
                3 => ":INVERT",
                _ => ":UPCASE",
            };
            Ok(resolve_sym(kw).unwrap_or(NIL))
        }),

        "EGCL::%SET-READTABLE-CASE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (egcl::%set-readtable-case mode rt) => mode

            let mode_kw = args.first().copied().unwrap_or(NIL);
            let rt = args.get(1).copied().unwrap_or(NIL);
            let name = sym_bare_name_rc(mode_kw);
            let code = match name.as_ref() {
                "UPCASE" => 0u8,
                "DOWNCASE" => 1,
                "PRESERVE" => 2,
                "INVERT" => 3,
                _ => {
                    return Err(EgclError::TypeError {
                        datum: mode_kw,
                        expected: "one of :UPCASE :DOWNCASE :PRESERVE :INVERT".into(),
                    });
                }
            };
            reader::set_readtable_case_mode(rt, code);
            Ok(mode_kw)
        }),

        "EGCL::%SYM-BY-INDEX" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            let idx = args.first().map(|v| v.as_fixnum() as u32).unwrap_or(0);
            Ok(arena_str(&format!(
                "key={:?} name={:?}",
                egcl_rt::symbols::registry_key(idx),
                egcl_rt::symbols::symbol_name(idx)
            )))
        }),

        "EGCL::%FN-LAMBDA-LIST" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            let f = args.first().copied().unwrap_or(NIL);
            Ok(if egcl_rt::function::is_interpreted_function(f) {
                egcl_rt::function::lambda_list(f)
            } else {
                NIL
            })
        }),

        "EGCL::%NATIVE-MUTEX" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            env.clear_mv();
            egcl_stdlib::synchronization::call(&args)
        }),

        "EGCL::%NATIVE-CONDITION" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            env.clear_mv();
            egcl_stdlib::synchronization::condition_call(&args)
        }),

        "EGCL::%SOCKET-READ-TIMEOUT" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%socket-read-timeout stream &optional timeout-ms) → ms | NIL.

            if !(1..=2).contains(&args.len()) {
                return Err(EgclError::ProgramError(
                    "%socket-read-timeout requires stream and optional timeout-ms".into(),
                ));
            }
            if let Some(value) = args.get(1).copied() {
                let timeout = if value.is_nil() {
                    None
                } else if value.is_fixnum() && value.as_fixnum() > 0 {
                    Some(std::time::Duration::from_millis(value.as_fixnum() as u64))
                } else {
                    return Err(EgclError::TypeError {
                        datum: value,
                        expected: "(OR NULL (INTEGER 1 *))".into(),
                    });
                };
                egcl_stdlib::streams::socket_set_read_timeout(args[0], timeout)?;
            }
            Ok(egcl_stdlib::streams::socket_read_timeout(args[0])?
                .map(|duration| EgclVal::from_fixnum(duration.as_millis() as i64))
                .unwrap_or(NIL))
        }),

        "EGCL::%SOCKET-LISTEN" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%socket-listen host port &optional backlog) → listener-id

            let host = if args.is_empty() {
                "127.0.0.1".to_string()
            } else {
                val_as_str(args[0])
            };
            let port = if args.len() > 1 && args[1].is_fixnum() {
                args[1].as_fixnum() as u16
            } else {
                0
            };
            let backlog = if args.len() > 2 && args[2].is_fixnum() {
                args[2].as_fixnum() as i32
            } else {
                5
            };
            let id = egcl_stdlib::socket_listen(&host, port, backlog)?;
            Ok(EgclVal::from_fixnum(id as i64))
        }),

        // LIST and GETF are here because they were the two operations a UI
        // frame spends its time in and neither had an evaluated handler, so
        // COMPILED code reached them through the synthesize-`(name 'arg …)`-and-
        // re-evaluate detour. Measured on x86-64 release, 200k iterations:
        // (list 1 2 3 4) 8.67us and (getf plist :key) 7.31us, against 0.195us
        // for CONS and 0.035us for CAR, which do have handlers.
        "LIST" => Some(|_operator, args, _env| Ok(vec_to_list(args))),

        "GETF" => Some(|_operator, args, _env| {
            // (getf plist indicator &optional default). Was a DEFUN in
            // boot.lisp -- a DO loop plus an &optional -- which is why it cost
            // 200x a CAR. The scan allocates nothing, so no rooting is needed.
            if args.len() < 2 || args.len() > 3 {
                return Err(EgclError::ProgramError(
                    "GETF called with the wrong number of arguments; requires 2 to 3".into(),
                ));
            }
            let indicator = args[1];
            let mut cell = args[0];
            while cell.is_cons() {
                let (key, rest) = cp(cell);
                if !rest.is_cons() {
                    break;
                }
                if key == indicator {
                    return Ok(cp(rest).0);
                }
                cell = cp(rest).1;
            }
            Ok(args.get(2).copied().unwrap_or(NIL))
        }),

        "EGCL::%MAKE-STRUCT" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%make-struct 'name value...) -> a structure instance
            //
            // What a DEFSTRUCT constructor calls instead of MAKE-INSTANCE. The
            // values arrive already defaulted by the constructor's own lambda
            // list, so there is nothing left for the initialization protocol to
            // do -- and CLHS does not run it for a structure anyway.

            if args.is_empty() {
                return Err(EgclError::ProgramError(
                    "%make-struct: a structure name is required".into(),
                ));
            }
            let class = egcl_stdlib::find_class(args[0]).ok_or_else(|| {
                EgclError::ProgramError(format!(
                    "%make-struct: no such structure: {}",
                    format_val(args[0])
                ))
            })?;
            egcl_stdlib::make_struct(class, &args[1..])
        }),

        "EGCL::%SOCKET-LISTENER-READY-P" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%socket-listener-ready-p listener-id &optional timeout-ms) → T | NIL
            //
            // The listener counterpart of %SOCKET-WAIT-FOR-INPUT, which cannot
            // see one: a listener is an id into a table, not a stream. Without
            // this, %SOCKET-ACCEPT blocks until somebody connects, so a program
            // with its own event loop cannot offer a REPL between frames.

            if args.is_empty() || !args[0].is_fixnum() {
                return Err(EgclError::ProgramError(
                    "%socket-listener-ready-p: a listener id is required".into(),
                ));
            }
            let id = args[0].as_fixnum() as u64;
            let timeout = if args.len() > 1 && args[1].is_fixnum() {
                args[1].as_fixnum() as i32
            } else {
                0
            };
            Ok(if egcl_stdlib::socket_listener_ready(id, timeout)? {
                T
            } else {
                NIL
            })
        }),

        "EGCL::%SOCKET-LOCAL-PORT" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() || !args[0].is_fixnum() {
                return Ok(NIL);
            }
            Ok(egcl_stdlib::socket_local_port(args[0].as_fixnum() as u64)
                .map(|p| EgclVal::from_fixnum(p as i64))
                .unwrap_or(NIL))
        }),

        "EGCL::%SOCKET-ACCEPT" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%socket-accept listener-id) → connection stream (blocks)

            if args.is_empty() || !args[0].is_fixnum() {
                return Err(EgclError::ProgramError(
                    "%socket-accept: listener id must be an integer".into(),
                ));
            }
            egcl_stdlib::socket_accept(args[0].as_fixnum() as u64)
        }),

        "EGCL::%SOCKET-CLOSE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%socket-close listener-id) → NIL. Connections are closed via CLOSE.

            if let Some(v) = args.first() {
                if v.is_fixnum() {
                    egcl_stdlib::socket_close_listener(v.as_fixnum() as u64);
                }
            }
            Ok(NIL)
        }),

        "EGCL::%SOCKET-FD" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%socket-fd stream) → integer fd | NIL

            let s = args.first().copied().unwrap_or(NIL);
            Ok(egcl_stdlib::stream_raw_fd(s)
                .map(|fd| EgclVal::from_fixnum(fd as i64))
                .unwrap_or(NIL))
        }),

        "EGCL::%SOCKET-WAIT-FOR-INPUT" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%socket-wait-for-input stream &optional timeout-ms) → T | NIL

            let s = args.first().copied().unwrap_or(NIL);
            let timeout = if args.len() > 1 && args[1].is_fixnum() {
                Some(args[1].as_fixnum() as i32)
            } else {
                None
            };
            Ok(if egcl_stdlib::stream_wait_for_input(s, timeout)? {
                T
            } else {
                NIL
            })
        }),

        "WRITE-BYTE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (write-byte integer stream) → integer

            if args.len() < 2 {
                return Err(EgclError::Internal(
                    "WRITE-BYTE requires a byte and a stream".into(),
                ));
            }
            let byte = args[0];
            let out = resolve_output_stream(args[1], env);
            check_pending_sigpipe_for_output()?;
            if is_gray_stream(out) {
                invoke_generic_function("STREAM-WRITE-BYTE", &[out, byte], env)?;
            } else {
                egcl_stdlib::stream_write_byte(out, byte)?;
            }
            Ok(byte)
        }),

        "READ-BYTE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (read-byte stream &optional eof-error-p eof-value)

            if args.is_empty() {
                return Err(EgclError::Internal("READ-BYTE requires a stream".into()));
            }
            let inp = resolve_input_stream(args[0], env);
            let eof_error = args.get(1).map(|v| *v != NIL).unwrap_or(true);
            let b = if is_gray_stream(inp) {
                invoke_generic_function("STREAM-READ-BYTE", &[inp], env)?
            } else {
                egcl_stdlib::stream_read_byte(inp)?
            };
            if b == EOF {
                if eof_error {
                    return Err(EgclError::StreamError("end of file on READ-BYTE".into()));
                }
                return Ok(args.get(2).copied().unwrap_or(NIL));
            }
            Ok(b)
        }),

        "SLEEP" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (sleep seconds) → NIL. Blocks the (single) thread.

            let secs = if args.is_empty() {
                0.0
            } else {
                num_val(args[0])?
            };
            if secs > 0.0 {
                std::thread::sleep(std::time::Duration::from_secs_f64(secs));
            }
            Ok(NIL)
        }),

        "EGCL::%EXIT" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%exit &optional code) — flush and terminate the process.

            let code = if !args.is_empty() && args[0].is_fixnum() {
                args[0].as_fixnum() as i32
            } else {
                0
            };
            use std::io::Write;
            let _ = std::io::stdout().flush();
            std::process::exit(code);
        }),

        "MACRO-FUNCTION" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (macro-function symbol &optional environment) → an expander or
            // NIL. egcl macros are not first-class functions; return T for a
            // macro (callers here use it as a boolean) and NIL otherwise.

            let s = args.first().copied().unwrap_or(NIL);
            if s.is_symbol() {
                let name = sym_name(s);
                let bare = symbol_bare_name(&name);
                let mdef = lookup_macro(env, &name).or_else(|| lookup_macro(env, &bare));
                if let Some(mdef) = &mdef {
                    // A first-class expander installed via
                    // (setf (macro-function name) fn) round-trips (bliss-fo0o).
                    if let Some(f) = mdef.function {
                        return Ok(f);
                    }
                }
                // A registered source/bytecode macro, OR a standard CL macro
                // that egcl implements as a special-form arm (AND/OR/WHEN/COND/
                // MULTIPLE-VALUE-BIND/DEFUN/…). Either way MACRO-FUNCTION must
                // return a genuine two-argument (form environment) expander per
                // CLHS 3.1.2.1.2.2, so `(funcall (macro-function 'NAME) …)`
                // enforces the arity — a wrong count trips the lambda binder's
                // PROGRAM-ERROR (ansi AND/OR/WHEN/COND/RETURN/DEFUN/MULTIPLE-
                // VALUE-*/…-ERROR.1/2). Standard special OPERATORS that are not
                // macros (IF, PROGN, LET, QUOTE, …) are excluded and return NIL.
                if mdef.is_some() || is_ansi_standard_macro(&bare) {
                    return synthesize_macro_expander(env, s);
                }
            }
            Ok(NIL)
        }),

        "SPECIAL-OPERATOR-P" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (special-operator-p symbol) → generalized boolean. The 25 ANSI
            // special operators (CLHS 3.1.2.1.2.1) that the evaluator handles
            // as special forms. Lenient on a non-symbol argument (returns NIL),
            // matching the sibling MACRO-FUNCTION arm above.

            let s = args.first().copied().unwrap_or(NIL);
            // A non-symbol argument is a TYPE-ERROR (special-operator-p.error.1).
            if !s.is_symbol() {
                return Err(EgclError::TypeError {
                    datum: s,
                    expected: "SYMBOL".to_string(),
                });
            }
            if is_ansi_special_operator(&sym_bare_name_rc(s)) {
                return Ok(T);
            }
            Ok(NIL)
        }),

        "COMPILER-MACRO-FUNCTION" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());
            // (compiler-macro-function name &optional environment) → the
            // compiler macro or NIL. Compiler macros are always optional
            // (CLHS 3.2.2.1).
            //
            // One installed as a FUNCTION — `(setf (compiler-macro-function …) fn)`
            // — is handed back as that function, which is what CLHS specifies and
            // what lets a caller copy it to another name (iolib's DEFALIAS does).
            // One defined by DEFINE-COMPILER-MACRO is a host closure with no Lisp
            // function object behind it, so it still answers T, the boolean
            // existence answer callers have relied on; making that a real function
            // needs the expander to become a Lisp lambda (bliss-0g5lg). A
            // `(setf f)` name never has one (the definer skips non-symbol names),
            // so NIL is correct there.

            let s = args.first().copied().unwrap_or(NIL);
            if s.is_symbol() {
                if let Some(installed) = super::installed_compiler_macro_function(s) {
                    return Ok(installed);
                }
                if compiler_macroexpand::has_compiler_macro(s) {
                    return Ok(T);
                }
            }
            Ok(NIL)
        }),

        "GET" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (get symbol indicator &optional default)

            let s = args.first().copied().unwrap_or(NIL);
            // A non-symbol first argument is a TYPE-ERROR (get.error.4).
            if !s.is_symbol() {
                return Err(EgclError::TypeError {
                    datum: s,
                    expected: "SYMBOL".to_string(),
                });
            }
            let key = args.get(1).copied().unwrap_or(NIL);
            let default = args.get(2).copied().unwrap_or(NIL);
            Ok(plist_lookup(symbol_plist_of(s), key).unwrap_or(default))
        }),

        "REMPROP" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (remprop symbol indicator) → T if present. Rebuild without the
            // first matching pair.

            let s = args.first().copied().unwrap_or(NIL);
            let key = args.get(1).copied().unwrap_or(NIL);
            // A non-symbol is a TYPE-ERROR (remprop.error.4).
            if !s.is_symbol() {
                return Err(EgclError::TypeError {
                    datum: s,
                    expected: "SYMBOL".to_string(),
                });
            }
            // NIL and T report is_symbol()=true but carry the SPECIAL tag, not
            // TAG_SYMBOL, so as_symbol_index() panics/aborts on them; they have
            // no registry-backed plist, so nothing to remove (bliss-x7aa).
            if s == NIL || s == T {
                return Ok(NIL);
            }
            let idx = s.as_symbol_index();
            let plist = egcl_rt::symbols::symbol_plist(idx).unwrap_or(NIL);
            let mut kept: Vec<EgclVal> = Vec::new();
            let mut removed = false;
            let mut c = plist;
            while c.is_cons() {
                let (k, r) = cp(c);
                if !r.is_cons() {
                    break;
                }
                let (v, r2) = cp(r);
                if !removed && k == key {
                    removed = true;
                } else {
                    kept.push(k);
                    kept.push(v);
                }
                c = r2;
            }
            if removed {
                egcl_rt::symbols::set_symbol_plist(idx, vec_to_list(&kept));
            }
            Ok(if removed { T } else { NIL })
        }),

        "MAKE-STRING-INPUT-STREAM" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (make-string-input-stream string &optional start end)

            if args.is_empty() {
                return Err(EgclError::Internal(
                    "MAKE-STRING-INPUT-STREAM requires a string".into(),
                ));
            }
            let string = args[0];
            let start = if args.len() > 1 && args[1].is_fixnum() {
                args[1].as_fixnum() as usize
            } else {
                0
            };
            let end = if args.len() > 2 && args[2].is_fixnum() {
                Some(args[2].as_fixnum() as usize)
            } else {
                None
            };
            egcl_stdlib::make_string_input_stream(string, start, end)
        }),

        "READ-CHAR" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (read-char &optional stream eof-error-p eof-value)

            let stream = if !args.is_empty() { args[0] } else { NIL };
            let eof_error_p = if args.len() > 1 { args[1] } else { T };
            let in_stream = resolve_input_stream(stream, env);
            let (result, at_eof) = if is_gray_stream(in_stream) {
                let r = invoke_generic_function("STREAM-READ-CHAR", &[in_stream], env)?;
                let eof = !r.is_character();
                (r, eof)
            } else {
                let r = egcl_stdlib::stream_read_char(in_stream)?;
                let eof = r == EOF;
                (r, eof)
            };
            if at_eof {
                if eof_error_p.is_nil() {
                    // Re-read eof-value from the rooted args after the
                    // (allocating) read (bliss-6b2 #2).
                    return Ok(if args.len() > 2 { args[2] } else { NIL });
                }
                return Err(EgclError::StreamError("end of file on READ-CHAR".into()));
            }
            Ok(result)
        }),

        "PEEK-CHAR" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (peek-char &optional peek-type stream eof-error-p eof-value
            //  recursive-p): return the next character WITHOUT consuming it.
            // peek-type NIL = next char; T = skip whitespace; a character =
            // skip until that character (all of which are consumed).

            let peek_type = args.first().copied().unwrap_or(NIL);
            let stream = args.get(1).copied().unwrap_or(NIL);
            let eof_error_p = args.get(2).copied().unwrap_or(T);
            let mut in_stream = resolve_input_stream(stream, env);
            egcl_rt::rooted_ref!(_stream_root = &mut in_stream);
            let skip_ws = peek_type == T;
            let until = if peek_type.is_character() {
                Some(peek_type.as_char())
            } else {
                None
            };
            loop {
                // Read one character (gray-stream aware, like READ-CHAR).
                let (c, at_eof) = if is_gray_stream(in_stream) {
                    let r = invoke_generic_function("STREAM-READ-CHAR", &[in_stream], env)?;
                    (r, !r.is_character())
                } else {
                    let r = egcl_stdlib::stream_read_char(in_stream)?;
                    (r, r == EOF)
                };
                if at_eof {
                    if eof_error_p.is_nil() {
                        return Ok(args.get(3).copied().unwrap_or(NIL));
                    }
                    return Err(EgclError::StreamError("end of file on PEEK-CHAR".into()));
                }
                let ch = c.as_char();
                let stop = match (skip_ws, until) {
                    (true, _) => !ch.is_whitespace(),
                    (false, Some(u)) => ch == u,
                    (false, None) => true,
                };
                if stop {
                    // Put the peeked character back and return it.
                    if is_gray_stream(in_stream) {
                        invoke_generic_function("STREAM-UNREAD-CHAR", &[in_stream, c], env)?;
                    } else {
                        egcl_stdlib::stream_unread_char(in_stream, c)?;
                    }
                    return Ok(c);
                }
                // Otherwise the character is consumed; keep scanning.
            }
        }),

        "UNREAD-CHAR" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (unread-char character &optional stream)

            if args.is_empty() {
                return Err(EgclError::Internal(
                    "UNREAD-CHAR requires a character".into(),
                ));
            }
            let ch = args[0];
            let stream = if args.len() > 1 { args[1] } else { NIL };
            let in_stream = resolve_input_stream(stream, env);
            if is_gray_stream(in_stream) {
                invoke_generic_function("STREAM-UNREAD-CHAR", &[in_stream, ch], env)?;
            } else {
                egcl_stdlib::stream_unread_char(in_stream, ch)?;
            }
            Ok(NIL)
        }),

        "SET" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (set symbol value) — assign SYMBOL's dynamic (special) value,
            // like SETQ on the symbol / (setf (symbol-value symbol) value).
            // Returns VALUE. Arity (2,2) is enforced above (set.error.*).

            let sym = args[0];
            egcl_rt::rooted!(val = args[1]);
            if !sym.is_symbol() {
                return Err(EgclError::TypeError {
                    datum: sym,
                    expected: "SYMBOL".to_string(),
                });
            }
            env.set_var_symbol(sym, *val);
            Ok(*val)
        }),

        "COMPILED-FUNCTION-P" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (compiled-function-p object) — the predicate for the
            // COMPILED-FUNCTION type, which TYPEP already decides. Routing
            // it through TYPEP rather than reimplementing the test keeps the
            // two from disagreeing, which is exactly what ansi
            // COMPILED-FUNCTION-P.1 checks over the whole test universe.

            let object = args.first().copied().unwrap_or(NIL);
            let spec = resolve_sym("COMPILED-FUNCTION").unwrap_or(NIL);
            Ok(if typep_matches(env, object, spec)? {
                T
            } else {
                NIL
            })
        }),

        "FUNCTION-LAMBDA-EXPRESSION" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (function-lambda-expression fn) → three values: the defining
            // lambda expression (an implementation MAY always return NIL —
            // CLHS 3.1.2.1.2 — and egcl does not retain one), whether the
            // function has a non-null lexical closure, and its name.

            let f = args.first().copied().unwrap_or(NIL);
            // CLHS 3.1.2.1.2 constrains this value in ONE direction: it may
            // be false only when the function is definitely known to have
            // been defined in the null lexical environment, and an
            // implementation is explicitly permitted to return true
            // otherwise. egcl does not retain that fact reliably — the
            // interpreted-function object's env cell is left NIL even for a
            // genuine closure — so answering true for any function is both
            // conforming and the safe direction. Claiming "not a closure"
            // from missing information is the answer that would be wrong.
            let closure_p = if is_function_value(f) { T } else { NIL };
            let name = if egcl_rt::function::is_interpreted_function(f) {
                egcl_rt::function::name(f)
            } else if f.is_symbol() {
                f
            } else {
                NIL
            };
            env.set_mv(vec![NIL, closure_p, name]);
            Ok(NIL)
        }),

        "EGCL-THREAD::MAKE-THREAD" | "EGCL-THREAD:MAKE-THREAD" => {
            Some(|_operator, args, _env| {
                let args = RootedVals::new(args.to_vec());

                // (egcl-thread:make-thread function &key name) — spawn a
                // dedicated native OS thread that runs FUNCTION with no
                // arguments and returns its value to a later JOIN-THREAD
                // (bliss-q9i1, §13.5.3). NAME is retained by both the Lisp
                // descriptor and the host OS thread (bliss-94kq).
                // The handle is presently the raw native-thread id as a fixnum;
                // a distinct first-class THREAD object is tracked as bliss-8z5i.
                //
                // The runtime roots the entry until the new thread adopts it.
                // Preserve a closure cons as-is: reifying it as a fresh function
                // would discard its local function namespace and EQ identity.

                if args.is_empty() {
                    return Err(EgclError::ProgramError(
                        "EGCL-THREAD:MAKE-THREAD requires a function".into(),
                    ));
                }
                for pair in args[1..].chunks(2) {
                    if !is_keyword_arg(pair[0]) {
                        return Err(EgclError::ProgramError(
                            "EGCL-THREAD:MAKE-THREAD argument names must be keywords".into(),
                        ));
                    }
                }
                validate_builtin_keywords(&args[1..], &["NAME"])?;
                let fnv = args[0];
                // Both closure representations resolve their captured state
                // through process-wide, precisely scanned registries.
                let shareable = fnv.is_symbol()
                    || is_closure_cons(fnv)
                    || egcl_rt::function::is_interpreted_function(fnv);
                if !shareable {
                    return Err(EgclError::ProgramError(
                        "EGCL-THREAD:MAKE-THREAD entry must be a function or a symbol \
                         naming a global function (bliss-nubv)."
                            .to_string(),
                    ));
                }
                let name = args[1..]
                    .chunks(2)
                    .find(|pair| key_bare(pair[0]) == "NAME")
                    .map(|pair| pair[1])
                    .filter(|value| !value.is_nil())
                    .map(|value| {
                        if !is_string_value(value) {
                            return Err(EgclError::TypeError {
                                datum: value,
                                expected: "a string thread name or NIL".to_string(),
                            });
                        }
                        Ok(val_as_str(value))
                    })
                    .transpose()?;
                let id = egcl_rt::make_thread_named(fnv, name)?;
                Ok(EgclVal::from_fixnum(id.0 as i64))
            })
        }

        "EGCL-THREAD::JOIN-THREAD" | "EGCL-THREAD:JOIN-THREAD" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (egcl-thread:join-thread thread &key timeout) — block until
            // THREAD's entry function returns, or return NIL/NIL when the
            // timeout in seconds expires (§13.5.3 Death; bliss-94kq).

            if args.is_empty() {
                return Err(EgclError::ProgramError(
                    "EGCL-THREAD:JOIN-THREAD requires a thread".into(),
                ));
            }
            for pair in args[1..].chunks(2) {
                if !is_keyword_arg(pair[0]) {
                    return Err(EgclError::ProgramError(
                        "EGCL-THREAD:JOIN-THREAD argument names must be keywords".into(),
                    ));
                }
            }
            validate_builtin_keywords(&args[1..], &["TIMEOUT"])?;
            let tv = args[0];
            if !tv.is_fixnum() {
                return Err(EgclError::TypeError {
                    datum: tv,
                    expected: "a egcl-thread thread handle".to_string(),
                });
            }
            let id = egcl_rt::NativeThreadId(tv.as_fixnum() as u64);
            let timeout_value = args[1..]
                .chunks(2)
                .find(|pair| key_bare(pair[0]) == "TIMEOUT")
                .map(|pair| pair[1]);
            let timeout = match timeout_value {
                None | Some(NIL) => None,
                Some(value) => {
                    let seconds = num_val(value)?;
                    if !seconds.is_finite() || seconds < 0.0 {
                        return Err(EgclError::TypeError {
                            datum: value,
                            expected: "a non-negative real timeout in seconds".to_string(),
                        });
                    }
                    Some(
                        std::time::Duration::try_from_secs_f64(seconds).map_err(|_| {
                            EgclError::TypeError {
                                datum: value,
                                expected: "a representable non-negative real timeout in seconds"
                                    .to_string(),
                            }
                        })?,
                    )
                }
            };
            match egcl_rt::join_thread_timeout(id, timeout)? {
                Some(value) => {
                    env.set_mv(vec![value, T]);
                    Ok(value)
                }
                None => {
                    env.set_mv(vec![NIL, NIL]);
                    Ok(NIL)
                }
            }
        }),

        "EGCL-THREAD::CURRENT-THREAD" | "EGCL-THREAD:CURRENT-THREAD" => {
            Some(|_operator, args, _env| {
                let args = RootedVals::new(args.to_vec());

                // (egcl-thread:current-thread) — the running thread's handle.

                if !args.is_empty() {
                    return Err(EgclError::ProgramError(
                        "EGCL-THREAD:CURRENT-THREAD takes no arguments".into(),
                    ));
                }
                Ok(EgclVal::from_fixnum(
                    egcl_rt::current_thread_id().0 as i64,
                ))
            })
        }

        "EGCL-THREAD::THREAD-NAME" | "EGCL-THREAD:THREAD-NAME" => {
            Some(|_operator, args, _env| {
                let args = RootedVals::new(args.to_vec());

                if args.len() != 1 || !args[0].is_fixnum() {
                    return Err(EgclError::ProgramError(
                        "EGCL-THREAD:THREAD-NAME requires one thread handle".into(),
                    ));
                }
                let id = egcl_rt::NativeThreadId(args[0].as_fixnum() as u64);
                Ok(egcl_rt::thread_name(id)
                    .map(|name| arena_str(&name))
                    .unwrap_or(NIL))
            })
        }

        "EGCL-THREAD::THREAD-ALIVE-P" | "EGCL-THREAD:THREAD-ALIVE-P" => {
            Some(|_operator, args, _env| {
                let args = RootedVals::new(args.to_vec());

                if args.len() != 1 || !args[0].is_fixnum() {
                    return Err(EgclError::ProgramError(
                        "EGCL-THREAD:THREAD-ALIVE-P requires one thread handle".into(),
                    ));
                }
                let id = egcl_rt::NativeThreadId(args[0].as_fixnum() as u64);
                Ok(if egcl_rt::thread_alive(id).unwrap_or(false) {
                    T
                } else {
                    NIL
                })
            })
        }

        "EGCL-THREAD::ALL-THREADS" | "EGCL-THREAD:ALL-THREADS" => {
            Some(|_operator, args, _env| {
                let args = RootedVals::new(args.to_vec());

                if !args.is_empty() {
                    return Err(EgclError::ProgramError(
                        "EGCL-THREAD:ALL-THREADS takes no arguments".into(),
                    ));
                }
                let mut ids = egcl_rt::live_thread_ids();
                ids.sort_by_key(|id| id.0);
                egcl_rt::rooted!(threads = NIL);
                for id in ids.into_iter().rev() {
                    *threads = arena_cons(EgclVal::from_fixnum(id.0 as i64), *threads);
                }
                Ok(*threads)
            })
        }

        "EGCL-THREAD::THREAD-YIELD" | "EGCL-THREAD:THREAD-YIELD" => {
            Some(|_operator, args, _env| {
                let args = RootedVals::new(args.to_vec());

                if !args.is_empty() {
                    return Err(EgclError::ProgramError(
                        "EGCL-THREAD:THREAD-YIELD takes no arguments".into(),
                    ));
                }
                egcl_rt::thread_yield();
                Ok(NIL)
            })
        }

        "ERROR" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());
            // Root the evaluated datum and format args across the condition
            // construction, which allocates (bliss-6b2 #2). eval_args keeps
            // the whole arg vector rooted; a slice into it stays rooted too.

            if args.is_empty() {
                return Err(EgclError::Internal("ERROR".into()));
            }
            egcl_rt::rooted!(control = args[0]);
            let format_args = &args[1..];
            // (error datum &rest args): a condition instance is signalled as
            // is; a condition-type symbol is built via MAKE-CONDITION with the
            // remaining args as initargs; a format-control string becomes a
            // SIMPLE-ERROR.
            let condition = match coerce_condition_designator(env, *control, format_args)? {
                Some(condition) => condition,
                None => make_simple_condition("SIMPLE-ERROR", *control, format_args, env)?,
            };
            // Give handlers the condition before rendering it. A handled error
            // must not run a user :report function or traverse its datum merely
            // to prepare an unused terminal message (bliss-michh).
            // Root across both handler callbacks and any uncaught-error report.
            egcl_rt::rooted!(condition = condition);
            match signal_condition_object(*condition, env) {
                Ok(_) => {
                    let report = match condition_report_text(env, *condition) {
                        Some(report) => report,
                        None if is_string_value(*control) => {
                            format_control_message(&val_as_str(*control), format_args)?
                        }
                        None => val_as_str(*control),
                    };
                    Err(EgclError::Internal(format!("ERROR: {}", report)))
                }
                Err(error) => Err(error),
            }
        }),

        "ELT" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (elt sequence index) — works on lists, vectors, and strings.

            if args.len() < 2 {
                return Err(EgclError::Internal(
                    "ELT requires a sequence and an index".into(),
                ));
            }
            let seq = args[0];
            let idx = args[1];
            if !idx.is_fixnum() || idx.as_fixnum() < 0 {
                return Err(EgclError::TypeError {
                    datum: idx,
                    expected: "non-negative sequence index".into(),
                });
            }
            egcl_stdlib::elt(seq, idx.as_fixnum() as usize)
        }),

        "AREF" | "SVREF" | "ROW-MAJOR-AREF" | "BIT" | "SBIT" => Some(|operator, args, _env| {
            let args = RootedVals::new(args.to_vec());
            let name = operator.to_owned();
            // One-dimensional array/vector/string access — delegates to elt.
            // BIT/SBIT read a bit array exactly like AREF (rank-1 here).

            let arr0 = args.first().copied().unwrap_or(NIL);
            // Multidimensional (rank ≥ 2) array: AREF takes one subscript per
            // axis (row-major); ROW-MAJOR-AREF takes a single flat index.
            if egcl_rt::types::md_array_p(arr0) {
                let storage = egcl_rt::types::md_array_storage(arr0).unwrap();
                let flat = if name == "ROW-MAJOR-AREF" {
                    let idx = args.get(1).copied().unwrap_or(NIL);
                    if !idx.is_fixnum() || idx.as_fixnum() < 0 {
                        return Err(EgclError::TypeError {
                            datum: idx,
                            expected: "non-negative row-major index".into(),
                        });
                    }
                    idx.as_fixnum() as usize
                } else {
                    md_row_major_index(arr0, &args[1..])?
                };
                return egcl_stdlib::elt(storage, flat);
            }
            if args.len() != 2 {
                return Err(EgclError::ProgramError(format!(
                    "{}: only one-dimensional arrays are supported",
                    name
                )));
            }
            let arr = args[0];
            let idx = args[1];
            if !idx.is_fixnum() || idx.as_fixnum() < 0 {
                return Err(EgclError::TypeError {
                    datum: idx,
                    expected: "non-negative array index".into(),
                });
            }
            // AREF ignores fill pointers — it may read any element up to the
            // total size, not just the active prefix ELT bounds against
            // (CLHS AREF; bliss-30be). SVREF/BIT/SBIT hit non-complex arrays
            // so `aref` delegates to `elt` for them.
            egcl_stdlib::aref(arr, idx.as_fixnum() as usize)
        }),

        "EGCL-INTERNAL::%EXPAND-TYPE-SPEC"
        | "EGCL-INTERNAL:%EXPAND-TYPE-SPEC"
        | "%EXPAND-TYPE-SPEC" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            let spec = args.first().copied().unwrap_or(NIL);
            resolve_type_spec(env, spec)
        }),

        "EGCL-INTERNAL::%MAKE-SIMPLE-VECTOR"
        | "EGCL-INTERNAL:%MAKE-SIMPLE-VECTOR"
        | "%MAKE-SIMPLE-VECTOR" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            let size = args.first().copied().unwrap_or(NIL);
            if !size.is_fixnum() || size.as_fixnum() < 0 {
                return Err(EgclError::TypeError {
                    datum: size,
                    expected: "non-negative array dimension".into(),
                });
            }
            let fill = args.get(1).copied().unwrap_or(NIL);
            Ok(egcl_stdlib::build_filled_simple_vector(
                size.as_fixnum() as usize,
                fill,
            ))
        }),

        "EGCL-INTERNAL::%MAKE-MD-ARRAY" | "EGCL-INTERNAL:%MAKE-MD-ARRAY" | "%MAKE-MD-ARRAY" => {
            Some(|_operator, args, _env| {
                let args = RootedVals::new(args.to_vec());

                egcl_rt::rooted!(fill = args.get(1).copied().unwrap_or(NIL));
                let dims_list = args.first().copied().unwrap_or(NIL);
                let mut dims = Vec::new();
                let mut c = dims_list;
                while c.is_cons() {
                    let (d, rest) = cp(c);
                    if !d.is_fixnum() || d.as_fixnum() < 0 {
                        return Err(EgclError::TypeError {
                            datum: d,
                            expected: "non-negative array dimension".into(),
                        });
                    }
                    dims.push(d.as_fixnum() as usize);
                    c = rest;
                }
                Ok(egcl_stdlib::build_md_array(&dims, *fill))
            })
        }

        "EGCL-INTERNAL::%MAKE-COMPLEX-VECTOR"
        | "EGCL-INTERNAL:%MAKE-COMPLEX-VECTOR"
        | "%MAKE-COMPLEX-VECTOR" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%make-complex-vector size fill-pointer adjustable-p &optional
            // initial-element) — build a rank-1 fill-pointer / adjustable
            // vector. Called by MAKE-ARRAY (boot.lisp) for the
            // :fill-pointer/:adjustable cases.

            if args.len() < 3 {
                return Err(EgclError::Internal(
                    "%MAKE-COMPLEX-VECTOR requires size, fill-pointer, adjustable".into(),
                ));
            }
            let size = if args[0].is_fixnum() {
                args[0].as_fixnum().max(0) as usize
            } else {
                0
            };
            let fp = if args[1].is_fixnum() {
                args[1].as_fixnum().max(0) as usize
            } else {
                // :fill-pointer T (or absent) ⇒ full length.
                size
            };
            let adjustable = !args[2].is_nil();
            let iel = if args.len() > 3 { args[3] } else { NIL };
            // 5th arg (optional): non-NIL ⇒ element-type CHARACTER (a string).
            let element_is_char = args.len() > 4 && !args[4].is_nil();
            // 6th arg (optional): non-NIL ⇒ the user asked for :fill-pointer,
            // so ARRAY-HAS-FILL-POINTER-P answers T. A plain :adjustable
            // array is a COMPLEX_ARRAY too but has no fill pointer
            // (bliss-0x9y). Absent ⇒ NIL, matching the old callers.
            let has_fill_pointer = args.len() > 5 && !args[5].is_nil();
            // 7th arg (optional): non-NIL ⇒ element-type BIT (bliss-65nx).
            let element_is_bit = args.len() > 6 && !args[6].is_nil();
            let elems = vec![iel; size];
            Ok(egcl_stdlib::build_complex_vector(
                &elems,
                size,
                fp,
                adjustable,
                element_is_char,
                element_is_bit,
                has_fill_pointer,
            ))
        }),

        "EGCL-INTERNAL::%MAKE-DISPLACED-ARRAY"
        | "EGCL-INTERNAL:%MAKE-DISPLACED-ARRAY"
        | "%MAKE-DISPLACED-ARRAY" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (%make-displaced-array base offset length fill-pointer
            // adjustable-p element-is-char has-fill-pointer) — build a
            // rank-1 array displaced to BASE (bliss-7o4y). Called by
            // MAKE-ARRAY (boot.lisp) for :displaced-to.

            if args.len() < 3 {
                return Err(EgclError::Internal(
                    "%MAKE-DISPLACED-ARRAY requires base, offset, length".into(),
                ));
            }
            let base = args[0];
            let fix = |v: EgclVal| {
                if v.is_fixnum() {
                    v.as_fixnum().max(0) as usize
                } else {
                    0
                }
            };
            let offset = fix(args[1]);
            let length = fix(args[2]);
            let Some(base_total) = egcl_stdlib::array_total_size(base) else {
                return Err(EgclError::TypeError {
                    datum: base,
                    expected: "array (:displaced-to)".to_string(),
                });
            };
            if offset + length > base_total {
                return Err(EgclError::TypeError {
                    datum: base,
                    expected: format!(
                        "displacement {offset}+{length} within array-total-size {base_total}"
                    ),
                });
            }
            let fp = if args.len() > 3 && args[3].is_fixnum() {
                args[3].as_fixnum().max(0) as usize
            } else {
                length
            };
            let adjustable = args.len() > 4 && !args[4].is_nil();
            let element_is_char = args.len() > 5 && !args[5].is_nil();
            let has_fill_pointer = args.len() > 6 && !args[6].is_nil();
            // 8th arg (optional): non-NIL ⇒ element-type BIT (bliss-65nx).
            let element_is_bit = args.len() > 7 && !args[7].is_nil();
            Ok(egcl_stdlib::build_displaced_vector(
                base,
                offset,
                length,
                fp,
                adjustable,
                element_is_char,
                element_is_bit,
                has_fill_pointer,
            ))
        }),

        "EGCL-INTERNAL::%ADJUST-ARRAY" | "EGCL-INTERNAL:%ADJUST-ARRAY" | "%ADJUST-ARRAY" => {
            Some(|_operator, args, _env| {
                let args = RootedVals::new(args.to_vec());

                // (%adjust-array complex-vector new-size fill-pointer initial-elt)
                // — grow a rank-1 fill-pointer/adjustable vector in place and set
                // its fill pointer. Called by MAKE-ARRAY's ADJUST-ARRAY wrapper.

                if args.len() < 2 {
                    return Err(EgclError::Internal(
                        "%ADJUST-ARRAY requires an array and a new size".into(),
                    ));
                }
                let arr = args[0];
                let new_size = if args[1].is_fixnum() {
                    args[1].as_fixnum().max(0) as usize
                } else {
                    0
                };
                let fill_pointer = args
                    .get(2)
                    .filter(|fp| fp.is_fixnum())
                    .map(|fp| fp.as_fixnum().max(0) as usize);
                let iel = args.get(3).copied().unwrap_or(NIL);
                egcl_stdlib::adjust_complex_vector(arr, new_size, fill_pointer, iel)
            })
        }

        "VECTOR-PUSH" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (vector-push new-element vector) → index used, or NIL if full.

            if args.len() < 2 {
                return Err(EgclError::Internal(
                    "VECTOR-PUSH requires an element and a vector".into(),
                ));
            }
            let val = args[0];
            let vec = args[1];
            egcl_stdlib::vector_push(vec, val)
        }),

        "VECTOR-PUSH-EXTEND" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (vector-push-extend new-element vector &optional extension)

            if args.len() < 2 {
                return Err(EgclError::Internal(
                    "VECTOR-PUSH-EXTEND requires an element and a vector".into(),
                ));
            }
            let val = args[0];
            let vec = args[1];
            let ext = if args.len() > 2 {
                let e = args[2];
                e.is_fixnum().then(|| e.as_fixnum().max(0) as usize)
            } else {
                None
            };
            egcl_stdlib::vector_push_extend(vec, val, ext)
        }),

        "APPEND" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            egcl_stdlib::sequences::append(&args)
        }),

        "NTH" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() < 2 {
                // Wrong arg count is a catchable PROGRAM-ERROR, not an
                // uncatchable internal abort (ansi-test nth.error.*; bliss-x7aa).
                return Err(EgclError::ProgramError(
                    "NTH requires an index and a list".into(),
                ));
            }
            // ANSI: NTH's index is a non-negative integer. A negative
            // index (or a non-integer) is a TYPE-ERROR, not a saturated 0 —
            // `num_val(-1) as usize` used to silently return element 0.
            let nidx = args[0];
            if !egcl_rt::types::integerp(nidx) || num_val(nidx)? < 0.0 {
                return Err(EgclError::TypeError {
                    datum: nidx,
                    expected: "(integer 0)".into(),
                });
            }
            // A valid fixnum index; any (non-negative) bignum is far past
            // the end of a real list, so it reads as NIL.
            let idx = if nidx.is_fixnum() {
                nidx.as_fixnum() as usize
            } else {
                usize::MAX
            };
            nth_element(idx, args[1])
        }),

        "GETHASH" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (gethash key table &optional default) -> value; sets the
            // second value to the present-p flag. Wrong argument count is a
            // (catchable) PROGRAM-ERROR (ANSI; gethash.error.*).

            if args.len() < 2 || args.len() > 3 {
                return Err(EgclError::ProgramError(
                    "GETHASH requires a key, a table, and an optional default".into(),
                ));
            }
            let key = args[0];
            let tbl = args[1];
            let default = if args.len() >= 3 { args[2] } else { NIL };
            let (val, present) = egcl_stdlib::gethash(key, tbl, default)?;
            env.set_mv(vec![val, if present { T } else { NIL }]);
            Ok(val)
        }),

        "EGCL::PUT-GETHASH" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // Store primitive for bytecode-lowered `(setf (gethash key table)
            // value)` (bliss-x5y.2). Arguments arrive already evaluated —
            // value, key, table, in the SETF handler's value-first order —
            // through apply_function's synthesize path. Returns the value.

            let val = args.first().copied().unwrap_or(NIL);
            let key = args.get(1).copied().unwrap_or(NIL);
            let tbl = args.get(2).copied().unwrap_or(NIL);
            egcl_stdlib::set_gethash(key, tbl, val)?;
            // Re-read from the rooted args: set_gethash may rehash/allocate.
            Ok(args.first().copied().unwrap_or(NIL))
        }),

        "EGCL::SET-FDEFINITION" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());
            // Store primitive for bytecode-lowered
            // `(setf (fdefinition name) fn)`. Unlike SYMBOL-FUNCTION,
            // FDEFINITION names any FUNCTION NAME, so a `(setf place)` cons
            // is legal and installs the writer on the mangled
            // %SETF-WRITER-place symbol — where the compiled SETF path calls
            // it and FBOUNDP / #'(setf f) / FMAKUNBOUND look (bliss-rg32).

            let name = args.first().copied().unwrap_or(NIL);
            let val = args.get(1).copied().unwrap_or(NIL);
            check_function_name(name)?;
            let fnval = coerce_installed_function(env, val);
            if name.is_symbol() {
                if let Some(index) = name.symbol_index() {
                    egcl_rt::symbols::set_symbol_function(index, fnval);
                }
            } else {
                let (_setf, tail) = cp(name);
                let place = cp(tail).0;
                if let Some(index) = resolve_sym(&setf_writer_symbol_name(&sym_name(place)))
                    .and_then(|writer| writer.symbol_index())
                {
                    egcl_rt::symbols::set_symbol_function(index, fnval);
                }
            }
            Ok(val)
        }),

        "EGCL::SET-SYMBOL-FUNCTION" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // Store primitive for bytecode-lowered `(setf (symbol-function|
            // fdefinition sym) fn)`. Args arrive evaluated (sym, value);
            // install fn as the symbol's global function and return it.

            let sym = args.first().copied().unwrap_or(NIL);
            let val = args.get(1).copied().unwrap_or(NIL);
            if !sym.is_symbol() {
                return Err(EgclError::Internal(format!(
                    "SET-SYMBOL-FUNCTION: expected a symbol, got {}",
                    format_val(sym)
                )));
            }
            let fnval = coerce_installed_function(env, val);
            egcl_rt::symbols::set_symbol_function(sym.as_symbol_index(), fnval);
            Ok(val)
        }),

        "EGCL::SET-SYMBOL-PLIST" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // Store primitive behind the `(setf symbol-plist)` writer function
            // (bliss-6buay). The writer cannot be `(setf (symbol-plist s) new)`:
            // whether the lowerer emits a direct store or a call to the writer
            // depends on the surrounding form, so such a body recurses into
            // itself in whichever context takes the call route. Args arrive
            // evaluated (symbol, plist); the SETF arm's TYPE-ERROR on a
            // non-symbol is kept so both routes reject the same inputs.
            let sym = args.first().copied().unwrap_or(NIL);
            let plist = args.get(1).copied().unwrap_or(NIL);
            if !sym.is_symbol() {
                return Err(EgclError::TypeError {
                    datum: sym,
                    expected: "SYMBOL".to_string(),
                });
            }
            if let Some(idx) = sym.symbol_index() {
                egcl_rt::symbols::set_symbol_plist(idx, plist);
            }
            Ok(plist)
        }),

        "EGCL::SET-SLOT-VALUE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            let instance = args.first().copied().unwrap_or(NIL);
            let slot = args.get(1).copied().unwrap_or(NIL);
            let val = args.get(2).copied().unwrap_or(NIL);
            store_slot_value(instance, slot, val, env)
        }),

        "EGCL::GET-ACCESSOR-SLOT" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());
            // Read primitive for bytecode-lowered `(accessor obj)` where ACCESSOR
            // is a DEFCLASS :accessor/:reader or a DEFSTRUCT accessor. The mirror
            // of SET-ACCESSOR-SLOT, and resolved at run time for the same reasons:
            // a function can be compiled before the class it touches exists, and
            // the class can be redefined afterwards.
            //
            // Exists because `(ball-x b)` otherwise lowered to a generic CallNamed
            // and paid full dispatch — 4.06us against 1.26us for the bare
            // `(slot-value b 'x)` it amounts to (bliss-fskhm).
            let instance = args.first().copied().unwrap_or(NIL);
            let accessor = args.get(1).copied().unwrap_or(NIL);
            // STRICT: only this instance's own class chain counts. If it does not
            // declare the accessor, this is not the simple case the lowerer bet on
            // — an EQL-specialized method, a user method on another class, a
            // non-instance argument — and real dispatch has to run. Guessing a
            // slot by name would read the wrong one silently.
            match accessor_slot_symbol_cached(env, instance, accessor) {
                Some(slot) => read_slot_value(instance, slot, env),
                None => invoke_generic_function(&sym_name(accessor), &[instance], env),
            }
        }),

        "EGCL::SET-ACCESSOR-SLOT" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());
            // Store primitive for bytecode-lowered `(setf (accessor obj) v)`
            // where ACCESSOR is a DEFCLASS :accessor/:reader/:writer or a
            // DEFSTRUCT accessor (bliss-ljmj).
            //
            // The accessor -> slot mapping is resolved HERE, at run time,
            // exactly as the tree-walker does. Baking the slot name in at
            // lowering time would be wrong in both directions: a function can
            // be compiled before the class it touches exists, and a class can
            // be redefined afterwards. The lowerer only uses the mapping's
            // existence as a gate.

            let instance = args.first().copied().unwrap_or(NIL);
            let accessor = args.get(1).copied().unwrap_or(NIL);
            let val = args.get(2).copied().unwrap_or(NIL);
            // Resolved against THIS instance's class, not by name alone: two
            // classes may give the same accessor name to differently-named
            // slots, and the name-only search returns an arbitrary one of them
            // (bliss-i6ga1).
            // The memo answers for the store side too: same (class, accessor) ->
            // slot question, same per-call string round trip avoided (bliss-1qjmm).
            if let Some(slot) = accessor_slot_symbol_cached(env, instance, accessor) {
                write_slot_value(instance, slot, val, env)?;
                return Ok(val);
            }
            let Some(slot_name) =
                accessor_slot_name_for_instance(env, instance, &sym_name(accessor))
            else {
                return Err(EgclError::Internal(format!(
                    "SETF: {} does not name a slot accessor",
                    format_val(accessor)
                )));
            };
            write_slot_value(instance, resolve_sym(&slot_name).unwrap_or(NIL), val, env)?;
            Ok(val)
        }),

        "REMHASH" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (remhash key table) — exactly two args; otherwise PROGRAM-ERROR
            // (ANSI; remhash.error.*).

            if args.len() != 2 {
                return Err(EgclError::ProgramError(
                    "REMHASH requires a key and a table".into(),
                ));
            }
            let removed = egcl_stdlib::remhash(args[0], args[1])?;
            Ok(if removed { T } else { NIL })
        }),

        "MAPHASH" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (maphash function hash-table): call FUNCTION on each key/value
            // pair through the unified function protocol (bliss-jtc.8) — any
            // callable (lambda, closure, heap function object, builtin), not
            // only a native pointer. Iterates a snapshot so the table may be
            // mutated (per-key) during the walk. Returns NIL.

            if args.len() != 2 {
                return Err(EgclError::ProgramError(
                    "MAPHASH requires a function and a table".into(),
                ));
            }
            egcl_rt::rooted!(function = args[0]);
            // Root the entry snapshot: each apply_function runs user code that
            // can relocate the still-pending Vec-resident keys/values (#2).
            egcl_rt::rooted!(entries = egcl_stdlib::hash_table_entries(args[1])?);
            for i in 0..entries.len() {
                let (key, value) = entries[i];
                apply_function(*function, &[key, value], env)?;
            }
            Ok(NIL)
        }),

        "SUBSEQ" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            subseq_values(&args)
        }),

        "SOME" | "EVERY" | "NOTANY" | "NOTEVERY" => Some(|operator, args, env| {
            let args = RootedVals::new(args.to_vec());
            let name = operator.to_owned();

            // CLHS `predicate &rest sequences+`: a predicate plus at least one
            // sequence are required — fewer is a PROGRAM-ERROR, not a silent
            // result (ansi SOME.ERROR.8/9, EVERY/NOTANY/NOTEVERY.ERROR.8/9).
            if args.len() < 2 {
                return Err(EgclError::ProgramError(format!(
                    "{name} requires a predicate and at least one sequence"
                )));
            }
            // Root predicate + all sequence elements across the apply loop
            // (bliss-6b2 #2): apply_function runs user code that can relocate
            // these Vec-resident values.
            egcl_rt::rooted!(pred = args[0]);
            egcl_rt::rooted!(seqs = Vec::<Vec<EgclVal>>::with_capacity(args.len() - 1));
            for s in &args[1..] {
                seqs.push(seq_elements(*s)?);
            }
            let minlen = seqs.iter().map(Vec::len).min().unwrap_or(0);
            for i in 0..minlen {
                let call_args: Vec<EgclVal> = seqs.iter().map(|s| s[i]).collect();
                let r = apply_function(*pred, &call_args, env)?;
                match name.as_str() {
                    "SOME" if !r.is_nil() => return Ok(r),
                    "EVERY" if r.is_nil() => return Ok(NIL),
                    "NOTANY" if !r.is_nil() => return Ok(NIL),
                    "NOTEVERY" if r.is_nil() => return Ok(T),
                    _ => {}
                }
            }
            Ok(match name.as_str() {
                "SOME" => NIL,
                "EVERY" | "NOTANY" => T,
                _ => NIL, // NOTEVERY
            })
        }),

        "MAKE-PATHNAME" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());
            // (make-pathname &key host device directory name type version defaults)

            // Track supplied-p per component so an explicit `:name nil`
            // (override) is distinguished from an unsupplied component (which
            // is taken from :defaults, per ANSI).
            let (mut host, mut device, mut directory) = (None, None, None);
            let (mut name_c, mut type_c, mut version) = (None, None, None);
            let mut defaults: Option<EgclVal> = None;
            let mut i = 0;
            while i + 1 < args.len() {
                let key = sym_bare_name_rc(args[i]);
                let val = args[i + 1];
                match key.as_ref() {
                    "HOST" => host = Some(val),
                    "DEVICE" => device = Some(val),
                    // A directory given as (:absolute|:relative comp…) uses
                    // reader keywords the stdlib can't match by hash; render
                    // it to a namestring the stdlib parser accepts.
                    "DIRECTORY" => {
                        // Reject syntactically impossible directory lists
                        // (`:up`/`:back` right after `:absolute` or a
                        // `:wild-inferiors`) with a FILE-ERROR before
                        // building the pathname (ansi make-pathname-error-*).
                        validate_make_pathname_directory(val)?;
                        directory = Some(match directory_designator_to_namestring(val) {
                            Some(s) => arena_str(&s),
                            None => val,
                        });
                    }
                    "NAME" => name_c = Some(val),
                    "TYPE" => type_c = Some(val),
                    "VERSION" => version = Some(val),
                    "DEFAULTS" => defaults = Some(val),
                    _ => {}
                }
                i += 2;
            }
            // Components not explicitly supplied are taken from :defaults
            // (ANSI). :defaults is a pathname *designator*, so a namestring
            // string must be coerced to a pathname first — otherwise its
            // components are silently dropped and, e.g., UIOP's
            // `pathname-directory-pathname` (make-pathname :defaults <string>)
            // loses the directory (bliss-aid). A non-coercible value stays
            // unused, as before.
            let d = match defaults {
                Some(v) if egcl_stdlib::is_pathname(v) => Some(v),
                Some(v) => coerce_pathname_designator(v).ok(),
                None => None,
            };
            let resolve = |supplied: Option<EgclVal>, from: fn(EgclVal) -> EgclVal| {
                supplied.unwrap_or_else(|| d.map(from).unwrap_or(NIL))
            };
            egcl_stdlib::make_pathname(
                resolve(host, egcl_stdlib::pathname_host),
                resolve(device, egcl_stdlib::pathname_device),
                resolve(directory, egcl_stdlib::pathname_directory),
                resolve(name_c, egcl_stdlib::pathname_name),
                resolve(type_c, egcl_stdlib::pathname_type),
                resolve(version, egcl_stdlib::pathname_version),
            )
        }),

        "PARSE-NAMESTRING" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());
            // (parse-namestring thing &optional host default &key start end
            // junk-allowed) — at least one argument; zero args, an unknown
            // keyword, or a dangling keyword is a PROGRAM-ERROR (ansi
            // parse-namestring.error.1 / parse-name-string.error.2/.3).

            if args.is_empty() {
                return Err(EgclError::ProgramError(
                    "PARSE-NAMESTRING requires at least 1 argument".into(),
                ));
            }
            // Validate the &key section (everything past thing/host/default).
            if args.len() > 3 {
                let keys = &args[3..];
                if keys.len() % 2 != 0 {
                    return Err(EgclError::ProgramError(
                        "PARSE-NAMESTRING called with an odd number of keyword arguments".into(),
                    ));
                }
                let mut i = 0;
                while i < keys.len() {
                    let ok = keys[i].is_symbol() && keys[i] != NIL && {
                        let bare = sym_bare_name_rc(keys[i]);
                        matches!(bare.as_ref(), "START" | "END" | "JUNK-ALLOWED")
                    };
                    if !ok {
                        return Err(EgclError::ProgramError(
                            "PARSE-NAMESTRING called with invalid keyword arguments".into(),
                        ));
                    }
                    i += 2;
                }
            }
            let mut thing = args[0];
            // A non-simple string designator (fill-pointer / adjustable /
            // displaced char array) → a fresh simple string the parser reads
            // (ansi parse-namestring.3). A pathname passes through unchanged.
            if !egcl_stdlib::is_pathname(thing) {
                thing = normalize_pathname_string_designator(thing);
            }
            egcl_rt::rooted_ref!(_thing_root = &mut thing);
            let mut host = args.get(1).copied();
            egcl_rt::rooted_ref!(_host_root = &mut host);
            let (pathname, position) = egcl_stdlib::parse_namestring(thing, host, None)?;
            env.set_mv(vec![pathname, EgclVal::from_fixnum(position as i64)]);
            Ok(pathname)
        }),

        "OPEN" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (open filespec &key direction element-type if-exists
            // if-does-not-exist external-format) → a stream, or NIL when
            // `:if-does-not-exist nil` and the file is missing (bliss-wne9.5).
            // The caller owns the stream and must CLOSE it; WITH-OPEN-FILE is
            // the same machinery with the close wired into an unwind.
            // `eval_args` hands back a self-rooting RootedVals, so the
            // filespec and option values stay traced without further work.

            let Some((path_val, opts)) = args.split_first() else {
                return Err(EgclError::ProgramError(
                    "OPEN requires a filespec argument".into(),
                ));
            };
            let path_val = *path_val;
            if env.sandbox {
                return Err(EgclError::SandboxViolation(format!(
                    "File access denied in sandbox mode: {}",
                    val_as_str(path_val)
                )));
            }
            egcl_rt::rooted!(path_val = path_val);
            let options = decode_open_options(opts, env)?;
            egcl_stdlib::open(
                *path_val,
                options.direction,
                options.element_type,
                options.if_exists,
                options.if_does_not_exist,
                egcl_stdlib::ExternalFormat::Utf8,
            )
        }),

        "EGCL-EXT:GET-PRECISE-TIME" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if !args.is_empty() {
                return Err(EgclError::ProgramError(format!(
                    "EGCL-EXT:GET-PRECISE-TIME requires no arguments, got {}",
                    args.len()
                )));
            }
            let (seconds, nanoseconds) = egcl_stdlib::time::get_precise_time();
            let seconds = EgclVal::from_fixnum(seconds);
            let nanoseconds = EgclVal::from_fixnum(nanoseconds);
            env.set_mv(vec![seconds, nanoseconds]);
            Ok(seconds)
        }),

        "ENCODE-UNIVERSAL-TIME" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (encode-universal-time second minute hour date month year
            //  &optional time-zone)

            if args.len() < 6 {
                return Err(EgclError::ProgramError(
                    "ENCODE-UNIVERSAL-TIME requires at least 6 arguments".into(),
                ));
            }
            let n = |v: EgclVal| -> Result<i64, EgclError> { Ok(num_val(v)? as i64) };
            let (second, minute, hour) = (n(args[0])?, n(args[1])?, n(args[2])?);
            let (date, month, mut year) = (n(args[3])?, n(args[4])?, n(args[5])?);
            // CLHS 25.1.4: a two-digit year is relative to a 50-year window
            // around the current year.
            if (0..=99).contains(&year) {
                let current = egcl_stdlib::time::decode_universal_time(
                    egcl_stdlib::time::get_universal_time(),
                    Some(0),
                )
                .5;
                let base = current - 50;
                year = base + (year - base).rem_euclid(100);
            }
            // Time zone is hours west of GMT; NIL / omitted means local,
            // which we model as GMT (see time.rs).
            let time_zone = match args.get(6) {
                Some(v) if !v.is_nil() => Some(num_val(*v)? as i64),
                _ => None,
            };
            Ok(EgclVal::from_fixnum(
                egcl_stdlib::time::encode_universal_time(
                    second, minute, hour, date, month, year, time_zone,
                ),
            ))
        }),

        "DECODE-UNIVERSAL-TIME" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (decode-universal-time universal-time &optional time-zone)
            //   => second, minute, hour, date, month, year,
            //      day-of-week, daylight-p, zone

            if args.is_empty() {
                return Err(EgclError::ProgramError(
                    "DECODE-UNIVERSAL-TIME requires a universal time".into(),
                ));
            }
            let universal = num_val(args[0])? as i64;
            let time_zone = match args.get(1) {
                Some(v) if !v.is_nil() => Some(num_val(*v)? as i64),
                _ => None,
            };
            let (sec, min, hour, date, month, year, dow, dst, zone) =
                egcl_stdlib::time::decode_universal_time(universal, time_zone);
            let values = vec![
                EgclVal::from_fixnum(sec),
                EgclVal::from_fixnum(min),
                EgclVal::from_fixnum(hour),
                EgclVal::from_fixnum(date),
                EgclVal::from_fixnum(month),
                EgclVal::from_fixnum(year),
                EgclVal::from_fixnum(dow),
                if dst { T } else { NIL },
                EgclVal::from_fixnum(zone),
            ];
            let first = values[0];
            env.set_mv(values);
            Ok(first)
        }),

        "TRANSLATE-LOGICAL-PATHNAME" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (translate-logical-pathname pathname &key …) — a physical
            // pathname translates to itself (EQ); a logical one is resolved
            // through its host's translation rules. Extra keyword args
            // (:allow-other-keys etc.) are accepted and ignored. Zero args is
            // a PROGRAM-ERROR (ansi translate-logical-pathname.error.1).

            if args.is_empty() {
                return Err(EgclError::ProgramError(
                    "TRANSLATE-LOGICAL-PATHNAME requires at least 1 argument".into(),
                ));
            }
            egcl_rt::rooted!(pn = coerce_to_pathname(args[0])?);
            if egcl_stdlib::is_logical_pathname(*pn) {
                return egcl_stdlib::translate_logical_pathname(*pn);
            }
            Ok(*pn)
        }),

        "MERGE-PATHNAMES" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());
            // (merge-pathnames pathname &optional default-pathname default-version)

            if args.is_empty() {
                return Err(EgclError::ProgramError(
                    "MERGE-PATHNAMES requires a pathname".into(),
                ));
            }
            // Coerce a pathname designator (string / pathname) to a pathname.
            let to_pathname = |v: EgclVal| -> Result<EgclVal, EgclError> {
                if egcl_stdlib::is_pathname(v) {
                    Ok(v)
                } else {
                    let mut v = v;
                    egcl_rt::rooted_ref!(_v_root = &mut v);
                    Ok(egcl_stdlib::parse_namestring(v, None, None)?.0)
                }
            };
            // Root the intermediate pathnames: to_pathname/lookup allocate and
            // can relocate an earlier result (bliss-6b2 #2).
            egcl_rt::rooted!(pathname = to_pathname(args[0])?);
            egcl_rt::rooted!(
                default = if args.len() > 1 {
                    to_pathname(args[1])?
                } else {
                    // ANSI default is *default-pathname-defaults*.
                    match env.lookup_var("*DEFAULT-PATHNAME-DEFAULTS*") {
                        Some(v) if !v.is_nil() => to_pathname(v)?,
                        _ => to_pathname(egcl_stdlib::make_lisp_string("./"))?,
                    }
                }
            );
            // ANSI: default-version defaults to :NEWEST when not supplied, so
            // a merged pathname whose name comes from a versionless source
            // gets version :NEWEST (ansi merge-pathnames.2/.3/.4/.7). An
            // explicit third argument (including NIL) is honoured as given
            // (merge-pathnames.1 passes NIL and expects the version to stay
            // NIL).
            let default_version = if args.len() > 2 {
                args[2]
            } else {
                resolve_sym(":NEWEST").unwrap_or(NIL)
            };
            egcl_stdlib::merge_pathnames(*pathname, *default, default_version)
        }),

        "PATHNAME-NAME" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            let pathname =
                coerce_pathname_designator(pathname_accessor_arg("PATHNAME-NAME", &args)?)?;
            Ok(egcl_stdlib::pathname_name(pathname))
        }),

        "PATHNAME-TYPE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            let pathname =
                coerce_pathname_designator(pathname_accessor_arg("PATHNAME-TYPE", &args)?)?;
            Ok(egcl_stdlib::pathname_type(pathname))
        }),

        "PATHNAME-DIRECTORY" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            let mut pathname = pathname_accessor_arg("PATHNAME-DIRECTORY", &args)?;
            // Coerce a namestring designator to a pathname first (ANSI).
            if !egcl_stdlib::is_pathname(pathname) {
                pathname = coerce_pathname_designator(pathname)?;
            }
            // ANSI PATHNAME-DIRECTORY returns a list (:absolute|:relative
            // comp…), not a namestring — UIOP does directory-list arithmetic
            // on it (bliss-lb6). Build the keywords with the interpreter's
            // interner so they are EQ to the reader's :absolute / :wild / ….
            match egcl_stdlib::pathname_directory_components(pathname) {
                Some((absolute, comps)) => {
                    let kw = |s: &str| resolve_sym(s).unwrap_or(NIL);
                    let mut elems = Vec::with_capacity(comps.len() + 1);
                    // Root the accumulator in place: each `make_lisp_string`
                    // below allocates and `resolve_sym` can intern, so a
                    // relocating minor GC on component N would leave the
                    // strings already pushed for components < N as stale
                    // pointers, and `vec_to_list` would then build the result
                    // out of them. This handler runs 2062 times during a
                    // first-load of Babel alone (bliss-noqr).
                    egcl_rt::rooted_ref!(_elems_root = &mut elems);
                    elems.push(kw(if absolute { ":ABSOLUTE" } else { ":RELATIVE" }));
                    for c in comps {
                        elems.push(match c {
                            egcl_stdlib::PathDirComp::Name(s) => {
                                egcl_stdlib::make_lisp_string(&s)
                            }
                            egcl_stdlib::PathDirComp::Up => kw(":UP"),
                            egcl_stdlib::PathDirComp::Wild => kw(":WILD"),
                            egcl_stdlib::PathDirComp::WildInferiors => kw(":WILD-INFERIORS"),
                        });
                    }
                    Ok(vec_to_list(&elems))
                }
                None => Ok(NIL),
            }
        }),

        "PATHNAME-HOST" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            let pathname =
                coerce_pathname_designator(pathname_accessor_arg("PATHNAME-HOST", &args)?)?;
            Ok(egcl_stdlib::pathname_host(pathname))
        }),

        "PATHNAME-DEVICE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            let pathname =
                coerce_pathname_designator(pathname_accessor_arg("PATHNAME-DEVICE", &args)?)?;
            // A logical pathname's device is :UNSPECIFIC (ANSI 19.3.2.1;
            // ansi pathname-device.7).
            if egcl_stdlib::is_logical_pathname(pathname) {
                return Ok(resolve_sym(":UNSPECIFIC").unwrap_or(NIL));
            }
            Ok(egcl_stdlib::pathname_device(pathname))
        }),

        "PATHNAME-VERSION" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (pathname-version pathname) — no :case keyword, so any extra
            // argument is a PROGRAM-ERROR (ansi pathname-version.error.2).

            if args.len() != 1 {
                return Err(EgclError::ProgramError(format!(
                    "PATHNAME-VERSION requires exactly 1 argument, got {}",
                    args.len()
                )));
            }
            let pathname = coerce_pathname_designator(args[0])?;
            Ok(egcl_stdlib::pathname_version(pathname))
        }),

        "WILD-PATHNAME-P" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (wild-pathname-p pathname &optional field-key) — 1 or 2 args;
            // other arity is a PROGRAM-ERROR (ansi wild-pathname-p.error.1/2).

            if args.is_empty() || args.len() > 2 {
                return Err(EgclError::ProgramError(format!(
                    "WILD-PATHNAME-P requires 1 or 2 arguments, got {}",
                    args.len()
                )));
            }
            // ANSI: the argument is a pathname designator (pathname, string,
            // or file/synonym stream). A stream's pathname is never wild, so
            // it answers NIL (ansi wild-pathname-p.29). A non-designator
            // (number, char, list, …) is a TYPE-ERROR — coerce_to_pathname
            // raises it (ansi wild-pathname-p.error.3/4).
            if is_stream(args[0]) {
                return Ok(NIL);
            }
            let pathname = coerce_to_pathname(args[0])?;
            let field = if args.len() > 1 { Some(args[1]) } else { None };
            Ok(if egcl_stdlib::wild_pathname_p(pathname, field) {
                T
            } else {
                NIL
            })
        }),

        "PATHNAME-MATCH-P" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (pathname-match-p pathname wildcard) — exactly two pathname
            // designators (strings coerce to pathnames). Other arity is a
            // PROGRAM-ERROR (ansi pathname-match-p.error.1/2/3).

            if args.len() != 2 {
                return Err(EgclError::ProgramError(format!(
                    "PATHNAME-MATCH-P requires exactly 2 arguments, got {}",
                    args.len()
                )));
            }
            egcl_rt::rooted!(pn = coerce_to_pathname(args[0])?);
            let wc = coerce_to_pathname(args[1])?;
            Ok(if egcl_stdlib::pathname_match_p(*pn, wc)? {
                T
            } else {
                NIL
            })
        }),

        "MIN" | "MAX" => Some(|operator, args, _env| {
            let args = RootedVals::new(args.to_vec());
            let name = operator.to_owned();
            // Return the actual extreme ARGUMENT (preserving its exact type),
            // compared with numeric_cmp — the old f64 path lost precision and
            // overflowed the `as i64` cast on bignums (bliss-05hy).

            if args.is_empty() {
                // ANSI: MIN/MAX require at least one argument; too few args
                // is a (catchable) PROGRAM-ERROR, not an internal error.
                return Err(EgclError::ProgramError(format!(
                    "{name} requires at least one argument"
                )));
            }
            let want_min = name == "MIN";
            let mut best = args[0];
            numeric_cmp(best, best)?; // type-check the first argument
            for &a in &args[1..] {
                let ord = numeric_cmp(a, best)?;
                let take = if want_min {
                    ord == Ordering::Less
                } else {
                    ord == Ordering::Greater
                };
                if take {
                    best = a;
                }
            }
            Ok(best)
        }),

        "REM" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // REM: remainder of TRUNCATE (sign of the dividend). Exact for
            // rationals (bliss-05hy); f64 fallback for floats.

            if args.len() < 2 {
                return Err(EgclError::Internal("REM requires two arguments".into()));
            }
            if let Some(res) = exact_int_div(args[0], args[1], RoundMode::Truncate) {
                return Ok(res?.1);
            }
            let av = num_val(args[0])?;
            let bv = num_val(args[1])?;
            if bv == 0.0 {
                return Err(EgclError::ArithmeticError("division by zero".into()));
            }
            Ok(EgclVal::from_single_float((av % bv) as f32))
        }),

        "MOD" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // MOD: remainder of FLOOR (sign of the *divisor*, ANSI), so
            // (mod -7 3) = 2. Exact for rationals; f64 fallback for floats.

            if args.len() < 2 {
                return Err(EgclError::Internal("MOD requires two arguments".into()));
            }
            if let Some(res) = exact_int_div(args[0], args[1], RoundMode::Floor) {
                return Ok(res?.1);
            }
            let av = num_val(args[0])?;
            let bv = num_val(args[1])?;
            if bv == 0.0 {
                return Err(EgclError::ArithmeticError("division by zero".into()));
            }
            Ok(EgclVal::from_single_float(float_mod(av, bv) as f32))
        }),

        "TRUNCATE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() || args.len() > 2 {
                return Err(EgclError::ProgramError(
                    "TRUNCATE takes one or two arguments".into(),
                ));
            }
            eval_int_div(args[0], args.get(1).copied(), RoundMode::Truncate, env)
        }),

        "CEILING" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() || args.len() > 2 {
                return Err(EgclError::ProgramError(
                    "CEILING takes one or two arguments".into(),
                ));
            }
            eval_int_div(args[0], args.get(1).copied(), RoundMode::Ceiling, env)
        }),

        "ROUND" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() || args.len() > 2 {
                return Err(EgclError::ProgramError(
                    "ROUND takes one or two arguments".into(),
                ));
            }
            eval_int_div(args[0], args.get(1).copied(), RoundMode::Round, env)
        }),

        "ASH" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() != 2 {
                return Err(EgclError::ProgramError(
                    "ASH requires exactly two arguments".into(),
                ));
            }
            env.clear_mv();
            egcl_stdlib::numbers::ash(args[0], args[1])
        }),

        "LOGAND" | "LOGIOR" | "LOGXOR" => Some(|operator, args, _env| {
            let args = RootedVals::new(args.to_vec());
            let name = operator.to_owned();

            apply_logop(&name, &args)
        }),

        "LOGNOT" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() != 1 {
                return Err(EgclError::ProgramError(
                    "LOGNOT requires exactly one argument".into(),
                ));
            }
            apply_logop("LOGNOT", &args)
        }),

        "LOGBITP" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() != 2 {
                return Err(EgclError::ProgramError(
                    "LOGBITP requires exactly two arguments".into(),
                ));
            }
            apply_logbitp(args[0], args[1])
        }),

        "INTEGER-LENGTH" | "LOGCOUNT" => Some(|operator, args, _env| {
            let args = RootedVals::new(args.to_vec());
            let name = operator.to_owned();

            if args.len() != 1 {
                return Err(EgclError::ProgramError(format!(
                    "{name} requires exactly one argument"
                )));
            }
            apply_intlen_or_logcount(&name, args[0])
        }),

        "EXPT" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() != 2 {
                return Err(EgclError::ProgramError(
                    "EXPT requires exactly two arguments".into(),
                ));
            }
            let a = args[0];
            let b = args[1];
            // Exact result when the base is rational and the exponent is an
            // integer: an exact rational/bignum, promoting past i64 range.
            if !a.is_single_float() && b.is_fixnum() {
                if let Some(base) = as_bigrat(a) {
                    let e = b.as_fixnum();
                    if e >= 0 {
                        return Ok(bigrat_pow(&base, e as u64).to_val());
                    }
                    if base.num.is_zero() {
                        return Err(EgclError::ArithmeticError("division by zero".into()));
                    }
                    // negative exponent: reciprocal of base^|e|
                    let p = bigrat_pow(&base, e.unsigned_abs());
                    return Ok(bigrat_div(&BigRat::from_i64(1), &p).to_val());
                }
            }
            // A complex base with an integer exponent is exact repeated
            // multiplication (e.g. i^2 = -1, canonicalised to the real -1).
            if egcl_rt::types::complexp(a) && b.is_fixnum() {
                let e = b.as_fixnum();
                if e == 0 {
                    // (expt z 0) = 1, coerced to z's contagious float format:
                    // a float complex yields #c(1.0 0.0) (CLHS type rules),
                    // a rational complex the exact integer 1.
                    let rp = egcl_rt::types::complex_realpart(a).unwrap_or(a);
                    let ip = egcl_rt::types::complex_imagpart(a).unwrap_or(NIL);
                    // `float_kind_of`, NOT `real_float_kind`: the latter
                    // never returns None -- it answers Single for anything
                    // that is not a double -- so this test was always true
                    // and the rational-complex case below was UNREACHABLE.
                    // `(expt #C(3 3) 0)` answered #C(1.0 0.0) instead of the
                    // integer 1 the comment above promises (ansi EXPT.7).
                    let kind = widen_float(float_kind_of(rp), float_kind_of(ip));
                    if kind != FloatKind::None {
                        egcl_rt::rooted!(one = box_float(1.0, kind));
                        let zero = box_float(0.0, kind);
                        return make_complex(*one, zero);
                    }
                    return Ok(EgclVal::from_fixnum(1));
                }
                egcl_rt::rooted!(factors = vec![a; e.unsigned_abs() as usize]);
                egcl_rt::rooted!(pos = complex_arith(CxOp::Mul, &factors)?);
                if e < 0 {
                    // Negative exponent: reciprocal 1 / base^|e|.
                    return complex_arith(CxOp::Div, &[EgclVal::from_fixnum(1), *pos]);
                }
                return Ok(*pos);
            }
            // A complex base, or a negative real base with a non-integer
            // exponent, gives a complex result (real `powf` returns NaN for
            // the latter): base^power = exp(power · log base) (bliss-mg63 kin).
            let base_negative =
                !egcl_rt::types::complexp(a) && num_val(a).map(|x| x < 0.0).unwrap_or(false);
            // A negative real base with a ZERO float exponent must not come
            // here: CLHS makes (expt x 0) equal 1 of the result type, and
            // the real path below already gets that right via powf(x, 0.0).
            // Routing it to complex_expt answered #C(1.0 0.0) for
            // `(expt -5 0.0)` instead of 1.0 (ansi EXPT.18, whose loop runs
            // i from -1000 -- positive bases were already correct).
            // A complex base with an integer zero exponent is handled
            // above; with a FLOAT zero exponent, float contagion really
            // does want #C(1.0 0.0), so complex_expt stays correct there.
            let exponent_zero =
                !egcl_rt::types::complexp(b) && num_val(b).map(|x| x == 0.0).unwrap_or(false);
            // (expt 0 y) with (realpart y) > 0 is (* x y) -- ansi
            // EXPT.29 asserts exactly `(eql (* x y) (expt x y))` over every
            // zero and every such exponent. Deferring to the multiply
            // kernel makes the TYPE follow contagion for free: the integer
            // 0 stays the integer 0 (because #C(0 0) canonicalises) while
            // (expt 0.0 #C(2 2)) is #C(0.0 0.0). Computing it through
            // complex_expt instead answered a bare 0.0 for every zero.
            // "Zero" includes a COMPLEX zero: EXPT.29's bases are
            // 0, 0.0, 0.0d0 AND #C(0.0 0.0), #C(0.0d0 0.0d0). Excluding the
            // complex ones sent them to complex_expt, which answered a bare
            // 0.0 instead of #C(0.0 0.0).
            let base_is_zero = if let Some(re) = egcl_rt::types::complex_realpart(a) {
                let im = egcl_rt::types::complex_imagpart(a).unwrap_or(NIL);
                num_val(re).map(|x| x == 0.0).unwrap_or(false)
                    && num_val(im).map(|x| x == 0.0).unwrap_or(false)
            } else {
                num_val(a).map(|x| x == 0.0).unwrap_or(false)
            };
            let exp_real_positive = if let Some(re) = egcl_rt::types::complex_realpart(b) {
                num_val(re).map(|x| x > 0.0).unwrap_or(false)
            } else {
                num_val(b).map(|x| x > 0.0).unwrap_or(false)
            };
            if base_is_zero && exp_real_positive {
                if let Some(r) = apply_numeric_op("*", &[a, b]) {
                    return r;
                }
            }
            // A COMPLEX EXPONENT also needs the complex path -- `num_val`
            // below rejects it, so `(expt 0 #C(2 2))` and
            // `(expt 2.0 #C(2 2))` type-errored (ansi EXPT.29, which pairs
            // every zero with every base including complex ones).
            if egcl_rt::types::complexp(a)
                || egcl_rt::types::complexp(b)
                || (base_negative && !b.is_fixnum() && !exponent_zero)
            {
                return complex_expt(a, b);
            }
            let av = num_val(a)?;
            let bv = num_val(b)?;
            let kind = widen_float(real_float_kind(a), real_float_kind(b));
            // A nonzero base cannot raise to an exact zero, so a zero
            // result is underflow; finite operands cannot give a true
            // infinity, so that is overflow (CLHS 12.1.4.3).
            let pow = check_float_range(
                av.powf(bv),
                kind,
                av.is_finite() && bv.is_finite(),
                av != 0.0,
            )?;
            Ok(box_float(pow, kind))
        }),

        "SIGNAL" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                return Err(EgclError::Internal("SIGNAL requires an argument".into()));
            }
            egcl_rt::rooted!(datum = args[0]);
            let initargs = &args[1..];
            // (signal datum &rest args): a condition-type symbol is built into
            // an instance so handler type-matching runs against the real CLOS
            // class hierarchy; a format-control string becomes a
            // SIMPLE-CONDITION (CLHS 9.1; HANDLER-BIND.10, IGNORE-ERRORS.5/6).
            let cond = match coerce_condition_designator(env, *datum, initargs)? {
                Some(condition) => condition,
                None => make_simple_condition("SIMPLE-CONDITION", *datum, initargs, env)?,
            };
            signal_condition_object(cond, env)
        }),

        "WARN" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                // Missing required argument (CLHS 3.5.1; WARN.15).
                return Err(EgclError::ProgramError("WARN requires an argument".into()));
            }
            egcl_rt::rooted!(datum = args[0]);
            let rest_args = &args[1..];
            // (warn datum &rest args): a warning-type symbol or condition is
            // used directly; a format-control string becomes a SIMPLE-WARNING.
            let message = if is_string_value(*datum) {
                format_control_message(&val_as_str(*datum), rest_args)?
            } else {
                val_as_str(*datum)
            };
            let datum_is_instance = egcl_stdlib::is_instance(*datum);
            let condition = match coerce_condition_designator(env, *datum, rest_args)? {
                Some(condition) => condition,
                None => make_simple_warning_condition(*datum, rest_args, env)?,
            };
            egcl_rt::rooted!(cond = condition);
            // The effective condition must be of type WARNING, else a
            // TYPE-ERROR (CLHS WARN; WARN.12/13/16/17/18). Passing initargs
            // alongside an already-constructed condition is likewise invalid
            // (WARN.14).
            let warning_sym = resolve_sym("WARNING").unwrap_or(NIL);
            let is_warning = typep_matches(env, *cond, warning_sym)?;
            if !is_warning || (datum_is_instance && !rest_args.is_empty()) {
                return Err(EgclError::TypeError {
                    datum: *datum,
                    expected: "WARNING".into(),
                });
            }
            // Establish a MUFFLE-WARNING restart for the dynamic extent of the
            // signal so a handler can suppress the default warning message.
            let base_len = env.restarts.len();
            env.restarts.push(RestartEntry {
                captured_blocks: env.block_stack.clone(),
                captured_tags: env.tag_stack.clone(),
                name: "MUFFLE-WARNING".to_string(),
                function: RestartFunction::ContinueNil,
                interactive_function: None,
                test_function: None,
                unwind_on_invoke: true,
                group_base: base_len,
                id: next_restart_id(),
                restart_obj: NIL,
                report: NIL,
            });
            let result = signal_condition_object(*cond, env);
            env.restarts.truncate(base_len);
            match result {
                Ok(_) => {
                    // Unhandled (or handler declined): print the warning per
                    // R5.104 to *ERROR-OUTPUT* (WARN.4) and return NIL.
                    // Warnings never enter the debugger.
                    let text = condition_report_string(env, *cond).unwrap_or(message);
                    let stream = env.lookup_var("*ERROR-OUTPUT*").unwrap_or(NIL);
                    if !stream.is_nil() {
                        let _ = write_str_to(stream, &format!("WARNING: {}\n", text), env);
                    } else {
                        eprintln!("WARNING: {}", text);
                    }
                    Ok(NIL)
                }
                Err(error) => {
                    if restart_invoked_name(&error).as_deref() == Some("MUFFLE-WARNING") {
                        return Ok(NIL);
                    }
                    Err(error)
                }
            }
        }),

        "MAKE-CONDITION" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                // A missing required argument is a PROGRAM-ERROR (CLHS 3.5.1;
                // MAKE-CONDITION.ERROR.1).
                return Err(EgclError::ProgramError(
                    "MAKE-CONDITION requires a type".into(),
                ));
            }
            let type_val = args[0];
            let type_name = if type_val.is_symbol() {
                sym_name(type_val)
            } else {
                // A class metaobject (e.g. from FIND-CLASS) designates its
                // name (MAKE-CONDITION.2).
                let cname = egcl_stdlib::class_name(type_val);
                if !cname.is_nil() {
                    sym_name(cname)
                } else {
                    val_as_str(type_val)
                }
            };
            // Initarg key/value pairs (drop an odd trailing arg, as the
            // original loop did) are already evaluated and rooted in `args`.
            let pair_count = (args.len().saturating_sub(1)) & !1;
            let initarg_pairs = &args[1..1 + pair_count];
            build_condition_instance(env, &type_name, initarg_pairs)
        }),

        "SLOT-VALUE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            let instance = args.first().copied().unwrap_or(NIL);
            let slot = args.get(1).copied().unwrap_or(NIL);
            slot_value_or_signal(instance, slot, env)
        }),

        "SLOT-BOUNDP" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            let instance = args.first().copied().unwrap_or(NIL);
            let slot = args.get(1).copied().unwrap_or(NIL);
            Ok(if slot_is_bound(instance, slot, env)? {
                T
            } else {
                NIL
            })
        }),

        "ALLOCATE-INSTANCE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            let class_input = args.first().copied().unwrap_or(NIL);
            // Resolved the same way MAKE-INSTANCE resolves it, deliberately.
            // SBCL rejects `(allocate-instance 'foo)` because there a class
            // and its name are different objects — but EGCL's FIND-CLASS
            // RETURNS THE SYMBOL (classes are not yet distinct metaobjects),
            // so rejecting symbols here would reject the only thing a caller
            // can obtain. Tightening this is bliss-rj5o, and belongs with
            // real class metaobjects rather than here.
            let class = resolve_class_metaobject(env, class_input)?;
            egcl_stdlib::clos::allocate_instance(class)
        }),

        "FIND-METHOD" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());
            // (find-method generic-function qualifiers specializers &optional
            // errorp) → the method with those qualifiers and specializers, or
            // (when errorp, the default, is NIL) NIL, else signal an error
            // (CLHS 7.6.2; bliss-7y1s). `#'gf` on a generic function yields its
            // NAME symbol here, so accept a symbol designator or a GF object.

            if args.len() < 3 {
                return Err(EgclError::ProgramError(
                    "FIND-METHOD requires a generic function, qualifiers, and specializers".into(),
                ));
            }
            let gf = args[0];
            let want_qual = qualifiers_to_method_qualifier(args[1]);
            let spec_args = list_to_vec(args[2]);
            let errorp = args.get(3).map(|v| !v.is_nil()).unwrap_or(true);

            let gf_name = if gf.is_symbol() {
                Some(sym_name(gf))
            } else if let Some(n) = builtin_wrapper_name(gf)
                .filter(|n| env.generics.contains_key(n) || env.methods.contains_key(n))
            {
                // `#'gf` reifies a FUNCTIONP wrapper rather than the bare
                // name symbol; map it back to the generic it stands for.
                Some(n)
            } else {
                env.generics
                    .borrow()
                    .iter()
                    .find(|(_, d)| d.generic_function == gf)
                    .map(|(n, _)| n.clone())
            };
            let not_found = |env: &mut Env, msg: String| -> Result<EgclVal, EgclError> {
                if errorp {
                    let condition = make_simple_error_condition(arena_str(&msg), env)?;
                    return Err(signal_and_raise(env, condition, msg));
                }
                Ok(NIL)
            };
            let Some(gf_name) = gf_name else {
                return not_found(
                    env,
                    format!("FIND-METHOD: {} is not a generic function", format_val(gf)),
                );
            };
            // `want_qual == None` means a qualifier list egcl's standard
            // combination never produces — no method can match.
            if let Some(want_qual) = want_qual {
                let methods = env
                    .methods
                    .borrow()
                    .get(&gf_name)
                    .cloned()
                    .unwrap_or_default();
                for m in &methods {
                    if m.qualifier == want_qual
                        && m.specializers.len() == spec_args.len()
                        && m.specializers
                            .iter()
                            .zip(spec_args.iter())
                            .all(|(ms, arg)| method_specializer_matches_designator(ms, *arg))
                    {
                        return Ok(m.method_id);
                    }
                }
            }
            not_found(
                env,
                format!(
                    "FIND-METHOD: no method for {gf_name} with the given qualifiers and specializers"
                ),
            )
        }),

        "EGCL-INTERNAL::%STANDARD-SHARED-INITIALIZE"
        | "EGCL-INTERNAL:%STANDARD-SHARED-INITIALIZE" => Some(|_operator, args, env| {
            egcl_rt::rooted!(args = args.to_vec());
            if args.len() != 3 {
                return Err(EgclError::ProgramError(format!(
                    "EGCL-INTERNAL:%STANDARD-SHARED-INITIALIZE requires 3 arguments, got {}",
                    args.len()
                )));
            }
            let eligible = if args[1] == T {
                None
            } else {
                let (names, tail) = list_to_vec_with_tail(args[1]);
                if !tail.is_nil() || names.iter().any(|name| !name.is_symbol()) {
                    return Err(EgclError::ProgramError(
                        "SHARED-INITIALIZE slot names must be T or a list of symbols".into(),
                    ));
                }
                Some(
                    names.iter()
                        .map(|name| sym_bare_name_rc(*name).to_string())
                        .collect::<Vec<_>>(),
                )
            };
            let class_name = class_name_for_instance_class(egcl_stdlib::class_of(args[0]));
            egcl_rt::rooted!(raw_initargs = list_to_vec(args[2]));
            egcl_rt::rooted!(initargs = resolved_initarg_values(&class_name, &raw_initargs, env)?);
            // Explicit initargs apply regardless of SLOT-NAMES. That argument
            // only selects unbound slots whose initforms may run afterwards.
            reinitialize_instance_values(args[0], &initargs, env)?;
            let explicit_slots = initargs.chunks_exact(2)
                .map(|pair| sym_bare_name_rc(pair[0]).to_string())
                .collect::<Vec<_>>();
            apply_class_initforms(
                args[0], &class_name, env, eligible.as_deref(), &explicit_slots,
            )?;
            Ok(args[0])
        }),

        "EGCL-INTERNAL::%STANDARD-REINITIALIZE-INSTANCE"
        | "EGCL-INTERNAL:%STANDARD-REINITIALIZE-INSTANCE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() != 2 {
                return Err(EgclError::ProgramError(format!(
                    "EGCL-INTERNAL:%STANDARD-REINITIALIZE-INSTANCE requires 2 arguments, got {}",
                    args.len()
                )));
            }
            let instance = args[0];
            let raw_initargs = list_to_vec(args[1]);
            let class_name = class_name_for_instance_class(egcl_stdlib::class_of(instance));
            let initargs = resolved_initarg_values(&class_name, &raw_initargs, env)?;
            reinitialize_instance_values(instance, &initargs, env)
        }),

        "PRINT-OBJECT" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // The system PRINT-OBJECT (CLHS 22.1.3): print OBJECT to STREAM
            // honouring *PRINT-ESCAPE*. A condition with a DEFINE-CONDITION
            // :report prints via it when escape is NIL (CONDITION-16/17/18).

            egcl_rt::rooted!(obj = args.first().copied().unwrap_or(NIL));
            egcl_rt::rooted!(stream = args.get(1).copied().unwrap_or(NIL));
            let escape = env
                .lookup_var("*PRINT-ESCAPE*")
                .map(|v| !v.is_nil())
                .unwrap_or(true);
            if !escape && print_condition_defined_report(*obj, *stream, env)? {
                return Ok(*obj);
            }
            let prev_env = PRINT_ENV.with(|c| c.replace(env as *mut Env));
            let control = if escape { "~S" } else { "~A" };
            let result = egcl_stdlib::format(*stream, control, &[*obj]);
            PRINT_ENV.with(|c| c.set(prev_env));
            result?;
            Ok(*obj)
        }),
        #[cfg(not(egcl_no_dynamic_code))]
        "LOAD" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                return Err(EgclError::Internal("LOAD requires a pathname".into()));
            }
            let mut path_val = args[0];
            egcl_rt::rooted_ref!(_path_root = &mut path_val);
            // :if-does-not-exist nil → return NIL for a missing file instead
            // of erroring (CLHS; slynk's load-user-init-file relies on this).
            let mut if_missing_nil = false;
            let mut i = 1;
            while i + 1 < args.len() {
                if args[i].is_symbol() && sym_bare_name_rc(args[i]).as_ref() == "IF-DOES-NOT-EXIST"
                {
                    if_missing_nil = args[i + 1] == NIL;
                }
                i += 2;
            }
            // LOAD also accepts an open input STREAM (CLHS): read and evaluate
            // every form from it. A SLY/SLIME client injects its runtime this
            // way — `(with-input-from-string (s …) (load s))`.
            if is_stream(path_val) {
                let mut contents = String::new();
                loop {
                    let ch = if is_gray_stream(path_val) {
                        invoke_generic_function("STREAM-READ-CHAR", &[path_val], env)?
                    } else {
                        egcl_stdlib::stream_read_char(path_val)?
                    };
                    if ch == EOF || !ch.is_character() {
                        break;
                    }
                    contents.push(ch.as_char());
                }
                // ANSI LOAD binds *PACKAGE* for the dynamic extent of the
                // load, and a STREAM load is still a load. LOAD_PATH_INTO_ENV
                // does this for the file case; this branch did not, so
                // `(with-input-from-string (s "(in-package :foo)") (load s))`
                // left the CALLER in FOO.
                //
                // That is the path a SLIME client injects its runtime through,
                // so connecting icl to EGCL left the REPL in ICL-RUNTIME
                // before the user had typed anything — the prompt said
                // ICL-RUNTIME> and every bare symbol read there, which is a
                // wrong-answer bug and not just a cosmetic one.
                //
                // CURRENT_PACKAGE as well as the value cell: it is the reader's
                // bare-symbol resolution context and what the prompt shows, and
                // the two are kept in step deliberately (bliss-lb6.12).
                let saved_package = env.current_package.clone();
                let result = read_eval_all_env(&contents, env);
                if env.current_package != saved_package {
                    env.current_package = saved_package.clone();
                    env.define_local("*PACKAGE*", package_object(&saved_package));
                    sync_package_value_cell(&saved_package);
                }
                result?;
                return Ok(T);
            }
            // LOAD accepts a pathname designator — a namestring OR a pathname
            // object (e.g. `#P"…"`, common in a ~/.egclrc). `val_as_str` on a
            // pathname yields its debug repr, so coerce via its namestring.
            let mut path = path_designator_to_string(path_val)?;
            // A relative LOAD pathname resolves against *DEFAULT-PATHNAME-
            // DEFAULTS* (CLHS): merge PATH with its directory when PATH is not
            // absolute. Read the value cell first so a LET/binding of the
            // variable is honored — ansi-test cons/load.lsp binds it to the
            // chapter directory and then `(load "cons.lsp")` (bliss-cpm9). Merge
            // through pathname objects so directory semantics are correct; the
            // default ("./") leaves CWD-relative behaviour unchanged.
            if !std::path::Path::new(&path).is_absolute() {
                let dpd = resolve_sym("*DEFAULT-PATHNAME-DEFAULTS*")
                    .and_then(|s| global_value_cell(s.as_symbol_index()))
                    .or_else(|| env.lookup_var("*DEFAULT-PATHNAME-DEFAULTS*"));
                if let Some(v) = dpd {
                    if !v.is_nil() {
                        if let (Ok(pn), Ok(dflt)) = (
                            coerce_pathname_designator(path_val),
                            coerce_pathname_designator(v),
                        ) {
                            if let Ok(merged) = egcl_stdlib::merge_pathnames(pn, dflt, NIL) {
                                if let Ok(s) = path_designator_to_string(merged) {
                                    if std::path::Path::new(&s).is_absolute() {
                                        path = s;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if if_missing_nil && !std::path::Path::new(&path).exists() {
                return Ok(NIL);
            }
            // ANSI LOAD returns a generalized boolean (T on success); the
            // last top-level form's value is not the result.
            load_path_into_env(&path, env)?;
            Ok(T)
        }),
        #[cfg(not(egcl_no_dynamic_code))]
        "COMPILE-FILE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());
            // (compile-file source &key output-file &allow-other-keys) — compile
            // SOURCE to a .bfasl (bliss-lb6.6). Returns three values per ANSI:
            // output-truename, warnings-p, failure-p. ASDF passes the target as
            // an :output-file keyword (not positional).

            if args.is_empty() {
                return Err(EgclError::Internal(
                    "COMPILE-FILE requires a source".into(),
                ));
            }
            egcl_rt::rooted!(source_pathname = resolve_against_dpd(args[0], env)?);
            let src_path = path_designator_to_string(*source_pathname)?;
            let mut out_path: Option<String> = None;
            let mut i = 1;
            // Extension: accept `(compile-file src out)` with a positional
            // output file (string/pathname). ANSI makes OUTPUT-FILE a
            // keyword, but the positional form is what callers reach for,
            // and it was previously ACCEPTED AND SILENTLY IGNORED — the
            // compile overwrote the default output path (bliss-m1a).
            if i < args.len() && !args[i].is_symbol() {
                out_path = Some(path_designator_to_string(args[i])?);
                i += 1;
            }
            // The remaining tail must be a well-formed &key list (output-file
            // verbose print external-format …, other keys tolerated) —
            // reject malformed tails instead of dropping arguments.
            if (args.len() - i) % 2 != 0 {
                return Err(EgclError::ProgramError(
                    "COMPILE-FILE: odd number of &KEY arguments".into(),
                ));
            }
            while i + 1 < args.len() {
                let key = args[i];
                let val = args[i + 1];
                if !key.is_symbol() {
                    return Err(EgclError::ProgramError(format!(
                        "COMPILE-FILE: {} is not a keyword argument name",
                        format_val(key)
                    )));
                }
                if sym_bare_name_rc(key).as_ref() == "OUTPUT-FILE" && val != NIL {
                    out_path = Some(path_designator_to_string(val)?);
                }
                i += 2;
            }
            let out_path = out_path.unwrap_or_else(|| {
                // CLHS 3.2.3: COMPILE-FILE's default output MUST equal
                // (COMPILE-FILE-PATHNAME input). Match COMPILE-FILE-PATHNAME
                // below exactly — strip `.lisp` OR `.lsp`, append `.fasl` —
                // or `compile-and-load` (ansi-test, which compiles a `.lsp`
                // then LOADs compile-file-pathname's result) fails to find
                // the artifact (bliss-30be).
                let stem = src_path
                    .strip_suffix(".lisp")
                    .or_else(|| src_path.strip_suffix(".lsp"))
                    .unwrap_or(&src_path);
                format!("{stem}.fasl")
            });
            let source = std::fs::read_to_string(&src_path).map_err(|e| {
                EgclError::FileError(format!("compile-file: cannot read {src_path}: {e}"))
            })?;
            // Per-file progress, gated on *COMPILE-VERBOSE* (default T, like
            // SBCL). Only fires when compilation actually happens — ASDF
            // calls COMPILE-FILE only for a missing/stale fasl — so warm
            // loads stay silent.
            let compile_verbose = env
                .lookup_var("*COMPILE-VERBOSE*")
                .map(|v| !v.is_nil())
                .unwrap_or(true);
            let compile_start = std::time::Instant::now();
            if compile_verbose {
                let written = std::fs::metadata(&src_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| format!(" (written {})", format_compile_note_date(d.as_secs())))
                    .unwrap_or_default();
                println!("; compiling file \"{src_path}\"{written}:");
            }
            // ANSI COMPILE-FILE binds *PACKAGE* (and *READTABLE*) for the
            // dynamic extent of the compilation (CLHS 3.2.1), so a file's
            // IN-PACKAGE forms don't leak into the caller — after
            // `(compile-file "lib/asdf.lisp")` the REPL stays in CL-USER, not
            // ASDF/FOOTER. Snapshot the current package and restore it once
            // compilation is done (mirrors LOAD in load_path_into_env).
            let saved_package = env.current_package.clone();
            let image = {
                // Bind BEFORE reading: #.*COMPILE-FILE-PATHNAME* must
                // capture the source, even when ASDF redirects the output.
                // Keep its spelling distinct from the canonical truename.
                egcl_rt::rooted!(source_truename = egcl_stdlib::truename(*source_pathname)?);
                let pathname_symbol = resolve_sym("*COMPILE-FILE-PATHNAME*").unwrap();
                let truename_symbol = resolve_sym("*COMPILE-FILE-TRUENAME*").unwrap();
                // Saved caller values also need roots across nested
                // compilation/GC. Dropping the guards restores on errors.
                egcl_rt::rooted!(
                    _compile_paths = vec![
                        DynBind::establish(pathname_symbol, *source_pathname),
                        DynBind::establish(truename_symbol, *source_truename),
                    ]
                );
                build_bfasl_from_source(&source, &src_path, env)
            };
            if env.current_package != saved_package {
                env.current_package = saved_package.clone();
                env.define_local("*PACKAGE*", package_object(&saved_package));
                sync_package_value_cell(&saved_package);
            }
            let image = image?;
            // Create the output directory if needed. ASDF's output-translations
            // route fasls into a per-implementation cache tree whose directories
            // may not exist yet; real CL relies on ASDF pre-creating them, but
            // creating them here is harmless and avoids a spurious file error.
            if let Some(parent) = std::path::Path::new(&out_path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Write the fasl ATOMICALLY (temp file in the same directory +
            // rename), so an interrupted or failed compile never leaves a
            // half-written .bfasl that a later LOAD would choke on ("HeapObj
            // not of type sequence") instead of recompiling (bliss-c3i).
            write_file_atomic(&out_path, &image).map_err(|e| {
                EgclError::FileError(format!("compile-file: cannot write {out_path}: {e}"))
            })?;
            if compile_verbose {
                let el = compile_start.elapsed();
                let secs = el.as_secs();
                println!("; wrote {out_path}");
                println!(
                    "; compilation finished in {}:{:02}:{:02}.{:03}",
                    secs / 3600,
                    (secs % 3600) / 60,
                    secs % 60,
                    el.subsec_millis()
                );
            }
            let (out_pn, _) = egcl_stdlib::parse_namestring(arena_str(&out_path), None, None)?;
            env.set_mv(vec![out_pn, NIL, NIL]);
            Ok(out_pn)
        }),

        "COMPILE-FILE-PATHNAME" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            // (compile-file-pathname input-file &key output-file &allow-other-keys)
            // Return the pathname COMPILE-FILE would write. With an explicit
            // :output-file, return that (as a pathname); otherwise the input
            // with a "fasl" type. ASDF calls this to compute output-files.

            if args.is_empty() {
                return Err(EgclError::Internal(
                    "compile-file-pathname: missing input-file".into(),
                ));
            }
            // Scan &key args for :output-file.
            let mut i = 1;
            while i + 1 < args.len() {
                let key = args[i];
                let val = args[i + 1];
                if key.is_symbol() && sym_bare_name_rc(key).as_ref() == "OUTPUT-FILE" && val != NIL
                {
                    let (pn, _) = egcl_stdlib::parse_namestring(
                        arena_str(&path_designator_to_string(val)?),
                        None,
                        None,
                    )?;
                    return Ok(pn);
                }
                i += 2;
            }
            let src = path_designator_to_string(args[0])?;
            let stem = src
                .strip_suffix(".lisp")
                .or_else(|| src.strip_suffix(".lsp"))
                .unwrap_or(&src);
            let (pn, _) =
                egcl_stdlib::parse_namestring(arena_str(&format!("{stem}.fasl")), None, None)?;
            Ok(pn)
        }),

        "EGCL-EXT:DECLARATION-SPECIFIER" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            macroexpand_declaration_specifier(args[0], args[1])
        }),

        "EGCL-EXT:DECLARATION-SPECIFIERS" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            macroexpand_declaration_specifiers(args[0], args[1])
        }),

        "EGCL-EXT:PROCLAIMED-DECLARATIONS" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if !args.is_empty() {
                return Err(EgclError::ProgramError(format!(
                    "EGCL-EXT:PROCLAIMED-DECLARATIONS requires no arguments; got {}",
                    args.len()
                )));
            }
            let mut list = NIL;
            egcl_rt::rooted_ref!(_list_root = &mut list);
            for name in proclaimed_declaration_names() {
                list = arena_cons(EgclVal::from_symbol_index(name), list);
            }
            Ok(list)
        }),

        "EGCL-EXT:HASH-TABLE-WEAKNESS" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());
            // Trivial-Garbage's HASH-TABLE-WEAKNESS reads this; the keyword
            // spelling matches what MAKE-HASH-TABLE's :WEAKNESS accepts.

            if args.len() != 1 {
                return Err(EgclError::ProgramError(format!(
                    "EGCL-EXT:HASH-TABLE-WEAKNESS requires one argument; got {}",
                    args.len()
                )));
            }
            let name = match egcl_stdlib::hash_table_weakness(args[0])? {
                None => return Ok(NIL),
                Some(egcl_stdlib::Weakness::Key) => "KEY",
                Some(egcl_stdlib::Weakness::Value) => "VALUE",
                Some(egcl_stdlib::Weakness::KeyAndValue) => "KEY-AND-VALUE",
            };
            // Keywords are interned under "KEYWORD:NAME" registry keys.
            Ok(EgclVal::from_symbol_index(egcl_rt::symbols::intern(
                &format!("KEYWORD:{name}"),
            )))
        }),

        "EGCL-EXT:PROCLAIMED-OPTIMIZE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if !args.is_empty() {
                return Err(EgclError::ProgramError(format!(
                    "EGCL-EXT:PROCLAIMED-OPTIMIZE requires no arguments; got {}",
                    args.len()
                )));
            }
            Ok(proclaimed_optimize_list())
        }),

        "EGCL-EXT:FINALIZE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() != 2 && args.len() != 4 {
                return Err(EgclError::ProgramError(format!(
                    "EGCL-EXT:FINALIZE requires object, function, and optional :DONT-SAVE value; got {} arguments",
                    args.len()
                )));
            }
            if args.len() == 4
                && (!args[2].is_symbol() || sym_bare_name_rc(args[2]).as_ref() != "DONT-SAVE")
            {
                return Err(EgclError::ProgramError(
                    "EGCL-EXT:FINALIZE only accepts the :DONT-SAVE keyword".into(),
                ));
            }
            if !is_function_value(args[1]) {
                return Err(EgclError::TypeError {
                    datum: args[1],
                    expected: "FUNCTION".into(),
                });
            }
            let key = egcl_rt::finalizer_key(args[0])?;
            egcl_rt::register_deferred_finalizer(key, args[1])?;
            Ok(args[0])
        }),

        "EGCL-EXT:CANCEL-FINALIZATION" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() != 1 {
                return Err(EgclError::ProgramError(format!(
                    "EGCL-EXT:CANCEL-FINALIZATION requires one argument; got {}",
                    args.len()
                )));
            }
            let key = egcl_rt::finalizer_key(args[0])?;
            egcl_rt::cancel_deferred_finalizers(key);
            Ok(NIL)
        }),

        "EGCL-EXT:GC" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() % 2 != 0 {
                return Err(EgclError::ProgramError(
                    "EGCL-EXT:GC keyword arguments must be paired".into(),
                ));
            }
            let mut full = false;
            let mut i = 0;
            while i < args.len() {
                if !args[i].is_symbol() {
                    return Err(EgclError::ProgramError(
                        "EGCL-EXT:GC expected a keyword argument".into(),
                    ));
                }
                match sym_bare_name_rc(args[i]).as_ref() {
                    "FULL" => full = !args[i + 1].is_nil(),
                    "VERBOSE" => {}
                    other => {
                        return Err(EgclError::ProgramError(format!(
                            "EGCL-EXT:GC does not accept :{other}"
                        )));
                    }
                }
                i += 2;
            }
            if full {
                egcl_rt::full_gc()?;
            } else {
                egcl_rt::collect_t0_minor()?;
            }
            run_deferred_lisp_finalizers(env);
            Ok(NIL)
        }),

        "EGCL-EXT:SPROF-START" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            let hz = args
                .first()
                .filter(|v| v.is_fixnum())
                .map(|v| v.as_fixnum() as u32)
                .unwrap_or(1000);
            sprof::start(hz);
            Ok(T)
        }),

        "EGCL-EXT:PROFILE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            for &d in args.iter() {
                if let Some(idx) = profiled_index_of(d) {
                    // Pin to T0 so counts are exact (native prologues stop
                    // bumping the tiering counter), then snapshot the baseline.
                    bytecode::profile_pin(idx);
                    let f = egcl_rt::symbols::symbol_function(idx);
                    let (ic, bc) = f
                        .map(|f| {
                            (
                                egcl_rt::function::invoke_count(f),
                                egcl_rt::function::back_edge_count(f),
                            )
                        })
                        .unwrap_or((0, 0));
                    PROFILED_FNS.with(|m| m.borrow_mut().insert(idx, (ic, bc)));
                }
            }
            Ok(NIL)
        }),

        "EGCL-EXT:UNPROFILE" => Some(|_operator, args, _env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                PROFILED_FNS.with(|m| {
                    for idx in m.borrow().keys().copied().collect::<Vec<_>>() {
                        bytecode::profile_unpin(idx);
                    }
                    m.borrow_mut().clear();
                });
            } else {
                for &d in args.iter() {
                    if let Some(idx) = profiled_index_of(d) {
                        bytecode::profile_unpin(idx);
                        PROFILED_FNS.with(|m| m.borrow_mut().remove(&idx));
                    }
                }
            }
            Ok(NIL)
        }),

        "READ-LINE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (read-line &optional stream eof-error-p eof-value recursive-p)
            // Mirror READ-CHAR's eof handling (bliss-49qk): eof-error-p and
            // eof-value were previously ignored — the stream arg was the only
            // one evaluated and EOF always returned NIL. Per CLHS, at EOF with
            // nothing read, signal END-OF-FILE unless eof-error-p is NIL, in
            // which case return eof-value (with a true second value).

            let stream = if !args.is_empty() { args[0] } else { NIL };
            let eof_error_p = if args.len() > 1 { args[1] } else { T };
            let inp = resolve_input_stream(stream, env);
            if is_gray_stream(inp) {
                // The Gray stream-read-line returns (values string eof-p);
                // invoke_generic_function yields the primary value (the line).
                let line = invoke_generic_function("STREAM-READ-LINE", &[inp], env)?;
                return Ok(line);
            }
            let (line_val, missing_newline) = egcl_stdlib::stream_read_line(inp)?;
            if line_val == EOF {
                if eof_error_p.is_nil() {
                    // Re-read eof-value from the rooted args after the
                    // (allocating) read (bliss-6b2 #2). Second value is T at
                    // EOF, matching read-line's missing-newline contract.
                    let eof_value = if args.len() > 2 { args[2] } else { NIL };
                    env.set_mv(vec![eof_value, T]);
                    return Ok(eof_value);
                }
                return Err(EgclError::StreamError("end of file on READ-LINE".into()));
            }
            env.set_mv(vec![line_val, if missing_newline { T } else { NIL }]);
            Ok(line_val)
        }),

        "WRITE-STRING" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (write-string string &optional stream &key start end)

            if args.is_empty() {
                return Err(EgclError::Internal(
                    "WRITE-STRING requires an argument".into(),
                ));
            }
            let full = val_as_str(args[0]);
            // A second positional argument is the stream designator, unless
            // it is a keyword (the start of &key start/end options).
            let (stream, kv_from) = if args.len() > 1 && !is_keyword_arg(args[1]) {
                (args[1], 2)
            } else {
                (NIL, 1)
            };
            // Honor &key start/end: write only the substring (cl-ppcre's
            // regex-replace stitches output with `(write-string s :start :end)`
            // and produced garbage when these were ignored).
            let chars: Vec<char> = full.chars().collect();
            let (kstart, kend) = read_start_end_keys(&args[kv_from..], chars.len());
            let s: String = chars[kstart..kend].iter().collect();
            let mut out = resolve_output_stream(stream, env);
            egcl_rt::rooted_ref!(_output_root = &mut out);
            check_pending_sigpipe_for_output()?;
            if is_gray_stream(out) {
                // Dispatch to the Gray stream-write-string generic (start 0,
                // end nil → whole string), passing the already-bounded slice.
                egcl_rt::rooted!(text = arena_str(&s));
                invoke_generic_function(
                    "STREAM-WRITE-STRING",
                    &[out, *text, EgclVal::from_fixnum(0), NIL],
                    env,
                )?;
                return Ok(args[0]);
            }
            write_str_to(out, &s, env)?;
            Ok(args[0])
        }),

        "WRITE-LINE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (write-line string &optional stream &key start end) — WRITE-STRING
            // followed by a newline; returns the string.

            if args.is_empty() {
                return Err(EgclError::Internal(
                    "WRITE-LINE requires an argument".into(),
                ));
            }
            let full = val_as_str(args[0]);
            let (stream, kv_from) = if args.len() > 1 && !is_keyword_arg(args[1]) {
                (args[1], 2)
            } else {
                (NIL, 1)
            };
            let chars: Vec<char> = full.chars().collect();
            let (kstart, kend) = read_start_end_keys(&args[kv_from..], chars.len());
            let s: String = chars[kstart..kend].iter().collect();
            let mut out = resolve_output_stream(stream, env);
            egcl_rt::rooted_ref!(_output_root = &mut out);
            check_pending_sigpipe_for_output()?;
            if is_gray_stream(out) {
                egcl_rt::rooted!(text = arena_str(&s));
                invoke_generic_function(
                    "STREAM-WRITE-STRING",
                    &[out, *text, EgclVal::from_fixnum(0), NIL],
                    env,
                )?;
                invoke_generic_function("STREAM-TERPRI", &[out], env)?;
                return Ok(args[0]);
            }
            write_str_to(out, &s, env)?;
            egcl_stdlib::stream_terpri(out)?;
            Ok(args[0])
        }),

        "MAKE-PACKAGE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                return Err(EgclError::ProgramError(
                    "MAKE-PACKAGE requires a package name argument".into(),
                ));
            }
            // Validate the keyword tail (CLHS 3.5.1): pairs must be complete,
            // keys must be symbols, and only :NICKNAMES / :USE are accepted
            // unless :ALLOW-OTHER-KEYS is true. ansi-test MAKE-PACKAGE.ERROR.*.
            validate_builtin_keywords(&args[1..], &["NICKNAMES", "USE"])?;
            let pkg_name = normalize_package_name(&string_designator_name(args[0]));
            // MAKE-PACKAGE on a name (or nickname) that already names a
            // package is a PACKAGE-ERROR (CLHS) — unlike DEFPACKAGE, which
            // redefines in place.
            if egcl_stdlib::find_package(&pkg_name).is_some()
                || matches!(
                    pkg_name.as_str(),
                    "COMMON-LISP" | "COMMON-LISP-USER" | "KEYWORD"
                )
            {
                // Correctable: CONTINUE yields the EXISTING package, which
                // is the only correction that leaves the image consistent.
                let existing = package_object(&pkg_name);
                if signal_correctable_package_error(
                    existing,
                    &format!("a package named {pkg_name} already exists"),
                    env,
                )? {
                    return Ok(package_object(&pkg_name));
                }
                unreachable!("signal_correctable_package_error returns Err unless continued");
            }
            // Parse :nicknames and :use keyword options.
            let mut nicknames = Vec::new();
            let mut uses: Vec<String> = Vec::new();
            let mut i = 1;
            while i + 1 < args.len() {
                let key = sym_bare_name_rc(args[i]);
                let value = args[i + 1];
                match key.as_ref() {
                    "NICKNAMES" => {
                        for nick in list_to_vec(value) {
                            nicknames.push(normalize_package_name(&string_designator_name(nick)));
                        }
                    }
                    "USE" => {
                        for used in list_to_vec(value) {
                            uses.push(resolve_package_name(env, &string_designator_name(used)));
                        }
                    }
                    _ => {}
                }
                i += 2;
            }
            let use_refs: Vec<&str> = uses.iter().map(String::as_str).collect();
            ensure_package_available(env, &pkg_name, &use_refs);
            if !nicknames.is_empty() {
                if let Some(pkg) = egcl_stdlib::find_package(&pkg_name) {
                    for nick in nicknames {
                        // Register the nickname with the reader too, so a
                        // package-qualified symbol written with the nickname
                        // (e.g. `uiop:foo`, UIOP being a nickname of
                        // UIOP/DRIVER) resolves at read time (bliss-lb6).
                        reader::register_package(&nick);
                        let _ = egcl_stdlib::add_nickname(pkg, &nick);
                    }
                }
            }
            Ok(package_object(&pkg_name))
        }),

        "APROPOS-LIST" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (apropos-list string &optional package) → matching symbols.

            let needle = val_as_str(args.first().copied().unwrap_or(NIL));
            let package = args.get(1).copied().unwrap_or(NIL);
            let matches = apropos_symbols(env, &needle, package);
            Ok(vec_to_list(&matches))
        }),

        "APROPOS" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            // (apropos string &optional package) — print each match to
            // *standard-output*; CLHS specifies no values are returned.

            let needle = val_as_str(args.first().copied().unwrap_or(NIL));
            let package = args.get(1).copied().unwrap_or(NIL);
            let matches = apropos_symbols(env, &needle, package);
            // Render everything BEFORE writing: `symbol_function`/variable
            // lookups below allocate, and the stream write can re-enter Lisp
            // through a Gray stream. Neither may run while we hold a
            // half-built view of the symbol list.
            let mut report = String::new();
            for sym in &matches {
                let full = sym_name_rc(*sym);
                let home = symbol_home_package_name(*sym);
                let bare = symbol_name_string(&full);
                // An uninterned symbol has no home package; CLHS prints
                // those as #:NAME.
                let mut line = if home.is_empty() {
                    format!("#:{bare}")
                } else {
                    format!("{}:{}", prompt_package_name(&home), bare)
                };
                if callable_body_of_symbol(env, *sym, &symbol_name_string(&full)).is_some() {
                    line.push_str(" (function)");
                }
                if env.lookup_var_symbol(*sym).is_some() {
                    line.push_str(" (value)");
                }
                line.push('\n');
                report.push_str(&line);
            }
            write_standard_output(env, &report)?;
            env.set_mv(Vec::new());
            Ok(NIL)
        }),

        "EGCL-INTERNAL::PACKAGE-SYMBOLS" | "EGCL-INTERNAL:PACKAGE-SYMBOLS" => {
            Some(|_operator, args, env| {
                let args = RootedVals::new(args.to_vec());

                let package = if args.is_empty() {
                    effective_package_name(env)
                } else {
                    normalize_package_name(&string_designator_name(args[0]))
                };
                // 2nd arg: NIL → present symbols, :EXTERNAL → external symbols
                // only (DO-EXTERNAL-SYMBOLS), any other true value → accessible
                // (present + inherited) symbols.
                let mode = args.get(1).copied().unwrap_or(NIL);
                if mode.is_symbol() && sym_bare_name_rc(mode).as_ref() == "EXTERNAL" {
                    return Ok(vec_to_list(&package_external_symbols(env, &package)));
                }
                Ok(vec_to_list(&package_symbols(env, &package, !mode.is_nil())))
            })
        }

        "USE-PACKAGE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                return Ok(T);
            }
            let mut names = Vec::new();
            let pkgs_val = args[0];
            if pkgs_val.is_cons() {
                for pkg in list_to_vec(pkgs_val) {
                    names.push(resolve_package_name(env, &string_designator_name(pkg)));
                }
            } else {
                names.push(resolve_package_name(env, &string_designator_name(pkgs_val)));
            }
            let target = if args.len() > 1 {
                let raw = string_designator_name(args[1]);
                resolve_package_name(env, &raw)
            } else {
                effective_package_name(env)
            };
            // ensure_package_available creates the target if needed and adds
            // each named package to its use-list (creating a placeholder for
            // any not yet defined).
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            ensure_package_available(env, &target, &name_refs);
            Ok(T)
        }),

        "UNUSE-PACKAGE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                return Ok(T);
            }
            // First arg is a package designator or a list of them.
            let pkgs_val = args[0];
            let mut used = Vec::new();
            let names: Vec<EgclVal> = if pkgs_val.is_cons() {
                list_to_vec(pkgs_val)
            } else if pkgs_val.is_nil() {
                Vec::new()
            } else {
                vec![pkgs_val]
            };
            for pv in names {
                let name = resolve_package_name(env, &string_designator_name(pv));
                if let Some(p) = egcl_stdlib::find_package(&name) {
                    used.push(p);
                }
            }
            let target = if args.len() > 1 {
                let raw = string_designator_name(args[1]);
                resolve_package_name(env, &raw)
            } else {
                effective_package_name(env)
            };
            if let Some(target_pkg) = egcl_stdlib::find_package(&target) {
                let _ = egcl_stdlib::unuse_package(&used, target_pkg);
            }
            Ok(T)
        }),

        "RENAME-PACKAGE" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.len() < 2 {
                return Err(EgclError::Internal(
                    "RENAME-PACKAGE requires package and new name".into(),
                ));
            }
            let old_raw = string_designator_name(args[0]);
            let old_name = resolve_package_name(env, &old_raw);
            let new_name = normalize_package_name(&string_designator_name(args[1]));
            // Optional new nicknames (3rd arg); default: keep the package's
            // current nicknames (egcl has always preserved them on rename).
            if let Some(pkg) = egcl_stdlib::find_package(&old_name) {
                let new_nicks: Vec<String> = if args.len() > 2 {
                    list_to_vec(args[2])
                        .iter()
                        .map(|n| normalize_package_name(&string_designator_name(*n)))
                        .collect()
                } else {
                    egcl_stdlib::package_nicknames(pkg)
                };
                let nick_refs: Vec<&str> = new_nicks.iter().map(String::as_str).collect();
                let _ = egcl_stdlib::rename_package(pkg, &new_name, &nick_refs);
                for nick in &new_nicks {
                    reader::register_package(nick);
                }
                // Rewrite the old qualifier baked into affected symbols'
                // registry keys and re-key the interpreter's name-keyed
                // definition maps, so SYMBOL-PACKAGE, printing, and
                // function/macro lookup all follow the rename (bliss-9fi3).
                if old_name != new_name {
                    let renamed = egcl_rt::symbols::rename_package_prefix(&old_name, &new_name);
                    rekey_renamed_symbols(env, &renamed);
                }
            }
            reader::register_package(&new_name);
            Ok(package_object(&new_name))
        }),

        "INTERN" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            let name_val = args.first().copied().unwrap_or(NIL);
            // INTERN's first argument is a string, not reader input: a
            // colon in it is an ordinary symbol-name character and must
            // not be interpreted as a package marker.
            let name_str = val_as_str(name_val);
            let pkg_name = if args.len() > 1 {
                let raw = string_designator_name(args[1]);
                resolve_package_name(env, &raw)
            } else {
                effective_package_name(env)
            };
            // KEYWORD is the one package whose symbols do NOT live in a
            // package's symbol tables: they are keyed `KEYWORD:<name>` in
            // the shared symbol registry, which is where the reader mints
            // them and where FIND-SYMBOL looks them up
            // (`present_symbol_with_status`). Interning one through the
            // package registry minted a SECOND symbol of the same name that
            // was not EQ to the reader's — so cl-json's decoded keys did not
            // match the `:message` / `:content` literals its callers write,
            // and completions read every Ollama reply as NIL (bliss-r8kt).
            // The registry key is the VERBATIM name: INTERN is
            // case-sensitive, so `(intern "abc" :keyword)` must name `:|abc|`.
            if pkg_name == "KEYWORD" {
                let key = format!("KEYWORD:{name_str}");
                let existed = reader::find_symbol_index(&key).is_some();
                let sym = EgclVal::from_symbol_index(reader::intern_symbol(&key));
                let status = if existed {
                    package_status_symbol("EXTERNAL")
                } else {
                    NIL
                };
                env.set_mv(vec![sym, status]);
                return Ok(sym);
            }
            // Package behavior belongs to egcl-stdlib. Ensure the package
            // exists, then let its registry perform the exact-case lookup,
            // inherited-symbol handling, allocation, and insertion.
            ensure_package_available(env, &pkg_name, &[]);
            let package = egcl_stdlib::find_package(&pkg_name).ok_or_else(|| {
                EgclError::PackageError(format!("there is no package named {pkg_name}"))
            })?;
            let (mut sym, intern_status) = egcl_stdlib::intern(&name_str, package)?;
            egcl_rt::rooted_ref!(_sym_root = &mut sym);
            let status = match intern_status {
                egcl_stdlib::InternStatus::Internal => package_status_symbol("INTERNAL"),
                egcl_stdlib::InternStatus::External => package_status_symbol("EXTERNAL"),
                egcl_stdlib::InternStatus::Inherited => package_status_symbol("INHERITED"),
                egcl_stdlib::InternStatus::New => NIL,
            };
            env.set_mv(vec![sym, status]);
            Ok(sym)
        }),

        "FIND-SYMBOL" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                return Err(EgclError::Internal("FIND-SYMBOL requires a name".into()));
            }
            // FIND-SYMBOL takes the name STRING verbatim (no readtable
            // upcasing, no package-prefix parsing — the package is the second
            // argument), and matches case-SENSITIVELY (CLHS): (find-symbol
            // "car") is NIL, (find-symbol "CAR") finds CAR (bliss-961p). So do
            // NOT run it through symbol_bare_name (which upcases + strips).
            let name = val_as_str(args[0]);
            // The package argument is optional and defaults to *PACKAGE*.
            let pkg_name = if args.len() > 1 {
                let pkg_raw = string_designator_name(args[1]);
                resolve_package_name(env, &pkg_raw)
            } else {
                effective_package_name(env)
            };
            if let Some((sym, status)) = find_symbol_in_package_cased(env, &pkg_name, &name, true) {
                env.set_mv(vec![sym, package_status_symbol(status)]);
                return Ok(sym);
            }
            env.set_mv(vec![NIL, NIL]);
            Ok(NIL)
        }),

        "UNEXPORT" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                return Ok(T);
            }
            let symbols = args[0];
            let pkg_name = if args.len() > 1 {
                normalize_package_name(&string_designator_name(args[1]))
            } else {
                effective_package_name(env)
            };
            let sym_vals: Vec<EgclVal> = if symbols.is_cons() {
                list_to_vec(symbols)
            } else if symbols.is_nil() {
                Vec::new()
            } else {
                vec![symbols]
            };
            if let Some(pkg) = egcl_stdlib::find_package(&pkg_name) {
                let mut resolved = Vec::with_capacity(sym_vals.len());
                for sv in sym_vals {
                    // UNEXPORT requires every symbol to be ACCESSIBLE in the
                    // package; an inaccessible one is a PACKAGE-ERROR (CLHS
                    // UNEXPORT; ansi-test UNEXPORT.5).
                    let name = symbol_bare_name(&val_as_str(sv));
                    let accessible = match find_symbol_in_package(env, &pkg_name, &name) {
                        Some((found, _)) if !sv.is_symbol() || found == sv => Some(found),
                        _ => None,
                    };
                    match accessible {
                        Some(sym) => resolved.push(sym),
                        None => {
                            return Err(EgclError::PackageError(format!(
                                "symbol {name} is not accessible in package {pkg_name}"
                            )));
                        }
                    }
                }
                let _ = egcl_stdlib::unexport(&resolved, pkg);
            }
            Ok(T)
        }),

        "SHADOW" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                return Ok(T);
            }
            let names_val = args[0];
            let pkg_name = if args.len() > 1 {
                normalize_package_name(&string_designator_name(args[1]))
            } else {
                effective_package_name(env)
            };
            ensure_package_available(env, &pkg_name, &[]);
            let mut names = Vec::new();
            // Case-preserving designators: (shadow "foo") shadows |foo|
            // (CLHS; the old symbol_bare_name upcased and shadowed FOO).
            if names_val.is_cons() {
                for name in list_to_vec(names_val) {
                    names.push(string_designator_name(name));
                }
            } else {
                names.push(string_designator_name(names_val));
            }
            // SHADOW forks a distinct present symbol shadowing any inherited
            // same-named one (bliss-b1o) — not a plain intern.
            if let Some(pkg) = egcl_stdlib::find_package(&pkg_name) {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let _ = egcl_stdlib::shadow(&refs, pkg);
            }
            Ok(T)
        }),

        "UNINTERN" => Some(|_operator, args, env| {
            let args = RootedVals::new(args.to_vec());

            if args.is_empty() {
                return Ok(NIL);
            }
            let symbol = args[0];
            let pkg_name = if args.len() > 1 {
                normalize_package_name(&string_designator_name(args[1]))
            } else {
                effective_package_name(env)
            };
            let name = symbol_bare_name(&val_as_str(symbol));
            let Some(pkg) = egcl_stdlib::find_package(&pkg_name) else {
                return Ok(NIL);
            };
            // Shadowing-reveal conflict (CLHS UNINTERN; ansi UNINTERN.8/9):
            // uninterning a SHADOWING symbol uncovers the inherited
            // same-named externals of the use list. If those are TWO OR
            // MORE distinct symbols, the reveal creates a name conflict —
            // a PACKAGE-ERROR; a single symbol (even via several used
            // packages) is fine.
            if egcl_stdlib::package_shadowing_symbols(pkg)
                .iter()
                .any(|s| string_designator_name(*s).eq_ignore_ascii_case(&name))
            {
                let mut revealed: Vec<EgclVal> = Vec::new();
                for used in egcl_stdlib::package_use_list(pkg) {
                    if let Some(ext) = egcl_stdlib::find_present_symbol(used, &name) {
                        if egcl_stdlib::is_external_symbol(used, &name) && !revealed.contains(&ext)
                        {
                            revealed.push(ext);
                        }
                    }
                }
                if revealed.len() > 1 {
                    return Err(EgclError::PackageError(format!(
                        "uninterning shadowing symbol {name} from {pkg_name} would \
                             reveal {} conflicting inherited symbols",
                        revealed.len()
                    )));
                }
            }
            let removed = match egcl_stdlib::find_present_symbol(pkg, &name) {
                Some(sym) => {
                    // Read the home BEFORE removing the symbol: once it is
                    // gone from the package, the name→home scan can no longer
                    // see it.
                    let home_is_this = sym.is_symbol()
                        && sym != NIL
                        && sym != T
                        && normalize_package_name(&symbol_home_package_name(sym)) == pkg_name;
                    let ok = egcl_stdlib::unintern(sym, pkg).unwrap_or(false);
                    // CLHS: if this package was the symbol's home package,
                    // uninterning makes the symbol homeless (SYMBOL-PACKAGE
                    // becomes NIL). Record it — egcl's name-prefix home model
                    // has no other way to represent "no home package".
                    if ok && home_is_this {
                        if let Some(idx) = sym.symbol_index() {
                            mark_symbol_homeless(idx);
                        }
                    }
                    ok
                }
                None => false,
            };
            Ok(if removed { T } else { NIL })
        }),
        _ => None,
    }
}
