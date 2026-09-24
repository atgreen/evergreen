;;; Shared benchmark protocol (bliss-jpd0, spec R10.09/R10.10/R10.59).
;;;
;;; Each benchmark file loads this, defines its workload, then calls
;;; (run-benchmark "name" #'thunk).  Timing is IN-PROCESS via
;;; get-internal-real-time, so implementation startup cost never pollutes the
;;; number (the old caveat about subtracting startup disappears).  One untimed
;;; warmup run precedes the timed run so a tiering implementation (torcl) is
;;; measured at its promoted tier — the spec goal G2 is PEAK throughput.
;;;
;;; Output protocol (parsed by ../run.sh): a single line
;;;   BENCH-MS <name> <milliseconds>

(defun bench-elapsed-ms (start end)
  (round (* 1000 (- end start)) internal-time-units-per-second))

(defun run-benchmark (name thunk)
  ;; Untimed warmup: lets torcl promote the hot code (T0 -> bytecode -> T1/T2)
  ;; and warms caches on any implementation.
  (funcall thunk)
  (let ((start (get-internal-real-time)))
    (funcall thunk)
    (let ((end (get-internal-real-time)))
      (format t "BENCH-MS ~a ~a~%" name (bench-elapsed-ms start end)))))
