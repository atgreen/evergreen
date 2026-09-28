package org.torcl.jvm;

import java.lang.invoke.MethodType;
import java.lang.reflect.*;
import java.util.*;
import java.util.concurrent.ConcurrentHashMap;

/** Checked public-member resolution for the Lisp-facing JAVA API. */
public final class Api {
    static final Object VOID = new Object();
    private static final Object TYPED = new Object();
    private static final Map<List<Object>, Executable> CACHE = new ConcurrentHashMap<>();
    private Api() { }
    public static void clear() { CACHE.clear(); }
    private static Class<?> type(Object name) throws ClassNotFoundException {
        if (name instanceof Class<?>) return (Class<?>)name;
        switch ((String)name) {
            case "boolean": return boolean.class;
            case "byte": return byte.class;
            case "short": return short.class;
            case "int": return int.class;
            case "long": return long.class;
            case "float": return float.class;
            case "double": return double.class;
            case "char": return char.class;
            case "void": return void.class;
            default: return Bridge.resolve(name, ClassLoader.getSystemClassLoader());
        }
    }
    private static boolean typed(Object x) {
        return x instanceof Object[] && ((Object[])x).length == 3 && ((Object[])x)[0] == TYPED;
    }
    static Object value(Object x) { return typed(x) ? ((Object[])x)[2] : x; }
    private static Class<?> source(Object x) {
        return typed(x) ? (Class<?>)((Object[])x)[1] : x == null ? null : x.getClass();
    }
    private static Class<?> boxed(Class<?> c) {
        if (c == boolean.class) return Boolean.class;
        if (c == byte.class) return Byte.class;
        if (c == short.class) return Short.class;
        if (c == char.class) return Character.class;
        if (c == int.class) return Integer.class;
        if (c == long.class) return Long.class;
        if (c == float.class) return Float.class;
        if (c == double.class) return Double.class;
        return c;
    }
    private static Class<?> unboxed(Class<?> c) {
        for (Class<?> p : new Class<?>[]{boolean.class, byte.class, short.class, char.class,
                                        int.class, long.class, float.class, double.class})
            if (boxed(p) == c) return p;
        return c;
    }
    private static boolean widens(Class<?> a, Class<?> b) {
        if (a == b) return true;
        if (a == byte.class && b == short.class) return true;
        if (a == byte.class || a == short.class || a == char.class)
            return b == int.class || b == long.class || b == float.class || b == double.class;
        if (a == int.class) return b == long.class || b == float.class || b == double.class;
        if (a == long.class) return b == float.class || b == double.class;
        return a == float.class && b == double.class;
    }
    private static boolean accepts(Class<?> from, Class<?> to, boolean boxing) {
        if (from == null) return !to.isPrimitive();
        if (from.isPrimitive() == to.isPrimitive())
            return from.isPrimitive() ? widens(from, to) : to.isAssignableFrom(from);
        if (!boxing) return false;
        return from.isPrimitive() ? to.isAssignableFrom(boxed(from))
            : unboxed(from).isPrimitive() && widens(unboxed(from), to);
    }
    private static boolean applicable(Executable e, Object[] args, boolean boxing) {
        Class<?>[] p = e.getParameterTypes();
        if (p.length != args.length) return false;
        for (int i = 0; i < p.length; ++i) if (!accepts(source(args[i]), p[i], boxing)) return false;
        return true;
    }
    private static boolean moreSpecific(Executable a, Executable b) {
        Class<?>[] x = a.getParameterTypes(), y = b.getParameterTypes();
        for (int i = 0; i < x.length; ++i) if (!accepts(x[i], y[i], false)) return false;
        if (Arrays.equals(x, y)) return b.getDeclaringClass().isAssignableFrom(a.getDeclaringClass());
        return true;
    }
    private static Executable select(int op, Class<?> owner, String name, String signature, Object[] args) throws ReflectiveOperationException {
        List<Object> key = new ArrayList<>(Arrays.asList(op, owner, name, signature));
        for (Object arg : args) key.add(source(arg));
        Executable cached = CACHE.get(key);
        if (cached != null) return cached;
        List<Executable> candidates = new ArrayList<>();
        MethodType exact = signature == null ? null : MethodType.fromMethodDescriptorString(
            signature.endsWith(")") ? signature + "V" : signature, owner.getClassLoader());
        for (Executable e : op == 8 ? owner.getConstructors() : owner.getMethods()) {
            if (e instanceof Method) {
                Method m = (Method)e;
                if (!m.getName().equals(name) || Modifier.isStatic(m.getModifiers()) != (op == 10) || m.isBridge()) continue;
                if (exact != null && !signature.endsWith(")") && exact.returnType() != m.getReturnType()) continue;
            }
            if (exact != null && !Arrays.equals(exact.parameterArray(), e.getParameterTypes())) continue;
            candidates.add(e);
        }
        List<Executable> matches = new ArrayList<>();
        for (boolean boxing : new boolean[]{false, true}) {
            for (Executable e : candidates) if (applicable(e, args, boxing)) matches.add(e);
            if (!matches.isEmpty()) break;
        }
        if (matches.isEmpty()) throw new NoSuchMethodException(owner.getName() + "." + name + " for " + key.subList(4, key.size()) + "; candidates " + candidates);
        List<Executable> best = new ArrayList<>();
        for (Executable a : matches) {
            boolean dominated = false;
            for (Executable b : matches) if (a != b && moreSpecific(b, a) && !moreSpecific(a, b)) { dominated = true; break; }
            if (!dominated) best.add(a);
        }
        // Reflection can expose the same inherited contract through several interfaces.
        Executable result = best.get(0);
        for (Executable e : best)
            if (!Arrays.equals(result.getParameterTypes(), e.getParameterTypes()))
                throw new AmbiguousCall("Ambiguous Java call: " + owner.getName() + "." + name + " for " + key.subList(4, key.size()) + "; candidates " + best);
        CACHE.put(key, result);
        return result;
    }
    static String methodKey(Method m) {
        return m.getName() + "\n" + MethodType.methodType(m.getReturnType(), m.getParameterTypes()).toMethodDescriptorString();
    }
    private static boolean objectMethod(Method m) {
        try { Object.class.getMethod(m.getName(), m.getParameterTypes()); return true; }
        catch (NoSuchMethodException e) { return false; }
    }
    private static Collection<Method> contracts(Class<?> iface) {
        if (!iface.isInterface() || !Modifier.isPublic(iface.getModifiers()))
            throw new IllegalArgumentException("Expected a public Java interface");
        Map<String, Method> methods = new TreeMap<>();
        for (Method m : iface.getMethods()) {
            if (!Modifier.isAbstract(m.getModifiers()) || objectMethod(m)) continue;
            String key = m.getName() + MethodType.methodType(void.class, m.getParameterTypes()).toMethodDescriptorString();
            Method previous = methods.get(key);
            if (previous == null || previous.getReturnType().isAssignableFrom(m.getReturnType())) methods.put(key, m);
        }
        return methods.values();
    }
    static String[] abstractMethods(Class<?> iface) {
        List<String> keys = new ArrayList<>();
        for (Method m : contracts(iface)) keys.add(methodKey(m));
        return keys.toArray(new String[0]);
    }
    static String callbackKey(Class<?> iface, Method invoked) {
        // A functional interface may inherit covariant declarations of one method.
        for (Method contract : contracts(iface))
            if (contract.getName().equals(invoked.getName()) && Arrays.equals(contract.getParameterTypes(), invoked.getParameterTypes()))
                return methodKey(contract);
        throw new IllegalArgumentException("Unknown callback contract: " + invoked);
    }
    public static Object dispatch(int op, Object target, String name, String signature, Object[] args) throws Throwable {
        if (op == 23) return value(args[0]);
        if (op == 20) {
            Class<?> component = target.getClass().getComponentType();
            if (component == null || !accepts(source(args[1]), component, true))
                throw new IllegalArgumentException("Incompatible array value");
            Array.set(target, (Integer)Bridge.convert(int.class, value(args[0])), Bridge.convert(component, value(args[1])));
            return VOID;
        }
        if (op == 21) {
            Class<?> owner = type(target);
            MethodType exact = MethodType.fromMethodDescriptorString(signature.endsWith(")") ? signature + "V" : signature, owner.getClassLoader());
            Method method = owner.getMethod(name, exact.parameterArray());
            if (Modifier.isStatic(method.getModifiers()) != (Boolean)value(args[0]))
                throw new IllegalArgumentException("Static/instance method mismatch");
            if (!signature.endsWith(")") && exact.returnType() != method.getReturnType())
                throw new NoSuchMethodException("Return type does not match binding");
            return true;
        }
        if (op == 22) {
            Class<?> owner = type(target);
            Set<String> members = new TreeSet<>();
            for (Constructor<?> c : owner.getConstructors()) members.add(c.toGenericString());
            for (Method m : owner.getMethods()) members.add(m.toGenericString());
            for (Field f : owner.getFields()) members.add(f.toGenericString());
            return members.toArray(new String[0]);
        }
        if (op == 19) return Bridge.callbackToken(target);
        if (op == 11) {
            Class<?> declared = type(target);
            if (declared == void.class) throw new IllegalArgumentException("void is not an argument type");
            return new Object[]{TYPED, declared, Bridge.convert(declared, value(args[0]))};
        }
        if (op == 12) return Bridge.proxy(type(target), ((Number)value(args[0])).longValue(), true);
        if (op == 13) return Array.newInstance(type(target), (Integer)Bridge.convert(int.class, value(args[0])));
        if (op == 18) return abstractMethods(type(target));
        if (op >= 14 && op <= 17) {
            boolean statik = op >= 16;
            Class<?> owner = statik ? type(target) : target.getClass();
            Field field = owner.getField(name);
            if (Modifier.isStatic(field.getModifiers()) != statik) throw new IllegalArgumentException("Static/instance field mismatch");
            Object receiver = statik ? null : target;
            if (op == 14 || op == 16) return field.get(receiver);
            if (!accepts(source(args[0]), field.getType(), true)) throw new IllegalArgumentException("Incompatible field value");
            field.set(receiver, Bridge.convert(field.getType(), value(args[0])));
            return VOID;
        }
        if (op < 8 || op > 10) throw new IllegalArgumentException("Unknown API operation");
        Class<?> owner = op == 9 ? target.getClass() : type(target);
        Executable chosen = select(op, owner, name, signature, args);
        Class<?>[] params = chosen.getParameterTypes();
        Object[] converted = new Object[args.length];
        for (int i = 0; i < args.length; ++i) converted[i] = Bridge.convert(params[i], value(args[i]));
        try {
            if (chosen instanceof Constructor<?>) return ((Constructor<?>)chosen).newInstance(converted);
            Method method = (Method)chosen;
            Object receiver = op == 10 ? null : target;
            if (!method.canAccess(receiver)) {
                method = Bridge.accessibleMethod(owner, method, receiver);
                if (method == null) throw new IllegalAccessException("No accessible public method declaration");
            }
            Object result = method.invoke(receiver, converted);
            return method.getReturnType() == void.class ? VOID : result;
        } catch (InvocationTargetException e) { throw e.getCause(); }
    }
}
