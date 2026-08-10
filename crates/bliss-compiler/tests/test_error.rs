//! Tests for bliss-compiler error types — CompilerError enum.
//!
//! Covers every variant's construction, field access, Debug/Display output,
//! and std::error::Error trait conformance.

use bliss_compiler::error::CompilerError;
use bliss_compiler::reader::SourcePos;

// ── ReaderError variant ─────────────────────────────────────────────

#[test]
fn reader_error_with_position() {
    let pos = SourcePos {
        file: Some("test.lisp".into()),
        line: 10,
        column: 5,
    };
    let err = CompilerError::ReaderError {
        message: "unexpected token".into(),
        position: Some(pos),
    };
    match &err {
        CompilerError::ReaderError { message, position } => {
            assert_eq!(message, "unexpected token");
            let p = position.as_ref().expect("position should be Some");
            assert_eq!(p.file.as_deref(), Some("test.lisp"));
            assert_eq!(p.line, 10);
            assert_eq!(p.column, 5);
        }
        _ => panic!("expected ReaderError"),
    }
}

#[test]
fn reader_error_without_position() {
    let err = CompilerError::ReaderError {
        message: "EOF while reading".into(),
        position: None,
    };
    match &err {
        CompilerError::ReaderError { message, position } => {
            assert_eq!(message, "EOF while reading");
            assert!(position.is_none());
        }
        _ => panic!("expected ReaderError"),
    }
}

#[test]
fn reader_error_empty_message() {
    let err = CompilerError::ReaderError {
        message: String::new(),
        position: None,
    };
    match &err {
        CompilerError::ReaderError { message, .. } => {
            assert!(message.is_empty());
        }
        _ => panic!("expected ReaderError"),
    }
}

// ── MacroExpansionError variant ─────────────────────────────────────

#[test]
fn macro_expansion_error_with_backtrace() {
    let err = CompilerError::MacroExpansionError {
        message: "undefined macro".into(),
        backtrace: vec!["(DEFUN FOO ...)".into(), "(WHEN ...)".into()],
    };
    match &err {
        CompilerError::MacroExpansionError { message, backtrace } => {
            assert_eq!(message, "undefined macro");
            assert_eq!(backtrace.len(), 2);
            assert_eq!(backtrace[0], "(DEFUN FOO ...)");
            assert_eq!(backtrace[1], "(WHEN ...)");
        }
        _ => panic!("expected MacroExpansionError"),
    }
}

#[test]
fn macro_expansion_error_empty_backtrace() {
    let err = CompilerError::MacroExpansionError {
        message: "expansion failed".into(),
        backtrace: vec![],
    };
    match &err {
        CompilerError::MacroExpansionError { backtrace, .. } => {
            assert!(backtrace.is_empty());
        }
        _ => panic!("expected MacroExpansionError"),
    }
}

// ── CircularExpansion variant ───────────────────────────────────────

#[test]
fn circular_expansion_stores_macro_name() {
    let err = CompilerError::CircularExpansion {
        macro_name: "MY-MACRO".into(),
    };
    match &err {
        CompilerError::CircularExpansion { macro_name } => {
            assert_eq!(macro_name, "MY-MACRO");
        }
        _ => panic!("expected CircularExpansion"),
    }
}

// ── IrError variant ────────────────────────────────────────────────

#[test]
fn ir_error_construction() {
    let err = CompilerError::IrError {
        message: "type mismatch in SSA".into(),
    };
    match &err {
        CompilerError::IrError { message } => {
            assert_eq!(message, "type mismatch in SSA");
        }
        _ => panic!("expected IrError"),
    }
}

// ── OptimisationError variant ──────────────────────────────────────

#[test]
fn optimisation_error_fields() {
    let err = CompilerError::OptimisationError {
        pass_name: "inline".into(),
        message: "recursion depth exceeded".into(),
    };
    match &err {
        CompilerError::OptimisationError { pass_name, message } => {
            assert_eq!(pass_name, "inline");
            assert_eq!(message, "recursion depth exceeded");
        }
        _ => panic!("expected OptimisationError"),
    }
}

// ── CodegenError variant ───────────────────────────────────────────

#[test]
fn codegen_error_construction() {
    let err = CompilerError::CodegenError {
        message: "unsupported instruction".into(),
    };
    match &err {
        CompilerError::CodegenError { message } => {
            assert_eq!(message, "unsupported instruction");
        }
        _ => panic!("expected CodegenError"),
    }
}

// ── RegisterAllocationError variant ────────────────────────────────

#[test]
fn register_allocation_error_construction() {
    let err = CompilerError::RegisterAllocationError {
        message: "too many live values".into(),
    };
    match &err {
        CompilerError::RegisterAllocationError { message } => {
            assert_eq!(message, "too many live values");
        }
        _ => panic!("expected RegisterAllocationError"),
    }
}

