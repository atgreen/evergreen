#+egcl (egcl-ext:setenv "EGCL_NATIVE_TRANSFER" "0" t)
(declaim (optimize (speed 3) (safety 1) (debug 0)))
;;; No type declarations: identical general-integer recursion on both runtimes.
(defun fibonacci (n)
  (if (< n 2) n (+ (fibonacci (- n 1)) (fibonacci (- n 2)))))
(defparameter *fib-input* 30)
(defparameter *fib-repetitions* 10)
(defun bench-validate ()
  (assert (= (fibonacci 0) 0))
  (assert (= (fibonacci 1) 1))
  (assert (= (fibonacci 10) 55))
  (assert (= (fibonacci *fib-input*) 832040)))
(defun bench-workload ()
  (let ((sum 0))
    (dotimes (i *fib-repetitions*) (incf sum (fibonacci *fib-input*)))
    sum))

;;; Short calls heat both the driver and recursive kernel without timing warmup.
(defun bench-train ()
  (let ((*fib-input* 10) (*fib-repetitions* 1))
    (dotimes (i 10000) (bench-workload))))

#+egcl (egcl-ext:setenv "EGCL_NATIVE_TRANSFER" "1" t)
(bench-validate)
(bench-train)
(dotimes (warmup 3) (unless (= (bench-workload) 8320400) (error "Warmup checksum failed")))

#+egcl
(defun bench-report-tiers (phase)
  (format t "BENCH-TIER ~a FIBONACCI ~d ~d ~d ~d~%" phase (egcl-ext:function-tier 'fibonacci) (egcl-ext:function-invoke-count 'fibonacci) (egcl-ext:function-back-edge-count 'fibonacci) (egcl-ext:function-osr-count 'fibonacci))
(format t "BENCH-TIER ~a BENCH-WORKLOAD ~d ~d ~d ~d~%" phase (egcl-ext:function-tier 'bench-workload) (egcl-ext:function-invoke-count 'bench-workload) (egcl-ext:function-back-edge-count 'bench-workload) (egcl-ext:function-osr-count 'bench-workload)))
#+egcl
(progn
  (dotimes (attempt 1000)
    (when (and (eql 2 (egcl-ext:function-tier 'fibonacci)) (eql 2 (egcl-ext:function-tier 'bench-workload))) (return))
    (sleep 0.01))
  (unless (and (eql 2 (egcl-ext:function-tier 'fibonacci)) (eql 2 (egcl-ext:function-tier 'bench-workload))) (error "Timed functions did not reach T2"))
  (bench-report-tiers "before"))

(let* ((deopts-before #+egcl (egcl-ext:deopt-count) #-egcl 0)
       (start (get-internal-real-time))
       (value (let ((value nil))
                (dotimes (repeat 1 value)
                  (setf value (bench-workload)))))
       (end (get-internal-real-time))
       (deopts-after #+egcl (egcl-ext:deopt-count) #-egcl 0))
  (format t "BENCH ~d ~d ~d~%" (- end start) internal-time-units-per-second value)
  (format t "BENCH-DEOPTS ~d~%" (- deopts-after deopts-before)))
#+egcl (bench-report-tiers "after")
