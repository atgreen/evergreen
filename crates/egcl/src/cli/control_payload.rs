// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Private ownership of values while a control transfer is paused for cleanup.
//! Catch/block tokens identify destinations, not individual throws. Keeping a
//! paused transfer in the shared token map lets a later throw overwrite it.

use super::{CONTROL_VALUES, EgclError, EgclVal};
use egcl_rt::gc::TraceHostRoots;

#[derive(Default)]
pub(super) struct ControlPayload {
    primary: Option<(String, EgclVal)>,
    multiple: Option<(String, EgclVal)>,
    restart_arguments: Option<(String, EgclVal)>,
}

/// Reserve map entries before entering generated native code. A failure is
/// reported to the caller while the ordinary Rust/Lisp emergency path is still
/// available; transfer dispatch must never discover a rehash failure halfway
/// through retiring a cleanup.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) fn reserve_control_values(additional: usize) -> Result<(), ()> {
    CONTROL_VALUES.with(|values| values.borrow_mut().try_reserve(additional).map_err(|_| ()))
}

impl ControlPayload {
    fn remove_matching(
        values: &mut std::collections::HashMap<String, EgclVal>,
        predicate: impl Fn(&str) -> bool,
    ) -> Option<(String, EgclVal)> {
        // HashMap's borrowed-key lookup lets us remove the owned String without
        // constructing a temporary key. This path runs after a native transfer
        // has already crossed into Rust, so a formatting allocation here would
        // turn an otherwise reserved transfer into an allocator failure.
        let key = values
            .keys()
            .find(|key| predicate(key.as_str()))
            .map(|key| (key.as_ptr(), key.len()));
        key.and_then(|(ptr, len)| {
            // The map is not mutated between taking this pointer and the
            // borrowed lookup. Rust's HashMap API cannot express this
            // allocation-free remove directly on the declared MSRV, so keep
            // the short raw-key window local and preserve the owned String.
            let key =
                unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len)) };
            values.remove_entry(key)
        })
    }

    /// Move the existing entries and their keys into private storage without
    /// allocating temporary lookup strings. Root the holder before cleanup.
    pub(super) fn take(token: &str) -> Self {
        CONTROL_VALUES.with(|values| {
            let mut values = values.borrow_mut();
            Self {
                primary: values.remove_entry(token),
                multiple: Self::remove_matching(&mut values, |key| {
                    key.strip_prefix(token) == Some("\0MV")
                }),
                restart_arguments: None,
            }
        })
    }

    pub(super) fn for_error(error: &EgclError) -> Self {
        let mut payload = match error {
            EgclError::Internal(token) => {
                // HANDLER-CASE transports a tagged error string, but stores
                // the selected condition under the underlying binding token.
                Self::take(token.strip_prefix("__HANDLER_CASE__:").unwrap_or(token))
            }
            _ => Self::default(),
        };
        if let Some(id) = super::restart_invoked_id(error) {
            payload.restart_arguments = CONTROL_VALUES.with(|values| {
                let mut values = values.borrow_mut();
                Self::remove_matching(&mut values, |key| {
                    key.strip_prefix("RESTART-ARGS:")
                        .and_then(|suffix| suffix.parse::<u64>().ok())
                        == Some(id)
                })
            });
        }
        payload
    }

    /// Resume this transfer. No Lisp allocation or GC occurs here. Inserting
    /// moved keys follows the existing Rust map-allocation policy; emergency
    /// host-allocation failure remains an ABI activation gate.
    pub(super) fn restore(self) {
        CONTROL_VALUES.with(|values| {
            let mut values = values.borrow_mut();
            for (key, value) in self
                .primary
                .into_iter()
                .chain(self.multiple)
                .chain(self.restart_arguments)
            {
                values.insert(key, value);
            }
        });
    }
}