// ── Debug trait ────────────────────────────────────────────────────

#[test]
fn debug_output_reader_error_with_position() {
    let err = CompilerError::ReaderError {
        message: "bad token".into(),
        position: Some(SourcePos { file: Some("a.lisp".into()), line: 1, column: 0 }),
    };
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("ReaderError"), "Debug should contain variant name: {}", dbg);
    assert!(dbg.contains("bad token"), "Debug should contain message: {}", dbg);
}

#[test]
fn debug_output_reader_error_no_position() {
    let err = CompilerError::ReaderError {
        message: "eof".into(),
        position: None,
    };
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("ReaderError"), "Debug should contain variant name");
    assert!(dbg.contains("None"), "Debug should show None for position");
}

#[test]
fn debug_output_macro_expansion_error() {
    let err = CompilerError::MacroExpansionError {
        message: "bad expansion".into(),
        backtrace: vec!["(DEFMACRO ...)".into()],
    };
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("MacroExpansionError"));
    assert!(dbg.contains("bad expansion"));
    assert!(dbg.contains("DEFMACRO"));
}

#[test]
fn debug_output_circular_expansion() {
    let err = CompilerError::CircularExpansion {
        macro_name: "LOOP".into(),
    };
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("CircularExpansion"));
    assert!(dbg.contains("LOOP"));
}

#[test]
fn debug_output_ir_error() {
    let err = CompilerError::IrError {
        message: "invalid node".into(),
    };
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("IrError"));
    assert!(dbg.contains("invalid node"));
}

#[test]
fn debug_output_optimisation_error() {
    let err = CompilerError::OptimisationError {
        pass_name: "escape-analysis".into(),
        message: "stack overflow".into(),
    };
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("OptimisationError"));
    assert!(dbg.contains("escape-analysis"));
    assert!(dbg.contains("stack overflow"));
}

#[test]
fn debug_output_codegen_error() {
    let err = CompilerError::CodegenError {
        message: "emit failed".into(),
    };
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("CodegenError"));
    assert!(dbg.contains("emit failed"));
}

#[test]
fn debug_output_register_allocation_error() {
    let err = CompilerError::RegisterAllocationError {
        message: "spill overflow".into(),
    };
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("RegisterAllocationError"));
    assert!(dbg.contains("spill overflow"));
}

// ── Display trait ──────────────────────────────────────────────────

#[test]
fn display_all_variants_produce_nonempty_output() {
    let errors: Vec<CompilerError> = vec![
        CompilerError::ReaderError {
            message: "unexpected )".into(),
            position: Some(SourcePos { file: Some("x.lisp".into()), line: 5, column: 3 }),
        },
        CompilerError::MacroExpansionError {
            message: "expansion failed".into(),
            backtrace: vec![],
        },
        CompilerError::CircularExpansion { macro_name: "WHEN".into() },
        CompilerError::IrError { message: "dangling edge".into() },
        CompilerError::OptimisationError { pass_name: "DCE".into(), message: "fault".into() },
        CompilerError::CodegenError { message: "encoding error".into() },
        CompilerError::RegisterAllocationError { message: "ran out of registers".into() },
    ];
    for (i, err) in errors.iter().enumerate() {
        let display = format!("{}", err);
        assert!(!display.is_empty(), "variant {} display should be non-empty", i);
    }
}

// ── std::error::Error trait ────────────────────────────────────────

#[test]
fn compiler_error_implements_error_trait() {
    // Verify CompilerError can be used as a dyn std::error::Error
    let err: Box<dyn std::error::Error> = Box::new(CompilerError::ReaderError {
        message: "test".into(),
        position: None,
    });
    // The error trait requires Display and Debug — exercise both
    let _ = format!("{}", err);
    let _ = format!("{:?}", err);
}

#[test]
fn compiler_error_source_is_none() {
    // CompilerError does not wrap another error, so source() should be None
    let err = CompilerError::IrError {
        message: "test".into(),
    };
    let error_ref: &dyn std::error::Error = &err;
    assert!(error_ref.source().is_none(), "CompilerError::source() should be None");
}

#[test]
fn all_variants_implement_error_trait() {
    // Each variant should be usable as dyn Error
    let errors: Vec<Box<dyn std::error::Error>> = vec![
        Box::new(CompilerError::ReaderError {
            message: "a".into(),
            position: None,
        }),
        Box::new(CompilerError::MacroExpansionError {
            message: "b".into(),
            backtrace: vec![],
        }),
        Box::new(CompilerError::CircularExpansion {
            macro_name: "c".into(),
        }),
        Box::new(CompilerError::IrError {
            message: "d".into(),
        }),
        Box::new(CompilerError::OptimisationError {
            pass_name: "e".into(),
            message: "f".into(),
        }),
        Box::new(CompilerError::CodegenError {
            message: "g".into(),
        }),
        Box::new(CompilerError::RegisterAllocationError {
            message: "h".into(),
        }),
    ];
    assert_eq!(errors.len(), 7, "should have all 7 variants");
    for (i, err) in errors.iter().enumerate() {
        let display = format!("{}", err);
        assert!(!display.is_empty(), "variant {} display should be non-empty", i);
        assert!(err.source().is_none(), "variant {} source should be None", i);
    }
}

