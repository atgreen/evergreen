package org.torcl.jvm;

/** Distinguishes resolution failures from exceptions thrown by application code. */
public final class AmbiguousCall extends ReflectiveOperationException {
    private static final long serialVersionUID = 1L;
    AmbiguousCall(String message) { super(message); }
}
