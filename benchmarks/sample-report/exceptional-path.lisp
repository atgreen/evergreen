(declaim (optimize (speed 3) (safety 1) (debug 0)))
(defun exceptional-kernel (x)
  (if (zerop (mod x 97))
      (let ((box (vector x)))
        (funcall #'car (aref box 0)))
      x))

(defun bench-validate ()
  (assert (= (exceptional-kernel 1) 1))
  (assert (eq :caught
             (handler-case (exceptional-kernel 0)
               (type-error () :caught)))))

(defun exceptional-call (x)
  (handler-case (exceptional-kernel x)
    (type-error () 0)))

(defun bench-train ()
  (dotimes (i 4096)
    (bench-workload)))

(defun bench-workload ()
  (let ((sum 0))
    (dotimes (i 2000 sum)
      (incf sum (exceptional-call i)))))

(bench-validate)
(bench-train)
(dotimes (warmup 3) (unless (= (bench-workload) 1978630) (error "Warmup checksum failed")))

#+torcl
(defun bench-report-tiers (phase)
  (format t "BENCH-TIER ~a BENCH-WORKLOAD ~d ~d ~d ~d~%" phase (torcl-ext:function-tier 'bench-workload) (torcl-ext:function-invoke-count 'bench-workload) (torcl-ext:function-back-edge-count 'bench-workload) (torcl-ext:function-osr-count 'bench-workload)))
#+torcl
(progn
  (dotimes (attempt 1000)
    (when (and (eql 2 (torcl-ext:function-tier 'bench-workload))) (return))
    (sleep 0.01))
  (unless (and (eql 2 (torcl-ext:function-tier 'bench-workload))) (error "Timed functions did not reach T2"))
  (bench-report-tiers "before"))

(let* ((deopts-before #+torcl (torcl-ext:deopt-count) #-torcl 0)
       (start (get-internal-real-time))
       (value (let ((value nil))
                (dotimes (repeat 100 value)
                  (setf value (bench-workload)))))
       (end (get-internal-real-time))
       (deopts-after #+torcl (torcl-ext:deopt-count) #-torcl 0))
  (format t "BENCH ~d ~d ~d~%" (- end start) internal-time-units-per-second value)
  (format t "BENCH-DEOPTS ~d~%" (- deopts-after deopts-before)))
#+torcl (bench-report-tiers "after")
