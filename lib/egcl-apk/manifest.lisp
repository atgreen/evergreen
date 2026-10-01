;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(in-package :egcl-apk)

;; Public framework IDs used by this bounded NativeActivity manifest. These
;; are data, not a dependency on android.jar. See AOSP ResourceTypes.h and
;; frameworks/base/core/res/res/values/public-final.xml.
(defparameter *android-attributes*
  '(("theme" . #x01010000) ("label" . #x01010001) ("name" . #x01010003)
    ("hasCode" . #x0101000c) ("debuggable" . #x0101000f) ("exported" . #x01010010)
    ("configChanges" . #x0101001f) ("value" . #x01010024)
    ("minSdkVersion" . #x0101020c) ("versionCode" . #x0101021b)
    ("versionName" . #x0101021c) ("targetSdkVersion" . #x01010270)
    ("glEsVersion" . #x01010281) ("required" . #x0101028e)
    ("extractNativeLibs" . #x010104ea)))
(defvar *xml-strings*)
(defun string-id (s)
  (or (position s *xml-strings* :test #'equal)
      (prog1 (length *xml-strings*) (setf *xml-strings* (append *xml-strings* (list s))))))
(defun xml-chunk (type header-size data)
  (bytes (u16 type) (u16 header-size) (u32 (+ 8 (length data))) data))
(defun string-length8 (n)
  (unless (< n #x8000) (error "Manifest string exceeds 32767 units"))
  (if (< n 128) (octets n) (octets (logior #x80 (ash n -8)) (logand n 255))))
(defun string-pool ()
  (let ((offset 0) (offsets nil) (strings nil))
    (dolist (s *xml-strings*)
      (let* ((encoded (utf8 s))
             (units (loop for c across s sum (if (> (char-code c) #xffff) 2 1)))
             (entry (bytes (string-length8 units) (string-length8 (length encoded)) encoded #(0))))
        (push (u32 offset) offsets) (push entry strings) (incf offset (length entry))))
    (xml-chunk #x0001 28
     (bytes (u32 (length strings)) (u32 0) (u32 #x100)
            (u32 (+ 28 (* 4 (length strings)))) (u32 0)
            (apply #'bytes (reverse offsets)) (apply #'bytes (reverse strings))
            (make-array (mod (- offset) 4) :element-type '(unsigned-byte 8) :initial-element 0)))))
(defun android-attribute (name type value) (list name type value t))
(defun string-attribute (name value) (android-attribute name 3 value))
(defun xml-node (type payload)
  (xml-chunk type 16 (bytes (u32 1) (u32 #xffffffff) payload)))
(defun xml-element (name attributes children)
  (let* ((name-id (string-id name))
         (uri (string-id "http://schemas.android.com/apk/res/android"))
         (sorted (stable-sort (copy-list attributes) #'<
                    :key (lambda (a) (or (cdr (assoc (first a) *android-attributes* :test #'equal)) #xffffffff))))
         (records
           (mapcar (lambda (a)
             (destructuring-bind (key type value namespaced) a
               (let ((index (if (= type 3) (string-id value) value)))
                 (bytes (u32 (if namespaced uri #xffffffff)) (u32 (string-id key))
                        (u32 (if (= type 3) index #xffffffff)) (u16 8) (octets 0 type) (u32 index))))) sorted)))
    (bytes (xml-node #x0102
             (bytes (u32 #xffffffff) (u32 name-id) (u16 20) (u16 20)
                    (u16 (length records)) (u16 0) (u16 0) (u16 0) (apply #'bytes records)))
           (apply #'bytes (mapcar (lambda (child) (apply #'xml-element child)) children))
           (xml-node #x0103 (bytes (u32 #xffffffff) (u32 name-id))))))
(defun valid-package-p (name)
  (and (stringp name) (find #\. name)
       (every (lambda (part)
         (and (plusp (length part))
              (find (char part 0) "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")
              (every (lambda (c) (find c "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_")) part)))
         (uiop:split-string name :separator "."))))
(defun manifest (&key package (label "EGCL") (version-code 1) (version-name "0.1")
                      (min-sdk 28) (target-sdk 34) (debuggable t) permissions)
  (unless (valid-package-p package) (error "Invalid Android package name: ~S" package))
  (unless (and (integerp min-sdk) (integerp target-sdk) (<= 28 min-sdk target-sdk)
               (integerp version-code) (<= 1 version-code #x7fffffff))
    (error "Invalid version or SDK levels (minimum supported API is 28)"))
  (dolist (p permissions) (unless (valid-package-p p) (error "Invalid permission: ~S" p)))
  (let* ((*xml-strings* (mapcar #'car *android-attributes*))
         (prefix (string-id "android"))
         (uri (string-id "http://schemas.android.com/apk/res/android"))
         (namespace (bytes (u32 prefix) (u32 uri)))
         (body
           (xml-element "manifest"
            (list (list "package" 3 package nil) (android-attribute "versionCode" 16 version-code)
                  (string-attribute "versionName" version-name))
            (append
             (list (list "uses-sdk" (list (android-attribute "minSdkVersion" 16 min-sdk)
                                          (android-attribute "targetSdkVersion" 16 target-sdk)) nil)
                   (list "uses-feature" (list (android-attribute "glEsVersion" 17 #x20000)
                                              (android-attribute "required" 18 #xffffffff)) nil))
             (mapcar (lambda (p) (list "uses-permission" (list (string-attribute "name" p)) nil)) permissions)
             (list
              (list "application"
               (list (android-attribute "theme" 1 #x0103022f) (string-attribute "label" label)
                     (android-attribute "hasCode" 18 0) (android-attribute "debuggable" 18 (if debuggable #xffffffff 0))
                     (android-attribute "extractNativeLibs" 18 0))
               (list
                (list "activity"
                 (list (string-attribute "name" "android.app.NativeActivity")
                       (android-attribute "exported" 18 #xffffffff)
                       (android-attribute "configChanges" 17 #x4a0))
                 (list (list "meta-data" (list (string-attribute "name" "android.app.lib_name")
                                              (string-attribute "value" "egcl_android")) nil)
                       (list "intent-filter" nil
                        (list (list "action" (list (string-attribute "name" "android.intent.action.MAIN")) nil)
                              (list "category" (list (string-attribute "name" "android.intent.category.LAUNCHER")) nil))))))))))))
    (xml-chunk #x0003 8
     (bytes (string-pool)
            (xml-chunk #x0180 8 (apply #'bytes (mapcar (lambda (a) (u32 (cdr a))) *android-attributes*)))
            (xml-node #x0100 namespace) body (xml-node #x0101 namespace)))))
