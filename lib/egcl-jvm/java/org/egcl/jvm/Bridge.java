package org.egcl.jvm;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.lang.invoke.MethodType;
import java.lang.reflect.Array;
import java.lang.reflect.Constructor;
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.lang.reflect.Proxy;

/** Private adapter. User-supplied types never become unchecked JNI signatures. */
public final class Bridge implements InvocationHandler {
    private final long callback;
    private final boolean signed;
    private Bridge(long callback, boolean signed) { this.callback = callback; this.signed = signed; }
    private static native Object invokeLisp(long callback, String name, Object[] args);

    static Class<?> resolve(Object type, ClassLoader loader) throws ClassNotFoundException {
        if (type instanceof Class<?>) return (Class<?>) type;
        if (!(type instanceof String)) throw new IllegalArgumentException("Expected a class name or Class object");
        return Class.forName((String) type, true, loader);
    }
    private static ClassLoader loader() { return ClassLoader.getSystemClassLoader(); }
    private static Object[] convert(Class<?>[] types, Object[] args) {
        if (types.length != args.length) throw new IllegalArgumentException("Argument count does not match signature");
        Object[] result = new Object[args.length];
        for (int i = 0; i < args.length; ++i) result[i] = convert(types[i], args[i]);
        return result;
    }
    static Object convert(Class<?> type, Object value) {
        if (type == void.class) return null;
        if (!type.isPrimitive()) {
            if (value != null && !type.isInstance(value))
                throw new IllegalArgumentException("Expected " + type.getTypeName());
            return value;
        }
        if (type == boolean.class) {
            if (!(value instanceof Boolean)) throw new IllegalArgumentException("Expected a boolean");
            return value;
        }
        if (type == char.class && value instanceof Character) return value;
        if (value instanceof Character) value = Integer.valueOf((Character)value);
        if (!(value instanceof Number)) throw new IllegalArgumentException("Expected a number for " + type);
        Number number = (Number) value;
        if (type == double.class) return number.doubleValue();
        if (type == float.class) {
            double x = number.doubleValue();
            if (Double.isFinite(x) && Math.abs(x) > Float.MAX_VALUE)
                throw new IllegalArgumentException("float overflow");
            return (float) x;
        }
        // No truncation, saturation, or rounding for integral signatures.
        if (!(number instanceof Byte || number instanceof Short || number instanceof Integer || number instanceof Long))
            throw new IllegalArgumentException("Expected an integer for " + type);
        long n = number.longValue();
        if (type == long.class) return n;
        if (type == int.class && n >= Integer.MIN_VALUE && n <= Integer.MAX_VALUE) return (int)n;
        if (type == short.class && n >= Short.MIN_VALUE && n <= Short.MAX_VALUE) return (short)n;
        if (type == byte.class && n >= Byte.MIN_VALUE && n <= Byte.MAX_VALUE) return (byte)n;
        if (type == char.class && n >= Character.MIN_VALUE && n <= Character.MAX_VALUE) return (char)n;
        throw new IllegalArgumentException("Integer out of range for " + type);
    }
    // Collections and other libraries return private implementation classes.
    // Invoke their public contract without opening modules or suppressing access checks.
    static Method accessibleMethod(Class<?> type, Method requested, Object receiver) {
        try {
            Method candidate = type.getMethod(requested.getName(), requested.getParameterTypes());
            if (candidate.getReturnType() == requested.getReturnType() && candidate.canAccess(receiver))
                return candidate;
        } catch (NoSuchMethodException ignored) { }
        for (Class<?> iface : type.getInterfaces()) {
            Method candidate = accessibleMethod(iface, requested, receiver);
            if (candidate != null) return candidate;
        }
        return type.getSuperclass() == null ? null : accessibleMethod(type.getSuperclass(), requested, receiver);
    }
    public static Object dispatch(int op, Object target, String name, String signature, Object[] args) throws Throwable {
        try {
            if (op >= 8) return Api.dispatch(op, target, name, signature, args);
            if (op == 3) return resolve(target, args.length == 0 ? loader() : (ClassLoader)args[0]);
            if (op == 4) {
                return proxy(resolve(target, loader()), ((Number)args[0]).longValue(), false);
            }
            if (op == 5) return Array.getLength(target);
            if (op == 6) return Array.get(target, (Integer)convert(int.class, args[0]));
            if (op == 7) {
                Array.set(target, (Integer)convert(int.class, args[0]), convert(target.getClass().getComponentType(), args[1]));
                return null;
            }
            Class<?> type = op == 1 ? target.getClass() : resolve(target, loader());
            MethodType descriptor = MethodType.fromMethodDescriptorString(signature, type.getClassLoader());
            Object[] converted = convert(descriptor.parameterArray(), args);
            if (op == 0) {
                if (descriptor.returnType() != void.class) throw new IllegalArgumentException("Constructor signature must return V");
                Constructor<?> constructor = type.getConstructor(descriptor.parameterArray());
                return constructor.newInstance(converted);
            }
            Method method = type.getMethod(name, descriptor.parameterArray());
            if (method.getReturnType() != descriptor.returnType()) throw new NoSuchMethodException("Return type does not match signature");
            if (Modifier.isStatic(method.getModifiers()) != (op == 2)) throw new IllegalArgumentException("Static/instance method mismatch");
            if (op == 1 && !method.canAccess(target)) {
                method = accessibleMethod(type, method, target);
                if (method == null) throw new IllegalAccessException("No accessible public method declaration");
            }
            return method.invoke(op == 2 ? null : target, converted);
        } catch (InvocationTargetException e) { throw e.getCause(); }
    }
    static long callbackToken(Object value) {
        if (value == null || !Proxy.isProxyClass(value.getClass())) return 0;
        InvocationHandler handler = Proxy.getInvocationHandler(value);
        return handler instanceof Bridge ? ((Bridge)handler).callback : 0;
    }
    static Object proxy(Class<?> iface, long callback, boolean signed) {
        if (!iface.isInterface() || !Modifier.isPublic(iface.getModifiers()))
            throw new IllegalArgumentException("Expected a public Java interface");
        return Proxy.newProxyInstance(iface.getClassLoader(), new Class<?>[]{iface}, new Bridge(callback, signed));
    }
    @Override public Object invoke(Object proxy, Method method, Object[] args) throws Throwable {
        if (method.getDeclaringClass() == Object.class) {
            switch (method.getName()) {
                case "hashCode": return System.identityHashCode(proxy);
                case "equals": return proxy == args[0];
                case "toString": return "EGCL callback " + callback;
                default: throw new AssertionError(method);
            }
        }
        if (signed && method.isDefault()) return InvocationHandler.invokeDefault(proxy, method, args == null ? new Object[0] : args);
        return convert(method.getReturnType(), invokeLisp(callback, signed ? Api.callbackKey(proxy.getClass().getInterfaces()[0], method) : method.getName(), args == null ? new Object[0] : args));
    }
    public static Object box(int kind, long integer, double real, String text) {
        switch (kind) {
            case 0: return null;
            case 1: return integer;
            case 2: return real;
            case 3: return text;
            case 4: return integer != 0;
            case 5:
                if (integer < 0 || integer > 65535) throw new IllegalArgumentException("Java char is one UTF-16 code unit");
                return (char)integer;
            case 6: return (float)real;
            default: throw new IllegalArgumentException("Invalid value kind");
        }
    }
    public static int kind(Object value) {
        if (value == Api.VOID) return 8;
        if (value == null) return 0;
        if (value instanceof String) return 3;
        if (value instanceof Boolean) return 4;
        if (value instanceof Character) return 5;
        if (value instanceof Float) return 6;
        if (value instanceof Double) return 2;
        if (value instanceof Byte || value instanceof Short || value instanceof Integer || value instanceof Long) return 1;
        return 7;
    }
    public static long integer(Object value) {
        if (value instanceof Boolean) return (Boolean)value ? 1 : 0;
        if (value instanceof Character) return (Character)value;
        return ((Number)value).longValue();
    }
    public static double real(Object value) { return ((Number)value).doubleValue(); }