impl TraceHostRoots for ControlPayload {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for (_, value) in self
            .primary
            .iter_mut()
            .chain(self.multiple.iter_mut())
            .chain(self.restart_arguments.iter_mut())
        {
            visit(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::ControlPayload;

    #[test]
    fn payload_take_uses_existing_secondary_and_restart_keys() {
        let _lock = super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let token = "payload-key";
        let restart_id = 0xfeed_u64;
        CONTROL_VALUES.with(|values| {
            let mut values = values.borrow_mut();
            values.clear();
            values.insert(token.into(), EgclVal::from_fixnum(1));
            values.insert(format!("{token}\0MV"), EgclVal::from_fixnum(2));
            values.insert(
                format!("RESTART-ARGS:{restart_id}"),
                EgclVal::from_fixnum(3),
            );
        });
        let payload = ControlPayload::take(token);
        assert!(payload.primary.is_some());
        assert!(payload.multiple.is_some());
        let error = EgclError::Internal(restart_invoked_token(restart_id, "RECOVER"));
        let restart_payload = ControlPayload::for_error(&error);
        assert_eq!(
            restart_payload
                .restart_arguments
                .as_ref()
                .map(|(_, value)| *value),
            Some(EgclVal::from_fixnum(3))
        );
        restart_payload.restore();
        payload.restore();
        CONTROL_VALUES.with(|values| values.borrow_mut().clear());
    }

    #[test]
    fn type_error_condition_preserves_heap_datum_identity() {
        let _lock = super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let datum_slot = resolve_sym("DATUM").unwrap();
        resolve_sym("NAME").unwrap();
        resolve_sym("EXPECTED-TYPE").unwrap();
        egcl_rt::rooted!(datum = arena_cons(T, NIL));
        let before = datum.to_raw();
        egcl_rt::rooted!(
            error = EgclError::TypeError {
                datum: *datum,
                expected: "SYMBOL".into()
            }
        );
        egcl_rt::rooted!(condition = egcl_error_to_condition(&mut env, &error).unwrap().unwrap());
        if std::env::var("EGCL_GC_STRESS").as_deref() == Ok("1") {
            assert_ne!(
                datum.to_raw(),
                before,
                "the condition build must relocate the datum"
            );
        }
        assert_eq!(
            read_slot_value(*condition, datum_slot, &env).unwrap(),
            *datum
        );
    }

    #[test]
    fn catch_tags_use_object_identity_across_cleanup_and_gc() {
        let _lock = super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        super::super::read_eval_all_env(
            "(setq heap-tag-a \"same tag\" heap-tag-b \"same tag\")
             (defun catch-tag-gc-value () (%force-minor-gc-for-test) :kept)",
            &mut env,
        )
        .unwrap();
        for treewalker in [false, true] {
            for source in [
                "(let ((a (list :tag)) (b (list :tag)))
                  (eq (catch a (catch b (throw a :outer)) :wrong) :outer))",
                "(let ((a heap-tag-a) (b heap-tag-b))
                  (eq (catch a (catch b (throw a :outer)) :wrong) :outer))",
                "(let ((a (make-symbol \"TAG\")) (b (make-symbol \"TAG\")))
                  (eq (catch a (catch b (throw a :outer)) :wrong) :outer))",
                "(let ((tag (list :tag)))
                  (eq (catch tag (catch tag (throw tag :inner)) :outer) :outer))",
                "(let ((tag (list :tag)))
                  (eq (catch tag (throw tag (catch-tag-gc-value))) :kept))",
            ] {
                let source = if treewalker {
                    format!("(eval '{source})")
                } else {
                    source.to_owned()
                };
                assert_eq!(
                    super::super::read_eval_all_env(&source, &mut env)
                        .unwrap_or_else(|error| panic!("{source}: {error:?}")),
                    T,
                    "{source}"
                );
                assert!(env.catch_stack.is_empty());
            }
        }
    }
    fn reentrant_cleanup_payloads(treewalker: bool) {
        let _lock = super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        for (value, expected) in [
            ("x", "(list x)"),
            ("(values)", "nil"),
            ("(values x)", "(list x)"),
            ("(values x (list x))", "(list x (list x))"),
        ] {
            let source = format!(
                "(let ((x (list :kept)))
              (equal (multiple-value-list
                (catch :outer
                  (unwind-protect (throw :outer {value})
                    (catch :inner
                      (unwind-protect (throw :outer (values :wrong :extra))
                        (throw :inner :redirect)))
                    (%force-minor-gc-for-test))))
                {expected}))"
            );
            let source = if treewalker {
                format!("(eval '{source})")
            } else {
                source
            };
            assert_eq!(
                super::super::read_eval_all_env(&source, &mut env).unwrap(),
                T,
                "treewalker={treewalker}, protected={value}"
            );
        }
    }

    #[test]
    fn reentrant_cleanup_payloads_bytecode() {
        reentrant_cleanup_payloads(false);
    }
    #[test]
    fn reentrant_cleanup_payloads_treewalker() {
        reentrant_cleanup_payloads(true);
    }

    #[test]
    fn reentrant_cleanup_keeps_conditions_restart_arguments_and_return_values() {
        let _lock = super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        for treewalker in [false, true] {
            for source in [
                "(let ((original (make-condition 'simple-error)))
               (eq (handler-case
                 (unwind-protect (error original)
                   (catch :inner
                     (unwind-protect (error (make-condition 'simple-error))
                       (throw :inner :redirect)))
                   (%force-minor-gc-for-test))
                 (error (condition) condition)) original))",
                "(let ((x (list :original)))
               (equal (restart-case
                  (unwind-protect (invoke-restart 'resume x)
                    (catch :inner
                      (unwind-protect (invoke-restart 'resume :wrong)
                        (throw :inner :redirect)))
                    (%force-minor-gc-for-test))
                  (resume (value) value)) x))",
                "(let ((x (list :original)))
               (equal (multiple-value-list
                 (block done
                   (unwind-protect (return-from done (values x (list x)))
                     (%force-minor-gc-for-test)
                     (values :wrong))))
                 (list x (list x))))",
                "(equal (multiple-value-list
               (block done (unwind-protect (return-from done (values)) (values :wrong)))) nil)",
            ] {
                let source = if treewalker {
                    format!("(eval '{source})")
                } else {
                    source.to_owned()
                };
                assert_eq!(
                    super::super::read_eval_all_env(&source, &mut env).unwrap(),
                    T,
                    "{source}"
                );
            }
        }
    }

    #[test]
    fn treewalker_cleanup_roots_the_original_error_datum() {
        let _lock = super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(
            form = reader::read_from_string(
                "(unwind-protect (car private-error-datum) (%force-minor-gc-for-test))"
            )
            .unwrap()
            .0
        );
        egcl_rt::rooted!(datum = arena_str("wrong datum"));
        env.set_var("PRIVATE-ERROR-DATUM", *datum);
        let before = datum.to_raw();
        egcl_rt::rooted!(result = eval_form(*form, &mut env));
        assert_ne!(datum.to_raw(), before, "cleanup moved the error datum");
        let Err(EgclError::TypeError {
            datum: recovered, ..
        }) = &*result
        else {
            panic!("expected original type error: {:?}", &*result);
        };
        assert_eq!(*recovered, *datum);
        assert_eq!(val_as_str(*recovered), "wrong datum");
    }
    #[test]
    fn raw_handler_error_reaches_only_older_clusters() {
        let _lock = super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        read_eval_all_env(
            "(setq *raw-inner* 0 *raw-outer* 0)
             (defun raw-inner (c)
               (setq *raw-inner* (+ *raw-inner* 1))
               (%force-minor-gc-for-test)
               (symbol-value 27))
             (defun raw-outer (c)
               (setq *raw-outer* (+ *raw-outer* 1))
               (setq *raw-datum* (slot-value c 'datum))
               (%force-minor-gc-for-test))",
            &mut env,
        )
        .unwrap();
        for name in ["RAW-OUTER", "RAW-INNER"] {
            env.handlers.push(HandlerCluster {
                entries: vec![HandlerEntry {
                    type_name: "TYPE-ERROR".into(),
                    handler: HandlerImpl::Function(resolve_sym(name).unwrap()),
                }],
            });
        }
        egcl_rt::rooted!(
            error = signal_raw_error_in_context(
                &mut env,
                EgclError::TypeError {
                    datum: EgclVal::from_fixnum(19),
                    expected: "SYMBOL".into(),
                }
            )
        );
        assert!(
            matches!(&*error, EgclError::Signalled { .. }),
            "{:?}",
            &*error
        );
        assert_eq!(env.handlers.len(), 2);
        assert_eq!(
            env.lookup_var("*RAW-INNER*"),
            Some(EgclVal::from_fixnum(1))
        );
        assert_eq!(
            env.lookup_var("*RAW-OUTER*"),
            Some(EgclVal::from_fixnum(1))
        );
        assert_eq!(
            env.lookup_var("*RAW-DATUM*"),
            Some(EgclVal::from_fixnum(27))
        );
    }
}
