;;; Rebuilding # after giving it constituent syntax, as named-readtables's
;;; (:merge :standard) does, restores every standard # syntax (GitHub #940).
(defun rebuild ()
  (let ((std (copy-readtable nil)) (rt (copy-readtable nil)))
    ;; named-readtables: clear every macro char to constituent syntax ...
    (dolist (c '(#\# #\' #\( #\) #\; #\" #\` #\,)) (set-syntax-from-char c #\A rt))
    ;; ... then merge :standard back in.
    (dolist (c '(#\' #\( #\) #\; #\" #\` #\,))
      (multiple-value-bind (f nt) (get-macro-character c std) (set-macro-character c f nt rt)))
    (handler-case (get-dispatch-macro-character #\# #\: rt)
      (error () (make-dispatch-macro-character #\# t rt)))
    (dolist (sub (coerce "'(*:.BOXRCASP=#+-|\\" 'list))
      (let ((f (get-dispatch-macro-character #\# sub std)))
        (when f (set-dispatch-macro-character #\# sub f rt))))
    rt))
(let ((*readtable* (rebuild)) (fails 0))
  (dolist (case '(("#'car" (function car)) ("#:foo" :uninterned) ("#(1 2)" #(1 2)) ("#\\a" #\a)
                  ("#x10" 16) ("#b101" 5) ("#o17" 15) ("#3r21" 7) ("#*101" #*101)
                  ("#.(+ 1 2)" 3) ("#+(or) 1 2" 2) ("#-(or) 1" 1) ("(a #'car)" (a (function car)))
                  ("#c(1 2)" #c(1 2)) ("#p\"/tmp/x\"" #p"/tmp/x") ("#|c|# 9" 9) ("#2a((1))" #2a((1)))
                  ("'(1 . #1=(2))" (quote (1 2)))))
    (destructuring-bind (text want) case
      (let ((got (handler-case (read-from-string text) (error (e) (list :error (type-of e))))))
        (unless (if (eq want :uninterned) (and (symbolp got) (null (symbol-package got))) (equalp got want))
          (incf fails) (format t "~&FAIL ~s => ~s~%" text got)))))
  (assert (zerop fails)))
;; After constituent syntax, # is no longer a dispatching macro character.
(let ((rt (copy-readtable nil)))
  (set-syntax-from-char #\# #\A rt)
  (assert (null (get-macro-character #\# rt)))
  (assert (handler-case (progn (get-dispatch-macro-character #\# #\' rt) nil) (error () t)))
  ;; A remade # starts with an empty table.
  (make-dispatch-macro-character #\# t rt)
  (assert (handler-case (progn (let ((*readtable* rt)) (read-from-string "#'car")) nil)
            (reader-error () t))))
(format t "~&SHARP-REBUILD-PASS~%")
