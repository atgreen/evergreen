import java.util.function.IntUnaryOperator;
import java.util.function.UnaryOperator;
public final class JvmFixture {
    private static IntUnaryOperator saved;
    public static int onThread(IntUnaryOperator operator, int value) throws Exception {
        int[] result = new int[1];
        Throwable[] failure = new Throwable[1];
        Thread thread = new Thread(() -> {
            try { result[0] = operator.applyAsInt(value); }
            catch (Throwable error) { failure[0] = error; }
        });
        thread.start(); thread.join();
        if (failure[0] != null) throw new Exception("callback thread failed", failure[0]);
        return result[0];
    }
    public static void remember(IntUnaryOperator operator) { saved = operator; }
    public static int remembered(int value) { return saved.applyAsInt(value); }
    public static void forget() { saved = null; }
    public static Object echo(UnaryOperator<Object> operator, Object value) { return operator.apply(value); }
    public static String unpaired() { return new String(new char[]{0xd800}); }
    public static int[] numbers() { return new int[]{10,20,30}; }
    public static void linger(int millis) {
        Thread thread = new Thread(() -> {
            try { Thread.sleep(millis); } catch (InterruptedException ignored) { }
        });
        thread.setDaemon(false);
        thread.start();
    }
    public static ClassLoader isolatedLoader() {
        return new java.net.URLClassLoader(new java.net.URL[]{
            JvmFixture.class.getProtectionDomain().getCodeSource().getLocation()}, null);
    }
    public static int pause(int millis) throws InterruptedException { Thread.sleep(millis); return 17; }
}