// ── Pattern matching exhaustiveness ────────────────────────────────

#[test]
fn match_all_variants_exhaustive() {
    // Ensures we can match on every variant — will fail to compile
    // if a variant is added without updating this test
    let errors = vec![
        CompilerError::ReaderError { message: "a".into(), position: None },
        CompilerError::MacroExpansionError { message: "b".into(), backtrace: vec![] },
        CompilerError::CircularExpansion { macro_name: "c".into() },
        CompilerError::IrError { message: "d".into() },
        CompilerError::OptimisationError { pass_name: "e".into(), message: "f".into() },
        CompilerError::CodegenError { message: "g".into() },
        CompilerError::RegisterAllocationError { message: "h".into() },
    ];
    for err in &errors {
        match err {
            CompilerError::ReaderError { .. } => {}
            CompilerError::MacroExpansionError { .. } => {}
            CompilerError::CircularExpansion { .. } => {}
            CompilerError::IrError { .. } => {}
            CompilerError::OptimisationError { .. } => {}
            CompilerError::CodegenError { .. } => {}
            CompilerError::RegisterAllocationError { .. } => {}
        }
    }
}

// ── SourcePos used inside CompilerError ────────────────────────────

#[test]
fn source_pos_clone_inside_reader_error() {
    let pos = SourcePos { file: Some("main.lisp".into()), line: 100, column: 42 };
    let pos2 = pos.clone();
    let err = CompilerError::ReaderError {
        message: "test".into(),
        position: Some(pos),
    };
    // Verify the cloned pos is independent
    assert_eq!(pos2.line, 100);
    assert_eq!(pos2.column, 42);
    // And the error still holds its own copy
    match &err {
        CompilerError::ReaderError { position, .. } => {
            let p = position.as_ref().unwrap();
            assert_eq!(p.line, 100);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn source_pos_debug_output() {
    let pos = SourcePos { file: None, line: 0, column: 0 };
    let dbg = format!("{:?}", pos);
    assert!(dbg.contains("SourcePos"), "Debug should mention SourcePos: {}", dbg);
    assert!(dbg.contains("None"), "Debug should show None file: {}", dbg);
}

#[test]
fn source_pos_with_file_debug() {
    let pos = SourcePos { file: Some("foo.lisp".into()), line: 99, column: 12 };
    let dbg = format!("{:?}", pos);
    assert!(dbg.contains("foo.lisp"));
    assert!(dbg.contains("99"));
    assert!(dbg.contains("12"));
}

// ── Edge cases: Unicode and special characters in error messages ────

#[test]
fn error_message_with_unicode() {
    let err = CompilerError::ReaderError {
        message: "unexpected character: λ".into(),
        position: None,
    };
    match &err {
        CompilerError::ReaderError { message, .. } => {
            assert!(message.contains('λ'));
        }
        _ => panic!("wrong variant"),
    }
    let dbg = format!("{:?}", err);
    assert!(dbg.contains("λ"));
}

#[test]
fn error_message_with_newlines() {
    let err = CompilerError::MacroExpansionError {
        message: "line1\nline2\nline3".into(),
        backtrace: vec!["form1\nform2".into()],
    };
    match &err {
        CompilerError::MacroExpansionError { message, backtrace } => {
            assert!(message.contains('\n'));
            assert_eq!(message.lines().count(), 3);
            assert!(backtrace[0].contains('\n'));
        }
        _ => panic!("wrong variant"),
    }
}

// ── Compiler error as Result ───────────────────────────────────────

#[test]
fn compiler_error_in_result_type() {
    fn failing_compile() -> Result<(), CompilerError> {
        Err(CompilerError::CodegenError {
            message: "not implemented".into(),
        })
    }
    let result = failing_compile();
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        CompilerError::CodegenError { message } => {
            assert_eq!(message, "not implemented");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn compiler_error_question_mark_propagation() {
    fn inner() -> Result<(), CompilerError> {
        Err(CompilerError::IrError {
            message: "bad graph".into(),
        })
    }
    fn outer() -> Result<(), CompilerError> {
        inner()?;
        Ok(())
    }
    assert!(outer().is_err());
}

#[test]
fn compiler_error_can_be_boxed_as_dyn_error() {
    fn failing() -> Result<(), Box<dyn std::error::Error>> {
        Err(Box::new(CompilerError::RegisterAllocationError {
            message: "spill".into(),
        }))
    }
    assert!(failing().is_err());
}
