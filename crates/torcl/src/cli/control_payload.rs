//! Private ownership of values while a control transfer is paused for cleanup.
//! Catch/block tokens identify destinations, not individual throws. Keeping a
//! paused transfer in the shared token map lets a later throw overwrite it.

use super::{CONTROL_VALUES, TorclError, TorclVal};
use torcl_rt::gc::TraceHostRoots;

#[derive(Default)]
pub(super) struct ControlPayload {
    primary: Option<(String, TorclVal)>,
    multiple: Option<(String, TorclVal)>,
    restart_arguments: Option<(String, TorclVal)>,
}

impl ControlPayload {
    /// Move the existing entries and their keys into private storage. No Lisp
    /// allocation occurs; constructing the secondary-value key follows the
    /// existing Rust host-allocation policy. Root the holder before cleanup.
    pub(super) fn take(token: &str) -> Self {
        let multiple_key = format!("{token}\0MV");
        CONTROL_VALUES.with(|values| {
            let mut values = values.borrow_mut();
            Self {
                primary: values.remove_entry(token),
                multiple: values.remove_entry(&multiple_key),
                restart_arguments: None,
            }
        })
    }

    pub(super) fn for_error(error: &TorclError) -> Self {
        let mut payload = match error {
            TorclError::Internal(token) => {
                // HANDLER-CASE transports a tagged error string, but stores
                // the selected condition under the underlying binding token.
                Self::take(token.strip_prefix("__HANDLER_CASE__:").unwrap_or(token))
            }
            _ => Self::default(),
        };
        if let Some(id) = super::restart_invoked_id(error) {
            let key = format!("RESTART-ARGS:{id}");
            payload.restart_arguments =
                CONTROL_VALUES.with(|values| values.borrow_mut().remove_entry(&key));
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
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut TorclVal)) {
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
    fn reentrant_cleanup_payloads(treewalker: bool) {
        let _lock = super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        torcl_rt::rooted_ref!(_env = &mut env);
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
        torcl_rt::rooted_ref!(_env = &mut env);
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
        torcl_rt::rooted_ref!(_env = &mut env);
        torcl_rt::rooted!(
            form = reader::read_from_string(
                "(unwind-protect (car private-error-datum) (%force-minor-gc-for-test))"
            )
            .unwrap()
            .0
        );
        torcl_rt::rooted!(datum = arena_str("wrong datum"));
        env.set_var("PRIVATE-ERROR-DATUM", *datum);
        let before = datum.to_raw();
        torcl_rt::rooted!(result = eval_form(*form, &mut env));
        assert_ne!(datum.to_raw(), before, "cleanup moved the error datum");
        let Err(TorclError::TypeError {
            datum: recovered, ..
        }) = &*result
        else {
            panic!("expected original type error: {:?}", &*result);
        };
        assert_eq!(*recovered, *datum);
        assert_eq!(val_as_str(*recovered), "wrong datum");
    }
}
