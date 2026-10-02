;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;;; Original Unix bindings for EGCL. Native operations live in egcl-stdlib.
(defpackage #:egcl-posix
  (:use #:cl)
  (:shadow #:open #:close)
  (:export #:syscall-error #:syscall-errno #:syscall-name #:raw-syscall
           #:getpid #:getppid #:kill #:waitpid #:open #:close #:o-rdonly
           #:getpagesize #:mmap #:munmap #:prot-read #:prot-write #:prot-none
           #:map-shared #:map-private #:map-anon
           #:stat #:stat-dev #:stat-ino #:stat-mode #:stat-nlink #:stat-uid #:stat-gid
           #:stat-rdev #:stat-size #:stat-atime #:stat-mtime #:stat-ctime
           #:stat-blksize #:stat-blocks
           #:wifexited #:wifsignaled #:wifstopped #:wexitstatus #:wtermsig #:wstopsig
           #:wnohang #:wuntraced #:sigstop #:sigcont #:sigterm #:sigkill))
(in-package #:egcl-posix)

(define-condition syscall-error (error)
  ((errno :initarg :errno :reader syscall-errno)
   (name :initarg :name :reader syscall-name))
  (:report (lambda (condition stream)
             (format stream "POSIX ~A failed with errno ~D"
                     (syscall-name condition) (syscall-errno condition)))))

(defun %checked-call (operation &rest arguments)
  (multiple-value-bind (result secondary errno)
      (apply #'egcl::%posix operation arguments)
    (when errno (error 'syscall-error :name operation :errno errno))
    (values result secondary)))

(defun getpid () (nth-value 0 (%checked-call :getpid)))
(defun raw-syscall (number &rest arguments)
  "Call a Linux syscall with up to six native words or foreign pointers.
Return the result and errno (NIL on success). The caller owns foreign storage
through completion and must obey the syscall's ABI and lifetime requirements."
  (multiple-value-bind (result secondary errno)
      (apply #'egcl::%posix :raw-syscall number arguments)
    (declare (ignore secondary))
    (values result errno)))
(defun getppid () (nth-value 0 (%checked-call :getppid)))
(defun kill (pid signal) (nth-value 0 (%checked-call :kill pid signal)))
(defun waitpid (pid options) (%checked-call :waitpid pid options))
(defun open (path flags &optional (mode 0))
  (nth-value 0 (%checked-call :open (namestring path) flags mode)))
(defun close (fd) (nth-value 0 (%checked-call :close fd)))
(defun getpagesize () (nth-value 0 (%checked-call :getpagesize)))
(defun mmap (address length protection flags fd offset)
  "Return an EGCL-FFI foreign pointer. Release the mapping with MUNMAP."
  (nth-value 0 (%checked-call :mmap address length protection flags fd offset)))
(defun munmap (address length) (nth-value 0 (%checked-call :munmap address length)))
(defstruct (stat (:constructor %make-stat
                    (dev ino mode nlink uid gid rdev size atime mtime ctime blksize blocks)))
  dev ino mode nlink uid gid rdev size atime mtime ctime blksize blocks)
(defun stat (path)
  (apply #'%make-stat (coerce (%checked-call :stat (namestring path)) 'list)))
(defun wifexited (status) (nth-value 0 (%checked-call :wifexited status)))
(defun wifsignaled (status) (nth-value 0 (%checked-call :wifsignaled status)))
(defun wifstopped (status) (nth-value 0 (%checked-call :wifstopped status)))
(defun wexitstatus (status) (nth-value 0 (%checked-call :wexitstatus status)))
(defun wtermsig (status) (nth-value 0 (%checked-call :wtermsig status)))
(defun wstopsig (status) (nth-value 0 (%checked-call :wstopsig status)))

(defconstant wnohang (egcl::%posix :wnohang))
(defconstant o-rdonly (egcl::%posix :o-rdonly))
(defconstant prot-read (egcl::%posix :prot-read))
(defconstant prot-write (egcl::%posix :prot-write))
(defconstant prot-none (egcl::%posix :prot-none))
(defconstant map-shared (egcl::%posix :map-shared))
(defconstant map-private (egcl::%posix :map-private))
(defconstant map-anon (egcl::%posix :map-anon))
(defconstant wuntraced (egcl::%posix :wuntraced))
(defconstant sigstop (egcl::%posix :sigstop))
(defconstant sigcont (egcl::%posix :sigcont))
(defconstant sigterm (egcl::%posix :sigterm))
(defconstant sigkill (egcl::%posix :sigkill))
(provide :egcl-posix)
