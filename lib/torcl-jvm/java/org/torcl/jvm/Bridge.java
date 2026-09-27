package org.torcl.jvm;

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
    private Bridge(long callback) { this.callback = callback; }
    private static native Object invokeLisp(long callback, String name, Object[] args);

    private static Class<?> resolve(Object type, ClassLoader loader) throws ClassNotFoundException {
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
    private static Object convert(Class<?> type, Object value) {
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
    private static Method accessibleMethod(Class<?> type, Method requested, Object receiver) {
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
            if (op == 3) return resolve(target, args.length == 0 ? loader() : (ClassLoader)args[0]);
            if (op == 4) {
                Class<?> iface = resolve(target, loader());
                if (!iface.isInterface() || !Modifier.isPublic(iface.getModifiers()))
                    throw new IllegalArgumentException("Expected a public Java interface");
                return Proxy.newProxyInstance(iface.getClassLoader(), new Class<?>[]{iface},
                    new Bridge(((Number)args[0]).longValue()));
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
    @Override public Object invoke(Object proxy, Method method, Object[] args) {
        if (method.getDeclaringClass() == Object.class) {
            switch (method.getName()) {
                case "hashCode": return System.identityHashCode(proxy);
                case "equals": return proxy == args[0];
                case "toString": return "TorCL callback " + callback;
                default: throw new AssertionError(method);
            }
        }
        return convert(method.getReturnType(), invokeLisp(callback, method.getName(), args == null ? new Object[0] : args));
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
}
