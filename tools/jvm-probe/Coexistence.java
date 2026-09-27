/** Test workload only: this is not a Java interop API. */
public final class Coexistence {
    private static native int callback(int value);
    public static int twice(int value) { return value * 2; }
    public static int recurse(int depth) {
        return depth == 0 ? 7 : callback(depth - 1) + 1;
    }
    public static int foreignThread(int value) throws InterruptedException {
        int[] result = new int[1];
        Thread thread = new Thread(() -> result[0] = callback(value));
        thread.start();
        thread.join();
        return result[0];
    }
    public static int collect(int unused) { System.gc(); return 19; }
    public static int overflow(int unused) {
        try { recursiveOverflow(1); return -1; }
        catch (StackOverflowError expected) { return 23; }
    }
    private static int recursiveOverflow(int n) { return recursiveOverflow(n + 1) + n; }
    public static int nullFault(int unused) {
        try { return ((Object) null).hashCode(); }
        catch (NullPointerException expected) { return 29; }
    }
    public static int throwing(int unused) { throw new IllegalStateException("probe exception"); }
    public static int overlapping(int value) throws InterruptedException {
        java.util.concurrent.CountDownLatch started = new java.util.concurrent.CountDownLatch(1);
        Thread collector = new Thread(() -> {
            started.countDown();
            for (int i = 0; i < 20; ++i) System.gc();
        });
        collector.start();
        started.await();
        int result = callback(value);
        collector.join();
        return result;
    }
    public static int pause(int millis) throws InterruptedException { Thread.sleep(millis); return 31; }
}