    // ── Java's standard streams ────────────────────────────────────────
    //
    // System.out writes to file descriptor 1 directly, so Java output bypassed
    // *STANDARD-OUTPUT* entirely: a WITH-OUTPUT-TO-STRING around a Java call saw
    // nothing, and output interleaved with Lisp's by flush timing rather than by
    // program order. (Unlike the Python case it was never LOST -- System.out is
    // autoflush-on-println -- so this is about capture and ordering only.)
    //
    // So both streams are redirected into byte buffers that Lisp drains at each
    // crossing and writes to *STANDARD-OUTPUT* / *ERROR-OUTPUT*. The write happens
    // in Lisp, not here, because only there does *STANDARD-OUTPUT* mean what the
    // caller intends -- including a capture in force.
    //
    // Redirection is deliberately at the Java level rather than dup2 on fd 1, so
    // native writes inside the JVM -- JNI libraries, -Xlog, crash reports -- keep
    // reaching the real fd 1 where a reader expects them.
    //
    // The cost, as for Python: output appears when the crossing ends, so a long
    // computation's progress prints arrive together. A pipe would not fix that
    // (nothing drains it while Lisp is blocked in the call, so Java would block
    // once the 64K kernel buffer filled) -- see the bead for that analysis.
    private static ByteArrayOutputStream outBuffer, errBuffer;
    private static PrintStream outStream, errStream;

