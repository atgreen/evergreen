;; A command string is interpreted by the native Windows shell.
(multiple-value-bind (status out err)
    (torcl-ext:run-program "echo WINDOWS-SHELL-OK & echo WINDOWS-STDERR-OK 1>&2 & exit /b 7")
  (assert (= status 7))
  (assert (search "WINDOWS-SHELL-OK" out))
  (assert (search "WINDOWS-STDERR-OK" err)))
(format t "WINDOWS-PROCESS-OK~%")
