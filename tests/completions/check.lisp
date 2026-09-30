;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;;; A real HTTP round trip through the pinned Completions stack. No implicit
;;;; downloads. The fixture (fake-ollama.py) runs an Ollama-shaped server on
;;;; EGCL_OLLAMA_PORT and echoes the prompt back, so a wrong request or a
;;;; mis-decoded reply fails the assertions below instead of passing silently.
(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE"))
       :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*download* nil
      ocicl-runtime:*local-only* t)

;; pure-tls stands in for cl+ssl: register it as immutable so nothing pulls the
;; CFFI-based original (the endpoint below is plain HTTP either way).
(asdf:load-system :pure-tls/cl+ssl-compat)
(asdf:register-immutable-system "cl+ssl")

(defvar *endpoint*
  (format nil "http://127.0.0.1:~A/api/chat" (uiop:getenv "EGCL_OLLAMA_PORT")))

;; 1. Dexador on its own: the request must be well-formed enough for a strict
;; HTTP server, and the response must parse back into a body and a status.
(asdf:load-system :dexador)
(multiple-value-bind (body status)
    (dex:post *endpoint*
              :content "{\"model\":\"probe\",\"messages\":[{\"role\":\"user\",\"content\":\"ping\"}]}"
              :headers '(("content-type" . "application/json")))
  (assert (= 200 status))
  (assert (search "echo:ping" body)))
(format t "COMPLETIONS-DEXADOR-OK~%")

;; 2. The whole library: JSON encode, POST, JSON decode, and the assistant text
;; pulled out of the decoded alist (which needs the decoder's keys to be EQ to
;; the keyword literals the library writes).
(asdf:load-system :completions)
(let ((completer (make-instance 'completions:ollama-completer
                                :endpoint *endpoint*
                                :model "egcl-test-model")))
  (multiple-value-bind (text history)
      (completions:get-completion
       completer '(((:role . "user") (:content . "hello from egcl"))))
    (assert (stringp text))
    (assert (search "echo:hello from egcl" text))
    (assert (search "model=egcl-test-model" text))
    ;; The returned history is the prompt plus the assistant turn.
    (assert (= 2 (length history)))
    (assert (string= "assistant" (cdr (assoc :role (second history)))))
    (assert (string= text (cdr (assoc :content (second history)))))))
(format t "COMPLETIONS-OK~%")