    /** Redirect the standard streams into buffers Lisp can drain. Idempotent. */
    public static synchronized void captureStreams() {
        if (outBuffer == null) { outBuffer = new ByteArrayOutputStream(); errBuffer = new ByteArrayOutputStream(); }
        // Reinstall when user code has replaced a stream, rather than silently
        // losing everything it writes from then on. The buffers are REUSED so a
        // caller that kept a reference to ours keeps working.
        if (System.out != outStream) {
            outStream = new PrintStream(outBuffer, true, java.nio.charset.StandardCharsets.UTF_8);
            System.setOut(outStream);
        }
        if (System.err != errStream) {
            errStream = new PrintStream(errBuffer, true, java.nio.charset.StandardCharsets.UTF_8);
            System.setErr(errStream);
        }
    }

    /**
     * Everything written to stream {@code which} (0 = out, 1 = err) since the last
     * call, or null when there is nothing -- the common case, kept cheap because
     * this runs at every crossing.
     */
    public static synchronized String takeOutput(int which) {
        captureStreams();
        ByteArrayOutputStream buffer = which == 0 ? outBuffer : errBuffer;
        if (buffer.size() == 0) return null;
        byte[] bytes = buffer.toByteArray();
        // A JVM thread can write while we drain, so the tail may hold a partial
        // UTF-8 sequence. Decoding it now would corrupt that character, so the
        // incomplete tail goes back in the buffer and joins the next drain.
        int complete = completeUtf8Length(bytes);
        buffer.reset();
        if (complete < bytes.length) buffer.write(bytes, complete, bytes.length - complete);
        return complete == 0 ? null : new String(bytes, 0, complete, java.nio.charset.StandardCharsets.UTF_8);
    }

    /** Length of the longest prefix of {@code bytes} ending on a UTF-8 boundary. */
    private static int completeUtf8Length(byte[] bytes) {
        // At most three bytes can be pending: a 4-byte sequence missing three.
        for (int back = 1; back <= 3 && back <= bytes.length; ++back) {
            int b = bytes[bytes.length - back] & 0xFF;
            if ((b & 0xC0) == 0x80) continue;          // a continuation byte, keep scanning
            int needed = b < 0x80 ? 1 : b < 0xE0 ? 2 : b < 0xF0 ? 3 : 4;
            // Complete when the lead byte and its continuations are all present.
            return needed <= back ? bytes.length : bytes.length - back;
        }
        return bytes.length;
    }
}
