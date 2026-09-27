public final class ErgonomicFixture implements AutoCloseable {
    public static int closes;
    @Override public void close() { ++closes; throw new IllegalStateException("close failed"); }
    public static String number(int n) { return "int"; }
    public static String number(long n) { return "long"; }
    public static String ambiguous(String value) { return "string"; }
    public static String ambiguous(java.util.List<?> value) { return "list"; }
    public static String boxed(Integer value) { return "integer"; }
    public static String boxed(Object value) { return "object"; }
    public static String narrowed(byte value) { return "byte"; }
    public static String join(String... values) { return String.join(",", values); }
    public static String supplierClass(java.util.function.Supplier<?> supplier) { return supplier.get().getClass().getName(); }
    public static void misleadingError() { throw new IllegalArgumentException("Ambiguous Java call: ordinary application error"); }
    public static String elementClass(Object[] values) { return values[0].getClass().getName(); }
    public static int attempts;
    public static void fail() { ++attempts; throw new IllegalStateException("expected failure"); }
    public interface Wide { Object get(); }
    public interface Narrow { String get(); }
    @FunctionalInterface public interface Covariant extends Wide, Narrow { }
    public static Object throughWide(Covariant f) { return ((Wide)f).get(); }
    public interface Overloaded {
        int apply(int value);
        String apply(String value);
    }
    public static int intCallback(Overloaded f) { return f.apply(7); }
    public static String stringCallback(Overloaded f) { return f.apply("hello"); }
    public int value = 3;
}
