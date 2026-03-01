import java.util.ArrayList;
import java.util.List;

/**
 * Debugium Java test target.
 * Breakpoint target: line 20 (inside fibonacci loop).
 */
public class TargetJava {

    static class FibResult {
        int index;
        long value;
        FibResult(int i, long v) { this.index = i; this.value = v; }
        public String toString() { return "Fib(" + index + ")=" + value; }
    }

    public static List<FibResult> fibonacci(int n) {
        List<FibResult> results = new ArrayList<>();
        long a = 0, b = 1;                        // line 20 — breakpoint here
        for (int i = 0; i < n; i++) {
            results.add(new FibResult(i, a));
            long temp = a + b;
            a = b;
            b = temp;
        }
        return results;
    }

    public static void main(String[] args) {
        System.out.println("TargetJava starting...");
        int count = 10;
        List<FibResult> fibs = fibonacci(count);
        for (FibResult r : fibs) {
            System.out.println(r);
        }
        System.out.println("Done. Total: " + fibs.size());
    }
}
