(defclass recursive-box () ((value)))
(let ((remaining 100))
  (defmethod slot-unbound (class (object recursive-box) name)
    (declare (ignore class))
    (if (zerop (decf remaining))
        nil
        (slot-value object name))))
(assert
 (handler-case (progn (slot-value (make-instance 'recursive-box) 'value) nil)
   (storage-condition () t)))
(write-line "SLOT-UNBOUND-RECURSION-PASS")
