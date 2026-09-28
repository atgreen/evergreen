(defpackage :torcl-jvm
  (:use :cl)
  (:export :start-jvm :stop-jvm :jvm-running-p :jvm-error :java-error
           :error-message :new :call :call-static :find-java-class :implement
           :release :retain :with-java-objects :java-object-p :same-object-p
           :array-length :array-ref :array-set :+null+ :weak-reference :promote
           :drain-output :draining))
